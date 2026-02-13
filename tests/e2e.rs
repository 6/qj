/// End-to-end tests: run the `jx` binary and compare output to expected values.
use std::process::Command;

fn jx(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .arg(filter)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("failed to run jx");

    assert!(
        output.status.success(),
        "jx exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("jx output was not valid UTF-8")
}

fn jx_compact(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(["-c", filter])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("failed to run jx");

    assert!(
        output.status.success(),
        "jx -c exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("jx output was not valid UTF-8")
}

fn jx_raw(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(["-r", filter])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("failed to run jx");

    assert!(
        output.status.success(),
        "jx -r exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("jx output was not valid UTF-8")
}

// --- Identity ---

#[test]
fn identity_object() {
    let out = jx_compact(".", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":2}"#);
}

#[test]
fn identity_array() {
    let out = jx_compact(".", "[1,2,3]");
    assert_eq!(out.trim(), "[1,2,3]");
}

#[test]
fn identity_scalar() {
    assert_eq!(jx_compact(".", "42").trim(), "42");
    assert_eq!(jx_compact(".", "true").trim(), "true");
    assert_eq!(jx_compact(".", "null").trim(), "null");
    assert_eq!(jx_compact(".", r#""hello""#).trim(), r#""hello""#);
}

// --- Field access ---

#[test]
fn field_access() {
    let out = jx_compact(".name", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#""alice""#);
}

#[test]
fn nested_field_access() {
    let out = jx_compact(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "42");
}

#[test]
fn missing_field() {
    let out = jx_compact(".missing", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "null");
}

// --- Array index ---

#[test]
fn array_index() {
    let out = jx_compact(".[1]", "[10,20,30]");
    assert_eq!(out.trim(), "20");
}

#[test]
fn negative_index() {
    let out = jx_compact(".[-1]", "[10,20,30]");
    assert_eq!(out.trim(), "30");
}

// --- Iteration ---

#[test]
fn iterate_array() {
    let out = jx_compact(".[]", "[1,2,3]");
    assert_eq!(out.trim(), "1\n2\n3");
}

#[test]
fn iterate_object_values() {
    let out = jx_compact(".[]", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), "1\n2");
}

// --- Pipe ---

#[test]
fn pipe_field_from_array() {
    let out = jx_compact(".[] | .name", r#"[{"name":"alice"},{"name":"bob"}]"#);
    assert_eq!(out.trim(), "\"alice\"\n\"bob\"");
}

// --- Select ---

#[test]
fn select_filter() {
    let out = jx_compact(".[] | select(.x > 2)", r#"[{"x":1},{"x":3},{"x":5}]"#);
    assert_eq!(out.trim(), "{\"x\":3}\n{\"x\":5}");
}

// --- Object construction ---

#[test]
fn object_construct() {
    let out = jx_compact("{name: .name}", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
}

// --- Array construction ---

#[test]
fn array_construct() {
    let out = jx_compact("[.[] | .x]", r#"[{"x":1},{"x":2},{"x":3}]"#);
    assert_eq!(out.trim(), "[1,2,3]");
}

// --- Arithmetic ---

#[test]
fn arithmetic() {
    assert_eq!(jx_compact(".x + 5", r#"{"x":10}"#).trim(), "15");
    assert_eq!(jx_compact(".x - 3", r#"{"x":10}"#).trim(), "7");
    assert_eq!(jx_compact(".x * 2", r#"{"x":10}"#).trim(), "20");
    assert_eq!(jx_compact(".x / 2", r#"{"x":10}"#).trim(), "5");
    assert_eq!(jx_compact(".x % 3", r#"{"x":10}"#).trim(), "1");
}

// --- Comparison ---

#[test]
fn comparison() {
    assert_eq!(jx_compact(".x > 5", r#"{"x":10}"#).trim(), "true");
    assert_eq!(jx_compact(".x < 5", r#"{"x":10}"#).trim(), "false");
    assert_eq!(jx_compact(".x == 10", r#"{"x":10}"#).trim(), "true");
    assert_eq!(jx_compact(".x != 10", r#"{"x":10}"#).trim(), "false");
}

// --- Builtins ---

#[test]
fn builtin_length() {
    assert_eq!(jx_compact("length", "[1,2,3]").trim(), "3");
    assert_eq!(jx_compact("length", r#""hello""#).trim(), "5");
}

#[test]
fn builtin_keys() {
    let out = jx_compact("keys", r#"{"b":2,"a":1}"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
}

#[test]
fn builtin_sort() {
    let out = jx_compact("sort", "[3,1,2]");
    assert_eq!(out.trim(), "[1,2,3]");
}

#[test]
fn builtin_map() {
    let out = jx_compact("map(. + 10)", "[1,2,3]");
    assert_eq!(out.trim(), "[11,12,13]");
}

#[test]
fn builtin_add() {
    assert_eq!(jx_compact("add", "[1,2,3]").trim(), "6");
}

#[test]
fn builtin_reverse() {
    assert_eq!(jx_compact("reverse", "[1,2,3]").trim(), "[3,2,1]");
}

#[test]
fn builtin_split_join() {
    let out = jx_compact(r#"split(" ")"#, r#""hello world""#);
    assert_eq!(out.trim(), r#"["hello","world"]"#);

    let out = jx_compact(r#"join("-")"#, r#"["a","b","c"]"#);
    assert_eq!(out.trim(), r#""a-b-c""#);
}

// --- If/then/else ---

#[test]
fn if_then_else() {
    let out = jx_compact(r#"if . > 5 then "big" else "small" end"#, "10");
    assert_eq!(out.trim(), r#""big""#);

    let out = jx_compact(r#"if . > 5 then "big" else "small" end"#, "3");
    assert_eq!(out.trim(), r#""small""#);
}

// --- Alternative ---

#[test]
fn alternative_operator() {
    assert_eq!(jx_compact(".x // 42", r#"{"y":1}"#).trim(), "42");
    assert_eq!(jx_compact(".x // 42", r#"{"x":7}"#).trim(), "7");
}

// --- Comma (multiple outputs) ---

#[test]
fn comma_multiple_outputs() {
    let out = jx_compact(".a, .b", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), "1\n2");
}

// --- Pretty output ---

#[test]
fn pretty_output() {
    let out = jx(".", r#"{"a":1}"#);
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
}

// --- Raw output ---

#[test]
fn raw_string_output() {
    let out = jx_raw(".name", r#"{"name":"hello world"}"#);
    assert_eq!(out.trim(), "hello world");
}

// --- Identity compact passthrough ---

#[test]
fn passthrough_identity_compact_object() {
    let out = jx_compact(".", r#"{"a": 1, "b": [2, 3]}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":[2,3]}"#);
}

#[test]
fn passthrough_identity_compact_array() {
    let out = jx_compact(".", r#"[ 1 , 2 , 3 ]"#);
    assert_eq!(out.trim(), "[1,2,3]");
}

#[test]
fn passthrough_identity_compact_nested() {
    let out = jx_compact(".", r#"{"a": {"b": {"c": [1, 2, 3]}}}"#);
    assert_eq!(out.trim(), r#"{"a":{"b":{"c":[1,2,3]}}}"#);
}

#[test]
fn passthrough_identity_compact_scalar() {
    assert_eq!(jx_compact(".", "42").trim(), "42");
    assert_eq!(jx_compact(".", "true").trim(), "true");
    assert_eq!(jx_compact(".", "null").trim(), "null");
    assert_eq!(jx_compact(".", r#""hello""#).trim(), r#""hello""#);
}

#[test]
fn passthrough_identity_pretty_not_affected() {
    // Non-compact identity should still go through the normal pretty-print path
    let out = jx(".", r#"{"a": 1}"#);
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
}

// --- Field compact passthrough ---

#[test]
fn passthrough_field_compact_basic() {
    let out = jx_compact(".name", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#""alice""#);
}

#[test]
fn passthrough_field_compact_object_value() {
    let out = jx_compact(".data", r#"{"data":{"x":1,"y":[2,3]}}"#);
    assert_eq!(out.trim(), r#"{"x":1,"y":[2,3]}"#);
}

#[test]
fn passthrough_field_compact_nested() {
    let out = jx_compact(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "42");
}

#[test]
fn passthrough_field_compact_missing() {
    let out = jx_compact(".missing", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "null");
}

#[test]
fn passthrough_field_compact_nested_missing() {
    let out = jx_compact(".a.b.missing", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "null");
}

#[test]
fn passthrough_field_compact_non_object() {
    // .field on an array should return null (jq semantics)
    let out = jx_compact(".x", "[1,2,3]");
    assert_eq!(out.trim(), "null");
}

#[test]
fn passthrough_field_compact_stdin() {
    // Same as basic but exercises the stdin path
    let out = jx_compact(".name", r#"{"name":"bob"}"#);
    assert_eq!(out.trim(), r#""bob""#);
}

#[test]
fn passthrough_field_pretty_not_affected() {
    // Without -c, field access should still use normal pretty-print path
    let out = jx(".data", r#"{"data":{"x":1}}"#);
    assert_eq!(out, "{\n  \"x\": 1\n}\n");
}

// --- Passthrough: .field | length ---

#[test]
fn passthrough_field_length_compact() {
    let out = jx_compact(".items | length", r#"{"items":[1,2,3]}"#);
    assert_eq!(out.trim(), "3");
}

#[test]
fn passthrough_field_length_pretty() {
    // length produces a scalar — same output regardless of compact mode
    let out = jx(".items | length", r#"{"items":[1,2,3]}"#);
    assert_eq!(out.trim(), "3");
}

#[test]
fn passthrough_nested_field_length() {
    let out = jx(".a.b | length", r#"{"a":{"b":[10,20]}}"#);
    assert_eq!(out.trim(), "2");
}

#[test]
fn passthrough_missing_field_length() {
    let out = jx(".missing | length", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "0");
}

#[test]
fn passthrough_bare_length_array() {
    let out = jx("length", "[1,2,3,4]");
    assert_eq!(out.trim(), "4");
}

#[test]
fn passthrough_bare_length_string() {
    let out = jx("length", r#""hello""#);
    assert_eq!(out.trim(), "5");
}

#[test]
fn passthrough_bare_length_object() {
    let out = jx("length", r#"{"a":1,"b":2,"c":3}"#);
    assert_eq!(out.trim(), "3");
}

#[test]
fn passthrough_field_length_object_value() {
    let out = jx(".data | length", r#"{"data":{"x":1,"y":2}}"#);
    assert_eq!(out.trim(), "2");
}

#[test]
fn passthrough_field_length_string_value() {
    let out = jx(".name | length", r#"{"name":"hello"}"#);
    assert_eq!(out.trim(), "5");
}

// --- Passthrough: .field | keys ---

#[test]
fn passthrough_field_keys_object() {
    let out = jx_compact(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
}

#[test]
fn passthrough_field_keys_pretty() {
    // keys produces an array — should work without -c too
    let out = jx(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
    // Pretty output should have newlines
    assert!(out.contains("\"a\""));
    assert!(out.contains("\"b\""));
}

#[test]
fn passthrough_bare_keys_object() {
    let out = jx_compact("keys", r#"{"b":2,"a":1,"c":3}"#);
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
}

#[test]
fn passthrough_bare_keys_array() {
    let out = jx_compact("keys", "[10,20,30]");
    assert_eq!(out.trim(), "[0,1,2]");
}

#[test]
fn passthrough_field_keys_array_value() {
    let out = jx_compact(".items | keys", r#"{"items":["x","y"]}"#);
    assert_eq!(out.trim(), "[0,1]");
}

// --- Number literal preservation ---

#[test]
fn number_trailing_zeros_preserved() {
    assert_eq!(jx_compact(".x", r#"{"x":75.80}"#).trim(), "75.80");
    assert_eq!(jx_compact(".x", r#"{"x":1.00}"#).trim(), "1.00");
    assert_eq!(jx_compact(".x", r#"{"x":0.10}"#).trim(), "0.10");
}

#[test]
fn number_scientific_notation_preserved() {
    assert_eq!(jx_compact(".x", r#"{"x":1.5e2}"#).trim(), "1.5e2");
    assert_eq!(jx_compact(".x", r#"{"x":1e10}"#).trim(), "1e10");
    assert_eq!(jx_compact(".x", r#"{"x":2.5E-3}"#).trim(), "2.5E-3");
}

#[test]
fn number_identity_preserves_formatting() {
    // Compact identity should preserve all number formatting
    assert_eq!(
        jx_compact(".", r#"{"a":75.80,"b":1.0e3}"#).trim(),
        r#"{"a":75.80,"b":1.0e3}"#
    );
}

#[test]
fn number_arithmetic_drops_raw_text() {
    // Arithmetic produces computed values — no raw text preservation
    assert_eq!(jx_compact(".x + .x", r#"{"x":37.9}"#).trim(), "75.8");
    assert_eq!(jx_compact(".x * 2", r#"{"x":1.50}"#).trim(), "3");
}

#[test]
fn number_integers_unchanged() {
    assert_eq!(jx_compact(".x", r#"{"x":42}"#).trim(), "42");
    assert_eq!(jx_compact(".x", r#"{"x":-1}"#).trim(), "-1");
    assert_eq!(jx_compact(".x", r#"{"x":0}"#).trim(), "0");
    assert_eq!(
        jx_compact(".x", r#"{"x":9223372036854775807}"#).trim(),
        "9223372036854775807"
    );
}

#[test]
fn number_pretty_preserves_formatting() {
    // Pretty mode should also preserve number literals
    let out = jx(".", r#"{"x":75.80}"#);
    assert!(
        out.contains("75.80"),
        "pretty output should preserve 75.80, got: {out}"
    );
}

// --- jq conformance tests ---
// These run both jx and jq and verify identical output.
// If jq is not installed, the tests pass (they only assert when both are available).

fn jq_available() -> bool {
    Command::new("jq")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_jq_compact(filter: &str, input: &str) -> Option<String> {
    let output = Command::new("jq")
        .args(["-c", filter])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?)
}

/// Assert that jx and jq produce identical output for a given filter+input.
fn assert_jq_compat(filter: &str, input: &str) {
    if !jq_available() {
        return;
    }
    let jx_out = jx_compact(filter, input);
    let jq_out = run_jq_compact(filter, input)
        .unwrap_or_else(|| panic!("jq failed on filter={filter:?} input={input:?}"));
    assert_eq!(
        jx_out.trim(),
        jq_out.trim(),
        "jx vs jq mismatch: filter={filter:?} input={input:?}"
    );
}

#[test]
fn jq_compat_number_formatting() {
    assert_jq_compat(".x", r#"{"x":75.80}"#);
    assert_jq_compat(".x", r#"{"x":0.10}"#);
    assert_jq_compat(".", r#"{"a":75.80}"#);
    // Note: jq normalizes scientific notation (e.g. 1.5e2 → 1.5E+2)
    // while jx preserves the exact original text. Both are valid.
}

#[test]
fn jq_compat_arithmetic() {
    assert_jq_compat(".x + .y", r#"{"x":1,"y":2}"#);
    assert_jq_compat(".x + .x", r#"{"x":37.9}"#);
    assert_jq_compat(".x * 2", r#"{"x":3.14}"#);
}

#[test]
fn jq_compat_field_access() {
    assert_jq_compat(".name", r#"{"name":"alice","age":30}"#);
    assert_jq_compat(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
    assert_jq_compat(".missing", r#"{"name":"alice"}"#);
}

#[test]
fn jq_compat_iteration() {
    assert_jq_compat(".[]", "[1,2,3]");
    assert_jq_compat(".[] | .name", r#"[{"name":"alice"},{"name":"bob"}]"#);
}

#[test]
fn jq_compat_builtins() {
    assert_jq_compat("length", "[1,2,3]");
    assert_jq_compat("keys", r#"{"b":2,"a":1}"#);
    assert_jq_compat("sort", "[3,1,2]");
    assert_jq_compat("map(. + 10)", "[1,2,3]");
    assert_jq_compat("add", "[1,2,3]");
    assert_jq_compat("reverse", "[1,2,3]");
}

#[test]
fn jq_compat_select() {
    assert_jq_compat(".[] | select(. > 2)", "[1,2,3,4,5]");
}

#[test]
fn jq_compat_string_ops() {
    assert_jq_compat(r#"split(" ")"#, r#""hello world""#);
    assert_jq_compat(r#"join("-")"#, r#"["a","b","c"]"#);
    assert_jq_compat("ascii_downcase", r#""HELLO""#);
    assert_jq_compat("ascii_upcase", r#""hello""#);
}

#[test]
fn jq_compat_conditionals() {
    assert_jq_compat(r#"if . > 5 then "big" else "small" end"#, "10");
    assert_jq_compat(".x // 42", r#"{"y":1}"#);
}

// --- File input ---

#[test]
fn file_input() {
    // twitter.json is a real test file
    let twitter =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/data/twitter.json");
    if twitter.exists() {
        let output = Command::new(env!("CARGO_BIN_EXE_jx"))
            .args(["-c", ".statuses | length", twitter.to_str().unwrap()])
            .output()
            .expect("failed to run jx");
        assert!(output.status.success());
        let out = String::from_utf8(output.stdout).unwrap();
        // twitter.json has 100 statuses
        let n: i64 = out.trim().parse().expect("expected integer output");
        assert!(n > 0, "expected positive length from twitter.json");
    }
}
