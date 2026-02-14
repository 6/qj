//! DOM-level operations: parse to Value tree, minify, and field extraction.

use anyhow::{Result, bail};
use std::ffi::c_char;
use std::rc::Rc;

use crate::value::Value;

use super::ffi::*;
use super::types::{check, padding};

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
            let raw_len = read_u32(buf, pos)? as usize;
            let raw = if raw_len > 0 {
                if *pos + raw_len > buf.len() {
                    bail!(
                        "flat token buffer truncated reading raw double text at byte {}",
                        *pos
                    );
                }
                let s = std::str::from_utf8(&buf[*pos..*pos + raw_len])?.to_string();
                *pos += raw_len;
                Some(s.into_boxed_str())
            } else {
                None
            };
            Ok(Value::Double(v, raw))
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
// DOM field extraction — parse, find nested field, return raw JSON bytes.
// ---------------------------------------------------------------------------

/// DOM parse a JSON buffer, navigate a chain of field names, and return
/// the raw compact JSON bytes of the found sub-tree.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
/// `fields` is the chain of field names (e.g. `["a", "b", "c"]` for `.a.b.c`).
/// Missing fields or non-object inputs return `b"null"` (jq semantics).
pub fn dom_find_field_raw(buf: &[u8], json_len: usize, fields: &[&str]) -> Result<Vec<u8>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let field_ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast()).collect();
    let field_lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    check(unsafe {
        jx_dom_find_field_raw(
            buf.as_ptr().cast(),
            json_len,
            field_ptrs.as_ptr(),
            field_lens.as_ptr(),
            fields.len(),
            &mut out_ptr,
            &mut out_len,
        )
    })?;
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(result)
}

/// DOM parse, navigate fields, and compute `length` in C++.
///
/// Returns `Ok(Some(bytes))` on success (decimal string), `Ok(None)` if the
/// target type is unsupported (Int/Double/Bool — caller should fall back).
pub fn dom_field_length(buf: &[u8], json_len: usize, fields: &[&str]) -> Result<Option<Vec<u8>>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let field_ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast()).collect();
    let field_lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    check(unsafe {
        jx_dom_field_length(
            buf.as_ptr().cast(),
            json_len,
            field_ptrs.as_ptr(),
            field_lens.as_ptr(),
            fields.len(),
            &mut out_ptr,
            &mut out_len,
        )
    })?;
    if out_len == usize::MAX - 1 {
        return Ok(None);
    }
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(Some(result))
}

