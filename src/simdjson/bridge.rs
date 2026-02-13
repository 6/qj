//! Safe Rust wrapper over the simdjson C-linkage FFI bridge.
//!
//! `Parser` is `Send` but not `Sync` — simdjson parsers are reusable but not
//! thread-safe. Each thread in the parallel NDJSON pipeline gets its own parser.

use anyhow::{Result, bail};
use std::ffi::c_char;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use crate::value::Value;

// ---------------------------------------------------------------------------
// FFI declarations (must match bridge.cpp exactly)
// ---------------------------------------------------------------------------

#[repr(C)]
struct JxParser {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn jx_parser_new() -> *mut JxParser;
    fn jx_parser_free(p: *mut JxParser);
    fn jx_simdjson_padding() -> usize;

    fn jx_parse_ondemand(p: *mut JxParser, buf: *const c_char, len: usize) -> i32;

    fn jx_doc_find_field_str(
        p: *mut JxParser,
        key: *const c_char,
        key_len: usize,
        out: *mut *const c_char,
        out_len: *mut usize,
    ) -> i32;
    fn jx_doc_find_field_int64(
        p: *mut JxParser,
        key: *const c_char,
        key_len: usize,
        out: *mut i64,
    ) -> i32;
    fn jx_doc_find_field_double(
        p: *mut JxParser,
        key: *const c_char,
        key_len: usize,
        out: *mut f64,
    ) -> i32;
    fn jx_doc_type(p: *mut JxParser, out_type: *mut i32) -> i32;

    fn jx_iterate_many_count(
        buf: *const c_char,
        len: usize,
        batch_size: usize,
        out_count: *mut u64,
    ) -> i32;
    fn jx_iterate_many_extract_field(
        buf: *const c_char,
        len: usize,
        batch_size: usize,
        field_name: *const c_char,
        field_name_len: usize,
        out_total_bytes: *mut u64,
    ) -> i32;

    fn jx_dom_to_flat(
        buf: *const c_char,
        len: usize,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> i32;
    fn jx_flat_buffer_free(ptr: *mut u8);

    fn jx_minify(
        buf: *const c_char,
        len: usize,
        out_ptr: *mut *mut c_char,
        out_len: *mut usize,
    ) -> i32;
    fn jx_minify_free(ptr: *mut c_char);
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the number of padding bytes simdjson requires after the input buffer.
pub fn padding() -> usize {
    unsafe { jx_simdjson_padding() }
}

/// Read a file into a Vec with SIMDJSON_PADDING extra zeroed bytes at the end.
pub fn read_padded(path: &Path) -> Result<Vec<u8>> {
    let data = fs::read(path)?;
    let pad = padding();
    let mut buf = Vec::with_capacity(data.len() + pad);
    buf.extend_from_slice(&data);
    buf.resize(data.len() + pad, 0);
    Ok(buf)
}

/// Read a file directly into a padded buffer — single allocation, no copy.
///
/// Returns `(buffer, json_len)` where `buffer` has `json_len` bytes of JSON
/// followed by SIMDJSON_PADDING zeroed bytes.
pub fn read_padded_file(path: &Path) -> Result<(Vec<u8>, usize)> {
    use std::io::Read;
    let meta = fs::metadata(path)?;
    let file_len = meta.len() as usize;
    let pad = padding();
    let mut buf = vec![0u8; file_len + pad];
    let mut f = fs::File::open(path)?;
    f.read_exact(&mut buf[..file_len])?;
    // Padding bytes are already zeroed from vec! initialization
    Ok((buf, file_len))
}

/// Create a padded copy of an in-memory slice.
pub fn pad_buffer(data: &[u8]) -> Vec<u8> {
    let pad = padding();
    let mut buf = Vec::with_capacity(data.len() + pad);
    buf.extend_from_slice(data);
    buf.resize(data.len() + pad, 0);
    buf
}

fn check(code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        bail!("simdjson error code {code}");
    }
}

/// Wraps a simdjson On-Demand parser. Reusable across multiple documents.
///
/// Send but NOT Sync — each thread needs its own Parser.
pub struct Parser {
    ptr: *mut JxParser,
}

unsafe impl Send for Parser {}

impl Parser {
    pub fn new() -> Result<Self> {
        let ptr = unsafe { jx_parser_new() };
        if ptr.is_null() {
            bail!("failed to allocate simdjson parser");
        }
        Ok(Self { ptr })
    }

