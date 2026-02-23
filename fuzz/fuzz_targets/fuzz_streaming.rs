#![no_main]
use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use qj::filter::{self, Filter};
use qj::output::{write_value, OutputConfig, OutputMode};
use qj::value::Value;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Lookup tables (shared with fuzz_eval)
// ---------------------------------------------------------------------------

const INTERESTING_DOUBLES: &[f64] = &[
    0.0, -0.0, 0.5, 1.5, 3.14, -1.0, 99.9, f64::NAN, f64::INFINITY, f64::NEG_INFINITY,
    f64::MIN, f64::MAX, f64::EPSILON, 1e308, 5e-324,
];

const STRINGS: &[&str] = &["", "a", "hello", "null", "true", "42", "foo bar", "\n\t"];

const KEYS: &[&str] = &["a", "b", "c", "d"];

// ---------------------------------------------------------------------------
// Fuzz value — reused from fuzz_eval
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum FuzzValue {
    Null,
    Bool(bool),
    SmallInt(i8),
    BigInt(i64),
    Double(u8),
    Str(u8),
    Array(Vec<FuzzValue>),
    Object(Vec<(u8, FuzzValue)>),
}

impl FuzzValue {
    fn arbitrary_depth(u: &mut Unstructured<'_>, depth: usize) -> arbitrary::Result<Self> {
        if depth == 0 {
            let choice = u.int_in_range(0u8..=5)?;
            return match choice {
                0 => Ok(FuzzValue::Null),
                1 => Ok(FuzzValue::Bool(u.arbitrary()?)),
                2 => Ok(FuzzValue::SmallInt(u.arbitrary()?)),
                3 => Ok(FuzzValue::BigInt(u.arbitrary()?)),
                4 => Ok(FuzzValue::Double(u.arbitrary()?)),
                _ => Ok(FuzzValue::Str(u.arbitrary()?)),
            };
        }
        let choice = u.int_in_range(0u8..=7)?;
        match choice {
            0 => Ok(FuzzValue::Null),
            1 => Ok(FuzzValue::Bool(u.arbitrary()?)),
            2 => Ok(FuzzValue::SmallInt(u.arbitrary()?)),
            3 => Ok(FuzzValue::BigInt(u.arbitrary()?)),
            4 => Ok(FuzzValue::Double(u.arbitrary()?)),
            5 => Ok(FuzzValue::Str(u.arbitrary()?)),
            6 => {
                let len = u.int_in_range(0u8..=5)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(FuzzValue::arbitrary_depth(u, depth - 1)?);
                }
                Ok(FuzzValue::Array(items))
            }
            _ => {
                let len = u.int_in_range(0u8..=5)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push((u.arbitrary()?, FuzzValue::arbitrary_depth(u, depth - 1)?));
                }
                Ok(FuzzValue::Object(items))
            }
        }
    }

    fn to_value(&self) -> Value {
        match self {
            FuzzValue::Null => Value::Null,
            FuzzValue::Bool(b) => Value::Bool(*b),
            FuzzValue::SmallInt(n) => Value::Int(*n as i64),
            FuzzValue::BigInt(n) => Value::Int(*n),
            FuzzValue::Double(idx) => {
                let f = INTERESTING_DOUBLES[*idx as usize % INTERESTING_DOUBLES.len()];
                Value::Double(f, None)
            }
            FuzzValue::Str(idx) => {
                Value::String(STRINGS[*idx as usize % STRINGS.len()].to_string())
            }
            FuzzValue::Array(items) => {
                Value::Array(Arc::new(items.iter().map(|v| v.to_value()).collect()))
            }
            FuzzValue::Object(items) => Value::Object(Arc::new(
                items
                    .iter()
                    .map(|(k, v)| {
                        let key = KEYS[*k as usize % KEYS.len()].to_string();
                        (key, v.to_value())
                    })
                    .collect(),
            )),
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzValue {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        FuzzValue::arbitrary_depth(u, 4)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_compact(v: &Value) -> String {
    let config = OutputConfig {
        mode: OutputMode::Compact,
        ..OutputConfig::default()
    };
    let mut buf = Vec::new();
    let _ = write_value(&mut buf, v, &config);
    String::from_utf8(buf).unwrap_or_default()
}

/// Returns true if the value contains NaN (which breaks equality).
fn contains_nan(v: &Value) -> bool {
    match v {
        Value::Double(f, _) => f.is_nan(),
        Value::Array(arr) => arr.iter().any(contains_nan),
        Value::Object(obj) => obj.iter().any(|(_, v)| contains_nan(v)),
        _ => false,
    }
}

/// Returns true if the value contains duplicate keys in any object.
/// Duplicate keys cause fromstream to merge, breaking roundtrip identity.
fn contains_duplicate_keys(v: &Value) -> bool {
    match v {
        Value::Object(obj) => {
            let mut seen = std::collections::HashSet::new();
            for (k, v) in obj.iter() {
                if !seen.insert(k.as_str()) {
                    return true;
                }
                if contains_duplicate_keys(v) {
                    return true;
                }
            }
            false
        }
        Value::Array(arr) => arr.iter().any(contains_duplicate_keys),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Fuzz target: tostream | fromstream roundtrip identity
//
// Property: for any JSON value (without NaN or duplicate keys),
// fromstream(tostream) == identity.
// ---------------------------------------------------------------------------

fuzz_target!(|value: FuzzValue| {
    let input = value.to_value();

    // NaN breaks equality (NaN != NaN); duplicate keys break roundtrip
    // because tostream emits both but fromstream's setpath overwrites.
    if contains_nan(&input) || contains_duplicate_keys(&input) {
        return;
    }

    // Build filter: fromstream(tostream)
    let tostream = Filter::Builtin("tostream".to_string(), vec![]);
    let roundtrip = Filter::Builtin("fromstream".to_string(), vec![tostream]);

    // Collect outputs
    let mut results = Vec::new();
    filter::eval::eval_filter(&roundtrip, &input, &mut |v: Value| {
        results.push(v);
    });

    // Should produce exactly one output identical to input
    assert_eq!(
        results.len(),
        1,
        "fromstream(tostream) should produce exactly 1 output, got {}\ninput: {}",
        results.len(),
        format_compact(&input),
    );

    let output_str = format_compact(&results[0]);
    let input_str = format_compact(&input);
    assert_eq!(
        input_str, output_str,
        "fromstream(tostream) roundtrip mismatch\ninput:  {input_str}\noutput: {output_str}",
    );
});
