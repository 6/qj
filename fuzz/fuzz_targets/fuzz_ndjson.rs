#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::simdjson::{iterate_many_count, iterate_many_extract_field, pad_buffer};

// Feed arbitrary bytes to iterate_many (NDJSON batch parser).
// Tests both count and field extraction paths.
fuzz_target!(|data: &[u8]| {
    let buf = pad_buffer(data);
    let _ = iterate_many_count(&buf, data.len(), 1_000_000);
    let _ = iterate_many_extract_field(&buf, data.len(), 1_000_000, "a");
});
