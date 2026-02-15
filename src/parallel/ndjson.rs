/// Parallel NDJSON (newline-delimited JSON) processing.
///
/// Splits NDJSON input into ~1MB chunks, processes chunks in parallel via
/// rayon, and concatenates output in order.
use anyhow::{Context, Result};
use memchr::memchr_iter;
use rayon::prelude::*;

use std::collections::HashSet;

use crate::filter::{CmpOp, Env, Filter};
use crate::output::{self, OutputConfig};
use crate::simdjson;

/// Detected fast-path strategy for NDJSON processing.
/// Field-chain patterns bypass the Value tree entirely.
#[derive(Debug)]
enum NdjsonFastPath {
    /// Normal path: parse → Value → eval → output
    None,
    /// `.field.chain` — extract raw JSON via C++ dom_find_field_raw
    FieldChain(Vec<String>),
    /// `select(.field == literal)` — compare raw bytes, output whole line or skip
    SelectEq {
        fields: Vec<String>,
        op: CmpOp,
        literal_bytes: Vec<u8>,
    },
    /// `length` or `.field | length` — compute length via C++ bridge
    Length(Vec<String>),
    /// `keys` or `.field | keys` — compute keys via C++ bridge
    Keys(Vec<String>),
    /// `select(.field == literal) | .out_field` — select then extract
    SelectEqField {
        pred_fields: Vec<String>,
        op: CmpOp,
        literal_bytes: Vec<u8>,
        out_fields: Vec<String>,
    },
    /// `{key1: .field1, key2: .field2}` — multi-field object construction (batch extraction)
    MultiFieldObj {
        /// Each entry: (pre-serialized JSON key bytes including quotes, field chain)
        entries: Vec<(Vec<u8>, Vec<String>)>,
    },
    /// `[.field1, .field2]` — multi-field array construction (batch extraction)
    MultiFieldArr { entries: Vec<Vec<String>> },
    /// `select(.f == lit) | {key: .field, ...}` — select then object construct
    SelectEqObj {
        pred_fields: Vec<String>,
        op: CmpOp,
        literal_bytes: Vec<u8>,
        entries: Vec<(Vec<u8>, Vec<String>)>,
    },
    /// `select(.f == lit) | [.field, ...]` — select then array construct
    SelectEqArr {
        pred_fields: Vec<String>,
        op: CmpOp,
        literal_bytes: Vec<u8>,
        entries: Vec<Vec<String>>,
    },
}

/// Target size for parallel chunks.
const CHUNK_TARGET_SIZE: usize = 1024 * 1024;

/// Heuristic: is this buffer NDJSON?
///
/// Checks if the first line is a complete JSON value (starts with `{`/`[`
/// and ends with `}`/`]`) and there is at least one more line starting
/// with `{`/`[`.
pub fn is_ndjson(buf: &[u8]) -> bool {
    let first_nl = match memchr::memchr(b'\n', buf) {
        Some(pos) => pos,
        None => return false,
    };

    let first_line = &buf[..first_nl];

    let first_byte = match first_line
        .iter()
        .find(|&&b| !matches!(b, b' ' | b'\t' | b'\r'))
    {
        Some(&b) => b,
        None => return false,
    };
    if first_byte != b'{' && first_byte != b'[' {
        return false;
    }

    let last_byte = match first_line
        .iter()
        .rfind(|&&b| !matches!(b, b' ' | b'\t' | b'\r'))
    {
        Some(&b) => b,
        None => return false,
    };
    if last_byte != b'}' && last_byte != b']' {
        return false;
    }

    // Must have another non-empty line starting with { or [
    let rest = &buf[first_nl + 1..];
    for &b in rest {
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            b'{' | b'[' => return true,
            _ => return false,
        }
    }
    false
}

