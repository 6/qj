/// JSON output formatting.
///
/// Writes `Value` directly to a `Write` sink — no intermediate `String`
/// allocation. Uses `itoa` for integers and `ryu` for floats.
use std::io::{self, Write};

use crate::value::Value;

/// Output formatting mode.
#[derive(Debug, Clone, Copy)]
pub enum OutputMode {
    /// Pretty-printed with indentation (default for TTY).
    Pretty,
    /// Compact single-line output (`-c`).
    Compact,
    /// Raw string output (`-r`) — strings without quotes.
    Raw,
}

/// Configuration for output formatting.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub mode: OutputMode,
    /// Indentation string (default "  ", or "\t" with --tab).
    pub indent: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Pretty,
            indent: "  ".to_string(),
        }
    }
}

/// Write a value to the output sink, followed by a newline.
pub fn write_value<W: Write>(w: &mut W, value: &Value, config: &OutputConfig) -> io::Result<()> {
    match config.mode {
        OutputMode::Pretty => {
            write_pretty(w, value, 0, &config.indent)?;
            w.write_all(b"\n")
        }
        OutputMode::Compact => {
            write_compact(w, value)?;
            w.write_all(b"\n")
        }
        OutputMode::Raw => {
            write_raw(w, value)?;
            w.write_all(b"\n")
        }
    }
}

// ---------------------------------------------------------------------------
// Compact output
// ---------------------------------------------------------------------------

pub(crate) fn write_compact<W: Write>(w: &mut W, value: &Value) -> io::Result<()> {
    match value {
        Value::Null => w.write_all(b"null"),
        Value::Bool(true) => w.write_all(b"true"),
        Value::Bool(false) => w.write_all(b"false"),
        Value::Int(n) => {
            let mut buf = itoa::Buffer::new();
            w.write_all(buf.format(*n).as_bytes())
        }
        Value::Double(f, raw) => write_double(w, *f, raw.as_deref()),
        Value::String(s) => write_json_string(w, s),
        Value::Array(arr) => {
            w.write_all(b"[")?;
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    w.write_all(b",")?;
                }
                write_compact(w, v)?;
            }
            w.write_all(b"]")
        }
        Value::Object(obj) => {
            w.write_all(b"{")?;
            for (i, (k, v)) in obj.iter().enumerate() {
                if i > 0 {
                    w.write_all(b",")?;
                }
                write_json_string(w, k)?;
                w.write_all(b":")?;
                write_compact(w, v)?;
            }
            w.write_all(b"}")
        }
    }
}

// ---------------------------------------------------------------------------
// Pretty output
// ---------------------------------------------------------------------------

fn write_pretty<W: Write>(w: &mut W, value: &Value, depth: usize, indent: &str) -> io::Result<()> {
    match value {
        Value::Null => w.write_all(b"null"),
        Value::Bool(true) => w.write_all(b"true"),
        Value::Bool(false) => w.write_all(b"false"),
        Value::Int(n) => {
            let mut buf = itoa::Buffer::new();
            w.write_all(buf.format(*n).as_bytes())
        }
        Value::Double(f, raw) => write_double(w, *f, raw.as_deref()),
        Value::String(s) => write_json_string(w, s),
        Value::Array(arr) if arr.is_empty() => w.write_all(b"[]"),
        Value::Array(arr) => {
            w.write_all(b"[\n")?;
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    w.write_all(b",\n")?;
                }
                write_indent(w, depth + 1, indent)?;
                write_pretty(w, v, depth + 1, indent)?;
            }
            w.write_all(b"\n")?;
            write_indent(w, depth, indent)?;
            w.write_all(b"]")
        }
        Value::Object(obj) if obj.is_empty() => w.write_all(b"{}"),
        Value::Object(obj) => {
            w.write_all(b"{\n")?;
            for (i, (k, v)) in obj.iter().enumerate() {
                if i > 0 {
                    w.write_all(b",\n")?;
                }
                write_indent(w, depth + 1, indent)?;
                write_json_string(w, k)?;
                w.write_all(b": ")?;
                write_pretty(w, v, depth + 1, indent)?;
            }
            w.write_all(b"\n")?;
            write_indent(w, depth, indent)?;
            w.write_all(b"}")
        }
    }
}

