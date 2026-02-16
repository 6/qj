//! DOM-level operations: parse to Value tree, minify, and field extraction.

use anyhow::{Result, bail};
use std::ffi::c_char;
use std::sync::Arc;

use crate::value::Value;

use super::ffi::*;
use super::types::{check, padding};

/// simdjson CAPACITY error code — returned when input exceeds ~4GB single-document limit.
pub const SIMDJSON_CAPACITY: i32 = 1;

// Token tags (must match bridge.cpp)
pub(crate) const TAG_NULL: u8 = 0;
pub(crate) const TAG_BOOL: u8 = 1;
pub(crate) const TAG_INT: u8 = 2;
pub(crate) const TAG_DOUBLE: u8 = 3;
pub(crate) const TAG_STRING: u8 = 4;
pub(crate) const TAG_ARRAY_START: u8 = 5;
pub(crate) const TAG_ARRAY_END: u8 = 6;
pub(crate) const TAG_OBJECT_START: u8 = 7;
pub(crate) const TAG_OBJECT_END: u8 = 8;

/// Parse a JSON buffer via simdjson DOM API and return a `Value` tree.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn dom_parse_to_value(buf: &[u8], json_len: usize) -> Result<Value> {
    assert!(buf.len() >= json_len + padding());
    let mut flat_ptr: *mut u8 = std::ptr::null_mut();
    let mut flat_len: usize = 0;
    // SAFETY: buf points to a valid buffer with json_len + SIMDJSON_PADDING bytes
    // (asserted above). flat_ptr/flat_len are valid stack references used as output
    // parameters. C++ heap-allocates the flat token buffer.
    //
    // Uses On-Demand path (not DOM tape walk) because this function is called from
    // fromjson with arbitrary user strings. The DOM parser may not handle all
    // malformed inputs the same way as On-Demand (different error propagation).
    check(unsafe { jx_dom_to_flat(buf.as_ptr().cast(), json_len, &mut flat_ptr, &mut flat_len) })?;

    // SAFETY: flat_ptr was heap-allocated by jx_dom_to_flat above and flat_len is
    // its byte count. We decode into a Value tree immediately; the pointer is freed
    // on the next line.
    let flat = unsafe { std::slice::from_raw_parts(flat_ptr, flat_len) };
    let result = decode_value(flat, &mut 0);
    // SAFETY: flat_ptr was allocated by C++ new[] in jx_dom_to_flat and has not
    // been freed yet. After this call the pointer is not used again.
    unsafe { jx_flat_buffer_free(flat_ptr) };
    result
}

/// Owns the flat token buffer allocated by C++.
///
/// The flat buffer uses a tag-length-value encoding that can be navigated
/// by `FlatValue` without allocating a full Rust `Value` tree.
pub struct FlatBuffer {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: The flat buffer is an independent heap allocation with no interior
// mutability or shared state. It can safely be sent across threads.
unsafe impl Send for FlatBuffer {}

impl FlatBuffer {
    /// Create from raw C++ allocated pointer and length.
    pub(crate) fn from_raw(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Get a reference to the flat buffer bytes.
    pub fn as_bytes(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: ptr was heap-allocated by jx_dom_to_flat with len bytes.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    /// Get a `FlatValue` pointing to the root of the flat buffer.
    pub fn root(&self) -> crate::flat_value::FlatValue<'_> {
        crate::flat_value::FlatValue::new(self.as_bytes(), 0)
    }
}

impl Drop for FlatBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: ptr was allocated by C++ new[] in jx_dom_to_flat.
            unsafe { jx_flat_buffer_free(self.ptr) };
        }
    }
}

/// Parse a JSON buffer and return a `Value` tree using the faster DOM tape walk.
///
/// Uses `jx_dom_to_flat_via_tape` (DOM tape walk) for flat buffer construction,
/// then decodes to a Value tree. Faster than `dom_parse_to_value()` which uses
/// On-Demand API, but only safe for inputs known to be valid JSON from files
/// (not arbitrary user strings like `fromjson` input, since the DOM parser handles
/// some malformed inputs differently than On-Demand).
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn dom_parse_to_value_fast(buf: &[u8], json_len: usize) -> Result<Value> {
    let flat_buf = dom_parse_to_flat_buf_tape(buf, json_len)?;
    decode_value(flat_buf.as_bytes(), &mut 0)
}

