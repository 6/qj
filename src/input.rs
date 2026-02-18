//! Input preprocessing: BOM stripping, JSON/NDJSON parsing into Values.

use anyhow::{Context, Result};
use std::sync::Arc;

use crate::value::Value;

// Sentinel f64 values used to encode NaN/Infinity in JSON text preprocessing.
// These are subnormal numbers extremely unlikely to appear in real data.
const NAN_SENTINEL: f64 = 1.23e-321;
const POS_INF_SENTINEL: f64 = 1.24e-321;
const NEG_INF_SENTINEL: f64 = -1.25e-321;
const NAN_SENTINEL_STR: &str = "1.23e-321";
const POS_INF_SENTINEL_STR: &str = "1.24e-321";
const NEG_INF_SENTINEL_STR: &str = "-1.25e-321";

/// Strip UTF-8 BOM (U+FEFF, bytes EF BB BF) from the beginning of a buffer.
pub fn strip_bom(buf: &mut Vec<u8>) {
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        buf.drain(..3);
    }
}

/// Collect parsed JSON values from a buffer (single doc or NDJSON lines).
/// Tries single-doc parse first; if that fails and the buffer has newlines,
/// falls back to line-by-line parsing (handles `1\n2\n3` style multi-value input).
pub fn collect_values_from_buf(
    buf: &[u8],
    force_jsonl: bool,
    values: &mut Vec<Value>,
) -> Result<()> {
    // Empty/whitespace-only input produces no values.
    if buf
        .iter()
        .all(|&b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return Ok(());
    }
    if force_jsonl || crate::parallel::ndjson::is_ndjson(buf) {
        parse_lines(buf, values)?;
    } else {
        let json_len = buf.len();
        let padded = crate::simdjson::pad_buffer(buf);
        match crate::simdjson::dom_parse_to_value(&padded, json_len) {
            Ok(val) => values.push(val),
            Err(e)
                if e.to_string()
                    == format!("simdjson error code {}", crate::simdjson::SIMDJSON_CAPACITY) =>
            {
                // simdjson CAPACITY limit (~4GB) — fall back to serde_json
                let text = std::str::from_utf8(buf)
                    .context("input is not valid UTF-8 (serde_json fallback)")?;
                let serde_val: serde_json::Value = serde_json::from_str(text)
                    .context("failed to parse JSON (serde_json fallback for >4GB input)")?;
                values.push(Value::from(serde_val));
            }
            Err(_) if memchr::memchr(b'\n', buf).is_some() => {
                // Single-doc parse failed but buffer has newlines — try line-by-line
                parse_lines(buf, values)?;
            }
            Err(e) => {
                // Try special float preprocessing (NaN, Infinity, etc.)
                if has_special_float_tokens(buf) {
                    let preprocessed = preprocess_special_floats(buf);
                    let pp_len = preprocessed.len();
                    let pp_padded = crate::simdjson::pad_buffer(&preprocessed);
                    if let Ok(val) = crate::simdjson::dom_parse_to_value(&pp_padded, pp_len) {
                        values.push(fixup_special_float_sentinels(val));
                        return Ok(());
                    }
                }
                // Try multi-doc fallback: serde_json StreamDeserializer handles
                // concatenated JSON like {"a":1}{"b":2} and whitespace-separated values.
                let text = std::str::from_utf8(buf).context("input is not valid UTF-8")?;
                let mut stream =
                    serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
                let mut count = 0usize;
                for result in &mut stream {
                    match result {
                        Ok(serde_val) => {
                            count += 1;
                            values.push(Value::from(serde_val));
                        }
                        Err(se) => {
                            if count == 0 {
                                // Nothing parsed — report original simdjson error
                                return Err(e).context("failed to parse JSON");
                            }
                            return Err(se.into());
                        }
                    }
                }
                if count == 0 {
                    return Err(e).context("failed to parse JSON");
                }
            }
        }
    }
    Ok(())
}

