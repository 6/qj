#![allow(dead_code)]
/// Shared test utilities for jq compatibility runners.
///
/// Provides tool discovery, JSON-aware comparison, process spawning,
/// and result caching used by both `feature_compat.rs` and `jq_compat_runner.rs`.
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Tool {
    pub name: String,
    pub path: String,
}

/// Discover qj (built by cargo) plus jq, jaq, gojq if on `$PATH`.
pub fn discover_tools() -> Vec<Tool> {
    let mut tools = vec![Tool {
        name: "qj".to_string(),
        path: env!("CARGO_BIN_EXE_qj").to_string(),
    }];
    for name in ["jq", "jaq", "gojq"] {
        if let Ok(output) = Command::new("which").arg(name).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    tools.push(Tool {
                        name: name.to_string(),
                        path,
                    });
                }
            }
        }
    }
    tools
}

/// Run a tool with the given filter and input on stdin. Returns stdout (even on
/// non-zero exit) or `None` if the process couldn't be spawned.
pub fn run_tool(tool: &Tool, filter: &str, input: &str, extra_args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(&tool.path);
    cmd.args(extra_args);
    cmd.arg(filter);

    let output = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            // Ignore BrokenPipe — child may exit before reading all input
            let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
            child.wait_with_output()
        })
        .ok()?;

    // Capture stdout regardless of exit status (like bash's || true)
    String::from_utf8(output.stdout).ok()
}

// ---------------------------------------------------------------------------
// Result caching for external tools (jq, jaq, gojq)
// ---------------------------------------------------------------------------

/// Cached results for external tools. qj is never cached.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ToolCache {
    /// Hash of (test file content + mise.toml). Stale if this doesn't match.
    pub content_hash: u64,
    /// tool_name -> { test_case_hash -> stdout_output }
    pub results: HashMap<String, HashMap<u64, Option<String>>>,
}

/// Compute a cache key from test file content and mise.toml (tool versions).
pub fn compute_cache_hash(test_file_content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    test_file_content.hash(&mut hasher);
    let mise_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("mise.toml");
    if let Ok(content) = std::fs::read_to_string(mise_path) {
        content.hash(&mut hasher);
    }
    hasher.finish()
}

/// Hash a single test invocation for cache lookup.
pub fn test_case_hash(filter: &str, input: &str, extra_args: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    filter.hash(&mut hasher);
    input.hash(&mut hasher);
    for arg in extra_args {
        arg.hash(&mut hasher);
    }
    hasher.finish()
}

fn cache_path(runner_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/jq_compat/.cache")
        .join(format!("{runner_name}.json"))
}

/// Load cache if it exists and the hash matches. Returns None if stale/missing.
pub fn load_cache(runner_name: &str, expected_hash: u64) -> Option<ToolCache> {
    let content = std::fs::read_to_string(cache_path(runner_name)).ok()?;
    let cache: ToolCache = serde_json::from_str(&content).ok()?;
    (cache.content_hash == expected_hash).then_some(cache)
}

/// Save cache to disk, creating the .cache directory if needed.
pub fn save_cache(runner_name: &str, cache: &ToolCache) {
    let path = cache_path(runner_name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, json);
    }
}

/// Max output size to cache (1 MB). Larger outputs are not cached to avoid
/// bloating the cache file (some jq.test cases produce multi-GB output).
const MAX_CACHED_OUTPUT: usize = 1_000_000;

/// Run a tool with caching. qj always runs fresh; external tools use cached
/// results when available, falling back to subprocess execution on cache miss.
pub fn run_tool_cached(
    tool: &Tool,
    filter: &str,
    input: &str,
    extra_args: &[&str],
    cache: &mut ToolCache,
) -> Option<String> {
    if tool.name == "qj" {
        return run_tool(tool, filter, input, extra_args);
    }

    let tc_hash = test_case_hash(filter, input, extra_args);

    // Cache hit
    if let Some(cached) = cache.results.get(&tool.name).and_then(|m| m.get(&tc_hash)) {
        return cached.clone();
    }

    // Cache miss: run and store (skip caching oversized outputs)
    let result = run_tool(tool, filter, input, extra_args);
    if result
        .as_ref()
        .map_or(true, |s| s.len() <= MAX_CACHED_OUTPUT)
    {
        cache
            .results
            .entry(tool.name.clone())
            .or_default()
            .insert(tc_hash, result.clone());
    }
    result
}

/// Recursively compare JSON values with numeric coercion (3 == 3.0),
/// matching jq's `==` semantics.
pub fn json_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| json_values_equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|bv| json_values_equal(v, bv)))
        }
        _ => a == b,
    }
}

/// JSON-aware line comparison. Parses both sides as JSON; if both parse,
/// compares with numeric coercion (so 3 == 3.0). Falls back to string comparison.
pub fn json_lines_equal(actual: &[&str], expected: &[&str]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual.iter().zip(expected.iter()).all(|(a, e)| {
        if a == e {
            return true;
        }
        // Try JSON-aware comparison with numeric coercion
        if let (Ok(va), Ok(ve)) = (
            serde_json::from_str::<serde_json::Value>(a),
            serde_json::from_str::<serde_json::Value>(e),
        ) {
            json_values_equal(&va, &ve)
        } else {
            false
        }
    })
}
