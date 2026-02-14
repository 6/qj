/// Shared test utilities for jq compatibility runners.
///
/// Provides tool discovery, JSON-aware comparison, and process spawning
/// used by both `feature_compat.rs` and `jq_compat_runner.rs`.
use std::io::Write;
use std::process::Command;

pub struct Tool {
    pub name: String,
    pub path: String,
}

/// Discover jx (built by cargo) plus jq, jaq, gojq if on `$PATH`.
pub fn discover_tools() -> Vec<Tool> {
    let mut tools = vec![Tool {
        name: "jx".to_string(),
        path: env!("CARGO_BIN_EXE_jx").to_string(),
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
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .ok()?;

    // Capture stdout regardless of exit status (like bash's || true)
    String::from_utf8(output.stdout).ok()
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
