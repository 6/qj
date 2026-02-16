#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::simdjson::{Parser, pad_buffer};

// Feed arbitrary bytes to the On-Demand parser via FFI.
// Any crash here is a bug in simdjson or the bridge.
fuzz_target!(|data: &[u8]| {
    let buf = pad_buffer(data);
    let mut parser = match Parser::new() {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Ok(mut doc) = parser.parse(&buf, data.len()) {
        let _ = doc.doc_type();
        let _ = doc.find_field_str("a");
    }
});
