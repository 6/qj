// bridge.cpp — C-linkage wrapper around simdjson APIs for Rust FFI.
//
// Design principles:
//   - All functions return int (0 = success, positive = simdjson error code).
//   - All functions use try/catch — no C++ exceptions cross FFI boundary.
//   - Caller provides pre-padded buffers (SIMDJSON_PADDING extra zeroed bytes).
//   - JxParser bundles parser + document together (document borrows parser).

#include "simdjson.h"
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
            // Just consume the document to advance the parser.
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
        for (auto doc_result : stream) {
            ondemand::document& doc = doc_result.value();
            std::string_view val;
            auto field_err = doc.find_field(field).get_string().get(val);
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
// DOM API — parse to flat token buffer for Rust Value construction.
//
// Token format (little-endian):
//   Null:        tag=0
//   Bool:        tag=1, u8 (0 or 1)
//   Int:         tag=2, i64
//   Double:      tag=3, f64
//   String:      tag=4, u32 len, bytes[len]
//   ArrayStart:  tag=5, u32 count
//   ArrayEnd:    tag=6
//   ObjectStart: tag=7, u32 count
//   ObjectEnd:   tag=8
//
// Object keys are emitted as String tokens before each value.
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

static void flatten_element(std::vector<uint8_t>& out, dom::element el) {
    switch (el.type()) {
        case dom::element_type::NULL_VALUE:
            emit_u8(out, TAG_NULL);
            break;
        case dom::element_type::BOOL:
            emit_u8(out, TAG_BOOL);
            emit_u8(out, el.get_bool().value() ? 1 : 0);
            break;
        case dom::element_type::INT64:
            emit_u8(out, TAG_INT);
            emit_i64(out, el.get_int64().value());
            break;
        case dom::element_type::UINT64: {
            // If it fits in i64, use INT; otherwise use DOUBLE.
            uint64_t u = el.get_uint64().value();
            if (u <= static_cast<uint64_t>(INT64_MAX)) {
                emit_u8(out, TAG_INT);
                emit_i64(out, static_cast<int64_t>(u));
            } else {
                emit_u8(out, TAG_DOUBLE);
                emit_f64(out, static_cast<double>(u));
            }
            break;
        }
        case dom::element_type::DOUBLE:
            emit_u8(out, TAG_DOUBLE);
            emit_f64(out, el.get_double().value());
            break;
        case dom::element_type::STRING:
            emit_string(out, el.get_string().value());
            break;
        case dom::element_type::ARRAY: {
            dom::array arr = el.get_array().value();
            // Count elements first
            uint32_t count = 0;
            for (auto it = arr.begin(); it != arr.end(); ++it) count++;
            emit_u8(out, TAG_ARRAY_START);
            emit_u32(out, count);
            for (dom::element child : arr) {
                flatten_element(out, child);
            }
            emit_u8(out, TAG_ARRAY_END);
            break;
        }
        case dom::element_type::OBJECT: {
            dom::object obj = el.get_object().value();
            uint32_t count = 0;
            for (auto it = obj.begin(); it != obj.end(); ++it) count++;
            emit_u8(out, TAG_OBJECT_START);
            emit_u32(out, count);
            for (auto field : obj) {
                // Key
                emit_string(out, field.key);
                // Value
                flatten_element(out, field.value);
            }
            emit_u8(out, TAG_OBJECT_END);
            break;
        }
    }
}

// Parse a JSON document using the DOM API and write a flat token buffer.
// Caller provides `buf` with SIMDJSON_PADDING extra zeroed bytes.
// On success, sets *out_ptr and *out_len to a heap-allocated buffer
// that the caller must free with jx_flat_buffer_free().
int jx_dom_to_flat(const char* buf, size_t len,
                   uint8_t** out_ptr, size_t* out_len) {
    try {
        dom::parser parser;
        dom::element doc;
        auto err = parser.parse(buf, len).get(doc);
        if (err) return static_cast<int>(err);

        std::vector<uint8_t> flat;
        flat.reserve(len); // Rough pre-allocation
        flatten_element(flat, doc);

        // Copy to heap buffer for Rust ownership.
        *out_len = flat.size();
        *out_ptr = new uint8_t[flat.size()];
        std::memcpy(*out_ptr, flat.data(), flat.size());
        return 0;
    } catch (...) {
        return -1;
    }
}

void jx_flat_buffer_free(uint8_t* ptr) {
    delete[] ptr;
}

} // extern "C"
