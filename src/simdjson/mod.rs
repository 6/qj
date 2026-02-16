mod ffi;
mod types;

mod bridge;

pub use bridge::{
    DomParser, FlatBuffer, SIMDJSON_CAPACITY, dom_array_map_builtin, dom_array_map_field,
    dom_array_map_fields_obj, dom_field_has, dom_field_keys, dom_field_length, dom_find_field_raw,
    dom_find_fields_raw, dom_parse_to_flat_buf, dom_parse_to_flat_buf_tape, dom_parse_to_value,
    dom_parse_to_value_fast, minify,
};
pub(crate) use bridge::{
    TAG_ARRAY_START, TAG_BOOL, TAG_DOUBLE, TAG_INT, TAG_NULL, TAG_OBJECT_START, TAG_STRING,
    decode_value,
};
pub use types::{
    Document, JsonType, PaddedFile, Parser, iterate_many_count, iterate_many_extract_field,
    pad_buffer, padding, read_padded, read_padded_file,
};
