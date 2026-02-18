/// JSON output formatting.
///
/// Writes `Value` directly to a `Write` sink — no intermediate `String`
/// allocation. Uses `itoa` for integers and `ryu` for floats.
use std::io::{self, Write};

use crate::value::Value;

/// Output formatting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Pretty-printed with indentation (default for TTY).
    Pretty,
    /// Compact single-line output (`-c`).
    Compact,
    /// Raw string output (`-r`) — strings without quotes.
    Raw,
}

/// ANSI color scheme for JSON output (matches jq's defaults).
#[derive(Debug, Clone)]
pub struct ColorScheme {
    pub null: &'static str,
    pub bool_val: &'static str,
    pub number: &'static str,
    pub string: &'static str,
    pub array_bracket: &'static str,
    pub object_brace: &'static str,
    pub object_key: &'static str,
    pub reset: &'static str,
}

impl ColorScheme {
    /// jq's default color scheme (matches jq 1.7+ output).
    pub fn jq_default() -> Self {
        Self {
            null: "\x1b[0;90m",
            bool_val: "\x1b[0;39m",
            number: "\x1b[0;39m",
            string: "\x1b[0;32m",
            array_bracket: "\x1b[1;39m",
            object_brace: "\x1b[1;39m",
            object_key: "\x1b[1;34m",
            reset: "\x1b[0m",
        }
    }

    /// No-color scheme (all empty strings).
    pub fn none() -> Self {
        Self {
            null: "",
            bool_val: "",
            number: "",
            string: "",
            array_bracket: "",
            object_brace: "",
            object_key: "",
            reset: "",
        }
    }

    fn is_enabled(&self) -> bool {
        !self.reset.is_empty()
    }
}

/// Configuration for output formatting.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub mode: OutputMode,
    /// Indentation string (default "  ", or "\t" with --tab).
    pub indent: String,
    /// Sort object keys alphabetically (`-S`).
    pub sort_keys: bool,
    /// Suppress trailing newline after each value (`-j`).
    pub join_output: bool,
    /// Color scheme for output.
    pub color: ColorScheme,
    /// Use NUL (`\0`) instead of newline as separator for string values (`--raw-output0`).
    pub null_separator: bool,
    /// Escape non-ASCII characters to `\uXXXX` sequences (`--ascii-output`).
    pub ascii_output: bool,
    /// Flush stdout after each output value (`--unbuffered`).
    pub unbuffered: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Pretty,
            indent: "  ".to_string(),
            sort_keys: false,
            join_output: false,
            color: ColorScheme::none(),
            null_separator: false,
            ascii_output: false,
            unbuffered: false,
        }
    }
}

/// Format a value as compact JSON (for error messages, etc).
pub fn format_compact(value: &Value) -> String {
    let config = OutputConfig {
        mode: OutputMode::Compact,
        ..Default::default()
    };
    let mut buf = Vec::new();
    write_value(&mut buf, value, &config).unwrap();
    // Trim trailing newline
    String::from_utf8(buf).unwrap().trim_end().to_string()
}