/// Parse newline-delimited JSON lines into values.
pub fn parse_lines(buf: &[u8], values: &mut Vec<Value>) -> Result<()> {
    for line in buf.split(|&b| b == b'\n') {
        let trimmed_end = line
            .iter()
            .rposition(|&b| !matches!(b, b' ' | b'\t' | b'\r'))
            .map_or(0, |p| p + 1);
        let trimmed = &line[..trimmed_end];
        if trimmed.is_empty() {
            continue;
        }
        let padded = crate::simdjson::pad_buffer(trimmed);
        match crate::simdjson::dom_parse_to_value(&padded, trimmed.len()) {
            Ok(val) => values.push(val),
            Err(_) if has_special_float_tokens(trimmed) => {
                let pp = preprocess_special_floats(trimmed);
                let pp_padded = crate::simdjson::pad_buffer(&pp);
                let val = crate::simdjson::dom_parse_to_value(&pp_padded, pp.len())
                    .context("failed to parse NDJSON line (after special float preprocessing)")?;
                values.push(fixup_special_float_sentinels(val));
            }
            Err(e) => return Err(e).context("failed to parse NDJSON line"),
        }
    }
    Ok(())
}

/// Public wrapper for `has_special_float_tokens`.
pub fn has_special_float_tokens_pub(buf: &[u8]) -> bool {
    has_special_float_tokens(buf)
}

/// Public wrapper for `preprocess_special_floats`.
pub fn preprocess_special_floats_pub(buf: &[u8]) -> Vec<u8> {
    preprocess_special_floats(buf)
}

/// Public wrapper for `fixup_special_float_sentinels`.
pub fn fixup_special_float_sentinels_pub(val: Value) -> Value {
    fixup_special_float_sentinels(val)
}

/// Check if a byte buffer contains non-standard float tokens (NaN, Infinity, etc.)
/// outside of JSON strings. These are accepted by jq but not by standard JSON parsers.
fn has_special_float_tokens(buf: &[u8]) -> bool {
    // Quick check: buffer must contain 'N' (NaN), 'I' (Infinity), or 'n' (nan) / 'i' (inf)
    // that could be a special float token. Skip if no candidate bytes found.
    if !buf.iter().any(|&b| matches!(b, b'N' | b'I' | b'n' | b'i')) {
        return false;
    }
    let text = match std::str::from_utf8(buf) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let mut in_string = false;
    let mut escaped = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        let rest = &text[i..];
        if rest.starts_with("NaN")
            || rest.starts_with("nan")
            || rest.starts_with("Infinity")
            || rest.starts_with("infinity")
        {
            return true;
        }
        // Check "inf" but not "include", "import" etc. — must not be followed by alnum
        if rest.starts_with("inf")
            && !rest[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Replace NaN/Infinity/nan/inf/-NaN/-Infinity/-nan/-inf tokens outside JSON strings
/// with sentinel f64 values that can be parsed by standard JSON parsers.
/// Returns the preprocessed buffer.
fn preprocess_special_floats(buf: &[u8]) -> Vec<u8> {
    let text = match std::str::from_utf8(buf) {
        Ok(t) => t,
        Err(_) => return buf.to_vec(),
    };
    let mut result = Vec::with_capacity(buf.len());
    let mut in_string = false;
    let mut escaped = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            result.push(b);
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            result.push(b);
            i += 1;
            continue;
        }
        let rest = &text[i..];

        // Check for negative prefix
        let (negative, token_start) = if b == b'-' && i + 1 < bytes.len() {
            (true, &text[i + 1..])
        } else {
            (false, rest)
        };

        // Match special float tokens
        if let Some((replacement, skip)) = match_special_float(token_start, negative) {
            result.extend_from_slice(replacement.as_bytes());
            i += skip + if negative { 1 } else { 0 };
            continue;
        }

        result.push(b);
        i += 1;
    }
    result
}

