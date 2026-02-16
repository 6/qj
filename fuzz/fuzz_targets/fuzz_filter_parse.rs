#![no_main]
use libfuzzer_sys::fuzz_target;

// Feed arbitrary UTF-8 strings to the jq filter parser.
// Catches panics in lexer/parser: uncovered unwrap()s, infinite loops,
// stack overflows on deeply nested expressions.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = qj::filter::parse(s);
    }
});
