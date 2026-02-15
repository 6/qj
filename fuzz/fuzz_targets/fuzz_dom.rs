#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::simdjson::{dom_parse_to_value, pad_buffer};

// Feed arbitrary bytes to the DOM parser → flat token buffer → Value tree.
// Exercises the C++ flatten_element + Rust decode_value path.
fuzz_target!(|data: &[u8]| {
    let buf = pad_buffer(data);
    let _ = dom_parse_to_value(&buf, data.len());
});
