#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::filter;
use qj::simdjson::{dom_parse_to_value, pad_buffer};
use qj::value::Value;

// Structured fuzzer: split input into JSON + filter, parse both, evaluate.
// Catches panics in builtins, type coercion, arithmetic, recursive eval.
fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }

    // First 2 bytes determine split point between JSON and filter.
    let split = u16::from_le_bytes([data[0], data[1]]) as usize;
    let rest = &data[2..];
    if rest.is_empty() {
        return;
    }
    let split = split % rest.len();

    let json_part = &rest[..split];
    let filter_part = &rest[split..];

    // Parse filter (must be valid UTF-8).
    let Ok(filter_str) = std::str::from_utf8(filter_part) else {
        return;
    };
    let Ok(filter) = filter::parse(filter_str) else {
        return;
    };

    // Parse JSON via DOM.
    let buf = pad_buffer(json_part);
    let Ok(value) = dom_parse_to_value(&buf, json_part.len()) else {
        return;
    };

    // Evaluate, collecting up to 1000 outputs to bound execution.
    let mut count = 0;
    filter::eval::eval_filter(&filter, &value, &mut |_v: Value| {
        count += 1;
        if count >= 1000 {
            return;
        }
    });
});
