//! simdjson (vendored in `simdjson/`) behind a small C-linkage bridge
//! (`bridge.cpp`): its DOM parser, whose tape `src/io` turns into jq values.

mod bridge;
mod ffi;

pub use bridge::{AllocError, Tape, TapeParser, pad_buffer, padding, tape_error};
