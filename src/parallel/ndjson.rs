/// Parallel NDJSON (newline-delimited JSON) processing.
///
/// Splits NDJSON input into ~1MB chunks, processes chunks in parallel via
/// rayon, and concatenates output in order.
use anyhow::{Context, Result};
use memchr::memchr_iter;
use rayon::prelude::*;

use crate::filter::{Filter, ObjKey, StringPart};
use crate::output::{self, OutputConfig};
use crate::simdjson;
use crate::value::Value;

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
        if start + target_size >= buf.len() {
            chunks.push(&buf[start..]);
            break;
        }

        // Find newline at or after target boundary
        match memchr::memchr(b'\n', &buf[start + target_size..]) {
            Some(offset) => {
                let end = start + target_size + offset + 1;
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
/// Falls back to sequential processing for small data or filters containing
/// non-thread-safe literals.
pub fn process_ndjson(
    data: &[u8],
    filter: &Filter,
    config: &OutputConfig,
) -> Result<(Vec<u8>, bool)> {
    if !filter_is_parallel_safe(filter) {
        return process_chunk(data, filter, config);
    }

    let chunks = split_chunks(data, CHUNK_TARGET_SIZE);
    if chunks.len() <= 1 {
        return process_chunk(data, filter, config);
    }

    // SAFETY: filter_is_parallel_safe() verified no Rc-containing literals,
    // so all data in the filter is immutable and thread-safe. eval() only
    // creates thread-local Values.
    let shared = SharedFilter::new(filter);

    let results: Result<Vec<(Vec<u8>, bool)>> = chunks
        .par_iter()
        .map(|&chunk| process_chunk(chunk, shared.get(), config))
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

/// Check if a filter can be safely shared across threads.
///
/// Returns `false` if the filter tree contains any `Value::Array` or
/// `Value::Object` literals (which use `Rc` and are not safe to clone
/// from multiple threads simultaneously).
fn filter_is_parallel_safe(filter: &Filter) -> bool {
    match filter {
        Filter::Literal(Value::Array(_) | Value::Object(_)) => false,
        Filter::Literal(_)
        | Filter::Identity
        | Filter::Iterate
        | Filter::Recurse
        | Filter::Field(_)
        | Filter::Var(_) => true,
        Filter::Index(f)
        | Filter::Select(f)
        | Filter::ArrayConstruct(f)
        | Filter::Not(f)
        | Filter::Try(f)
        | Filter::Neg(f) => filter_is_parallel_safe(f),
        Filter::Pipe(a, b)
        | Filter::Compare(a, _, b)
        | Filter::Arith(a, _, b)
        | Filter::BoolOp(a, _, b)
        | Filter::Alternative(a, b)
        | Filter::Bind(a, _, b)
        | Filter::TryCatch(a, b) => filter_is_parallel_safe(a) && filter_is_parallel_safe(b),
        Filter::Comma(filters) | Filter::Builtin(_, filters) => {
            filters.iter().all(filter_is_parallel_safe)
        }
        Filter::ObjectConstruct(pairs) => pairs.iter().all(|(k, v)| {
            (match k {
                ObjKey::Name(_) => true,
                ObjKey::Expr(f) => filter_is_parallel_safe(f),
            }) && filter_is_parallel_safe(v)
        }),
        Filter::Slice(s, e) => {
            s.as_ref().is_none_or(|f| filter_is_parallel_safe(f))
                && e.as_ref().is_none_or(|f| filter_is_parallel_safe(f))
        }
        Filter::IfThenElse(c, t, e) => {
            filter_is_parallel_safe(c)
                && filter_is_parallel_safe(t)
                && e.as_ref().is_none_or(|f| filter_is_parallel_safe(f))
        }
        Filter::Reduce(src, _, init, update) => {
            filter_is_parallel_safe(src)
                && filter_is_parallel_safe(init)
                && filter_is_parallel_safe(update)
        }
        Filter::Foreach(src, _, init, update, extract) => {
            filter_is_parallel_safe(src)
                && filter_is_parallel_safe(init)
                && filter_is_parallel_safe(update)
                && extract.as_ref().is_none_or(|f| filter_is_parallel_safe(f))
        }
        Filter::StringInterp(parts) => parts.iter().all(|p| match p {
            StringPart::Lit(_) => true,
            StringPart::Expr(f) => filter_is_parallel_safe(f),
        }),
    }
}

/// Process a single chunk of NDJSON lines sequentially.
fn process_chunk(chunk: &[u8], filter: &Filter, config: &OutputConfig) -> Result<(Vec<u8>, bool)> {
    let mut output_buf = Vec::new();
    let mut had_output = false;

    let mut start = 0;
    for nl_pos in memchr_iter(b'\n', chunk) {
        let line = &chunk[start..nl_pos];
        start = nl_pos + 1;
        process_line(line, filter, config, &mut output_buf, &mut had_output)?;
    }

    // Handle last line without trailing newline
    if start < chunk.len() {
        process_line(
            &chunk[start..],
            filter,
            config,
            &mut output_buf,
            &mut had_output,
        )?;
    }

    Ok((output_buf, had_output))
}

/// Process a single NDJSON line: parse, eval filter, write output.
fn process_line(
    line: &[u8],
    filter: &Filter,
    config: &OutputConfig,
    output_buf: &mut Vec<u8>,
    had_output: &mut bool,
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

    let padded = simdjson::pad_buffer(trimmed);
    let value = simdjson::dom_parse_to_value(&padded, trimmed.len())
        .context("failed to parse NDJSON line")?;

    crate::filter::eval::eval_filter(filter, &value, &mut |v| {
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
    fn filter_safety_check() {
        // Simple filters are parallel-safe
        assert!(filter_is_parallel_safe(&Filter::Identity));
        assert!(filter_is_parallel_safe(&Filter::Field("name".into())));
        assert!(filter_is_parallel_safe(&Filter::Literal(Value::Int(42))));
        assert!(filter_is_parallel_safe(&Filter::Literal(Value::String(
            "hello".into()
        ))));

        // Literal arrays/objects are NOT parallel-safe
        assert!(!filter_is_parallel_safe(&Filter::Literal(Value::Array(
            std::rc::Rc::new(vec![])
        ))));
        assert!(!filter_is_parallel_safe(&Filter::Literal(Value::Object(
            std::rc::Rc::new(vec![])
        ))));

        // Nested unsafe literal
        assert!(!filter_is_parallel_safe(&Filter::Pipe(
            Box::new(Filter::Identity),
            Box::new(Filter::Literal(Value::Array(std::rc::Rc::new(vec![])))),
        )));
    }

    #[test]
    fn process_ndjson_basic() {
        let data = b"{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n";
        let filter = crate::filter::parse(".name").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            indent: String::new(),
        };
        let (output, had_output) = process_ndjson(data, &filter, &config).unwrap();
        assert!(had_output);
        assert_eq!(String::from_utf8(output).unwrap(), "\"alice\"\n\"bob\"\n");
    }

    #[test]
    fn process_ndjson_identity() {
        let data = b"{\"a\":1}\n{\"b\":2}\n";
        let filter = crate::filter::parse(".").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            indent: String::new(),
        };
        let (output, _) = process_ndjson(data, &filter, &config).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn process_ndjson_empty_lines() {
        let data = b"{\"a\":1}\n\n{\"b\":2}\n\n";
        let filter = crate::filter::parse(".").unwrap();
        let config = OutputConfig {
            mode: crate::output::OutputMode::Compact,
            indent: String::new(),
        };
        let (output, _) = process_ndjson(data, &filter, &config).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
    }
}
