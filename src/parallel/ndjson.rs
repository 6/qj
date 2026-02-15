/// Parallel NDJSON (newline-delimited JSON) processing.
///
/// Splits NDJSON input into ~1MB chunks, processes chunks in parallel via
/// rayon, and concatenates output in order.
use anyhow::{Context, Result};
use memchr::memchr_iter;
use rayon::prelude::*;

use std::collections::HashSet;

use crate::filter::{Env, Filter};
use crate::output::{self, OutputConfig};
use crate::simdjson;

/// Detected fast-path strategy for NDJSON processing.
/// Field-chain patterns bypass the Value tree entirely.
enum NdjsonFastPath {
    /// Normal path: parse → Value → eval → output
    None,
    /// `.field.chain` — extract raw JSON via C++ dom_find_field_raw
    FieldChain(Vec<String>),
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
    if std::env::var_os("QJ_NO_FIELD_FAST").is_some() {
        return NdjsonFastPath::None;
    }
    let mut fields = Vec::new();
    if crate::filter::collect_field_chain(filter, &mut fields) && !fields.is_empty() {
        NdjsonFastPath::FieldChain(fields)
    } else {
        NdjsonFastPath::None
    }
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

    // Fast path: field-chain extraction via C++ bridge (no Value tree)
    if let NdjsonFastPath::FieldChain(fields) = fast_path {
        let padded = prepare_padded(trimmed, scratch);
        let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
        let raw = simdjson::dom_find_field_raw(padded, trimmed.len(), &field_refs)
            .context("failed to extract field from NDJSON line")?;
        *had_output = true;
        // In raw output mode, strip surrounding quotes from string values
        if config.mode == output::OutputMode::Raw
            && raw.len() >= 2
            && raw[0] == b'"'
            && raw[raw.len() - 1] == b'"'
        {
            // Unescape JSON string: handle \n, \t, \\, \", \uXXXX etc.
            let inner = &raw[1..raw.len() - 1];
            unescape_json_string(inner, output_buf);
        } else {
            output_buf.extend_from_slice(&raw);
        }
        if config.null_separator {
            output_buf.push(0);
        } else if !config.join_output {
            output_buf.push(b'\n');
        }
        return Ok(());
    }

    // Normal path: parse → Value → eval → output
    let padded = prepare_padded(trimmed, scratch);
    let value = simdjson::dom_parse_to_value(padded, trimmed.len())
        .context("failed to parse NDJSON line")?;

    crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |v| {
        *had_output = true;
        output::write_value(output_buf, &v, config).ok();
    });

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
            NdjsonFastPath::None => panic!("expected FieldChain"),
        }
    }

    #[test]
    fn fast_path_detects_nested_field_chain() {
        let filter = crate::filter::parse(".actor.login").unwrap();
        match detect_fast_path(&filter) {
            NdjsonFastPath::FieldChain(fields) => assert_eq!(fields, vec!["actor", "login"]),
            NdjsonFastPath::None => panic!("expected FieldChain"),
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
}