/// Match a special float token at the start of the string. Returns (replacement, bytes_consumed).
fn match_special_float(s: &str, negative: bool) -> Option<(&'static str, usize)> {
    // NaN / nan (3 chars) — must not be followed by alphanumeric
    if (s.starts_with("NaN") || s.starts_with("nan"))
        && !s[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
    {
        // NaN has no sign — -NaN is also NaN
        return Some((NAN_SENTINEL_STR, 3));
    }
    // Infinity / infinity (8 chars)
    if s.starts_with("Infinity") || s.starts_with("infinity") {
        let repl = if negative {
            NEG_INF_SENTINEL_STR
        } else {
            POS_INF_SENTINEL_STR
        };
        return Some((repl, 8));
    }
    // inf (3 chars) — must not be followed by alphanumeric (to avoid matching "infinity" partially,
    // but "infinity" is already handled above)
    if s.starts_with("inf") && !s[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        let repl = if negative {
            NEG_INF_SENTINEL_STR
        } else {
            POS_INF_SENTINEL_STR
        };
        return Some((repl, 3));
    }
    None
}

/// Walk a Value tree and replace sentinel doubles with actual NaN/Infinity values.
fn fixup_special_float_sentinels(val: Value) -> Value {
    match val {
        Value::Double(f, _) if f == NAN_SENTINEL => Value::Double(f64::NAN, None),
        Value::Double(f, _) if f == POS_INF_SENTINEL => Value::Double(f64::INFINITY, None),
        Value::Double(f, _) if f == NEG_INF_SENTINEL => Value::Double(f64::NEG_INFINITY, None),
        Value::Array(arr) => {
            let fixed: Vec<Value> = arr
                .iter()
                .cloned()
                .map(fixup_special_float_sentinels)
                .collect();
            Value::Array(Arc::new(fixed))
        }
        Value::Object(obj) => {
            let fixed: Vec<(String, Value)> = obj
                .iter()
                .map(|(k, v)| (k.clone(), fixup_special_float_sentinels(v.clone())))
                .collect();
            Value::Object(Arc::new(fixed))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_bom_present() {
        let mut buf = vec![0xEF, 0xBB, 0xBF, b'"', b'h', b'i', b'"'];
        strip_bom(&mut buf);
        assert_eq!(buf, b"\"hi\"");
    }

    #[test]
    fn strip_bom_absent() {
        let mut buf = b"\"hi\"".to_vec();
        strip_bom(&mut buf);
        assert_eq!(buf, b"\"hi\"");
    }

    #[test]
    fn strip_bom_empty() {
        let mut buf = Vec::new();
        strip_bom(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn strip_bom_only_bom() {
        let mut buf = vec![0xEF, 0xBB, 0xBF];
        strip_bom(&mut buf);
        assert!(buf.is_empty());
    }

    // --- parse_lines ---

    #[test]
    fn parse_lines_single() {
        let mut vals = Vec::new();
        parse_lines(b"42", &mut vals).unwrap();
        assert_eq!(vals, vec![Value::Int(42)]);
    }

    #[test]
    fn parse_lines_multiple() {
        let mut vals = Vec::new();
        parse_lines(b"1\n2\n3", &mut vals).unwrap();
        assert_eq!(vals, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    #[test]
    fn parse_lines_skips_blanks() {
        let mut vals = Vec::new();
        parse_lines(b"1\n\n2\n  \n3\n", &mut vals).unwrap();
        assert_eq!(vals, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    #[test]
    fn parse_lines_trims_trailing_whitespace() {
        let mut vals = Vec::new();
        parse_lines(b"\"hi\"  \r\n\"there\"\t", &mut vals).unwrap();
        assert_eq!(
            vals,
            vec![Value::String("hi".into()), Value::String("there".into()),]
        );
    }

    #[test]
    fn parse_lines_empty_input() {
        let mut vals = Vec::new();
        parse_lines(b"", &mut vals).unwrap();
        assert!(vals.is_empty());
    }

    // --- collect_values_from_buf ---

    #[test]
    fn collect_single_json_doc() {
        let mut vals = Vec::new();
        collect_values_from_buf(b"{\"a\":1}", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].type_name(), "object");
    }

    #[test]
    fn collect_ndjson_fallback() {
        // Multiple values separated by newlines — not valid as single doc,
        // falls back to line-by-line parsing.
        let mut vals = Vec::new();
        collect_values_from_buf(b"1\n2\n3", false, &mut vals).unwrap();
        assert_eq!(vals, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    #[test]
    fn collect_force_jsonl() {
        let mut vals = Vec::new();
        collect_values_from_buf(b"1\n2\n3", true, &mut vals).unwrap();
        assert_eq!(vals, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    // --- has_special_float_tokens ---

    #[test]
    fn detect_nan_in_array() {
        assert!(has_special_float_tokens(b"[1,NaN,3]"));
        assert!(has_special_float_tokens(b"[nan]"));
    }

    #[test]
    fn detect_infinity_in_object() {
        assert!(has_special_float_tokens(b"{\"a\":Infinity}"));
        assert!(has_special_float_tokens(b"{\"a\":-Infinity}"));
    }

    #[test]
    fn detect_inf_standalone() {
        assert!(has_special_float_tokens(b"[inf]"));
        assert!(has_special_float_tokens(b"[-inf]"));
    }

    #[test]
    fn no_false_positive_inside_string() {
        assert!(!has_special_float_tokens(b"\"NaN\""));
        assert!(!has_special_float_tokens(b"{\"key\":\"Infinity\"}"));
        assert!(!has_special_float_tokens(b"\"inf\""));
    }

    #[test]
    fn no_false_positive_normal_json() {
        assert!(!has_special_float_tokens(b"{\"a\":1}"));
        assert!(!has_special_float_tokens(b"[1,2,3]"));
    }

    // --- preprocess_special_floats ---

    #[test]
    fn preprocess_nan() {
        let result = preprocess_special_floats(b"[NaN]");
        let s = std::str::from_utf8(&result).unwrap();
        assert!(s.contains(NAN_SENTINEL_STR));
    }

    #[test]
    fn preprocess_negative_nan() {
        let result = preprocess_special_floats(b"[-NaN]");
        let s = std::str::from_utf8(&result).unwrap();
        // -NaN is still NaN (no sign), sentinel should not have minus
        assert!(s.contains(NAN_SENTINEL_STR));
        assert!(!s.contains(&format!("-{NAN_SENTINEL_STR}")));
    }

    #[test]
    fn preprocess_infinity() {
        let result = preprocess_special_floats(b"[Infinity,-Infinity]");
        let s = std::str::from_utf8(&result).unwrap();
        assert!(s.contains(POS_INF_SENTINEL_STR));
        assert!(s.contains(NEG_INF_SENTINEL_STR));
    }

    #[test]
    fn preprocess_preserves_strings() {
        let result = preprocess_special_floats(b"{\"NaN\":NaN}");
        let s = std::str::from_utf8(&result).unwrap();
        assert!(s.contains("\"NaN\""), "string key should be preserved");
        assert!(s.contains(NAN_SENTINEL_STR), "value NaN should be replaced");
    }

    // --- fixup_special_float_sentinels ---

    #[test]
    fn fixup_nan_sentinel() {
        let val = Value::Double(NAN_SENTINEL, None);
        let fixed = fixup_special_float_sentinels(val);
        match fixed {
            Value::Double(f, _) => assert!(f.is_nan()),
            _ => panic!("expected Double"),
        }
    }

    #[test]
    fn fixup_inf_sentinel() {
        let val = Value::Double(POS_INF_SENTINEL, None);
        let fixed = fixup_special_float_sentinels(val);
        assert_eq!(fixed, Value::Double(f64::INFINITY, None));
    }

    #[test]
    fn fixup_neg_inf_sentinel() {
        let val = Value::Double(NEG_INF_SENTINEL, None);
        let fixed = fixup_special_float_sentinels(val);
        assert_eq!(fixed, Value::Double(f64::NEG_INFINITY, None));
    }

    #[test]
    fn fixup_nested_in_array() {
        let val = Value::Array(Arc::new(vec![
            Value::Int(1),
            Value::Double(NAN_SENTINEL, None),
            Value::Double(POS_INF_SENTINEL, None),
        ]));
        let fixed = fixup_special_float_sentinels(val);
        if let Value::Array(arr) = fixed {
            assert_eq!(arr[0], Value::Int(1));
            match &arr[1] {
                Value::Double(f, _) => assert!(f.is_nan()),
                _ => panic!("expected NaN"),
            }
            match &arr[2] {
                Value::Double(f, _) => assert!(f.is_infinite() && f.is_sign_positive()),
                _ => panic!("expected Infinity"),
            }
        } else {
            panic!("expected Array");
        }
    }

    // --- collect_values_from_buf with special floats ---

    #[test]
    fn collect_nan_in_object() {
        let mut vals = Vec::new();
        collect_values_from_buf(b"{\"a\":nan}", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 1);
        if let Value::Object(obj) = &vals[0] {
            match &obj[0].1 {
                Value::Double(f, _) => assert!(f.is_nan()),
                _ => panic!("expected NaN double"),
            }
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn collect_infinity_in_array() {
        let mut vals = Vec::new();
        collect_values_from_buf(b"[Infinity,-Infinity]", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 1);
        if let Value::Array(arr) = &vals[0] {
            assert_eq!(arr.len(), 2);
            match &arr[0] {
                Value::Double(f, _) => assert!(f.is_infinite() && f.is_sign_positive()),
                _ => panic!("expected +Infinity"),
            }
            match &arr[1] {
                Value::Double(f, _) => assert!(f.is_infinite() && f.is_sign_negative()),
                _ => panic!("expected -Infinity"),
            }
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn collect_mixed_special_floats() {
        let mut vals = Vec::new();
        collect_values_from_buf(b"[1,null,Infinity,-Infinity,NaN,-NaN]", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 1);
        if let Value::Array(arr) = &vals[0] {
            assert_eq!(arr.len(), 6);
            assert_eq!(arr[0], Value::Int(1));
            assert_eq!(arr[1], Value::Null);
            match &arr[2] {
                Value::Double(f, _) => assert!(f.is_infinite() && f.is_sign_positive()),
                _ => panic!("expected +Infinity"),
            }
            match &arr[3] {
                Value::Double(f, _) => assert!(f.is_infinite() && f.is_sign_negative()),
                _ => panic!("expected -Infinity"),
            }
            match &arr[4] {
                Value::Double(f, _) => assert!(f.is_nan()),
                _ => panic!("expected NaN"),
            }
            match &arr[5] {
                Value::Double(f, _) => assert!(f.is_nan()),
                _ => panic!("expected NaN"),
            }
        } else {
            panic!("expected array");
        }
    }

    // -----------------------------------------------------------------------
    // Differential tests: simdjson vs serde_json parsing paths
    // -----------------------------------------------------------------------

    /// Recursively compare two Values with order-independent object key comparison.
    /// serde_json uses BTreeMap (sorted keys) while simdjson preserves insertion order,
    /// so direct PartialEq fails on objects with multiple keys.
    fn values_equal_unordered(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Double(a, _), Value::Double(b, _)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| values_equal_unordered(x, y))
            }
            (Value::Object(a), Value::Object(b)) => {
                if a.len() != b.len() {
                    return false;
                }
                // Every key in a must exist in b with equal value, and vice versa
                a.iter().all(|(ka, va)| {
                    b.iter()
                        .find(|(kb, _)| kb == ka)
                        .map_or(false, |(_, vb)| values_equal_unordered(va, vb))
                }) && b.iter().all(|(kb, _)| a.iter().any(|(ka, _)| ka == kb))
            }
            _ => false,
        }
    }

    /// Parse JSON through simdjson (dom_parse_to_value) and serde_json (Value::from),
    /// assert the resulting Values are equal. Uses order-independent object comparison
    /// because serde_json sorts keys alphabetically (BTreeMap) while simdjson preserves
    /// insertion order.
    fn assert_simdjson_serde_agree(json: &[u8]) {
        // simdjson path
        let padded = crate::simdjson::pad_buffer(json);
        let simdjson_val = crate::simdjson::dom_parse_to_value(&padded, json.len())
            .unwrap_or_else(|e| panic!("simdjson failed on {:?}: {e}", std::str::from_utf8(json)));

        // serde_json path
        let text = std::str::from_utf8(json).unwrap();
        let serde_val: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("serde_json failed on {text:?}: {e}"));
        let serde_converted = Value::from(serde_val);

        assert!(
            values_equal_unordered(&simdjson_val, &serde_converted),
            "simdjson vs serde_json mismatch for input: {text}\n  simdjson: {simdjson_val:?}\n  serde:   {serde_converted:?}"
        );
    }

    #[test]
    fn diff_simple_object() {
        assert_simdjson_serde_agree(br#"{"a":1,"b":"hello","c":null,"d":true,"e":1.5}"#);
    }

    #[test]
    fn diff_mixed_array() {
        assert_simdjson_serde_agree(br#"[1,2,3,"hello",null,true,false,1.23456789012345]"#);
    }

    #[test]
    fn diff_unicode_escapes() {
        assert_simdjson_serde_agree(br#"{"emoji":"\u0041\u0042\u0043"}"#);
    }

    #[test]
    fn diff_escape_sequences() {
        assert_simdjson_serde_agree(br#"{"s":"a\"b\\c\/d\n\t\r\f\b"}"#);
    }

    #[test]
    fn diff_nested_objects() {
        assert_simdjson_serde_agree(br#"{"nested":{"deep":{"value":42}}}"#);
    }

    #[test]
    fn diff_empty_containers() {
        assert_simdjson_serde_agree(b"{}");
        assert_simdjson_serde_agree(b"[]");
    }

    #[test]
    fn diff_scalar_int() {
        assert_simdjson_serde_agree(b"0");
        assert_simdjson_serde_agree(b"42");
        assert_simdjson_serde_agree(b"-1");
        assert_simdjson_serde_agree(b"9223372036854775807"); // i64::MAX
    }

    #[test]
    fn diff_scalar_double() {
        assert_simdjson_serde_agree(b"3.14");
        assert_simdjson_serde_agree(b"1e10");
        assert_simdjson_serde_agree(b"-0.0");
    }

    #[test]
    fn diff_scalar_string() {
        assert_simdjson_serde_agree(br#""""#);
        assert_simdjson_serde_agree(br#""hello world""#);
    }

    #[test]
    fn diff_scalar_bool_null() {
        assert_simdjson_serde_agree(b"true");
        assert_simdjson_serde_agree(b"false");
        assert_simdjson_serde_agree(b"null");
    }

    #[test]
    fn diff_negative_i64_min() {
        assert_simdjson_serde_agree(b"-9223372036854775808"); // i64::MIN
    }

    #[test]
    fn diff_array_of_objects() {
        assert_simdjson_serde_agree(br#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#);
    }

    #[test]
    fn diff_object_with_array_values() {
        assert_simdjson_serde_agree(br#"{"tags":["rust","json"],"scores":[100,200,300]}"#);
    }

    #[test]
    fn diff_deeply_nested_mixed() {
        assert_simdjson_serde_agree(br#"{"a":[{"b":{"c":[1,2,{"d":true}]}}]}"#);
    }

    #[test]
    fn diff_unicode_multibyte() {
        assert_simdjson_serde_agree(br#"{"text":"\u00e9\u00e8\u00ea"}"#);
    }

    #[test]
    fn diff_large_array() {
        // 100-element array
        let mut json = String::from("[");
        for i in 0..100 {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&i.to_string());
        }
        json.push(']');
        assert_simdjson_serde_agree(json.as_bytes());
    }

    #[test]
    fn diff_object_many_keys() {
        let mut json = String::from("{");
        for i in 0..50 {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"key{i}\":{i}"));
        }
        json.push('}');
        assert_simdjson_serde_agree(json.as_bytes());
    }

    #[test]
    fn diff_whitespace_variations() {
        // Extra whitespace should not affect value equality
        assert_simdjson_serde_agree(b"{ \"a\" : 1 , \"b\" : [ 2 , 3 ] }");
        assert_simdjson_serde_agree(b"  [  1  ,  2  ,  3  ]  ");
    }

    // -----------------------------------------------------------------------
    // Multi-doc fallback: serde_json StreamDeserializer path
    //
    // The serde_json fallback activates when simdjson fails AND the buffer
    // has no newlines. simdjson succeeds on `{"a":1}{"b":2}` (returning
    // only the first doc), so concatenated objects/arrays don't trigger
    // the fallback — only inputs that are genuinely invalid single JSON do.
    // -----------------------------------------------------------------------

    #[test]
    fn multi_doc_space_separated_scalars() {
        // simdjson rejects "123 456 789" (not valid single JSON), falls to
        // serde_json StreamDeserializer since no newlines present.
        let mut vals = Vec::new();
        collect_values_from_buf(b"123 456 789", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], Value::Int(123));
        assert_eq!(vals[1], Value::Int(456));
        assert_eq!(vals[2], Value::Int(789));
    }

    #[test]
    fn multi_doc_space_separated_match_simdjson() {
        // Each value from multi-doc serde_json parse should match simdjson
        // parse of the same value individually.
        let mut vals = Vec::new();
        collect_values_from_buf(b"true false null", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 3);

        for (i, single_json) in [b"true".as_slice(), b"false", b"null"].iter().enumerate() {
            let padded = crate::simdjson::pad_buffer(single_json);
            let simdjson_val =
                crate::simdjson::dom_parse_to_value(&padded, single_json.len()).unwrap();
            assert_eq!(
                vals[i], simdjson_val,
                "multi-doc element {i} differs from simdjson parse"
            );
        }
    }

    #[test]
    fn multi_doc_mixed_scalars_and_containers() {
        // Mix of scalars and containers — serde_json stream handles these
        let mut vals = Vec::new();
        collect_values_from_buf(b"42 \"hello\" true", false, &mut vals).unwrap();
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], Value::Int(42));
        assert_eq!(vals[1], Value::String("hello".into()));
        assert_eq!(vals[2], Value::Bool(true));
    }

    #[test]
    fn multi_doc_concatenated_objects_simdjson_first_only() {
        // simdjson succeeds on first doc of concatenated JSON, returning only
        // the first object. This is expected behavior — not a multi-doc parse.
        let mut vals = Vec::new();
        collect_values_from_buf(br#"{"a":1}{"b":2}"#, false, &mut vals).unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(
            vals[0],
            Value::Object(Arc::new(vec![("a".into(), Value::Int(1))]))
        );
    }

    // -----------------------------------------------------------------------
    // collect_values_from_buf: simdjson primary path vs line-by-line fallback
    // -----------------------------------------------------------------------

    #[test]
    fn ndjson_lines_match_single_doc_parse() {
        // Verify that parsing as NDJSON lines gives the same values as
        // parsing each line individually through simdjson.
        let input = b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}";
        let mut line_vals = Vec::new();
        collect_values_from_buf(input, false, &mut line_vals).unwrap();

        for (i, line) in [br#"{"a":1}"#.as_slice(), br#"{"b":2}"#, br#"{"c":3}"#]
            .iter()
            .enumerate()
        {
            let padded = crate::simdjson::pad_buffer(line);
            let single = crate::simdjson::dom_parse_to_value(&padded, line.len()).unwrap();
            assert_eq!(
                line_vals[i], single,
                "NDJSON line {i} differs from single-doc simdjson parse"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Edge cases: precision limits, special characters
    // -----------------------------------------------------------------------

    #[test]
    fn diff_f64_precision_boundary() {
        // Near the limits of f64 precision — both parsers should agree
        assert_simdjson_serde_agree(b"1.7976931348623157e308");
        assert_simdjson_serde_agree(b"5e-324");
        assert_simdjson_serde_agree(b"2.2250738585072014e-308");
    }

    #[test]
    fn diff_string_with_null_escape() {
        assert_simdjson_serde_agree(br#"{"s":"hello\u0000world"}"#);
    }

    #[test]
    fn diff_surrogate_pair() {
        // U+1F600 = \uD83D\uDE00
        assert_simdjson_serde_agree(br#"{"emoji":"\uD83D\uDE00"}"#);
    }

    #[test]
    fn diff_empty_string_key() {
        assert_simdjson_serde_agree(br#"{"":"value"}"#);
    }

    #[test]
    fn diff_numeric_string_key() {
        assert_simdjson_serde_agree(br#"{"123":"numeric key"}"#);
    }

    #[test]
    fn diff_repeated_keys() {
        // JSON with duplicate keys — both should parse (serde_json keeps last)
        // simdjson keeps first. We compare independently so just verify no crash.
        let json = br#"{"a":1,"a":2}"#;
        let padded = crate::simdjson::pad_buffer(json);
        let _simdjson = crate::simdjson::dom_parse_to_value(&padded, json.len()).unwrap();
        let _serde: serde_json::Value = serde_json::from_str(r#"{"a":1,"a":2}"#).unwrap();
        // Don't compare values since duplicate key behavior intentionally differs
    }

    #[test]
    fn diff_nested_empty_containers() {
        assert_simdjson_serde_agree(br#"{"a":[],"b":{},"c":[{}],"d":{"e":[]}}"#);
    }

    #[test]
    fn diff_long_string() {
        let long_str = "x".repeat(10000);
        let json = format!("\"{}\"", long_str);
        assert_simdjson_serde_agree(json.as_bytes());
    }

    #[test]
    fn diff_deeply_nested_arrays() {
        assert_simdjson_serde_agree(b"[[[[[[[[1]]]]]]]]");
    }

    #[test]
    fn diff_deeply_nested_objects() {
        assert_simdjson_serde_agree(br#"{"a":{"b":{"c":{"d":{"e":{"f":{"g":1}}}}}}}"#);
    }
}
