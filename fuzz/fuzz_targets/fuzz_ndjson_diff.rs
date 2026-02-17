#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::filter::{self, Env};
use qj::output::{OutputConfig, OutputMode};
use qj::parallel::ndjson::process_ndjson;

/// Check if every non-empty line in the NDJSON data is a valid JSON object.
/// We restrict to objects because:
/// 1. Real NDJSON data is always object-per-line
/// 2. The fast paths are designed for objects — non-object values (bare numbers,
///    strings, arrays) can produce different error handling between paths
/// 3. The on-demand and DOM parsers have different strictness on malformed input,
///    so we only assert on well-formed data. See docs/FIX_TODOS.md.
fn is_valid_ndjson_objects(data: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(data) else {
        return false;
    };
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(serde_json::Value::Object(_)) => {}
            _ => return false,
        }
    }
    true
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

// Differential fuzzer: run process_ndjson with fast path enabled vs disabled,
// assert identical output. Catches drift between fast path and normal evaluator.
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

    // Only test well-formed NDJSON. The fast path (on-demand parser) and normal
    // path (DOM parser) have different strictness on malformed input — that
    // divergence is a known issue (docs/FIX_TODOS.md), not what this fuzzer
    // targets. We want to find semantic divergences on valid JSON.
    if !is_valid_ndjson_objects(ndjson_data) {
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
    // SAFETY: fuzz targets are single-threaded, so env var mutation is safe.
    unsafe {
        std::env::remove_var("QJ_NO_FAST_PATH");
    }
    let fast_result = process_ndjson(ndjson_data, &filter, &config, &env);

    // Run WITHOUT fast path.
    unsafe {
        std::env::set_var("QJ_NO_FAST_PATH", "1");
    }
    let normal_result = process_ndjson(ndjson_data, &filter, &config, &env);

    // Clean up env.
    unsafe {
        std::env::remove_var("QJ_NO_FAST_PATH");
    }

    // Compare results.
    match (&fast_result, &normal_result) {
        (Ok((fast_out, _)), Ok((normal_out, _))) => {
            if fast_out != normal_out {
                let fast_s = String::from_utf8_lossy(fast_out);
                let normal_s = String::from_utf8_lossy(normal_out);
                // Known divergence: -0 preserved by fast path (raw byte
                // passthrough), normalized to 0 by normal path (simdjson parses
                // -0 as INT(0)). See docs/FIX_TODOS.md.
                // TODO: remove after fixing -0 preservation
                let fast_normalized = fast_s
                    .replace(":-0}", ":0}")
                    .replace(":-0,", ":0,")
                    .replace(":-0\n", ":0\n");
                if fast_normalized != *normal_s {
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
        }
        // Both errors on valid JSON — unexpected but not a divergence.
        (Err(_), Err(_)) => {}
        // One succeeds and the other fails on valid JSON — shouldn't happen.
        (Ok((fast_out, _)), Err(e)) => {
            panic!(
                "Fast path succeeded but normal path failed on valid JSON for filter: {filter_str}\n\
                 Normal error: {e}\n\
                 Fast output: {:?}",
                String::from_utf8_lossy(fast_out),
            );
        }
        (Err(e), Ok((normal_out, _))) => {
            panic!(
                "Fast path failed but normal path succeeded on valid JSON for filter: {filter_str}\n\
                 Fast error: {e}\n\
                 Normal output: {:?}",
                String::from_utf8_lossy(normal_out),
            );
        }
    }
});