/// Write a value to the output sink, followed by a newline (unless join_output).
pub fn write_value<W: Write>(w: &mut W, value: &Value, config: &OutputConfig) -> io::Result<()> {
    match config.mode {
        OutputMode::Pretty => {
            let fmt = PrettyFmt {
                indent: &config.indent,
            };
            write_value_inner(
                w,
                value,
                &fmt,
                0,
                config.sort_keys,
                &config.color,
                config.ascii_output,
            )?;
        }
        OutputMode::Compact => {
            write_value_inner(
                w,
                value,
                &CompactFmt,
                0,
                config.sort_keys,
                &config.color,
                config.ascii_output,
            )?;
        }
        OutputMode::Raw => write_raw(
            w,
            value,
            config.sort_keys,
            &config.color,
            config.ascii_output,
        )?,
    }
    if !config.join_output {
        if config.null_separator {
            w.write_all(b"\0")?;
        } else {
            w.write_all(b"\n")?;
        }
    }
    if config.unbuffered {
        w.flush()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Generic formatter infrastructure
// ---------------------------------------------------------------------------

/// Trait abstracting the whitespace/indentation differences between compact and
/// pretty-printed JSON output.  Methods handle **only** whitespace (newlines,
/// indentation, space after colon). Structural characters (`{`, `}`, `[`, `]`,
/// `,`, `:`) are written by `write_value_inner` with color wrapping.
///
/// Using a generic type parameter (not `dyn`) ensures monomorphization for
/// zero runtime cost.
trait JsonFormatter {
    /// Whitespace after `[`. Pretty: `\n`, Compact: nothing.
    fn after_open<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()>;
    /// Whitespace + indent before the first element. Pretty: indent, Compact: nothing.
    fn before_first<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()>;
    /// Whitespace after `,` between elements. Pretty: `\n` + indent, Compact: nothing.
    fn after_sep<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()>;
    /// Whitespace before `]`/`}`. Pretty: `\n` + indent, Compact: nothing.
    fn before_close<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()>;
    /// Whitespace after `:`. Pretty: ` `, Compact: nothing.
    fn after_colon<W: Write>(&self, w: &mut W) -> io::Result<()>;
}

struct CompactFmt;

impl JsonFormatter for CompactFmt {
    #[inline]
    fn after_open<W: Write>(&self, _w: &mut W, _depth: usize) -> io::Result<()> {
        Ok(())
    }
    #[inline]
    fn before_first<W: Write>(&self, _w: &mut W, _depth: usize) -> io::Result<()> {
        Ok(())
    }
    #[inline]
    fn after_sep<W: Write>(&self, _w: &mut W, _depth: usize) -> io::Result<()> {
        Ok(())
    }
    #[inline]
    fn before_close<W: Write>(&self, _w: &mut W, _depth: usize) -> io::Result<()> {
        Ok(())
    }
    #[inline]
    fn after_colon<W: Write>(&self, _w: &mut W) -> io::Result<()> {
        Ok(())
    }
}

struct PrettyFmt<'a> {
    indent: &'a str,
}

impl<'a> JsonFormatter for PrettyFmt<'a> {
    fn after_open<W: Write>(&self, w: &mut W, _depth: usize) -> io::Result<()> {
        w.write_all(b"\n")
    }
    fn before_first<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()> {
        write_indent(w, depth + 1, self.indent)
    }
    fn after_sep<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()> {
        w.write_all(b"\n")?;
        write_indent(w, depth + 1, self.indent)
    }
    fn before_close<W: Write>(&self, w: &mut W, depth: usize) -> io::Result<()> {
        w.write_all(b"\n")?;
        write_indent(w, depth, self.indent)
    }
    fn after_colon<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(b" ")
    }
}

/// Write a colored structural character (brace, bracket, comma, colon).
/// When color is disabled, writes just the character with zero overhead.
#[inline]
fn write_colored<W: Write>(w: &mut W, ch: &[u8], color_code: &str, reset: &str) -> io::Result<()> {
    if !color_code.is_empty() {
        w.write_all(color_code.as_bytes())?;
        w.write_all(ch)?;
        w.write_all(reset.as_bytes())
    } else {
        w.write_all(ch)
    }
}