    /// Parse a JSON document using On-Demand API.
    ///
    /// `buf` must contain at least `padding()` extra zeroed bytes after `json_len`.
    /// The returned `Document` borrows `self` — you cannot parse another document
    /// until the `Document` is dropped.
    pub fn parse<'a>(&'a mut self, buf: &'a [u8], json_len: usize) -> Result<Document<'a>> {
        assert!(
            buf.len() >= json_len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        check(unsafe { jx_parse_ondemand(self.ptr, buf.as_ptr().cast(), json_len) })?;
        Ok(Document {
            parser: self,
            _phantom: std::marker::PhantomData,
        })
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        unsafe { jx_parser_free(self.ptr) };
    }
}

/// A parsed On-Demand document. Borrows the parser (which owns internal buffers).
pub struct Document<'a> {
    parser: &'a mut Parser,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> Document<'a> {
    /// Extract a string field from the top-level object.
    /// Returns a reference into the parser's internal buffer — valid until next parse.
    pub fn find_field_str(&mut self, key: &str) -> Result<&'a str> {
        let mut out_ptr: *const c_char = std::ptr::null();
        let mut out_len: usize = 0;
        check(unsafe {
            jx_doc_find_field_str(
                self.parser.ptr,
                key.as_ptr().cast(),
                key.len(),
                &mut out_ptr,
                &mut out_len,
            )
        })?;
        let slice = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) };
        Ok(std::str::from_utf8(slice)?)
    }

    /// Extract an int64 field from the top-level object.
    pub fn find_field_int64(&mut self, key: &str) -> Result<i64> {
        let mut out: i64 = 0;
        check(unsafe {
            jx_doc_find_field_int64(self.parser.ptr, key.as_ptr().cast(), key.len(), &mut out)
        })?;
        Ok(out)
    }

    /// Extract a double field from the top-level object.
    pub fn find_field_double(&mut self, key: &str) -> Result<f64> {
        let mut out: f64 = 0.0;
        check(unsafe {
            jx_doc_find_field_double(self.parser.ptr, key.as_ptr().cast(), key.len(), &mut out)
        })?;
        Ok(out)
    }

    /// Get the JSON type of the document root.
    pub fn doc_type(&mut self) -> Result<JsonType> {
        let mut t: i32 = 0;
        check(unsafe { jx_doc_type(self.parser.ptr, &mut t) })?;
        Ok(JsonType::from_raw(t))
    }
}

/// JSON value types as reported by simdjson.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonType {
    Array,
    Object,
    Number,
    String,
    Boolean,
    Null,
    Unknown(i32),
}

impl JsonType {
    fn from_raw(v: i32) -> Self {
        // simdjson json_type: unknown=0, array=1, object=2, number=3,
        // string=4, boolean=5, null=6
        match v {
            1 => Self::Array,
            2 => Self::Object,
            3 => Self::Number,
            4 => Self::String,
            5 => Self::Boolean,
            6 => Self::Null,
            other => Self::Unknown(other),
        }
    }
}

// ---------------------------------------------------------------------------
// iterate_many helpers (run full loop in C++ for benchmarking)
// ---------------------------------------------------------------------------

/// Count documents in a padded NDJSON buffer using simdjson's iterate_many.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn iterate_many_count(buf: &[u8], json_len: usize, batch_size: usize) -> Result<u64> {
    assert!(buf.len() >= json_len + padding());
    let mut count: u64 = 0;
    check(unsafe { jx_iterate_many_count(buf.as_ptr().cast(), json_len, batch_size, &mut count) })?;
    Ok(count)
}

/// Extract a string field from every NDJSON document, returning total bytes extracted.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn iterate_many_extract_field(
    buf: &[u8],
    json_len: usize,
    batch_size: usize,
    field: &str,
) -> Result<u64> {
    assert!(buf.len() >= json_len + padding());
    let mut total: u64 = 0;
    check(unsafe {
        jx_iterate_many_extract_field(
            buf.as_ptr().cast(),
            json_len,
            batch_size,
            field.as_ptr().cast(),
            field.len(),
            &mut total,
        )
    })?;
    Ok(total)
}

