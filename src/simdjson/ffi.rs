//! Raw FFI declarations for the simdjson C-linkage bridge.
//!
//! These must match bridge.cpp exactly.

use std::ffi::c_char;

#[repr(C)]
pub(super) struct JxParser {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    pub(super) fn jx_parser_new() -> *mut JxParser;
    pub(super) fn jx_parser_free(p: *mut JxParser);
    pub(super) fn jx_simdjson_padding() -> usize;

    pub(super) fn jx_parse_ondemand(p: *mut JxParser, buf: *const c_char, len: usize) -> i32;

    pub(super) fn jx_doc_find_field_str(
        p: *mut JxParser,
        key: *const c_char,
        key_len: usize,
        out: *mut *const c_char,
        out_len: *mut usize,
    ) -> i32;
    pub(super) fn jx_doc_find_field_int64(
        p: *mut JxParser,
        key: *const c_char,
        key_len: usize,
        out: *mut i64,
    ) -> i32;
    pub(super) fn jx_doc_find_field_double(
        p: *mut JxParser,
        key: *const c_char,
        key_len: usize,
        out: *mut f64,
    ) -> i32;
    pub(super) fn jx_doc_type(p: *mut JxParser, out_type: *mut i32) -> i32;

    pub(super) fn jx_iterate_many_count(
        buf: *const c_char,
        len: usize,
        batch_size: usize,
        out_count: *mut u64,
    ) -> i32;
    pub(super) fn jx_iterate_many_extract_field(
        buf: *const c_char,
        len: usize,
        batch_size: usize,
        field_name: *const c_char,
        field_name_len: usize,
        out_total_bytes: *mut u64,
    ) -> i32;

    pub(super) fn jx_dom_to_flat(
        buf: *const c_char,
        len: usize,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> i32;
    pub(super) fn jx_flat_buffer_free(ptr: *mut u8);

    pub(super) fn jx_minify(
        buf: *const c_char,
        len: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;
    pub(super) fn jx_minify_free(ptr: *mut c_char);

    pub(super) fn jx_dom_find_field_raw(
        buf: *const c_char,
        len: usize,
        fields: *const *const c_char,
        field_lens: *const usize,
        field_count: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;

    pub(super) fn jx_dom_field_length(
        buf: *const c_char,
        len: usize,
        fields: *const *const c_char,
        field_lens: *const usize,
        field_count: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;

    pub(super) fn jx_dom_field_keys(
        buf: *const c_char,
        len: usize,
        fields: *const *const c_char,
        field_lens: *const usize,
        field_count: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;
}
