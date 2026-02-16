//! Input preprocessing: BOM stripping, JSON/NDJSON parsing into Values.

use anyhow::{Context, Result};

use crate::value::Value;

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
        let val = crate::simdjson::dom_parse_to_value(&padded, trimmed.len())
            .context("failed to parse NDJSON line")?;
        values.push(val);
    }
    Ok(())
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
}