/// Parse a JSON buffer via simdjson On-Demand API and return the raw flat token buffer.
///
/// Uses the On-Demand path which creates a fresh parser per call — suitable for
/// per-line NDJSON processing where each call parses a small document.
/// For single large documents, prefer `dom_parse_to_flat_buf_tape` which uses the
/// faster DOM tape walk.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn dom_parse_to_flat_buf(buf: &[u8], json_len: usize) -> Result<FlatBuffer> {
    assert!(buf.len() >= json_len + padding());
    let mut flat_ptr: *mut u8 = std::ptr::null_mut();
    let mut flat_len: usize = 0;
    // SAFETY: buf points to a valid buffer with json_len + SIMDJSON_PADDING bytes
    // (asserted above). flat_ptr/flat_len are valid stack references used as output
    // parameters. C++ heap-allocates the flat token buffer.
    check(unsafe { jx_dom_to_flat(buf.as_ptr().cast(), json_len, &mut flat_ptr, &mut flat_len) })?;
    Ok(FlatBuffer::from_raw(flat_ptr, flat_len))
}

/// Parse a JSON buffer via DOM tape walk and return the raw flat token buffer.
///
/// Uses the DOM tape walk path which is ~2x faster than On-Demand for large
/// documents (pre-indexed tape, pre-unescaped strings). Best for single-document
/// JSON processing where the function is called once per file.
///
/// `buf` must include SIMDJSON_PADDING extra zeroed bytes after `json_len`.
pub fn dom_parse_to_flat_buf_tape(buf: &[u8], json_len: usize) -> Result<FlatBuffer> {
    assert!(buf.len() >= json_len + padding());
    let mut flat_ptr: *mut u8 = std::ptr::null_mut();
    let mut flat_len: usize = 0;
    check(unsafe {
        jx_dom_to_flat_via_tape(buf.as_ptr().cast(), json_len, &mut flat_ptr, &mut flat_len)
    })?;
    Ok(FlatBuffer::from_raw(flat_ptr, flat_len))
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

pub(crate) fn decode_value(buf: &[u8], pos: &mut usize) -> Result<Value> {
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
            Ok(Value::Array(Arc::new(arr)))
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
            Ok(Value::Object(Arc::new(obj)))
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
    // SAFETY: buf points to a valid buffer with json_len + SIMDJSON_PADDING bytes
    // (asserted above). out_ptr/out_len are valid stack references used as output
    // parameters. C++ heap-allocates the minified result.
    check(unsafe { jx_minify(buf.as_ptr().cast(), json_len, &mut out_ptr, &mut out_len) })?;
    // SAFETY: out_ptr was heap-allocated by jx_minify above and out_len is its byte
    // count. We copy into a Vec immediately; the pointer is freed on the next line.
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    // SAFETY: out_ptr was allocated by C++ new[] in jx_minify and has not been freed
    // yet. After this call the pointer is not used again.
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
    // SAFETY: buf points to a valid buffer with json_len + SIMDJSON_PADDING bytes
    // (asserted above). field_ptrs/field_lens point to valid slices matching
    // fields.len(). out_ptr/out_len are valid stack references. C++ heap-allocates
    // the result.
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
    // SAFETY: out_ptr was heap-allocated by jx_dom_find_field_raw above and out_len
    // is its byte count. We copy into a Vec immediately; the pointer is freed on the
    // next line.
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    // SAFETY: out_ptr was allocated by C++ new[] in jx_dom_find_field_raw and has
    // not been freed yet. After this call the pointer is not used again.
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
    // SAFETY: buf points to a valid buffer with json_len + SIMDJSON_PADDING bytes
    // (asserted above). field_ptrs/field_lens point to valid slices matching
    // fields.len(). out_ptr/out_len are valid stack references. C++ heap-allocates
    // the result.
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
    // SAFETY: out_ptr was heap-allocated by jx_dom_field_length above and out_len
    // is its byte count. We copy into a Vec immediately; the pointer is freed on the
    // next line.
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    // SAFETY: out_ptr was allocated by C++ new[] in jx_dom_field_length and has not
    // been freed yet. After this call the pointer is not used again.
    unsafe { jx_minify_free(out_ptr) };
    Ok(Some(result))
}

/// DOM parse, navigate fields, and compute `keys` or `keys_unsorted` in C++.
///
/// Returns `Ok(Some(bytes))` on success (JSON array string), `Ok(None)` if the
/// target type is unsupported (caller should fall back).
/// `sorted`: true for `keys` (alphabetically sorted), false for `keys_unsorted`.
pub fn dom_field_keys(
    buf: &[u8],
    json_len: usize,
    fields: &[&str],
    sorted: bool,
) -> Result<Option<Vec<u8>>> {
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
            if sorted { 1 } else { 0 },
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

/// DOM parse, navigate fields, and check `has("key")` in C++.
///
/// Returns `Ok(Some(true/false))` on success, `Ok(None)` if the target is not
/// an object (caller should fall back).
pub fn dom_field_has(
    buf: &[u8],
    json_len: usize,
    fields: &[&str],
    key: &str,
) -> Result<Option<bool>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let field_ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast()).collect();
    let field_lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
    let mut result: i32 = 0;
    let rc = unsafe {
        jx_dom_field_has(
            buf.as_ptr().cast(),
            json_len,
            field_ptrs.as_ptr(),
            field_lens.as_ptr(),
            fields.len(),
            key.as_ptr().cast(),
            key.len(),
            &mut result,
        )
    };
    if rc == -2 {
        return Ok(None);
    }
    check(rc)?;
    Ok(Some(result != 0))
}

