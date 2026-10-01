//! simdjson (vendored in `simdjson/`) behind a small C-linkage bridge
//! (`bridge.cpp`): its DOM parser, whose tape `src/io` turns into jq values.

mod bridge;
mod ffi;

pub use bridge::{
    AllocError, Implementation, Tape, TapeParser, active_implementation,
    checked_active_implementation, implementations, pad_buffer, padding, supported_implementations,
    tape_error,
};
