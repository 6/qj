#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::filter::{self, Env};
use qj::output::{OutputConfig, OutputMode};
use qj::parallel::ndjson::{process_ndjson, process_ndjson_no_fast_path};

/// Allocation-free check: data must be valid for NDJSON differential testing.
/// - No control characters except \n, \r, \t (avoids parser strictness edge cases)
/// - Every non-empty line (after ASCII whitespace trim) starts with '{'
/// Matches the trim logic in process_line (ndjson.rs).
fn is_plausible_ndjson(data: &[u8]) -> bool {
    // Reject control characters (except newline, CR, tab).
    for &b in data {
        if b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t' {
            return false;
        }
    }
    let mut has_content = false;
    for line in data.split(|&b| b == b'\n') {
        // Trim trailing space/tab/CR, leading space/tab/CR (matching process_line).
        let end = line
            .iter()
            .rposition(|&b| !matches!(b, b' ' | b'\t' | b'\r'))
            .map_or(0, |p| p + 1);
        let start = line[..end]
            .iter()
            .position(|&b| !matches!(b, b' ' | b'\t' | b'\r'))
            .unwrap_or(end);
        let trimmed = &line[start..end];
        if trimmed.is_empty() {
            continue;
        }
        if trimmed[0] != b'{' {
            return false;
        }
        // Quick single-object check: track brace depth (respecting strings).
        // Must start at depth 0, reach 0 exactly once at the end, and have
        // no trailing content after the closing brace.
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut escape = false;
        let mut closed_at: Option<usize> = None;
        for (i, &b) in trimmed.iter().enumerate() {
            if escape {
                escape = false;
                continue;
            }
            if in_string {
                if b == b'\\' {
                    escape = true;
                } else if b == b'"' {
                    in_string = false;
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => {
                    if closed_at.is_some() {
                        return false; // content after root object closed
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                    if depth == 0 {
                        closed_at = Some(i);
                    }
                }
                _ => {
                    if closed_at.is_some() {
                        return false; // trailing content after root object
                    }
                }
            }
        }
        if depth != 0 || closed_at.is_none() {
            return false;
        }
        has_content = true;
    }
    has_content
}

/// Hardcoded fast-path-eligible filter patterns.
/// The fuzzer selects one based on the first byte of input,
/// then uses the remaining bytes as NDJSON data.
const FILTERS: &[&str] = &[
    // FieldChain
    ".name",
    ".actor.login",
    ".a.b.c",
    // SelectEq (string)
    "select(.type == \"PushEvent\")",
    "select(.name == \"test\")",
    // SelectEq (int/bool/null)
    "select(.count == 42)",
    "select(.active == true)",
    "select(.value == null)",
    // SelectNe
    "select(.type != \"PushEvent\")",
    // SelectOrd
    "select(.count > 10)",
    "select(.count < 100)",
    "select(.count >= 50)",
    "select(.count <= 50)",
    "select(.name > \"m\")",
    // SelectEq + Field
    "select(.type == \"PushEvent\") | .name",
    "select(.count > 10) | .name",
    // SelectEq + Object
    "select(.type == \"PushEvent\") | {name, count: .count}",
    // SelectEq + Array
    "select(.type == \"PushEvent\") | [.name, .count]",
    // MultiFieldObj
    "{name, count: .count}",
    "{type: .type, login: .actor.login}",
    // MultiFieldArr
    "[.name, .count]",
    "[.type, .actor.login]",
    // Length/Keys/KeysUnsorted
    "length",
    ".meta | length",
    "keys",
    ".meta | keys",
    "keys_unsorted",
    ".meta | keys_unsorted",
    // Type
    "type",
    ".meta | type",
    // Has
    "has(\"name\")",
    ".meta | has(\"x\")",
    // SelectStringPred
    "select(.name | test(\"^A\"))",
    "select(.name | startswith(\"test\"))",
    "select(.name | endswith(\".com\"))",
    "select(.name | contains(\"oo\"))",
    // SelectStringPred + Field
    "select(.name | contains(\"oo\")) | .count",
];

// Differential fuzzer: run process_ndjson with fast path vs without,
// assert identical output when both paths succeed.
//
// Uses process_ndjson_no_fast_path (direct call) instead of the QJ_NO_FAST_PATH
// env var to avoid non-deterministic behavior from env var mutation in a
// long-running fuzzer process.
//
// Minimal pre-validation: every non-empty line must start with '{' (after
// trimming). No allocation — just byte scanning. This filters out obviously
// non-JSON input where the fast path and normal path are known to disagree
// (see docs/LIMITATIONS.md).
//
// Only compares when both paths succeed.
fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // Select filter based on first byte.
    let filter_idx = data[0] as usize % FILTERS.len();
    let filter_str = FILTERS[filter_idx];
    let ndjson_data = &data[1..];

    // Need at least one byte of NDJSON.
    if ndjson_data.is_empty() {
        return;
    }

    // Quick structural check: every non-empty line (after trimming) must start
    // with '{'. This is allocation-free and filters out obviously non-JSON input
    // where the two parser paths are known to disagree.
    if !is_plausible_ndjson(ndjson_data) {
        return;
    }

    // Parse the filter.
    let Ok(filter) = filter::parse(filter_str) else {
        return;
    };

    let config = OutputConfig {
        mode: OutputMode::Compact,
        ..OutputConfig::default()
    };
    let env = Env::empty();

    // Run WITH fast path (default).
    let fast_result = process_ndjson(ndjson_data, &filter, &config, &env);

    // Run WITHOUT fast path (direct call, no env var needed).
    let normal_result = process_ndjson_no_fast_path(ndjson_data, &filter, &config, &env);

    // Only compare when both paths succeed. Error disagreements on malformed
    // input are a known architectural issue (on-demand vs DOM parser strictness).
    if let (Ok((fast_out, _)), Ok((normal_out, _))) = (&fast_result, &normal_result) {
        if fast_out != normal_out {
            let fast_s = String::from_utf8_lossy(fast_out);
            let normal_s = String::from_utf8_lossy(normal_out);
            panic!(
                "Fast path diverged from normal path for filter: {filter_str}\n\
                 Input ({} bytes): {:?}\n\
                 Fast output:   {:?}\n\
                 Normal output: {:?}",
                ndjson_data.len(),
                String::from_utf8_lossy(ndjson_data),
                fast_s,
                normal_s,
            );
        }
    }
});
