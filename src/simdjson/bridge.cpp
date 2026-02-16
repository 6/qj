// bridge.cpp — C-linkage wrapper around simdjson APIs for Rust FFI.
//
// Design principles:
//   - All functions return int (0 = success, positive = simdjson error code).
//   - All functions use try/catch — no C++ exceptions cross FFI boundary.
//   - Caller provides pre-padded buffers (SIMDJSON_PADDING extra zeroed bytes).
//   - JxParser bundles parser + document together (document borrows parser).

#include "simdjson.h"
#include <cstdlib>
#include <cstring>
#include <vector>

using namespace simdjson;

// Opaque handle holding both the parser and the most recent document.
// The document borrows internal parser buffers, so they must live together.
struct JxParser {
    ondemand::parser parser;
    ondemand::document document;
    // Reusable padded_string for iterate_many
    padded_string ndjson_buf;
};

extern "C" {

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

JxParser* jx_parser_new() {
    try {
        return new JxParser();
    } catch (...) {
        return nullptr;
    }
}

void jx_parser_free(JxParser* p) {
    delete p;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

size_t jx_simdjson_padding() {
    return SIMDJSON_PADDING;
}

// ---------------------------------------------------------------------------
// On-Demand parsing — caller must provide a buffer with SIMDJSON_PADDING
// extra zeroed bytes after `len`.
// ---------------------------------------------------------------------------

int jx_parse_ondemand(JxParser* p, const char* buf, size_t len) {
    try {
        auto sv = padded_string_view(buf, len, len + SIMDJSON_PADDING);
        auto err = p->parser.iterate(sv).get(p->document);
        return static_cast<int>(err);
    } catch (...) {
        return -1;
    }
}

// ---------------------------------------------------------------------------
// Field extraction — operate on the most recently parsed document.
// ---------------------------------------------------------------------------

int jx_doc_find_field_str(JxParser* p, const char* key, size_t key_len,
                          const char** out, size_t* out_len) {
    try {
        std::string_view k(key, key_len);
        std::string_view result;
        auto err = p->document.find_field(k).get_string().get(result);
        if (err) return static_cast<int>(err);
        *out = result.data();
        *out_len = result.size();
        return 0;
    } catch (...) {
        return -1;
    }
}

int jx_doc_find_field_int64(JxParser* p, const char* key, size_t key_len,
                            int64_t* out) {
    try {
        std::string_view k(key, key_len);
        auto err = p->document.find_field(k).get_int64().get(*out);
        return static_cast<int>(err);
    } catch (...) {
        return -1;
    }
}

int jx_doc_find_field_double(JxParser* p, const char* key, size_t key_len,
                             double* out) {
    try {
        std::string_view k(key, key_len);
        auto err = p->document.find_field(k).get_double().get(*out);
        return static_cast<int>(err);
    } catch (...) {
        return -1;
    }
}

// Returns simdjson::ondemand::json_type as int:
//   0 = array, 1 = object, 2 = number, 3 = string, 4 = boolean, 5 = null
int jx_doc_type(JxParser* p, int* out_type) {
    try {
        ondemand::json_type t;
        auto err = p->document.type().get(t);
        if (err) return static_cast<int>(err);
        *out_type = static_cast<int>(t);
        return 0;
    } catch (...) {
        return -1;
    }
}

// ---------------------------------------------------------------------------
// Benchmark helpers — run full loops in C++ to measure pure simdjson
// throughput without per-document FFI overhead.
// ---------------------------------------------------------------------------

// Count documents in an NDJSON buffer using iterate_many.
int jx_iterate_many_count(const char* buf, size_t len, size_t batch_size,
                          uint64_t* out_count) {
    try {
        ondemand::parser parser;
        // iterate_many needs a padded_string or padded_string_view.
        // The caller guarantees SIMDJSON_PADDING extra bytes.
        auto sv = padded_string_view(buf, len, len + SIMDJSON_PADDING);
        ondemand::document_stream stream;
        auto err = parser.iterate_many(sv, batch_size).get(stream);
        if (err) return static_cast<int>(err);

        uint64_t count = 0;
        for (auto doc_result : stream) {
            // SAFETY: check error before .value() — calling .value() on a
            // malformed document can abort inside simdjson (fuzz-found crash).
            if (doc_result.error()) continue;
            ondemand::document& doc = doc_result.value();
            (void)doc;
            count++;
        }
        *out_count = count;
        return 0;
    } catch (...) {
        return -1;
    }
}

// Extract a string field from every document in NDJSON, sum up the lengths
// (to prevent optimizer from eliding work). Returns total bytes extracted.
int jx_iterate_many_extract_field(const char* buf, size_t len,
                                  size_t batch_size,
                                  const char* field_name, size_t field_name_len,
                                  uint64_t* out_total_bytes) {
    try {
        ondemand::parser parser;
        auto sv = padded_string_view(buf, len, len + SIMDJSON_PADDING);
        std::string_view field(field_name, field_name_len);

        ondemand::document_stream stream;
        auto err = parser.iterate_many(sv, batch_size).get(stream);
        if (err) return static_cast<int>(err);

        uint64_t total = 0;
        // Use DOM parser for field extraction — simdjson's on-demand API
        // segfaults on malformed objects like `{z}` inside iterate_many
        // (fuzz-found crash). The DOM parser fully validates JSON first.
        dom::parser dom_parser;
        auto dom_stream = dom_parser.parse_many(
            reinterpret_cast<const uint8_t*>(buf), len, batch_size);
        for (auto doc_result : dom_stream) {
            dom::element doc;
            if (doc_result.get(doc)) continue;
            dom::object obj;
            if (doc.get_object().get(obj)) continue;
            std::string_view val;
            auto field_err = obj[field].get_string().get(val);
            if (!field_err) {
                total += val.size();
            }
        }
        *out_total_bytes = total;
        return 0;
    } catch (...) {
        return -1;
    }
}

// ---------------------------------------------------------------------------
// Parse to flat token buffer for Rust Value construction.
// Uses On-Demand API to preserve raw number text (jq literal compat).
//
// Token format (little-endian):
//   Null:        tag=0
//   Bool:        tag=1, u8 (0 or 1)
//   Int:         tag=2, i64
//   Double:      tag=3, f64, u32 raw_len, bytes[raw_len]
//   String:      tag=4, u32 len, bytes[len]
//   ArrayStart:  tag=5, u32 count
//   ArrayEnd:    tag=6
//   ObjectStart: tag=7, u32 count
//   ObjectEnd:   tag=8
//
// Object keys are emitted as String tokens before each value.
// Double includes raw_len + raw text from JSON source (for literal
// preservation). raw_len=0 means no raw text (e.g. uint64 overflow).
// ---------------------------------------------------------------------------

static const uint8_t TAG_NULL = 0;
static const uint8_t TAG_BOOL = 1;
static const uint8_t TAG_INT = 2;
static const uint8_t TAG_DOUBLE = 3;
static const uint8_t TAG_STRING = 4;
static const uint8_t TAG_ARRAY_START = 5;
static const uint8_t TAG_ARRAY_END = 6;
static const uint8_t TAG_OBJECT_START = 7;
static const uint8_t TAG_OBJECT_END = 8;

static void emit_u8(std::vector<uint8_t>& out, uint8_t v) {
    out.push_back(v);
}

static void emit_u32(std::vector<uint8_t>& out, uint32_t v) {
    out.push_back(static_cast<uint8_t>(v));
    out.push_back(static_cast<uint8_t>(v >> 8));
    out.push_back(static_cast<uint8_t>(v >> 16));
    out.push_back(static_cast<uint8_t>(v >> 24));
}

static void emit_i64(std::vector<uint8_t>& out, int64_t v) {
    uint64_t u;
    std::memcpy(&u, &v, sizeof(u));
    for (int i = 0; i < 8; i++) {
        out.push_back(static_cast<uint8_t>(u >> (i * 8)));
    }
}

static void emit_f64(std::vector<uint8_t>& out, double v) {
    uint64_t u;
    std::memcpy(&u, &v, sizeof(u));
    for (int i = 0; i < 8; i++) {
        out.push_back(static_cast<uint8_t>(u >> (i * 8)));
    }
}

static void emit_string(std::vector<uint8_t>& out, std::string_view sv) {
    emit_u8(out, TAG_STRING);
    emit_u32(out, static_cast<uint32_t>(sv.size()));
    out.insert(out.end(), sv.begin(), sv.end());
}

// Trim raw_json_token() result to valid JSON number characters.
// raw_json_token() may include trailing whitespace or punctuation.
static size_t trim_number_len(std::string_view raw) {
    size_t len = 0;
    for (size_t i = 0; i < raw.size(); i++) {
        char c = raw[i];
        if ((c >= '0' && c <= '9') || c == '.' || c == '-' ||
            c == '+' || c == 'e' || c == 'E') {
            len = i + 1;
        } else {
            break;
        }
    }
    return len;
}

// Emit a double with its raw JSON text for literal preservation.
static void emit_double_with_raw(std::vector<uint8_t>& out, double v,
                                  std::string_view raw) {
    emit_u8(out, TAG_DOUBLE);
    emit_f64(out, v);
    size_t raw_len = trim_number_len(raw);
    emit_u32(out, static_cast<uint32_t>(raw_len));
    if (raw_len > 0) {
        out.insert(out.end(), raw.begin(), raw.begin() + raw_len);
    }
}

// Patch a u32 value at a specific position in the output buffer.
static void patch_u32(std::vector<uint8_t>& out, size_t pos, uint32_t v) {
    out[pos]     = static_cast<uint8_t>(v);
    out[pos + 1] = static_cast<uint8_t>(v >> 8);
    out[pos + 2] = static_cast<uint8_t>(v >> 16);
    out[pos + 3] = static_cast<uint8_t>(v >> 24);
}

static const int MAX_DEPTH = 1024;

// Emit a number from its raw JSON token, handling the case where simdjson
// rejects integers beyond u64 (BIGINT_ERROR).  When get_number() succeeds
// we go through emit_number(); otherwise we fall back to strtod + raw text.
static void emit_number_or_bigint(std::vector<uint8_t>& out,
                                   std::string_view raw) {
    // Parse raw token to approximate f64 — strtod handles arbitrarily long
    // digit strings and gives the closest IEEE 754 double.
    size_t raw_len = trim_number_len(raw);
    std::string tmp(raw.data(), raw_len);
    char* end = nullptr;
    double d = std::strtod(tmp.c_str(), &end);
    emit_double_with_raw(out, d, raw);
}

// Emit a number value from its raw token and number info.
static void emit_number(std::vector<uint8_t>& out,
                         std::string_view raw,
                         ondemand::number num) {
    switch (num.get_number_type()) {
        case ondemand::number_type::signed_integer:
            emit_u8(out, TAG_INT);
            emit_i64(out, num.get_int64());
            break;
        case ondemand::number_type::unsigned_integer: {
            uint64_t u = num.get_uint64();
            if (u <= static_cast<uint64_t>(INT64_MAX)) {
                emit_u8(out, TAG_INT);
                emit_i64(out, static_cast<int64_t>(u));
            } else {
                emit_double_with_raw(out, static_cast<double>(u), raw);
            }
            break;
        }
        case ondemand::number_type::floating_point_number:
        case ondemand::number_type::big_integer:
            emit_double_with_raw(out, num.get_double(), raw);
            break;
    }
}

static void flatten_ondemand(std::vector<uint8_t>& out,
                              ondemand::value val, int depth) {
    if (depth > MAX_DEPTH) {
        throw simdjson::simdjson_error(simdjson::DEPTH_ERROR);
    }
    auto type = val.type().value();
    switch (type) {
        case ondemand::json_type::null:
            val.is_null().value();  // consume
            emit_u8(out, TAG_NULL);
            break;
        case ondemand::json_type::boolean:
            emit_u8(out, TAG_BOOL);
            emit_u8(out, val.get_bool().value() ? 1 : 0);
            break;
        case ondemand::json_type::number: {
            std::string_view raw = val.raw_json_token();
            auto num_result = val.get_number();
            if (num_result.error() == BIGINT_ERROR) {
                emit_number_or_bigint(out, raw);
            } else {
                ondemand::number num = num_result.value();
                val.get_double(); // consume the value
                emit_number(out, raw, num);
            }
            break;
        }
        case ondemand::json_type::string:
            emit_string(out, val.get_string().value());
            break;
        case ondemand::json_type::array: {
            emit_u8(out, TAG_ARRAY_START);
            size_t count_pos = out.size();
            emit_u32(out, 0); // placeholder
            uint32_t count = 0;
            for (auto element : val.get_array()) {
                flatten_ondemand(out, element.value(), depth + 1);
                count++;
            }
            patch_u32(out, count_pos, count);
            emit_u8(out, TAG_ARRAY_END);
            break;
        }
        case ondemand::json_type::object: {
            emit_u8(out, TAG_OBJECT_START);
            size_t count_pos = out.size();
            emit_u32(out, 0); // placeholder
            uint32_t count = 0;
            for (auto field : val.get_object()) {
                emit_string(out, field.unescaped_key().value());
                flatten_ondemand(out, field.value(), depth + 1);
                count++;
            }
            patch_u32(out, count_pos, count);
            emit_u8(out, TAG_OBJECT_END);
            break;
        }
        default:
            // json_type::unknown — shouldn't occur in valid JSON
            emit_u8(out, TAG_NULL);
            break;
    }
}

// Parse a JSON document and write a flat token buffer.
// Uses On-Demand API to preserve raw number text.
// Caller provides `buf` with SIMDJSON_PADDING extra zeroed bytes.
// On success, sets *out_ptr and *out_len to a heap-allocated buffer
// that the caller must free with jx_flat_buffer_free().
int jx_dom_to_flat(const char* buf, size_t len,
                   uint8_t** out_ptr, size_t* out_len) {
    try {
        ondemand::parser parser;
        auto padded = padded_string_view(buf, len, len + SIMDJSON_PADDING);
        ondemand::document doc = parser.iterate(padded).value();

        std::vector<uint8_t> flat;
        flat.reserve(len); // Rough pre-allocation

        auto type = doc.type().value();
        if (type == ondemand::json_type::array ||
            type == ondemand::json_type::object) {
            // Non-scalar: use get_value() + recursive flatten
            flatten_ondemand(flat, doc.get_value().value(), 0);
        } else {
            // Scalar document: handle directly from document
            switch (type) {
                case ondemand::json_type::null:
                    doc.is_null().value();
                    emit_u8(flat, TAG_NULL);
                    break;
                case ondemand::json_type::boolean:
                    emit_u8(flat, TAG_BOOL);
                    emit_u8(flat, doc.get_bool().value() ? 1 : 0);
                    break;
                case ondemand::json_type::number: {
                    std::string_view raw = doc.raw_json_token();
                    auto num_result = doc.get_number();
                    if (num_result.error() == BIGINT_ERROR) {
                        emit_number_or_bigint(flat, raw);
                    } else {
                        ondemand::number num = num_result.value();
                        doc.get_double(); // consume
                        emit_number(flat, raw, num);
                    }
                    break;
                }
                case ondemand::json_type::string:
                    emit_string(flat, doc.get_string().value());
                    break;
                default:
                    emit_u8(flat, TAG_NULL);
                    break;
            }
        }

        // Copy to heap buffer for Rust ownership.
        *out_len = flat.size();
        *out_ptr = new uint8_t[flat.size()];
        std::memcpy(*out_ptr, flat.data(), flat.size());
        return 0;
    } catch (simdjson::simdjson_error& e) {
        return static_cast<int>(e.error());
    } catch (...) {
        return -1;
    }
}

// ---------------------------------------------------------------------------
// DOM tape walk — faster flat buffer construction.
//
// Uses dom::parser (SIMD-indexed tape + pre-unescaped strings) instead of
// On-Demand API. A parallel cursor into the original JSON extracts raw
// number text for literal preservation (e.g. "75.80" stays "75.80").
//
// ~2x faster than flatten_ondemand() because DOM tape is pre-indexed
// and strings are already unescaped — no per-value type dispatch overhead.
// ---------------------------------------------------------------------------

// Check if a character is part of a JSON number literal.
static bool is_number_char(char c) {
    return (c >= '0' && c <= '9') || c == '.' || c == '-' ||
           c == '+' || c == 'e' || c == 'E';
}

// Advance cursor past whitespace, commas, and colons.
static void advance_cursor(const char*& cursor) {
    while (*cursor == ' ' || *cursor == '\n' || *cursor == '\r' ||
           *cursor == '\t' || *cursor == ',' || *cursor == ':')
        cursor++;
}

// Advance cursor past a JSON string literal (handles escape sequences).
static void skip_json_string(const char*& cursor) {
    assert(*cursor == '"');
    cursor++; // opening quote
    while (*cursor != '"') {
        if (*cursor == '\\') cursor++; // skip escaped char
        cursor++;
    }
    cursor++; // closing quote
}

static void walk_element(std::vector<uint8_t>& flat,
                          dom::element elem, const char*& cursor,
                          int depth) {
    if (depth > MAX_DEPTH) {
        throw simdjson::simdjson_error(simdjson::DEPTH_ERROR);
    }
    advance_cursor(cursor);
    switch (elem.type()) {
        case dom::element_type::STRING: {
            skip_json_string(cursor);
            std::string_view sv = elem.get_string().value();
            emit_string(flat, sv);
            break;
        }
        case dom::element_type::INT64: {
            // Skip raw number text in original JSON
            if (*cursor == '-') cursor++;
            while (*cursor >= '0' && *cursor <= '9') cursor++;
            emit_u8(flat, TAG_INT);
            emit_i64(flat, elem.get_int64().value());
            break;
        }
        case dom::element_type::UINT64: {
            const char* start = cursor;
            while (*cursor >= '0' && *cursor <= '9') cursor++;
            uint64_t u = elem.get_uint64().value();
            if (u <= static_cast<uint64_t>(INT64_MAX)) {
                emit_u8(flat, TAG_INT);
                emit_i64(flat, static_cast<int64_t>(u));
            } else {
                // Beyond i64 range — emit as double with raw text
                std::string_view raw(start, cursor - start);
                emit_double_with_raw(flat, static_cast<double>(u), raw);
            }
            break;
        }
        case dom::element_type::DOUBLE: {
            const char* start = cursor;
            while (is_number_char(*cursor)) cursor++;
            std::string_view raw(start, cursor - start);
            emit_double_with_raw(flat, elem.get_double().value(), raw);
            break;
        }
        case dom::element_type::BOOL: {
            bool b = elem.get_bool().value();
            cursor += b ? 4 : 5; // "true" or "false"
            emit_u8(flat, TAG_BOOL);
            emit_u8(flat, b ? 1 : 0);
            break;
        }
        case dom::element_type::NULL_VALUE: {
            cursor += 4; // "null"
            emit_u8(flat, TAG_NULL);
            break;
        }
        case dom::element_type::ARRAY: {
            assert(*cursor == '[');
            cursor++;
            emit_u8(flat, TAG_ARRAY_START);
            size_t count_pos = flat.size();
            emit_u32(flat, 0); // placeholder
            uint32_t count = 0;
            for (dom::element child : dom::array(elem)) {
                walk_element(flat, child, cursor, depth + 1);
                count++;
            }
            patch_u32(flat, count_pos, count);
            advance_cursor(cursor);
            assert(*cursor == ']');
            cursor++; // skip ']'
            emit_u8(flat, TAG_ARRAY_END);
            break;
        }
        case dom::element_type::OBJECT: {
            assert(*cursor == '{');
            cursor++;
            emit_u8(flat, TAG_OBJECT_START);
            size_t count_pos = flat.size();
            emit_u32(flat, 0);
            uint32_t count = 0;
            for (auto field : dom::object(elem)) {
                advance_cursor(cursor);
                skip_json_string(cursor); // skip key in original JSON
                emit_string(flat, field.key);
                walk_element(flat, field.value, cursor, depth + 1);
                count++;
            }
            patch_u32(flat, count_pos, count);
            advance_cursor(cursor);
            assert(*cursor == '}');
            cursor++; // skip '}'
            emit_u8(flat, TAG_OBJECT_END);
            break;
        }
    }
}

// Parse a JSON document using DOM API and walk the tape to produce
// a flat token buffer. Faster than jx_dom_to_flat (On-Demand based)
// because DOM tape is pre-indexed with strings already unescaped.
// Raw number text is extracted via a parallel cursor for literal fidelity.
//
// Falls back to On-Demand path (jx_dom_to_flat) if DOM parse fails with
// NUM_ERROR (numbers beyond u64 range that DOM can't handle).
int jx_dom_to_flat_via_tape(const char* buf, size_t len,
                             uint8_t** out_ptr, size_t* out_len) {
    try {
        dom::parser parser;
        dom::element root;
        auto err = parser.parse(buf, len).get(root);
        if (err) {
            if (err == NUMBER_ERROR || err == BIGINT_ERROR) {
                // DOM can't handle big integers — fall back to On-Demand
                return jx_dom_to_flat(buf, len, out_ptr, out_len);
            }
            return static_cast<int>(err);
        }
        std::vector<uint8_t> flat;
        flat.reserve(len);
        const char* cursor = buf;
        walk_element(flat, root, cursor, 0);
        *out_len = flat.size();
        *out_ptr = new uint8_t[flat.size()];
        std::memcpy(*out_ptr, flat.data(), flat.size());
        return 0;
    } catch (simdjson::simdjson_error& e) {
        return static_cast<int>(e.error());
    } catch (...) { return -1; }
}

void jx_flat_buffer_free(uint8_t* ptr) {
    delete[] ptr;
}

// ---------------------------------------------------------------------------
// Minify — compact JSON without DOM construction (~10 GB/s).
// ---------------------------------------------------------------------------

int jx_minify(const char* buf, size_t len,
              char** out_ptr, size_t* out_len) {
    try {
        char* dst = new char[len];  // minified output is always <= input
        size_t dst_len;
        auto err = simdjson::minify(buf, len, dst, dst_len);
        if (err) { delete[] dst; return static_cast<int>(err); }
        *out_ptr = dst;
        *out_len = dst_len;
        return 0;
    } catch (...) { return -1; }
}

void jx_minify_free(char* ptr) {
    delete[] ptr;
}

// ---------------------------------------------------------------------------
// DOM field extraction — parse, navigate nested fields, serialize sub-tree.
// ---------------------------------------------------------------------------

// Navigate a chain of field names from the document root.
// Returns: 0 = found (result set), 1 = null (field missing / non-object parent),
//          2 = error (parse failed).
static int navigate_fields(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    dom::parser& parser, dom::element& result)
{
    auto err = parser.parse(buf, len).get(result);
    if (err) return 2;

    for (size_t i = 0; i < field_count; i++) {
        std::string_view key(fields[i], field_lens[i]);
        if (result.type() != dom::element_type::OBJECT) return 1;
        auto field_err = result.at_key(key).get(result);
        if (field_err) return 1; // field not found
    }
    return 0;
}

// JSON-escape a string for output (adds surrounding quotes).
static void json_escape(const std::string_view sv, std::string& out) {
    out.push_back('"');
    for (char c : sv) {
        switch (c) {
            case '"':  out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\b': out += "\\b";  break;
            case '\f': out += "\\f";  break;
            case '\n': out += "\\n";  break;
            case '\r': out += "\\r";  break;
            case '\t': out += "\\t";  break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char hex[8];
                    snprintf(hex, sizeof(hex), "\\u%04x", static_cast<unsigned char>(c));
                    out += hex;
                } else {
                    out.push_back(c);
                }
        }
    }
    out.push_back('"');
}

// Navigate a field chain using On-Demand API, returning the raw JSON bytes
// of the leaf value (preserves original number representation, escapes, etc.).
// Returns: 0 = found (raw set), 1 = null (field missing), 2 = parse error.
static int navigate_fields_raw(
    ondemand::parser& parser,
    const char* buf, size_t len,
    const char* const* fields, const size_t* field_lens, size_t field_count,
    std::string_view& raw)
{
    auto sv = padded_string_view(buf, len, len + SIMDJSON_PADDING);
    ondemand::document doc;
    auto err = parser.iterate(sv).get(doc);
    if (err) return 2;

    ondemand::value current;
    // Get first field from document root.
    {
        std::string_view key(fields[0], field_lens[0]);
        auto r = doc.find_field(key);
        if (r.error()) return 1;
        current = r.value();
    }
    // Navigate remaining fields.
    for (size_t i = 1; i < field_count; i++) {
        std::string_view key(fields[i], field_lens[i]);
        ondemand::object obj;
        if (current.get_object().get(obj)) return 1;
        auto r = obj.find_field(key);
        if (r.error()) return 1;
        current = r.value();
    }
    auto raw_result = current.raw_json();
    if (raw_result.error()) return 2;
    raw = raw_result.value();
    return 0;
}

// Trim trailing whitespace from a raw_json() result.
static std::string_view trim_raw_json(std::string_view raw) {
    while (!raw.empty() && (raw.back() == ' ' || raw.back() == '\n' ||
           raw.back() == '\r' || raw.back() == '\t' || raw.back() == ','))
        raw.remove_suffix(1);
    return raw;
}

int jx_dom_find_field_raw(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    char** out_ptr, size_t* out_len)
{
    try {
        ondemand::parser parser;
        std::string_view raw;
        int nav = navigate_fields_raw(parser, buf, len, fields, field_lens, field_count, raw);
        if (nav == 2) return -1; // parse error
        if (nav == 1) {
            *out_ptr = new char[4];
            std::memcpy(*out_ptr, "null", 4);
            *out_len = 4;
            return 0;
        }
        raw = trim_raw_json(raw);
        *out_len = raw.size();
        *out_ptr = new char[raw.size()];
        std::memcpy(*out_ptr, raw.data(), raw.size());
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// DOM field + length — navigate fields, then compute length.
//
// Return codes via *out_len:
//   >= 0 : success (length value written as decimal string in *out_ptr)
//   -2   : unsupported type (caller should fall back to normal pipeline)
// Function return: 0 = success, -1 = error.
// ---------------------------------------------------------------------------

int jx_dom_field_length(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::parser parser;
        dom::element result;
        int nav = navigate_fields(buf, len, fields, field_lens, field_count, parser, result);
        if (nav == 2) return -1; // parse error
        if (nav == 1) {
            // null → length 0
            *out_ptr = new char[1];
            (*out_ptr)[0] = '0';
            *out_len = 1;
            return 0;
        }

        int64_t length;
        switch (result.type()) {
            case dom::element_type::ARRAY:
                length = static_cast<int64_t>(dom::array(result).size());
                break;
            case dom::element_type::OBJECT:
                length = static_cast<int64_t>(dom::object(result).size());
                break;
            case dom::element_type::STRING:
                length = static_cast<int64_t>(result.get_string().value().size());
                break;
            case dom::element_type::NULL_VALUE:
                length = 0;
                break;
            default:
                // Int/Double/Bool — unsupported, signal fallback
                *out_ptr = nullptr;
                *out_len = static_cast<size_t>(-2);
                return 0;
        }

        std::string s = std::to_string(length);
        *out_len = s.size();
        *out_ptr = new char[s.size()];
        std::memcpy(*out_ptr, s.data(), s.size());
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// DOM field + keys — navigate fields, then compute keys.
//
// Return: 0 = success, -1 = error.
// *out_len = -2 means unsupported type (caller falls back).
// ---------------------------------------------------------------------------

int jx_dom_field_keys(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    int sorted,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::parser parser;
        dom::element result;
        int nav = navigate_fields(buf, len, fields, field_lens, field_count, parser, result);
        if (nav == 2) return -1; // parse error
        if (nav == 1) {
            // null → no output (jq produces no output for keys on null)
            *out_ptr = nullptr;
            *out_len = static_cast<size_t>(-2);
            return 0;
        }

        switch (result.type()) {
            case dom::element_type::OBJECT: {
                dom::object obj = dom::object(result);
                // Collect keys
                std::vector<std::string_view> keys;
                for (auto field : obj) {
                    keys.push_back(field.key);
                }
                if (sorted) std::sort(keys.begin(), keys.end());
                // Build JSON array string
                std::string s;
                s.push_back('[');
                for (size_t i = 0; i < keys.size(); i++) {
                    if (i > 0) s.push_back(',');
                    json_escape(keys[i], s);
                }
                s.push_back(']');
                *out_len = s.size();
                *out_ptr = new char[s.size()];
                std::memcpy(*out_ptr, s.data(), s.size());
                return 0;
            }
            case dom::element_type::ARRAY: {
                dom::array arr = dom::array(result);
                size_t count = arr.size();
                // Build [0,1,2,...,n-1]
                std::string s;
                s.push_back('[');
                for (size_t i = 0; i < count; i++) {
                    if (i > 0) s.push_back(',');
                    s += std::to_string(i);
                }
                s.push_back(']');
                *out_len = s.size();
                *out_ptr = new char[s.size()];
                std::memcpy(*out_ptr, s.data(), s.size());
                return 0;
            }
            default:
                // Unsupported type — signal fallback
                *out_ptr = nullptr;
                *out_len = static_cast<size_t>(-2);
                return 0;
        }
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// Batch field extraction — parse once, extract N field chains.
//
// Each chain is an array of field segments (e.g., ["actor", "login"]).
// Results are packed into a single heap buffer:
//   [u32 len1][bytes1][u32 len2][bytes2]...
// Missing fields produce "null" (4 bytes). Caller frees with jx_minify_free.
// ---------------------------------------------------------------------------

int jx_dom_find_fields_raw(
    const char* buf, size_t len,
    const char* const* const* chains,
    const size_t* const* chain_lens,
    const size_t* chain_counts,
    size_t num_chains,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::parser parser;
        dom::element doc;
        auto err = parser.parse(buf, len).get(doc);
        if (err) return -1;

        // First pass: extract all fields, collect serialized results
        std::string packed;
        packed.reserve(num_chains * 32); // rough estimate

        for (size_t i = 0; i < num_chains; i++) {
            dom::element cur = doc;
            bool found = true;
            for (size_t j = 0; j < chain_counts[i]; j++) {
                std::string_view key(chains[i][j], chain_lens[i][j]);
                if (cur.type() != dom::element_type::OBJECT) { found = false; break; }
                auto field_err = cur.at_key(key).get(cur);
                if (field_err) { found = false; break; }
            }

            std::string s;
            if (found) {
                s = simdjson::to_string(cur);
            } else {
                s = "null";
            }

            // Pack: [u32 len][bytes]
            uint32_t slen = static_cast<uint32_t>(s.size());
            packed.append(reinterpret_cast<const char*>(&slen), 4);
            packed.append(s);
        }

        *out_len = packed.size();
        *out_ptr = new char[packed.size()];
        std::memcpy(*out_ptr, packed.data(), packed.size());
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// Reusable DOM parser — avoids per-call parser construction.
// ---------------------------------------------------------------------------

struct JxDomParser {
    dom::parser dom;           // used for length/keys (needs validated tree)
    ondemand::parser ondemand; // used for raw field extraction (preserves original bytes)
};

JxDomParser* jx_dom_parser_new() {
    try {
        return new JxDomParser();
    } catch (...) { return nullptr; }
}

void jx_dom_parser_free(JxDomParser* p) {
    delete p;
}

int jx_dom_find_field_raw_reuse(
    JxDomParser* p,
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    char** out_ptr, size_t* out_len)
{
    try {
        std::string_view raw;
        int nav = navigate_fields_raw(p->ondemand, buf, len,
                                       fields, field_lens, field_count, raw);
        if (nav == 2) return -1;
        if (nav == 1) {
            *out_ptr = new char[4];
            std::memcpy(*out_ptr, "null", 4);
            *out_len = 4;
            return 0;
        }
        raw = trim_raw_json(raw);
        *out_len = raw.size();
        *out_ptr = new char[raw.size()];
        std::memcpy(*out_ptr, raw.data(), raw.size());
        return 0;
    } catch (...) { return -1; }
}

int jx_dom_find_fields_raw_reuse(
    JxDomParser* p,
    const char* buf, size_t len,
    const char* const* const* chains,
    const size_t* const* chain_lens,
    const size_t* chain_counts,
    size_t num_chains,
    char** out_ptr, size_t* out_len)
{
    try {
        std::string packed;
        packed.reserve(num_chains * 32);
        for (size_t i = 0; i < num_chains; i++) {
            std::string_view raw;
            int nav = navigate_fields_raw(p->ondemand, buf, len,
                                           chains[i], chain_lens[i],
                                           chain_counts[i], raw);
            std::string_view val;
            if (nav == 0) {
                val = trim_raw_json(raw);
            } else {
                val = std::string_view("null", 4);
            }
            uint32_t slen = static_cast<uint32_t>(val.size());
            packed.append(reinterpret_cast<const char*>(&slen), 4);
            packed.append(val);
        }
        *out_len = packed.size();
        *out_ptr = new char[packed.size()];
        std::memcpy(*out_ptr, packed.data(), packed.size());
        return 0;
    } catch (...) { return -1; }
}

int jx_dom_field_length_reuse(
    JxDomParser* p,
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::element result;
        int nav = navigate_fields(buf, len, fields, field_lens, field_count, p->dom, result);
        if (nav == 2) return -1;
        if (nav == 1) {
            // null → length not applicable, signal to Rust to fall back
            return -2;
        }
        std::string s;
        switch (result.type()) {
            case dom::element_type::OBJECT: {
                dom::object obj;
                if (result.get(obj)) return -2;
                size_t count = 0;
                for (auto kv : obj) { (void)kv; count++; }
                s = std::to_string(count);
                break;
            }
            case dom::element_type::ARRAY: {
                dom::array arr;
                if (result.get(arr)) return -2;
                size_t count = 0;
                for (auto el : arr) { (void)el; count++; }
                s = std::to_string(count);
                break;
            }
            case dom::element_type::STRING: {
                // signal to Rust: string length needs full Value
                return -2;
            }
            default:
                return -2;
        }
        *out_len = s.size();
        *out_ptr = new char[s.size()];
        std::memcpy(*out_ptr, s.data(), s.size());
        return 0;
    } catch (...) { return -1; }
}

int jx_dom_field_keys_reuse(
    JxDomParser* p,
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    int sorted,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::element result;
        int nav = navigate_fields(buf, len, fields, field_lens, field_count, p->dom, result);
        if (nav == 2) return -1;
        if (nav == 1) return -2;

        std::string s;
        switch (result.type()) {
            case dom::element_type::OBJECT: {
                dom::object obj;
                if (result.get(obj)) return -2;
                std::vector<std::string_view> keys;
                for (auto kv : obj) {
                    keys.push_back(kv.key);
                }
                if (sorted) std::sort(keys.begin(), keys.end());
                s = "[";
                for (size_t i = 0; i < keys.size(); i++) {
                    if (i > 0) s += ",";
                    json_escape(keys[i], s);
                }
                s += "]";
                break;
            }
            case dom::element_type::ARRAY: {
                dom::array arr;
                if (result.get(arr)) return -2;
                size_t count = 0;
                for (auto el : arr) { (void)el; count++; }
                s = "[";
                for (size_t i = 0; i < count; i++) {
                    if (i > 0) s += ",";
                    s += std::to_string(i);
                }
                s += "]";
                break;
            }
            default:
                return -2;
        }
        *out_len = s.size();
        *out_ptr = new char[s.size()];
        std::memcpy(*out_ptr, s.data(), s.size());
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// Array map field — iterate root array, extract one field chain per element.
//
// Returns: 0 = success, -1 = error, -2 = root is not an array (fallback).
// wrap_array: 1 = output as JSON array [v1,v2,...], 0 = one value per line.
// Missing fields produce "null". Caller frees with jx_minify_free.
// ---------------------------------------------------------------------------

int jx_dom_array_map_field(
    const char* buf, size_t len,
    const char** prefix, const size_t* prefix_lens, size_t prefix_count,
    const char** fields, const size_t* field_lens, size_t field_count,
    int wrap_array,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::parser parser;
        dom::element root;
        auto err = parser.parse(buf, len).get(root);
        if (err) return -1;

        // Navigate prefix field chain to reach the array
        dom::element target = root;
        for (size_t i = 0; i < prefix_count; i++) {
            std::string_view key(prefix[i], prefix_lens[i]);
            if (target.type() != dom::element_type::OBJECT) return -2;
            auto field_err = target.at_key(key).get(target);
            if (field_err) return -2;
        }
        if (target.type() != dom::element_type::ARRAY) return -2;

        std::string out;
        out.reserve(len / 4);
        if (wrap_array) out.push_back('[');

        bool first = true;
        for (dom::element elem : dom::array(target)) {
            if (!first) {
                if (wrap_array) out.push_back(',');
                else out.push_back('\n');
            }
            first = false;

            // null → null (matches jq); non-object/non-null → fallback to evaluator
            if (elem.type() == dom::element_type::NULL_VALUE) {
                out += "null";
                continue;
            }
            if (elem.type() != dom::element_type::OBJECT) return -2;

            // Navigate field chain within this element
            dom::element cur = elem;
            bool found = true;
            for (size_t i = 0; i < field_count; i++) {
                std::string_view key(fields[i], field_lens[i]);
                if (cur.type() != dom::element_type::OBJECT) { found = false; break; }
                auto field_err = cur.at_key(key).get(cur);
                if (field_err) { found = false; break; }
            }

            if (found) {
                out += simdjson::to_string(cur);
            } else {
                out += "null";
            }
        }

        if (wrap_array) out.push_back(']');

        *out_len = out.size();
        *out_ptr = new char[out.size()];
        std::memcpy(*out_ptr, out.data(), out.size());
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// Array map fields obj — iterate array, extract N fields per element, emit
// {"key1":v1,"key2":v2,...} per element.
//
// Returns: 0 = success, -1 = error, -2 = target is not an array (fallback).
// wrap_array: 1 = output as JSON array [{...},{...},...], 0 = one obj per line.
// Missing fields produce "null". Caller frees with jx_minify_free.
//
// `keys`/`key_lens`: JSON-encoded key strings (e.g. `"user"` with quotes).
// `fields`/`field_lens`: bare field names to extract from each element.
// Both arrays have `field_count` entries (1:1 correspondence).
// ---------------------------------------------------------------------------

int jx_dom_array_map_fields_obj(
    const char* buf, size_t len,
    const char** prefix, const size_t* prefix_lens, size_t prefix_count,
    const char** keys, const size_t* key_lens,
    const char** fields, const size_t* field_lens,
    size_t field_count,
    int wrap_array,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::parser parser;
        dom::element root;
        auto err = parser.parse(buf, len).get(root);
        if (err) return -1;

        // Navigate prefix field chain to reach the array
        dom::element target = root;
        for (size_t i = 0; i < prefix_count; i++) {
            std::string_view key(prefix[i], prefix_lens[i]);
            if (target.type() != dom::element_type::OBJECT) return -2;
            auto field_err = target.at_key(key).get(target);
            if (field_err) return -2;
        }
        if (target.type() != dom::element_type::ARRAY) return -2;

        std::string out;
        out.reserve(len / 4);
        if (wrap_array) out.push_back('[');

        bool first_elem = true;
        for (dom::element elem : dom::array(target)) {
            if (!first_elem) {
                if (wrap_array) out.push_back(',');
                else out.push_back('\n');
            }
            first_elem = false;

            // null → all-null obj (matches jq); non-object/non-null → fallback
            bool is_null = (elem.type() == dom::element_type::NULL_VALUE);
            if (!is_null && elem.type() != dom::element_type::OBJECT) return -2;

            out.push_back('{');
            for (size_t i = 0; i < field_count; i++) {
                if (i > 0) out.push_back(',');
                // Emit pre-encoded key
                out.append(keys[i], key_lens[i]);
                out.push_back(':');

                if (is_null) {
                    out += "null";
                } else {
                    // Extract field value
                    std::string_view field_name(fields[i], field_lens[i]);
                    dom::element val;
                    bool found = !elem.at_key(field_name).get(val);
                    if (found) {
                        out += simdjson::to_string(val);
                    } else {
                        out += "null";
                    }
                }
            }
            out.push_back('}');
        }

        if (wrap_array) out.push_back(']');

        *out_len = out.size();
        *out_ptr = new char[out.size()];
        std::memcpy(*out_ptr, out.data(), out.size());
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// Field has — check if an object has a given key.
//
// Returns: 0 = success (*result set), -1 = error, -2 = fallback (not object).
// *result: 1 = has key, 0 = does not have key.
// ---------------------------------------------------------------------------

int jx_dom_field_has(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    const char* key, size_t key_len,
    int* result)
{
    try {
        dom::parser parser;
        dom::element target;
        int nav = navigate_fields(buf, len, fields, field_lens, field_count, parser, target);
        if (nav == 2) return -1;
        if (nav == 1) return -2;
        if (target.type() != dom::element_type::OBJECT) return -2;
        dom::element val;
        *result = target.at_key(std::string_view(key, key_len)).get(val) ? 0 : 1;
        return 0;
    } catch (...) { return -1; }
}

int jx_dom_field_has_reuse(
    JxDomParser* p,
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    const char* key, size_t key_len,
    int* result)
{
    try {
        dom::element target;
        int nav = navigate_fields(buf, len, fields, field_lens, field_count, p->dom, target);
        if (nav == 2) return -1;
        if (nav == 1) return -2;
        if (target.type() != dom::element_type::OBJECT) return -2;
        dom::element val;
        *result = target.at_key(std::string_view(key, key_len)).get(val) ? 0 : 1;
        return 0;
    } catch (...) { return -1; }
}

// ---------------------------------------------------------------------------
// Array map builtin — iterate array, apply a builtin per element.
//
// ops: 0=length, 1=keys, 2=type, 3=has
// Returns: 0 = success, -1 = error, -2 = fallback (not array or unsupported element).
// ---------------------------------------------------------------------------

static const char* type_string(dom::element_type t) {
    switch (t) {
        case dom::element_type::OBJECT: return "\"object\"";
        case dom::element_type::ARRAY:  return "\"array\"";
        case dom::element_type::STRING: return "\"string\"";
        case dom::element_type::INT64:
        case dom::element_type::UINT64:
        case dom::element_type::DOUBLE: return "\"number\"";
        case dom::element_type::BOOL:   return "\"boolean\"";
        case dom::element_type::NULL_VALUE: return "\"null\"";
        default: return "\"null\"";
    }
}

int jx_dom_array_map_builtin(
    const char* buf, size_t len,
    const char** prefix, const size_t* prefix_lens, size_t prefix_count,
    int op, int sorted,
    const char* arg, size_t arg_len,
    int wrap_array,
    char** out_ptr, size_t* out_len)
{
    try {
        dom::parser parser;
        dom::element root;
        auto err = parser.parse(buf, len).get(root);
        if (err) return -1;

        dom::element target = root;
        for (size_t i = 0; i < prefix_count; i++) {
            std::string_view key(prefix[i], prefix_lens[i]);
            if (target.type() != dom::element_type::OBJECT) return -2;
            auto field_err = target.at_key(key).get(target);
            if (field_err) return -2;
        }
        if (target.type() != dom::element_type::ARRAY) return -2;

        std::string out;
        out.reserve(len / 8);
        if (wrap_array) out.push_back('[');

        bool first = true;
        for (dom::element elem : dom::array(target)) {
            if (!first) {
                if (wrap_array) out.push_back(',');
                else out.push_back('\n');
            }
            first = false;

            switch (op) {
                case 0: { // length
                    switch (elem.type()) {
                        case dom::element_type::OBJECT:
                            out += std::to_string(dom::object(elem).size());
                            break;
                        case dom::element_type::ARRAY:
                            out += std::to_string(dom::array(elem).size());
                            break;
                        case dom::element_type::STRING: {
                            std::string_view sv;
                            if (!elem.get(sv)) {
                                out += std::to_string(sv.size());
                            } else {
                                return -2;
                            }
                            break;
                        }
                        case dom::element_type::NULL_VALUE:
                            out += "null";
                            break;
                        default:
                            return -2;
                    }
                    break;
                }
                case 1: { // keys
                    switch (elem.type()) {
                        case dom::element_type::OBJECT: {
                            dom::object obj = dom::object(elem);
                            std::vector<std::string_view> keys;
                            for (auto field : obj) keys.push_back(field.key);
                            if (sorted) std::sort(keys.begin(), keys.end());
                            out.push_back('[');
                            for (size_t i = 0; i < keys.size(); i++) {
                                if (i > 0) out.push_back(',');
                                json_escape(keys[i], out);
                            }
                            out.push_back(']');
                            break;
                        }
                        case dom::element_type::ARRAY: {
                            size_t count = dom::array(elem).size();
                            out.push_back('[');
                            for (size_t i = 0; i < count; i++) {
                                if (i > 0) out.push_back(',');
                                out += std::to_string(i);
                            }
                            out.push_back(']');
                            break;
                        }
                        case dom::element_type::NULL_VALUE:
                            return -2;
                        default:
                            return -2;
                    }
                    break;
                }
                case 2: { // type
                    out += type_string(elem.type());
                    break;
                }
                case 3: { // has
                    if (elem.type() != dom::element_type::OBJECT) return -2;
                    dom::element val;
                    std::string_view k(arg, arg_len);
                    out += (elem.at_key(k).get(val) ? "false" : "true");
                    break;
                }
                default:
                    return -1;
            }
        }

        if (wrap_array) out.push_back(']');

        *out_len = out.size();
        *out_ptr = new char[out.size()];
        std::memcpy(*out_ptr, out.data(), out.size());
        return 0;
    } catch (...) { return -1; }
}

} // extern "C"