/// Navigate prefix, iterate array, apply a builtin per element.
///
/// `op`: 0=length, 1=keys, 2=type, 3=has.
/// `sorted`: for keys op, whether to sort.
/// `arg`: for has op, the key name to check.
///
/// Returns `Ok(Some(bytes))` on success, `Ok(None)` if fallback needed.
pub fn dom_array_map_builtin(
    buf: &[u8],
    json_len: usize,
    prefix: &[&str],
    op: i32,
    sorted: bool,
    arg: &str,
    wrap_array: bool,
) -> Result<Option<Vec<u8>>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let prefix_ptrs: Vec<*const c_char> = prefix.iter().map(|f| f.as_ptr().cast()).collect();
    let prefix_lens: Vec<usize> = prefix.iter().map(|f| f.len()).collect();
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let rc = unsafe {
        jx_dom_array_map_builtin(
            buf.as_ptr().cast(),
            json_len,
            prefix_ptrs.as_ptr(),
            prefix_lens.as_ptr(),
            prefix.len(),
            op,
            if sorted { 1 } else { 0 },
            arg.as_ptr().cast(),
            arg.len(),
            if wrap_array { 1 } else { 0 },
            &mut out_ptr,
            &mut out_len,
        )
    };
    if rc == -2 {
        return Ok(None);
    }
    check(rc)?;
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(Some(result))
}

/// Navigate a prefix field chain, then iterate the target array and extract
/// a field chain from each element.
///
/// Returns `Ok(Some(bytes))` on success, `Ok(None)` if target is not an array (fallback).
/// `wrap_array`: true = output as `[v1,v2,...]`, false = output `v1\nv2\n...`.
pub fn dom_array_map_field(
    buf: &[u8],
    json_len: usize,
    prefix: &[&str],
    fields: &[&str],
    wrap_array: bool,
) -> Result<Option<Vec<u8>>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let prefix_ptrs: Vec<*const c_char> = prefix.iter().map(|f| f.as_ptr().cast()).collect();
    let prefix_lens: Vec<usize> = prefix.iter().map(|f| f.len()).collect();
    let field_ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast()).collect();
    let field_lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let rc = unsafe {
        jx_dom_array_map_field(
            buf.as_ptr().cast(),
            json_len,
            prefix_ptrs.as_ptr(),
            prefix_lens.as_ptr(),
            prefix.len(),
            field_ptrs.as_ptr(),
            field_lens.as_ptr(),
            fields.len(),
            if wrap_array { 1 } else { 0 },
            &mut out_ptr,
            &mut out_len,
        )
    };
    if rc == -2 {
        return Ok(None);
    }
    check(rc)?;
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(Some(result))
}