/// Unified recursive value writer, parameterized by formatter.
/// Structural characters are written with color wrapping; the formatter
/// handles only whitespace (newlines, indentation).
fn write_value_inner<W: Write, F: JsonFormatter>(
    w: &mut W,
    value: &Value,
    fmt: &F,
    depth: usize,
    sort_keys: bool,
    color: &ColorScheme,
    ascii_output: bool,
) -> io::Result<()> {
    let c = color.is_enabled();
    match value {
        Value::Null => {
            if c {
                w.write_all(color.null.as_bytes())?;
            }
            w.write_all(b"null")?;
            if c {
                w.write_all(color.reset.as_bytes())?;
            }
            Ok(())
        }
        Value::Bool(b) => {
            if c {
                w.write_all(color.bool_val.as_bytes())?;
            }
            w.write_all(if *b { b"true" } else { b"false" })?;
            if c {
                w.write_all(color.reset.as_bytes())?;
            }
            Ok(())
        }
        Value::Int(n) => {
            if c {
                w.write_all(color.number.as_bytes())?;
            }
            let mut buf = itoa::Buffer::new();
            w.write_all(buf.format(*n).as_bytes())?;
            if c {
                w.write_all(color.reset.as_bytes())?;
            }
            Ok(())
        }
        Value::Double(f, raw) => {
            if c {
                w.write_all(color.number.as_bytes())?;
            }
            write_double(w, *f, raw.as_deref())?;
            if c {
                w.write_all(color.reset.as_bytes())?;
            }
            Ok(())
        }
        Value::String(s) => {
            if c {
                w.write_all(color.string.as_bytes())?;
            }
            if ascii_output {
                write_json_string_ascii(w, s)?;
            } else {
                write_json_string(w, s)?;
            }
            if c {
                w.write_all(color.reset.as_bytes())?;
            }
            Ok(())
        }
        Value::Array(arr) if arr.is_empty() => {
            write_colored(w, b"[]", color.array_bracket, color.reset)
        }
        Value::Array(arr) => {
            write_colored(w, b"[", color.array_bracket, color.reset)?;
            fmt.after_open(w, depth)?;
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    write_colored(w, b",", color.array_bracket, color.reset)?;
                    fmt.after_sep(w, depth)?;
                } else {
                    fmt.before_first(w, depth)?;
                }
                write_value_inner(w, v, fmt, depth + 1, sort_keys, color, ascii_output)?;
            }
            fmt.before_close(w, depth)?;
            write_colored(w, b"]", color.array_bracket, color.reset)
        }
        Value::Object(obj) if obj.is_empty() => {
            write_colored(w, b"{}", color.object_brace, color.reset)
        }
        Value::Object(obj) => {
            write_colored(w, b"{", color.object_brace, color.reset)?;
            fmt.after_open(w, depth)?;
            let sorted;
            let pairs: &[(String, Value)] = if sort_keys {
                sorted = {
                    let mut v = obj.as_ref().clone();
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                    v
                };
                &sorted
            } else {
                obj.as_ref()
            };
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    write_colored(w, b",", color.object_brace, color.reset)?;
                    fmt.after_sep(w, depth)?;
                } else {
                    fmt.before_first(w, depth)?;
                }
                if c {
                    w.write_all(color.object_key.as_bytes())?;
                }
                if ascii_output {
                    write_json_string_ascii(w, k)?;
                } else {
                    write_json_string(w, k)?;
                }
                if c {
                    w.write_all(color.reset.as_bytes())?;
                }
                write_colored(w, b":", color.object_brace, color.reset)?;
                fmt.after_colon(w)?;
                write_value_inner(w, v, fmt, depth + 1, sort_keys, color, ascii_output)?;
            }
            fmt.before_close(w, depth)?;
            write_colored(w, b"}", color.object_brace, color.reset)
        }
    }
}

// ---------------------------------------------------------------------------
// Public thin wrappers (preserve existing API)
// ---------------------------------------------------------------------------

pub(crate) fn write_compact<W: Write>(w: &mut W, value: &Value, sort_keys: bool) -> io::Result<()> {
    write_value_inner(
        w,
        value,
        &CompactFmt,
        0,
        sort_keys,
        &ColorScheme::none(),
        false,
    )
}

// ---------------------------------------------------------------------------
// Raw output (-r)
// ---------------------------------------------------------------------------

