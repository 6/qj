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
}
