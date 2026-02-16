//! Raw FFI declarations for the simdjson C-linkage bridge.
//!
//! These must match bridge.cpp exactly.

use std::ffi::c_char;

#[repr(C)]
pub(super) struct JxParser {
    _opaque: [u8; 0],
}

#[repr(C)]
pub(super) struct JxDomParser {
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
    pub(super) fn jx_dom_to_flat_via_tape(
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

    pub(super) fn jx_dom_find_fields_raw(
        buf: *const c_char,
        len: usize,
        chains: *const *const *const c_char,
        chain_lens: *const *const usize,
        chain_counts: *const usize,
        num_chains: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;

    // --- Reusable DOM parser ---

    pub(super) fn jx_dom_parser_new() -> *mut JxDomParser;
    pub(super) fn jx_dom_parser_free(p: *mut JxDomParser);

    pub(super) fn jx_dom_find_field_raw_reuse(
        p: *mut JxDomParser,
        buf: *const c_char,
        len: usize,
        fields: *const *const c_char,
        field_lens: *const usize,
        field_count: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;

    pub(super) fn jx_dom_find_fields_raw_reuse(
        p: *mut JxDomParser,
        buf: *const c_char,
        len: usize,
        chains: *const *const *const c_char,
        chain_lens: *const *const usize,
        chain_counts: *const usize,
        num_chains: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;

    pub(super) fn jx_dom_field_length_reuse(
        p: *mut JxDomParser,
        buf: *const c_char,
        len: usize,
        fields: *const *const c_char,
        field_lens: *const usize,
        field_count: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;

    pub(super) fn jx_dom_field_keys_reuse(
        p: *mut JxDomParser,
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

    pub(super) fn jx_dom_array_map_field(
        buf: *const c_char,
        len: usize,
        prefix: *const *const c_char,
        prefix_lens: *const usize,
        prefix_count: usize,
        fields: *const *const c_char,
        field_lens: *const usize,
        field_count: usize,
        wrap_array: i32,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;
}