/// DOM parse, navigate fields, and compute `keys` in C++.
///
/// Returns `Ok(Some(bytes))` on success (JSON array string), `Ok(None)` if the
/// target type is unsupported (caller should fall back).
pub fn dom_field_keys(buf: &[u8], json_len: usize, fields: &[&str]) -> Result<Option<Vec<u8>>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let field_ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast()).collect();
    let field_lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    check(unsafe {
        jx_dom_field_keys(
            buf.as_ptr().cast(),
            json_len,
            field_ptrs.as_ptr(),
            field_lens.as_ptr(),
            fields.len(),
            &mut out_ptr,
            &mut out_len,
        )
    })?;
    if out_len == usize::MAX - 1 {
        return Ok(None);
    }
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(Some(result))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::types::pad_buffer;
    use super::*;

    #[test]
    fn parse_deeply_nested_arrays() {
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
    fn parse_deeply_nested_objects() {
        let mut json = Vec::new();
        for i in 0..1100 {
            json.extend_from_slice(format!("{{\"k{}\":", i).as_bytes());
        }
        json.extend_from_slice(b"null");
        for _ in 0..1100 {
            json.push(b'}');
        }
        let buf = pad_buffer(&json);
        assert!(dom_parse_to_value(&buf, json.len()).is_err());
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
        let json = b"9223372036854775807";
        let buf = pad_buffer(json);
        assert_eq!(
            dom_parse_to_value(&buf, json.len()).unwrap(),
            Value::Int(i64::MAX)
        );
    }

    #[test]
    fn dom_parse_uint64_beyond_i64() {
        let json = b"9223372036854775808";
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        match val {
            Value::Double(d, _) => assert!((d - 9223372036854775808.0).abs() < 1.0),
            other => panic!("expected Double, got {:?}", other),
        }
    }

    #[test]
    fn dom_parse_negative_integer() {
        let json = b"-9223372036854775808";
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
                Value::Double(3.14, None),
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
        let _ = minify(&buf, json.len());
    }

    #[test]
    fn field_raw_basic() {
        let json = br#"{"name":"alice","age":30}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["name"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#""alice""#);
    }

    #[test]
    fn field_raw_object_value() {
        let json = br#"{"data":{"x":1,"y":[2,3]}}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["data"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"{"x":1,"y":[2,3]}"#);
    }

    #[test]
    fn field_raw_nested() {
        let json = br#"{"a":{"b":{"c":42}}}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["a", "b", "c"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "42");
    }

    #[test]
    fn field_raw_missing() {
        let json = br#"{"name":"alice"}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["missing"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "null");
    }

    #[test]
    fn field_raw_non_object() {
        let json = b"[1,2,3]";
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["x"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "null");
    }

    #[test]
    fn field_raw_nested_missing() {
        let json = br#"{"a":{"b":{"c":42}}}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["a", "b", "missing"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "null");
    }

    #[test]
    fn field_raw_int_value() {
        let json = br#"{"count":42}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["count"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "42");
    }

    #[test]
    fn field_raw_bool_value() {
        let json = br#"{"active":true}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["active"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "true");
    }

    #[test]
    fn field_raw_null_value() {
        let json = br#"{"val":null}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["val"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "null");
    }

    #[test]
    fn field_raw_array_value() {
        let json = br#"{"items":[1,2,3]}"#;
        let buf = pad_buffer(json);
        let out = dom_find_field_raw(&buf, json.len(), &["items"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "[1,2,3]");
    }

    #[test]
    fn field_length_array() {
        let json = br#"{"items":[1,2,3]}"#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &["items"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "3");
    }

    #[test]
    fn field_length_object() {
        let json = br#"{"data":{"a":1,"b":2}}"#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &["data"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "2");
    }

    #[test]
    fn field_length_string() {
        let json = br#"{"name":"hello"}"#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &["name"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "5");
    }

    #[test]
    fn field_length_missing_is_zero() {
        let json = br#"{"name":"alice"}"#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &["missing"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "0");
    }

    #[test]
    fn field_length_null_is_zero() {
        let json = br#"{"val":null}"#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &["val"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "0");
    }

    #[test]
    fn field_length_number_unsupported() {
        let json = br#"{"n":42}"#;
        let buf = pad_buffer(json);
        assert!(
            dom_field_length(&buf, json.len(), &["n"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn field_length_bare_array() {
        let json = b"[1,2,3,4,5]";
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &[]).unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "5");
    }

    #[test]
    fn field_length_bare_string() {
        let json = br#""hello""#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &[]).unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "5");
    }

    #[test]
    fn field_length_nested() {
        let json = br#"{"a":{"b":[1,2]}}"#;
        let buf = pad_buffer(json);
        let out = dom_field_length(&buf, json.len(), &["a", "b"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "2");
    }

    #[test]
    fn field_keys_object() {
        let json = br#"{"data":{"b":2,"a":1}}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &["data"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["a","b"]"#);
    }

    #[test]
    fn field_keys_array() {
        let json = br#"{"items":["x","y","z"]}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &["items"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "[0,1,2]");
    }

    #[test]
    fn field_keys_bare_object() {
        let json = br#"{"b":2,"a":1,"c":3}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &[]).unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["a","b","c"]"#);
    }

    #[test]
    fn field_keys_missing_unsupported() {
        let json = br#"{"name":"alice"}"#;
        let buf = pad_buffer(json);
        assert!(
            dom_field_keys(&buf, json.len(), &["missing"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn field_keys_string_unsupported() {
        let json = br#"{"name":"alice"}"#;
        let buf = pad_buffer(json);
        assert!(
            dom_field_keys(&buf, json.len(), &["name"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn field_keys_escaped_key() {
        let json = br#"{"data":{"key\"with\\escape":1}}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &["data"])
            .unwrap()
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            r#"["key\"with\\escape"]"#
        );
    }
}
