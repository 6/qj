mod bridge;

pub use bridge::{
    Document, JsonType, Parser, dom_find_field_raw, dom_parse_to_value, iterate_many_count,
    iterate_many_extract_field, minify, pad_buffer, padding, read_padded, read_padded_file,
};