// ---------------------------------------------------------------------------
// DOM parse → Value (via flat token buffer)
// ---------------------------------------------------------------------------

// Token tags (must match bridge.cpp)
const TAG_NULL: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_INT: u8 = 2;
const TAG_DOUBLE: u8 = 3;
const TAG_STRING: u8 = 4;
const TAG_ARRAY_START: u8 = 5;
const TAG_ARRAY_END: u8 = 6;
const TAG_OBJECT_START: u8 = 7;
const TAG_OBJECT_END: u8 = 8;

/// Parse a JSON buffer via simdjson DOM API and return a `Value` tree.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn dom_parse_to_value(buf: &[u8], json_len: usize) -> Result<Value> {
    assert!(buf.len() >= json_len + padding());
    let mut flat_ptr: *mut u8 = std::ptr::null_mut();
    let mut flat_len: usize = 0;
    check(unsafe { jx_dom_to_flat(buf.as_ptr().cast(), json_len, &mut flat_ptr, &mut flat_len) })?;

    // Safety: flat_ptr is a heap allocation from C++ new[].
    let flat = unsafe { std::slice::from_raw_parts(flat_ptr, flat_len) };
    let result = decode_value(flat, &mut 0);
    unsafe { jx_flat_buffer_free(flat_ptr) };
    result
}

fn read_u8(buf: &[u8], pos: &mut usize) -> Result<u8> {
    if *pos >= buf.len() {
        bail!("flat token buffer truncated at byte {}", *pos);
    }
    let v = buf[*pos];
    *pos += 1;
    Ok(v)
}

fn read_u32(buf: &[u8], pos: &mut usize) -> Result<u32> {
    if *pos + 4 > buf.len() {
        bail!("flat token buffer truncated at byte {}", *pos);
    }
    let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    Ok(v)
}

fn read_i64(buf: &[u8], pos: &mut usize) -> Result<i64> {
    if *pos + 8 > buf.len() {
        bail!("flat token buffer truncated at byte {}", *pos);
    }
    let v = i64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    Ok(v)
}

fn read_f64(buf: &[u8], pos: &mut usize) -> Result<f64> {
    if *pos + 8 > buf.len() {
        bail!("flat token buffer truncated at byte {}", *pos);
    }
    let v = f64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    Ok(v)
}

fn read_string(buf: &[u8], pos: &mut usize) -> Result<String> {
    let len = read_u32(buf, pos)? as usize;
    if *pos + len > buf.len() {
        bail!(
            "flat token buffer truncated reading string at byte {}",
            *pos
        );
    }
    let s = std::str::from_utf8(&buf[*pos..*pos + len])?.to_string();
    *pos += len;
    Ok(s)
}

fn decode_value(buf: &[u8], pos: &mut usize) -> Result<Value> {
    let tag = read_u8(buf, pos)?;
    match tag {
        TAG_NULL => Ok(Value::Null),
        TAG_BOOL => {
            let v = read_u8(buf, pos)?;
            Ok(Value::Bool(v != 0))
        }
        TAG_INT => {
            let v = read_i64(buf, pos)?;
            Ok(Value::Int(v))
        }
        TAG_DOUBLE => {
            let v = read_f64(buf, pos)?;
            Ok(Value::Double(v))
        }
        TAG_STRING => {
            let s = read_string(buf, pos)?;
            Ok(Value::String(s))
        }
        TAG_ARRAY_START => {
            let count = read_u32(buf, pos)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                arr.push(decode_value(buf, pos)?);
            }
            let end_tag = read_u8(buf, pos)?;
            if end_tag != TAG_ARRAY_END {
                bail!("expected ArrayEnd tag, got {end_tag}");
            }
            Ok(Value::Array(Rc::new(arr)))
        }
        TAG_OBJECT_START => {
            let count = read_u32(buf, pos)? as usize;
            let mut obj = Vec::with_capacity(count);
            for _ in 0..count {
                // Key is emitted as a String token
                let key_tag = read_u8(buf, pos)?;
                if key_tag != TAG_STRING {
                    bail!("expected String tag for object key, got {key_tag}");
                }
                let key = read_string(buf, pos)?;
                let val = decode_value(buf, pos)?;
                obj.push((key, val));
            }
            let end_tag = read_u8(buf, pos)?;
            if end_tag != TAG_OBJECT_END {
                bail!("expected ObjectEnd tag, got {end_tag}");
            }
            Ok(Value::Object(Rc::new(obj)))
        }
        _ => bail!("unknown flat token tag {tag}"),
    }
}

