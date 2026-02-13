mod bridge;

pub use bridge::{
    Document, JsonType, Parser, iterate_many_count, iterate_many_extract_field, pad_buffer,
    padding, read_padded,
};
