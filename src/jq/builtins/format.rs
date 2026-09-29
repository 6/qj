//! `f_format`: `format/1`, which `@name` compiles to.
//!
//! Port of builtin.c. Owned by Track B1 (docs/JQ_PORT_PLAN.md).
//!
//! jq 1.8.1 knows `text`, `json`, `csv`, `tsv`, `html`, `uri`, `urid`, `sh`, `base64` and
//! `base64d`. There is no `@base32`/`@base32d` in 1.8.1: `jq -n '"hello" | @base32'` fails
//! with `base32 is not a valid format`, and so does the port.

use super::general::tostring;
use super::{CResult, Host};
use crate::jq::value::unicode::utf8_is_valid;
use crate::jq::value::{Error, Str, Value};

/// `format` (nargs 2): port of builtin.c `f_format`.
///
/// The format name is compared as a C string (`strcmp`), so anything after a NUL is
/// ignored: `format("json\u0000x")` is `@json`.
pub fn f_format(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let fmt = std::mem::take(&mut args[0]);
    let Value::String(fmt_str) = &fmt else {
        return Err(Error::type_error(&fmt, "is not a valid format"));
    };
    match fmt_str.as_c_str() {
        "json" => Ok(Value::from(input.to_json())),
        "text" => Ok(tostring(input)),
        "csv" => csv_tsv(
            input,
            "cannot be csv-formatted, only array",
            "\"",
            ",",
            csv_escape,
        ),
        "tsv" => csv_tsv(
            input,
            "cannot be tsv-formatted, only array",
            "",
            "\t",
            tsv_escape,
        ),
        "html" => {
            let input = tostring(input);
            Ok(Value::from(escape_string(as_text(&input), html_escape)))
        }
        "uri" => Ok(uri(as_text(&tostring(input)))),
        "urid" => urid(tostring(input)),
        "sh" => sh(input),
        "base64" => Ok(base64(as_text(&tostring(input)).as_bytes())),
        "base64d" => base64d(tostring(input)),
        _ => {
            // jv_string_concat(fmt, jv_string(" is not a valid format")): the whole
            // string, NULs included.
            let mut msg = fmt_str.clone();
            msg.push_str(" is not a valid format");
            Err(Error::new(Value::String(msg)))
        }
    }
}

/// The string of a value that `tostring` made a string.
fn as_text(v: &Value) -> &Str {
    match v {
        Value::String(s) => s,
        _ => unreachable!("tostring returns a string"),
    }
}

/// `@csv` escapings (`"\"\"\"\0"`): `"` doubles.
fn csv_escape(b: u8) -> Option<&'static str> {
    match b {
        b'"' => Some("\"\""),
        _ => None,
    }
}

/// `@tsv` escapings (`"\t\\t\0\r\\r\0\n\\n\0\\\\\\\0"`).
fn tsv_escape(b: u8) -> Option<&'static str> {
    match b {
        b'\t' => Some("\\t"),
        b'\r' => Some("\\r"),
        b'\n' => Some("\\n"),
        b'\\' => Some("\\\\"),
        _ => None,
    }
}

/// `@html` escapings (`"&&amp;\0<&lt;\0>&gt;\0'&apos;\0\"&quot;\0"`).
fn html_escape(b: u8) -> Option<&'static str> {
    match b {
        b'&' => Some("&amp;"),
        b'<' => Some("&lt;"),
        b'>' => Some("&gt;"),
        b'\'' => Some("&apos;"),
        b'"' => Some("&quot;"),
        _ => None,
    }
}

/// `@sh` escapings (`"''\\''\0"`): `'` becomes `'\''`.
fn sh_escape(b: u8) -> Option<&'static str> {
    match b {
        b'\'' => Some("'\\''"),
        _ => None,
    }
}

