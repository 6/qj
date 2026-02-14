//! Safe wrapper types for the simdjson parser: `Parser`, `Document`, `JsonType`,
//! and utility functions for padded buffer management.
//!
//! `Parser` is `Send` but not `Sync` — simdjson parsers are reusable but not
//! thread-safe. Each thread in the parallel NDJSON pipeline gets its own parser.

use anyhow::{Result, bail};
use std::ffi::c_char;
use std::fs;
use std::path::Path;

use super::ffi::*;

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

pub(super) fn check(code: i32) -> Result<()> {
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
    pub(super) ptr: *mut JxParser,
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
        let json = b"not json at all!!!";
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        if let Ok(mut doc) = result {
            assert!(doc.doc_type().is_err() || doc.find_field_str("x").is_err());
        }
    }

    #[test]
    fn parse_empty_input() {
        let json = b"";
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
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
        if let Ok(mut doc) = result {
            assert!(doc.find_field_str("a").is_err());
        }
    }

    #[test]
    fn parse_max_length_string_key() {
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
        let json = br#"{"s": "\uD800"}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let result = parser.parse(&buf, json.len());
        if let Ok(mut doc) = result {
            let _ = doc.find_field_str("s");
        }
    }

    #[test]
    fn parse_many_types_in_one_doc() {
        let json = br#"{"s":"a","i":42,"d":1.5,"b":true,"n":null,"a":[1],"o":{"x":1}}"#;
        let buf = pad_buffer(json);
        let mut parser = Parser::new().unwrap();
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.find_field_str("s").unwrap(), "a");
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
        assert_eq!(total, 15);
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
}
