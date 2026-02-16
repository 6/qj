#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::output::{OutputConfig, OutputMode, write_value};
use qj::simdjson::{dom_parse_to_value, pad_buffer};

// Feed arbitrary bytes through DOM parsing, then format with each output mode.
// Catches panics in string unescaping (raw mode), indentation edge cases
// (pretty-print), and compact mode serialization.
fuzz_target!(|data: &[u8]| {
    let buf = pad_buffer(data);
    let Ok(value) = dom_parse_to_value(&buf, data.len()) else {
        return;
    };

    for mode in [OutputMode::Compact, OutputMode::Pretty, OutputMode::Raw] {
        let config = OutputConfig {
            mode,
            ..OutputConfig::default()
        };
        let mut out = Vec::new();
        let _ = write_value(&mut out, &value, &config);
    }

    // Also test with sort_keys enabled.
    let config = OutputConfig {
        mode: OutputMode::Compact,
        sort_keys: true,
        ..OutputConfig::default()
    };
    let mut out = Vec::new();
    let _ = write_value(&mut out, &value, &config);
});