fn write_raw<W: Write>(
    w: &mut W,
    value: &Value,
    sort_keys: bool,
    color: &ColorScheme,
    ascii_output: bool,
) -> io::Result<()> {
    match value {
        // Raw mode: strings are output without quotes.
        // With --ascii-output, jq outputs the full JSON-encoded string (with quotes),
        // so we fall through to the compact path which handles ascii escaping.
        Value::String(s) if !ascii_output => w.write_all(s.as_bytes()),
        // Everything else is the same as compact (with color)
        _ => write_value_inner(w, value, &CompactFmt, 0, sort_keys, color, ascii_output),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn write_indent<W: Write>(w: &mut W, depth: usize, indent: &str) -> io::Result<()> {
    for _ in 0..depth {
        w.write_all(indent.as_bytes())?;
    }
    Ok(())
}

/// Write a JSON-escaped string (with surrounding quotes).
pub fn write_json_string<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
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

/// Write a JSON-escaped string with non-ASCII characters escaped to `\uXXXX` sequences.
/// Supplementary plane characters (U+10000+) are encoded as surrogate pairs.
fn write_json_string_ascii<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    w.write_all(b"\"")?;
    for ch in s.chars() {
        match ch {
            '"' => w.write_all(b"\\\"")?,
            '\\' => w.write_all(b"\\\\")?,
            '\n' => w.write_all(b"\\n")?,
            '\r' => w.write_all(b"\\r")?,
            '\t' => w.write_all(b"\\t")?,
            '\x08' => w.write_all(b"\\b")?,
            '\x0c' => w.write_all(b"\\f")?,
            c if (c as u32) < 0x20 => write!(w, "\\u{:04x}", c as u32)?,
            c if c.is_ascii() => w.write_all(&[c as u8])?,
            c if (c as u32) <= 0xFFFF => write!(w, "\\u{:04x}", c as u32)?,
            c => {
                // Surrogate pair for supplementary plane
                let n = c as u32 - 0x10000;
                let hi = 0xD800 + (n >> 10);
                let lo = 0xDC00 + (n & 0x3FF);
                write!(w, "\\u{:04x}\\u{:04x}", hi, lo)?;
            }
        }
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
        // jq represents +/-Infinity as +/-DBL_MAX (1.7976931348623157e+308)
        if f.is_sign_positive() {
            return w.write_all(b"1.7976931348623157e+308");
        } else {
            return w.write_all(b"-1.7976931348623157e+308");
        }
    }
    // Use raw JSON text when available (literal preservation)
    if let Some(text) = raw {
        return w.write_all(text.as_bytes());
    }
    // For computed doubles (no raw text), use ryu for the shortest
    // representation, then adjust formatting for integer-valued results
    // to match jq behavior (no ".0" suffix, plain integers when possible).
    let mut buf = ryu::Buffer::new();
    let s = buf.format(f);
    if f.fract() == 0.0 {
        if let Some(e_pos) = s.find('e') {
            let exp: i32 = s[e_pos + 1..].parse().unwrap_or(0);
            if exp > 0 {
                let mantissa = &s[..e_pos];
                let frac_len = mantissa.find('.').map_or(0, |d| mantissa.len() - d - 1);
                let zeros_needed = exp as usize - frac_len;
                // jq expands to plain integer when trailing zeros <= 15.
                // Beyond that, it uses scientific notation (e.g., "1e+20").
                if zeros_needed <= 15 {
                    let (int_part, frac_part) = match mantissa.find('.') {
                        Some(d) => (&mantissa[..d], &mantissa[d + 1..]),
                        None => (mantissa, ""),
                    };
                    w.write_all(int_part.as_bytes())?;
                    w.write_all(frac_part.as_bytes())?;
                    for _ in 0..zeros_needed {
                        w.write_all(b"0")?;
                    }
                    return Ok(());
                }
            }
        }
        // ryu output like "5.0" or "-100.0" — strip the ".0" suffix
        if let Some(stripped) = s.strip_suffix(".0") {
            return w.write_all(stripped.as_bytes());
        }
    }
    // For scientific notation, add "+" to positive exponents to match jq
    // (ryu: "1.5e10" → jq: "1.5e+10")
    if let Some(e_pos) = s.find('e') {
        w.write_all(&s.as_bytes()[..e_pos])?;
        w.write_all(b"e")?;
        let exp_str = &s[e_pos + 1..];
        if !exp_str.starts_with('-') {
            w.write_all(b"+")?;
        }
        return w.write_all(exp_str.as_bytes());
    }
    w.write_all(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        assert_eq!(compact(&Value::Double(-0.0, None)), "-0");
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
        let v = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(compact(&v), "[1,2,3]");
    }

    #[test]
    fn compact_empty_array() {
        assert_eq!(compact(&Value::Array(Arc::new(vec![]))), "[]");
    }

    #[test]
    fn compact_object() {
        let v = Value::Object(Arc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Bool(true)),
        ]));
        assert_eq!(compact(&v), r#"{"a":1,"b":true}"#);
    }

    #[test]
    fn compact_empty_object() {
        assert_eq!(compact(&Value::Object(Arc::new(vec![]))), "{}");
    }

    #[test]
    fn pretty_object() {
        let v = Value::Object(Arc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ]));
        assert_eq!(pretty(&v), "{\n  \"a\": 1,\n  \"b\": 2\n}");
    }

    #[test]
    fn pretty_nested() {
        let v = Value::Object(Arc::new(vec![(
            "arr".into(),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])),
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
        // jq represents infinity as +/- DBL_MAX
        assert_eq!(
            compact(&Value::Double(f64::INFINITY, None)),
            "1.7976931348623157e+308"
        );
        assert_eq!(
            compact(&Value::Double(f64::NEG_INFINITY, None)),
            "-1.7976931348623157e+308"
        );
    }

    #[test]
    fn large_int() {
        assert_eq!(
            compact(&Value::Int(9223372036854775807)),
            "9223372036854775807"
        );
    }

    #[test]
    fn double_at_i64_max_boundary() {
        // 2^63 = 9223372036854775808.0 is one above i64::MAX — must NOT format as i64
        let val = Value::Double(9223372036854775808.0, None);
        let s = compact(&val);
        // Must not truncate to i64::MAX
        assert_ne!(s, "9223372036854775807");
        // Should format as plain integer (no scientific notation), matching jq
        assert_eq!(s, "9223372036854776000");
    }

    #[test]
    fn double_large_integer_plain_format() {
        // Integer-valued doubles: expand to plain when trailing zeros <= 15,
        // otherwise scientific notation with e+ (matching jq behavior).
        assert_eq!(
            compact(&Value::Double(5.564623688220226e21, None)),
            "5564623688220226000000" // zeros_needed=6, expanded
        );
        assert_eq!(compact(&Value::Double(1e20, None)), "1e+20"); // zeros_needed=20, scientific
        assert_eq!(compact(&Value::Double(-1e19, None)), "-1e+19"); // zeros_needed=19, scientific
        // At the threshold boundary: 1e15 has exactly 15 trailing zeros → expanded
        assert_eq!(compact(&Value::Double(1e15, None)), "1000000000000000");
        // 1e16 has 16 trailing zeros → scientific
        assert_eq!(compact(&Value::Double(1e16, None)), "1e+16");
    }

    #[test]
    fn double_at_i64_min_formats_via_ryu() {
        // i64::MIN = -2^63 as a Double (not Int) formats via ryu expansion,
        // matching jq's output for computed doubles near the i64 boundary.
        let val = Value::Double(i64::MIN as f64, None);
        assert_eq!(compact(&val), "-9223372036854776000");
    }

    #[test]
    fn sort_keys_object() {
        let v = Value::Object(Arc::new(vec![
            ("b".into(), Value::Int(2)),
            ("a".into(), Value::Int(1)),
        ]));
        let config = OutputConfig {
            mode: OutputMode::Compact,
            sort_keys: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap().trim(), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn sort_keys_nested() {
        let v = Value::Object(Arc::new(vec![
            (
                "z".into(),
                Value::Object(Arc::new(vec![
                    ("b".into(), Value::Int(2)),
                    ("a".into(), Value::Int(1)),
                ])),
            ),
            ("a".into(), Value::Int(0)),
        ]));
        let config = OutputConfig {
            mode: OutputMode::Compact,
            sort_keys: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap().trim(),
            r#"{"a":0,"z":{"a":1,"b":2}}"#
        );
    }

    #[test]
    fn join_output_no_newline() {
        let v = Value::String("hello".into());
        let config = OutputConfig {
            mode: OutputMode::Raw,
            join_output: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        assert_eq!(buf, b"hello"); // no trailing newline
    }

    // --- Output formatting edge cases (TEST_TODOS_3 #10) ---

    #[test]
    fn raw_output_control_characters() {
        // Raw mode outputs string bytes directly, including control chars
        let config = OutputConfig {
            mode: OutputMode::Raw,
            ..Default::default()
        };
        // String with tab, newline, carriage return
        let v = Value::String("a\tb\nc\rd".into());
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        assert_eq!(buf, b"a\tb\nc\rd\n"); // raw bytes + trailing newline

        // String with NUL byte
        let v = Value::String("before\0after".into());
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        assert_eq!(buf, b"before\0after\n");
    }

    #[test]
    fn compact_control_characters_escaped() {
        // Compact mode should JSON-escape control characters
        let v = Value::String("a\tb\nc".into());
        let s = compact(&v);
        assert_eq!(s, r#""a\tb\nc""#);

        // NUL byte
        let v = Value::String("x\0y".into());
        let s = compact(&v);
        assert_eq!(s, r#""x\u0000y""#);

        // BEL (0x07)
        let v = Value::String("x\x07y".into());
        let s = compact(&v);
        assert_eq!(s, r#""x\u0007y""#);
    }

    #[test]
    fn pretty_deeply_nested_array() {
        // Build a 50-level deep nested array: [[[...[42]...]]]
        let mut v = Value::Int(42);
        for _ in 0..50 {
            v = Value::Array(Arc::new(vec![v]));
        }
        let config = OutputConfig {
            mode: OutputMode::Pretty,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // Should have 50 opening brackets and 50 closing brackets
        assert_eq!(s.matches('[').count(), 50);
        assert_eq!(s.matches(']').count(), 50);
        assert!(s.contains("42"));
        // Deepest indent should be 50 * 2 = 100 spaces
        let deepest_line = s.lines().find(|l| l.contains("42")).unwrap();
        let indent = deepest_line.len() - deepest_line.trim_start().len();
        assert_eq!(indent, 100); // 50 levels * 2 spaces
    }

    #[test]
    fn pretty_deeply_nested_object() {
        // Build a 30-level deep nested object: {"a":{"a":...{"a":1}...}}
        let mut v = Value::Int(1);
        for _ in 0..30 {
            v = Value::Object(Arc::new(vec![("a".into(), v)]));
        }
        let config = OutputConfig {
            mode: OutputMode::Pretty,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s.matches('{').count(), 30);
        assert_eq!(s.matches('}').count(), 30);
        assert!(s.contains(": 1"));
    }

    #[test]
    fn ascii_output_surrogate_pairs() {
        // Supplementary plane character (emoji) should be encoded as surrogate pair
        let v = Value::String("🎉".into()); // U+1F389
        let config = OutputConfig {
            mode: OutputMode::Compact,
            ascii_output: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        let s = String::from_utf8(buf).unwrap().trim_end().to_string();
        assert_eq!(s, r#""\ud83c\udf89""#);
    }

    #[test]
    fn ascii_output_bmp_characters() {
        // BMP character should use single \uXXXX escape
        let v = Value::String("café".into());
        let config = OutputConfig {
            mode: OutputMode::Compact,
            ascii_output: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        let s = String::from_utf8(buf).unwrap().trim_end().to_string();
        assert_eq!(s, r#""caf\u00e9""#);
    }

    #[test]
    fn ascii_output_control_chars() {
        // Control characters in ASCII mode should be \uXXXX
        let v = Value::String("\x01\x02\x1f".into());
        let config = OutputConfig {
            mode: OutputMode::Compact,
            ascii_output: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        let s = String::from_utf8(buf).unwrap().trim_end().to_string();
        assert_eq!(s, r#""\u0001\u0002\u001f""#);
    }

    #[test]
    fn compact_large_object() {
        // Object with many keys — verify correct formatting
        let pairs: Vec<(String, Value)> =
            (0..100).map(|i| (format!("k{i}"), Value::Int(i))).collect();
        let v = Value::Object(Arc::new(pairs));
        let s = compact(&v);
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
        // Verify all 100 keys present
        for i in 0..100 {
            assert!(s.contains(&format!(r#""k{i}""#)));
        }
        // No newlines in compact mode
        assert!(!s.contains('\n'));
    }

    #[test]
    fn sort_keys_deeply_nested() {
        // Verify sort_keys works recursively through nested structures
        let inner = Value::Object(Arc::new(vec![
            ("z".into(), Value::Int(1)),
            ("a".into(), Value::Int(2)),
        ]));
        let arr = Value::Array(Arc::new(vec![
            Value::Object(Arc::new(vec![
                ("m".into(), Value::Int(3)),
                ("b".into(), Value::Int(4)),
            ])),
            inner,
        ]));
        let outer = Value::Object(Arc::new(vec![
            ("y".into(), arr),
            ("x".into(), Value::Int(0)),
        ]));
        let config = OutputConfig {
            mode: OutputMode::Compact,
            sort_keys: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &outer, &config).unwrap();
        let s = String::from_utf8(buf).unwrap().trim_end().to_string();
        assert_eq!(s, r#"{"x":0,"y":[{"b":4,"m":3},{"a":2,"z":1}]}"#);
    }

    #[test]
    fn double_neg_infinity() {
        assert_eq!(
            compact(&Value::Double(f64::NEG_INFINITY, None)),
            "-1.7976931348623157e+308"
        );
    }

    #[test]
    fn double_negative_zero() {
        // -0.0 should output as -0 (matching jq)
        let s = compact(&Value::Double(-0.0, None));
        assert_eq!(s, "-0");
    }

    #[test]
    fn raw_output0_nul_separator() {
        let v = Value::String("hello".into());
        let config = OutputConfig {
            mode: OutputMode::Raw,
            null_separator: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        assert_eq!(buf, b"hello\0");
    }

    #[test]
    fn pretty_tab_indent() {
        let v = Value::Object(Arc::new(vec![("a".into(), Value::Int(1))]));
        let config = OutputConfig {
            mode: OutputMode::Pretty,
            indent: "\t".into(),
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_value(&mut buf, &v, &config).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\t\"a\""));
    }
}