fn write_indent<W: Write>(w: &mut W, depth: usize, indent: &str) -> io::Result<()> {
    for _ in 0..depth {
        w.write_all(indent.as_bytes())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Raw output (-r)
// ---------------------------------------------------------------------------

fn write_raw<W: Write>(w: &mut W, value: &Value) -> io::Result<()> {
    match value {
        // Raw mode: strings are output without quotes
        Value::String(s) => w.write_all(s.as_bytes()),
        // Everything else is the same as compact
        _ => write_compact(w, value),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Write a JSON-escaped string (with surrounding quotes).
fn write_json_string<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    w.write_all(b"\"")?;
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: Option<&[u8]> = match b {
            b'"' => Some(b"\\\""),
            b'\\' => Some(b"\\\\"),
            b'\n' => Some(b"\\n"),
            b'\r' => Some(b"\\r"),
            b'\t' => Some(b"\\t"),
            b'\x08' => Some(b"\\b"),
            b'\x0c' => Some(b"\\f"),
            0..=0x1f => None, // handled below
            _ => continue,
        };
        if let Some(esc) = escape {
            // Flush preceding safe bytes
            if start < i {
                w.write_all(&bytes[start..i])?;
            }
            w.write_all(esc)?;
            start = i + 1;
        } else if b <= 0x1f {
            // Control character — \u00XX
            if start < i {
                w.write_all(&bytes[start..i])?;
            }
            write!(w, "\\u{:04x}", b)?;
            start = i + 1;
        }
    }
    // Flush remaining
    if start < bytes.len() {
        w.write_all(&bytes[start..])?;
    }
    w.write_all(b"\"")
}

/// Write a double in jq-compatible format.
///
/// If `raw` is `Some`, uses the original JSON literal text (preserving
/// trailing zeros, scientific notation, etc.) to match jq 1.7+ behavior.
/// Otherwise uses ryu for computed values.
fn write_double<W: Write>(w: &mut W, f: f64, raw: Option<&str>) -> io::Result<()> {
    if f.is_nan() {
        return w.write_all(b"null");
    }
    if f.is_infinite() {
        return w.write_all(b"null");
    }
    // Use raw JSON text when available (literal preservation)
    if let Some(text) = raw {
        return w.write_all(text.as_bytes());
    }
    // If the double is an exact integer in i64 range, output as integer
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        let mut buf = itoa::Buffer::new();
        return w.write_all(buf.format(f as i64).as_bytes());
    }
    let mut buf = ryu::Buffer::new();
    let s = buf.format(f);
    w.write_all(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn compact(v: &Value) -> String {
        let config = OutputConfig {
            mode: OutputMode::Compact,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, v, &config).unwrap();
        // Trim trailing newline
        String::from_utf8(buf).unwrap().trim_end().to_string()
    }

    fn pretty(v: &Value) -> String {
        let config = OutputConfig {
            mode: OutputMode::Pretty,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, v, &config).unwrap();
        String::from_utf8(buf).unwrap().trim_end().to_string()
    }

    fn raw(v: &Value) -> String {
        let config = OutputConfig {
            mode: OutputMode::Raw,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, v, &config).unwrap();
        String::from_utf8(buf).unwrap().trim_end().to_string()
    }

    #[test]
    fn compact_null() {
        assert_eq!(compact(&Value::Null), "null");
    }

    #[test]
    fn compact_bool() {
        assert_eq!(compact(&Value::Bool(true)), "true");
        assert_eq!(compact(&Value::Bool(false)), "false");
    }

    #[test]
    fn compact_int() {
        assert_eq!(compact(&Value::Int(42)), "42");
        assert_eq!(compact(&Value::Int(-1)), "-1");
        assert_eq!(compact(&Value::Int(0)), "0");
    }

    #[test]
    fn compact_double() {
        assert_eq!(compact(&Value::Double(3.14, None)), "3.14");
        // Integer-valued doubles render without .0
        assert_eq!(compact(&Value::Double(1.0, None)), "1");
        assert_eq!(compact(&Value::Double(-0.0, None)), "0");
    }

    #[test]
    fn compact_double_raw_preserved() {
        // Raw text preserves original formatting
        assert_eq!(compact(&Value::Double(75.8, Some("75.80".into()))), "75.80");
        assert_eq!(
            compact(&Value::Double(150.0, Some("1.5e2".into()))),
            "1.5e2"
        );
    }

    #[test]
    fn compact_string() {
        assert_eq!(compact(&Value::String("hello".into())), r#""hello""#);
    }

    #[test]
    fn compact_string_escaping() {
        assert_eq!(
            compact(&Value::String("a\"b\\c\nd".into())),
            r#""a\"b\\c\nd""#
        );
    }

    #[test]
    fn compact_control_chars() {
        assert_eq!(
            compact(&Value::String("\x00\x1f".into())),
            r#""\u0000\u001f""#
        );
    }

    #[test]
    fn compact_array() {
        let v = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(compact(&v), "[1,2,3]");
    }

    #[test]
    fn compact_empty_array() {
        assert_eq!(compact(&Value::Array(Rc::new(vec![]))), "[]");
    }

    #[test]
    fn compact_object() {
        let v = Value::Object(Rc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Bool(true)),
        ]));
        assert_eq!(compact(&v), r#"{"a":1,"b":true}"#);
    }

    #[test]
    fn compact_empty_object() {
        assert_eq!(compact(&Value::Object(Rc::new(vec![]))), "{}");
    }

    #[test]
    fn pretty_object() {
        let v = Value::Object(Rc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ]));
        assert_eq!(pretty(&v), "{\n  \"a\": 1,\n  \"b\": 2\n}");
    }

    #[test]
    fn pretty_nested() {
        let v = Value::Object(Rc::new(vec![(
            "arr".into(),
            Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)])),
        )]));
        assert_eq!(pretty(&v), "{\n  \"arr\": [\n    1,\n    2\n  ]\n}");
    }

    #[test]
    fn raw_string() {
        assert_eq!(raw(&Value::String("hello world".into())), "hello world");
    }

    #[test]
    fn raw_non_string() {
        assert_eq!(raw(&Value::Int(42)), "42");
    }

    #[test]
    fn double_nan() {
        assert_eq!(compact(&Value::Double(f64::NAN, None)), "null");
    }

    #[test]
    fn double_infinity() {
        assert_eq!(compact(&Value::Double(f64::INFINITY, None)), "null");
    }

    #[test]
    fn large_int() {
        assert_eq!(
            compact(&Value::Int(9223372036854775807)),
            "9223372036854775807"
        );
    }
}