/// Port of builtin.c `escape_string`: replaces the ASCII characters `escapings` maps,
/// and NUL, which every format turns into the two characters `\0` (`lookup[0]`).
/// Escaped characters are ASCII, so scanning bytes finds exactly the codepoints jq's
/// `jvp_utf8_next` loop finds.
fn escape_string(input: &str, escapings: fn(u8) -> Option<&'static str>) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len() + 2);
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b >= 128 {
            continue;
        }
        let rep = if b == 0 { Some("\\0") } else { escapings(b) };
        if let Some(rep) = rep {
            out.push_str(&input[start..i]);
            out.push_str(rep);
            start = i + 1;
        }
    }
    out.push_str(&input[start..]);
    out
}

/// The `csv`/`tsv` branch of `f_format`. `null` and NaN are empty fields, booleans and
/// numbers are dumped (literals keep their text), strings are escaped (and quoted for
/// CSV); arrays and objects are errors, reported as "csv" rows for TSV too.
fn csv_tsv(
    input: Value,
    msg: &str,
    quotes: &str,
    sep: &str,
    escapings: fn(u8) -> Option<&'static str>,
) -> CResult {
    let Value::Array(a) = &input else {
        return Err(Error::type_error(&input, msg));
    };
    let mut line = String::new();
    for (i, x) in a.iter().enumerate() {
        if i != 0 {
            line.push_str(sep);
        }
        match x {
            // null rendered as empty string
            Value::Null => {}
            Value::Bool(_) => line.push_str(&x.to_json()),
            // NaN, render as empty string
            Value::Number(n) if n.value().is_nan() => {}
            Value::Number(_) => line.push_str(&x.to_json()),
            Value::String(s) => {
                line.push_str(quotes);
                line.push_str(&escape_string(s.as_str(), escapings));
                line.push_str(quotes);
            }
            Value::Array(_) | Value::Object(_) => {
                return Err(Error::type_error(x, "is not valid in a csv row"));
            }
        }
    }
    Ok(Value::from(line))
}

/// The `sh` branch of `f_format`: a non-array input is treated as a one-element array;
/// strings are single-quoted, scalars dumped, containers are errors.
fn sh(input: Value) -> CResult {
    let items: Vec<Value> = match input {
        Value::Array(a) => a.into_vec(),
        other => vec![other],
    };
    let mut line = String::new();
    for (i, x) in items.iter().enumerate() {
        if i != 0 {
            line.push(' ');
        }
        match x {
            Value::Null | Value::Bool(_) | Value::Number(_) => line.push_str(&x.to_json()),
            Value::String(s) => {
                line.push('\'');
                line.push_str(&escape_string(s.as_str(), sh_escape));
                line.push('\'');
            }
            Value::Array(_) | Value::Object(_) => {
                return Err(Error::type_error(x, "can not be escaped for shell"));
            }
        }
    }
    Ok(Value::from(line))
}

/// `CHARS_ALPHANUM "-_.~"`: the bytes `@uri` leaves alone.
fn uri_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

/// The `uri` branch of `f_format`: every byte outside the unreserved set becomes `%XX`.
fn uri(s: &str) -> Value {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut line = String::with_capacity(s.len());
    for &ch in s.as_bytes() {
        if uri_unreserved(ch) {
            line.push(ch as char);
        } else {
            line.push('%');
            line.push(HEX[(ch >> 4) as usize] as char);
            line.push(HEX[(ch & 0xF) as usize] as char);
        }
    }
    Value::from(line)
}

