#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::output::{OutputConfig, OutputMode, write_value};
use qj::value::Value;

// Fuzz the computed double formatting path (Value::Double with no raw text).
// For each random f64, formats it and verifies the output:
//   1. Does not panic
//   2. Is valid JSON (parses back to an f64)
//   3. Round-trips: parsed value equals the original f64
//
// This catches formatting bugs like the itoa/ryu divergence where doubles
// near i64 boundaries produced incorrect output.
fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }

    let f = f64::from_le_bytes(data[..8].try_into().unwrap());

    // Skip NaN and infinity — these format as "null" by design.
    if f.is_nan() || f.is_infinite() {
        return;
    }

    // Format as computed double (no raw text).
    let value = Value::Double(f, None);
    let config = OutputConfig {
        mode: OutputMode::Compact,
        ..OutputConfig::default()
    };
    let mut out = Vec::new();
    write_value(&mut out, &value, &config).expect("write_value should not fail");

    let output = std::str::from_utf8(&out).expect("output must be valid UTF-8");
    let output = output.trim();

    // Must parse back to a valid f64.
    let parsed: f64 = output
        .parse()
        .unwrap_or_else(|e| panic!("output {output:?} for f64 {f} is not a valid number: {e}"));

    // Round-trip: parsed value must equal the original.
    // Use to_bits for exact comparison (handles -0.0 normalization).
    let f_normalized = if f == 0.0 { 0.0f64 } else { f };
    assert_eq!(
        parsed.to_bits(),
        f_normalized.to_bits(),
        "round-trip failed: f={f}, output={output:?}, parsed={parsed}"
    );
});
