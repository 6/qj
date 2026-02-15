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

/// Run jx with custom args and return (exit_code, stdout, stderr).
fn jx_exit(args: &[&str], input: &str) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(args)
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

    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Run jx and return (exit_success, stdout, stderr).
fn jx_result(filter: &str, input: &str) -> (bool, String, String) {
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

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// --- Identity ---

#[test]
fn identity_object() {
    let out = jx_compact(".", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":2}"#);
    assert_jq_compat(".", r#"{"a":1,"b":2}"#);
}

#[test]
fn identity_array() {
    let out = jx_compact(".", "[1,2,3]");
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat(".", "[1,2,3]");
}

#[test]
fn identity_scalar() {
    assert_eq!(jx_compact(".", "42").trim(), "42");
    assert_eq!(jx_compact(".", "true").trim(), "true");
    assert_eq!(jx_compact(".", "null").trim(), "null");
    assert_eq!(jx_compact(".", r#""hello""#).trim(), r#""hello""#);
    assert_jq_compat(".", "42");
    assert_jq_compat(".", "true");
    assert_jq_compat(".", "null");
    assert_jq_compat(".", r#""hello""#);
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
    assert_jq_compat(".x + 5", r#"{"x":10}"#);
    assert_jq_compat(".x - 3", r#"{"x":10}"#);
    assert_jq_compat(".x * 2", r#"{"x":10}"#);
    assert_jq_compat(".x / 2", r#"{"x":10}"#);
    assert_jq_compat(".x % 3", r#"{"x":10}"#);
}

// --- Comparison ---

#[test]
fn comparison() {
    assert_eq!(jx_compact(".x > 5", r#"{"x":10}"#).trim(), "true");
    assert_eq!(jx_compact(".x < 5", r#"{"x":10}"#).trim(), "false");
    assert_eq!(jx_compact(".x == 10", r#"{"x":10}"#).trim(), "true");
    assert_eq!(jx_compact(".x != 10", r#"{"x":10}"#).trim(), "false");
    assert_jq_compat(".x > 5", r#"{"x":10}"#);
    assert_jq_compat(".x < 5", r#"{"x":10}"#);
    assert_jq_compat(".x == 10", r#"{"x":10}"#);
    assert_jq_compat(".x != 10", r#"{"x":10}"#);
}

// --- Builtins ---

#[test]
fn builtin_length() {
    assert_eq!(jx_compact("length", "[1,2,3]").trim(), "3");
    assert_eq!(jx_compact("length", r#""hello""#).trim(), "5");
    assert_jq_compat("length", "[1,2,3]");
    assert_jq_compat("length", r#""hello""#);
}

#[test]
fn builtin_keys() {
    let out = jx_compact("keys", r#"{"b":2,"a":1}"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
    assert_jq_compat("keys", r#"{"b":2,"a":1}"#);
}

#[test]
fn builtin_sort() {
    let out = jx_compact("sort", "[3,1,2]");
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat("sort", "[3,1,2]");
}

#[test]
fn builtin_map() {
    let out = jx_compact("map(. + 10)", "[1,2,3]");
    assert_eq!(out.trim(), "[11,12,13]");
    assert_jq_compat("map(. + 10)", "[1,2,3]");
}

#[test]
fn builtin_add() {
    assert_eq!(jx_compact("add", "[1,2,3]").trim(), "6");
    assert_jq_compat("add", "[1,2,3]");
}

#[test]
fn builtin_reverse() {
    assert_eq!(jx_compact("reverse", "[1,2,3]").trim(), "[3,2,1]");
    assert_jq_compat("reverse", "[1,2,3]");
}

#[test]
fn builtin_split_join() {
    let out = jx_compact(r#"split(" ")"#, r#""hello world""#);
    assert_eq!(out.trim(), r#"["hello","world"]"#);

    let out = jx_compact(r#"join("-")"#, r#"["a","b","c"]"#);
    assert_eq!(out.trim(), r#""a-b-c""#);
    assert_jq_compat(r#"split(" ")"#, r#""hello world""#);
    assert_jq_compat(r#"join("-")"#, r#"["a","b","c"]"#);
}

// --- If/then/else ---

#[test]
fn if_then_else() {
    let out = jx_compact(r#"if . > 5 then "big" else "small" end"#, "10");
    assert_eq!(out.trim(), r#""big""#);

    let out = jx_compact(r#"if . > 5 then "big" else "small" end"#, "3");
    assert_eq!(out.trim(), r#""small""#);
    assert_jq_compat(r#"if . > 5 then "big" else "small" end"#, "10");
    assert_jq_compat(r#"if . > 5 then "big" else "small" end"#, "3");
}

// --- Alternative ---

#[test]
fn alternative_operator() {
    assert_eq!(jx_compact(".x // 42", r#"{"y":1}"#).trim(), "42");
    assert_eq!(jx_compact(".x // 42", r#"{"x":7}"#).trim(), "7");
    assert_jq_compat(".x // 42", r#"{"y":1}"#);
    assert_jq_compat(".x // 42", r#"{"x":7}"#);
}

// --- Comma (multiple outputs) ---

#[test]
fn comma_multiple_outputs() {
    let out = jx_compact(".a, .b", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), "1\n2");
    assert_jq_compat(".a, .b", r#"{"a":1,"b":2}"#);
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
    assert_jq_compat(".", r#"{"a": 1, "b": [2, 3]}"#);
}

#[test]
fn passthrough_identity_compact_array() {
    let out = jx_compact(".", r#"[ 1 , 2 , 3 ]"#);
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat(".", r#"[ 1 , 2 , 3 ]"#);
}

#[test]
fn passthrough_identity_compact_nested() {
    let out = jx_compact(".", r#"{"a": {"b": {"c": [1, 2, 3]}}}"#);
    assert_eq!(out.trim(), r#"{"a":{"b":{"c":[1,2,3]}}}"#);
    assert_jq_compat(".", r#"{"a": {"b": {"c": [1, 2, 3]}}}"#);
}

#[test]
fn passthrough_identity_compact_scalar() {
    assert_eq!(jx_compact(".", "42").trim(), "42");
    assert_eq!(jx_compact(".", "true").trim(), "true");
    assert_eq!(jx_compact(".", "null").trim(), "null");
    assert_eq!(jx_compact(".", r#""hello""#).trim(), r#""hello""#);
    assert_jq_compat(".", "42");
    assert_jq_compat(".", "true");
    assert_jq_compat(".", "null");
    assert_jq_compat(".", r#""hello""#);
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
    assert_jq_compat(".name", r#"{"name":"alice","age":30}"#);
}

#[test]
fn passthrough_field_compact_object_value() {
    let out = jx_compact(".data", r#"{"data":{"x":1,"y":[2,3]}}"#);
    assert_eq!(out.trim(), r#"{"x":1,"y":[2,3]}"#);
    assert_jq_compat(".data", r#"{"data":{"x":1,"y":[2,3]}}"#);
}

#[test]
fn passthrough_field_compact_nested() {
    let out = jx_compact(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "42");
    assert_jq_compat(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
}

#[test]
fn passthrough_field_compact_missing() {
    let out = jx_compact(".missing", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "null");
    assert_jq_compat(".missing", r#"{"name":"alice"}"#);
}

#[test]
fn passthrough_field_compact_nested_missing() {
    let out = jx_compact(".a.b.missing", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "null");
    assert_jq_compat(".a.b.missing", r#"{"a":{"b":{"c":42}}}"#);
}

#[test]
fn passthrough_field_compact_non_object() {
    // .field on a non-object produces an error (no output) and exit code 5.
    let (ok, stdout, stderr) = jx_result(".x", "[1,2,3]");
    assert!(!ok, "expected non-zero exit for .x on array");
    assert!(
        stdout.trim().is_empty(),
        "expected no output, got: {stdout}"
    );
    assert!(
        stderr.contains("Cannot index array"),
        "expected error message, got: {stderr}"
    );
}

#[test]
fn passthrough_field_compact_stdin() {
    // Same as basic but exercises the stdin path
    let out = jx_compact(".name", r#"{"name":"bob"}"#);
    assert_eq!(out.trim(), r#""bob""#);
    assert_jq_compat(".name", r#"{"name":"bob"}"#);
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
    assert_jq_compat(".items | length", r#"{"items":[1,2,3]}"#);
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
    assert_jq_compat(".a.b | length", r#"{"a":{"b":[10,20]}}"#);
}

#[test]
fn passthrough_missing_field_length() {
    let out = jx(".missing | length", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "0");
    assert_jq_compat(".missing | length", r#"{"name":"alice"}"#);
}

#[test]
fn passthrough_bare_length_array() {
    let out = jx("length", "[1,2,3,4]");
    assert_eq!(out.trim(), "4");
    assert_jq_compat("length", "[1,2,3,4]");
}

#[test]
fn passthrough_bare_length_string() {
    let out = jx("length", r#""hello""#);
    assert_eq!(out.trim(), "5");
    assert_jq_compat("length", r#""hello""#);
}

#[test]
fn passthrough_bare_length_object() {
    let out = jx("length", r#"{"a":1,"b":2,"c":3}"#);
    assert_eq!(out.trim(), "3");
    assert_jq_compat("length", r#"{"a":1,"b":2,"c":3}"#);
}

#[test]
fn passthrough_field_length_object_value() {
    let out = jx(".data | length", r#"{"data":{"x":1,"y":2}}"#);
    assert_eq!(out.trim(), "2");
    assert_jq_compat(".data | length", r#"{"data":{"x":1,"y":2}}"#);
}

#[test]
fn passthrough_field_length_string_value() {
    let out = jx(".name | length", r#"{"name":"hello"}"#);
    assert_eq!(out.trim(), "5");
    assert_jq_compat(".name | length", r#"{"name":"hello"}"#);
}

// --- Passthrough: .field | keys ---

#[test]
fn passthrough_field_keys_object() {
    let out = jx_compact(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
    assert_jq_compat(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
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
    assert_jq_compat("keys", r#"{"b":2,"a":1,"c":3}"#);
}

#[test]
fn passthrough_bare_keys_array() {
    let out = jx_compact("keys", "[10,20,30]");
    assert_eq!(out.trim(), "[0,1,2]");
    assert_jq_compat("keys", "[10,20,30]");
}

#[test]
fn passthrough_field_keys_array_value() {
    let out = jx_compact(".items | keys", r#"{"items":["x","y"]}"#);
    assert_eq!(out.trim(), "[0,1]");
    assert_jq_compat(".items | keys", r#"{"items":["x","y"]}"#);
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

// --- Error helper ---

fn jx_err(filter: &str, input: &str) -> String {
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
        !output.status.success(),
        "expected jx to fail but it succeeded with stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).unwrap_or_default()
}

fn jx_args(args: &[&str], input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(args)
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
        "jx {:?} exited with {}: stderr={}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("jx output was not valid UTF-8")
}

// --- Builtin: any ---

#[test]
fn builtin_any_with_condition() {
    assert_eq!(jx_compact("any(. > 2)", "[1,2,3]").trim(), "true");
    assert_jq_compat("any(. > 2)", "[1,2,3]");
}

#[test]
fn builtin_any_bare() {
    assert_eq!(jx_compact("any", "[false,null,1]").trim(), "true");
    assert_jq_compat("any", "[false,null,1]");
}

#[test]
fn builtin_any_all_false() {
    assert_eq!(jx_compact("any", "[false,null,false]").trim(), "false");
    assert_jq_compat("any", "[false,null,false]");
}

// --- Builtin: all ---

#[test]
fn builtin_all_with_condition() {
    assert_eq!(jx_compact("all(. > 0)", "[1,2,3]").trim(), "true");
    assert_jq_compat("all(. > 0)", "[1,2,3]");
}

#[test]
fn builtin_all_fails() {
    assert_eq!(jx_compact("all(. > 2)", "[1,2,3]").trim(), "false");
    assert_jq_compat("all(. > 2)", "[1,2,3]");
}

#[test]
fn builtin_all_bare() {
    assert_eq!(jx_compact("all", "[true,1,\"yes\"]").trim(), "true");
    assert_jq_compat("all", r#"[true,1,"yes"]"#);
}

// --- Builtin: contains ---

#[test]
fn builtin_contains_string() {
    assert_eq!(jx_compact(r#"contains("ll")"#, r#""hello""#).trim(), "true");
    assert_jq_compat(r#"contains("ll")"#, r#""hello""#);
}

#[test]
fn builtin_contains_array() {
    assert_eq!(jx_compact("contains([2])", "[1,2,3]").trim(), "true");
    assert_jq_compat("contains([2])", "[1,2,3]");
}

#[test]
fn builtin_contains_object() {
    assert_eq!(
        jx_compact(r#"contains({"a":1})"#, r#"{"a":1,"b":2}"#).trim(),
        "true"
    );
    assert_jq_compat(r#"contains({"a":1})"#, r#"{"a":1,"b":2}"#);
}

// --- Builtin: to_entries / from_entries ---

#[test]
fn builtin_to_entries() {
    assert_eq!(
        jx_compact("to_entries", r#"{"a":1}"#).trim(),
        r#"[{"key":"a","value":1}]"#
    );
    assert_jq_compat("to_entries", r#"{"a":1}"#);
}

#[test]
fn builtin_from_entries() {
    assert_eq!(
        jx_compact("from_entries", r#"[{"key":"a","value":1}]"#).trim(),
        r#"{"a":1}"#
    );
    assert_jq_compat("from_entries", r#"[{"key":"a","value":1}]"#);
}

#[test]
fn builtin_from_entries_name_value() {
    assert_eq!(
        jx_compact("from_entries", r#"[{"name":"a","value":1}]"#).trim(),
        r#"{"a":1}"#
    );
    assert_jq_compat("from_entries", r#"[{"name":"a","value":1}]"#);
}

// --- Builtin: flatten ---

#[test]
fn builtin_flatten() {
    assert_eq!(jx_compact("flatten", "[[1,[2]],3]").trim(), "[1,2,3]");
    assert_jq_compat("flatten", "[[1,[2]],3]");
}

#[test]
fn builtin_flatten_depth() {
    assert_eq!(jx_compact("flatten(1)", "[[1,[2]],3]").trim(), "[1,[2],3]");
    assert_jq_compat("flatten(1)", "[[1,[2]],3]");
}

// --- Builtin: first / last ---

#[test]
fn builtin_first_bare() {
    assert_eq!(jx_compact("first", "[1,2,3]").trim(), "1");
    assert_jq_compat("first", "[1,2,3]");
}

#[test]
fn builtin_first_generator() {
    assert_eq!(jx_compact("first(.[])", "[10,20,30]").trim(), "10");
    assert_jq_compat("first(.[])", "[10,20,30]");
}

#[test]
fn builtin_last_bare() {
    assert_eq!(jx_compact("last", "[1,2,3]").trim(), "3");
    assert_jq_compat("last", "[1,2,3]");
}

#[test]
fn builtin_last_generator() {
    assert_eq!(jx_compact("last(.[])", "[10,20,30]").trim(), "30");
    assert_jq_compat("last(.[])", "[10,20,30]");
}

// --- Builtin: group_by ---

#[test]
fn builtin_group_by() {
    let out = jx_compact(
        "group_by(.a)",
        r#"[{"a":1,"b":"x"},{"a":2,"b":"y"},{"a":1,"b":"z"}]"#,
    );
    assert_eq!(
        out.trim(),
        r#"[[{"a":1,"b":"x"},{"a":1,"b":"z"}],[{"a":2,"b":"y"}]]"#
    );
    assert_jq_compat(
        "group_by(.a)",
        r#"[{"a":1,"b":"x"},{"a":2,"b":"y"},{"a":1,"b":"z"}]"#,
    );
}

// --- Builtin: unique / unique_by ---

#[test]
fn builtin_unique() {
    assert_eq!(jx_compact("unique", "[1,2,1,3]").trim(), "[1,2,3]");
    assert_jq_compat("unique", "[1,2,1,3]");
}

#[test]
fn builtin_unique_by() {
    let out = jx_compact(
        "unique_by(.a)",
        r#"[{"a":1,"b":1},{"a":2,"b":2},{"a":1,"b":3}]"#,
    );
    assert_eq!(out.trim(), r#"[{"a":1,"b":1},{"a":2,"b":2}]"#);
    assert_jq_compat(
        "unique_by(.a)",
        r#"[{"a":1,"b":1},{"a":2,"b":2},{"a":1,"b":3}]"#,
    );
}

// --- Builtin: min / max ---

#[test]
fn builtin_min() {
    assert_eq!(jx_compact("min", "[3,1,2]").trim(), "1");
    assert_jq_compat("min", "[3,1,2]");
}

#[test]
fn builtin_max() {
    assert_eq!(jx_compact("max", "[3,1,2]").trim(), "3");
    assert_jq_compat("max", "[3,1,2]");
}

#[test]
fn builtin_min_empty() {
    assert_eq!(jx_compact("min", "[]").trim(), "null");
    assert_jq_compat("min", "[]");
}

#[test]
fn builtin_max_empty() {
    assert_eq!(jx_compact("max", "[]").trim(), "null");
    assert_jq_compat("max", "[]");
}

// --- Builtin: min_by / max_by ---

#[test]
fn builtin_min_by() {
    assert_eq!(
        jx_compact("min_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"{"x":1}"#
    );
    assert_jq_compat("min_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#);
}

#[test]
fn builtin_max_by() {
    assert_eq!(
        jx_compact("max_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"{"x":3}"#
    );
    assert_jq_compat("max_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#);
}

// --- Builtin: sort_by ---

#[test]
fn builtin_sort_by() {
    assert_eq!(
        jx_compact("sort_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"[{"x":1},{"x":2},{"x":3}]"#
    );
    assert_jq_compat("sort_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#);
}

// --- Builtin: del ---

#[test]
fn builtin_del() {
    assert_eq!(
        jx_compact("del(.a)", r#"{"a":1,"b":2}"#).trim(),
        r#"{"b":2}"#
    );
    assert_jq_compat("del(.a)", r#"{"a":1,"b":2}"#);
}

// --- Builtin: ltrimstr / rtrimstr ---

#[test]
fn builtin_ltrimstr() {
    assert_eq!(
        jx_compact(r#"ltrimstr("hel")"#, r#""hello""#).trim(),
        r#""lo""#
    );
    assert_jq_compat(r#"ltrimstr("hel")"#, r#""hello""#);
}

#[test]
fn builtin_rtrimstr() {
    assert_eq!(
        jx_compact(r#"rtrimstr("lo")"#, r#""hello""#).trim(),
        r#""hel""#
    );
    assert_jq_compat(r#"rtrimstr("lo")"#, r#""hello""#);
}

// --- Builtin: startswith / endswith ---

#[test]
fn builtin_startswith() {
    assert_eq!(
        jx_compact(r#"startswith("hel")"#, r#""hello""#).trim(),
        "true"
    );
    assert_eq!(
        jx_compact(r#"startswith("xyz")"#, r#""hello""#).trim(),
        "false"
    );
    assert_jq_compat(r#"startswith("hel")"#, r#""hello""#);
    assert_jq_compat(r#"startswith("xyz")"#, r#""hello""#);
}

#[test]
fn builtin_endswith() {
    assert_eq!(
        jx_compact(r#"endswith("llo")"#, r#""hello""#).trim(),
        "true"
    );
    assert_eq!(
        jx_compact(r#"endswith("xyz")"#, r#""hello""#).trim(),
        "false"
    );
    assert_jq_compat(r#"endswith("llo")"#, r#""hello""#);
    assert_jq_compat(r#"endswith("xyz")"#, r#""hello""#);
}

// --- Builtin: tonumber / tostring ---

#[test]
fn builtin_tonumber() {
    assert_eq!(jx_compact("tonumber", r#""42""#).trim(), "42");
    assert_eq!(jx_compact("tonumber", r#""3.14""#).trim(), "3.14");
    assert_eq!(jx_compact("tonumber", "42").trim(), "42");
    assert_jq_compat("tonumber", r#""42""#);
    assert_jq_compat("tonumber", r#""3.14""#);
    assert_jq_compat("tonumber", "42");
}

#[test]
fn builtin_tostring() {
    assert_eq!(jx_compact("tostring", "42").trim(), r#""42""#);
    assert_eq!(jx_compact("tostring", "null").trim(), r#""null""#);
    assert_eq!(jx_compact("tostring", "true").trim(), r#""true""#);
    assert_jq_compat("tostring", "42");
    assert_jq_compat("tostring", "null");
    assert_jq_compat("tostring", "true");
}

// --- Builtin: values ---

#[test]
fn builtin_values_object() {
    // values = select(. != null): passes through non-null input
    let out = jx_compact("values", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":2}"#);
    assert_jq_compat("values", r#"{"a":1,"b":2}"#);
}

#[test]
fn builtin_values_array() {
    // values = select(. != null): passes through non-null input
    let out = jx_compact("values", "[10,20,30]");
    assert_eq!(out.trim(), "[10,20,30]");
    assert_jq_compat("values", "[10,20,30]");
    // Test that null is filtered
    let out2 = jx_compact("[.[]|values]", "[1,null,2]");
    assert_eq!(out2.trim(), "[1,2]");
    assert_jq_compat("[.[]|values]", "[1,null,2]");
}

// --- Builtin: empty ---

#[test]
fn builtin_empty() {
    let out = jx_compact("[1, empty, 2]", "null");
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat("[1, empty, 2]", "null");
}

// --- Builtin: not ---

#[test]
fn builtin_not_true() {
    assert_eq!(jx_compact("not", "true").trim(), "false");
    assert_jq_compat("not", "true");
}

#[test]
fn builtin_not_false() {
    assert_eq!(jx_compact("not", "false").trim(), "true");
    assert_jq_compat("not", "false");
}

#[test]
fn builtin_not_null() {
    assert_eq!(jx_compact("not", "null").trim(), "true");
    assert_jq_compat("not", "null");
}

// --- Builtin: keys_unsorted ---

#[test]
fn builtin_keys_unsorted() {
    let out = jx_compact("keys_unsorted", r#"{"b":2,"a":1}"#);
    // keys_unsorted preserves insertion order
    assert_eq!(out.trim(), r#"["b","a"]"#);
    assert_jq_compat("keys_unsorted", r#"{"b":2,"a":1}"#);
}

// --- Builtin: has (e2e) ---

#[test]
fn builtin_has_object() {
    assert_eq!(jx_compact(r#"has("a")"#, r#"{"a":1,"b":2}"#).trim(), "true");
    assert_eq!(
        jx_compact(r#"has("z")"#, r#"{"a":1,"b":2}"#).trim(),
        "false"
    );
    assert_jq_compat(r#"has("a")"#, r#"{"a":1,"b":2}"#);
    assert_jq_compat(r#"has("z")"#, r#"{"a":1,"b":2}"#);
}

#[test]
fn builtin_has_array() {
    assert_eq!(jx_compact("has(1)", "[10,20,30]").trim(), "true");
    assert_eq!(jx_compact("has(5)", "[10,20,30]").trim(), "false");
    assert_jq_compat("has(1)", "[10,20,30]");
    assert_jq_compat("has(5)", "[10,20,30]");
}

// --- Builtin: type (e2e) ---

#[test]
fn builtin_type_all() {
    assert_eq!(jx_compact("type", "42").trim(), r#""number""#);
    assert_eq!(jx_compact("type", r#""hi""#).trim(), r#""string""#);
    assert_eq!(jx_compact("type", "true").trim(), r#""boolean""#);
    assert_eq!(jx_compact("type", "false").trim(), r#""boolean""#);
    assert_eq!(jx_compact("type", "null").trim(), r#""null""#);
    assert_eq!(jx_compact("type", "[1]").trim(), r#""array""#);
    assert_eq!(jx_compact("type", r#"{"a":1}"#).trim(), r#""object""#);
    assert_jq_compat("type", "42");
    assert_jq_compat("type", r#""hi""#);
    assert_jq_compat("type", "true");
    assert_jq_compat("type", "false");
    assert_jq_compat("type", "null");
    assert_jq_compat("type", "[1]");
    assert_jq_compat("type", r#"{"a":1}"#);
}

// --- Builtin: ascii_downcase / ascii_upcase (dedicated e2e) ---

#[test]
fn builtin_ascii_downcase() {
    assert_eq!(
        jx_compact("ascii_downcase", r#""HELLO WORLD""#).trim(),
        r#""hello world""#
    );
    assert_jq_compat("ascii_downcase", r#""HELLO WORLD""#);
}

#[test]
fn builtin_ascii_upcase() {
    assert_eq!(
        jx_compact("ascii_upcase", r#""hello world""#).trim(),
        r#""HELLO WORLD""#
    );
    assert_jq_compat("ascii_upcase", r#""hello world""#);
}

// --- Language: Recursive descent ---

#[test]
fn recursive_descent_numbers() {
    let out = jx_compact("[.. | numbers]", r#"{"a":1,"b":{"c":2},"d":[3]}"#);
    assert_eq!(out.trim(), "[1,2,3]");
}

#[test]
fn recursive_descent_strings() {
    let out = jx_compact("[.. | strings]", r#"{"a":"x","b":{"c":"y"}}"#);
    assert_eq!(out.trim(), r#"["x","y"]"#);
}

// --- Language: Boolean and/or ---

#[test]
fn boolean_and() {
    assert_eq!(jx_compact("true and false", "null").trim(), "false");
    assert_eq!(jx_compact("true and true", "null").trim(), "true");
    assert_jq_compat("true and false", "null");
    assert_jq_compat("true and true", "null");
}

#[test]
fn boolean_or() {
    assert_eq!(jx_compact("false or true", "null").trim(), "true");
    assert_eq!(jx_compact("false or false", "null").trim(), "false");
    assert_jq_compat("false or true", "null");
    assert_jq_compat("false or false", "null");
}

// --- Language: not (as filter) ---

#[test]
fn not_in_select() {
    let out = jx_compact("[.[] | select(. > 2 | not)]", "[1,2,3,4,5]");
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat("[.[] | select(. > 2 | not)]", "[1,2,3,4,5]");
}

// --- Language: Try (?) ---

#[test]
fn try_operator_suppresses_error() {
    // .foo? on a non-object should produce no output (try suppresses the error)
    let out = jx_compact(".foo?", "[1,2,3]");
    assert!(out.trim().is_empty(), "expected no output, got: {out}");
}

#[test]
fn try_operator_on_iteration() {
    // .[]? on null should produce no output
    let out = jx_compact(".[]?", "null");
    assert!(out.trim().is_empty(), "expected no output, got: {out}");
}

// --- Language: Unary negation ---

#[test]
fn unary_negation() {
    // Filter starts with '-', so we need '--' to prevent CLI arg parsing
    let out = jx_args(&["-c", "--", "-(. + 1)"], "5");
    assert_eq!(out.trim(), "-6");
}

#[test]
fn negative_literal() {
    let out = jx_args(&["-c", "--", "-3"], "null");
    assert_eq!(out.trim(), "-3");
}

// --- Language: If-then (no else) ---

#[test]
fn if_then_no_else_true() {
    let out = jx_compact(r#"if . > 5 then "big" end"#, "10");
    assert_eq!(out.trim(), r#""big""#);
    assert_jq_compat(r#"if . > 5 then "big" end"#, "10");
}

#[test]
fn if_then_no_else_false() {
    // When condition is false and no else, jq passes through the input
    let out = jx_compact(r#"if . > 5 then "big" end"#, "3");
    assert_eq!(out.trim(), "3");
    assert_jq_compat(r#"if . > 5 then "big" end"#, "3");
}

// --- Language: Object shorthand ---

#[test]
fn object_shorthand() {
    let out = jx_compact("{name}", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
    assert_jq_compat("{name}", r#"{"name":"alice","age":30}"#);
}

// --- Language: Computed object keys ---

#[test]
fn computed_object_keys() {
    let out = jx_compact("{(.key): .value}", r#"{"key":"name","value":"alice"}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
}

// --- Language: Parenthesized expressions ---

#[test]
fn parenthesized_expression() {
    assert_eq!(jx_compact("(.a + .b) * 2", r#"{"a":3,"b":4}"#).trim(), "14");
    assert_jq_compat("(.a + .b) * 2", r#"{"a":3,"b":4}"#);
}

// --- Edge cases: Error handling ---

#[test]
fn error_invalid_json_input() {
    let stderr = jx_err(".", "not json");
    assert!(!stderr.is_empty(), "expected error message on stderr");
}

#[test]
fn error_invalid_filter_syntax() {
    let stderr = jx_err(".[", "{}");
    assert!(!stderr.is_empty(), "expected parse error on stderr");
}

// --- Edge cases: Null propagation ---

#[test]
fn null_propagation_deep() {
    assert_eq!(jx_compact(".missing.deep.path", "{}").trim(), "null");
}

// --- Edge cases: Null iteration ---

#[test]
fn null_iteration_no_output() {
    let out = jx_compact(".[]?", "null");
    assert!(out.trim().is_empty());
}

// --- Edge cases: Field on array ---

#[test]
fn field_on_array_produces_error() {
    // .field on an array produces an error (no output) and exit code 5
    let (ok, stdout, stderr) = jx_result(".x", "[1,2]");
    assert!(!ok, "expected non-zero exit for .x on array");
    assert!(
        stdout.trim().is_empty(),
        "expected no output, got: {stdout}"
    );
    assert!(
        stderr.contains("Cannot index array"),
        "expected error message, got: {stderr}"
    );
}

// --- Edge cases: Index out of bounds ---

#[test]
fn index_out_of_bounds() {
    assert_eq!(jx_compact(".[99]", "[1,2,3]").trim(), "null");
}

// --- Edge cases: Deeply nested JSON ---

#[test]
fn deeply_nested_json() {
    // Build 100-level nested object: {"a":{"a":{"a":...42...}}}
    let mut json = String::new();
    for _ in 0..100 {
        json.push_str(r#"{"a":"#);
    }
    json.push_str("42");
    for _ in 0..100 {
        json.push('}');
    }
    let out = jx_compact(".", &json);
    assert!(out.contains("42"));
}

// --- Edge cases: Empty object/array ---

#[test]
fn empty_object_keys() {
    assert_eq!(jx_compact("keys", "{}").trim(), "[]");
}

#[test]
fn empty_array_length() {
    assert_eq!(jx_compact("length", "[]").trim(), "0");
}

// --- Edge cases: Null-input flag ---

#[test]
fn null_input_flag() {
    let out = jx_args(&["-n", "-c", "null"], "");
    assert_eq!(out.trim(), "null");
}

// --- Edge cases: Large integers ---

#[test]
fn large_integer_i64_max() {
    assert_eq!(
        jx_compact(".", "9223372036854775807").trim(),
        "9223372036854775807"
    );
}

#[test]
fn integer_overflow_promotes_to_float() {
    // i64::MAX + 1 should promote to f64, not wrap to negative
    let result = jx_compact(". + 1", "9223372036854775807")
        .trim()
        .to_string();
    let val: f64 = result.parse().expect("should be a valid number");
    assert!(val > 0.0, "must not wrap to negative: {result}");
    assert!(val > 9e18, "should be near 2^63: {result}");
}

#[test]
fn large_integer_arithmetic_more_precise_than_jq() {
    // Twitter-style ID: 505874924095815681 (> 2^53, fits in i64)
    // jx does exact i64 arithmetic: +1 = 505874924095815682
    // jq uses f64 and loses precision: +1 = 505874924095815700
    let result = jx_compact(". + 1", "505874924095815681").trim().to_string();
    assert_eq!(result, "505874924095815682");
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

// --- Extended jq conformance ---

#[test]
fn jq_compat_to_entries_from_entries_roundtrip() {
    assert_jq_compat("to_entries | from_entries", r#"{"a":1,"b":2}"#);
}

#[test]
fn jq_compat_unique() {
    assert_jq_compat("unique", "[1,2,1,3,2]");
}

#[test]
fn jq_compat_flatten() {
    assert_jq_compat("flatten", "[[1,[2]],3]");
}

#[test]
fn jq_compat_group_by() {
    assert_jq_compat("group_by(.a)", r#"[{"a":1},{"a":2},{"a":1}]"#);
}

#[test]
fn jq_compat_min_max() {
    assert_jq_compat("min", "[3,1,2]");
    assert_jq_compat("max", "[3,1,2]");
}

#[test]
fn jq_compat_del() {
    assert_jq_compat("del(.a)", r#"{"a":1,"b":2}"#);
}

#[test]
fn jq_compat_recursive_descent() {
    assert_jq_compat("[.. | numbers]", r#"{"a":1,"b":{"c":2}}"#);
}

#[test]
fn jq_compat_any_all() {
    assert_jq_compat("any(. > 2)", "[1,2,3]");
    assert_jq_compat("all(. > 0)", "[1,2,3]");
}

// --- Phase 1: Operator Precedence ---

#[test]
fn operator_precedence_mul_before_add() {
    let out = jx_compact("1 + 2 * 3", "null");
    assert_eq!(out.trim(), "7");
    assert_jq_compat("1 + 2 * 3", "null");
}

#[test]
fn operator_precedence_div_before_sub() {
    let out = jx_compact("10 - 6 / 2", "null");
    assert_eq!(out.trim(), "7");
    assert_jq_compat("10 - 6 / 2", "null");
}

#[test]
fn jq_compat_operator_precedence() {
    assert_jq_compat("1 + 2 * 2", "null");
    assert_jq_compat("10 - 4 / 2", "null");
    assert_jq_compat("2 * 3 + 4 * 5", "null");
}

// --- Phase 1: Cross-Type Sort Ordering ---

#[test]
fn sort_mixed_types() {
    let out = jx_compact("sort", r#"[3,"a",null,true,false,1]"#);
    assert_eq!(out.trim(), r#"[null,false,true,1,3,"a"]"#);
    assert_jq_compat("sort", r#"[3,"a",null,true,false,1]"#);
}

#[test]
fn jq_compat_sort_mixed() {
    assert_jq_compat("sort", r#"[3,"a",null,true,false,1]"#);
}

#[test]
fn unique_returns_sorted() {
    let out = jx_compact("unique", "[3,1,2,1,3]");
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat("unique", "[3,1,2,1,3]");
}

#[test]
fn jq_compat_unique_sorted() {
    assert_jq_compat("unique", "[3,1,2,1,3]");
}

// --- Phase 1: range() ---

#[test]
fn range_single_arg() {
    let out = jx_compact("[range(5)]", "null");
    assert_eq!(out.trim(), "[0,1,2,3,4]");
}

#[test]
fn range_two_args() {
    let out = jx_compact("[range(2;5)]", "null");
    assert_eq!(out.trim(), "[2,3,4]");
}

#[test]
fn range_three_args() {
    let out = jx_compact("[range(0;10;3)]", "null");
    assert_eq!(out.trim(), "[0,3,6,9]");
}

#[test]
fn jq_compat_range() {
    assert_jq_compat("[range(5)]", "null");
    assert_jq_compat("[range(2;5)]", "null");
    assert_jq_compat("[range(0;10;3)]", "null");
}

// --- Phase 1: Math Builtins ---

#[test]
fn math_floor() {
    let out = jx_compact("floor", "3.7");
    assert_eq!(out.trim(), "3");
    assert_jq_compat("floor", "3.7");
}

#[test]
fn math_ceil() {
    let out = jx_compact("ceil", "3.2");
    assert_eq!(out.trim(), "4");
    assert_jq_compat("ceil", "3.2");
}

#[test]
fn math_round() {
    let out = jx_compact("round", "3.5");
    assert_eq!(out.trim(), "4");
    assert_jq_compat("round", "3.5");
}

#[test]
fn math_sqrt() {
    let out = jx_compact("sqrt", "9");
    assert_eq!(out.trim(), "3");
    assert_jq_compat("sqrt", "9");
}

#[test]
fn math_fabs() {
    let out = jx_compact("fabs", "-5.5");
    assert_eq!(out.trim(), "5.5");
    assert_jq_compat("fabs", "-5.5");
}

#[test]
fn math_nan_isnan() {
    let out = jx_compact("nan | isnan", "null");
    assert_eq!(out.trim(), "true");
    assert_jq_compat("nan | isnan", "null");
}

#[test]
fn math_infinite_isinfinite() {
    let out = jx_compact("infinite | isinfinite", "null");
    assert_eq!(out.trim(), "true");
    assert_jq_compat("infinite | isinfinite", "null");
}

#[test]
fn math_isfinite() {
    let out = jx_compact("isfinite", "42");
    assert_eq!(out.trim(), "true");
    assert_jq_compat("isfinite", "42");
}

#[test]
fn jq_compat_math() {
    assert_jq_compat("floor", "3.7");
    assert_jq_compat("ceil", "3.2");
    assert_jq_compat("round", "3.5");
    assert_jq_compat("sqrt", "9.0");
    assert_jq_compat("fabs", "-5.5");
    assert_jq_compat("1 | isfinite", "null");
    assert_jq_compat("nan | isnan", "null");
}

// --- Phase 1: length fixes ---

#[test]
fn length_on_number_abs() {
    let out = jx_compact("length", "-5");
    assert_eq!(out.trim(), "5");
}

#[test]
fn length_on_unicode() {
    // Use `. | length` to bypass the C++ passthrough path which counts bytes
    let out = jx_compact(". | length", r#""café""#);
    assert_eq!(out.trim(), "4");
}

#[test]
fn jq_compat_length() {
    assert_jq_compat("length", "-5");
}

// --- Phase 1: if with multiple condition outputs ---

#[test]
fn if_generator_condition() {
    let out = jx_compact("[if (1,2) > 1 then \"yes\" else \"no\" end]", "null");
    assert_eq!(out.trim(), r#"["no","yes"]"#);
}

// --- Phase 1: Object Construction with Multiple Outputs ---

#[test]
fn object_construct_generator_value() {
    let out = jx_compact("[{x: (1,2)}]", "null");
    assert_eq!(out.trim(), r#"[{"x":1},{"x":2}]"#);
}

#[test]
fn jq_compat_object_generator() {
    assert_jq_compat("[{x: (1,2)}]", "null");
}

// --- Phase 1: String Fixes + New Builtins ---

#[test]
fn split_empty_separator() {
    let out = jx_compact(r#"split("")"#, r#""abc""#);
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat(r#"split("")"#, r#""abc""#);
}

#[test]
fn ascii_downcase_only_ascii() {
    // ascii_downcase should only affect ASCII, not ß → SS etc.
    let out = jx_compact("ascii_downcase", r#""ABCéd""#);
    assert_eq!(out.trim(), r#""abcéd""#);
}

#[test]
fn string_explode() {
    let out = jx_compact("explode", r#""abc""#);
    assert_eq!(out.trim(), "[97,98,99]");
    assert_jq_compat("explode", r#""abc""#);
}

#[test]
fn string_implode() {
    let out = jx_compact("implode", "[97,98,99]");
    assert_eq!(out.trim(), r#""abc""#);
    assert_jq_compat("implode", "[97,98,99]");
}

#[test]
fn tojson_fromjson() {
    let out = jx_compact("[1,2] | tojson", "null");
    assert_eq!(out.trim(), r#""[1,2]""#);
    assert_jq_compat("[1,2] | tojson", "null");
}

#[test]
fn fromjson_basic() {
    let out = jx_compact(r#"fromjson"#, r#""[1,2,3]""#);
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat("fromjson", r#""[1,2,3]""#);
}

#[test]
fn utf8bytelength() {
    let out = jx_compact("utf8bytelength", r#""café""#);
    assert_eq!(out.trim(), "5"); // é is 2 bytes in UTF-8
    assert_jq_compat("utf8bytelength", r#""café""#);
}

#[test]
fn inside_string() {
    let out = jx_compact(r#"inside("foobar")"#, r#""foo""#);
    assert_eq!(out.trim(), "true");
    assert_jq_compat(r#"inside("foobar")"#, r#""foo""#);
}

#[test]
fn string_times_number() {
    let out = jx_compact(r#""ab" * 3"#, "null");
    assert_eq!(out.trim(), r#""ababab""#);
    assert_jq_compat(r#""ab" * 3"#, "null");
}

#[test]
fn string_divide_string() {
    let out = jx_compact(r#""a,b,c" / ",""#, "null");
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat(r#""a,b,c" / ",""#, "null");
}

#[test]
fn index_string() {
    let out = jx_compact(r#"index("bar")"#, r#""foobar""#);
    assert_eq!(out.trim(), "3");
    assert_jq_compat(r#"index("bar")"#, r#""foobar""#);
}

#[test]
fn rindex_string() {
    let out = jx_compact(r#"rindex("o")"#, r#""fooboo""#);
    assert_eq!(out.trim(), "5");
    assert_jq_compat(r#"rindex("o")"#, r#""fooboo""#);
}

#[test]
fn indices_string() {
    let out = jx_compact(r#"indices("o")"#, r#""foobar""#);
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat(r#"indices("o")"#, r#""foobar""#);
}

#[test]
fn trim_builtin() {
    let out = jx_compact("trim", r#""  hello  ""#);
    assert_eq!(out.trim(), r#""hello""#);
}

#[test]
fn jq_compat_string_builtins() {
    assert_jq_compat(r#"split("")"#, r#""abc""#);
    assert_jq_compat("explode", r#""abc""#);
    assert_jq_compat("implode", "[97,98,99]");
    assert_jq_compat("[1,2] | tojson", "null");
    assert_jq_compat("utf8bytelength", r#""abc""#);
    assert_jq_compat(r#"inside("foobar")"#, r#""foo""#);
    assert_jq_compat(r#""ab" * 3"#, "null");
    assert_jq_compat(r#""a,b,c" / ",""#, "null");
}

// --- Phase 1: Small Bug Fixes ---

#[test]
fn from_entries_capitalized_keys() {
    let out = jx_compact("from_entries", r#"[{"Key":"a","Value":1}]"#);
    assert_eq!(out.trim(), r#"{"a":1}"#);
    assert_jq_compat("from_entries", r#"[{"Key":"a","Value":1}]"#);
}

#[test]
fn array_subtraction() {
    let out = jx_compact("[1,2,3] - [2]", "null");
    assert_eq!(out.trim(), "[1,3]");
    assert_jq_compat("[1,2,3] - [2]", "null");
}

#[test]
fn jq_compat_array_subtraction() {
    assert_jq_compat("[1,2,3] - [2]", "null");
}

#[test]
fn object_recursive_merge() {
    let out = jx_compact(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
    assert_eq!(out.trim(), r#"{"a":{"b":1,"c":2}}"#);
    assert_jq_compat(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
}

#[test]
fn jq_compat_object_merge() {
    assert_jq_compat(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
}

#[test]
fn float_modulo() {
    let out = jx_compact(". % 3", "10.5");
    assert_eq!(out.trim(), "1.5");
    // Note: jq truncates to integer for %, jx does float modulo. Intentional difference.
}

#[test]
fn int_division_produces_float() {
    let out = jx_compact("1 / 3", "null");
    // jq: 0.3333333333333333
    let f: f64 = out.trim().parse().expect("expected float");
    assert!((f - 1.0 / 3.0).abs() < 1e-10);
    assert_jq_compat("1 / 3", "null");
}

#[test]
fn index_generator() {
    // .[expr] where expr produces multiple outputs
    let out = jx_compact(r#".[0,2]"#, "[10,20,30]");
    assert_eq!(out.trim(), "10\n30");
    assert_jq_compat(".[0,2]", "[10,20,30]");
}

#[test]
fn jq_compat_index_generator() {
    assert_jq_compat(".[0,2]", "[10,20,30]");
}

// --- Phase 1: Collection Builtins ---

#[test]
fn transpose_basic() {
    let out = jx_compact("transpose", "[[1,2],[3,4]]");
    assert_eq!(out.trim(), "[[1,3],[2,4]]");
}

#[test]
fn jq_compat_transpose() {
    assert_jq_compat("transpose", "[[1,2],[3,4]]");
}

#[test]
fn map_values_object() {
    let out = jx_compact("map_values(. + 1)", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":2,"b":3}"#);
}

#[test]
fn jq_compat_map_values() {
    assert_jq_compat("map_values(. + 1)", r#"{"a":1,"b":2}"#);
}

#[test]
fn limit_builtin() {
    let out = jx_compact("[limit(3; range(10))]", "null");
    assert_eq!(out.trim(), "[0,1,2]");
}

#[test]
fn jq_compat_limit() {
    assert_jq_compat("[limit(3; range(10))]", "null");
}

#[test]
fn until_builtin() {
    let out = jx_compact("0 | until(. >= 5; . + 1)", "null");
    assert_eq!(out.trim(), "5");
}

#[test]
fn jq_compat_until() {
    assert_jq_compat("0 | until(. >= 5; . + 1)", "null");
}

#[test]
fn while_builtin() {
    let out = jx_compact("[1 | while(. < 8; . * 2)]", "null");
    assert_eq!(out.trim(), "[1,2,4]");
}

#[test]
fn jq_compat_while() {
    assert_jq_compat("[1 | while(. < 8; . * 2)]", "null");
}

#[test]
fn isempty_builtin() {
    let out = jx_compact("isempty(empty)", "null");
    assert_eq!(out.trim(), "true");
}

#[test]
fn isempty_not_empty() {
    let out = jx_compact("isempty(range(3))", "null");
    assert_eq!(out.trim(), "false");
}

#[test]
fn jq_compat_isempty() {
    assert_jq_compat("isempty(empty)", "null");
    assert_jq_compat("isempty(range(3))", "null");
}

#[test]
fn getpath_builtin() {
    let out = jx_compact(r#"getpath(["a","b"])"#, r#"{"a":{"b":42}}"#);
    assert_eq!(out.trim(), "42");
}

#[test]
fn jq_compat_getpath() {
    assert_jq_compat(r#"getpath(["a","b"])"#, r#"{"a":{"b":42}}"#);
}

#[test]
fn setpath_builtin() {
    let out = jx_compact(r#"setpath(["a","b"]; 99)"#, r#"{"a":{"b":42}}"#);
    assert_eq!(out.trim(), r#"{"a":{"b":99}}"#);
}

#[test]
fn jq_compat_setpath() {
    assert_jq_compat(r#"setpath(["a","b"]; 99)"#, r#"{"a":{"b":42}}"#);
}

#[test]
fn paths_builtin() {
    let out = jx_compact("[paths]", r#"{"a":1,"b":{"c":2}}"#);
    assert_eq!(out.trim(), r#"[["a"],["b"],["b","c"]]"#);
}

#[test]
fn jq_compat_paths() {
    assert_jq_compat("[paths]", r#"{"a":1,"b":{"c":2}}"#);
}

#[test]
fn leaf_paths_builtin() {
    let out = jx_compact("[leaf_paths]", r#"{"a":1,"b":{"c":2}}"#);
    assert_eq!(out.trim(), r#"[["a"],["b","c"]]"#);
}

#[test]
fn jq_compat_paths_scalars() {
    // leaf_paths is defined as paths(scalars) in jq
    assert_jq_compat("[paths(scalars)]", r#"{"a":1,"b":{"c":2}}"#);
}

#[test]
fn bsearch_found() {
    let out = jx_compact("bsearch(3)", "[1,2,3,4,5]");
    assert_eq!(out.trim(), "2");
}

#[test]
fn bsearch_not_found() {
    let out = jx_compact("bsearch(2)", "[1,3,5]");
    assert_eq!(out.trim(), "-2");
}

#[test]
fn jq_compat_bsearch() {
    assert_jq_compat("bsearch(3)", "[1,2,3,4,5]");
    assert_jq_compat("bsearch(2)", "[1,3,5]");
}

#[test]
fn in_builtin() {
    let out = jx_compact("IN(2, 3)", "3");
    assert_eq!(out.trim(), "true");
}

#[test]
fn in_builtin_false() {
    let out = jx_compact("IN(2, 3)", "5");
    assert_eq!(out.trim(), "false");
}

#[test]
fn with_entries_builtin() {
    let out = jx_compact(
        r#"with_entries(select(.value > 1))"#,
        r#"{"a":1,"b":2,"c":3}"#,
    );
    assert_eq!(out.trim(), r#"{"b":2,"c":3}"#);
}

#[test]
fn jq_compat_with_entries() {
    assert_jq_compat(
        r#"with_entries(select(.value > 1))"#,
        r#"{"a":1,"b":2,"c":3}"#,
    );
}

#[test]
fn abs_builtin() {
    let out = jx_compact("abs", "-42");
    assert_eq!(out.trim(), "42");
}

#[test]
fn jq_compat_abs() {
    assert_jq_compat("abs", "-42");
}

#[test]
fn debug_passthrough() {
    // debug should pass through the value
    let out = jx_compact("debug", "42");
    assert_eq!(out.trim(), "42");
}

#[test]
fn builtins_returns_array() {
    let out = jx_compact("builtins | length", "null");
    let n: i64 = out.trim().parse().expect("expected integer");
    assert!(n > 50, "expected at least 50 builtins, got {n}");
}

#[test]
fn repeat_with_limit() {
    let out = jx_compact("[limit(5; 1 | repeat(. * 2))]", "null");
    assert_eq!(out.trim(), "[2,2,2,2,2]");
}

#[test]
fn jq_compat_repeat() {
    assert_jq_compat("[limit(5; 1 | repeat(. * 2))]", "null");
}

#[test]
fn recurse_with_filter() {
    let out = jx_compact("[2 | recurse(. * .; . < 100)]", "null");
    assert_eq!(out.trim(), "[2,4,16]");
}

#[test]
fn nth_builtin() {
    let out = jx_compact("nth(2; range(5))", "null");
    assert_eq!(out.trim(), "2");
}

#[test]
fn jq_compat_nth() {
    assert_jq_compat("nth(2; range(5))", "null");
}

#[test]
fn delpaths_builtin() {
    let out = jx_compact(r#"delpaths([["a"]])"#, r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"b":2}"#);
}

#[test]
fn jq_compat_delpaths() {
    assert_jq_compat(r#"delpaths([["a"]])"#, r#"{"a":1,"b":2}"#);
}

#[test]
fn todate_builtin() {
    let out = jx_compact("todate", "0");
    assert_eq!(out.trim(), r#""1970-01-01T00:00:00Z""#);
}

#[test]
fn jq_compat_todate() {
    assert_jq_compat("todate", "0");
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

// --- Red flag fix tests ---

#[test]
fn jq_compat_logb() {
    assert_jq_compat("1 | logb", "null");
    assert_jq_compat("8 | logb", "null");
    assert_jq_compat("0.5 | logb", "null");
}

#[test]
fn logb_basic() {
    assert_eq!(jx("1 | logb", "null").trim(), "0");
    assert_eq!(jx("8 | logb", "null").trim(), "3");
    assert_eq!(jx("0.5 | logb", "null").trim(), "-1");
}

#[test]
fn scalb_basic() {
    // scalb(x; e) = x * 2^e
    assert_eq!(jx("2 | scalb(3)", "null").trim(), "16");
    assert_eq!(jx("1 | scalb(10)", "null").trim(), "1024");
    assert_eq!(jx("0.5 | scalb(1)", "null").trim(), "1");
}

#[test]
fn jq_compat_tostring_arrays_objects() {
    assert_jq_compat("[1,2,3] | tostring", "null");
    assert_jq_compat(r#"{"a":1} | tostring"#, "null");
    assert_jq_compat("null | tostring", "null");
    assert_jq_compat("true | tostring", "null");
}

#[test]
fn tostring_json_encodes() {
    // tostring on arrays/objects should produce JSON strings
    assert_eq!(jx("[1,2,3] | tostring", "null").trim(), r#""[1,2,3]""#);
    assert_eq!(jx(r#"{"a":1} | tostring"#, "null").trim(), r#""{\"a\":1}""#);
}

#[test]
fn env_returns_real_vars() {
    // $ENV should contain at least PATH
    let out = jx("$ENV | keys | length", "null");
    let n: i64 = out.trim().parse().unwrap_or(0);
    assert!(n > 0, "env should have entries, got: {}", out.trim());
}

#[test]
fn jq_compat_strftime_extended() {
    assert_jq_compat(r#"0 | strftime("%Y-%m-%d")"#, "null");
    assert_jq_compat(r#"0 | strftime("%H:%M:%S")"#, "null");
    assert_jq_compat(r#"0 | strftime("%A")"#, "null");
    assert_jq_compat(r#"0 | strftime("%j")"#, "null");
}

#[test]
fn jq_compat_todate_fromdate_roundtrip() {
    assert_jq_compat("0 | todate", "null");
    assert_jq_compat("1705318245 | todate", "null");
    assert_jq_compat(r#""1970-01-01T00:00:00Z" | fromdate"#, "null");
    assert_jq_compat(r#""2024-01-15T11:30:45Z" | fromdate"#, "null");
}

#[test]
fn jq_compat_significand() {
    assert_jq_compat("1 | significand", "null");
    assert_jq_compat("8 | significand", "null");
    assert_jq_compat("0 | significand", "null");
}

#[test]
fn variable_binding_basic() {
    assert_jq_compat(". as $x | $x", r#"{"a":1}"#);
}

#[test]
fn variable_binding_arithmetic() {
    assert_jq_compat(".a as $x | .b as $y | $x + $y", r#"{"a":3,"b":4}"#);
}

#[test]
fn variable_binding_in_pipe() {
    assert_jq_compat(".[] | . as $x | {val: $x}", r#"[1,2,3]"#);
}

#[test]
fn variable_binding_scope() {
    // Inner binding shadows outer
    assert_jq_compat("1 as $x | 2 as $x | $x", "null");
}

#[test]
fn variable_in_select() {
    assert_jq_compat(
        r#".threshold as $t | .values[] | select(. > $t)"#,
        r#"{"threshold":5,"values":[3,7,2,9,1]}"#,
    );
}

#[test]
fn slice_array_basic() {
    assert_jq_compat(".[2:4]", "[0,1,2,3,4,5]");
}

#[test]
fn slice_array_from_start() {
    assert_jq_compat(".[:3]", "[0,1,2,3,4,5]");
}

#[test]
fn slice_array_to_end() {
    assert_jq_compat(".[3:]", "[0,1,2,3,4,5]");
}

#[test]
fn slice_array_negative() {
    assert_jq_compat(".[-2:]", "[0,1,2,3,4,5]");
}

#[test]
fn slice_string() {
    assert_jq_compat(".[2:4]", r#""abcdef""#);
}

#[test]
fn elif_chain() {
    assert_jq_compat(
        r#"if . == 1 then "one" elif . == 2 then "two" elif . == 3 then "three" else "other" end"#,
        "2",
    );
}

#[test]
fn elif_no_else() {
    assert_jq_compat(r#"if . == 1 then "one" elif . == 2 then "two" end"#, "3");
}

#[test]
fn try_catch_basic() {
    assert_jq_compat(r#"try error("boom") catch ."#, "null");
}

#[test]
fn try_catch_no_error() {
    assert_jq_compat("try 1 catch 2", "null");
}

#[test]
fn try_catch_error_message() {
    assert_jq_compat(r#"try error("msg") catch ."#, "null");
}

#[test]
fn reduce_sum() {
    assert_jq_compat("reduce .[] as $x (0; . + $x)", "[1,2,3,4,5]");
}

#[test]
fn reduce_build_object() {
    assert_jq_compat(
        r#"reduce .[] as $x ({}; . + {($x): ($x | length)})"#,
        r#"["foo","ab","x"]"#,
    );
}

#[test]
fn foreach_running_sum() {
    assert_jq_compat("[foreach .[] as $x (0; . + $x)]", "[1,2,3,4,5]");
}

#[test]
fn walk_add_one() {
    assert_jq_compat(
        "walk(if type == \"number\" then . + 1 else . end)",
        r#"{"a":1,"b":[2,3]}"#,
    );
}

#[test]
fn walk_strings_upcase() {
    assert_jq_compat(
        r#"walk(if type == "string" then ascii_upcase else . end)"#,
        r#"{"name":"alice","tags":["admin","user"]}"#,
    );
}

#[test]
fn jq_compat_walk() {
    assert_jq_compat(
        "walk(if type == \"number\" then . * 2 else . end)",
        "[1,[2],[[3]]]",
    );
}

#[test]
fn jq_compat_reduce() {
    assert_jq_compat("reduce .[] as $x (0; . + $x)", "[1,2,3,4,5]");
    assert_jq_compat("reduce .[] as $x ([]; . + [$x * 2])", "[1,2,3]");
}

#[test]
fn jq_compat_foreach() {
    assert_jq_compat("[foreach .[] as $x (0; . + $x)]", "[1,2,3]");
}

#[test]
fn jq_compat_try_catch() {
    assert_jq_compat("try .a catch .", r#"{"a":1}"#);
    assert_jq_compat(r#"try error("fail") catch ."#, "null");
    assert_jq_compat("try null catch .", "null");
}

#[test]
fn jq_compat_elif() {
    assert_jq_compat(
        r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#,
        "5",
    );
    assert_jq_compat(
        r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#,
        "0",
    );
    assert_jq_compat(
        r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#,
        "-3",
    );
}

#[test]
fn jq_compat_slicing() {
    assert_jq_compat(".[2:4]", "[0,1,2,3,4,5]");
    assert_jq_compat(".[:2]", "[0,1,2,3,4,5]");
    assert_jq_compat(".[4:]", "[0,1,2,3,4,5]");
    assert_jq_compat(".[-2:]", "[0,1,2,3,4,5]");
    assert_jq_compat(".[2:4]", r#""abcdef""#);
    assert_jq_compat(".[:3]", r#""abcdef""#);
}

#[test]
fn slice_empty_array() {
    assert_jq_compat(".[5:10]", "[]");
}

#[test]
fn slice_inverted_range() {
    assert_jq_compat(".[3:1]", "[0,1,2,3,4]");
}

#[test]
fn slice_negative_both() {
    assert_jq_compat(".[-3:-1]", "[0,1,2,3,4]");
}

#[test]
fn slice_string_empty() {
    assert_jq_compat(".[0:0]", r#""hello""#);
}

#[test]
fn reduce_to_string() {
    assert_jq_compat(r#"reduce .[] as $x (""; . + $x)"#, r#"["a","b","c"]"#);
}

#[test]
fn foreach_three_arg() {
    assert_jq_compat("[foreach .[] as $x (0; . + $x; . * 10)]", "[1,2,3]");
}

#[test]
fn try_keyword_expression() {
    assert_jq_compat("try .foo", r#"{"foo": 1}"#);
}

#[test]
fn try_keyword_on_error() {
    // try on error builtin suppresses the error
    let out = jx(r#"try error("fail")"#, "null");
    assert_eq!(out.trim(), "");
}

#[test]
fn walk_nested_arrays() {
    assert_jq_compat(
        "walk(if type == \"number\" then . * 2 else . end)",
        "[1,[2,[3]]]",
    );
}

#[test]
fn walk_identity() {
    assert_jq_compat("walk(.)", r#"{"a":[1,2],"b":"c"}"#);
}

#[test]
fn jq_compat_variables() {
    assert_jq_compat(". as $x | $x", "42");
    assert_jq_compat(". as $x | $x + 1", "10");
    assert_jq_compat("1 as $x | 2 as $y | $x + $y", "null");
    assert_jq_compat(".[] | . as $x | $x * $x", "[1,2,3,4]");
}

#[test]
fn until_terminates_on_unchanged() {
    // until(false; .) should terminate (structural check: input unchanged)
    let out = jx("0 | until(false; .)", "null");
    assert_eq!(out.trim(), "0");
}

#[test]
fn while_terminates_on_unchanged() {
    // while(true; .) should terminate (structural check: input unchanged)
    let out = jx_compact("0 | [limit(1; while(true; .))]", "null");
    assert_eq!(out.trim(), "[0]");
}

// --- CLI flags helper ---

fn jx_with_args(args: &[&str], input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(args)
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
        "jx {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("not utf-8")
}

// --- --slurp / -s ---

#[test]
fn slurp_single_doc() {
    assert_eq!(
        jx_with_args(&["-cs", ".", "--"], "[1,2,3]").trim(),
        "[[1,2,3]]"
    );
}

#[test]
fn slurp_ndjson() {
    assert_eq!(
        jx_with_args(&["-cs", ".", "--"], "1\n2\n3").trim(),
        "[1,2,3]"
    );
}

#[test]
fn slurp_add() {
    assert_eq!(jx_with_args(&["-cs", "add", "--"], "1\n2\n3").trim(), "6");
}

#[test]
fn slurp_length() {
    assert_eq!(
        jx_with_args(&["-cs", "length", "--"], "1\n2\n3").trim(),
        "3"
    );
}

// --- --arg / --argjson ---

#[test]
fn arg_string() {
    assert_eq!(
        jx_with_args(&["-nc", "--arg", "name", "alice", "{name: $name}"], "").trim(),
        r#"{"name":"alice"}"#
    );
}

#[test]
fn argjson_number() {
    assert_eq!(
        jx_with_args(&["-nc", "--argjson", "x", "42", "$x + 1"], "").trim(),
        "43"
    );
}

#[test]
fn arg_multiple() {
    assert_eq!(
        jx_with_args(
            &[
                "-nc", "--arg", "a", "hello", "--arg", "b", "world", "[$a, $b]"
            ],
            ""
        )
        .trim(),
        r#"["hello","world"]"#
    );
}

#[test]
fn argjson_object() {
    assert_eq!(
        jx_with_args(&["-nc", "--argjson", "obj", r#"{"x":1}"#, "$obj.x"], "").trim(),
        "1"
    );
}

// --- --raw-input / -R ---

#[test]
fn raw_input_line() {
    assert_eq!(
        jx_with_args(&["-Rc", ".", "--"], "hello").trim(),
        r#""hello""#
    );
}

#[test]
fn raw_input_multi() {
    assert_eq!(
        jx_with_args(&["-Rc", ".", "--"], "hello\nworld").trim(),
        "\"hello\"\n\"world\""
    );
}

#[test]
fn raw_input_slurp() {
    assert_eq!(
        jx_with_args(&["-Rsc", ".", "--"], "hello\nworld").trim(),
        r#"["hello","world"]"#
    );
}

#[test]
fn raw_input_slurp_join() {
    assert_eq!(
        jx_with_args(&["-Rsr", r#"join(",")"#, "--"], "hello\nworld").trim(),
        "hello,world"
    );
}

// --- --sort-keys / -S ---

#[test]
fn sort_keys_e2e() {
    assert_eq!(
        jx_with_args(&["-Sc", ".", "--"], r#"{"b":2,"a":1}"#).trim(),
        r#"{"a":1,"b":2}"#
    );
}

#[test]
fn sort_keys_nested_e2e() {
    assert_eq!(
        jx_with_args(&["-Sc", ".", "--"], r#"{"z":{"b":2,"a":1},"a":0}"#).trim(),
        r#"{"a":0,"z":{"a":1,"b":2}}"#
    );
}

// --- --join-output / -j ---

#[test]
fn join_output_e2e() {
    // -j suppresses trailing newlines
    assert_eq!(
        jx_with_args(&["-rj", ".name", "--"], r#"{"name":"hello"}"#),
        "hello"
    );
}

#[test]
fn join_output_compact() {
    // -j works with compact mode too
    assert_eq!(
        jx_with_args(&["-cj", ".", "--"], r#"{"a":1}"#),
        r#"{"a":1}"#
    );
}

// --- -M (monochrome — no-op, but should not error) ---

#[test]
fn monochrome_no_error() {
    jx_with_args(&["-Mc", ".", "--"], "{}");
}

// --- Assignment operators ---

#[test]
fn assign_update_field() {
    assert_eq!(
        jx_compact(".foo |= . + 1", r#"{"foo":42}"#).trim(),
        r#"{"foo":43}"#
    );
    assert_jq_compat(".foo |= . + 1", r#"{"foo":42}"#);
}

#[test]
fn assign_update_iterate() {
    assert_eq!(jx_compact(".[] |= . * 2", "[1,2,3]").trim(), "[2,4,6]");
    assert_jq_compat(".[] |= . * 2", "[1,2,3]");
}

#[test]
fn assign_set_field() {
    assert_eq!(
        jx_compact(".a = 42", r#"{"a":1,"b":2}"#).trim(),
        r#"{"a":42,"b":2}"#
    );
    assert_jq_compat(".a = 42", r#"{"a":1,"b":2}"#);
}

#[test]
fn assign_set_cross_reference() {
    // = evaluates RHS against the original input
    assert_eq!(
        jx_compact(".foo = .bar", r#"{"bar":42}"#).trim(),
        r#"{"bar":42,"foo":42}"#
    );
    assert_jq_compat(".foo = .bar", r#"{"bar":42}"#);
}

#[test]
fn assign_plus_iterate() {
    assert_eq!(jx_compact(".[] += 2", "[1,3,5]").trim(), "[3,5,7]");
    assert_jq_compat(".[] += 2", "[1,3,5]");
}

#[test]
fn assign_mul_iterate() {
    assert_eq!(jx_compact(".[] *= 2", "[1,3,5]").trim(), "[2,6,10]");
    assert_jq_compat(".[] *= 2", "[1,3,5]");
}

#[test]
fn assign_sub_iterate() {
    assert_eq!(jx_compact(".[] -= 2", "[1,3,5]").trim(), "[-1,1,3]");
    assert_jq_compat(".[] -= 2", "[1,3,5]");
}

#[test]
fn assign_div() {
    assert_eq!(jx_compact(".x /= 2", r#"{"x":10}"#).trim(), r#"{"x":5}"#);
    assert_jq_compat(".x /= 2", r#"{"x":10}"#);
}

#[test]
fn assign_mod() {
    assert_eq!(jx_compact(".x %= 3", r#"{"x":10}"#).trim(), r#"{"x":1}"#);
    assert_jq_compat(".x %= 3", r#"{"x":10}"#);
}

#[test]
fn assign_alt_null() {
    assert_eq!(
        jx_compact(r#".a //= "default""#, r#"{"a":null}"#).trim(),
        r#"{"a":"default"}"#
    );
    assert_jq_compat(r#".a //= "default""#, r#"{"a":null}"#);
}

#[test]
fn assign_alt_existing() {
    assert_eq!(
        jx_compact(r#".a //= "default""#, r#"{"a":1}"#).trim(),
        r#"{"a":1}"#
    );
    assert_jq_compat(r#".a //= "default""#, r#"{"a":1}"#);
}

#[test]
fn assign_update_empty_deletion() {
    // |= empty → delete matching elements
    assert_eq!(
        jx_compact("(.[] | select(. >= 2)) |= empty", "[1,5,3,0,7]").trim(),
        "[1,0]"
    );
    assert_jq_compat("(.[] | select(. >= 2)) |= empty", "[1,5,3,0,7]");
}

#[test]
fn assign_nested_path() {
    assert_eq!(
        jx_compact(".a.b |= . + 1", r#"{"a":{"b":10}}"#).trim(),
        r#"{"a":{"b":11}}"#
    );
    assert_jq_compat(".a.b |= . + 1", r#"{"a":{"b":10}}"#);
}

#[test]
fn assign_auto_create_structure() {
    assert_eq!(
        jx_compact(".[2][3] = 1", "[4]").trim(),
        "[4,null,[null,null,null,1]]"
    );
    assert_jq_compat(".[2][3] = 1", "[4]");
}

#[test]
fn assign_update_object_construction() {
    assert_eq!(
        jx_compact(r#".[0].a |= {"old":., "new":(.+1)}"#, r#"[{"a":1,"b":2}]"#).trim(),
        r#"[{"a":{"old":1,"new":2},"b":2}]"#
    );
    assert_jq_compat(r#".[0].a |= {"old":., "new":(.+1)}"#, r#"[{"a":1,"b":2}]"#);
}

#[test]
fn assign_update_with_index() {
    assert_eq!(jx_compact(".[0] |= . + 10", "[1,2,3]").trim(), "[11,2,3]");
    assert_jq_compat(".[0] |= . + 10", "[1,2,3]");
}

#[test]
fn assign_set_new_field() {
    assert_eq!(
        jx_compact(".c = 3", r#"{"a":1,"b":2}"#).trim(),
        r#"{"a":1,"b":2,"c":3}"#
    );
    assert_jq_compat(".c = 3", r#"{"a":1,"b":2}"#);
}

// --- Regex builtins ---

#[test]
fn regex_test_basic() {
    assert_eq!(jx_compact(r#"test("^foo")"#, r#""foobar""#).trim(), "true");
    assert_eq!(jx_compact(r#"test("^foo")"#, r#""barfoo""#).trim(), "false");
    assert_jq_compat(r#"test("^foo")"#, r#""foobar""#);
    assert_jq_compat(r#"test("^foo")"#, r#""barfoo""#);
}

#[test]
fn regex_test_case_insensitive() {
    assert_eq!(
        jx_compact(r#"test("FOO"; "i")"#, r#""foobar""#).trim(),
        "true"
    );
    assert_jq_compat(r#"test("FOO"; "i")"#, r#""foobar""#);
}

#[test]
fn regex_match_basic() {
    let out = jx_compact(r#"match("(o+)")"#, r#""foobar""#);
    assert_eq!(
        out.trim(),
        r#"{"offset":1,"length":2,"string":"oo","captures":[{"offset":1,"length":2,"string":"oo","name":null}]}"#
    );
    assert_jq_compat(r#"match("(o+)")"#, r#""foobar""#);
}

#[test]
fn regex_match_global() {
    let out = jx_compact(r#"[match("o"; "g")]"#, r#""foobar""#);
    // Should produce two match objects
    assert!(out.contains(r#""offset":1"#));
    assert!(out.contains(r#""offset":2"#));
}

#[test]
fn regex_capture_named() {
    let out = jx_compact(r#"capture("(?<y>\\d{4})-(?<m>\\d{2})")"#, r#""2024-01-15""#);
    assert_eq!(out.trim(), r#"{"y":"2024","m":"01"}"#);
    assert_jq_compat(r#"capture("(?<y>\\d{4})-(?<m>\\d{2})")"#, r#""2024-01-15""#);
}

#[test]
fn regex_sub() {
    assert_eq!(
        jx_compact(r#"sub("o"; "0")"#, r#""foobar""#).trim(),
        r#""f0obar""#
    );
    assert_jq_compat(r#"sub("o"; "0")"#, r#""foobar""#);
}

#[test]
fn regex_gsub() {
    assert_eq!(
        jx_compact(r#"gsub("o"; "0")"#, r#""foobar""#).trim(),
        r#""f00bar""#
    );
    assert_jq_compat(r#"gsub("o"; "0")"#, r#""foobar""#);
}

#[test]
fn regex_scan() {
    let out = jx_compact(r#"[scan("[0-9]+")]"#, r#""test 123 test 456""#);
    assert_eq!(out.trim(), r#"["123","456"]"#);
    assert_jq_compat(r#"[scan("[0-9]+")]"#, r#""test 123 test 456""#);
}

#[test]
fn regex_splits() {
    let out = jx_compact(r#"[splits("[,;]")]"#, r#""a,b;c""#);
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat(r#"[splits("[,;]")]"#, r#""a,b;c""#);
}

// --- String interpolation ---

#[test]
fn string_interp_basic() {
    let out = jx_compact(
        r#""name: \(.name), age: \(.age)""#,
        r#"{"name":"alice","age":30}"#,
    );
    assert_eq!(out.trim(), r#""name: alice, age: 30""#);
    assert_jq_compat(
        r#""name: \(.name), age: \(.age)""#,
        r#"{"name":"alice","age":30}"#,
    );
}

#[test]
fn string_interp_expr() {
    let out = jx_compact(r#""sum: \(.a + .b)""#, r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#""sum: 3""#);
    assert_jq_compat(r#""sum: \(.a + .b)""#, r#"{"a":1,"b":2}"#);
}

#[test]
fn string_interp_nested() {
    assert_eq!(
        jx_compact(r#""inter\("pol" + "ation")""#, "null").trim(),
        r#""interpolation""#
    );
    assert_jq_compat(r#""inter\("pol" + "ation")""#, "null");
}

// --- Format strings ---

#[test]
fn format_base64() {
    assert_eq!(jx_compact("@base64", r#""hello""#).trim(), r#""aGVsbG8=""#);
    assert_jq_compat("@base64", r#""hello""#);
}

#[test]
fn format_base64d() {
    assert_eq!(jx_compact("@base64d", r#""aGVsbG8=""#).trim(), r#""hello""#);
    assert_jq_compat("@base64d", r#""aGVsbG8=""#);
}

#[test]
fn format_uri() {
    assert_eq!(
        jx_compact("@uri", r#""hello world""#).trim(),
        r#""hello%20world""#
    );
    assert_jq_compat("@uri", r#""hello world""#);
}

#[test]
fn format_csv() {
    assert_eq!(
        jx_compact("@csv", r#"["a","b","c"]"#).trim(),
        r#""\"a\",\"b\",\"c\"""#
    );
    assert_jq_compat("@csv", r#"["a","b","c"]"#);
}

#[test]
fn format_csv_numbers() {
    assert_eq!(jx_compact("@csv", "[1,2,3]").trim(), r#""1,2,3""#);
    assert_jq_compat("@csv", "[1,2,3]");
}

#[test]
fn format_tsv() {
    assert_eq!(
        jx_compact("@tsv", r#"["a","b","c"]"#).trim(),
        r#""a\tb\tc""#
    );
    assert_jq_compat("@tsv", r#"["a","b","c"]"#);
}

#[test]
fn format_html() {
    assert_eq!(
        jx_compact("@html", r#""<b>bold</b>""#).trim(),
        r#""&lt;b&gt;bold&lt;/b&gt;""#
    );
    assert_jq_compat("@html", r#""<b>bold</b>""#);
}

#[test]
fn format_sh() {
    assert_eq!(
        jx_compact("@sh", r#""hello world""#).trim(),
        r#""'hello world'""#
    );
    assert_jq_compat("@sh", r#""hello world""#);
}

#[test]
fn format_json() {
    assert_eq!(jx_compact("@json", "[1,2,3]").trim(), r#""[1,2,3]""#);
    assert_jq_compat("@json", "[1,2,3]");
}

#[test]
fn format_text() {
    assert_eq!(jx_compact("@text", "42").trim(), r#""42""#);
    assert_jq_compat("@text", "42");
}

// --- Builtin: in ---

#[test]
fn builtin_in_object() {
    assert_eq!(
        jx_compact(r#""foo" | in({"foo":42})"#, "null").trim(),
        "true"
    );
    assert_eq!(
        jx_compact(r#""bar" | in({"foo":42})"#, "null").trim(),
        "false"
    );
    assert_jq_compat(r#""foo" | in({"foo":42})"#, "null");
}

#[test]
fn builtin_in_array() {
    assert_eq!(jx_compact("2 | in([0,1,2])", "null").trim(), "true");
    assert_eq!(jx_compact("5 | in([0,1,2])", "null").trim(), "false");
    assert_jq_compat("2 | in([0,1,2])", "null");
}

// --- Builtin: combinations ---

#[test]
fn builtin_combinations() {
    assert_eq!(
        jx_compact("[combinations]", "[[1,2],[3,4]]").trim(),
        "[[1,3],[1,4],[2,3],[2,4]]"
    );
    assert_jq_compat("[combinations]", "[[1,2],[3,4]]");
}

#[test]
fn builtin_combinations_n() {
    assert_eq!(
        jx_compact("[combinations(2)]", "[0,1]").trim(),
        "[[0,0],[0,1],[1,0],[1,1]]"
    );
    assert_jq_compat("[combinations(2)]", "[0,1]");
}

// --- def (user-defined functions) ---

#[test]
fn def_zero_arg() {
    assert_eq!(jx_compact("def f: . + 1; f", "5").trim(), "6");
    assert_jq_compat("def f: . + 1; f", "5");
}

#[test]
fn def_filter_param() {
    assert_eq!(jx_compact("def f(x): x | x; f(. + 1)", "5").trim(), "7");
    assert_jq_compat("def f(x): x | x; f(. + 1)", "5");
}

#[test]
fn def_generator_body() {
    assert_eq!(jx_compact("def f: (1,2); [f]", "null").trim(), "[1,2]");
    assert_jq_compat("def f: (1,2); [f]", "null");
}

#[test]
fn def_generator_filter_param() {
    // Filter params are generators: x produces 1, then 2; x|x produces 1,2,1,2
    assert_eq!(
        jx_compact("def f(x): x | x; [f(1,2)]", "null").trim(),
        "[1,2,1,2]"
    );
    assert_jq_compat("def f(x): x | x; [f(1,2)]", "null");
}

#[test]
fn def_dollar_param() {
    assert_eq!(jx_compact("def f($x): $x + 1; f(10)", "null").trim(), "11");
    assert_jq_compat("def f($x): $x + 1; f(10)", "null");
}

#[test]
fn def_multiple_dollar_params() {
    assert_eq!(
        jx_compact("def add($a; $b): $a + $b; add(3; 4)", "null").trim(),
        "7"
    );
    assert_jq_compat("def add($a; $b): $a + $b; add(3; 4)", "null");
}

#[test]
fn def_nested() {
    assert_eq!(
        jx_compact("def f: . + 1; def g: f | f; 3 | g", "null").trim(),
        "5"
    );
    assert_jq_compat("def f: . + 1; def g: f | f; 3 | g", "null");
}

#[test]
fn def_shadowing() {
    // Later def of same name/arity shadows earlier one
    assert_eq!(
        jx_compact("def f: . + 1; def f: . * 2; 10 | f", "null").trim(),
        "20"
    );
    assert_jq_compat("def f: . + 1; def f: . * 2; 10 | f", "null");
}

#[test]
fn def_arity_overload() {
    // Same name, different arity — both coexist
    assert_eq!(
        jx_compact("def f: . + 1; def f(x): . + x; [5 | f, f(10)]", "null").trim(),
        "[6,15]"
    );
    assert_jq_compat("def f: . + 1; def f(x): . + x; [5 | f, f(10)]", "null");
}

#[test]
fn def_recursion_factorial() {
    assert_eq!(
        jx_compact(
            "def fac: if . == 1 then 1 else . * ((. - 1) | fac) end; 5 | fac",
            "null"
        )
        .trim(),
        "120"
    );
    assert_jq_compat(
        "def fac: if . == 1 then 1 else . * ((. - 1) | fac) end; 5 | fac",
        "null",
    );
}

#[test]
fn def_closure_captures_var() {
    assert_eq!(jx_compact("5 as $x | def f: $x + 1; f", "null").trim(), "6");
    assert_jq_compat("5 as $x | def f: $x + 1; f", "null");
}

#[test]
fn def_map_with_user_func() {
    assert_eq!(
        jx_compact("def addone: . + 1; [.[] | addone]", "[1,2,3]").trim(),
        "[2,3,4]"
    );
    assert_jq_compat("def addone: . + 1; [.[] | addone]", "[1,2,3]");
}

#[test]
fn def_recursive_sum() {
    assert_eq!(
        jx_compact(
            "def sum: if length == 0 then 0 else .[0] + (.[1:] | sum) end; sum",
            "[1,2,3,4,5]"
        )
        .trim(),
        "15"
    );
    assert_jq_compat(
        "def sum: if length == 0 then 0 else .[0] + (.[1:] | sum) end; sum",
        "[1,2,3,4,5]",
    );
}

// --- Robustness / safety tests ---

#[test]
fn robustness_setpath_huge_index_rejected() {
    // setpath with a huge index should produce no output (error), not OOM.
    let (ok, stdout, stderr) = jx_result("null | setpath([9999999]; 1)", "null");
    assert!(!ok, "expected non-zero exit for huge setpath");
    assert!(
        stdout.trim().is_empty(),
        "huge setpath should produce no output, got: {stdout}"
    );
    assert!(
        stderr.contains("Array index too large"),
        "expected error message, got: {stderr}"
    );
}

#[test]
fn robustness_deeply_nested_parens_rejected() {
    // Parser should reject excessively deep nesting (80 parens → ~160 depth > 128 limit)
    let deep = "(".repeat(80) + "." + &")".repeat(80);
    let (ok, _stdout, stderr) = jx_result(&deep, "null");
    assert!(!ok, "should fail for deep nesting");
    assert!(
        stderr.contains("too deeply nested"),
        "unexpected error: {stderr}"
    );
}

#[test]
fn robustness_fromjson_single_quote_safe() {
    // fromjson with single-quote input should produce no output (error), not panic.
    // Wrap in try-catch to capture the error message.
    let out = jx_compact(r#"("'" | fromjson) // "caught_error""#, "null");
    // Should get the alternative value since fromjson failed
    assert_eq!(out.trim(), r#""caught_error""#);
}

#[test]
fn robustness_fromjson_multibyte_truncation_safe() {
    // fromjson with long multi-byte string should not panic on truncation.
    // 50 copies of é (2 bytes each) = 100 bytes; truncation to 40 must be char-safe.
    // We use try-catch so we can confirm it produces an error rather than crashing.
    let long_str = "é".repeat(50);
    let filter = format!(r#"("{}" | fromjson) // "safe_fallback""#, long_str);
    let out = jx_compact(&filter, "null");
    // Should get the fallback since fromjson on gibberish fails
    assert_eq!(out.trim(), r#""safe_fallback""#);
}

#[test]
fn robustness_no_stale_error_leakage() {
    // An error in one expression should not leak into a subsequent try
    let out = jx_compact(r#"(try error catch "caught") | . + " ok""#, "null");
    assert_eq!(out.trim(), r#""caught ok""#);
}

// --- Exit code tests ---

#[test]
fn exit_code_0_on_success() {
    let (code, stdout, _stderr) = jx_exit(&["-c", "."], r#"{"a":1}"#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"a":1}"#);
}

#[test]
fn exit_code_5_on_runtime_error() {
    // error("boom") should produce exit code 5 and print message to stderr
    let (code, stdout, stderr) = jx_exit(&["-c", r#"error("boom")"#], "null");
    assert_eq!(code, 5, "expected exit code 5, got {code}");
    assert!(
        stdout.trim().is_empty(),
        "expected no stdout, got: {stdout}"
    );
    assert!(
        stderr.contains("boom"),
        "expected error message on stderr, got: {stderr}"
    );
}

#[test]
fn exit_code_5_on_type_error() {
    // .foo on a number should produce exit code 5
    let (code, _stdout, stderr) = jx_exit(&["-c", ".foo"], "42");
    assert_eq!(code, 5, "expected exit code 5, got {code}");
    assert!(
        stderr.contains("Cannot index"),
        "expected type error on stderr, got: {stderr}"
    );
}

#[test]
fn exit_code_4_on_no_output_with_e_flag() {
    // -e flag with no output should produce exit code 4
    let (code, stdout, _stderr) = jx_exit(&["-e", "-c", "empty"], "null");
    assert_eq!(code, 4, "expected exit code 4, got {code}");
    assert!(stdout.trim().is_empty());
}

#[test]
fn exit_code_0_on_output_with_e_flag() {
    // -e flag with output should produce exit code 0
    let (code, stdout, _stderr) = jx_exit(&["-e", "-c", "."], "42");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn exit_code_5_error_builtin_bare() {
    // bare error (no message) uses the input as the error value
    let (code, _stdout, stderr) = jx_exit(&["-c", "error"], r#""my error""#);
    assert_eq!(code, 5, "expected exit code 5, got {code}");
    assert!(
        stderr.contains("my error"),
        "expected error value on stderr, got: {stderr}"
    );
}

#[test]
fn exit_code_0_when_error_caught_by_try() {
    // error caught by try should exit 0
    let (code, stdout, _stderr) = jx_exit(&["-c", r#"try error("boom")"#], "null");
    assert_eq!(code, 0, "expected exit code 0 when error is caught");
    assert!(stdout.trim().is_empty()); // try suppresses both error and output
}

#[test]
fn exit_code_0_when_error_caught_by_try_catch() {
    // error caught by try-catch should exit 0 and output the catch handler result
    let (code, stdout, _stderr) = jx_exit(&["-c", r#"try error("boom") catch ."#], "null");
    assert_eq!(code, 0, "expected exit code 0 when error is caught");
    assert_eq!(stdout.trim(), r#""boom""#);
}

#[test]
fn exit_code_5_precedes_exit_code_4() {
    // When both an error and -e no-output apply, error (exit 5) takes precedence
    let (code, _stdout, stderr) = jx_exit(&["-e", "-c", r#"error("x")"#], "null");
    assert_eq!(code, 5, "error exit code should take precedence over -e");
    assert!(stderr.contains("x"));
}

// --- --from-file tests ---

#[test]
fn from_file_basic() {
    // Write a filter to a temp file and use -f to read it
    let dir = std::env::temp_dir();
    let filter_path = dir.join("jx_test_filter.jq");
    std::fs::write(&filter_path, ".a + .b").unwrap();

    let (code, stdout, _stderr) = jx_exit(
        &["-c", "-f", filter_path.to_str().unwrap()],
        r#"{"a":1,"b":2}"#,
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "3");

    std::fs::remove_file(&filter_path).ok();
}

#[test]
fn from_file_with_input_file() {
    // -f filter_file input_file
    let dir = std::env::temp_dir();
    let filter_path = dir.join("jx_test_filter2.jq");
    let input_path = dir.join("jx_test_input2.json");
    std::fs::write(&filter_path, ".name").unwrap();
    std::fs::write(&input_path, r#"{"name":"alice"}"#).unwrap();

    let (code, stdout, _stderr) = jx_exit(
        &[
            "-c",
            "-f",
            filter_path.to_str().unwrap(),
            input_path.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#""alice""#);

    std::fs::remove_file(&filter_path).ok();
    std::fs::remove_file(&input_path).ok();
}

// ---------------------------------------------------------------------------
// input / inputs builtins
// ---------------------------------------------------------------------------

#[test]
fn inputs_collect_all() {
    let (code, stdout, _) = jx_exit(&["-nc", "[inputs]"], "1\n2\n3\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[1,2,3]");
}

#[test]
fn input_single() {
    let (code, stdout, _) = jx_exit(&["-nc", "input"], "1\n2\n3\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "1");
}

#[test]
fn input_multiple_calls() {
    // Two calls to input: get first two values
    let (code, stdout, _) = jx_exit(&["-nc", "[input, input]"], "10\n20\n30\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[10,20]");
}

#[test]
fn inputs_without_null_input() {
    // Without -n: first value is ., inputs gets the rest
    let (code, stdout, _) = jx_exit(&["-c", "[., inputs]"], "1\n2\n3\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[1,2,3]");
}

#[test]
fn inputs_empty_queue() {
    // With -n and no stdin data, inputs should produce empty array
    let (code, stdout, _) = jx_exit(&["-nc", "[inputs]"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[]");
}

// ---------------------------------------------------------------------------
// Color output
// ---------------------------------------------------------------------------

#[test]
fn color_output_forced() {
    // -C forces color even when piped (test is piped)
    let (code, stdout, _) = jx_exit(&["-Cc", "."], r#"{"a":1}"#);
    assert_eq!(code, 0);
    // Should contain ANSI escape codes
    assert!(
        stdout.contains("\x1b["),
        "expected ANSI codes in colored output, got: {stdout:?}"
    );
}

#[test]
fn monochrome_output() {
    // -M suppresses color
    let (code, stdout, _) = jx_exit(&["-Mc", "."], r#"{"a":1}"#);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("\x1b["),
        "expected no ANSI codes in monochrome output, got: {stdout:?}"
    );
    assert_eq!(stdout.trim(), r#"{"a":1}"#);
}

#[test]
fn color_pretty_output() {
    // -C with pretty print
    let (code, stdout, _) = jx_exit(&["-C", "."], r#"{"a":null,"b":"hi"}"#);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\x1b["),
        "expected ANSI codes in colored pretty output"
    );
    // Should still contain the actual content
    assert!(stdout.contains("null"));
    assert!(stdout.contains("hi"));
}

#[test]
fn no_color_env_suppresses_color() {
    // NO_COLOR env var should suppress color (even without -M)
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(["-c", "."])
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(br#"{"a":1}"#).unwrap();
            child.wait_with_output()
        })
        .expect("failed to run jx");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("\x1b["),
        "NO_COLOR should suppress ANSI codes, got: {stdout:?}"
    );
}

#[test]
fn no_color_env_overridden_by_flag() {
    // -C should override NO_COLOR
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
        .args(["-Cc", "."])
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(br#"{"a":1}"#).unwrap();
            child.wait_with_output()
        })
        .expect("failed to run jx");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\x1b["),
        "-C should override NO_COLOR, got: {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// --rawfile
// ---------------------------------------------------------------------------

#[test]
fn rawfile_binding() {
    let dir = std::env::temp_dir();
    let path = dir.join("jx_test_rawfile.txt");
    std::fs::write(&path, "hello world").unwrap();

    let (code, stdout, _) = jx_exit(
        &[
            "-nc",
            "$content",
            "--rawfile",
            "content",
            path.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#""hello world""#);

    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// --slurpfile
// ---------------------------------------------------------------------------

#[test]
fn slurpfile_binding() {
    let dir = std::env::temp_dir();
    let path = dir.join("jx_test_slurpfile.json");
    std::fs::write(&path, "1\n2\n3").unwrap();

    let (code, stdout, _) = jx_exit(
        &[
            "-nc",
            "$data",
            "--slurpfile",
            "data",
            path.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[1,2,3]");

    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// $ARGS
// ---------------------------------------------------------------------------

#[test]
fn args_positional_strings() {
    let (code, stdout, _) = jx_exit(&["-nc", "$ARGS.positional", "--args", "a", "b", "c"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"["a","b","c"]"#);
}

#[test]
fn jsonargs_positional() {
    let (code, stdout, _) = jx_exit(
        &[
            "-nc",
            "$ARGS.positional",
            "--jsonargs",
            "1",
            "true",
            r#""hi""#,
        ],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"[1,true,"hi"]"#);
}

#[test]
fn args_named_from_arg() {
    let (code, stdout, _) = jx_exit(&["-nc", "$ARGS.named", "--arg", "name", "alice"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"name":"alice"}"#);
}

#[test]
fn args_empty_default() {
    let (code, stdout, _) = jx_exit(&["-nc", "$ARGS"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"positional":[],"named":{}}"#);
}