// ---------------------------------------------------------------------------
// Minify — compact JSON via simdjson::minify(), no DOM construction.
// ---------------------------------------------------------------------------

/// Minify JSON using simdjson's SIMD-accelerated minifier.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
/// Returns the compacted JSON as a `Vec<u8>`.
pub fn minify(buf: &[u8], json_len: usize) -> Result<Vec<u8>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    check(unsafe { jx_minify(buf.as_ptr().cast(), json_len, &mut out_ptr, &mut out_len) })?;
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_is_nonzero() {
        assert!(padding() > 0);
    }

    #[test]
    fn parse_and_extract_string() {
        let json = br#"{"name": "hello", "age": 42}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.find_field_str("name").unwrap(), "hello");
    }

    #[test]
    fn parse_and_extract_int() {
        let json = br#"{"name": "hello", "age": 42}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();

        // On-Demand is forward-only — parse fresh for each field access order.
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.find_field_int64("age").unwrap(), 42);
    }

    #[test]
    fn parse_and_extract_double() {
        let json = br#"{"pi": 3.14159}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        let val = doc.find_field_double("pi").unwrap();
        assert!((val - 3.14159).abs() < 1e-10);
    }

    #[test]
    fn doc_type_object() {
        let json = br#"{"x": 1}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.doc_type().unwrap(), JsonType::Object);
    }

    #[test]
    fn parser_reuse() {
        let mut parser = Parser::new().unwrap();

        let json1 = br#"{"a": "first"}"#;
        let buf1 = pad_buffer(json1);
        {
            let mut doc = parser.parse(&buf1, json1.len()).unwrap();
            assert_eq!(doc.find_field_str("a").unwrap(), "first");
        }

        let json2 = br#"{"a": "second"}"#;
        let buf2 = pad_buffer(json2);
        {
            let mut doc = parser.parse(&buf2, json2.len()).unwrap();
            assert_eq!(doc.find_field_str("a").unwrap(), "second");
        }
    }

    #[test]
    fn invalid_json_returns_error() {
        // On-Demand is lazy — iterate() may succeed, but consuming the document fails.
        let json = b"not json at all!!!";
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        if let Ok(mut doc) = result {
            // If parse didn't fail, accessing content should fail.
            assert!(doc.doc_type().is_err() || doc.find_field_str("x").is_err());
        }
        // If parse itself failed, that's also correct.
    }

    #[test]
    fn iterate_many_count_basic() {
        let ndjson = b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
        let buf = pad_buffer(ndjson);
        let count = iterate_many_count(&buf, ndjson.len(), 1_000_000).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn iterate_many_extract_field_basic() {
        let ndjson = b"{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n{\"name\":\"charlie\"}\n";
        let buf = pad_buffer(ndjson);
        let total = iterate_many_extract_field(&buf, ndjson.len(), 1_000_000, "name").unwrap();
        // "alice" (5) + "bob" (3) + "charlie" (7) = 15
        assert_eq!(total, 15);
    }

    // -----------------------------------------------------------------------
    // FFI edge-case tests — exercise C++ bridge with adversarial inputs
    // -----------------------------------------------------------------------

    #[test]
    fn parse_empty_input() {
        let json = b"";
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        // Empty input should error at parse or on access
        if let Ok(mut doc) = result {
            assert!(doc.doc_type().is_err());
        }
    }

    #[test]
    fn parse_only_whitespace() {
        let json = b"   \t\n  ";
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        if let Ok(mut doc) = result {
            assert!(doc.doc_type().is_err());
        }
    }

    #[test]
    fn parse_truncated_json() {
        let json = br#"{"name": "hel"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        if let Ok(mut doc) = result {
            assert!(doc.find_field_str("name").is_err());
        }
    }

    #[test]
    fn parse_null_bytes_in_input() {
        let json = b"{\"a\": \"b\x00c\"}";
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        // simdjson should reject unescaped control characters in strings
        if let Ok(mut doc) = result {
            assert!(doc.find_field_str("a").is_err());
        }
    }

    #[test]
    fn parse_deeply_nested_arrays() {
        // 1100 levels of nesting — exceeds simdjson's 1024 depth limit.
        // On-Demand is lazy: parse/doc_type may succeed (it just sees '['),
        // but the DOM path should reject it.
        let mut json = Vec::new();
        for _ in 0..1100 {
            json.push(b'[');
        }
        json.push(b'1');
        for _ in 0..1100 {
            json.push(b']');
        }
        let buf = pad_buffer(&json);
        // DOM parse should fail on excessive depth
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
    }

    #[test]
    fn parse_deeply_nested_objects() {
        // 1100 levels of object nesting — exceeds simdjson's 1024 depth limit.
        let mut json = Vec::new();
        for i in 0..1100 {
            json.extend_from_slice(format!("{{\"k{}\":", i).as_bytes());
        }
        json.extend_from_slice(b"null");
        for _ in 0..1100 {
            json.push(b'}');
        }
        let buf = pad_buffer(&json);
        // DOM parse should fail on excessive depth
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
    }

    #[test]
    fn parse_max_length_string_key() {
        // 1MB key name
        let key = "k".repeat(1_000_000);
        let json = format!("{{\"{}\": 1}}", key);
        let json_bytes = json.as_bytes();
        let buf = pad_buffer(json_bytes);
        let mut parser = Parser::new().unwrap();
        let mut doc = parser.parse(&buf, json_bytes.len()).unwrap();
        assert_eq!(doc.find_field_int64(&key).unwrap(), 1);
    }

    #[test]
    fn parse_unicode_escape_sequences() {
        let json = br#"{"emoji": "\u0048\u0065\u006C\u006C\u006F"}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.find_field_str("emoji").unwrap(), "Hello");
    }

    #[test]
    fn parse_lone_surrogate() {
        // \uD800 without trailing surrogate — simdjson may accept or reject
        let json = br#"{"s": "\uD800"}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        // Either parse fails or field extraction fails — both are acceptable
        if let Ok(mut doc) = result {
            // If simdjson accepts it, the Rust from_utf8 check should catch it
            let _ = doc.find_field_str("s");
        }
    }

    #[test]
    fn parse_many_types_in_one_doc() {
        let json = br#"{"s":"a","i":42,"d":1.5,"b":true,"n":null,"a":[1],"o":{"x":1}}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        // Parse and check the first field only (On-Demand is forward-only)
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.find_field_str("s").unwrap(), "a");
    }

    #[test]
    fn iterate_many_empty_input() {
        let ndjson = b"";
        let buf = pad_buffer(ndjson);
        let count = iterate_many_count(&buf, ndjson.len(), 1_000_000).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn iterate_many_only_whitespace() {
        let ndjson = b"\n\n\n";
        let buf = pad_buffer(ndjson);
        let count = iterate_many_count(&buf, ndjson.len(), 1_000_000).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn iterate_many_single_doc() {
        let ndjson = b"{\"a\":1}\n";
        let buf = pad_buffer(ndjson);
        let count = iterate_many_count(&buf, ndjson.len(), 1_000_000).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn iterate_many_no_trailing_newline() {
        let ndjson = b"{\"a\":1}\n{\"a\":2}";
        let buf = pad_buffer(ndjson);
        let count = iterate_many_count(&buf, ndjson.len(), 1_000_000).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dom_parse_empty_input() {
        let json = b"";
        let buf = pad_buffer(json);
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
    }

    #[test]
    fn dom_parse_whitespace_only() {
        let json = b"   ";
        let buf = pad_buffer(json);
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
    }

    #[test]
    fn dom_parse_truncated() {
        let json = br#"{"a": [1, 2"#;
        let buf = pad_buffer(json);
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
    }

    #[test]
    fn dom_parse_deeply_nested() {
        // 1100 levels — exceeds simdjson 1024 limit
        let mut json = Vec::new();
        for _ in 0..1100 {
            json.push(b'[');
        }
        json.push(b'1');
        for _ in 0..1100 {
            json.push(b']');
        }
        let buf = pad_buffer(&json);
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
    }

    #[test]
    fn dom_parse_empty_object() {
        let json = b"{}";
        let buf = pad_buffer(json);
        assert_eq!(
            dom_parse_to_value(&buf, json.len()).unwrap(),
            Value::Object(Rc::new(vec![]))
        );
    }

    #[test]
    fn dom_parse_empty_array() {
        let json = b"[]";
        let buf = pad_buffer(json);
        assert_eq!(
            dom_parse_to_value(&buf, json.len()).unwrap(),
            Value::Array(Rc::new(vec![]))
        );
    }

    #[test]
    fn dom_parse_large_integer() {
        let json = b"9223372036854775807"; // i64::MAX
        let buf = pad_buffer(json);
        assert_eq!(
            dom_parse_to_value(&buf, json.len()).unwrap(),
            Value::Int(i64::MAX)
        );
    }

    #[test]
    fn dom_parse_uint64_beyond_i64() {
        let json = b"9223372036854775808"; // i64::MAX + 1, should become Double
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        match val {
            Value::Double(d) => assert!((d - 9223372036854775808.0).abs() < 1.0),
            other => panic!("expected Double, got {:?}", other),
        }
    }

    #[test]
    fn dom_parse_negative_integer() {
        let json = b"-9223372036854775808"; // i64::MIN
        let buf = pad_buffer(json);
        assert_eq!(
            dom_parse_to_value(&buf, json.len()).unwrap(),
            Value::Int(i64::MIN)
        );
    }

    #[test]
    fn dom_parse_escaped_strings() {
        let json = br#"{"s": "a\"b\\c\/d\n\t\r"}"#;
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        match val {
            Value::Object(fields) => {
                assert_eq!(fields[0].1, Value::String("a\"b\\c/d\n\t\r".into()));
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn dom_parse_simple_object() {
        let json = br#"{"name": "alice", "age": 30, "active": true}"#;
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        assert_eq!(
            val,
            Value::Object(Rc::new(vec![
                ("name".into(), Value::String("alice".into())),
                ("age".into(), Value::Int(30)),
                ("active".into(), Value::Bool(true)),
            ]))
        );
    }

    #[test]
    fn dom_parse_nested() {
        let json = br#"{"a": [1, 2], "b": {"c": null}}"#;
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        assert_eq!(
            val,
            Value::Object(Rc::new(vec![
                (
                    "a".into(),
                    Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]))
                ),
                (
                    "b".into(),
                    Value::Object(Rc::new(vec![("c".into(), Value::Null)]))
                ),
            ]))
        );
    }

    #[test]
    fn dom_parse_array() {
        let json = br#"[1, "two", 3.14, false, null]"#;
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        assert_eq!(
            val,
            Value::Array(Rc::new(vec![
                Value::Int(1),
                Value::String("two".into()),
                Value::Double(3.14),
                Value::Bool(false),
                Value::Null,
            ]))
        );
    }

    #[test]
    fn dom_parse_scalar() {
        let json = b"42";
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        assert_eq!(val, Value::Int(42));
    }

    #[test]
    fn dom_parse_string() {
        let json = br#""hello world""#;
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        assert_eq!(val, Value::String("hello world".into()));
    }

    // -----------------------------------------------------------------------
    // Minify tests
    // -----------------------------------------------------------------------

    #[test]
    fn minify_object() {
        let json = br#"{ "a" : 1 , "b" : [2, 3] }"#;
        let buf = pad_buffer(json);
        let out = minify(&buf, json.len()).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"{"a":1,"b":[2,3]}"#);
    }

    #[test]
    fn minify_already_compact() {
        let json = br#"{"a":1}"#;
        let buf = pad_buffer(json);
        let out = minify(&buf, json.len()).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn minify_scalar() {
        let json = b"42";
        let buf = pad_buffer(json);
        let out = minify(&buf, json.len()).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "42");
    }

    #[test]
    fn minify_empty_input() {
        let json = b"";
        let buf = pad_buffer(json);
        // Empty input may succeed with empty output or error — both acceptable
        let _ = minify(&buf, json.len());
    }
}