/// The `urid` branch of `f_format`, reproducing jq's byte-level loop:
/// * decoding stops at the first NUL (`while (*s)`);
/// * a `%XX` run is read as one UTF-8 sequence, its length taken from the lead byte,
///   and must be valid UTF-8 (`%C3%A9` is `é`; `%80`, `%ED%A0%80`, `%C3A9` are errors);
/// * any other byte is appended on its own (`jv_string_append_buf(line, s++, 1)`), so a
///   literal non-ASCII character becomes one U+FFFD per byte: `"é" | @urid` is `"��"`.
fn urid(input: Value) -> CResult {
    const ERRMSG: &str = "is not a valid uri encoding";
    let s = as_text(&input).as_c_str().as_bytes();
    // Reads past the end see C's terminating NUL.
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let hex = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut line = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'%' {
            if s[i] < 0x80 {
                line.push(s[i] as char);
            } else {
                line.push('\u{FFFD}');
            }
            i += 1;
            continue;
        }
        let mut unicode = [0u8; 4];
        let mut b = 0;
        // check leading bits of first octet to determine length of unicode character
        // (https://datatracker.ietf.org/doc/html/rfc3629#section-3)
        while b == 0 || (b < 4 && unicode[0] >> 7 & 1 != 0 && unicode[0] >> (7 - b) & 1 != 0) {
            if at(i) != b'%' {
                return Err(Error::type_error(&input, ERRMSG));
            }
            i += 1;
            for _ in 0..2 {
                let Some(d) = hex(at(i)) else {
                    return Err(Error::type_error(&input, ERRMSG));
                };
                i += 1;
                unicode[b] = (unicode[b] << 4) | d;
            }
            b += 1;
        }
        if !utf8_is_valid(&unicode[..b]) {
            return Err(Error::type_error(&input, ERRMSG));
        }
        line.push_str(std::str::from_utf8(&unicode[..b]).expect("validated"));
    }
    Ok(Value::from(line))
}

/// `BASE64_ENCODE_TABLE`.
const BASE64_ENCODE_TABLE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `BASE64_INVALID_ENTRY`.
const BASE64_INVALID_ENTRY: u8 = 0xFF;

/// `BASE64_DECODE_TABLE`: the 6-bit value of each base64 byte, 99 for `=` (which the
/// decoder never looks up, as it stops at the first `=`), 0xFF for everything else.
/// jq's table has 255 entries; byte 0xFF never occurs in a (valid UTF-8) jq string.
static BASE64_DECODE_TABLE: [u8; 256] = {
    let mut t = [BASE64_INVALID_ENTRY; 256];
    let mut i = 0;
    while i < 64 {
        t[BASE64_ENCODE_TABLE[i] as usize] = i as u8;
        i += 1;
    }
    t[b'=' as usize] = 99;
    t
};

/// The `base64` branch of `f_format` (padded, standard alphabet).
fn base64(data: &[u8]) -> Value {
    let mut line = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let mut code: u32 = 0;
        for j in 0..3 {
            code <<= 8;
            code |= chunk.get(j).copied().unwrap_or(0) as u32;
        }
        let mut buf = [0u8; 4];
        for (j, c) in buf.iter_mut().enumerate() {
            *c = BASE64_ENCODE_TABLE[((code >> (18 - j * 6)) & 0x3f) as usize];
        }
        if n < 3 {
            buf[3] = b'=';
        }
        if n < 2 {
            buf[2] = b'=';
        }
        line.push_str(std::str::from_utf8(&buf).expect("ASCII"));
    }
    Value::from(line)
}

/// The `base64d` branch of `f_format`. Decoding stops at the first `=`; a leftover
/// group of 2 or 3 symbols yields 1 or 2 bytes, a single leftover symbol is an error.
/// The bytes become a string with jq's U+FFFD replacement (`jv_string_sized`).
fn base64d(input: Value) -> CResult {
    let data = as_text(&input).as_bytes();
    let mut result: Vec<u8> = Vec::with_capacity((3 * data.len()) / 4 + 1);
    let mut input_bytes_read = 0;
    let mut code: u32 = 0;
    for &c in data.iter().take_while(|&&c| c != b'=') {
        let d = BASE64_DECODE_TABLE[c as usize];
        if d == BASE64_INVALID_ENTRY {
            return Err(Error::type_error(&input, "is not valid base64 data"));
        }
        code <<= 6;
        code |= d as u32;
        input_bytes_read += 1;
        if input_bytes_read == 4 {
            result.push((code >> 16) as u8);
            result.push((code >> 8) as u8);
            result.push(code as u8);
            input_bytes_read = 0;
            code = 0;
        }
    }
    match input_bytes_read {
        3 => {
            result.push((code >> 10) as u8);
            result.push((code >> 2) as u8);
        }
        2 => result.push((code >> 4) as u8),
        1 => return Err(Error::type_error(&input, "trailing base64 byte found")),
        _ => {}
    }
    Ok(Value::String(Str::from_bytes(&result)))
}
