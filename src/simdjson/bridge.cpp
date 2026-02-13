// bridge.cpp — C-linkage wrapper around simdjson On-Demand API for Rust FFI.
//
// Design principles:
//   - All functions return int (0 = success, positive = simdjson error code).
//   - All functions use try/catch — no C++ exceptions cross FFI boundary.
//   - Caller provides pre-padded buffers (SIMDJSON_PADDING extra zeroed bytes).
//   - JxParser bundles parser + document together (document borrows parser).

#include "simdjson.h"
#include <cstring>

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

} // extern "C"