/// Navigate a prefix field chain, then iterate the target array and extract
/// N fields per element, emitting `{"key1":v1,"key2":v2,...}` per element.
///
/// `keys`: pre-encoded JSON key strings (e.g. `"\"user\""`) — 1:1 with `fields`.
/// `fields`: bare field names to extract from each element.
///
/// Returns `Ok(Some(bytes))` on success, `Ok(None)` if target is not an array (fallback).
/// `wrap_array`: true = output as `[{...},{...},...]`, false = output `{...}\n{...}\n...`.
pub fn dom_array_map_fields_obj(
    buf: &[u8],
    json_len: usize,
    prefix: &[&str],
    keys: &[&[u8]],
    fields: &[&str],
    wrap_array: bool,
) -> Result<Option<Vec<u8>>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    let prefix_ptrs: Vec<*const c_char> = prefix.iter().map(|f| f.as_ptr().cast()).collect();
    let prefix_lens: Vec<usize> = prefix.iter().map(|f| f.len()).collect();
    let key_ptrs: Vec<*const c_char> = keys.iter().map(|k| k.as_ptr().cast()).collect();
    let key_lens: Vec<usize> = keys.iter().map(|k| k.len()).collect();
    let field_ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast()).collect();
    let field_lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let rc = unsafe {
        jx_dom_array_map_fields_obj(
            buf.as_ptr().cast(),
            json_len,
            prefix_ptrs.as_ptr(),
            prefix_lens.as_ptr(),
            prefix.len(),
            key_ptrs.as_ptr(),
            key_lens.as_ptr(),
            field_ptrs.as_ptr(),
            field_lens.as_ptr(),
            fields.len(),
            if wrap_array { 1 } else { 0 },
            &mut out_ptr,
            &mut out_len,
        )
    };
    if rc == -2 {
        return Ok(None);
    }
    check(rc)?;
    let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
    unsafe { jx_minify_free(out_ptr) };
    Ok(Some(result))
}

