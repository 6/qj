// bridge.cpp — C-linkage wrapper around simdjson for Rust FFI.
//
// qj uses one part of simdjson: the DOM parser, whose tape, string buffer
// and structural indexes are handed to Rust after a successful parse, so
// Rust builds jq values directly from them (src/io/simd.rs): no
// intermediate serialization, and number literals are read from the source
// text at their structural index (jq keeps literal text).
//
// Design principles:
//   - Functions that can fail return int (0 = success, positive = simdjson
//     error code, -1 = an exception, e.g. allocation failure).
//   - No C++ exception crosses the FFI boundary.
//   - Callers provide buffers with SIMDJSON_PADDING readable bytes after the
//     text (their contents don't matter).

#include "simdjson.h"

using namespace simdjson;

extern "C" {

size_t jx_simdjson_padding() {
    return SIMDJSON_PADDING;
}

struct JxTapeParser {
    dom::parser parser;
};

JxTapeParser* jx_tape_parser_new() {
    try {
        return new JxTapeParser();
    } catch (...) {
        return nullptr;
    }
}

void jx_tape_parser_free(JxTapeParser* p) {
    delete p;
}

// Parse exactly one JSON document in buf[0..len). The caller guarantees
// SIMDJSON_PADDING readable bytes after buf[len - 1] and that buf does not
// start with a UTF-8 BOM (simdjson would skip it; jq does not, mid-stream).
// On success (0) the out-parameters point into the parser, valid until the
// next call or until the parser is freed.
int jx_tape_parse(JxTapeParser* p, const uint8_t* buf, size_t len,
                  const uint64_t** tape, const uint8_t** strings,
                  const uint32_t** structurals, size_t* n_structurals) {
    try {
        dom::element root;
        auto err = p->parser.parse(buf, len, false).get(root);
        if (err) return static_cast<int>(err);
        *tape = p->parser.doc.tape.get();
        *strings = p->parser.doc.string_buf.get();
        *structurals = p->parser.implementation->structural_indexes.get();
        *n_structurals = p->parser.implementation->n_structural_indexes;
        return 0;
    } catch (...) {
        return -1;
    }
}

// Bytes currently allocated by the parser for documents (its capacity).
size_t jx_tape_parser_capacity(JxTapeParser* p) {
    return p->parser.capacity();
}

} // extern "C"
