mod bridge;

pub use bridge::{
    Document, JsonType, Parser, dom_parse_to_value, iterate_many_count, iterate_many_extract_field,
    pad_buffer, padding, read_padded,
};