/// Batch field extraction: parse once, extract N field chains.
///
/// Each entry in `field_chains` is a slice of field segments, e.g. `&["actor", "login"]`.
/// Returns a Vec of raw JSON byte results, one per chain. Missing fields produce `b"null"`.
pub fn dom_find_fields_raw(
    buf: &[u8],
    json_len: usize,
    field_chains: &[&[&str]],
) -> Result<Vec<Vec<u8>>> {
    assert!(
        buf.len() >= json_len + padding(),
        "buffer must include SIMDJSON_PADDING extra bytes"
    );
    if field_chains.is_empty() {
        return Ok(Vec::new());
    }

    // Build the triple-pointer structure: chains[i] is an array of c_char pointers
    let chain_ptrs: Vec<Vec<*const c_char>> = field_chains
        .iter()
        .map(|chain| chain.iter().map(|f| f.as_ptr().cast::<c_char>()).collect())
        .collect();
    let chain_lens: Vec<Vec<usize>> = field_chains
        .iter()
        .map(|chain| chain.iter().map(|f| f.len()).collect())
        .collect();

    let chain_ptr_ptrs: Vec<*const *const c_char> = chain_ptrs.iter().map(|v| v.as_ptr()).collect();
    let chain_len_ptrs: Vec<*const usize> = chain_lens.iter().map(|v| v.as_ptr()).collect();
    let chain_counts: Vec<usize> = field_chains.iter().map(|c| c.len()).collect();

    let mut out_ptr: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;

    // SAFETY: buf is padded (asserted). All pointer arrays are valid and match
    // field_chains dimensions. out_ptr/out_len are valid stack references.
    check(unsafe {
        jx_dom_find_fields_raw(
            buf.as_ptr().cast(),
            json_len,
            chain_ptr_ptrs.as_ptr(),
            chain_len_ptrs.as_ptr(),
            chain_counts.as_ptr(),
            field_chains.len(),
            &mut out_ptr,
            &mut out_len,
        )
    })?;

    // Unpack the length-prefixed buffer: [u32 len][bytes][u32 len][bytes]...
    let packed = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) };
    let mut results = Vec::with_capacity(field_chains.len());
    let mut offset = 0;
    for _ in 0..field_chains.len() {
        if offset + 4 > packed.len() {
            bail!("truncated batch field extraction result");
        }
        let slen = u32::from_ne_bytes(packed[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if offset + slen > packed.len() {
            bail!("truncated batch field extraction result");
        }
        results.push(packed[offset..offset + slen].to_vec());
        offset += slen;
    }

    // SAFETY: out_ptr was allocated by C++ new[] and has not been freed yet.
    unsafe { jx_minify_free(out_ptr) };
    Ok(results)
}

// ---------------------------------------------------------------------------
// Reusable DOM parser — avoids per-call parser construction in NDJSON loops.
// ---------------------------------------------------------------------------

/// A reusable simdjson DOM parser handle.
///
/// Creating a `DomParser` allocates internal simdjson buffers once.
/// Subsequent parse calls reuse those buffers, avoiding per-line allocation.
/// Each thread should have its own `DomParser` (not `Sync`).
pub struct DomParser {
    ptr: *mut JxDomParser,
}

unsafe impl Send for DomParser {}

impl DomParser {
    pub fn new() -> Result<Self> {
        let ptr = unsafe { jx_dom_parser_new() };
        if ptr.is_null() {
            bail!("failed to create DOM parser");
        }
        Ok(Self { ptr })
    }

    /// Extract a single field chain as raw JSON bytes.
    pub fn find_field_raw(
        &mut self,
        buf: &[u8],
        json_len: usize,
        fields: &[&str],
    ) -> Result<Vec<u8>> {
        assert!(
            buf.len() >= json_len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        let ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast::<c_char>()).collect();
        let lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();

        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;

        check(unsafe {
            jx_dom_find_field_raw_reuse(
                self.ptr,
                buf.as_ptr().cast(),
                json_len,
                ptrs.as_ptr(),
                lens.as_ptr(),
                fields.len(),
                &mut out_ptr,
                &mut out_len,
            )
        })?;

        let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
        unsafe { jx_minify_free(out_ptr) };
        Ok(result)
    }

    /// Batch extract N field chains as raw JSON bytes.
    pub fn find_fields_raw(
        &mut self,
        buf: &[u8],
        json_len: usize,
        field_chains: &[&[&str]],
    ) -> Result<Vec<Vec<u8>>> {
        assert!(
            buf.len() >= json_len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        if field_chains.is_empty() {
            return Ok(Vec::new());
        }

        let chain_ptrs: Vec<Vec<*const c_char>> = field_chains
            .iter()
            .map(|chain| chain.iter().map(|f| f.as_ptr().cast::<c_char>()).collect())
            .collect();
        let chain_lens: Vec<Vec<usize>> = field_chains
            .iter()
            .map(|chain| chain.iter().map(|f| f.len()).collect())
            .collect();

        let chain_ptr_ptrs: Vec<*const *const c_char> =
            chain_ptrs.iter().map(|v| v.as_ptr()).collect();
        let chain_len_ptrs: Vec<*const usize> = chain_lens.iter().map(|v| v.as_ptr()).collect();
        let chain_counts: Vec<usize> = field_chains.iter().map(|c| c.len()).collect();

        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;

        check(unsafe {
            jx_dom_find_fields_raw_reuse(
                self.ptr,
                buf.as_ptr().cast(),
                json_len,
                chain_ptr_ptrs.as_ptr(),
                chain_len_ptrs.as_ptr(),
                chain_counts.as_ptr(),
                field_chains.len(),
                &mut out_ptr,
                &mut out_len,
            )
        })?;

        // Unpack length-prefixed buffer
        let packed = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) };
        let mut results = Vec::with_capacity(field_chains.len());
        let mut offset = 0;
        for _ in 0..field_chains.len() {
            if offset + 4 > packed.len() {
                bail!("truncated batch field extraction result");
            }
            let slen = u32::from_ne_bytes(packed[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            if offset + slen > packed.len() {
                bail!("truncated batch field extraction result");
            }
            results.push(packed[offset..offset + slen].to_vec());
            offset += slen;
        }
        unsafe { jx_minify_free(out_ptr) };
        Ok(results)
    }

    /// Compute length of a field chain result.
    pub fn field_length(
        &mut self,
        buf: &[u8],
        json_len: usize,
        fields: &[&str],
    ) -> Result<Option<Vec<u8>>> {
        assert!(
            buf.len() >= json_len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        let ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast::<c_char>()).collect();
        let lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();

        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;

        let rc = unsafe {
            jx_dom_field_length_reuse(
                self.ptr,
                buf.as_ptr().cast(),
                json_len,
                ptrs.as_ptr(),
                lens.as_ptr(),
                fields.len(),
                &mut out_ptr,
                &mut out_len,
            )
        };

        if rc == -2 {
            return Ok(None); // needs full Value fallback
        }
        check(rc)?;

        let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
        unsafe { jx_minify_free(out_ptr) };
        Ok(Some(result))
    }

    /// Compute keys of a field chain result.
    /// `sorted`: true for `keys`, false for `keys_unsorted`.
    pub fn field_keys(
        &mut self,
        buf: &[u8],
        json_len: usize,
        fields: &[&str],
        sorted: bool,
    ) -> Result<Option<Vec<u8>>> {
        assert!(
            buf.len() >= json_len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        let ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast::<c_char>()).collect();
        let lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();

        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;

        let rc = unsafe {
            jx_dom_field_keys_reuse(
                self.ptr,
                buf.as_ptr().cast(),
                json_len,
                ptrs.as_ptr(),
                lens.as_ptr(),
                fields.len(),
                if sorted { 1 } else { 0 },
                &mut out_ptr,
                &mut out_len,
            )
        };

        if rc == -2 {
            return Ok(None);
        }
        check(rc)?;

        let result = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) }.to_vec();
        unsafe { jx_minify_free(out_ptr) };
        Ok(Some(result))
    }

    /// Check `has("key")` on a field chain result.
    /// Returns `Ok(Some(true/false))` on success, `Ok(None)` if not an object (fallback).
    pub fn field_has(
        &mut self,
        buf: &[u8],
        json_len: usize,
        fields: &[&str],
        key: &str,
    ) -> Result<Option<bool>> {
        assert!(
            buf.len() >= json_len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        let ptrs: Vec<*const c_char> = fields.iter().map(|f| f.as_ptr().cast::<c_char>()).collect();
        let lens: Vec<usize> = fields.iter().map(|f| f.len()).collect();
        let mut result: i32 = 0;

        let rc = unsafe {
            jx_dom_field_has_reuse(
                self.ptr,
                buf.as_ptr().cast(),
                json_len,
                ptrs.as_ptr(),
                lens.as_ptr(),
                fields.len(),
                key.as_ptr().cast(),
                key.len(),
                &mut result,
            )
        };

        if rc == -2 {
            return Ok(None);
        }
        check(rc)?;
        Ok(Some(result != 0))
    }
}

impl Drop for DomParser {
    fn drop(&mut self) {
        unsafe { jx_dom_parser_free(self.ptr) };
    }
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
            Value::Object(Arc::new(vec![]))
        );
    }

    #[test]
    fn dom_parse_empty_array() {
        let json = b"[]";
        let buf = pad_buffer(json);
        assert_eq!(
            dom_parse_to_value(&buf, json.len()).unwrap(),
            Value::Array(Arc::new(vec![]))
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
            Value::Double(d, raw) => {
                assert!((d - 9223372036854775808.0).abs() < 1.0);
                assert_eq!(raw.as_deref(), Some("9223372036854775808"));
            }
            other => panic!("expected Double, got {:?}", other),
        }
    }

    #[test]
    fn dom_parse_bigint_beyond_u64() {
        // 29-digit number — exceeds u64::MAX, previously caused BIGINT_ERROR
        let json = b"99999999999999999999999999999";
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        match val {
            Value::Double(d, raw) => {
                assert!(d > 9.9e28);
                assert_eq!(raw.as_deref(), Some("99999999999999999999999999999"));
            }
            other => panic!("expected Double with raw text, got {:?}", other),
        }
    }

    #[test]
    fn dom_parse_bigint_in_object() {
        let json = br#"{"id":99999999999999999999999999999}"#;
        let buf = pad_buffer(json);
        let val = dom_parse_to_value(&buf, json.len()).unwrap();
        match val {
            Value::Object(pairs) => {
                let (key, id_val) = &pairs[0];
                assert_eq!(key, "id");
                match id_val {
                    Value::Double(d, raw) => {
                        assert!(*d > 9.9e28);
                        assert_eq!(raw.as_deref(), Some("99999999999999999999999999999"));
                    }
                    other => panic!("expected Double, got {:?}", other),
                }
            }
            other => panic!("expected Object, got {:?}", other),
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
            Value::Object(Arc::new(vec![
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
            Value::Object(Arc::new(vec![
                (
                    "a".into(),
                    Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
                ),
                (
                    "b".into(),
                    Value::Object(Arc::new(vec![("c".into(), Value::Null)]))
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
            Value::Array(Arc::new(vec![
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
        let out = dom_field_keys(&buf, json.len(), &["data"], true)
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["a","b"]"#);
    }

    #[test]
    fn field_keys_array() {
        let json = br#"{"items":["x","y","z"]}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &["items"], true)
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "[0,1,2]");
    }

    #[test]
    fn field_keys_bare_object() {
        let json = br#"{"b":2,"a":1,"c":3}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &[], true)
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["a","b","c"]"#);
    }

    #[test]
    fn field_keys_unsorted() {
        let json = br#"{"b":2,"a":1,"c":3}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &[], false)
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["b","a","c"]"#);
    }

    #[test]
    fn field_keys_missing_unsupported() {
        let json = br#"{"name":"alice"}"#;
        let buf = pad_buffer(json);
        assert!(
            dom_field_keys(&buf, json.len(), &["missing"], true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn field_keys_string_unsupported() {
        let json = br#"{"name":"alice"}"#;
        let buf = pad_buffer(json);
        assert!(
            dom_field_keys(&buf, json.len(), &["name"], true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn field_keys_escaped_key() {
        let json = br#"{"data":{"key\"with\\escape":1}}"#;
        let buf = pad_buffer(json);
        let out = dom_field_keys(&buf, json.len(), &["data"], true)
            .unwrap()
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            r#"["key\"with\\escape"]"#
        );
    }

    // --- DomParser reuse ---

    #[test]
    fn dom_parser_new_and_drop() {
        let _dp = DomParser::new().unwrap();
        // Drop runs automatically — just ensure no panic/crash.
    }

    #[test]
    fn dom_parser_find_field_raw() {
        let mut dp = DomParser::new().unwrap();
        let json = br#"{"name":"alice","age":30}"#;
        let buf = pad_buffer(json);
        let out = dp.find_field_raw(&buf, json.len(), &["name"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#""alice""#);
    }

    #[test]
    fn dom_parser_find_fields_raw() {
        let mut dp = DomParser::new().unwrap();
        let json = br#"{"a":1,"b":"two","c":[3]}"#;
        let buf = pad_buffer(json);
        let chains: &[&[&str]] = &[&["a"], &["b"], &["c"]];
        let results = dp.find_fields_raw(&buf, json.len(), chains).unwrap();
        assert_eq!(results[0], b"1");
        assert_eq!(results[1], b"\"two\"");
        assert_eq!(results[2], b"[3]");
    }

    #[test]
    fn dom_parser_field_length() {
        let mut dp = DomParser::new().unwrap();
        let json = br#"{"items":[10,20,30]}"#;
        let buf = pad_buffer(json);
        let out = dp
            .field_length(&buf, json.len(), &["items"])
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), "3");
    }

    #[test]
    fn dom_parser_field_keys() {
        let mut dp = DomParser::new().unwrap();
        let json = br#"{"data":{"x":1,"y":2}}"#;
        let buf = pad_buffer(json);
        let out = dp
            .field_keys(&buf, json.len(), &["data"], true)
            .unwrap()
            .unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["x","y"]"#);
    }

    #[test]
    fn dom_parser_reuse_many_documents() {
        let mut dp = DomParser::new().unwrap();
        for i in 0..200 {
            let json = format!(r#"{{"val":{i}}}"#);
            let buf = pad_buffer(json.as_bytes());
            let out = dp.find_field_raw(&buf, json.len(), &["val"]).unwrap();
            assert_eq!(std::str::from_utf8(&out).unwrap(), i.to_string());
        }
    }

    #[test]
    fn dom_parser_field_length_unsupported() {
        let mut dp = DomParser::new().unwrap();
        let json = br#"{"n":42}"#;
        let buf = pad_buffer(json);
        assert!(dp.field_length(&buf, json.len(), &["n"]).unwrap().is_none());
    }

    #[test]
    fn dom_parser_field_keys_unsupported() {
        let mut dp = DomParser::new().unwrap();
        let json = br#"{"n":42}"#;
        let buf = pad_buffer(json);
        assert!(
            dp.field_keys(&buf, json.len(), &["n"], true)
                .unwrap()
                .is_none()
        );
    }

    // --- dom_parse_to_value_fast (DOM tape walk) ---

    /// Helper: assert that dom_parse_to_value_fast produces identical output
    /// to dom_parse_to_value for a given JSON input.
    fn assert_fast_matches_standard(json: &[u8]) {
        let buf = pad_buffer(json);
        let standard = dom_parse_to_value(&buf, json.len()).unwrap();
        let fast = dom_parse_to_value_fast(&buf, json.len()).unwrap();
        assert_eq!(
            standard,
            fast,
            "fast path mismatch for input: {}",
            std::str::from_utf8(json).unwrap_or("<non-utf8>")
        );
    }

    #[test]
    fn fast_parse_simple_object() {
        assert_fast_matches_standard(br#"{"name": "alice", "age": 30, "active": true}"#);
    }

    #[test]
    fn fast_parse_nested() {
        assert_fast_matches_standard(br#"{"a": [1, 2], "b": {"c": null}}"#);
    }

    #[test]
    fn fast_parse_array() {
        assert_fast_matches_standard(br#"[1, "two", 3.14, false, null]"#);
    }

    #[test]
    fn fast_parse_scalars() {
        assert_fast_matches_standard(b"42");
        assert_fast_matches_standard(b"-99");
        assert_fast_matches_standard(b"3.14");
        assert_fast_matches_standard(b"true");
        assert_fast_matches_standard(b"false");
        assert_fast_matches_standard(b"null");
        assert_fast_matches_standard(br#""hello""#);
    }

    #[test]
    fn fast_parse_empty_containers() {
        assert_fast_matches_standard(b"{}");
        assert_fast_matches_standard(b"[]");
    }

    #[test]
    fn fast_parse_escaped_strings() {
        assert_fast_matches_standard(br#"{"s": "a\"b\\c\/d\n\t\r"}"#);
    }

    #[test]
    fn fast_parse_large_integer() {
        assert_fast_matches_standard(b"9223372036854775807");
    }

    #[test]
    fn fast_parse_negative_integer() {
        assert_fast_matches_standard(b"-9223372036854775808");
    }

    #[test]
    fn fast_parse_uint64_beyond_i64() {
        assert_fast_matches_standard(b"9223372036854775808");
    }

    #[test]
    fn fast_parse_double_with_raw_text() {
        // Verify raw number text is preserved through the tape walk
        let json = b"75.80";
        let buf = pad_buffer(json);
        let val = dom_parse_to_value_fast(&buf, json.len()).unwrap();
        match val {
            Value::Double(d, raw) => {
                assert!((d - 75.8).abs() < 1e-10);
                assert_eq!(raw.as_deref(), Some("75.80"));
            }
            other => panic!("expected Double, got {:?}", other),
        }
    }

    #[test]
    fn fast_parse_scientific_notation_preserved() {
        let json = b"1e2";
        let buf = pad_buffer(json);
        let val = dom_parse_to_value_fast(&buf, json.len()).unwrap();
        match val {
            Value::Double(d, raw) => {
                assert!((d - 100.0).abs() < 1e-10);
                assert_eq!(raw.as_deref(), Some("1e2"));
            }
            other => panic!("expected Double, got {:?}", other),
        }
    }

    #[test]
    fn fast_parse_deeply_nested_array() {
        let json = b"[[[[1]]]]";
        assert_fast_matches_standard(json);
    }

    #[test]
    fn fast_parse_deeply_nested_object() {
        assert_fast_matches_standard(br#"{"a":{"b":{"c":{"d":"deep"}}}}"#);
    }

    #[test]
    fn fast_parse_mixed_whitespace() {
        assert_fast_matches_standard(b"{ \"a\" : 1 , \"b\" : [ 2 , 3 ] }");
    }

    #[test]
    fn fast_parse_bigint_fallback() {
        // Numbers beyond u64 range should fall back to On-Demand path
        let json = b"99999999999999999999999999999";
        let buf = pad_buffer(json);
        let standard = dom_parse_to_value(&buf, json.len()).unwrap();
        let fast = dom_parse_to_value_fast(&buf, json.len()).unwrap();
        assert_eq!(standard, fast);
    }

    #[test]
    fn fast_parse_bigint_in_object_fallback() {
        let json = br#"{"id":99999999999999999999999999999}"#;
        let buf = pad_buffer(json);
        let standard = dom_parse_to_value(&buf, json.len()).unwrap();
        let fast = dom_parse_to_value_fast(&buf, json.len()).unwrap();
        assert_eq!(standard, fast);
    }

    // --- flat buffer equivalence (tape walk vs on-demand) ---

    #[test]
    fn flat_buf_tape_walk_produces_same_bytes() {
        // Verify the tape walk flat buffer decodes to the same Value as
        // the On-Demand flat buffer
        let json = br#"{"name":"alice","scores":[10,20.5],"active":true,"meta":null}"#;
        let buf = pad_buffer(json);
        let from_ondemand = dom_parse_to_value(&buf, json.len()).unwrap();
        let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
        let from_tape = decode_value(flat_buf.as_bytes(), &mut 0).unwrap();
        assert_eq!(from_ondemand, from_tape);
    }

    #[test]
    fn fast_parse_error_on_invalid_json() {
        let json = b"not valid json";
        let buf = pad_buffer(json);
        assert!(dom_parse_to_value_fast(&buf, json.len()).is_err());
    }

    #[test]
    fn fast_parse_error_on_empty_input() {
        let json = b"";
        let buf = pad_buffer(json);
        assert!(dom_parse_to_value_fast(&buf, json.len()).is_err());
    }
}