/// Split buffer into chunks of approximately `target_size` bytes,
/// always breaking at newline boundaries.
pub fn split_chunks(buf: &[u8], target_size: usize) -> Vec<&[u8]> {
    if buf.is_empty() {
        return vec![];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < buf.len() {
        let boundary = start.saturating_add(target_size);
        if boundary >= buf.len() {
            chunks.push(&buf[start..]);
            break;
        }

        // Find newline at or after target boundary
        match memchr::memchr(b'\n', &buf[boundary..]) {
            Some(offset) => {
                let end = boundary + offset + 1;
                chunks.push(&buf[start..end]);
                start = end;
            }
            None => {
                chunks.push(&buf[start..]);
                break;
            }
        }
    }

    chunks
}

/// Process an NDJSON buffer, returning `(output_bytes, had_output)`.
///
/// Automatically parallelizes across cores for data larger than one chunk.
/// Falls back to sequential processing for small data, filters containing
/// non-thread-safe literals, or when a non-empty env is provided (Env uses
/// Rc which is not Send).
pub fn process_ndjson(
    data: &[u8],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
) -> Result<(Vec<u8>, bool)> {
    let needs_env = if env.is_empty() {
        false
    } else {
        let mut var_refs = HashSet::new();
        filter.collect_var_refs(&mut var_refs);
        var_refs.iter().any(|v| env.get_var(v).is_some())
    };
    if needs_env || !filter.is_parallel_safe() {
        return process_chunk(data, filter, config, &NdjsonFastPath::None, env);
    }

    // Detect field-chain fast path: `.field` or `.field.nested.path`
    // Bypasses Value tree entirely — extracts raw JSON via C++ bridge.
    let fast_path = detect_fast_path(filter);

    let chunks = split_chunks(data, CHUNK_TARGET_SIZE);
    if chunks.len() <= 1 {
        return process_chunk(data, filter, config, &fast_path, env);
    }

    // SAFETY: filter_is_parallel_safe() verified no Rc-containing literals,
    // so all data in the filter is immutable and thread-safe. eval() only
    // creates thread-local Values.
    let shared = SharedFilter::new(filter);

    let results: Result<Vec<(Vec<u8>, bool)>> = chunks
        .par_iter()
        .map(|&chunk| {
            let empty_env = Env::empty();
            process_chunk(chunk, shared.get(), config, &fast_path, &empty_env)
        })
        .collect();

    let results = results?;

    let total_size: usize = results.iter().map(|(buf, _)| buf.len()).sum();
    let mut out = Vec::with_capacity(total_size);
    let mut had_output = false;

    for (buf, ho) in results {
        out.extend_from_slice(&buf);
        had_output |= ho;
    }

    Ok((out, had_output))
}

fn detect_fast_path(filter: &Filter) -> NdjsonFastPath {
    // Allow disabling fast path for benchmarking A/B comparisons.
    if std::env::var_os("QJ_NO_FAST_PATH").is_some() {
        return NdjsonFastPath::None;
    }
    let mut fields = Vec::new();
    if crate::filter::collect_field_chain(filter, &mut fields) && !fields.is_empty() {
        return NdjsonFastPath::FieldChain(fields);
    }
    // Select + extract/construct (must be checked before bare select)
    if let Some(fp) = detect_select_extract_fast_path(filter) {
        return fp;
    }
    if let Some(fp) = detect_select_fast_path(filter) {
        return fp;
    }
    if let Some(fp) = detect_multi_field_fast_path(filter) {
        return fp;
    }
    if let Some(fp) = detect_length_keys_fast_path(filter) {
        return fp;
    }
    NdjsonFastPath::None
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Pointer-based wrapper to share `&Filter` across rayon threads.
///
/// Uses a raw pointer internally so `&Filter` (which is !Send because of Rc
/// in Value) never appears in the closure's captured types.
///
/// # Safety
///
/// Only safe when `filter_is_parallel_safe()` returns `true`, meaning the
/// filter tree contains no `Rc`-based `Value::Array` or `Value::Object`
/// literals. In that case, sharing the filter is safe because:
/// - The filter is only read during `eval()`
/// - Each thread creates its own `Value`s; no cross-thread sharing
/// - Scalar literal clones (`Int`, `Bool`, `String`, etc.) are thread-safe
struct SharedFilter {
    ptr: *const Filter,
}
unsafe impl Send for SharedFilter {}
unsafe impl Sync for SharedFilter {}

impl SharedFilter {
    fn new(filter: &Filter) -> Self {
        Self {
            ptr: filter as *const Filter,
        }
    }

    fn get(&self) -> &Filter {
        // SAFETY: the pointer is valid for the lifetime of the caller's
        // borrow of the original Filter (ensured by process_ndjson's scope).
        unsafe { &*self.ptr }
    }
}

/// Process a single chunk of NDJSON lines sequentially.
fn process_chunk(
    chunk: &[u8],
    filter: &Filter,
    config: &OutputConfig,
    fast_path: &NdjsonFastPath,
    env: &Env,
) -> Result<(Vec<u8>, bool)> {
    let mut output_buf = Vec::new();
    let mut had_output = false;
    // Reusable scratch buffer for simdjson padding — avoids per-line allocation.
    let mut scratch = Vec::new();

    let mut start = 0;
    for nl_pos in memchr_iter(b'\n', chunk) {
        let line = &chunk[start..nl_pos];
        start = nl_pos + 1;
        process_line(
            line,
            filter,
            config,
            fast_path,
            env,
            &mut output_buf,
            &mut had_output,
            &mut scratch,
        )?;
    }

    // Handle last line without trailing newline
    if start < chunk.len() {
        process_line(
            &chunk[start..],
            filter,
            config,
            fast_path,
            env,
            &mut output_buf,
            &mut had_output,
            &mut scratch,
        )?;
    }

    Ok((output_buf, had_output))
}

/// Unescape a JSON string interior (without surrounding quotes) into the output buffer.
/// Handles \\, \", \n, \t, \r, \b, \f, \/, and \uXXXX sequences.
fn unescape_json_string(data: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\\' && i + 1 < data.len() {
            match data[i + 1] {
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'/' => {
                    out.push(b'/');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b'f' => {
                    out.push(0x0C);
                    i += 2;
                }
                b'u' if i + 5 < data.len() => {
                    if let Ok(s) = std::str::from_utf8(&data[i + 2..i + 6])
                        && let Ok(cp) = u16::from_str_radix(s, 16)
                    {
                        // Check for surrogate pair
                        if (0xD800..=0xDBFF).contains(&cp)
                            && i + 11 < data.len()
                            && data[i + 6] == b'\\'
                            && data[i + 7] == b'u'
                            && let Ok(s2) = std::str::from_utf8(&data[i + 8..i + 12])
                            && let Ok(cp2) = u16::from_str_radix(s2, 16)
                        {
                            let full =
                                0x10000 + ((cp as u32 - 0xD800) << 10) + (cp2 as u32 - 0xDC00);
                            if let Some(c) = char::from_u32(full) {
                                let mut buf = [0u8; 4];
                                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                                i += 12;
                                continue;
                            }
                        }
                        if let Some(c) = char::from_u32(cp as u32) {
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                        i += 6;
                        continue;
                    }
                    out.push(data[i]);
                    i += 1;
                }
                _ => {
                    out.push(data[i]);
                    i += 1;
                }
            }
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
}

/// Write the line terminator (newline, NUL, or nothing) after a fast-path output.
#[inline]
fn write_line_terminator(output_buf: &mut Vec<u8>, config: &OutputConfig) {
    if config.null_separator {
        output_buf.push(0);
    } else if !config.join_output {
        output_buf.push(b'\n');
    }
}

/// Emit a raw field value — in raw mode, strip quotes and unescape; otherwise emit as-is.
#[inline]
fn emit_raw_field(output_buf: &mut Vec<u8>, raw: &[u8], config: &OutputConfig) {
    if config.mode == output::OutputMode::Raw
        && raw.len() >= 2
        && raw[0] == b'"'
        && raw[raw.len() - 1] == b'"'
    {
        let inner = &raw[1..raw.len() - 1];
        unescape_json_string(inner, output_buf);
    } else {
        output_buf.extend_from_slice(raw);
    }
}

/// Serialize a string as a JSON key with surrounding quotes.
/// E.g., `actor` → `b"\"actor\""`, `key"with` → `b"\"key\\\"with\""`.
fn json_key_bytes(key: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(key.len() + 2);
    buf.push(b'"');
    for &b in key.as_bytes() {
        match b {
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b if b < 0x20 => {
                buf.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
            }
            _ => buf.push(b),
        }
    }
    buf.push(b'"');
    buf
}

/// Prepare a reusable padded buffer for simdjson. Avoids allocation per line
/// by reusing the scratch buffer — only reallocates if the line is larger
/// than any previous one in this chunk.
fn prepare_padded<'a>(trimmed: &[u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
    let pad = simdjson::padding();
    let needed = trimmed.len() + pad;
    if scratch.len() < needed {
        scratch.resize(needed, 0);
    }
    scratch[..trimmed.len()].copy_from_slice(trimmed);
    // Zero the padding region (required by simdjson)
    scratch[trimmed.len()..trimmed.len() + pad].fill(0);
    &scratch[..needed]
}

/// Process a single NDJSON line: parse, eval filter, write output.
#[allow(clippy::too_many_arguments)]
fn process_line(
    line: &[u8],
    filter: &Filter,
    config: &OutputConfig,
    fast_path: &NdjsonFastPath,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    // Trim trailing whitespace
    let end = line
        .iter()
        .rposition(|&b| !matches!(b, b' ' | b'\t' | b'\r'))
        .map_or(0, |p| p + 1);
    let trimmed = &line[..end];

    if trimmed.is_empty() {
        return Ok(());
    }

    match fast_path {
        NdjsonFastPath::FieldChain(fields) => {
            let padded = prepare_padded(trimmed, scratch);
            let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            let raw = simdjson::dom_find_field_raw(padded, trimmed.len(), &field_refs)
                .context("failed to extract field from NDJSON line")?;
            *had_output = true;
            emit_raw_field(output_buf, &raw, config);
            write_line_terminator(output_buf, config);
        }
        NdjsonFastPath::SelectEq {
            fields,
            op,
            literal_bytes,
        } => {
            process_line_select_eq(
                trimmed,
                fields,
                *op,
                literal_bytes,
                filter,
                config,
                env,
                output_buf,
                had_output,
                scratch,
            )?;
        }
        NdjsonFastPath::Length(fields) => {
            process_line_length(
                trimmed, fields, filter, config, env, output_buf, had_output, scratch,
            )?;
        }
        NdjsonFastPath::Keys(fields) => {
            process_line_keys(
                trimmed, fields, filter, config, env, output_buf, had_output, scratch,
            )?;
        }
        NdjsonFastPath::SelectEqField {
            pred_fields,
            op,
            literal_bytes,
            out_fields,
        } => {
            process_line_select_eq_field(
                trimmed,
                pred_fields,
                *op,
                literal_bytes,
                out_fields,
                filter,
                config,
                env,
                output_buf,
                had_output,
                scratch,
            )?;
        }
        NdjsonFastPath::MultiFieldObj { entries } => {
            process_line_multi_field_obj(
                trimmed, entries, config, output_buf, had_output, scratch,
            )?;
        }
        NdjsonFastPath::MultiFieldArr { entries } => {
            process_line_multi_field_arr(
                trimmed, entries, config, output_buf, had_output, scratch,
            )?;
        }
        NdjsonFastPath::SelectEqObj {
            pred_fields,
            op,
            literal_bytes,
            entries,
        } => {
            process_line_select_eq_obj(
                trimmed,
                pred_fields,
                *op,
                literal_bytes,
                entries,
                filter,
                config,
                env,
                output_buf,
                had_output,
                scratch,
            )?;
        }
        NdjsonFastPath::SelectEqArr {
            pred_fields,
            op,
            literal_bytes,
            entries,
        } => {
            process_line_select_eq_arr(
                trimmed,
                pred_fields,
                *op,
                literal_bytes,
                entries,
                filter,
                config,
                env,
                output_buf,
                had_output,
                scratch,
            )?;
        }
        NdjsonFastPath::None => {
            // Normal path: parse → Value → eval → output
            let padded = prepare_padded(trimmed, scratch);
            let value = simdjson::dom_parse_to_value(padded, trimmed.len())
                .context("failed to parse NDJSON line")?;

            crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
                *had_output = true;
                output::write_value(output_buf, &v, config).ok();
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Fast-path detection: select(.field == literal)
// ---------------------------------------------------------------------------

fn detect_select_fast_path(filter: &Filter) -> Option<NdjsonFastPath> {
    let inner = match filter {
        Filter::Select(inner) => inner,
        _ => return None,
    };
    let (lhs, op, rhs) = match inner.as_ref() {
        Filter::Compare(lhs, op, rhs) => (lhs, op, rhs),
        _ => return None,
    };
    // Only support Eq and Ne for byte-level comparison
    if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
        return None;
    }
    // Try both orientations: (.field == lit) and (lit == .field)
    let (fields, literal_bytes) = if let Some((f, b)) = try_field_literal(lhs, rhs) {
        (f, b)
    } else if let Some((f, b)) = try_field_literal(rhs, lhs) {
        (f, b)
    } else {
        return None;
    };
    Some(NdjsonFastPath::SelectEq {
        fields,
        op: *op,
        literal_bytes,
    })
}

/// Try to decompose (field_chain_side, literal_side) into (fields, serialized_bytes).
fn try_field_literal(field_side: &Filter, lit_side: &Filter) -> Option<(Vec<String>, Vec<u8>)> {
    let mut fields = Vec::new();
    if !crate::filter::collect_field_chain(field_side, &mut fields) || fields.is_empty() {
        return None;
    }
    let literal_bytes = serialize_literal(lit_side)?;
    Some((fields, literal_bytes))
}

/// Serialize a Filter::Literal scalar to its JSON byte representation.
fn serialize_literal(filter: &Filter) -> Option<Vec<u8>> {
    use crate::value::Value;
    match filter {
        Filter::Literal(Value::String(s)) => {
            // JSON-encode: "value"
            let mut buf = Vec::with_capacity(s.len() + 2);
            buf.push(b'"');
            // Escape special characters in the string
            for &b in s.as_bytes() {
                match b {
                    b'"' => buf.extend_from_slice(b"\\\""),
                    b'\\' => buf.extend_from_slice(b"\\\\"),
                    b'\n' => buf.extend_from_slice(b"\\n"),
                    b'\r' => buf.extend_from_slice(b"\\r"),
                    b'\t' => buf.extend_from_slice(b"\\t"),
                    b if b < 0x20 => {
                        buf.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
                    }
                    _ => buf.push(b),
                }
            }
            buf.push(b'"');
            Some(buf)
        }
        Filter::Literal(Value::Int(n)) => Some(n.to_string().into_bytes()),
        Filter::Literal(Value::Double(f, _)) => {
            // Use serde_json-style float formatting
            let s = if f.fract() == 0.0 && f.is_finite() {
                // Integers stored as float: avoid trailing ".0" for exact match
                format!("{}", *f as i64)
            } else {
                format!("{}", f)
            };
            Some(s.into_bytes())
        }
        Filter::Literal(Value::Bool(b)) => Some(if *b {
            b"true".to_vec()
        } else {
            b"false".to_vec()
        }),
        Filter::Literal(Value::Null) => Some(b"null".to_vec()),
        _ => None,
    }
}

/// Classify JSON value type from its first byte.
/// Returns a tag so that different types compare as definitely unequal.
fn json_type_tag(bytes: &[u8]) -> u8 {
    match bytes.first() {
        Some(b'"') => b'"',                     // string
        Some(b't') | Some(b'f') => b'b',        // boolean
        Some(b'n') => b'n',                     // null
        Some(b'{') => b'{',                     // object
        Some(b'[') => b'[',                     // array
        Some(b'-') | Some(b'0'..=b'9') => b'0', // number
        _ => b'?',
    }
}

/// Check if a byte mismatch between raw JSON and serialized literal bytes
/// definitively means the values are not equal (no fallback needed).
///
/// Returns `true` when different bytes guarantee different values.
/// Returns `false` when the values might still be equal despite different
/// byte representations (e.g., `1.0` vs `1`, `"\u0041"` vs `"A"`).
fn bytes_mismatch_is_definitive(raw: &[u8], literal_bytes: &[u8]) -> bool {
    let raw_type = json_type_tag(raw);
    let lit_type = json_type_tag(literal_bytes);

    // Different JSON types are never equal
    if raw_type != lit_type {
        return true;
    }

    match raw_type {
        // null, booleans have exactly one byte representation
        b'n' | b'b' => true,
        // Strings: if neither side has backslash escapes, byte mismatch = value mismatch.
        // Escapes like \uXXXX can encode the same char differently.
        b'"' => {
            let raw_inner = &raw[1..raw.len().saturating_sub(1)];
            let lit_inner = &literal_bytes[1..literal_bytes.len().saturating_sub(1)];
            !raw_inner.contains(&b'\\') && !lit_inner.contains(&b'\\')
        }
        // Numbers: if both are plain integers (no '.', 'e', 'E'), byte mismatch = value mismatch.
        // Different float representations (1.0 vs 1, 1e2 vs 100) need full comparison.
        b'0' => {
            let raw_is_plain_int = !raw.iter().any(|&b| b == b'.' || b == b'e' || b == b'E');
            let lit_is_plain_int = !literal_bytes
                .iter()
                .any(|&b| b == b'.' || b == b'e' || b == b'E');
            raw_is_plain_int && lit_is_plain_int
        }
        // Objects/arrays: can't determine from bytes alone
        _ => false,
    }
}

/// Process a line with the select(.field == literal) fast path.
///
/// Uses byte-level comparison as an optimization:
/// - Bytes match → values definitely equal (fast accept)
/// - Bytes mismatch + definitive type check → values definitely unequal (fast reject)
/// - Bytes mismatch + ambiguous → fall back to full parse+eval (safe)
#[allow(clippy::too_many_arguments)]
fn process_line_select_eq(
    trimmed: &[u8],
    fields: &[String],
    op: CmpOp,
    literal_bytes: &[u8],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
    let raw = simdjson::dom_find_field_raw(padded, trimmed.len(), &field_refs)
        .context("failed to extract field from NDJSON line")?;

    if raw == literal_bytes {
        // Bytes match → values definitely equal
        if matches!(op, CmpOp::Eq) {
            *had_output = true;
            output_buf.extend_from_slice(trimmed);
            write_line_terminator(output_buf, config);
        }
    } else if bytes_mismatch_is_definitive(&raw, literal_bytes) {
        // Bytes don't match and we're certain values aren't equal
        if matches!(op, CmpOp::Ne) {
            *had_output = true;
            output_buf.extend_from_slice(trimmed);
            write_line_terminator(output_buf, config);
        }
    } else {
        // Ambiguous: bytes differ but values might be equal (e.g. 1.0 vs 1, \u0041 vs A).
        // Fall back to full parse + eval for correctness.
        let padded = prepare_padded(trimmed, scratch);
        let value = simdjson::dom_parse_to_value(padded, trimmed.len())
            .context("failed to parse NDJSON line")?;
        crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
            *had_output = true;
            output::write_value(output_buf, &v, config).ok();
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fast-path detection: length / keys
// ---------------------------------------------------------------------------

fn detect_length_keys_fast_path(filter: &Filter) -> Option<NdjsonFastPath> {
    // Bare `length` or `keys`
    if let Filter::Builtin(name, args) = filter
        && args.is_empty()
    {
        match name.as_str() {
            "length" => return Some(NdjsonFastPath::Length(vec![])),
            "keys" | "keys_unsorted" => return Some(NdjsonFastPath::Keys(vec![])),
            _ => {}
        }
    }
    // `.field | length` or `.field | keys`
    if let Some((fields, builtin)) = crate::filter::decompose_field_builtin(filter) {
        match builtin {
            "length" => return Some(NdjsonFastPath::Length(fields)),
            "keys" | "keys_unsorted" => return Some(NdjsonFastPath::Keys(fields)),
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Fast-path detection: select(.f == lit) | extract
// ---------------------------------------------------------------------------

/// Detect `select(.field == literal) | field_or_obj_or_arr` composite patterns.
fn detect_select_extract_fast_path(filter: &Filter) -> Option<NdjsonFastPath> {
    // Must be Pipe(Select(Compare(...)), rhs)
    let (lhs, rhs) = match filter {
        Filter::Pipe(lhs, rhs) => (lhs.as_ref(), rhs.as_ref()),
        _ => return None,
    };
    let select_inner = match lhs {
        Filter::Select(inner) => inner.as_ref(),
        _ => return None,
    };
    let (cmp_lhs, op, cmp_rhs) = match select_inner {
        Filter::Compare(l, op, r) => (l.as_ref(), op, r.as_ref()),
        _ => return None,
    };
    if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
        return None;
    }
    let (pred_fields, literal_bytes) = if let Some((f, b)) = try_field_literal(cmp_lhs, cmp_rhs) {
        (f, b)
    } else if let Some((f, b)) = try_field_literal(cmp_rhs, cmp_lhs) {
        (f, b)
    } else {
        return None;
    };

    // RHS: try field chain first
    let mut out_fields = Vec::new();
    if crate::filter::collect_field_chain(rhs, &mut out_fields) && !out_fields.is_empty() {
        return Some(NdjsonFastPath::SelectEqField {
            pred_fields,
            op: *op,
            literal_bytes,
            out_fields,
        });
    }

    // RHS: try object construct
    if let Some(entries) = try_multi_field_obj(rhs) {
        return Some(NdjsonFastPath::SelectEqObj {
            pred_fields,
            op: *op,
            literal_bytes,
            entries,
        });
    }

    // RHS: try array construct
    if let Some(entries) = try_multi_field_arr(rhs) {
        return Some(NdjsonFastPath::SelectEqArr {
            pred_fields,
            op: *op,
            literal_bytes,
            entries,
        });
    }

    None
}

fn detect_multi_field_fast_path(filter: &Filter) -> Option<NdjsonFastPath> {
    if let Some(entries) = try_multi_field_obj(filter) {
        return Some(NdjsonFastPath::MultiFieldObj { entries });
    }
    if let Some(entries) = try_multi_field_arr(filter) {
        return Some(NdjsonFastPath::MultiFieldArr { entries });
    }
    None
}

/// Try to decompose an ObjectConstruct into (json_key_bytes, field_chain) pairs.
/// Returns None if any key is an Expr or any value is not a field chain.
fn try_multi_field_obj(filter: &Filter) -> Option<Vec<(Vec<u8>, Vec<String>)>> {
    let pairs = match filter {
        Filter::ObjectConstruct(pairs) => pairs,
        _ => return None,
    };
    if pairs.is_empty() {
        return None;
    }
    let mut entries = Vec::with_capacity(pairs.len());
    for (key, val_filter) in pairs {
        let key_name = match key {
            crate::filter::ObjKey::Name(s) => s,
            crate::filter::ObjKey::Expr(_) => return None,
        };
        let mut fields = Vec::new();
        if !crate::filter::collect_field_chain(val_filter, &mut fields) || fields.is_empty() {
            return None;
        }
        entries.push((json_key_bytes(key_name), fields));
    }
    Some(entries)
}

/// Try to decompose an ArrayConstruct(Comma([field_chains...])) into field chain entries.
fn try_multi_field_arr(filter: &Filter) -> Option<Vec<Vec<String>>> {
    let inner = match filter {
        Filter::ArrayConstruct(inner) => inner.as_ref(),
        _ => return None,
    };
    let items = match inner {
        Filter::Comma(items) => items.as_slice(),
        // Single field chain in array: [.field]
        other => {
            let mut fields = Vec::new();
            if crate::filter::collect_field_chain(other, &mut fields) && !fields.is_empty() {
                return Some(vec![fields]);
            }
            return None;
        }
    };
    if items.is_empty() {
        return None;
    }
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let mut fields = Vec::new();
        if !crate::filter::collect_field_chain(item, &mut fields) || fields.is_empty() {
            return None;
        }
        entries.push(fields);
    }
    Some(entries)
}

/// Process a line with the `length` fast path.
#[allow(clippy::too_many_arguments)]
fn process_line_length(
    trimmed: &[u8],
    fields: &[String],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
    match simdjson::dom_field_length(padded, trimmed.len(), &field_refs)? {
        Some(result) => {
            *had_output = true;
            output_buf.extend_from_slice(&result);
            write_line_terminator(output_buf, config);
        }
        None => {
            // Fallback: unsupported type (e.g. string length) — use normal path
            let padded = prepare_padded(trimmed, scratch);
            let value = simdjson::dom_parse_to_value(padded, trimmed.len())
                .context("failed to parse NDJSON line")?;
            crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
                *had_output = true;
                output::write_value(output_buf, &v, config).ok();
            });
        }
    }
    Ok(())
}

/// Process a line with the `keys` fast path.
#[allow(clippy::too_many_arguments)]
fn process_line_keys(
    trimmed: &[u8],
    fields: &[String],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
    match simdjson::dom_field_keys(padded, trimmed.len(), &field_refs)? {
        Some(result) => {
            *had_output = true;
            output_buf.extend_from_slice(&result);
            write_line_terminator(output_buf, config);
        }
        None => {
            // Fallback: unsupported type — use normal path
            let padded = prepare_padded(trimmed, scratch);
            let value = simdjson::dom_parse_to_value(padded, trimmed.len())
                .context("failed to parse NDJSON line")?;
            crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
                *had_output = true;
                output::write_value(output_buf, &v, config).ok();
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Processing: select(.f == lit) | .field / {obj} / [arr]
// ---------------------------------------------------------------------------

/// Process a line with the `select(.f == lit) | .field` fast path.
#[allow(clippy::too_many_arguments)]
fn process_line_select_eq_field(
    trimmed: &[u8],
    pred_fields: &[String],
    op: CmpOp,
    literal_bytes: &[u8],
    out_fields: &[String],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let pred_refs: Vec<&str> = pred_fields.iter().map(|s| s.as_str()).collect();
    let raw_pred = simdjson::dom_find_field_raw(padded, trimmed.len(), &pred_refs)
        .context("failed to extract predicate field from NDJSON line")?;

    if raw_pred == literal_bytes {
        // Bytes match → values definitely equal
        if matches!(op, CmpOp::Eq) {
            let padded = prepare_padded(trimmed, scratch);
            let out_refs: Vec<&str> = out_fields.iter().map(|s| s.as_str()).collect();
            let raw_out = simdjson::dom_find_field_raw(padded, trimmed.len(), &out_refs)
                .context("failed to extract output field from NDJSON line")?;
            *had_output = true;
            emit_raw_field(output_buf, &raw_out, config);
            write_line_terminator(output_buf, config);
        }
    } else if bytes_mismatch_is_definitive(&raw_pred, literal_bytes) {
        // Bytes don't match and we're certain values aren't equal
        if matches!(op, CmpOp::Ne) {
            let padded = prepare_padded(trimmed, scratch);
            let out_refs: Vec<&str> = out_fields.iter().map(|s| s.as_str()).collect();
            let raw_out = simdjson::dom_find_field_raw(padded, trimmed.len(), &out_refs)
                .context("failed to extract output field from NDJSON line")?;
            *had_output = true;
            emit_raw_field(output_buf, &raw_out, config);
            write_line_terminator(output_buf, config);
        }
    } else {
        // Ambiguous: fall back to full parse+eval
        let padded = prepare_padded(trimmed, scratch);
        let value = simdjson::dom_parse_to_value(padded, trimmed.len())
            .context("failed to parse NDJSON line")?;
        crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
            *had_output = true;
            output::write_value(output_buf, &v, config).ok();
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Processing: multi-field object/array construction
// ---------------------------------------------------------------------------

/// Process a line with the `{key1: .field1, key2: .field2}` fast path (batch extraction).
fn process_line_multi_field_obj(
    trimmed: &[u8],
    entries: &[(Vec<u8>, Vec<String>)],
    config: &OutputConfig,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let field_chains: Vec<Vec<&str>> = entries
        .iter()
        .map(|(_, fields)| fields.iter().map(|s| s.as_str()).collect())
        .collect();
    let chain_refs: Vec<&[&str]> = field_chains.iter().map(|v| v.as_slice()).collect();
    let raw_values = simdjson::dom_find_fields_raw(padded, trimmed.len(), &chain_refs)
        .context("failed to batch-extract fields for object construction")?;

    output_buf.push(b'{');
    for (i, (key_bytes, _)) in entries.iter().enumerate() {
        if i > 0 {
            output_buf.push(b',');
        }
        output_buf.extend_from_slice(key_bytes);
        output_buf.push(b':');
        output_buf.extend_from_slice(&raw_values[i]);
    }
    output_buf.push(b'}');
    *had_output = true;
    write_line_terminator(output_buf, config);
    Ok(())
}

/// Process a line with the `[.field1, .field2]` fast path (batch extraction).
fn process_line_multi_field_arr(
    trimmed: &[u8],
    entries: &[Vec<String>],
    config: &OutputConfig,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let field_chains: Vec<Vec<&str>> = entries
        .iter()
        .map(|fields| fields.iter().map(|s| s.as_str()).collect())
        .collect();
    let chain_refs: Vec<&[&str]> = field_chains.iter().map(|v| v.as_slice()).collect();
    let raw_values = simdjson::dom_find_fields_raw(padded, trimmed.len(), &chain_refs)
        .context("failed to batch-extract fields for array construction")?;

    output_buf.push(b'[');
    for (i, _) in entries.iter().enumerate() {
        if i > 0 {
            output_buf.push(b',');
        }
        output_buf.extend_from_slice(&raw_values[i]);
    }
    output_buf.push(b']');
    *had_output = true;
    write_line_terminator(output_buf, config);
    Ok(())
}

/// Process a line with the `select(.f == lit) | {key: .field, ...}` fast path.
#[allow(clippy::too_many_arguments)]
fn process_line_select_eq_obj(
    trimmed: &[u8],
    pred_fields: &[String],
    op: CmpOp,
    literal_bytes: &[u8],
    entries: &[(Vec<u8>, Vec<String>)],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let pred_refs: Vec<&str> = pred_fields.iter().map(|s| s.as_str()).collect();
    let raw_pred = simdjson::dom_find_field_raw(padded, trimmed.len(), &pred_refs)
        .context("failed to extract predicate field from NDJSON line")?;

    let should_output = if raw_pred == literal_bytes {
        matches!(op, CmpOp::Eq)
    } else if bytes_mismatch_is_definitive(&raw_pred, literal_bytes) {
        matches!(op, CmpOp::Ne)
    } else {
        // Ambiguous: fall back to full parse+eval
        let padded = prepare_padded(trimmed, scratch);
        let value = simdjson::dom_parse_to_value(padded, trimmed.len())
            .context("failed to parse NDJSON line")?;
        crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
            *had_output = true;
            output::write_value(output_buf, &v, config).ok();
        });
        return Ok(());
    };

    if should_output {
        let padded = prepare_padded(trimmed, scratch);
        let field_chains: Vec<Vec<&str>> = entries
            .iter()
            .map(|(_, fields)| fields.iter().map(|s| s.as_str()).collect())
            .collect();
        let chain_refs: Vec<&[&str]> = field_chains.iter().map(|v| v.as_slice()).collect();
        let raw_values = simdjson::dom_find_fields_raw(padded, trimmed.len(), &chain_refs)
            .context("failed to batch-extract fields for select+obj")?;

        output_buf.push(b'{');
        for (i, (key_bytes, _)) in entries.iter().enumerate() {
            if i > 0 {
                output_buf.push(b',');
            }
            output_buf.extend_from_slice(key_bytes);
            output_buf.push(b':');
            output_buf.extend_from_slice(&raw_values[i]);
        }
        output_buf.push(b'}');
        *had_output = true;
        write_line_terminator(output_buf, config);
    }
    Ok(())
}

/// Process a line with the `select(.f == lit) | [.field, ...]` fast path.
#[allow(clippy::too_many_arguments)]
fn process_line_select_eq_arr(
    trimmed: &[u8],
    pred_fields: &[String],
    op: CmpOp,
    literal_bytes: &[u8],
    entries: &[Vec<String>],
    filter: &Filter,
    config: &OutputConfig,
    env: &Env,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    let padded = prepare_padded(trimmed, scratch);
    let pred_refs: Vec<&str> = pred_fields.iter().map(|s| s.as_str()).collect();
    let raw_pred = simdjson::dom_find_field_raw(padded, trimmed.len(), &pred_refs)
        .context("failed to extract predicate field from NDJSON line")?;

    let should_output = if raw_pred == literal_bytes {
        matches!(op, CmpOp::Eq)
    } else if bytes_mismatch_is_definitive(&raw_pred, literal_bytes) {
        matches!(op, CmpOp::Ne)
    } else {
        let padded = prepare_padded(trimmed, scratch);
        let value = simdjson::dom_parse_to_value(padded, trimmed.len())
            .context("failed to parse NDJSON line")?;
        crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
            *had_output = true;
            output::write_value(output_buf, &v, config).ok();
        });
        return Ok(());
    };

    if should_output {
        let padded = prepare_padded(trimmed, scratch);
        let field_chains: Vec<Vec<&str>> = entries
            .iter()
            .map(|fields| fields.iter().map(|s| s.as_str()).collect())
            .collect();
        let chain_refs: Vec<&[&str]> = field_chains.iter().map(|v| v.as_slice()).collect();
        let raw_values = simdjson::dom_find_fields_raw(padded, trimmed.len(), &chain_refs)
            .context("failed to batch-extract fields for select+arr")?;

        output_buf.push(b'[');
        for (i, _) in entries.iter().enumerate() {
            if i > 0 {
                output_buf.push(b',');
            }
            output_buf.extend_from_slice(&raw_values[i]);
        }
        output_buf.push(b']');
        *had_output = true;
        write_line_terminator(output_buf, config);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_ndjson_objects() {
        assert!(is_ndjson(b"{\"a\":1}\n{\"b\":2}\n"));
        assert!(is_ndjson(b"{\"a\":1}\n{\"b\":2}"));
    }

    #[test]
    fn detect_ndjson_arrays() {
        assert!(is_ndjson(b"[1,2]\n[3,4]\n"));
    }

    #[test]
    fn not_ndjson_single_object() {
        assert!(!is_ndjson(b"{\"a\":1}\n"));
    }

    #[test]
    fn not_ndjson_pretty_printed() {
        assert!(!is_ndjson(b"{\n  \"a\": 1\n}\n"));
    }

    #[test]
    fn not_ndjson_single_line() {
        assert!(!is_ndjson(b"{\"a\":1}"));
    }

    #[test]
    fn not_ndjson_empty() {
        assert!(!is_ndjson(b""));
    }

    #[test]
    fn split_chunks_basic() {
        let data = b"line1\nline2\nline3\n";
        let chunks = split_chunks(data, 6);
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, data.len());
        // All chunks except possibly the last end with newline
        for (i, chunk) in chunks.iter().enumerate() {
            if i < chunks.len() - 1 {
                assert!(chunk.ends_with(b"\n"));
            }
        }
    }

    #[test]
    fn split_chunks_single() {
        let data = b"line1\n";
        let chunks = split_chunks(data, 1024 * 1024);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], data);
    }

    #[test]
    fn split_chunks_empty() {
        assert!(split_chunks(b"", 1024).is_empty());
    }

    #[test]
    fn split_chunks_huge_target_size() {
        // target_size larger than buf — should return one chunk without overflow
        let data = b"line1\nline2\n";
        let chunks = split_chunks(data, usize::MAX);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], &data[..]);
    }

    #[test]
    fn process_ndjson_basic() {
        let data = b"{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n";
        let filter = crate::filter::parse(".name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(String::from_utf8(output).unwrap(), "\"alice\"\n\"bob\"\n");
    }

    #[test]
    fn process_ndjson_identity() {
        let data = b"{\"a\":1}\n{\"b\":2}\n";
        let filter = crate::filter::parse(".").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn process_ndjson_empty_lines() {
        let data = b"{\"a\":1}\n\n{\"b\":2}\n\n";
        let filter = crate::filter::parse(".").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
    }

    // --- Field-chain fast path tests ---

    #[test]
    fn fast_path_detects_field_chain() {
        let filter = crate::filter::parse(".name").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::FieldChain(fields) => assert_eq!(fields, vec!["name"]),
            other => panic!("expected FieldChain, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_nested_field_chain() {
        let filter = crate::filter::parse(".actor.login").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::FieldChain(fields) => assert_eq!(fields, vec!["actor", "login"]),
            other => panic!("expected FieldChain, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_not_identity() {
        let filter = crate::filter::parse(".").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    #[test]
    fn fast_path_not_complex_filter() {
        let filter = crate::filter::parse(".[] | .name").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    #[test]
    fn fast_path_field_extraction_string() {
        let data = b"{\"type\":\"PushEvent\"}\n{\"type\":\"WatchEvent\"}\n";
        let filter = crate::filter::parse(".type").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\"PushEvent\"\n\"WatchEvent\"\n"
        );
    }

    #[test]
    fn fast_path_field_extraction_number() {
        let data = b"{\"count\":42}\n{\"count\":7}\n";
        let filter = crate::filter::parse(".count").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "42\n7\n");
    }

    #[test]
    fn fast_path_field_extraction_nested() {
        let data = b"{\"a\":{\"b\":\"deep\"}}\n{\"a\":{\"b\":\"val\"}}\n";
        let filter = crate::filter::parse(".a.b").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "\"deep\"\n\"val\"\n");
    }

    #[test]
    fn fast_path_missing_field_returns_null() {
        let data = b"{\"name\":\"alice\"}\n{\"age\":30}\n";
        let filter = crate::filter::parse(".name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "\"alice\"\nnull\n");
    }

    #[test]
    fn fast_path_raw_output_unquotes_strings() {
        let data = b"{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n";
        let filter = crate::filter::parse(".name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Raw,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "alice\nbob\n");
    }

    #[test]
    fn fast_path_raw_output_non_string_passes_through() {
        let data = b"{\"count\":42}\n{\"active\":true}\n";
        let filter = crate::filter::parse(".count").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Raw,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        // First line is number (no quotes to strip), second is null (missing field)
        assert_eq!(String::from_utf8(output).unwrap(), "42\nnull\n");
    }

    // --- unescape_json_string tests ---

    #[test]
    fn unescape_basic() {
        let mut out = Vec::new();
        unescape_json_string(b"hello world", &mut out);
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn unescape_backslash_sequences() {
        let mut out = Vec::new();
        unescape_json_string(br#"line1\nline2\ttab\\back\"quote"#, &mut out);
        assert_eq!(out, b"line1\nline2\ttab\\back\"quote");
    }

    #[test]
    fn unescape_unicode() {
        let mut out = Vec::new();
        unescape_json_string(br#"\u0048\u0065\u006C\u006C\u006F"#, &mut out);
        assert_eq!(out, b"Hello");
    }

    #[test]
    fn unescape_surrogate_pair() {
        let mut out = Vec::new();
        // U+1F30D = Earth Globe Europe-Africa (surrogate pair: D83C DF0D)
        unescape_json_string(br#"\uD83C\uDF0D"#, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "\u{1F30D}");
    }

    // --- Select fast-path detection tests ---

    #[test]
    fn fast_path_detects_select_eq_string() {
        let filter = crate::filter::parse("select(.type == \"PushEvent\")").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEq {
                fields,
                op,
                literal_bytes,
            } => {
                assert_eq!(fields, vec!["type"]);
                assert_eq!(op, CmpOp::Eq);
                assert_eq!(literal_bytes, b"\"PushEvent\"");
            }
            other => panic!("expected SelectEq, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_eq_int() {
        let filter = crate::filter::parse("select(.count == 42)").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEq {
                fields,
                op,
                literal_bytes,
            } => {
                assert_eq!(fields, vec!["count"]);
                assert_eq!(op, CmpOp::Eq);
                assert_eq!(literal_bytes, b"42");
            }
            other => panic!("expected SelectEq, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_eq_bool() {
        let filter = crate::filter::parse("select(.active == true)").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEq {
                fields,
                op,
                literal_bytes,
            } => {
                assert_eq!(fields, vec!["active"]);
                assert_eq!(op, CmpOp::Eq);
                assert_eq!(literal_bytes, b"true");
            }
            other => panic!("expected SelectEq, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_eq_null() {
        let filter = crate::filter::parse("select(.x == null)").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEq {
                fields,
                op,
                literal_bytes,
            } => {
                assert_eq!(fields, vec!["x"]);
                assert_eq!(op, CmpOp::Eq);
                assert_eq!(literal_bytes, b"null");
            }
            other => panic!("expected SelectEq, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_ne() {
        let filter = crate::filter::parse("select(.type != \"PushEvent\")").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEq {
                fields,
                op,
                literal_bytes,
            } => {
                assert_eq!(fields, vec!["type"]);
                assert_eq!(op, CmpOp::Ne);
                assert_eq!(literal_bytes, b"\"PushEvent\"");
            }
            other => panic!("expected SelectEq, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_reversed_operands() {
        let filter = crate::filter::parse("select(\"PushEvent\" == .type)").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEq {
                fields,
                op,
                literal_bytes,
            } => {
                assert_eq!(fields, vec!["type"]);
                assert_eq!(op, CmpOp::Eq);
                assert_eq!(literal_bytes, b"\"PushEvent\"");
            }
            other => panic!("expected SelectEq, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_select_gt_not_supported() {
        let filter = crate::filter::parse("select(.count > 10)").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    #[test]
    fn fast_path_select_no_literal_not_supported() {
        let filter = crate::filter::parse("select(.a == .b)").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    #[test]
    fn fast_path_select_eq_matching_line() {
        let data = b"{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"type\":\"PushEvent\",\"id\":1}\n"
        );
    }

    #[test]
    fn fast_path_select_ne_matching_line() {
        let data = b"{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
        let filter = crate::filter::parse("select(.type != \"PushEvent\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"type\":\"WatchEvent\",\"id\":2}\n"
        );
    }

    #[test]
    fn fast_path_select_eq_missing_field() {
        // Missing field returns null — select(.x == null) should match
        let data = b"{\"a\":1}\n{\"x\":null}\n";
        let filter = crate::filter::parse("select(.x == null)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"a\":1}\n{\"x\":null}\n"
        );
    }

    // --- Length/keys fast-path detection tests ---

    #[test]
    fn fast_path_detects_bare_length() {
        let filter = crate::filter::parse("length").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::Length(fields) => assert!(fields.is_empty()),
            other => panic!("expected Length, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_bare_keys() {
        let filter = crate::filter::parse("keys").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::Keys(fields) => assert!(fields.is_empty()),
            other => panic!("expected Keys, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_field_length() {
        let filter = crate::filter::parse(".items | length").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::Length(fields) => assert_eq!(fields, vec!["items"]),
            other => panic!("expected Length, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_field_keys() {
        let filter = crate::filter::parse(".data | keys").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::Keys(fields) => assert_eq!(fields, vec!["data"]),
            other => panic!("expected Keys, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_length_on_objects() {
        let data = b"{\"a\":1,\"b\":2}\n{\"x\":1}\n";
        let filter = crate::filter::parse("length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(String::from_utf8(output).unwrap(), "2\n1\n");
    }

    #[test]
    fn fast_path_length_on_arrays() {
        let data = b"{\"items\":[1,2,3]}\n{\"items\":[4,5]}\n";
        let filter = crate::filter::parse(".items | length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "3\n2\n");
    }

    #[test]
    fn fast_path_keys_on_objects() {
        let data = b"{\"b\":2,\"a\":1}\n{\"x\":1}\n";
        let filter = crate::filter::parse("keys").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[\"a\",\"b\"]\n[\"x\"]\n"
        );
    }

    // --- Select fast-path edge cases ---

    #[test]
    fn fast_path_select_no_match_no_output() {
        // No lines match — should produce no output, had_output = false
        let data = b"{\"type\":\"WatchEvent\"}\n{\"type\":\"IssuesEvent\"}\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(!had_output);
        assert!(output.is_empty());
    }

    #[test]
    fn fast_path_select_all_match() {
        let data = b"{\"type\":\"PushEvent\"}\n{\"type\":\"PushEvent\"}\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"type\":\"PushEvent\"}\n{\"type\":\"PushEvent\"}\n"
        );
    }

    #[test]
    fn fast_path_select_empty_string_literal() {
        let data = b"{\"name\":\"\"}\n{\"name\":\"bob\"}\n";
        let filter = crate::filter::parse("select(.name == \"\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"name\":\"\"}\n");
    }

    #[test]
    fn fast_path_select_nested_field() {
        let data = b"{\"a\":{\"b\":\"yes\"},\"id\":1}\n{\"a\":{\"b\":\"no\"},\"id\":2}\n";
        let filter = crate::filter::parse("select(.a.b == \"yes\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"a\":{\"b\":\"yes\"},\"id\":1}\n"
        );
    }

    #[test]
    fn fast_path_select_with_empty_lines() {
        let data = b"{\"type\":\"PushEvent\"}\n\n{\"type\":\"WatchEvent\"}\n\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"type\":\"PushEvent\"}\n"
        );
    }

    #[test]
    fn fast_path_select_false_literal() {
        let data = b"{\"active\":false}\n{\"active\":true}\n";
        let filter = crate::filter::parse("select(.active == false)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"active\":false}\n");
    }

    #[test]
    fn fast_path_select_int_zero() {
        let data = b"{\"n\":0}\n{\"n\":1}\n";
        let filter = crate::filter::parse("select(.n == 0)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"n\":0}\n");
    }

    #[test]
    fn fast_path_select_negative_int() {
        let data = b"{\"n\":-1}\n{\"n\":1}\n";
        let filter = crate::filter::parse("select(.n == -1)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"n\":-1}\n");
    }

    // --- Length/keys fast-path edge cases ---

    #[test]
    fn fast_path_length_empty_object() {
        let data = b"{}\n{\"a\":1}\n";
        let filter = crate::filter::parse("length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "0\n1\n");
    }

    #[test]
    fn fast_path_length_empty_array_field() {
        let data = b"{\"items\":[]}\n{\"items\":[1]}\n";
        let filter = crate::filter::parse(".items | length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "0\n1\n");
    }

    #[test]
    fn fast_path_length_string_fallback() {
        // String length requires fallback to normal path
        let data = b"{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n";
        let filter = crate::filter::parse(".name | length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "5\n3\n");
    }

    #[test]
    fn fast_path_keys_empty_object() {
        let data = b"{}\n{\"a\":1}\n";
        let filter = crate::filter::parse("keys").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "[]\n[\"a\"]\n");
    }

    #[test]
    fn fast_path_keys_array_fallback() {
        // Array keys produces indices — requires fallback
        let data = b"[10,20,30]\n[40]\n";
        let filter = crate::filter::parse("keys").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "[0,1,2]\n[0]\n");
    }

    #[test]
    fn fast_path_length_with_empty_lines() {
        let data = b"{\"a\":1}\n\n{\"b\":2,\"c\":3}\n";
        let filter = crate::filter::parse("length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "1\n2\n");
    }

    #[test]
    fn fast_path_nested_field_length() {
        let data = b"{\"a\":{\"b\":[1,2,3]}}\n{\"a\":{\"b\":[4]}}\n";
        let filter = crate::filter::parse(".a.b | length").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "3\n1\n");
    }

    #[test]
    fn fast_path_nested_field_keys() {
        let data = b"{\"meta\":{\"b\":2,\"a\":1}}\n{\"meta\":{\"z\":1}}\n";
        let filter = crate::filter::parse(".meta | keys").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[\"a\",\"b\"]\n[\"z\"]\n"
        );
    }

    // --- bytes_mismatch_is_definitive unit tests ---

    #[test]
    fn definitive_different_types() {
        // string vs number
        assert!(bytes_mismatch_is_definitive(b"\"hello\"", b"42"));
        // string vs null
        assert!(bytes_mismatch_is_definitive(b"\"hello\"", b"null"));
        // number vs bool
        assert!(bytes_mismatch_is_definitive(b"42", b"true"));
        // null vs string
        assert!(bytes_mismatch_is_definitive(b"null", b"\"x\""));
        // number vs string
        assert!(bytes_mismatch_is_definitive(b"1", b"\"1\""));
    }

    #[test]
    fn definitive_null_and_bools() {
        // null only has one representation
        assert!(bytes_mismatch_is_definitive(b"null", b"null ")); // won't happen, but tests the logic
        // bools only have one representation each
        assert!(bytes_mismatch_is_definitive(b"true", b"false"));
        assert!(bytes_mismatch_is_definitive(b"false", b"true"));
    }

    #[test]
    fn definitive_plain_strings() {
        // Plain strings without escapes: mismatch is definitive
        assert!(bytes_mismatch_is_definitive(b"\"hello\"", b"\"world\""));
        assert!(bytes_mismatch_is_definitive(b"\"abc\"", b"\"ab\""));
        assert!(bytes_mismatch_is_definitive(b"\"\"", b"\"x\""));
    }

    #[test]
    fn not_definitive_strings_with_escapes() {
        // \u0041 vs A — same string, different bytes
        assert!(!bytes_mismatch_is_definitive(b"\"\\u0041\"", b"\"A\""));
        // Raw has escape
        assert!(!bytes_mismatch_is_definitive(
            b"\"caf\\u00e9\"",
            b"\"cafe\""
        ));
    }

    #[test]
    fn definitive_plain_integers() {
        // Both plain integers: mismatch is definitive
        assert!(bytes_mismatch_is_definitive(b"42", b"43"));
        assert!(bytes_mismatch_is_definitive(b"-1", b"1"));
        assert!(bytes_mismatch_is_definitive(b"0", b"1"));
    }

    #[test]
    fn not_definitive_float_vs_int() {
        // 1.0 vs 1 — might be equal
        assert!(!bytes_mismatch_is_definitive(b"1.0", b"1"));
        // 1e2 vs 100 — might be equal
        assert!(!bytes_mismatch_is_definitive(b"1e2", b"100"));
        // 1E2 vs 100
        assert!(!bytes_mismatch_is_definitive(b"1E2", b"100"));
        // 42.0 vs 42
        assert!(!bytes_mismatch_is_definitive(b"42.0", b"42"));
    }

    // --- Select fast-path correctness with fallback ---

    #[test]
    fn fast_path_select_float_vs_int_eq() {
        // 1.0 == 1 should match (like jq)
        let data = b"{\"n\":1.0,\"id\":\"a\"}\n{\"n\":2,\"id\":\"b\"}\n";
        let filter = crate::filter::parse("select(.n == 1)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"n\":1.0,\"id\":\"a\"}\n"
        );
    }

    #[test]
    fn fast_path_select_float_vs_int_ne() {
        // 1.0 != 1 should NOT match (they're equal)
        let data = b"{\"n\":1.0}\n{\"n\":2}\n";
        let filter = crate::filter::parse("select(.n != 1)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"n\":2}\n");
    }

    #[test]
    fn fast_path_select_scientific_notation() {
        // 1e2 == 100 should match
        let data = b"{\"n\":1e2,\"id\":\"a\"}\n{\"n\":99,\"id\":\"b\"}\n";
        let filter = crate::filter::parse("select(.n == 100)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"n\":1e2,\"id\":\"a\"}\n"
        );
    }

    #[test]
    fn fast_path_select_unicode_escape_match() {
        // \u0041 is "A" — should match select(.s == "A")
        let data = b"{\"s\":\"\\u0041\",\"id\":1}\n{\"s\":\"B\",\"id\":2}\n";
        let filter = crate::filter::parse("select(.s == \"A\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"s\":\"\\u0041\",\"id\":1}\n"
        );
    }

    #[test]
    fn fast_path_select_type_mismatch_no_fallback() {
        // Field is string "42", literal is int 42 — different types, definitive mismatch
        let data = b"{\"n\":\"42\"}\n{\"n\":42}\n";
        let filter = crate::filter::parse("select(.n == 42)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"n\":42}\n");
    }

    #[test]
    fn fast_path_select_missing_field_vs_string() {
        // Missing field returns null, comparing with string "x" — definitive mismatch
        let data = b"{\"a\":1}\n{\"x\":\"hello\"}\n";
        let filter = crate::filter::parse("select(.x == \"hello\")").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"x\":\"hello\"}\n");
    }

    #[test]
    fn fast_path_select_trailing_zero_float() {
        // 42.00 == 42 should match
        let data = b"{\"n\":42.00}\n{\"n\":43}\n";
        let filter = crate::filter::parse("select(.n == 42)").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"n\":42.00}\n");
    }

    // --- Scratch buffer reuse test ---

    #[test]
    fn prepare_padded_reuses_buffer() {
        let mut scratch = Vec::new();
        let line1 = b"short";
        let padded1 = prepare_padded(line1, &mut scratch);
        assert!(padded1.len() >= line1.len() + crate::simdjson::padding());

        let line2 = b"a much longer line that should not cause reallocation if scratch is big enough already";
        let padded2 = prepare_padded(line2, &mut scratch);
        assert!(padded2.len() >= line2.len() + crate::simdjson::padding());
        assert_eq!(&padded2[..line2.len()], line2);
    }

    // --- SelectEqField detection tests ---

    #[test]
    fn fast_path_detects_select_eq_field() {
        let filter = crate::filter::parse("select(.type == \"PushEvent\") | .actor.login").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEqField {
                pred_fields,
                op,
                literal_bytes,
                out_fields,
            } => {
                assert_eq!(pred_fields, vec!["type"]);
                assert_eq!(op, CmpOp::Eq);
                assert_eq!(literal_bytes, b"\"PushEvent\"");
                assert_eq!(out_fields, vec!["actor", "login"]);
            }
            other => panic!("expected SelectEqField, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_ne_field() {
        let filter = crate::filter::parse("select(.type != \"PushEvent\") | .name").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEqField { op, .. } => assert_eq!(op, CmpOp::Ne),
            other => panic!("expected SelectEqField, got {:?}", other),
        }
    }

    // --- MultiFieldObj / MultiFieldArr detection tests ---

    #[test]
    fn fast_path_detects_multi_field_obj() {
        let filter = crate::filter::parse("{type: .type, actor: .actor.login}").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::MultiFieldObj { entries } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].0, b"\"type\"");
                assert_eq!(entries[0].1, vec!["type"]);
                assert_eq!(entries[1].0, b"\"actor\"");
                assert_eq!(entries[1].1, vec!["actor", "login"]);
            }
            other => panic!("expected MultiFieldObj, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_multi_field_obj_shorthand() {
        let filter = crate::filter::parse("{type, name}").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::MultiFieldObj { entries } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].1, vec!["type"]);
                assert_eq!(entries[1].1, vec!["name"]);
            }
            other => panic!("expected MultiFieldObj, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_not_obj_with_expr_key() {
        let filter = crate::filter::parse("{(.key): .value}").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    #[test]
    fn fast_path_not_obj_with_complex_value() {
        let filter = crate::filter::parse("{total: (.x + .y)}").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    #[test]
    fn fast_path_detects_multi_field_arr() {
        let filter = crate::filter::parse("[.type, .actor.login]").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::MultiFieldArr { entries } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0], vec!["type"]);
                assert_eq!(entries[1], vec!["actor", "login"]);
            }
            other => panic!("expected MultiFieldArr, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_single_field_arr() {
        let filter = crate::filter::parse("[.name]").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::MultiFieldArr { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0], vec!["name"]);
            }
            other => panic!("expected MultiFieldArr, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_not_arr_with_iterate() {
        let filter = crate::filter::parse("[.[] | .x]").unwrap();
        assert!(matches!(detect_fast_path(&filter), NdjsonFastPath::None));
    }

    // --- SelectEqObj / SelectEqArr detection tests ---

    #[test]
    fn fast_path_detects_select_eq_obj() {
        let filter =
            crate::filter::parse("select(.type == \"PushEvent\") | {type, actor: .actor.login}")
                .unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEqObj {
                pred_fields,
                entries,
                ..
            } => {
                assert_eq!(pred_fields, vec!["type"]);
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected SelectEqObj, got {:?}", other),
        }
    }

    #[test]
    fn fast_path_detects_select_eq_arr() {
        let filter = crate::filter::parse("select(.type == \"PushEvent\") | [.type, .id]").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::SelectEqArr {
                pred_fields,
                entries,
                ..
            } => {
                assert_eq!(pred_fields, vec!["type"]);
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected SelectEqArr, got {:?}", other),
        }
    }

    // --- SelectEqField processing tests ---

    #[test]
    fn fast_path_select_eq_field_matching() {
        let data = b"{\"type\":\"PushEvent\",\"name\":\"alice\"}\n{\"type\":\"WatchEvent\",\"name\":\"bob\"}\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\") | .name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(String::from_utf8(output).unwrap(), "\"alice\"\n");
    }

    #[test]
    fn fast_path_select_eq_field_no_match() {
        let data = b"{\"type\":\"WatchEvent\",\"name\":\"a\"}\n{\"type\":\"IssuesEvent\",\"name\":\"b\"}\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\") | .name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(!had_output);
        assert!(output.is_empty());
    }

    #[test]
    fn fast_path_select_eq_field_missing_output() {
        // Predicate matches but output field is missing → null
        let data = b"{\"type\":\"PushEvent\"}\n{\"type\":\"WatchEvent\",\"name\":\"b\"}\n";
        let filter = crate::filter::parse("select(.type == \"PushEvent\") | .name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "null\n");
    }

    #[test]
    fn fast_path_select_eq_field_float_fallback() {
        // 1.0 == 1 requires fallback — should still produce correct result
        let data = b"{\"n\":1.0,\"name\":\"a\"}\n{\"n\":2,\"name\":\"b\"}\n";
        let filter = crate::filter::parse("select(.n == 1) | .name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "\"a\"\n");
    }

    // --- MultiFieldObj / MultiFieldArr processing tests ---

    #[test]
    fn fast_path_multi_field_obj_basic() {
        let data = b"{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
        let filter = crate::filter::parse("{type, id: .id}").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n"
        );
    }

    #[test]
    fn fast_path_multi_field_obj_missing_field() {
        // Missing field should produce null
        let data = b"{\"type\":\"PushEvent\"}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
        let filter = crate::filter::parse("{type, id: .id}").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"type\":\"PushEvent\",\"id\":null}\n{\"type\":\"WatchEvent\",\"id\":2}\n"
        );
    }

    #[test]
    fn fast_path_multi_field_obj_nested() {
        let data = b"{\"actor\":{\"login\":\"alice\"},\"repo\":{\"name\":\"foo\"}}\n";
        let filter = crate::filter::parse("{actor: .actor.login, repo: .repo.name}").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"actor\":\"alice\",\"repo\":\"foo\"}\n"
        );
    }

    #[test]
    fn fast_path_multi_field_arr_basic() {
        let data = b"{\"x\":1,\"y\":2}\n{\"x\":3,\"y\":4}\n";
        let filter = crate::filter::parse("[.x, .y]").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(had_output);
        assert_eq!(String::from_utf8(output).unwrap(), "[1,2]\n[3,4]\n");
    }

    #[test]
    fn fast_path_multi_field_arr_missing_field() {
        let data = b"{\"x\":1}\n{\"x\":2,\"y\":3}\n";
        let filter = crate::filter::parse("[.x, .y]").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "[1,null]\n[2,3]\n");
    }

    #[test]
    fn fast_path_multi_field_arr_nested() {
        let data = b"{\"a\":{\"b\":\"deep\"},\"c\":1}\n";
        let filter = crate::filter::parse("[.a.b, .c]").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "[\"deep\",1]\n");
    }

    // --- SelectEqObj / SelectEqArr processing tests ---

    #[test]
    fn fast_path_select_eq_obj_basic() {
        let data = b"{\"type\":\"A\",\"x\":1}\n{\"type\":\"B\",\"x\":2}\n";
        let filter = crate::filter::parse("select(.type == \"A\") | {x: .x}").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"x\":1}\n");
    }

    #[test]
    fn fast_path_select_eq_obj_no_match() {
        let data = b"{\"type\":\"B\",\"x\":1}\n{\"type\":\"C\",\"x\":2}\n";
        let filter = crate::filter::parse("select(.type == \"A\") | {x: .x}").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, had_output) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert!(!had_output);
        assert!(output.is_empty());
    }

    #[test]
    fn fast_path_select_eq_arr_basic() {
        let data = b"{\"type\":\"A\",\"x\":1,\"y\":2}\n{\"type\":\"B\",\"x\":3,\"y\":4}\n";
        let filter = crate::filter::parse("select(.type == \"A\") | [.x, .y]").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            ..Default::default()
        };
        let env = crate::filter::Env::empty();
        let (output, _) = process_ndjson(data, &filter, &config, &env).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "[1,2]\n");
    }
}
