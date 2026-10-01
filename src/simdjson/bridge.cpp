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

#include <cstring>

using namespace simdjson;

// Copies a NUL-terminated name into buf (truncated to cap bytes).
static void jx_copy_name(const std::string& name, char* buf, size_t cap) {
    if (cap == 0) return;
    size_t n = name.size() < cap - 1 ? name.size() : cap - 1;
    std::memcpy(buf, name.data(), n);
    buf[n] = '\0';
}

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

// simdjson's kernels ("implementations": icelake, haswell, westmere and
// fallback on x86-64, arm64 on ARM, and so on), in its order of preference.
// jx_tape_parser_new's parsers use the active one: the first this CPU
// supports, or the one SIMDJSON_FORCE_IMPLEMENTATION names.

size_t jx_simdjson_implementation_count() {
    return get_available_implementations().size();
}

// Copies the name of kernel i into buf and returns 1 if this CPU can run
// it, 0 if it can't, -1 if there is no kernel i.
int jx_simdjson_implementation(size_t i, char* buf, size_t cap) {
    const auto& list = get_available_implementations();
    if (i >= list.size()) return -1;
    const implementation* impl = list.begin()[i];
    jx_copy_name(impl->name(), buf, cap);
    return impl->supported_by_runtime_system() ? 1 : 0;
}

// Copies the active kernel's name into buf. It is "unsupported" when
// SIMDJSON_FORCE_IMPLEMENTATION names no kernel compiled in (every parse
// then fails); simdjson doesn't check that the CPU can run a forced one.
void jx_simdjson_active_implementation(char* buf, size_t cap) {
    jx_copy_name(get_active_implementation()->name(), buf, cap);
}

// A parser on the named kernel, whatever the active one is. Null if no
// kernel of that name is compiled in, if this CPU can't run it, or if
// allocation fails.
JxTapeParser* jx_tape_parser_new_implementation(const char* name) {
    const implementation* impl = get_available_implementations()[name];
    if (!impl || !impl->supported_by_runtime_system()) return nullptr;
    try {
        auto* p = new JxTapeParser();
        // dom::parser::allocate keeps the implementation it has, growing it
        // to each document's size.
        if (impl->create_dom_parser_implementation(0, DEFAULT_MAX_DEPTH,
                                                   p->parser.implementation)) {
            delete p;
            return nullptr;
        }
        return p;
    } catch (...) {
        return nullptr;
    }
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
