//! Raw FFI declarations for the simdjson C-linkage bridge.
//!
//! These must match bridge.cpp exactly.

#[repr(C)]
pub(super) struct JxTapeParser {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    pub(super) fn jx_simdjson_padding() -> usize;

    pub(super) fn jx_tape_parser_new() -> *mut JxTapeParser;
    pub(super) fn jx_tape_parser_free(p: *mut JxTapeParser);
    pub(super) fn jx_tape_parse(
        p: *mut JxTapeParser,
        buf: *const u8,
        len: usize,
        tape: *mut *const u64,
        strings: *mut *const u8,
        structurals: *mut *const u32,
        n_structurals: *mut usize,
    ) -> i32;
    pub(super) fn jx_tape_parser_capacity(p: *mut JxTapeParser) -> usize;
}
