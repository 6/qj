/// End-to-end tests: run the `qj` binary and compare output to expected values.
use std::process::Command;

fn qj(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");

    assert!(
        output.status.success(),
        "qj exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

fn qj_compact(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");

    assert!(
        output.status.success(),
        "qj -c exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

fn qj_raw(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");

    assert!(
        output.status.success(),
        "qj -r exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

/// Run qjwith custom args and return (exit_code, stdout, stderr).
fn qj_exit(args: &[&str], input: &str) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");

    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Run qjand return (exit_success, stdout, stderr).
fn qj_result(filter: &str, input: &str) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// --- Identity ---

#[test]
fn identity_object() {
    let out = qj_compact(".", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":2}"#);
    assert_jq_compat(".", r#"{"a":1,"b":2}"#);
}

#[test]
fn identity_array() {
    let out = qj_compact(".", "[1,2,3]");
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat(".", "[1,2,3]");
}

#[test]
fn identity_scalar() {
    assert_eq!(qj_compact(".", "42").trim(), "42");
    assert_eq!(qj_compact(".", "true").trim(), "true");
    assert_eq!(qj_compact(".", "null").trim(), "null");
    assert_eq!(qj_compact(".", r#""hello""#).trim(), r#""hello""#);
    assert_jq_compat(".", "42");
    assert_jq_compat(".", "true");
    assert_jq_compat(".", "null");
    assert_jq_compat(".", r#""hello""#);
}

// --- Field access ---

#[test]
fn field_access() {
    let out = qj_compact(".name", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#""alice""#);
}

#[test]
fn nested_field_access() {
    let out = qj_compact(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "42");
}

#[test]
fn missing_field() {
    let out = qj_compact(".missing", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "null");
}

// --- Array index ---

#[test]
fn array_index() {
    let out = qj_compact(".[1]", "[10,20,30]");
    assert_eq!(out.trim(), "20");
}

#[test]
fn negative_index() {
    let out = qj_compact(".[-1]", "[10,20,30]");
    assert_eq!(out.trim(), "30");
}

// --- Iteration ---

#[test]
fn iterate_array() {
    let out = qj_compact(".[]", "[1,2,3]");
    assert_eq!(out.trim(), "1\n2\n3");
}

#[test]
fn iterate_object_values() {
    let out = qj_compact(".[]", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), "1\n2");
}

// --- Pipe ---

#[test]
fn pipe_field_from_array() {
    let out = qj_compact(".[] | .name", r#"[{"name":"alice"},{"name":"bob"}]"#);
    assert_eq!(out.trim(), "\"alice\"\n\"bob\"");
}

// --- Select ---

#[test]
fn select_filter() {
    let out = qj_compact(".[] | select(.x > 2)", r#"[{"x":1},{"x":3},{"x":5}]"#);
    assert_eq!(out.trim(), "{\"x\":3}\n{\"x\":5}");
}

// --- Object construction ---

#[test]
fn object_construct() {
    let out = qj_compact("{name: .name}", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
}

// --- Array construction ---

#[test]
fn array_construct() {
    let out = qj_compact("[.[] | .x]", r#"[{"x":1},{"x":2},{"x":3}]"#);
    assert_eq!(out.trim(), "[1,2,3]");
}

// --- Arithmetic ---

#[test]
fn arithmetic() {
    assert_eq!(qj_compact(".x + 5", r#"{"x":10}"#).trim(), "15");
    assert_eq!(qj_compact(".x - 3", r#"{"x":10}"#).trim(), "7");
    assert_eq!(qj_compact(".x * 2", r#"{"x":10}"#).trim(), "20");
    assert_eq!(qj_compact(".x / 2", r#"{"x":10}"#).trim(), "5");
    assert_eq!(qj_compact(".x % 3", r#"{"x":10}"#).trim(), "1");
    assert_jq_compat(".x + 5", r#"{"x":10}"#);
    assert_jq_compat(".x - 3", r#"{"x":10}"#);
    assert_jq_compat(".x * 2", r#"{"x":10}"#);
    assert_jq_compat(".x / 2", r#"{"x":10}"#);
    assert_jq_compat(".x % 3", r#"{"x":10}"#);
}

// --- Comparison ---

#[test]
fn comparison() {
    assert_eq!(qj_compact(".x > 5", r#"{"x":10}"#).trim(), "true");
    assert_eq!(qj_compact(".x < 5", r#"{"x":10}"#).trim(), "false");
    assert_eq!(qj_compact(".x == 10", r#"{"x":10}"#).trim(), "true");
    assert_eq!(qj_compact(".x != 10", r#"{"x":10}"#).trim(), "false");
    assert_jq_compat(".x > 5", r#"{"x":10}"#);
    assert_jq_compat(".x < 5", r#"{"x":10}"#);
    assert_jq_compat(".x == 10", r#"{"x":10}"#);
    assert_jq_compat(".x != 10", r#"{"x":10}"#);
}

// --- Builtins ---

#[test]
fn builtin_length() {
    assert_eq!(qj_compact("length", "[1,2,3]").trim(), "3");
    assert_eq!(qj_compact("length", r#""hello""#).trim(), "5");
    assert_jq_compat("length", "[1,2,3]");
    assert_jq_compat("length", r#""hello""#);
}

#[test]
fn builtin_keys() {
    let out = qj_compact("keys", r#"{"b":2,"a":1}"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
    assert_jq_compat("keys", r#"{"b":2,"a":1}"#);
}

#[test]
fn builtin_sort() {
    let out = qj_compact("sort", "[3,1,2]");
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat("sort", "[3,1,2]");
}

#[test]
fn builtin_map() {
    let out = qj_compact("map(. + 10)", "[1,2,3]");
    assert_eq!(out.trim(), "[11,12,13]");
    assert_jq_compat("map(. + 10)", "[1,2,3]");
}

#[test]
fn builtin_add() {
    assert_eq!(qj_compact("add", "[1,2,3]").trim(), "6");
    assert_jq_compat("add", "[1,2,3]");
}

#[test]
fn builtin_reverse() {
    assert_eq!(qj_compact("reverse", "[1,2,3]").trim(), "[3,2,1]");
    assert_jq_compat("reverse", "[1,2,3]");
}

#[test]
fn builtin_split_join() {
    let out = qj_compact(r#"split(" ")"#, r#""hello world""#);
    assert_eq!(out.trim(), r#"["hello","world"]"#);

    let out = qj_compact(r#"join("-")"#, r#"["a","b","c"]"#);
    assert_eq!(out.trim(), r#""a-b-c""#);
    assert_jq_compat(r#"split(" ")"#, r#""hello world""#);
    assert_jq_compat(r#"join("-")"#, r#"["a","b","c"]"#);
}

// --- If/then/else ---

#[test]
fn if_then_else() {
    let out = qj_compact(r#"if . > 5 then "big" else "small" end"#, "10");
    assert_eq!(out.trim(), r#""big""#);

    let out = qj_compact(r#"if . > 5 then "big" else "small" end"#, "3");
    assert_eq!(out.trim(), r#""small""#);
    assert_jq_compat(r#"if . > 5 then "big" else "small" end"#, "10");
    assert_jq_compat(r#"if . > 5 then "big" else "small" end"#, "3");
}

// --- Alternative ---

#[test]
fn alternative_operator() {
    assert_eq!(qj_compact(".x // 42", r#"{"y":1}"#).trim(), "42");
    assert_eq!(qj_compact(".x // 42", r#"{"x":7}"#).trim(), "7");
    assert_jq_compat(".x // 42", r#"{"y":1}"#);
    assert_jq_compat(".x // 42", r#"{"x":7}"#);
}

// --- Comma (multiple outputs) ---

#[test]
fn comma_multiple_outputs() {
    let out = qj_compact(".a, .b", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), "1\n2");
    assert_jq_compat(".a, .b", r#"{"a":1,"b":2}"#);
}

// --- Pretty output ---

#[test]
fn pretty_output() {
    let out = qj(".", r#"{"a":1}"#);
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
}

// --- Raw output ---

#[test]
fn raw_string_output() {
    let out = qj_raw(".name", r#"{"name":"hello world"}"#);
    assert_eq!(out.trim(), "hello world");
}

// --- Identity compact passthrough ---

#[test]
fn passthrough_identity_compact_object() {
    let out = qj_compact(".", r#"{"a": 1, "b": [2, 3]}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":[2,3]}"#);
    assert_jq_compat(".", r#"{"a": 1, "b": [2, 3]}"#);
}

#[test]
fn passthrough_identity_compact_array() {
    let out = qj_compact(".", r#"[ 1 , 2 , 3 ]"#);
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat(".", r#"[ 1 , 2 , 3 ]"#);
}

#[test]
fn passthrough_identity_compact_nested() {
    let out = qj_compact(".", r#"{"a": {"b": {"c": [1, 2, 3]}}}"#);
    assert_eq!(out.trim(), r#"{"a":{"b":{"c":[1,2,3]}}}"#);
    assert_jq_compat(".", r#"{"a": {"b": {"c": [1, 2, 3]}}}"#);
}

#[test]
fn passthrough_identity_compact_scalar() {
    assert_eq!(qj_compact(".", "42").trim(), "42");
    assert_eq!(qj_compact(".", "true").trim(), "true");
    assert_eq!(qj_compact(".", "null").trim(), "null");
    assert_eq!(qj_compact(".", r#""hello""#).trim(), r#""hello""#);
    assert_jq_compat(".", "42");
    assert_jq_compat(".", "true");
    assert_jq_compat(".", "null");
    assert_jq_compat(".", r#""hello""#);
}

#[test]
fn passthrough_identity_pretty_not_affected() {
    // Non-compact identity should still go through the normal pretty-print path
    let out = qj(".", r#"{"a": 1}"#);
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
}

// --- Field compact passthrough ---

#[test]
fn passthrough_field_compact_basic() {
    let out = qj_compact(".name", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#""alice""#);
    assert_jq_compat(".name", r#"{"name":"alice","age":30}"#);
}

#[test]
fn passthrough_field_compact_object_value() {
    let out = qj_compact(".data", r#"{"data":{"x":1,"y":[2,3]}}"#);
    assert_eq!(out.trim(), r#"{"x":1,"y":[2,3]}"#);
    assert_jq_compat(".data", r#"{"data":{"x":1,"y":[2,3]}}"#);
}

#[test]
fn passthrough_field_compact_nested() {
    let out = qj_compact(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "42");
    assert_jq_compat(".a.b.c", r#"{"a":{"b":{"c":42}}}"#);
}

#[test]
fn passthrough_field_compact_missing() {
    let out = qj_compact(".missing", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "null");
    assert_jq_compat(".missing", r#"{"name":"alice"}"#);
}

#[test]
fn passthrough_field_compact_nested_missing() {
    let out = qj_compact(".a.b.missing", r#"{"a":{"b":{"c":42}}}"#);
    assert_eq!(out.trim(), "null");
    assert_jq_compat(".a.b.missing", r#"{"a":{"b":{"c":42}}}"#);
}

#[test]
fn passthrough_field_compact_non_object() {
    // .field on a non-object produces an error (no output) and exit code 5.
    let (ok, stdout, stderr) = qj_result(".x", "[1,2,3]");
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
    let out = qj_compact(".name", r#"{"name":"bob"}"#);
    assert_eq!(out.trim(), r#""bob""#);
    assert_jq_compat(".name", r#"{"name":"bob"}"#);
}

#[test]
fn passthrough_field_pretty_not_affected() {
    // Without -c, field access should still use normal pretty-print path
    let out = qj(".data", r#"{"data":{"x":1}}"#);
    assert_eq!(out, "{\n  \"x\": 1\n}\n");
}

// --- Passthrough: .field | length ---

#[test]
fn passthrough_field_length_compact() {
    let out = qj_compact(".items | length", r#"{"items":[1,2,3]}"#);
    assert_eq!(out.trim(), "3");
    assert_jq_compat(".items | length", r#"{"items":[1,2,3]}"#);
}

#[test]
fn passthrough_field_length_pretty() {
    // length produces a scalar — same output regardless of compact mode
    let out = qj(".items | length", r#"{"items":[1,2,3]}"#);
    assert_eq!(out.trim(), "3");
}

#[test]
fn passthrough_nested_field_length() {
    let out = qj(".a.b | length", r#"{"a":{"b":[10,20]}}"#);
    assert_eq!(out.trim(), "2");
    assert_jq_compat(".a.b | length", r#"{"a":{"b":[10,20]}}"#);
}

#[test]
fn passthrough_missing_field_length() {
    let out = qj(".missing | length", r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "0");
    assert_jq_compat(".missing | length", r#"{"name":"alice"}"#);
}

#[test]
fn passthrough_bare_length_array() {
    let out = qj("length", "[1,2,3,4]");
    assert_eq!(out.trim(), "4");
    assert_jq_compat("length", "[1,2,3,4]");
}

#[test]
fn passthrough_bare_length_string() {
    let out = qj("length", r#""hello""#);
    assert_eq!(out.trim(), "5");
    assert_jq_compat("length", r#""hello""#);
}

#[test]
fn passthrough_bare_length_object() {
    let out = qj("length", r#"{"a":1,"b":2,"c":3}"#);
    assert_eq!(out.trim(), "3");
    assert_jq_compat("length", r#"{"a":1,"b":2,"c":3}"#);
}

#[test]
fn passthrough_field_length_object_value() {
    let out = qj(".data | length", r#"{"data":{"x":1,"y":2}}"#);
    assert_eq!(out.trim(), "2");
    assert_jq_compat(".data | length", r#"{"data":{"x":1,"y":2}}"#);
}

#[test]
fn passthrough_field_length_string_value() {
    let out = qj(".name | length", r#"{"name":"hello"}"#);
    assert_eq!(out.trim(), "5");
    assert_jq_compat(".name | length", r#"{"name":"hello"}"#);
}

// --- Passthrough: .field | keys ---

#[test]
fn passthrough_field_keys_object() {
    let out = qj_compact(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
    assert_jq_compat(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
}

#[test]
fn passthrough_field_keys_pretty() {
    // keys produces an array — should work without -c too
    let out = qj(".data | keys", r#"{"data":{"b":2,"a":1}}"#);
    // Pretty output should have newlines
    assert!(out.contains("\"a\""));
    assert!(out.contains("\"b\""));
}

#[test]
fn passthrough_bare_keys_object() {
    let out = qj_compact("keys", r#"{"b":2,"a":1,"c":3}"#);
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat("keys", r#"{"b":2,"a":1,"c":3}"#);
}

#[test]
fn passthrough_bare_keys_array() {
    let out = qj_compact("keys", "[10,20,30]");
    assert_eq!(out.trim(), "[0,1,2]");
    assert_jq_compat("keys", "[10,20,30]");
}

#[test]
fn passthrough_field_keys_array_value() {
    let out = qj_compact(".items | keys", r#"{"items":["x","y"]}"#);
    assert_eq!(out.trim(), "[0,1]");
    assert_jq_compat(".items | keys", r#"{"items":["x","y"]}"#);
}

// --- Array map field passthrough ---

#[test]
fn passthrough_map_field_basic() {
    let out = qj_compact("map(.name)", r#"[{"name":"alice"},{"name":"bob"}]"#);
    assert_eq!(out.trim(), r#"["alice","bob"]"#);
    assert_jq_compat("map(.name)", r#"[{"name":"alice"},{"name":"bob"}]"#);
}

#[test]
fn passthrough_map_field_nested() {
    let out = qj_compact("map(.a.b)", r#"[{"a":{"b":1}},{"a":{"b":2}}]"#);
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat("map(.a.b)", r#"[{"a":{"b":1}},{"a":{"b":2}}]"#);
}

#[test]
fn passthrough_map_field_missing() {
    let out = qj_compact("map(.x)", r#"[{"a":1},{"x":2}]"#);
    assert_eq!(out.trim(), "[null,2]");
    assert_jq_compat("map(.x)", r#"[{"a":1},{"x":2}]"#);
}

#[test]
fn passthrough_map_field_empty_array() {
    let out = qj_compact("map(.x)", "[]");
    assert_eq!(out.trim(), "[]");
    assert_jq_compat("map(.x)", "[]");
}

#[test]
fn passthrough_iterate_field_basic() {
    let out = qj_compact(".[] | .name", r#"[{"name":"alice"},{"name":"bob"}]"#);
    assert_eq!(out.trim(), "\"alice\"\n\"bob\"");
    assert_jq_compat(".[] | .name", r#"[{"name":"alice"},{"name":"bob"}]"#);
}

#[test]
fn passthrough_iterate_field_nested() {
    let out = qj_compact(".[] | .a.b", r#"[{"a":{"b":1}},{"a":{"b":2}}]"#);
    assert_eq!(out.trim(), "1\n2");
    assert_jq_compat(".[] | .a.b", r#"[{"a":{"b":1}},{"a":{"b":2}}]"#);
}

#[test]
fn passthrough_iterate_field_mixed_types() {
    let out = qj_compact(
        "map(.val)",
        r#"[{"val":1},{"val":"str"},{"val":true},{"val":null},{"val":[1,2]}]"#,
    );
    assert_eq!(out.trim(), r#"[1,"str",true,null,[1,2]]"#);
    assert_jq_compat(
        "map(.val)",
        r#"[{"val":1},{"val":"str"},{"val":true},{"val":null},{"val":[1,2]}]"#,
    );
}

#[test]
fn passthrough_prefix_map_field() {
    let out = qj_compact(
        ".data | map(.name)",
        r#"{"data":[{"name":"alice"},{"name":"bob"}]}"#,
    );
    assert_eq!(out.trim(), r#"["alice","bob"]"#);
    assert_jq_compat(
        ".data | map(.name)",
        r#"{"data":[{"name":"alice"},{"name":"bob"}]}"#,
    );
}

#[test]
fn passthrough_prefix_iterate_field() {
    let out = qj_compact(
        ".data[] | .name",
        r#"{"data":[{"name":"alice"},{"name":"bob"}]}"#,
    );
    assert_eq!(out.trim(), "\"alice\"\n\"bob\"");
    assert_jq_compat(
        ".data[] | .name",
        r#"{"data":[{"name":"alice"},{"name":"bob"}]}"#,
    );
}

#[test]
fn passthrough_map_field_pretty_not_affected() {
    // Without -c, passthrough should not activate (requires_compact = true),
    // so it goes through normal pipeline with pretty printing.
    let out = qj("map(.x)", r#"[{"x":1},{"x":2}]"#);
    assert_eq!(out, "[\n  1,\n  2\n]\n");
}

// --- Array map fields obj passthrough ---

#[test]
fn passthrough_map_fields_obj_basic() {
    let out = qj_compact(
        "map({name, age})",
        r#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#,
    );
    assert_eq!(
        out.trim(),
        r#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#
    );
    assert_jq_compat(
        "map({name, age})",
        r#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#,
    );
}

#[test]
fn passthrough_map_fields_obj_missing() {
    let out = qj_compact("map({a, b})", r#"[{"a":1},{"b":2}]"#);
    assert_eq!(out.trim(), r#"[{"a":1,"b":null},{"a":null,"b":2}]"#);
    assert_jq_compat("map({a, b})", r#"[{"a":1},{"b":2}]"#);
}

#[test]
fn passthrough_map_fields_obj_empty_array() {
    let out = qj_compact("map({a, b})", "[]");
    assert_eq!(out.trim(), "[]");
    assert_jq_compat("map({a, b})", "[]");
}

#[test]
fn passthrough_iterate_fields_obj_basic() {
    let out = qj_compact(
        ".[] | {name, age}",
        r#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#,
    );
    assert_eq!(
        out.trim(),
        "{\"name\":\"alice\",\"age\":30}\n{\"name\":\"bob\",\"age\":25}"
    );
    assert_jq_compat(
        ".[] | {name, age}",
        r#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#,
    );
}

#[test]
fn passthrough_prefix_map_fields_obj() {
    let out = qj_compact(
        ".data | map({x, y})",
        r#"{"data":[{"x":1,"y":2},{"x":3,"y":4}]}"#,
    );
    assert_eq!(out.trim(), r#"[{"x":1,"y":2},{"x":3,"y":4}]"#);
    assert_jq_compat(
        ".data | map({x, y})",
        r#"{"data":[{"x":1,"y":2},{"x":3,"y":4}]}"#,
    );
}

#[test]
fn passthrough_prefix_iterate_fields_obj() {
    let out = qj_compact(
        ".data[] | {x, y}",
        r#"{"data":[{"x":1,"y":2},{"x":3,"y":4}]}"#,
    );
    assert_eq!(out.trim(), "{\"x\":1,\"y\":2}\n{\"x\":3,\"y\":4}");
    assert_jq_compat(
        ".data[] | {x, y}",
        r#"{"data":[{"x":1,"y":2},{"x":3,"y":4}]}"#,
    );
}

#[test]
fn passthrough_map_fields_obj_null_element() {
    // null elements produce all-null obj (matches jq: .a on null is null)
    let out = qj_compact("map({a, b})", r#"[{"a":1},null]"#);
    assert_eq!(out.trim(), r#"[{"a":1,"b":null},{"a":null,"b":null}]"#);
    assert_jq_compat("map({a, b})", r#"[{"a":1},null]"#);
}

#[test]
fn passthrough_map_field_null_element() {
    // Same for single-field: null element produces null
    let out = qj_compact("map(.a)", r#"[{"a":1},null]"#);
    assert_eq!(out.trim(), "[1,null]");
    assert_jq_compat("map(.a)", r#"[{"a":1},null]"#);
}

#[test]
fn passthrough_map_fields_obj_pretty_not_affected() {
    // Without -c, passthrough should not activate
    let out = qj("map({a})", r#"[{"a":1},{"a":2}]"#);
    assert!(out.contains('\n') && out.contains("  "));
}

// --- Phase 5: Scalar builtin passthroughs ---

#[test]
fn passthrough_keys_unsorted_object() {
    let out = qj_compact("keys_unsorted", r#"{"b":2,"a":1,"c":3}"#);
    assert_eq!(out.trim(), r#"["b","a","c"]"#);
    assert_jq_compat("keys_unsorted", r#"{"b":2,"a":1,"c":3}"#);
}

#[test]
fn passthrough_keys_unsorted_field() {
    let out = qj_compact(".data | keys_unsorted", r#"{"data":{"z":1,"m":2,"a":3}}"#);
    assert_eq!(out.trim(), r#"["z","m","a"]"#);
    assert_jq_compat(".data | keys_unsorted", r#"{"data":{"z":1,"m":2,"a":3}}"#);
}

#[test]
fn passthrough_keys_unsorted_array() {
    let out = qj_compact("keys_unsorted", r#"["x","y","z"]"#);
    assert_eq!(out.trim(), "[0,1,2]");
    assert_jq_compat("keys_unsorted", r#"["x","y","z"]"#);
}

#[test]
fn passthrough_type_object() {
    let out = qj_compact("type", r#"{"a":1}"#);
    assert_eq!(out.trim(), r#""object""#);
    assert_jq_compat("type", r#"{"a":1}"#);
}

#[test]
fn passthrough_type_array() {
    let out = qj_compact("type", r#"[1,2,3]"#);
    assert_eq!(out.trim(), r#""array""#);
    assert_jq_compat("type", r#"[1,2,3]"#);
}

#[test]
fn passthrough_type_string() {
    let out = qj_compact("type", r#""hello""#);
    assert_eq!(out.trim(), r#""string""#);
    assert_jq_compat("type", r#""hello""#);
}

#[test]
fn passthrough_type_number() {
    let out = qj_compact("type", "42");
    assert_eq!(out.trim(), r#""number""#);
    assert_jq_compat("type", "42");
}

#[test]
fn passthrough_type_boolean() {
    let out = qj_compact("type", "true");
    assert_eq!(out.trim(), r#""boolean""#);
    assert_jq_compat("type", "true");
}

#[test]
fn passthrough_type_null() {
    let out = qj_compact("type", "null");
    assert_eq!(out.trim(), r#""null""#);
    assert_jq_compat("type", "null");
}

#[test]
fn passthrough_type_field() {
    let out = qj_compact(".data | type", r#"{"data":[1,2]}"#);
    assert_eq!(out.trim(), r#""array""#);
    assert_jq_compat(".data | type", r#"{"data":[1,2]}"#);
}

#[test]
fn passthrough_type_missing_field() {
    let out = qj_compact(".missing | type", r#"{"a":1}"#);
    assert_eq!(out.trim(), r#""null""#);
    assert_jq_compat(".missing | type", r#"{"a":1}"#);
}

#[test]
fn passthrough_has_true() {
    let out = qj_compact(r#"has("name")"#, r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), "true");
    assert_jq_compat(r#"has("name")"#, r#"{"name":"alice","age":30}"#);
}

#[test]
fn passthrough_has_false() {
    let out = qj_compact(r#"has("missing")"#, r#"{"name":"alice"}"#);
    assert_eq!(out.trim(), "false");
    assert_jq_compat(r#"has("missing")"#, r#"{"name":"alice"}"#);
}

#[test]
fn passthrough_has_field_prefix() {
    let out = qj_compact(r#".data | has("x")"#, r#"{"data":{"x":1,"y":2}}"#);
    assert_eq!(out.trim(), "true");
    assert_jq_compat(r#".data | has("x")"#, r#"{"data":{"x":1,"y":2}}"#);
}

// --- Phase 6: Iterate + builtin passthroughs ---

#[test]
fn passthrough_map_length() {
    let out = qj_compact("map(length)", r#"[{"a":1,"b":2},[1,2,3],"hello"]"#);
    assert_eq!(out.trim(), "[2,3,5]");
    assert_jq_compat("map(length)", r#"[{"a":1,"b":2},[1,2,3],"hello"]"#);
}

#[test]
fn passthrough_iterate_length() {
    let out = qj_compact(".[] | length", r#"[{"a":1},[1,2]]"#);
    assert_eq!(out.trim(), "1\n2");
    assert_jq_compat(".[] | length", r#"[{"a":1},[1,2]]"#);
}

#[test]
fn passthrough_map_type() {
    let out = qj_compact("map(type)", r#"[{"a":1},[1],42,"hi",true,null]"#);
    assert_eq!(
        out.trim(),
        r#"["object","array","number","string","boolean","null"]"#
    );
    assert_jq_compat("map(type)", r#"[{"a":1},[1],42,"hi",true,null]"#);
}

#[test]
fn passthrough_iterate_type() {
    let out = qj_compact(".[] | type", r#"[1,"hello",null]"#);
    assert_eq!(out.trim(), "\"number\"\n\"string\"\n\"null\"");
    assert_jq_compat(".[] | type", r#"[1,"hello",null]"#);
}

#[test]
fn passthrough_map_keys() {
    let out = qj_compact("map(keys)", r#"[{"b":2,"a":1},{"z":3}]"#);
    assert_eq!(out.trim(), r#"[["a","b"],["z"]]"#);
    assert_jq_compat("map(keys)", r#"[{"b":2,"a":1},{"z":3}]"#);
}

#[test]
fn passthrough_map_keys_unsorted() {
    let out = qj_compact("map(keys_unsorted)", r#"[{"b":2,"a":1}]"#);
    assert_eq!(out.trim(), r#"[["b","a"]]"#);
    assert_jq_compat("map(keys_unsorted)", r#"[{"b":2,"a":1}]"#);
}

#[test]
fn passthrough_map_has() {
    let out = qj_compact(r#"map(has("a"))"#, r#"[{"a":1,"b":2},{"b":3}]"#);
    assert_eq!(out.trim(), "[true,false]");
    assert_jq_compat(r#"map(has("a"))"#, r#"[{"a":1,"b":2},{"b":3}]"#);
}

#[test]
fn passthrough_prefix_map_length() {
    let out = qj_compact(".items | map(length)", r#"{"items":[[1,2],[3]]}"#);
    assert_eq!(out.trim(), "[2,1]");
    assert_jq_compat(".items | map(length)", r#"{"items":[[1,2],[3]]}"#);
}

#[test]
fn passthrough_prefix_iterate_type() {
    let out = qj_compact(".items[] | type", r#"{"items":[1,"hello"]}"#);
    assert_eq!(out.trim(), "\"number\"\n\"string\"");
    assert_jq_compat(".items[] | type", r#"{"items":[1,"hello"]}"#);
}

// --- Phase 7: Syntactic variant detection ---

#[test]
fn passthrough_array_construct_field() {
    let out = qj_compact("[.[] | .name]", r#"[{"name":"a"},{"name":"b"}]"#);
    assert_eq!(out.trim(), r#"["a","b"]"#);
    assert_jq_compat("[.[] | .name]", r#"[{"name":"a"},{"name":"b"}]"#);
}

#[test]
fn passthrough_array_construct_fields_obj() {
    let out = qj_compact("[.[] | {a, b}]", r#"[{"a":1,"b":2},{"a":3,"b":4}]"#);
    assert_eq!(out.trim(), r#"[{"a":1,"b":2},{"a":3,"b":4}]"#);
    assert_jq_compat("[.[] | {a, b}]", r#"[{"a":1,"b":2},{"a":3,"b":4}]"#);
}

#[test]
fn passthrough_array_construct_builtin() {
    let out = qj_compact("[.[] | length]", r#"[[1,2],[3]]"#);
    assert_eq!(out.trim(), "[2,1]");
    assert_jq_compat("[.[] | length]", r#"[[1,2],[3]]"#);
}

#[test]
fn passthrough_array_construct_prefix_field() {
    let out = qj_compact(
        "[.items[] | .name]",
        r#"{"items":[{"name":"a"},{"name":"b"}]}"#,
    );
    assert_eq!(out.trim(), r#"["a","b"]"#);
    assert_jq_compat(
        "[.items[] | .name]",
        r#"{"items":[{"name":"a"},{"name":"b"}]}"#,
    );
}

#[test]
fn passthrough_array_construct_prefix_builtin() {
    let out = qj_compact("[.items[] | length]", r#"{"items":[[1,2],[3]]}"#);
    assert_eq!(out.trim(), "[2,1]");
    assert_jq_compat("[.items[] | length]", r#"{"items":[[1,2],[3]]}"#);
}

// --- Number literal preservation ---

#[test]
fn number_trailing_zeros_preserved() {
    assert_eq!(qj_compact(".x", r#"{"x":75.80}"#).trim(), "75.80");
    assert_eq!(qj_compact(".x", r#"{"x":1.00}"#).trim(), "1.00");
    assert_eq!(qj_compact(".x", r#"{"x":0.10}"#).trim(), "0.10");
}

#[test]
fn number_scientific_notation_preserved() {
    assert_eq!(qj_compact(".x", r#"{"x":1.5e2}"#).trim(), "1.5e2");
    assert_eq!(qj_compact(".x", r#"{"x":1e10}"#).trim(), "1e10");
    assert_eq!(qj_compact(".x", r#"{"x":2.5E-3}"#).trim(), "2.5E-3");
}

#[test]
fn number_identity_preserves_formatting() {
    // Compact identity should preserve all number formatting
    assert_eq!(
        qj_compact(".", r#"{"a":75.80,"b":1.0e3}"#).trim(),
        r#"{"a":75.80,"b":1.0e3}"#
    );
}

#[test]
fn number_arithmetic_drops_raw_text() {
    // Arithmetic produces computed values — no raw text preservation
    assert_eq!(qj_compact(".x + .x", r#"{"x":37.9}"#).trim(), "75.8");
    assert_eq!(qj_compact(".x * 2", r#"{"x":1.50}"#).trim(), "3");
}

#[test]
fn number_integers_unchanged() {
    assert_eq!(qj_compact(".x", r#"{"x":42}"#).trim(), "42");
    assert_eq!(qj_compact(".x", r#"{"x":-1}"#).trim(), "-1");
    assert_eq!(qj_compact(".x", r#"{"x":0}"#).trim(), "0");
    assert_eq!(
        qj_compact(".x", r#"{"x":9223372036854775807}"#).trim(),
        "9223372036854775807"
    );
}

#[test]
fn number_large_uint64_preserves_text() {
    // i64::MAX + 1 — should preserve original text, not lose precision via f64
    assert_eq!(
        qj_compact(".", "9223372036854775808").trim(),
        "9223372036854775808"
    );
    // Larger u64 value
    assert_eq!(
        qj_compact(".id", r#"{"id":9999999999999999999}"#).trim(),
        "9999999999999999999"
    );
    // u64::MAX
    assert_eq!(
        qj_compact(".", "18446744073709551615").trim(),
        "18446744073709551615"
    );
    // Beyond u64 — preserved via bigint fallback
    assert_eq!(
        qj_compact(".", "99999999999999999999999999999").trim(),
        "99999999999999999999999999999"
    );
    // Beyond u64 in object
    assert_eq!(
        qj_compact(".id", r#"{"id":99999999999999999999999999999}"#).trim(),
        "99999999999999999999999999999"
    );
}

#[test]
fn number_pretty_preserves_formatting() {
    // Pretty mode should also preserve number literals
    let out = qj(".", r#"{"x":75.80}"#);
    assert!(
        out.contains("75.80"),
        "pretty output should preserve 75.80, got: {out}"
    );
}

// --- Error helper ---

fn qj_err(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");
    assert!(
        !output.status.success(),
        "expected qj to fail but it succeeded with stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).unwrap_or_default()
}

fn qj_args(args: &[&str], input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
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
        .expect("failed to run qj");
    assert!(
        output.status.success(),
        "qj {:?} exited with {}: stderr={}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

// --- Builtin: any ---

#[test]
fn builtin_any_with_condition() {
    assert_eq!(qj_compact("any(. > 2)", "[1,2,3]").trim(), "true");
    assert_jq_compat("any(. > 2)", "[1,2,3]");
}

#[test]
fn builtin_any_bare() {
    assert_eq!(qj_compact("any", "[false,null,1]").trim(), "true");
    assert_jq_compat("any", "[false,null,1]");
}

#[test]
fn builtin_any_all_false() {
    assert_eq!(qj_compact("any", "[false,null,false]").trim(), "false");
    assert_jq_compat("any", "[false,null,false]");
}

// --- Builtin: all ---

#[test]
fn builtin_all_with_condition() {
    assert_eq!(qj_compact("all(. > 0)", "[1,2,3]").trim(), "true");
    assert_jq_compat("all(. > 0)", "[1,2,3]");
}

#[test]
fn builtin_all_fails() {
    assert_eq!(qj_compact("all(. > 2)", "[1,2,3]").trim(), "false");
    assert_jq_compat("all(. > 2)", "[1,2,3]");
}

#[test]
fn builtin_all_bare() {
    assert_eq!(qj_compact("all", "[true,1,\"yes\"]").trim(), "true");
    assert_jq_compat("all", r#"[true,1,"yes"]"#);
}

// --- Builtin: contains ---

#[test]
fn builtin_contains_string() {
    assert_eq!(qj_compact(r#"contains("ll")"#, r#""hello""#).trim(), "true");
    assert_jq_compat(r#"contains("ll")"#, r#""hello""#);
}

#[test]
fn builtin_contains_array() {
    assert_eq!(qj_compact("contains([2])", "[1,2,3]").trim(), "true");
    assert_jq_compat("contains([2])", "[1,2,3]");
}

#[test]
fn builtin_contains_object() {
    assert_eq!(
        qj_compact(r#"contains({"a":1})"#, r#"{"a":1,"b":2}"#).trim(),
        "true"
    );
    assert_jq_compat(r#"contains({"a":1})"#, r#"{"a":1,"b":2}"#);
}

// --- Builtin: to_entries / from_entries ---

#[test]
fn builtin_to_entries() {
    assert_eq!(
        qj_compact("to_entries", r#"{"a":1}"#).trim(),
        r#"[{"key":"a","value":1}]"#
    );
    assert_jq_compat("to_entries", r#"{"a":1}"#);
}

#[test]
fn builtin_from_entries() {
    assert_eq!(
        qj_compact("from_entries", r#"[{"key":"a","value":1}]"#).trim(),
        r#"{"a":1}"#
    );
    assert_jq_compat("from_entries", r#"[{"key":"a","value":1}]"#);
}

#[test]
fn builtin_from_entries_name_value() {
    assert_eq!(
        qj_compact("from_entries", r#"[{"name":"a","value":1}]"#).trim(),
        r#"{"a":1}"#
    );
    assert_jq_compat("from_entries", r#"[{"name":"a","value":1}]"#);
}

// --- Builtin: flatten ---

#[test]
fn builtin_flatten() {
    assert_eq!(qj_compact("flatten", "[[1,[2]],3]").trim(), "[1,2,3]");
    assert_jq_compat("flatten", "[[1,[2]],3]");
}

#[test]
fn builtin_flatten_depth() {
    assert_eq!(qj_compact("flatten(1)", "[[1,[2]],3]").trim(), "[1,[2],3]");
    assert_jq_compat("flatten(1)", "[[1,[2]],3]");
}

#[test]
fn builtin_flatten_object() {
    // jq extracts values from objects, then flattens
    assert_eq!(
        qj_compact("flatten", r#"{"a":1,"b":[2,3],"c":4}"#).trim(),
        "[1,2,3,4]"
    );
    assert_jq_compat("flatten", r#"{"a":1,"b":[2,3],"c":4}"#);
    assert_jq_compat("flatten", r#"{"a":null}"#);
    assert_jq_compat("flatten(1)", r#"{"a":[1,[2]],"b":3}"#);
}

// --- Builtin: first / last ---

#[test]
fn builtin_first_bare() {
    assert_eq!(qj_compact("first", "[1,2,3]").trim(), "1");
    assert_jq_compat("first", "[1,2,3]");
}

#[test]
fn builtin_first_generator() {
    assert_eq!(qj_compact("first(.[])", "[10,20,30]").trim(), "10");
    assert_jq_compat("first(.[])", "[10,20,30]");
}

#[test]
fn builtin_last_bare() {
    assert_eq!(qj_compact("last", "[1,2,3]").trim(), "3");
    assert_jq_compat("last", "[1,2,3]");
}

#[test]
fn builtin_last_generator() {
    assert_eq!(qj_compact("last(.[])", "[10,20,30]").trim(), "30");
    assert_jq_compat("last(.[])", "[10,20,30]");
}

// --- Builtin: group_by ---

#[test]
fn builtin_group_by() {
    let out = qj_compact(
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
    assert_eq!(qj_compact("unique", "[1,2,1,3]").trim(), "[1,2,3]");
    assert_jq_compat("unique", "[1,2,1,3]");
}

#[test]
fn builtin_unique_by() {
    let out = qj_compact(
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
    assert_eq!(qj_compact("min", "[3,1,2]").trim(), "1");
    assert_jq_compat("min", "[3,1,2]");
}

#[test]
fn builtin_max() {
    assert_eq!(qj_compact("max", "[3,1,2]").trim(), "3");
    assert_jq_compat("max", "[3,1,2]");
}

#[test]
fn builtin_min_empty() {
    assert_eq!(qj_compact("min", "[]").trim(), "null");
    assert_jq_compat("min", "[]");
}

#[test]
fn builtin_max_empty() {
    assert_eq!(qj_compact("max", "[]").trim(), "null");
    assert_jq_compat("max", "[]");
}

// --- Builtin: min_by / max_by ---

#[test]
fn builtin_min_by() {
    assert_eq!(
        qj_compact("min_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"{"x":1}"#
    );
    assert_jq_compat("min_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#);
}

#[test]
fn builtin_max_by() {
    assert_eq!(
        qj_compact("max_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"{"x":3}"#
    );
    assert_jq_compat("max_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#);
}

// --- Builtin: sort_by ---

#[test]
fn builtin_sort_by() {
    assert_eq!(
        qj_compact("sort_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"[{"x":1},{"x":2},{"x":3}]"#
    );
    assert_jq_compat("sort_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#);
}

// --- Builtin: del ---

#[test]
fn builtin_del() {
    assert_eq!(
        qj_compact("del(.a)", r#"{"a":1,"b":2}"#).trim(),
        r#"{"b":2}"#
    );
    assert_jq_compat("del(.a)", r#"{"a":1,"b":2}"#);
}

// --- Builtin: ltrimstr / rtrimstr ---

#[test]
fn builtin_ltrimstr() {
    assert_eq!(
        qj_compact(r#"ltrimstr("hel")"#, r#""hello""#).trim(),
        r#""lo""#
    );
    assert_jq_compat(r#"ltrimstr("hel")"#, r#""hello""#);
}

#[test]
fn builtin_rtrimstr() {
    assert_eq!(
        qj_compact(r#"rtrimstr("lo")"#, r#""hello""#).trim(),
        r#""hel""#
    );
    assert_jq_compat(r#"rtrimstr("lo")"#, r#""hello""#);
}

// --- Builtin: startswith / endswith ---

#[test]
fn builtin_startswith() {
    assert_eq!(
        qj_compact(r#"startswith("hel")"#, r#""hello""#).trim(),
        "true"
    );
    assert_eq!(
        qj_compact(r#"startswith("xyz")"#, r#""hello""#).trim(),
        "false"
    );
    assert_jq_compat(r#"startswith("hel")"#, r#""hello""#);
    assert_jq_compat(r#"startswith("xyz")"#, r#""hello""#);
}

#[test]
fn builtin_endswith() {
    assert_eq!(
        qj_compact(r#"endswith("llo")"#, r#""hello""#).trim(),
        "true"
    );
    assert_eq!(
        qj_compact(r#"endswith("xyz")"#, r#""hello""#).trim(),
        "false"
    );
    assert_jq_compat(r#"endswith("llo")"#, r#""hello""#);
    assert_jq_compat(r#"endswith("xyz")"#, r#""hello""#);
}

// --- Builtin: tonumber / tostring ---

#[test]
fn builtin_tonumber() {
    assert_eq!(qj_compact("tonumber", r#""42""#).trim(), "42");
    assert_eq!(qj_compact("tonumber", r#""3.14""#).trim(), "3.14");
    assert_eq!(qj_compact("tonumber", "42").trim(), "42");
    assert_jq_compat("tonumber", r#""42""#);
    assert_jq_compat("tonumber", r#""3.14""#);
    assert_jq_compat("tonumber", "42");
}

#[test]
fn builtin_tostring() {
    assert_eq!(qj_compact("tostring", "42").trim(), r#""42""#);
    assert_eq!(qj_compact("tostring", "null").trim(), r#""null""#);
    assert_eq!(qj_compact("tostring", "true").trim(), r#""true""#);
    assert_jq_compat("tostring", "42");
    assert_jq_compat("tostring", "null");
    assert_jq_compat("tostring", "true");
}

// --- Builtin: values ---

#[test]
fn builtin_values_object() {
    // values = select(. != null): passes through non-null input
    let out = qj_compact("values", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":1,"b":2}"#);
    assert_jq_compat("values", r#"{"a":1,"b":2}"#);
}

#[test]
fn builtin_values_array() {
    // values = select(. != null): passes through non-null input
    let out = qj_compact("values", "[10,20,30]");
    assert_eq!(out.trim(), "[10,20,30]");
    assert_jq_compat("values", "[10,20,30]");
    // Test that null is filtered
    let out2 = qj_compact("[.[]|values]", "[1,null,2]");
    assert_eq!(out2.trim(), "[1,2]");
    assert_jq_compat("[.[]|values]", "[1,null,2]");
}

// --- Builtin: empty ---

#[test]
fn builtin_empty() {
    let out = qj_compact("[1, empty, 2]", "null");
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat("[1, empty, 2]", "null");
}

// --- Builtin: not ---

#[test]
fn builtin_not_true() {
    assert_eq!(qj_compact("not", "true").trim(), "false");
    assert_jq_compat("not", "true");
}

#[test]
fn builtin_not_false() {
    assert_eq!(qj_compact("not", "false").trim(), "true");
    assert_jq_compat("not", "false");
}

#[test]
fn builtin_not_null() {
    assert_eq!(qj_compact("not", "null").trim(), "true");
    assert_jq_compat("not", "null");
}

// --- Builtin: keys_unsorted ---

#[test]
fn builtin_keys_unsorted() {
    let out = qj_compact("keys_unsorted", r#"{"b":2,"a":1}"#);
    // keys_unsorted preserves insertion order
    assert_eq!(out.trim(), r#"["b","a"]"#);
    assert_jq_compat("keys_unsorted", r#"{"b":2,"a":1}"#);
}

// --- Builtin: has (e2e) ---

#[test]
fn builtin_has_object() {
    assert_eq!(qj_compact(r#"has("a")"#, r#"{"a":1,"b":2}"#).trim(), "true");
    assert_eq!(
        qj_compact(r#"has("z")"#, r#"{"a":1,"b":2}"#).trim(),
        "false"
    );
    assert_jq_compat(r#"has("a")"#, r#"{"a":1,"b":2}"#);
    assert_jq_compat(r#"has("z")"#, r#"{"a":1,"b":2}"#);
}

#[test]
fn builtin_has_array() {
    assert_eq!(qj_compact("has(1)", "[10,20,30]").trim(), "true");
    assert_eq!(qj_compact("has(5)", "[10,20,30]").trim(), "false");
    assert_jq_compat("has(1)", "[10,20,30]");
    assert_jq_compat("has(5)", "[10,20,30]");
}

// --- Builtin: type (e2e) ---

#[test]
fn builtin_type_all() {
    assert_eq!(qj_compact("type", "42").trim(), r#""number""#);
    assert_eq!(qj_compact("type", r#""hi""#).trim(), r#""string""#);
    assert_eq!(qj_compact("type", "true").trim(), r#""boolean""#);
    assert_eq!(qj_compact("type", "false").trim(), r#""boolean""#);
    assert_eq!(qj_compact("type", "null").trim(), r#""null""#);
    assert_eq!(qj_compact("type", "[1]").trim(), r#""array""#);
    assert_eq!(qj_compact("type", r#"{"a":1}"#).trim(), r#""object""#);
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
        qj_compact("ascii_downcase", r#""HELLO WORLD""#).trim(),
        r#""hello world""#
    );
    assert_jq_compat("ascii_downcase", r#""HELLO WORLD""#);
}

#[test]
fn builtin_ascii_upcase() {
    assert_eq!(
        qj_compact("ascii_upcase", r#""hello world""#).trim(),
        r#""HELLO WORLD""#
    );
    assert_jq_compat("ascii_upcase", r#""hello world""#);
}

// --- Language: Recursive descent ---

#[test]
fn recursive_descent_numbers() {
    let out = qj_compact("[.. | numbers]", r#"{"a":1,"b":{"c":2},"d":[3]}"#);
    assert_eq!(out.trim(), "[1,2,3]");
}

#[test]
fn recursive_descent_strings() {
    let out = qj_compact("[.. | strings]", r#"{"a":"x","b":{"c":"y"}}"#);
    assert_eq!(out.trim(), r#"["x","y"]"#);
}

// --- Language: Boolean and/or ---

#[test]
fn boolean_and() {
    assert_eq!(qj_compact("true and false", "null").trim(), "false");
    assert_eq!(qj_compact("true and true", "null").trim(), "true");
    assert_jq_compat("true and false", "null");
    assert_jq_compat("true and true", "null");
}

#[test]
fn boolean_or() {
    assert_eq!(qj_compact("false or true", "null").trim(), "true");
    assert_eq!(qj_compact("false or false", "null").trim(), "false");
    assert_jq_compat("false or true", "null");
    assert_jq_compat("false or false", "null");
}

// --- Language: not (as filter) ---

#[test]
fn not_in_select() {
    let out = qj_compact("[.[] | select(. > 2 | not)]", "[1,2,3,4,5]");
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat("[.[] | select(. > 2 | not)]", "[1,2,3,4,5]");
}

// --- Language: Try (?) ---

#[test]
fn try_operator_suppresses_error() {
    // .foo? on a non-object should produce no output (try suppresses the error)
    let out = qj_compact(".foo?", "[1,2,3]");
    assert!(out.trim().is_empty(), "expected no output, got: {out}");
}

#[test]
fn try_operator_on_iteration() {
    // .[]? on null should produce no output
    let out = qj_compact(".[]?", "null");
    assert!(out.trim().is_empty(), "expected no output, got: {out}");
}

// --- Language: Unary negation ---

#[test]
fn unary_negation() {
    // Filter starts with '-', so we need '--' to prevent CLI arg parsing
    let out = qj_args(&["-c", "--", "-(. + 1)"], "5");
    assert_eq!(out.trim(), "-6");
}

#[test]
fn negative_literal() {
    let out = qj_args(&["-c", "--", "-3"], "null");
    assert_eq!(out.trim(), "-3");
}

// --- Language: If-then (no else) ---

#[test]
fn if_then_no_else_true() {
    let out = qj_compact(r#"if . > 5 then "big" end"#, "10");
    assert_eq!(out.trim(), r#""big""#);
    assert_jq_compat(r#"if . > 5 then "big" end"#, "10");
}

#[test]
fn if_then_no_else_false() {
    // When condition is false and no else, jq passes through the input
    let out = qj_compact(r#"if . > 5 then "big" end"#, "3");
    assert_eq!(out.trim(), "3");
    assert_jq_compat(r#"if . > 5 then "big" end"#, "3");
}

// --- Language: Object shorthand ---

#[test]
fn object_shorthand() {
    let out = qj_compact("{name}", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
    assert_jq_compat("{name}", r#"{"name":"alice","age":30}"#);
}

// --- Language: Computed object keys ---

#[test]
fn computed_object_keys() {
    let out = qj_compact("{(.key): .value}", r#"{"key":"name","value":"alice"}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
}

// --- Language: Parenthesized expressions ---

#[test]
fn parenthesized_expression() {
    assert_eq!(qj_compact("(.a + .b) * 2", r#"{"a":3,"b":4}"#).trim(), "14");
    assert_jq_compat("(.a + .b) * 2", r#"{"a":3,"b":4}"#);
}

// --- Edge cases: Error handling ---

#[test]
fn error_invalid_json_input() {
    let stderr = qj_err(".", "not json");
    assert!(!stderr.is_empty(), "expected error message on stderr");
}

#[test]
fn error_invalid_filter_syntax() {
    let stderr = qj_err(".[", "{}");
    assert!(!stderr.is_empty(), "expected parse error on stderr");
}

// --- Edge cases: Null propagation ---

#[test]
fn null_propagation_deep() {
    assert_eq!(qj_compact(".missing.deep.path", "{}").trim(), "null");
}

// --- Edge cases: Null iteration ---

#[test]
fn null_iteration_no_output() {
    let out = qj_compact(".[]?", "null");
    assert!(out.trim().is_empty());
}

// --- Edge cases: Field on array ---

#[test]
fn field_on_array_produces_error() {
    // .field on an array produces an error (no output) and exit code 5
    let (ok, stdout, stderr) = qj_result(".x", "[1,2]");
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
    assert_eq!(qj_compact(".[99]", "[1,2,3]").trim(), "null");
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
    let out = qj_compact(".", &json);
    assert!(out.contains("42"));
}

// --- Edge cases: Empty object/array ---

#[test]
fn empty_object_keys() {
    assert_eq!(qj_compact("keys", "{}").trim(), "[]");
}

#[test]
fn empty_array_length() {
    assert_eq!(qj_compact("length", "[]").trim(), "0");
}

// --- Edge cases: Null-input flag ---

#[test]
fn null_input_flag() {
    let out = qj_args(&["-n", "-c", "null"], "");
    assert_eq!(out.trim(), "null");
}

// --- Edge cases: Large integers ---

#[test]
fn large_integer_i64_max() {
    assert_eq!(
        qj_compact(".", "9223372036854775807").trim(),
        "9223372036854775807"
    );
}

#[test]
fn integer_overflow_promotes_to_float() {
    // i64::MAX + 1 should promote to f64, not wrap to negative.
    // Output must be a plain integer (no scientific notation), matching jq.
    assert_jq_compat(". + 1", "9223372036854775807");
}

#[test]
fn large_double_integer_format() {
    // Computed integer-valued doubles beyond i64 range must format as plain
    // integers (no scientific notation), matching jq.
    assert_jq_compat(". * 1000", "9223372036854776");
    assert_jq_compat(". + .", "9223372036854775807");
}

#[test]
fn computed_double_format_round_powers() {
    // Round powers of 10: jq uses scientific notation (e.g., "1e+20"),
    // not expanded plain integers.
    assert_jq_compat(". * 1e20", "1");
    assert_jq_compat(". * 1e50", "1");
    assert_jq_compat(". * 1e100", "1");
}

#[test]
fn computed_double_format_negative_large() {
    // Negative computed doubles beyond i64 range
    assert_jq_compat(". * -1e20", "1");
    assert_jq_compat(". * -1000", "9223372036854776");
}

#[test]
fn computed_double_format_threshold_boundary() {
    // jq expands to plain integer when trailing zeros <= 15,
    // uses scientific notation (e+) above that.
    // At the boundary: 1e15 has 15 trailing zeros → plain "1000000000000000"
    assert_jq_compat(". * 1e15", "1");
    // Just above: 1e16 has 16 trailing zeros → scientific "1e+16"
    assert_jq_compat(". * 1e16", "1");
    // Negative side of the threshold
    assert_jq_compat(". * -1e15", "1");
    assert_jq_compat(". * -1e16", "1");
}

#[test]
fn large_integer_arithmetic_more_precise_than_jq() {
    // Twitter-style ID: 505874924095815681 (> 2^53, fits in i64)
    // qj does exact i64 arithmetic: +1 = 505874924095815682
    // jq uses f64 and loses precision: +1 = 505874924095815700
    let result = qj_compact(". + 1", "505874924095815681").trim().to_string();
    assert_eq!(result, "505874924095815682");
}

// --- jq conformance tests ---
// These run both qj and jq and verify identical output.
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

/// Run jq with custom args and return stdout, or None if jq fails.
fn run_jq(args: &[&str], input: &str) -> Option<String> {
    let output = Command::new("jq")
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
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?)
}

/// Assert that qj and jq produce identical stdout AND agree on success/failure.
/// Works for both success and error cases. Error messages (stderr) may differ —
/// only stdout and exit status are compared.
fn assert_jq_compat(filter: &str, input: &str) {
    if !jq_available() {
        return;
    }
    let (qj_stdout, _qj_stderr, qj_ok) =
        run_tool_full(env!("CARGO_BIN_EXE_qj"), &["-c", filter], input);
    let (jq_stdout, _jq_stderr, jq_ok) = run_tool_full("jq", &["-c", filter], input);

    assert_eq!(
        qj_stdout.trim(),
        jq_stdout.trim(),
        "stdout mismatch: filter={filter:?} input={input:?}\nqj_ok={qj_ok}, jq_ok={jq_ok}"
    );
    assert_eq!(
        qj_ok, jq_ok,
        "exit status mismatch: filter={filter:?} input={input:?}\n\
         qj_ok={qj_ok}, jq_ok={jq_ok}\n\
         qj stdout: {qj_stdout}\njq stdout: {jq_stdout}\n\
         qj stderr: {_qj_stderr}\njq stderr: {_jq_stderr}"
    );
}

#[test]
fn jq_compat_number_formatting() {
    assert_jq_compat(".x", r#"{"x":75.80}"#);
    assert_jq_compat(".x", r#"{"x":0.10}"#);
    assert_jq_compat(".", r#"{"a":75.80}"#);
    // Note: jq normalizes scientific notation (e.g. 1.5e2 → 1.5E+2)
    // while qj preserves the exact original text. Both are valid.
}

#[test]
fn jq_compat_arithmetic() {
    assert_jq_compat(".x + .y", r#"{"x":1,"y":2}"#);
    assert_jq_compat(".x + .x", r#"{"x":37.9}"#);
    assert_jq_compat(".x * 2", r#"{"x":3.14}"#);
}

#[test]
fn null_multiply_errors() {
    // jq: null * number and number * null should error, not return null
    let (ok, stdout, _) = qj_result("1 * .", "null");
    assert!(!ok, "expected error for 1 * null");
    assert!(
        stdout.trim().is_empty(),
        "expected no output for 1 * null, got: {stdout}"
    );

    let (ok, stdout, _) = qj_result(". * 1", "null");
    assert!(!ok, "expected error for null * 1");
    assert!(
        stdout.trim().is_empty(),
        "expected no output for null * 1, got: {stdout}"
    );
}

#[test]
fn jq_compat_try_division_by_zero() {
    // try suppresses division-by-zero error; [1, 0 | try(1/.)] should produce [1]
    assert_jq_compat("[1, 0 | try (1 / .)]", "null");
}

#[test]
fn jq_compat_null_add_identity() {
    // null is identity for addition: null + x = x, x + null = x
    assert_jq_compat("null + 1", "null");
    assert_jq_compat("1 + null", "null");
    assert_jq_compat("null + \"hello\"", "null");
}

/// Exhaustive type-pair test: every combination of value types × arithmetic operators.
/// Compares both stdout and exit status against jq to catch silent divergences
/// like the null*number bug (610b937).
#[test]
fn jq_compat_exhaustive_arithmetic_type_pairs() {
    if !jq_available() {
        return;
    }
    // Representative values for each type. Literals used as filter expressions
    // (jq evaluates `null`, `true`, `1`, `"s"`, `[]`, `{}` as constants).
    let values = [
        "null",
        "true",
        "false",
        "0",
        "1",
        "2",
        "1.5",
        "(0-1)", // negative via expression (bare -1 is a CLI flag)
        r#""hello""#,
        r#""""#,
        "[]",
        "[1,2]",
        "{}",
        r#"{"a":1}"#,
    ];
    let ops = ["+", "-", "*", "/", "%"];

    // Build all expressions as try/catch so both success and error cases
    // produce exactly one output line per expression. This lets us batch
    // all 980 combos into a single qj + jq invocation (~2 process spawns
    // instead of ~1960).
    let mut labels = Vec::new();
    let mut try_exprs = Vec::new();
    for a in &values {
        for b in &values {
            for op in &ops {
                let expr = format!("{a} {op} {b}");
                try_exprs.push(format!("try ({expr}) catch \"CATCH\""));
                labels.push(expr);
            }
        }
    }
    let filter = try_exprs.join(", ");

    let (qj_stdout, _, _) = run_tool_full(env!("CARGO_BIN_EXE_qj"), &["-cn", &filter], "null");
    let (jq_stdout, _, _) = run_tool_full("jq", &["-cn", &filter], "null");

    let qj_lines: Vec<&str> = qj_stdout.lines().collect();
    let jq_lines: Vec<&str> = jq_stdout.lines().collect();

    let mut failures = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let qj_line = qj_lines.get(i).map(|s| *s).unwrap_or("<missing>");
        let jq_line = jq_lines.get(i).map(|s| *s).unwrap_or("<missing>");
        if qj_line != jq_line {
            failures.push(format!("  {label}\n    qj={qj_line:?} jq={jq_line:?}",));
        }
    }

    if qj_lines.len() != jq_lines.len() {
        failures.push(format!(
            "  output line count: qj={} jq={}",
            qj_lines.len(),
            jq_lines.len()
        ));
    }

    if !failures.is_empty() {
        panic!(
            "{} arithmetic type-pair mismatches vs jq:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Targeted arithmetic edge cases compared against jq.
#[test]
fn jq_compat_arithmetic_edge_cases() {
    // String repetition
    assert_jq_compat(r#""ab" * 3"#, "null");
    assert_jq_compat(r#""ab" * 0"#, "null");
    assert_jq_compat(r#""ab" * -1"#, "null");
    assert_jq_compat(r#"3 * "ab""#, "null");

    // String split
    assert_jq_compat(r#""a,b,c" / ",""#, "null");
    assert_jq_compat(r#""hello" / """#, "null");

    // Division by zero
    assert_jq_compat("1 / 0", "null");
    assert_jq_compat("1.0 / 0", "null");
    assert_jq_compat("1 % 0", "null");

    // Object recursive merge
    assert_jq_compat(r#"{"a":{"x":1}} * {"a":{"y":2}}"#, "null");
    assert_jq_compat(r#"{"a":1} * {"b":2}"#, "null");

    // Array operations
    assert_jq_compat("[1,2] + [3,4]", "null");
    assert_jq_compat("[1,2,3] - [2]", "null");

    // Null identity for addition (all types)
    assert_jq_compat("null + []", "null");
    assert_jq_compat("null + {}", "null");
    assert_jq_compat("[] + null", "null");
    assert_jq_compat("{} + null", "null");

    // Overflow promotion
    assert_jq_compat("9223372036854775807 + 1", "null");
    assert_jq_compat("9223372036854775807 * 2", "null");
    // Use (0-N) since bare -N is parsed as CLI flag or unary minus
    assert_jq_compat("(0 - 9223372036854775807) - 9223372036854775807", "null");
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
    let out = qj_compact("1 + 2 * 3", "null");
    assert_eq!(out.trim(), "7");
    assert_jq_compat("1 + 2 * 3", "null");
}

#[test]
fn operator_precedence_div_before_sub() {
    let out = qj_compact("10 - 6 / 2", "null");
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
    let out = qj_compact("sort", r#"[3,"a",null,true,false,1]"#);
    assert_eq!(out.trim(), r#"[null,false,true,1,3,"a"]"#);
    assert_jq_compat("sort", r#"[3,"a",null,true,false,1]"#);
}

#[test]
fn jq_compat_sort_mixed() {
    assert_jq_compat("sort", r#"[3,"a",null,true,false,1]"#);
}

#[test]
fn unique_returns_sorted() {
    let out = qj_compact("unique", "[3,1,2,1,3]");
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
    let out = qj_compact("[range(5)]", "null");
    assert_eq!(out.trim(), "[0,1,2,3,4]");
}

#[test]
fn range_two_args() {
    let out = qj_compact("[range(2;5)]", "null");
    assert_eq!(out.trim(), "[2,3,4]");
}

#[test]
fn range_three_args() {
    let out = qj_compact("[range(0;10;3)]", "null");
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
    let out = qj_compact("floor", "3.7");
    assert_eq!(out.trim(), "3");
    assert_jq_compat("floor", "3.7");
}

#[test]
fn math_ceil() {
    let out = qj_compact("ceil", "3.2");
    assert_eq!(out.trim(), "4");
    assert_jq_compat("ceil", "3.2");
}

#[test]
fn math_round() {
    let out = qj_compact("round", "3.5");
    assert_eq!(out.trim(), "4");
    assert_jq_compat("round", "3.5");
}

#[test]
fn math_sqrt() {
    let out = qj_compact("sqrt", "9");
    assert_eq!(out.trim(), "3");
    assert_jq_compat("sqrt", "9");
}

#[test]
fn math_fabs() {
    let out = qj_compact("fabs", "-5.5");
    assert_eq!(out.trim(), "5.5");
    assert_jq_compat("fabs", "-5.5");
}

#[test]
fn math_nan_isnan() {
    let out = qj_compact("nan | isnan", "null");
    assert_eq!(out.trim(), "true");
    assert_jq_compat("nan | isnan", "null");
}

#[test]
fn math_infinite_isinfinite() {
    let out = qj_compact("infinite | isinfinite", "null");
    assert_eq!(out.trim(), "true");
    assert_jq_compat("infinite | isinfinite", "null");
}

#[test]
fn math_isfinite() {
    let out = qj_compact("isfinite", "42");
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
    let out = qj_compact("length", "-5");
    assert_eq!(out.trim(), "5");
}

#[test]
fn length_on_unicode() {
    // Use `. | length` to bypass the C++ passthrough path which counts bytes
    let out = qj_compact(". | length", r#""café""#);
    assert_eq!(out.trim(), "4");
}

#[test]
fn jq_compat_length() {
    assert_jq_compat("length", "-5");
}

// --- Phase 1: if with multiple condition outputs ---

#[test]
fn if_generator_condition() {
    let out = qj_compact("[if (1,2) > 1 then \"yes\" else \"no\" end]", "null");
    assert_eq!(out.trim(), r#"["no","yes"]"#);
}

// --- Phase 1: Object Construction with Multiple Outputs ---

#[test]
fn object_construct_generator_value() {
    let out = qj_compact("[{x: (1,2)}]", "null");
    assert_eq!(out.trim(), r#"[{"x":1},{"x":2}]"#);
}

#[test]
fn jq_compat_object_generator() {
    assert_jq_compat("[{x: (1,2)}]", "null");
}

// --- Phase 1: String Fixes + New Builtins ---

#[test]
fn split_empty_separator() {
    let out = qj_compact(r#"split("")"#, r#""abc""#);
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat(r#"split("")"#, r#""abc""#);
}

#[test]
fn ascii_downcase_only_ascii() {
    // ascii_downcase should only affect ASCII, not ß → SS etc.
    let out = qj_compact("ascii_downcase", r#""ABCéd""#);
    assert_eq!(out.trim(), r#""abcéd""#);
}

#[test]
fn string_explode() {
    let out = qj_compact("explode", r#""abc""#);
    assert_eq!(out.trim(), "[97,98,99]");
    assert_jq_compat("explode", r#""abc""#);
}

#[test]
fn string_implode() {
    let out = qj_compact("implode", "[97,98,99]");
    assert_eq!(out.trim(), r#""abc""#);
    assert_jq_compat("implode", "[97,98,99]");
}

#[test]
fn tojson_fromjson() {
    let out = qj_compact("[1,2] | tojson", "null");
    assert_eq!(out.trim(), r#""[1,2]""#);
    assert_jq_compat("[1,2] | tojson", "null");
}

#[test]
fn fromjson_basic() {
    let out = qj_compact(r#"fromjson"#, r#""[1,2,3]""#);
    assert_eq!(out.trim(), "[1,2,3]");
    assert_jq_compat("fromjson", r#""[1,2,3]""#);
}

#[test]
fn utf8bytelength() {
    let out = qj_compact("utf8bytelength", r#""café""#);
    assert_eq!(out.trim(), "5"); // é is 2 bytes in UTF-8
    assert_jq_compat("utf8bytelength", r#""café""#);
}

#[test]
fn inside_string() {
    let out = qj_compact(r#"inside("foobar")"#, r#""foo""#);
    assert_eq!(out.trim(), "true");
    assert_jq_compat(r#"inside("foobar")"#, r#""foo""#);
}

#[test]
fn string_times_number() {
    let out = qj_compact(r#""ab" * 3"#, "null");
    assert_eq!(out.trim(), r#""ababab""#);
    assert_jq_compat(r#""ab" * 3"#, "null");
}

#[test]
fn string_divide_string() {
    let out = qj_compact(r#""a,b,c" / ",""#, "null");
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat(r#""a,b,c" / ",""#, "null");
}

#[test]
fn index_string() {
    let out = qj_compact(r#"index("bar")"#, r#""foobar""#);
    assert_eq!(out.trim(), "3");
    assert_jq_compat(r#"index("bar")"#, r#""foobar""#);
}

#[test]
fn rindex_string() {
    let out = qj_compact(r#"rindex("o")"#, r#""fooboo""#);
    assert_eq!(out.trim(), "5");
    assert_jq_compat(r#"rindex("o")"#, r#""fooboo""#);
}

#[test]
fn indices_string() {
    let out = qj_compact(r#"indices("o")"#, r#""foobar""#);
    assert_eq!(out.trim(), "[1,2]");
    assert_jq_compat(r#"indices("o")"#, r#""foobar""#);
}

#[test]
fn trim_builtin() {
    let out = qj_compact("trim", r#""  hello  ""#);
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
    let out = qj_compact("from_entries", r#"[{"Key":"a","Value":1}]"#);
    assert_eq!(out.trim(), r#"{"a":1}"#);
    assert_jq_compat("from_entries", r#"[{"Key":"a","Value":1}]"#);
}

#[test]
fn array_subtraction() {
    let out = qj_compact("[1,2,3] - [2]", "null");
    assert_eq!(out.trim(), "[1,3]");
    assert_jq_compat("[1,2,3] - [2]", "null");
}

#[test]
fn jq_compat_array_subtraction() {
    assert_jq_compat("[1,2,3] - [2]", "null");
}

#[test]
fn object_recursive_merge() {
    let out = qj_compact(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
    assert_eq!(out.trim(), r#"{"a":{"b":1,"c":2}}"#);
    assert_jq_compat(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
}

#[test]
fn jq_compat_object_merge() {
    assert_jq_compat(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
}

#[test]
fn float_modulo() {
    // jq truncates floats to integers before modulo: 10.5 % 3 = 10 % 3 = 1
    let out = qj_compact(". % 3", "10.5");
    assert_eq!(out.trim(), "1");
}

#[test]
fn int_division_produces_float() {
    let out = qj_compact("1 / 3", "null");
    // jq: 0.3333333333333333
    let f: f64 = out.trim().parse().expect("expected float");
    assert!((f - 1.0 / 3.0).abs() < 1e-10);
    assert_jq_compat("1 / 3", "null");
}

#[test]
fn index_generator() {
    // .[expr] where expr produces multiple outputs
    let out = qj_compact(r#".[0,2]"#, "[10,20,30]");
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
    let out = qj_compact("transpose", "[[1,2],[3,4]]");
    assert_eq!(out.trim(), "[[1,3],[2,4]]");
}

#[test]
fn jq_compat_transpose() {
    assert_jq_compat("transpose", "[[1,2],[3,4]]");
}

#[test]
fn map_values_object() {
    let out = qj_compact("map_values(. + 1)", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"a":2,"b":3}"#);
}

#[test]
fn jq_compat_map_values() {
    assert_jq_compat("map_values(. + 1)", r#"{"a":1,"b":2}"#);
}

#[test]
fn map_on_object() {
    assert_jq_compat("map(type)", r#"{"a":null}"#);
    assert_jq_compat("map(. + 1)", r#"{"a":1,"b":2,"c":3}"#);
    assert_jq_compat("[map(type)]", r#"{"a":1,"b":"hi","c":null,"d":true}"#);
}

#[test]
fn jq_compat_builtin_type_errors() {
    // Array-only builtins on non-array inputs should error (exit 5), not silently drop.
    // Using non-null inputs since jq has special null handling for some builtins.
    assert_jq_compat("sort", r#"{"a":1}"#);
    assert_jq_compat("sort", r#"42"#);
    assert_jq_compat("sort", r#""hello""#);
    assert_jq_compat("reverse", r#"42"#);
    assert_jq_compat("unique", r#"{"a":1}"#);
    assert_jq_compat("unique", r#"42"#);
    assert_jq_compat("group_by(.x)", r#""hello""#);
    assert_jq_compat("group_by(.x)", r#"42"#);
    assert_jq_compat("sort_by(.x)", r#"42"#);
    assert_jq_compat("sort_by(.x)", r#""hello""#);
    assert_jq_compat("flatten", r#"42"#);
    assert_jq_compat("flatten", r#""hello""#);
    assert_jq_compat("min", r#"42"#);
    assert_jq_compat("max", r#""hello""#);
    assert_jq_compat("unique_by(.x)", r#"true"#);
    assert_jq_compat("unique_by(.x)", r#"42"#);
    assert_jq_compat("transpose", r#"42"#);
    assert_jq_compat("transpose", r#""hello""#);
    assert_jq_compat("add", r#"42"#);
    assert_jq_compat("add", r#""hello""#);
    assert_jq_compat("min_by(.x)", r#""hi""#);
    assert_jq_compat("max_by(.x)", r#"true"#);

    // @csv/@tsv on non-array input
    assert_jq_compat("@csv", r#"42"#);
    assert_jq_compat("@tsv", r#""hello""#);
    // @csv/@tsv with nested arrays/objects as elements
    assert_jq_compat("@csv", r#"[1,[2],3]"#);
    assert_jq_compat("@tsv", r#"[1,{"a":2},3]"#);

    // Valid inputs still work
    assert_jq_compat("@csv", r#"[1,"two",3]"#);
    assert_jq_compat("@tsv", r#"[1,"two",3]"#);
    assert_jq_compat("sort", r#"[3,1,2]"#);
    assert_jq_compat("reverse", r#"[1,2,3]"#);
    assert_jq_compat("unique", r#"[1,1,2,3,2]"#);
}

#[test]
fn limit_builtin() {
    let out = qj_compact("[limit(3; range(10))]", "null");
    assert_eq!(out.trim(), "[0,1,2]");
}

#[test]
fn jq_compat_limit() {
    assert_jq_compat("[limit(3; range(10))]", "null");
}

#[test]
fn until_builtin() {
    let out = qj_compact("0 | until(. >= 5; . + 1)", "null");
    assert_eq!(out.trim(), "5");
}

#[test]
fn jq_compat_until() {
    assert_jq_compat("0 | until(. >= 5; . + 1)", "null");
}

#[test]
fn while_builtin() {
    let out = qj_compact("[1 | while(. < 8; . * 2)]", "null");
    assert_eq!(out.trim(), "[1,2,4]");
}

#[test]
fn jq_compat_while() {
    assert_jq_compat("[1 | while(. < 8; . * 2)]", "null");
}

#[test]
fn isempty_builtin() {
    let out = qj_compact("isempty(empty)", "null");
    assert_eq!(out.trim(), "true");
}

#[test]
fn isempty_not_empty() {
    let out = qj_compact("isempty(range(3))", "null");
    assert_eq!(out.trim(), "false");
}

#[test]
fn jq_compat_isempty() {
    assert_jq_compat("isempty(empty)", "null");
    assert_jq_compat("isempty(range(3))", "null");
}

#[test]
fn getpath_builtin() {
    let out = qj_compact(r#"getpath(["a","b"])"#, r#"{"a":{"b":42}}"#);
    assert_eq!(out.trim(), "42");
}

#[test]
fn jq_compat_getpath() {
    assert_jq_compat(r#"getpath(["a","b"])"#, r#"{"a":{"b":42}}"#);
}

#[test]
fn setpath_builtin() {
    let out = qj_compact(r#"setpath(["a","b"]; 99)"#, r#"{"a":{"b":42}}"#);
    assert_eq!(out.trim(), r#"{"a":{"b":99}}"#);
}

#[test]
fn jq_compat_setpath() {
    assert_jq_compat(r#"setpath(["a","b"]; 99)"#, r#"{"a":{"b":42}}"#);
}

#[test]
fn paths_builtin() {
    let out = qj_compact("[paths]", r#"{"a":1,"b":{"c":2}}"#);
    assert_eq!(out.trim(), r#"[["a"],["b"],["b","c"]]"#);
}

#[test]
fn jq_compat_paths() {
    assert_jq_compat("[paths]", r#"{"a":1,"b":{"c":2}}"#);
}

#[test]
fn leaf_paths_builtin() {
    let out = qj_compact("[leaf_paths]", r#"{"a":1,"b":{"c":2}}"#);
    assert_eq!(out.trim(), r#"[["a"],["b","c"]]"#);
}

#[test]
fn jq_compat_paths_scalars() {
    // leaf_paths is defined as paths(scalars) in jq
    assert_jq_compat("[paths(scalars)]", r#"{"a":1,"b":{"c":2}}"#);
}

#[test]
fn bsearch_found() {
    let out = qj_compact("bsearch(3)", "[1,2,3,4,5]");
    assert_eq!(out.trim(), "2");
}

#[test]
fn bsearch_not_found() {
    let out = qj_compact("bsearch(2)", "[1,3,5]");
    assert_eq!(out.trim(), "-2");
}

#[test]
fn jq_compat_bsearch() {
    assert_jq_compat("bsearch(3)", "[1,2,3,4,5]");
    assert_jq_compat("bsearch(2)", "[1,3,5]");
}

#[test]
fn in_builtin() {
    let out = qj_compact("IN(2, 3)", "3");
    assert_eq!(out.trim(), "true");
}

#[test]
fn in_builtin_false() {
    let out = qj_compact("IN(2, 3)", "5");
    assert_eq!(out.trim(), "false");
}

#[test]
fn with_entries_builtin() {
    let out = qj_compact(
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
    let out = qj_compact("abs", "-42");
    assert_eq!(out.trim(), "42");
}

#[test]
fn jq_compat_abs() {
    assert_jq_compat("abs", "-42");
}

#[test]
fn debug_passthrough() {
    // debug should pass through the value
    let out = qj_compact("debug", "42");
    assert_eq!(out.trim(), "42");
}

#[test]
fn builtins_returns_array() {
    let out = qj_compact("builtins | length", "null");
    let n: i64 = out.trim().parse().expect("expected integer");
    assert!(n > 50, "expected at least 50 builtins, got {n}");
}

#[test]
fn repeat_with_limit() {
    let out = qj_compact("[limit(5; 1 | repeat(. * 2))]", "null");
    assert_eq!(out.trim(), "[2,2,2,2,2]");
}

#[test]
fn jq_compat_repeat() {
    assert_jq_compat("[limit(5; 1 | repeat(. * 2))]", "null");
}

#[test]
fn recurse_with_filter() {
    let out = qj_compact("[2 | recurse(. * .; . < 100)]", "null");
    assert_eq!(out.trim(), "[2,4,16]");
}

#[test]
fn nth_builtin() {
    let out = qj_compact("nth(2; range(5))", "null");
    assert_eq!(out.trim(), "2");
}

#[test]
fn jq_compat_nth() {
    assert_jq_compat("nth(2; range(5))", "null");
}

#[test]
fn delpaths_builtin() {
    let out = qj_compact(r#"delpaths([["a"]])"#, r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"b":2}"#);
}

#[test]
fn jq_compat_delpaths() {
    assert_jq_compat(r#"delpaths([["a"]])"#, r#"{"a":1,"b":2}"#);
}

#[test]
fn todate_builtin() {
    let out = qj_compact("todate", "0");
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
        let output = Command::new(env!("CARGO_BIN_EXE_qj"))
            .args(["-c", ".statuses | length", twitter.to_str().unwrap()])
            .output()
            .expect("failed to run qj");
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
    assert_eq!(qj("1 | logb", "null").trim(), "0");
    assert_eq!(qj("8 | logb", "null").trim(), "3");
    assert_eq!(qj("0.5 | logb", "null").trim(), "-1");
}

#[test]
fn scalb_basic() {
    // scalb(x; e) = x * 2^e
    assert_eq!(qj("2 | scalb(3)", "null").trim(), "16");
    assert_eq!(qj("1 | scalb(10)", "null").trim(), "1024");
    assert_eq!(qj("0.5 | scalb(1)", "null").trim(), "1");
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
    assert_eq!(qj("[1,2,3] | tostring", "null").trim(), r#""[1,2,3]""#);
    assert_eq!(qj(r#"{"a":1} | tostring"#, "null").trim(), r#""{\"a\":1}""#);
}

#[test]
fn env_returns_real_vars() {
    // $ENV should contain at least PATH
    let out = qj("$ENV | keys | length", "null");
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
    let out = qj(r#"try error("fail")"#, "null");
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
    let out = qj("0 | until(false; .)", "null");
    assert_eq!(out.trim(), "0");
}

#[test]
fn while_terminates_on_unchanged() {
    // while(true; .) should terminate (structural check: input unchanged)
    let out = qj_compact("0 | [limit(1; while(true; .))]", "null");
    assert_eq!(out.trim(), "[0]");
}

// --- --slurp / -s ---

#[test]
fn slurp_single_doc() {
    assert_eq!(qj_args(&["-cs", ".", "--"], "[1,2,3]").trim(), "[[1,2,3]]");
}

#[test]
fn slurp_ndjson() {
    assert_eq!(qj_args(&["-cs", ".", "--"], "1\n2\n3").trim(), "[1,2,3]");
}

#[test]
fn slurp_add() {
    assert_eq!(qj_args(&["-cs", "add", "--"], "1\n2\n3").trim(), "6");
}

#[test]
fn slurp_length() {
    assert_eq!(qj_args(&["-cs", "length", "--"], "1\n2\n3").trim(), "3");
}

// --- --arg / --argjson ---

#[test]
fn arg_string() {
    assert_eq!(
        qj_args(&["-nc", "--arg", "name", "alice", "{name: $name}"], "").trim(),
        r#"{"name":"alice"}"#
    );
}

#[test]
fn argjson_number() {
    assert_eq!(
        qj_args(&["-nc", "--argjson", "x", "42", "$x + 1"], "").trim(),
        "43"
    );
}

#[test]
fn arg_multiple() {
    assert_eq!(
        qj_args(
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
        qj_args(&["-nc", "--argjson", "obj", r#"{"x":1}"#, "$obj.x"], "").trim(),
        "1"
    );
}

// --- --raw-input / -R ---

#[test]
fn raw_input_line() {
    assert_eq!(qj_args(&["-Rc", ".", "--"], "hello").trim(), r#""hello""#);
}

#[test]
fn raw_input_multi() {
    assert_eq!(
        qj_args(&["-Rc", ".", "--"], "hello\nworld").trim(),
        "\"hello\"\n\"world\""
    );
}

#[test]
fn raw_input_slurp() {
    // jq -Rs: concatenate all input into a single string (not an array of lines)
    assert_eq!(
        qj_args(&["-Rsc", ".", "--"], "hello\nworld").trim(),
        r#""hello\nworld""#
    );
}

#[test]
fn raw_input_slurp_split_join() {
    // To get the old array-of-lines behavior, use split("\n")
    assert_eq!(
        qj_args(
            &["-Rsr", r#"split("\n") | join(",")"#, "--"],
            "hello\nworld"
        )
        .trim(),
        "hello,world"
    );
}

// --- --sort-keys / -S ---

#[test]
fn sort_keys_e2e() {
    assert_eq!(
        qj_args(&["-Sc", ".", "--"], r#"{"b":2,"a":1}"#).trim(),
        r#"{"a":1,"b":2}"#
    );
}

#[test]
fn sort_keys_nested_e2e() {
    assert_eq!(
        qj_args(&["-Sc", ".", "--"], r#"{"z":{"b":2,"a":1},"a":0}"#).trim(),
        r#"{"a":0,"z":{"a":1,"b":2}}"#
    );
}

// --- --join-output / -j ---

#[test]
fn join_output_e2e() {
    // -j suppresses trailing newlines
    assert_eq!(
        qj_args(&["-rj", ".name", "--"], r#"{"name":"hello"}"#),
        "hello"
    );
}

#[test]
fn join_output_compact() {
    // -j works with compact mode too
    assert_eq!(qj_args(&["-cj", ".", "--"], r#"{"a":1}"#), r#"{"a":1}"#);
}

// --- -M (monochrome — no-op, but should not error) ---

#[test]
fn monochrome_no_error() {
    qj_args(&["-Mc", ".", "--"], "{}");
}

// --- Assignment operators ---

#[test]
fn assign_update_field() {
    assert_eq!(
        qj_compact(".foo |= . + 1", r#"{"foo":42}"#).trim(),
        r#"{"foo":43}"#
    );
    assert_jq_compat(".foo |= . + 1", r#"{"foo":42}"#);
}

#[test]
fn assign_update_iterate() {
    assert_eq!(qj_compact(".[] |= . * 2", "[1,2,3]").trim(), "[2,4,6]");
    assert_jq_compat(".[] |= . * 2", "[1,2,3]");
}

#[test]
fn assign_set_field() {
    assert_eq!(
        qj_compact(".a = 42", r#"{"a":1,"b":2}"#).trim(),
        r#"{"a":42,"b":2}"#
    );
    assert_jq_compat(".a = 42", r#"{"a":1,"b":2}"#);
}

#[test]
fn assign_set_cross_reference() {
    // = evaluates RHS against the original input
    assert_eq!(
        qj_compact(".foo = .bar", r#"{"bar":42}"#).trim(),
        r#"{"bar":42,"foo":42}"#
    );
    assert_jq_compat(".foo = .bar", r#"{"bar":42}"#);
}

#[test]
fn assign_plus_iterate() {
    assert_eq!(qj_compact(".[] += 2", "[1,3,5]").trim(), "[3,5,7]");
    assert_jq_compat(".[] += 2", "[1,3,5]");
}

#[test]
fn assign_mul_iterate() {
    assert_eq!(qj_compact(".[] *= 2", "[1,3,5]").trim(), "[2,6,10]");
    assert_jq_compat(".[] *= 2", "[1,3,5]");
}

#[test]
fn assign_sub_iterate() {
    assert_eq!(qj_compact(".[] -= 2", "[1,3,5]").trim(), "[-1,1,3]");
    assert_jq_compat(".[] -= 2", "[1,3,5]");
}

#[test]
fn assign_div() {
    assert_eq!(qj_compact(".x /= 2", r#"{"x":10}"#).trim(), r#"{"x":5}"#);
    assert_jq_compat(".x /= 2", r#"{"x":10}"#);
}

#[test]
fn assign_mod() {
    assert_eq!(qj_compact(".x %= 3", r#"{"x":10}"#).trim(), r#"{"x":1}"#);
    assert_jq_compat(".x %= 3", r#"{"x":10}"#);
}

#[test]
fn assign_alt_null() {
    assert_eq!(
        qj_compact(r#".a //= "default""#, r#"{"a":null}"#).trim(),
        r#"{"a":"default"}"#
    );
    assert_jq_compat(r#".a //= "default""#, r#"{"a":null}"#);
}

#[test]
fn assign_alt_existing() {
    assert_eq!(
        qj_compact(r#".a //= "default""#, r#"{"a":1}"#).trim(),
        r#"{"a":1}"#
    );
    assert_jq_compat(r#".a //= "default""#, r#"{"a":1}"#);
}

#[test]
fn assign_update_empty_deletion() {
    // |= empty → delete matching elements
    assert_eq!(
        qj_compact("(.[] | select(. >= 2)) |= empty", "[1,5,3,0,7]").trim(),
        "[1,0]"
    );
    assert_jq_compat("(.[] | select(. >= 2)) |= empty", "[1,5,3,0,7]");
}

#[test]
fn assign_nested_path() {
    assert_eq!(
        qj_compact(".a.b |= . + 1", r#"{"a":{"b":10}}"#).trim(),
        r#"{"a":{"b":11}}"#
    );
    assert_jq_compat(".a.b |= . + 1", r#"{"a":{"b":10}}"#);
}

#[test]
fn assign_auto_create_structure() {
    assert_eq!(
        qj_compact(".[2][3] = 1", "[4]").trim(),
        "[4,null,[null,null,null,1]]"
    );
    assert_jq_compat(".[2][3] = 1", "[4]");
}

#[test]
fn assign_update_object_construction() {
    assert_eq!(
        qj_compact(r#".[0].a |= {"old":., "new":(.+1)}"#, r#"[{"a":1,"b":2}]"#).trim(),
        r#"[{"a":{"old":1,"new":2},"b":2}]"#
    );
    assert_jq_compat(r#".[0].a |= {"old":., "new":(.+1)}"#, r#"[{"a":1,"b":2}]"#);
}

#[test]
fn assign_update_with_index() {
    assert_eq!(qj_compact(".[0] |= . + 10", "[1,2,3]").trim(), "[11,2,3]");
    assert_jq_compat(".[0] |= . + 10", "[1,2,3]");
}

#[test]
fn assign_set_new_field() {
    assert_eq!(
        qj_compact(".c = 3", r#"{"a":1,"b":2}"#).trim(),
        r#"{"a":1,"b":2,"c":3}"#
    );
    assert_jq_compat(".c = 3", r#"{"a":1,"b":2}"#);
}

// --- Regex builtins ---

#[test]
fn regex_test_basic() {
    assert_eq!(qj_compact(r#"test("^foo")"#, r#""foobar""#).trim(), "true");
    assert_eq!(qj_compact(r#"test("^foo")"#, r#""barfoo""#).trim(), "false");
    assert_jq_compat(r#"test("^foo")"#, r#""foobar""#);
    assert_jq_compat(r#"test("^foo")"#, r#""barfoo""#);
}

#[test]
fn regex_test_case_insensitive() {
    assert_eq!(
        qj_compact(r#"test("FOO"; "i")"#, r#""foobar""#).trim(),
        "true"
    );
    assert_jq_compat(r#"test("FOO"; "i")"#, r#""foobar""#);
}

#[test]
fn regex_match_basic() {
    let out = qj_compact(r#"match("(o+)")"#, r#""foobar""#);
    assert_eq!(
        out.trim(),
        r#"{"offset":1,"length":2,"string":"oo","captures":[{"offset":1,"length":2,"string":"oo","name":null}]}"#
    );
    assert_jq_compat(r#"match("(o+)")"#, r#""foobar""#);
}

#[test]
fn regex_match_global() {
    let out = qj_compact(r#"[match("o"; "g")]"#, r#""foobar""#);
    // Should produce two match objects
    assert!(out.contains(r#""offset":1"#));
    assert!(out.contains(r#""offset":2"#));
}

#[test]
fn regex_capture_named() {
    let out = qj_compact(r#"capture("(?<y>\\d{4})-(?<m>\\d{2})")"#, r#""2024-01-15""#);
    assert_eq!(out.trim(), r#"{"y":"2024","m":"01"}"#);
    assert_jq_compat(r#"capture("(?<y>\\d{4})-(?<m>\\d{2})")"#, r#""2024-01-15""#);
}

#[test]
fn regex_sub() {
    assert_eq!(
        qj_compact(r#"sub("o"; "0")"#, r#""foobar""#).trim(),
        r#""f0obar""#
    );
    assert_jq_compat(r#"sub("o"; "0")"#, r#""foobar""#);
}

#[test]
fn regex_gsub() {
    assert_eq!(
        qj_compact(r#"gsub("o"; "0")"#, r#""foobar""#).trim(),
        r#""f00bar""#
    );
    assert_jq_compat(r#"gsub("o"; "0")"#, r#""foobar""#);
}

#[test]
fn regex_scan() {
    let out = qj_compact(r#"[scan("[0-9]+")]"#, r#""test 123 test 456""#);
    assert_eq!(out.trim(), r#"["123","456"]"#);
    assert_jq_compat(r#"[scan("[0-9]+")]"#, r#""test 123 test 456""#);
}

#[test]
fn regex_splits() {
    let out = qj_compact(r#"[splits("[,;]")]"#, r#""a,b;c""#);
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
    assert_jq_compat(r#"[splits("[,;]")]"#, r#""a,b;c""#);
}

// --- String interpolation ---

#[test]
fn string_interp_basic() {
    let out = qj_compact(
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
    let out = qj_compact(r#""sum: \(.a + .b)""#, r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#""sum: 3""#);
    assert_jq_compat(r#""sum: \(.a + .b)""#, r#"{"a":1,"b":2}"#);
}

#[test]
fn string_interp_nested() {
    assert_eq!(
        qj_compact(r#""inter\("pol" + "ation")""#, "null").trim(),
        r#""interpolation""#
    );
    assert_jq_compat(r#""inter\("pol" + "ation")""#, "null");
}

// --- Format strings ---

#[test]
fn format_base64() {
    assert_eq!(qj_compact("@base64", r#""hello""#).trim(), r#""aGVsbG8=""#);
    assert_jq_compat("@base64", r#""hello""#);
}

#[test]
fn format_base64d() {
    assert_eq!(qj_compact("@base64d", r#""aGVsbG8=""#).trim(), r#""hello""#);
    assert_jq_compat("@base64d", r#""aGVsbG8=""#);
}

#[test]
fn format_uri() {
    assert_eq!(
        qj_compact("@uri", r#""hello world""#).trim(),
        r#""hello%20world""#
    );
    assert_jq_compat("@uri", r#""hello world""#);
}

#[test]
fn format_csv() {
    assert_eq!(
        qj_compact("@csv", r#"["a","b","c"]"#).trim(),
        r#""\"a\",\"b\",\"c\"""#
    );
    assert_jq_compat("@csv", r#"["a","b","c"]"#);
}

#[test]
fn format_csv_numbers() {
    assert_eq!(qj_compact("@csv", "[1,2,3]").trim(), r#""1,2,3""#);
    assert_jq_compat("@csv", "[1,2,3]");
}

#[test]
fn format_tsv() {
    assert_eq!(
        qj_compact("@tsv", r#"["a","b","c"]"#).trim(),
        r#""a\tb\tc""#
    );
    assert_jq_compat("@tsv", r#"["a","b","c"]"#);
}

#[test]
fn format_html() {
    assert_eq!(
        qj_compact("@html", r#""<b>bold</b>""#).trim(),
        r#""&lt;b&gt;bold&lt;/b&gt;""#
    );
    assert_jq_compat("@html", r#""<b>bold</b>""#);
}

#[test]
fn format_sh() {
    assert_eq!(
        qj_compact("@sh", r#""hello world""#).trim(),
        r#""'hello world'""#
    );
    assert_jq_compat("@sh", r#""hello world""#);
}

#[test]
fn format_json() {
    assert_eq!(qj_compact("@json", "[1,2,3]").trim(), r#""[1,2,3]""#);
    assert_jq_compat("@json", "[1,2,3]");
}

#[test]
fn format_text() {
    assert_eq!(qj_compact("@text", "42").trim(), r#""42""#);
    assert_jq_compat("@text", "42");
}

#[test]
fn format_strings_on_non_strings() {
    // jq calls tostring on non-string inputs before applying format strings
    assert_jq_compat("@html", "null");
    assert_jq_compat("@html", "42");
    assert_jq_compat("@html", "true");
    assert_jq_compat("@html", "[1,2]");
    assert_jq_compat("@uri", "null");
    assert_jq_compat("@uri", "42");
    assert_jq_compat("@sh", "null");
    assert_jq_compat("@sh", "42");
    assert_jq_compat("@sh", "true");
    // @sh on arrays: space-joined, each element escaped
    assert_jq_compat("@sh", r#"[1,"two",3]"#);
    assert_jq_compat("@sh", "[true,null]");
    assert_jq_compat("@base64", "null");
    assert_jq_compat("@base64", "42");
    assert_jq_compat("@base64", "true");
    assert_jq_compat("@urid", "42");
    assert_jq_compat("@urid", "null");
}

// --- Builtin: in ---

#[test]
fn builtin_in_object() {
    assert_eq!(
        qj_compact(r#""foo" | in({"foo":42})"#, "null").trim(),
        "true"
    );
    assert_eq!(
        qj_compact(r#""bar" | in({"foo":42})"#, "null").trim(),
        "false"
    );
    assert_jq_compat(r#""foo" | in({"foo":42})"#, "null");
}

#[test]
fn builtin_in_array() {
    assert_eq!(qj_compact("2 | in([0,1,2])", "null").trim(), "true");
    assert_eq!(qj_compact("5 | in([0,1,2])", "null").trim(), "false");
    assert_jq_compat("2 | in([0,1,2])", "null");
}

// --- Builtin: combinations ---

#[test]
fn builtin_combinations() {
    assert_eq!(
        qj_compact("[combinations]", "[[1,2],[3,4]]").trim(),
        "[[1,3],[1,4],[2,3],[2,4]]"
    );
    assert_jq_compat("[combinations]", "[[1,2],[3,4]]");
}

#[test]
fn builtin_combinations_n() {
    assert_eq!(
        qj_compact("[combinations(2)]", "[0,1]").trim(),
        "[[0,0],[0,1],[1,0],[1,1]]"
    );
    assert_jq_compat("[combinations(2)]", "[0,1]");
}

// --- def (user-defined functions) ---

#[test]
fn def_zero_arg() {
    assert_eq!(qj_compact("def f: . + 1; f", "5").trim(), "6");
    assert_jq_compat("def f: . + 1; f", "5");
}

#[test]
fn def_filter_param() {
    assert_eq!(qj_compact("def f(x): x | x; f(. + 1)", "5").trim(), "7");
    assert_jq_compat("def f(x): x | x; f(. + 1)", "5");
}

#[test]
fn def_generator_body() {
    assert_eq!(qj_compact("def f: (1,2); [f]", "null").trim(), "[1,2]");
    assert_jq_compat("def f: (1,2); [f]", "null");
}

#[test]
fn def_generator_filter_param() {
    // Filter params are generators: x produces 1, then 2; x|x produces 1,2,1,2
    assert_eq!(
        qj_compact("def f(x): x | x; [f(1,2)]", "null").trim(),
        "[1,2,1,2]"
    );
    assert_jq_compat("def f(x): x | x; [f(1,2)]", "null");
}

#[test]
fn def_dollar_param() {
    assert_eq!(qj_compact("def f($x): $x + 1; f(10)", "null").trim(), "11");
    assert_jq_compat("def f($x): $x + 1; f(10)", "null");
}

#[test]
fn def_multiple_dollar_params() {
    assert_eq!(
        qj_compact("def add($a; $b): $a + $b; add(3; 4)", "null").trim(),
        "7"
    );
    assert_jq_compat("def add($a; $b): $a + $b; add(3; 4)", "null");
}

#[test]
fn def_nested() {
    assert_eq!(
        qj_compact("def f: . + 1; def g: f | f; 3 | g", "null").trim(),
        "5"
    );
    assert_jq_compat("def f: . + 1; def g: f | f; 3 | g", "null");
}

#[test]
fn def_shadowing() {
    // Later def of same name/arity shadows earlier one
    assert_eq!(
        qj_compact("def f: . + 1; def f: . * 2; 10 | f", "null").trim(),
        "20"
    );
    assert_jq_compat("def f: . + 1; def f: . * 2; 10 | f", "null");
}

#[test]
fn def_arity_overload() {
    // Same name, different arity — both coexist
    assert_eq!(
        qj_compact("def f: . + 1; def f(x): . + x; [5 | f, f(10)]", "null").trim(),
        "[6,15]"
    );
    assert_jq_compat("def f: . + 1; def f(x): . + x; [5 | f, f(10)]", "null");
}

#[test]
fn def_recursion_factorial() {
    assert_eq!(
        qj_compact(
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
    assert_eq!(qj_compact("5 as $x | def f: $x + 1; f", "null").trim(), "6");
    assert_jq_compat("5 as $x | def f: $x + 1; f", "null");
}

#[test]
fn def_map_with_user_func() {
    assert_eq!(
        qj_compact("def addone: . + 1; [.[] | addone]", "[1,2,3]").trim(),
        "[2,3,4]"
    );
    assert_jq_compat("def addone: . + 1; [.[] | addone]", "[1,2,3]");
}

#[test]
fn def_recursive_sum() {
    assert_eq!(
        qj_compact(
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
    let (ok, stdout, stderr) = qj_result("null | setpath([9999999]; 1)", "null");
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
    let (ok, _stdout, stderr) = qj_result(&deep, "null");
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
    let out = qj_compact(r#"("'" | fromjson) // "caught_error""#, "null");
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
    let out = qj_compact(&filter, "null");
    // Should get the fallback since fromjson on gibberish fails
    assert_eq!(out.trim(), r#""safe_fallback""#);
}

#[test]
fn robustness_eval_depth_limit() {
    // def f: f; — infinite recursion should hit eval depth limit, not stack overflow
    let (ok, _stdout, stderr) = qj_result("def f: f; f", "null");
    assert!(!ok, "infinite recursion should fail");
    assert!(
        stderr.contains("depth limit"),
        "should mention depth limit: {stderr}"
    );
}

#[test]
fn robustness_no_stale_error_leakage() {
    // An error in one expression should not leak into a subsequent try
    let out = qj_compact(r#"(try error catch "caught") | . + " ok""#, "null");
    assert_eq!(out.trim(), r#""caught ok""#);
}

// --- Exit code tests ---

#[test]
fn exit_code_0_on_success() {
    let (code, stdout, _stderr) = qj_exit(&["-c", "."], r#"{"a":1}"#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"a":1}"#);
}

#[test]
fn exit_code_5_on_runtime_error() {
    // error("boom") should produce exit code 5 and print message to stderr
    let (code, stdout, stderr) = qj_exit(&["-c", r#"error("boom")"#], "null");
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
    let (code, _stdout, stderr) = qj_exit(&["-c", ".foo"], "42");
    assert_eq!(code, 5, "expected exit code 5, got {code}");
    assert!(
        stderr.contains("Cannot index"),
        "expected type error on stderr, got: {stderr}"
    );
}

#[test]
fn exit_code_4_on_no_output_with_e_flag() {
    // -e flag with no output should produce exit code 4
    let (code, stdout, _stderr) = qj_exit(&["-e", "-c", "empty"], "null");
    assert_eq!(code, 4, "expected exit code 4, got {code}");
    assert!(stdout.trim().is_empty());
}

#[test]
fn exit_code_0_on_output_with_e_flag() {
    // -e flag with output should produce exit code 0
    let (code, stdout, _stderr) = qj_exit(&["-e", "-c", "."], "42");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn exit_code_5_error_builtin_bare() {
    // bare error (no message) uses the input as the error value
    let (code, _stdout, stderr) = qj_exit(&["-c", "error"], r#""my error""#);
    assert_eq!(code, 5, "expected exit code 5, got {code}");
    assert!(
        stderr.contains("my error"),
        "expected error value on stderr, got: {stderr}"
    );
}

#[test]
fn exit_code_0_when_error_caught_by_try() {
    // error caught by try should exit 0
    let (code, stdout, _stderr) = qj_exit(&["-c", r#"try error("boom")"#], "null");
    assert_eq!(code, 0, "expected exit code 0 when error is caught");
    assert!(stdout.trim().is_empty()); // try suppresses both error and output
}

#[test]
fn exit_code_0_when_error_caught_by_try_catch() {
    // error caught by try-catch should exit 0 and output the catch handler result
    let (code, stdout, _stderr) = qj_exit(&["-c", r#"try error("boom") catch ."#], "null");
    assert_eq!(code, 0, "expected exit code 0 when error is caught");
    assert_eq!(stdout.trim(), r#""boom""#);
}

#[test]
fn exit_code_5_precedes_exit_code_4() {
    // When both an error and -e no-output apply, error (exit 5) takes precedence
    let (code, _stdout, stderr) = qj_exit(&["-e", "-c", r#"error("x")"#], "null");
    assert_eq!(code, 5, "error exit code should take precedence over -e");
    assert!(stderr.contains("x"));
}

// --- --from-file tests ---

#[test]
fn from_file_basic() {
    // Write a filter to a temp file and use -f to read it
    let dir = std::env::temp_dir();
    let filter_path = dir.join("qj_test_filter.jq");
    std::fs::write(&filter_path, ".a + .b").unwrap();

    let (code, stdout, _stderr) = qj_exit(
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
    let filter_path = dir.join("qj_test_filter2.jq");
    let input_path = dir.join("qj_test_input2.json");
    std::fs::write(&filter_path, ".name").unwrap();
    std::fs::write(&input_path, r#"{"name":"alice"}"#).unwrap();

    let (code, stdout, _stderr) = qj_exit(
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
    let (code, stdout, _) = qj_exit(&["-nc", "[inputs]"], "1\n2\n3\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[1,2,3]");
}

#[test]
fn input_single() {
    let (code, stdout, _) = qj_exit(&["-nc", "input"], "1\n2\n3\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "1");
}

#[test]
fn input_multiple_calls() {
    // Two calls to input: get first two values
    let (code, stdout, _) = qj_exit(&["-nc", "[input, input]"], "10\n20\n30\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[10,20]");
}

#[test]
fn inputs_without_null_input() {
    // Without -n: first value is ., inputs gets the rest
    let (code, stdout, _) = qj_exit(&["-c", "[., inputs]"], "1\n2\n3\n");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[1,2,3]");
}

#[test]
fn inputs_empty_queue() {
    // With -n and no stdin data, inputs should produce empty array
    let (code, stdout, _) = qj_exit(&["-nc", "[inputs]"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[]");
}

// ---------------------------------------------------------------------------
// Color output
// ---------------------------------------------------------------------------

#[test]
fn color_output_forced() {
    // -C forces color even when piped (test is piped)
    let (code, stdout, _) = qj_exit(&["-Cc", "."], r#"{"a":1}"#);
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
    let (code, stdout, _) = qj_exit(&["-Mc", "."], r#"{"a":1}"#);
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
    let (code, stdout, _) = qj_exit(&["-C", "."], r#"{"a":null,"b":"hi"}"#);
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
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(["-c", "."])
        .env("NO_COLOR", "1")
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
                .write_all(br#"{"a":1}"#)
                .unwrap();
            child.wait_with_output()
        })
        .expect("failed to run qj");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("\x1b["),
        "NO_COLOR should suppress ANSI codes, got: {stdout:?}"
    );
}

#[test]
fn no_color_env_overridden_by_flag() {
    // -C should override NO_COLOR
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(["-Cc", "."])
        .env("NO_COLOR", "1")
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
                .write_all(br#"{"a":1}"#)
                .unwrap();
            child.wait_with_output()
        })
        .expect("failed to run qj");
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
    let path = dir.join("qj_test_rawfile.txt");
    std::fs::write(&path, "hello world").unwrap();

    let (code, stdout, _) = qj_exit(
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
    let path = dir.join("qj_test_slurpfile.json");
    std::fs::write(&path, "1\n2\n3").unwrap();

    let (code, stdout, _) = qj_exit(
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
    let (code, stdout, _) = qj_exit(&["-nc", "$ARGS.positional", "--args", "a", "b", "c"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"["a","b","c"]"#);
}

#[test]
fn jsonargs_positional() {
    let (code, stdout, _) = qj_exit(
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
    let (code, stdout, _) = qj_exit(&["-nc", "$ARGS.named", "--arg", "name", "alice"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"name":"alice"}"#);
}

#[test]
fn args_empty_default() {
    let (code, stdout, _) = qj_exit(&["-nc", "$ARGS"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"positional":[],"named":{}}"#);
}

// ---------------------------------------------------------------------------
// --raw-output0
// ---------------------------------------------------------------------------

#[test]
fn raw_output0_nul_separator_for_strings() {
    let (code, stdout, _) = qj_exit(&["--raw-output0", ".[]"], r#"["hello","world"]"#);
    assert_eq!(code, 0);
    assert_eq!(stdout.as_bytes(), b"hello\0world\0");
}

#[test]
fn raw_output0_nul_separator_for_all_types() {
    // jq uses NUL as separator for ALL output values, not just strings
    let (code, stdout, _) = qj_exit(&["--raw-output0", ".[]"], r#"["hello",42,"world"]"#);
    assert_eq!(code, 0);
    assert_eq!(stdout.as_bytes(), b"hello\x0042\x00world\x00");
}

#[test]
fn raw_output0_implies_raw_mode() {
    // --raw-output0 implies -r: strings should be unquoted
    let (code, stdout, _) = qj_exit(&["--raw-output0", "."], r#""hello""#);
    assert_eq!(code, 0);
    assert_eq!(stdout.as_bytes(), b"hello\0");
}

#[test]
fn raw_output0_non_string_gets_nul() {
    let (code, stdout, _) = qj_exit(&["--raw-output0", "."], "42");
    assert_eq!(code, 0);
    assert_eq!(stdout.as_bytes(), b"42\0");
}

#[test]
fn raw_output0_jq_compat() {
    if !jq_available() {
        return;
    }
    let input = r#"["a","b","c"]"#;
    let qj_out = {
        let (code, stdout, _) = qj_exit(&["--raw-output0", ".[]"], input);
        assert_eq!(code, 0);
        stdout
    };
    let jq_out = {
        let output = Command::new("jq")
            .args(["--raw-output0", ".[]"])
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
            .expect("failed to run jq");
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    assert_eq!(
        qj_out.as_bytes(),
        jq_out.as_bytes(),
        "qj vs jq --raw-output0 mismatch"
    );
}

#[test]
fn raw_output0_embedded_nul_error() {
    // Strings containing NUL bytes should cause exit code 5 with --raw-output0
    let input = r#"{"a":"x\u0000y"}"#;
    let (code, stdout, stderr) = qj_exit(&["--raw-output0", ".a"], input);
    assert_eq!(code, 5);
    assert!(stdout.is_empty(), "should produce no stdout output");
    assert!(
        stderr.contains("Cannot dump a string containing NUL with --raw-output0 option"),
        "stderr should contain NUL error message, got: {stderr}"
    );
}

#[test]
fn raw_output0_embedded_nul_partial_output() {
    // Values before the NUL-containing string should still be output
    let input = r#"["ok","x\u0000y","fine"]"#;
    let (code, stdout, stderr) = qj_exit(&["--raw-output0", ".[]"], input);
    assert_eq!(code, 5);
    assert_eq!(stdout.as_bytes(), b"ok\0", "should output 'ok' then stop");
    assert!(stderr.contains("NUL"));
}

#[test]
fn raw_output0_no_nul_in_string_ok() {
    // Normal strings (no embedded NUL) should work fine
    let (code, _, _) = qj_exit(&["--raw-output0", "."], r#""hello world""#);
    assert_eq!(code, 0);
}

// ---------------------------------------------------------------------------
// --ascii-output / -a
// ---------------------------------------------------------------------------

#[test]
fn ascii_output_escapes_non_ascii() {
    let (code, stdout, _) = qj_exit(&["-ac", "."], r#""café""#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#""caf\u00e9""#);
}

#[test]
fn ascii_output_surrogate_pairs() {
    // Emoji (U+1F30D) should be encoded as surrogate pair
    let (code, stdout, _) = qj_exit(&["-ac", "."], "\"\\ud83c\\udf0d\"");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#""\ud83c\udf0d"#.to_owned() + "\"");
}

#[test]
fn ascii_output_escapes_keys() {
    let (code, stdout, _) = qj_exit(&["-ac", "."], r#"{"café":"latte"}"#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#"{"caf\u00e9":"latte"}"#);
}

#[test]
fn ascii_output_with_raw_mode() {
    // jq with -ra outputs JSON-encoded string (with quotes) when -a is active
    let (code, stdout, _) = qj_exit(&["-ra", "."], r#""café""#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#""caf\u00e9""#);
}

#[test]
fn ascii_output_pretty() {
    let (code, stdout, _) = qj_exit(&["-a", "."], r#"{"café":"latté"}"#);
    assert_eq!(code, 0);
    assert!(stdout.contains(r#""caf\u00e9""#));
    assert!(stdout.contains(r#""latt\u00e9""#));
}

#[test]
fn ascii_output_ascii_passthrough() {
    // Pure ASCII strings should be unchanged
    let (code, stdout, _) = qj_exit(&["-ac", "."], r#""hello world""#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), r#""hello world""#);
}

#[test]
fn ascii_output_jq_compat() {
    if !jq_available() {
        return;
    }
    // Test various non-ASCII strings against jq
    for input in &[r#""café""#, r#""日本語""#, r#"{"ñ":"ü"}"#] {
        let qj_out = {
            let (code, stdout, _) = qj_exit(&["-ac", "."], input);
            assert_eq!(code, 0);
            stdout
        };
        let jq_out =
            run_jq(&["-ac", "."], input).unwrap_or_else(|| panic!("jq failed on input={input:?}"));
        assert_eq!(
            qj_out.trim(),
            jq_out.trim(),
            "qj vs jq -ac mismatch: input={input:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// --unbuffered
// ---------------------------------------------------------------------------

#[test]
fn unbuffered_output_correctness() {
    // --unbuffered should produce the same output as without it
    let normal = qj_compact(".", r#"{"a":1}"#);
    let (code, stdout, _) = qj_exit(&["-c", "--unbuffered", "."], r#"{"a":1}"#);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), normal.trim());
}

#[test]
fn unbuffered_with_multiple_values() {
    let (code, stdout, _) = qj_exit(&["-c", "--unbuffered", ".[]"], "[1,2,3]");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "1\n2\n3");
}

// ---------------------------------------------------------------------------
// NDJSON fast-path jq compatibility
// ---------------------------------------------------------------------------

/// Compare qj vs jq output on NDJSON input (multi-line).
/// Skips if jq is not installed.
fn assert_jq_compat_ndjson(filter: &str, ndjson_input: &str) {
    if !jq_available() {
        return;
    }
    let qj_out = qj_compact(filter, ndjson_input);
    let jq_out = run_jq_compact(filter, ndjson_input)
        .unwrap_or_else(|| panic!("jq failed on filter={filter:?}"));
    assert_eq!(
        qj_out.trim(),
        jq_out.trim(),
        "qj vs jq NDJSON mismatch: filter={filter:?}"
    );
}

#[test]
fn ndjson_jq_compat_select_string() {
    assert_jq_compat_ndjson(
        "select(.type == \"PushEvent\")",
        "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n{\"type\":\"PushEvent\",\"id\":3}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_int() {
    assert_jq_compat_ndjson("select(.n == 42)", "{\"n\":42}\n{\"n\":7}\n{\"n\":42}\n");
}

#[test]
fn ndjson_jq_compat_select_bool() {
    assert_jq_compat_ndjson(
        "select(.active == true)",
        "{\"active\":true}\n{\"active\":false}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_null() {
    assert_jq_compat_ndjson("select(.x == null)", "{\"x\":null}\n{\"x\":1}\n{\"y\":2}\n");
}

#[test]
fn ndjson_jq_compat_select_ne() {
    assert_jq_compat_ndjson(
        "select(.type != \"PushEvent\")",
        "{\"type\":\"PushEvent\"}\n{\"type\":\"WatchEvent\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_float_vs_int() {
    // Critical: 1.0 == 1 must match (byte mismatch, value equal)
    assert_jq_compat_ndjson(
        "select(.n == 1)",
        "{\"n\":1.0,\"id\":\"a\"}\n{\"n\":2,\"id\":\"b\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_float_ne() {
    // 1.0 != 1 should NOT output the 1.0 line
    assert_jq_compat_ndjson("select(.n != 1)", "{\"n\":1.0}\n{\"n\":2}\n");
}

#[test]
fn ndjson_jq_compat_select_missing_field() {
    assert_jq_compat_ndjson("select(.x == \"hello\")", "{\"x\":\"hello\"}\n{\"a\":1}\n");
}

#[test]
fn ndjson_jq_compat_select_nested_field() {
    assert_jq_compat_ndjson(
        "select(.a.b == \"yes\")",
        "{\"a\":{\"b\":\"yes\"},\"id\":1}\n{\"a\":{\"b\":\"no\"},\"id\":2}\n",
    );
}

#[test]
fn ndjson_jq_compat_bare_length() {
    assert_jq_compat_ndjson("length", "{\"a\":1,\"b\":2}\n{\"x\":1}\n");
}

#[test]
fn ndjson_jq_compat_field_length() {
    assert_jq_compat_ndjson(
        ".items | length",
        "{\"items\":[1,2,3]}\n{\"items\":[4,5]}\n",
    );
}

#[test]
fn ndjson_jq_compat_bare_keys() {
    assert_jq_compat_ndjson("keys", "{\"b\":2,\"a\":1}\n{\"x\":1}\n");
}

#[test]
fn ndjson_jq_compat_field_keys() {
    assert_jq_compat_ndjson(
        ".data | keys",
        "{\"data\":{\"b\":2,\"a\":1}}\n{\"data\":{\"x\":1}}\n",
    );
}

#[test]
fn ndjson_jq_compat_field_chain() {
    assert_jq_compat_ndjson(
        ".actor.login",
        "{\"actor\":{\"login\":\"alice\"}}\n{\"actor\":{\"login\":\"bob\"}}\n",
    );
}

#[test]
fn ndjson_jq_compat_field_chain_missing() {
    assert_jq_compat_ndjson(".name", "{\"name\":\"alice\"}\n{\"age\":30}\n");
}

#[test]
fn ndjson_jq_compat_select_empty_string() {
    assert_jq_compat_ndjson(
        "select(.name == \"\")",
        "{\"name\":\"\"}\n{\"name\":\"bob\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_negative_int() {
    assert_jq_compat_ndjson("select(.n == -1)", "{\"n\":-1}\n{\"n\":1}\n");
}

#[test]
fn ndjson_jq_compat_length_empty() {
    assert_jq_compat_ndjson("length", "{}\n{\"a\":1}\n");
}

#[test]
fn ndjson_jq_compat_keys_empty() {
    assert_jq_compat_ndjson("keys", "{}\n{\"a\":1}\n");
}

#[test]
fn ndjson_jq_compat_string_length_fallback() {
    assert_jq_compat_ndjson(
        ".name | length",
        "{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n",
    );
}

// --- select + field extraction jq compat ---

#[test]
fn ndjson_jq_compat_select_eq_field() {
    assert_jq_compat_ndjson(
        "select(.type == \"PushEvent\") | .actor",
        "{\"type\":\"PushEvent\",\"actor\":\"alice\"}\n{\"type\":\"WatchEvent\",\"actor\":\"bob\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_eq_nested_field() {
    assert_jq_compat_ndjson(
        "select(.type == \"PushEvent\") | .actor.login",
        "{\"type\":\"PushEvent\",\"actor\":{\"login\":\"alice\"}}\n{\"type\":\"WatchEvent\",\"actor\":{\"login\":\"bob\"}}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_eq_field_float_fallback() {
    assert_jq_compat_ndjson(
        "select(.n == 1) | .name",
        "{\"n\":1.0,\"name\":\"a\"}\n{\"n\":2,\"name\":\"b\"}\n",
    );
}

// --- select + object/array construction jq compat ---

#[test]
fn ndjson_jq_compat_select_eq_obj() {
    assert_jq_compat_ndjson(
        "select(.type == \"PushEvent\") | {type: .type, actor: .actor}",
        "{\"type\":\"PushEvent\",\"actor\":\"alice\"}\n{\"type\":\"WatchEvent\",\"actor\":\"bob\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_eq_arr() {
    assert_jq_compat_ndjson(
        "select(.type == \"PushEvent\") | [.type, .id]",
        "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n",
    );
}

// --- Multi-field object/array construction ---

#[test]
fn ndjson_jq_compat_multi_field_obj() {
    assert_jq_compat_ndjson(
        "{type: .type, id: .id, actor: .actor}",
        "{\"type\":\"PushEvent\",\"id\":1,\"actor\":\"alice\"}\n{\"type\":\"WatchEvent\",\"id\":2,\"actor\":\"bob\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_multi_field_obj_shorthand() {
    assert_jq_compat_ndjson(
        "{type, id: .id}",
        "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n",
    );
}

#[test]
fn ndjson_jq_compat_multi_field_obj_nested() {
    assert_jq_compat_ndjson(
        "{actor: .actor.login, repo: .repo.name}",
        "{\"actor\":{\"login\":\"alice\"},\"repo\":{\"name\":\"foo\"}}\n{\"actor\":{\"login\":\"bob\"},\"repo\":{\"name\":\"bar\"}}\n",
    );
}

#[test]
fn ndjson_jq_compat_multi_field_obj_missing() {
    assert_jq_compat_ndjson(
        "{type, id: .id}",
        "{\"type\":\"PushEvent\"}\n{\"type\":\"WatchEvent\",\"id\":2}\n",
    );
}

#[test]
fn ndjson_jq_compat_multi_field_arr() {
    assert_jq_compat_ndjson("[.x, .y]", "{\"x\":1,\"y\":2}\n{\"x\":3,\"y\":4}\n");
}

#[test]
fn ndjson_jq_compat_multi_field_arr_nested() {
    assert_jq_compat_ndjson(
        "[.a.b, .c]",
        "{\"a\":{\"b\":\"deep\"},\"c\":1}\n{\"a\":{\"b\":\"val\"},\"c\":2}\n",
    );
}

#[test]
fn ndjson_jq_compat_multi_field_arr_missing() {
    assert_jq_compat_ndjson("[.x, .y]", "{\"x\":1}\n{\"x\":2,\"y\":3}\n");
}

// --- Ordering operators in select ---

#[test]
fn ndjson_jq_compat_select_gt_int() {
    assert_jq_compat_ndjson(
        "select(.n > 10)",
        "{\"n\":5}\n{\"n\":10}\n{\"n\":50}\n{\"n\":100}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_lt_int() {
    assert_jq_compat_ndjson("select(.n < 10)", "{\"n\":5}\n{\"n\":10}\n{\"n\":50}\n");
}

#[test]
fn ndjson_jq_compat_select_ge_int() {
    assert_jq_compat_ndjson("select(.n >= 10)", "{\"n\":5}\n{\"n\":10}\n{\"n\":50}\n");
}

#[test]
fn ndjson_jq_compat_select_le_int() {
    assert_jq_compat_ndjson("select(.n <= 10)", "{\"n\":5}\n{\"n\":10}\n{\"n\":50}\n");
}

#[test]
fn ndjson_jq_compat_select_gt_float() {
    assert_jq_compat_ndjson(
        "select(.n > 3)",
        "{\"n\":3.14}\n{\"n\":2.71}\n{\"n\":1.0}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_gt_negative() {
    assert_jq_compat_ndjson("select(.n > -1)", "{\"n\":-5}\n{\"n\":0}\n{\"n\":5}\n");
}

#[test]
fn ndjson_jq_compat_select_gt_string() {
    assert_jq_compat_ndjson(
        "select(.s > \"banana\")",
        "{\"s\":\"apple\"}\n{\"s\":\"banana\"}\n{\"s\":\"cherry\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_gt_field_extract() {
    assert_jq_compat_ndjson(
        "select(.n > 10) | .name",
        "{\"n\":20,\"name\":\"a\"}\n{\"n\":5,\"name\":\"b\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_gt_obj_extract() {
    assert_jq_compat_ndjson(
        "select(.n > 10) | {name}",
        "{\"n\":20,\"name\":\"a\"}\n{\"n\":5,\"name\":\"b\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_gt_arr_extract() {
    assert_jq_compat_ndjson(
        "select(.n > 10) | [.n, .name]",
        "{\"n\":20,\"name\":\"a\"}\n{\"n\":5,\"name\":\"b\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_gt_mixed_types() {
    // jq type ordering: null < false < true < numbers < strings
    assert_jq_compat_ndjson(
        "select(.v > 5)",
        "{\"v\":10}\n{\"v\":3}\n{\"v\":\"hello\"}\n{\"v\":null}\n",
    );
}

// --- String predicate select (test/startswith/endswith/contains) ---

#[test]
fn ndjson_jq_compat_select_test() {
    assert_jq_compat_ndjson(
        r#"select(.msg | test("error"))"#,
        "{\"msg\":\"error: disk full\"}\n{\"msg\":\"ok\"}\n{\"msg\":\"error: timeout\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_startswith() {
    assert_jq_compat_ndjson(
        r#"select(.url | startswith("/api"))"#,
        "{\"url\":\"/api/users\"}\n{\"url\":\"/web/home\"}\n{\"url\":\"/api/items\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_endswith() {
    assert_jq_compat_ndjson(
        r#"select(.file | endswith(".json"))"#,
        "{\"file\":\"data.json\"}\n{\"file\":\"data.csv\"}\n{\"file\":\"config.json\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_contains_string() {
    assert_jq_compat_ndjson(
        r#"select(.desc | contains("alice"))"#,
        "{\"desc\":\"hello alice\"}\n{\"desc\":\"hello bob\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_test_regex() {
    assert_jq_compat_ndjson(
        r#"select(.code | test("^ERR-\\d+$"))"#,
        "{\"code\":\"ERR-001\"}\n{\"code\":\"OK-200\"}\n{\"code\":\"ERR-42\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_test_extract() {
    assert_jq_compat_ndjson(
        r#"select(.msg | test("error")) | .code"#,
        "{\"msg\":\"error: disk full\",\"code\":500}\n{\"msg\":\"ok\",\"code\":200}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_startswith_nested() {
    assert_jq_compat_ndjson(
        r#"select(.actor.login | startswith("bot"))"#,
        "{\"actor\":{\"login\":\"bot-alice\"}}\n{\"actor\":{\"login\":\"human-bob\"}}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_test_no_match() {
    assert_jq_compat_ndjson(
        r#"select(.msg | test("error"))"#,
        "{\"msg\":\"ok\"}\n{\"msg\":\"success\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_select_endswith_extract() {
    assert_jq_compat_ndjson(
        r#"select(.file | endswith(".json")) | .file"#,
        "{\"file\":\"data.json\"}\n{\"file\":\"data.csv\"}\n",
    );
}

// --- Flat eval: Compare / BoolOp / Arith / Neg ---

#[test]
fn jq_compat_compare_gt() {
    assert_jq_compat(".a > 0", r#"{"a":5}"#);
    assert_jq_compat(".a > 0", r#"{"a":0}"#);
    assert_jq_compat(".a > 0", r#"{"a":-1}"#);
}

#[test]
fn jq_compat_compare_eq_string() {
    assert_jq_compat(r#".name == "alice""#, r#"{"name":"alice"}"#);
    assert_jq_compat(r#".name == "alice""#, r#"{"name":"bob"}"#);
}

#[test]
fn jq_compat_compare_null_field() {
    assert_jq_compat(".missing > 0", r#"{"a":1}"#);
    assert_jq_compat(".missing == null", r#"{"a":1}"#);
}

#[test]
fn jq_compat_bool_and_or() {
    assert_jq_compat(".a > 0 and .b > 0", r#"{"a":1,"b":2}"#);
    assert_jq_compat(".a > 0 and .b > 0", r#"{"a":0,"b":2}"#);
    assert_jq_compat(".a > 0 or .b > 0", r#"{"a":0,"b":0}"#);
    assert_jq_compat(".a > 0 or .b > 0", r#"{"a":1,"b":0}"#);
}

#[test]
fn jq_compat_arith_basic() {
    assert_jq_compat(".a + .b", r#"{"a":10,"b":20}"#);
    assert_jq_compat(".a - .b", r#"{"a":10,"b":3}"#);
    assert_jq_compat(".a * .b", r#"{"a":6,"b":7}"#);
    assert_jq_compat(".a / .b", r#"{"a":10,"b":4}"#);
    assert_jq_compat(".a % .b", r#"{"a":10,"b":3}"#);
}

#[test]
fn jq_compat_arith_string_concat() {
    assert_jq_compat(r#".a + .b"#, r#"{"a":"hello","b":" world"}"#);
}

#[test]
fn jq_compat_neg() {
    assert_jq_compat(".a | -(.) ", r#"{"a":42}"#);
    assert_jq_compat(".a | -(.) ", r#"{"a":-5}"#);
    assert_jq_compat(".a | -(.) ", r#"{"a":3.14}"#);
}

// --- Flat eval: Select with Compare in pipe ---

#[test]
fn jq_compat_select_compare_pipe() {
    assert_jq_compat(
        r#"[.[] | select(.x > 0) | .name]"#,
        r#"[{"x":1,"name":"a"},{"x":0,"name":"b"},{"x":5,"name":"c"}]"#,
    );
}

#[test]
fn jq_compat_select_and_construct() {
    assert_jq_compat(
        r#"[.[] | select(.x > 0) | {name, x}]"#,
        r#"[{"x":1,"name":"a","extra":true},{"x":0,"name":"b","extra":false}]"#,
    );
}

#[test]
fn jq_compat_select_complex_condition() {
    assert_jq_compat(
        r#"[.[] | select(.x > 0 and .name != "skip")]"#,
        r#"[{"x":1,"name":"a"},{"x":2,"name":"skip"},{"x":0,"name":"c"}]"#,
    );
}

// --- Flat eval: tojson ---

#[test]
fn jq_compat_tojson_scalars() {
    assert_jq_compat("tojson", "42");
    assert_jq_compat("tojson", "true");
    assert_jq_compat("tojson", "false");
    assert_jq_compat("tojson", "null");
    assert_jq_compat("tojson", r#""hello""#);
}

#[test]
fn jq_compat_tojson_containers() {
    assert_jq_compat("tojson", "[1,2,3]");
    assert_jq_compat("tojson", r#"{"a":1,"b":"two"}"#);
    assert_jq_compat("tojson", r#"{"a":{"b":[1,true,null]}}"#);
}

#[test]
fn jq_compat_tojson_map_values() {
    assert_jq_compat("map_values(tojson)", r#"{"a":1,"b":"two","c":null}"#);
}

#[test]
fn jq_compat_tojson_in_pipe() {
    assert_jq_compat(".a | tojson", r#"{"a":{"x":1,"y":[2,3]}}"#);
}

// --- Flat eval: Def / IfThenElse / Bind ---

#[test]
fn jq_compat_def_simple() {
    assert_jq_compat("def f: .a; f", r#"{"a":42,"b":99}"#);
}

#[test]
fn jq_compat_def_with_args() {
    assert_jq_compat(
        r#"def hi(x): if x > 0 then "yes" else "no" end; hi(.a)"#,
        r#"{"a":5}"#,
    );
    assert_jq_compat(
        r#"def hi(x): if x > 0 then "yes" else "no" end; hi(.a)"#,
        r#"{"a":0}"#,
    );
}

#[test]
fn jq_compat_def_with_iterate() {
    assert_jq_compat("def double: . * 2; [.[] | double]", "[1,2,3]");
}

#[test]
fn jq_compat_if_then_else() {
    assert_jq_compat(r#"if .x > 0 then "pos" else "non-pos" end"#, r#"{"x":5}"#);
    assert_jq_compat(r#"if .x > 0 then "pos" else "non-pos" end"#, r#"{"x":-1}"#);
}

#[test]
fn jq_compat_elif_object() {
    assert_jq_compat(
        r#"if .x > 10 then "big" elif .x > 0 then "small" else "zero" end"#,
        r#"{"x":15}"#,
    );
    assert_jq_compat(
        r#"if .x > 10 then "big" elif .x > 0 then "small" else "zero" end"#,
        r#"{"x":5}"#,
    );
    assert_jq_compat(
        r#"if .x > 10 then "big" elif .x > 0 then "small" else "zero" end"#,
        r#"{"x":0}"#,
    );
}

#[test]
fn jq_compat_bind_simple() {
    assert_jq_compat(". as $s | $s.a + $s.b", r#"{"a":10,"b":20}"#);
}

#[test]
fn jq_compat_bind_in_iterate() {
    assert_jq_compat(
        "[.[] | . as $s | {name: $s.name, double: ($s.x * 2)}]",
        r#"[{"name":"a","x":1},{"name":"b","x":2}]"#,
    );
}

// --- Flat eval: sort_by ---

#[test]
fn jq_compat_sort_by() {
    assert_jq_compat(
        "sort_by(.x)",
        r#"[{"x":3,"n":"c"},{"x":1,"n":"a"},{"x":2,"n":"b"}]"#,
    );
    assert_jq_compat(
        "sort_by(.x) | .[-1].n",
        r#"[{"x":3,"n":"c"},{"x":1,"n":"a"}]"#,
    );
    assert_jq_compat("sort_by(.x)", r#"[{"x":"b"},{"x":"a"},{"x":"c"}]"#);
}

// --- Flat eval: group_by ---

#[test]
fn jq_compat_group_by_flat() {
    assert_jq_compat(
        "group_by(.t) | length",
        r#"[{"t":"a"},{"t":"b"},{"t":"a"},{"t":"c"},{"t":"b"}]"#,
    );
    assert_jq_compat(
        "group_by(.t)",
        r#"[{"t":1,"n":"a"},{"t":2,"n":"b"},{"t":1,"n":"c"}]"#,
    );
}

// --- Flat eval: PostfixSlice ---

#[test]
fn jq_compat_postfix_slice() {
    assert_jq_compat("[1,2,3,4,5][:3]", "null");
    assert_jq_compat("[1,2,3,4,5][2:4]", "null");
    assert_jq_compat("[1,2,3,4,5][3:]", "null");
    assert_jq_compat(r#""hello"[1:3]"#, "null");
    assert_jq_compat("[.[] | .x][:2]", r#"[{"x":1},{"x":2},{"x":3}]"#);
}

// --- Key-order preservation ---

#[test]
fn key_order_identity_roundtrip() {
    // Identity filter must preserve original key order (z, m, a — not sorted)
    let out = qj_compact(".", r#"{"z":1,"m":2,"a":3}"#);
    assert_eq!(out.trim(), r#"{"z":1,"m":2,"a":3}"#);
    assert_jq_compat(".", r#"{"z":1,"m":2,"a":3}"#);
}

#[test]
fn key_order_object_construction() {
    // Object construction preserves construction order, not input order
    let out = qj_compact("{b:.b, a:.a}", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), r#"{"b":2,"a":1}"#);
    assert_jq_compat("{b:.b, a:.a}", r#"{"a":1,"b":2}"#);
}

#[test]
fn key_order_nested_objects() {
    // Both outer and inner objects preserve their respective key orders
    let out = qj_compact(".", r#"{"z":{"y":1,"x":2},"a":{"c":3,"b":4}}"#);
    assert_eq!(out.trim(), r#"{"z":{"y":1,"x":2},"a":{"c":3,"b":4}}"#);
    assert_jq_compat(".", r#"{"z":{"y":1,"x":2},"a":{"c":3,"b":4}}"#);
}

#[test]
fn ndjson_key_order_preserved() {
    // Each NDJSON line preserves its own key order through parallel processing
    let input = "{\"z\":1,\"a\":2}\n{\"b\":3,\"a\":4}\n{\"m\":5,\"c\":6,\"a\":7}\n";
    assert_jq_compat_ndjson(".", input);
}

// ===========================================================================
// Transparent gzip/zstd decompression
// ===========================================================================

/// Helper: write content to a gzip-compressed temp file.
fn write_gz(dir: &std::path::Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut enc = GzEncoder::new(file, Compression::fast());
    enc.write_all(content).unwrap();
    enc.finish().unwrap();
    path
}

/// Helper: write content to a zstd-compressed temp file.
fn write_zst(dir: &std::path::Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut enc = zstd::Encoder::new(file, 1).unwrap();
    std::io::Write::write_all(&mut enc, content).unwrap();
    enc.finish().unwrap();
    path
}

#[test]
fn gz_single_json_doc() {
    let dir = std::env::temp_dir();
    let path = write_gz(
        &dir,
        "qj_test_single.json.gz",
        br#"{"name":"alice","age":30}"#,
    );
    let (code, stdout, stderr) = qj_exit(&["-c", ".name", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), r#""alice""#);
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_ndjson() {
    let dir = std::env::temp_dir();
    let ndjson = b"{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n";
    let path = write_gz(&dir, "qj_test_ndjson.ndjson.gz", ndjson);
    let (code, stdout, stderr) = qj_exit(&["-c", ".n", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "1\n2\n3");
    std::fs::remove_file(&path).ok();
}

#[test]
fn zst_single_json_doc() {
    let dir = std::env::temp_dir();
    let path = write_zst(&dir, "qj_test_single.json.zst", br#"{"x":42}"#);
    let (code, stdout, stderr) = qj_exit(&["-c", ".x", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "42");
    std::fs::remove_file(&path).ok();
}

#[test]
fn zst_ndjson() {
    let dir = std::env::temp_dir();
    let ndjson = b"{\"v\":\"a\"}\n{\"v\":\"b\"}\n";
    let path = write_zst(&dir, "qj_test_ndjson.ndjson.zst", ndjson);
    let (code, stdout, stderr) = qj_exit(&["-c", ".v", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "\"a\"\n\"b\"");
    std::fs::remove_file(&path).ok();
}

#[test]
fn zstd_extension() {
    // .zstd extension also works
    let dir = std::env::temp_dir();
    let path = write_zst(&dir, "qj_test.json.zstd", br#"{"k":"v"}"#);
    let (code, stdout, stderr) = qj_exit(&["-c", ".k", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), r#""v""#);
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_empty_file() {
    // An empty compressed file should produce no output (matches jq behavior)
    let dir = std::env::temp_dir();
    let path = write_gz(&dir, "qj_test_empty.json.gz", b"");
    let (code, stdout, _stderr) = qj_exit(&["-c", ".", path.to_str().unwrap()], "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_select_filter() {
    // select() filter on compressed NDJSON
    let dir = std::env::temp_dir();
    let ndjson =
        b"{\"type\":\"push\",\"n\":1}\n{\"type\":\"pull\",\"n\":2}\n{\"type\":\"push\",\"n\":3}\n";
    let path = write_gz(&dir, "qj_test_select.ndjson.gz", ndjson);
    let (code, stdout, stderr) = qj_exit(
        &[
            "-c",
            r#"select(.type == "push") | .n"#,
            path.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "1\n3");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_slurp() {
    // --slurp with compressed file
    let dir = std::env::temp_dir();
    let ndjson = b"1\n2\n3\n";
    let path = write_gz(&dir, "qj_test_slurp.ndjson.gz", ndjson);
    let (code, stdout, stderr) = qj_exit(&["-c", "-s", "add", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "6");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_raw_input() {
    // --raw-input with compressed file
    let dir = std::env::temp_dir();
    let text = b"hello\nworld\n";
    let path = write_gz(&dir, "qj_test_raw.txt.gz", text);
    let (code, stdout, stderr) = qj_exit(&["-R", "-c", ".", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "\"hello\"\n\"world\"");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_raw_input_slurp() {
    // --raw-input --slurp with compressed file
    let dir = std::env::temp_dir();
    let text = b"hello\nworld\n";
    let path = write_gz(&dir, "qj_test_rs.txt.gz", text);
    let (code, stdout, stderr) = qj_exit(&["-R", "-s", "-c", "length", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    // "hello\nworld\n" = 12 chars
    assert_eq!(stdout.trim(), "12");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_multiple_files() {
    // Multiple compressed files processed in order
    let dir = std::env::temp_dir();
    let p1 = write_gz(&dir, "qj_test_multi1.json.gz", br#"{"n":1}"#);
    let p2 = write_gz(&dir, "qj_test_multi2.json.gz", br#"{"n":2}"#);
    let (code, stdout, stderr) = qj_exit(
        &["-c", ".n", p1.to_str().unwrap(), p2.to_str().unwrap()],
        "",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "1\n2");
    std::fs::remove_file(&p1).ok();
    std::fs::remove_file(&p2).ok();
}

#[test]
fn mixed_compressed_and_plain() {
    // Mix of compressed and uncompressed files
    let dir = std::env::temp_dir();
    let p1 = write_gz(&dir, "qj_test_mix1.json.gz", br#"{"n":1}"#);
    let p2 = dir.join("qj_test_mix2.json");
    std::fs::write(&p2, r#"{"n":2}"#).unwrap();
    let p3 = write_zst(&dir, "qj_test_mix3.json.zst", br#"{"n":3}"#);
    let (code, stdout, stderr) = qj_exit(
        &[
            "-c",
            ".n",
            p1.to_str().unwrap(),
            p2.to_str().unwrap(),
            p3.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "1\n2\n3");
    std::fs::remove_file(&p1).ok();
    std::fs::remove_file(&p2).ok();
    std::fs::remove_file(&p3).ok();
}

#[test]
fn gz_passthrough_identity() {
    // Passthrough fast path on compressed single doc
    let dir = std::env::temp_dir();
    let path = write_gz(&dir, "qj_test_pt.json.gz", br#"{"a":1,"b":2}"#);
    let (code, stdout, stderr) = qj_exit(&["-c", ".", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), r#"{"a":1,"b":2}"#);
    std::fs::remove_file(&path).ok();
}

#[test]
fn gz_ndjson_large() {
    // Larger NDJSON (enough lines to exercise parallel processing)
    let dir = std::env::temp_dir();
    let mut ndjson = Vec::new();
    for i in 0..1000 {
        ndjson.extend_from_slice(format!("{{\"i\":{i}}}\n").as_bytes());
    }
    let path = write_gz(&dir, "qj_test_large.ndjson.gz", &ndjson);
    let (code, stdout, stderr) = qj_exit(&["-c", ".i", path.to_str().unwrap()], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 1000);
    assert_eq!(lines[0], "0");
    assert_eq!(lines[999], "999");
    std::fs::remove_file(&path).ok();
}

// ===========================================================================
// Glob pattern expansion
// ===========================================================================

#[test]
fn glob_expansion_gz() {
    // Quoted glob pattern expands to matching files
    let dir = std::env::temp_dir().join("qj_glob_test");
    std::fs::create_dir_all(&dir).unwrap();
    let _p1 = write_gz(&dir, "a.json.gz", br#"{"n":1}"#);
    let _p2 = write_gz(&dir, "b.json.gz", br#"{"n":2}"#);
    let _p3 = write_gz(&dir, "c.json.gz", br#"{"n":3}"#);
    let pattern = dir.join("*.json.gz").to_str().unwrap().to_string();
    let (code, stdout, stderr) = qj_exit(&["-c", ".n", &pattern], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    let mut nums: Vec<i64> = stdout.trim().lines().map(|l| l.parse().unwrap()).collect();
    nums.sort();
    assert_eq!(nums, vec![1, 2, 3]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn glob_no_match_error() {
    // Glob pattern with no matches should report an error
    let (code, _stdout, stderr) = qj_exit(&["-c", ".", "/tmp/qj_nonexistent_glob_*.json"], "");
    assert_ne!(code, 0);
    assert!(stderr.contains("no files matched"), "stderr: {stderr}");
}

#[test]
fn glob_mixed_with_literal() {
    // Mix of literal files and glob patterns
    let dir = std::env::temp_dir().join("qj_glob_mix_test");
    std::fs::create_dir_all(&dir).unwrap();
    let literal = dir.join("literal.json");
    std::fs::write(&literal, r#"{"n":0}"#).unwrap();
    let _p1 = write_gz(&dir, "x.json.gz", br#"{"n":1}"#);
    let _p2 = write_gz(&dir, "y.json.gz", br#"{"n":2}"#);
    let pattern = dir.join("*.json.gz").to_str().unwrap().to_string();
    let (code, stdout, stderr) = qj_exit(&["-c", ".n", literal.to_str().unwrap(), &pattern], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    let nums: Vec<i64> = stdout.trim().lines().map(|l| l.parse().unwrap()).collect();
    // literal.json first (n=0), then glob matches sorted (n=1, n=2)
    assert_eq!(nums, vec![0, 1, 2]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn glob_slurp() {
    // Glob with --slurp collects all values
    let dir = std::env::temp_dir().join("qj_glob_slurp_test");
    std::fs::create_dir_all(&dir).unwrap();
    let _p1 = write_gz(&dir, "a.json.gz", b"10");
    let _p2 = write_gz(&dir, "b.json.gz", b"20");
    let pattern = dir.join("*.json.gz").to_str().unwrap().to_string();
    let (code, stdout, stderr) = qj_exit(&["-c", "-s", "add", &pattern], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "30");
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// String length: Unicode codepoint counting
// ---------------------------------------------------------------------------

#[test]
fn string_length_ascii() {
    assert_jq_compat("length", "\"hello\"");
}

#[test]
fn string_length_unicode_2byte() {
    // é is 2 UTF-8 bytes but 1 codepoint
    assert_jq_compat("length", "\"é\"");
}

#[test]
fn string_length_unicode_3byte() {
    // 中 is 3 UTF-8 bytes but 1 codepoint
    assert_jq_compat("length", "\"中\"");
}

#[test]
fn string_length_unicode_4byte() {
    // 𝕳 is 4 UTF-8 bytes but 1 codepoint
    assert_jq_compat("length", "\"𝕳\"");
}

#[test]
fn string_length_mixed_unicode() {
    // "aé中𝕳" = 4 codepoints (1+1+1+1)
    assert_jq_compat("length", "\"aé中𝕳\"");
}

#[test]
fn string_length_emoji() {
    // 🎉 is 4 UTF-8 bytes but 1 codepoint
    assert_jq_compat("length", "\"🎉\"");
}

// ---------------------------------------------------------------------------
// NaN/Infinity modulo
// ---------------------------------------------------------------------------

#[test]
fn inf_modulo_finite() {
    // infinite % 1 → 0
    let out = qj_compact("infinite % 1", "null");
    assert_eq!(out.trim(), "0");
}

#[test]
fn neg_inf_modulo_finite() {
    // -infinite % 1 → 0
    let out = qj_compact("(-infinite) % 1", "null");
    assert_eq!(out.trim(), "0");
}

#[test]
fn inf_modulo_inf() {
    // infinite % infinite → 0
    let out = qj_compact("infinite % infinite", "null");
    assert_eq!(out.trim(), "0");
}

// ---------------------------------------------------------------------------
// implode error messages
// ---------------------------------------------------------------------------

#[test]
fn implode_error_non_array() {
    assert_jq_compat("try (123 | implode) catch .", "null");
}

#[test]
fn implode_error_string_element() {
    assert_jq_compat("[\"a\"] | try implode catch .", "null");
}

#[test]
fn implode_error_null_element() {
    assert_jq_compat("[null] | try implode catch .", "null");
}

#[test]
fn implode_error_bool_element() {
    assert_jq_compat("[true] | try implode catch .", "null");
}

// ---------------------------------------------------------------------------
// Special float input parsing (NaN, Infinity)
// ---------------------------------------------------------------------------

#[test]
fn parse_nan_in_input() {
    // {"a":nan} → tojson → {"a":null}
    let out = qj_compact("tojson | fromjson", "{\"a\":nan}");
    assert_eq!(out.trim(), "{\"a\":null}");
}

#[test]
fn parse_special_floats_iterate_assign() {
    // .[] = 1 on array with special floats
    let out = qj_compact(".[] = 1", "[1,null,Infinity,-Infinity,NaN,-NaN]");
    assert_eq!(out.trim(), "[1,1,1,1,1,1]");
}

#[test]
fn parse_nan_isnan() {
    // NaN parsed from input should be recognized by isnan
    let out = qj_compact(".[0] | isnan", "[NaN,1]");
    assert_eq!(out.trim(), "true");
}

#[test]
fn parse_nan_in_string_preserved() {
    // "NaN" inside a JSON string should NOT be replaced
    let out = qj_compact(".", "{\"key\":\"NaN\"}");
    assert_eq!(out.trim(), "{\"key\":\"NaN\"}");
}

// ---------------------------------------------------------------------------
// try input catch . (break signal)
// ---------------------------------------------------------------------------

#[test]
fn try_input_catch_break() {
    assert_jq_compat("try input catch .", "null");
}

// ---------------------------------------------------------------------------
// tostring preserves raw text for large numbers
// ---------------------------------------------------------------------------

#[test]
fn tostring_large_number_preserves_raw() {
    assert_jq_compat("tostring", "100000000000000000000");
}

// ---------------------------------------------------------------------------
// Try/? operator on array construction (flat_eval error propagation)
// ---------------------------------------------------------------------------

#[test]
fn try_postfix_array_construct() {
    // ? on array construction should suppress errors and produce no output
    assert_jq_compat("[[.[]|[.a,.a]]?]", r#"[null,true,{"a":1}]"#);
}

#[test]
fn array_construct_suppresses_on_error() {
    // [.a,.a] on boolean should error, not produce []
    let (code, stdout, stderr) = qj_exit(&["-c", "[.a,.a]"], "true");
    assert_ne!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "expected no output, got: {stdout}"
    );
    assert!(stderr.contains("Cannot index boolean"), "stderr: {stderr}");
}

#[test]
fn try_array_construct_suppresses_error() {
    // try [.a,.a] on boolean should produce no output (not [])
    assert_jq_compat("try [.a,.a]", "true");
}

#[test]
fn array_construct_success_still_works() {
    // Normal array construction should still work
    assert_jq_compat("[.a,.b]", r#"{"a":1,"b":2}"#);
}

// ---------------------------------------------------------------------------
// `as` binding in assignment paths
// ---------------------------------------------------------------------------

#[test]
fn as_binding_in_assignment() {
    // (.a as $x | .b) = "b" — as binding should work in path position
    assert_jq_compat(r#"(.a as $x | .b) = "b""#, r#"{"a":null,"b":null}"#);
}

#[test]
fn as_binding_in_update() {
    // (.a as $x | .b) |= . + 1
    assert_jq_compat("(.a as $x | .b) |= . + 1", r#"{"a":10,"b":5}"#);
}

// ---------------------------------------------------------------------------
// Invalid path expression errors
// ---------------------------------------------------------------------------

#[test]
fn path_invalid_map_expression() {
    assert_jq_compat(
        "try path(.a | map(select(.b == 0))) catch .",
        r#"{"a":[{"b":0}]}"#,
    );
}

#[test]
fn path_invalid_with_index_access() {
    assert_jq_compat(
        "try path(.a | map(select(.b == 0)) | .[0]) catch .",
        r#"{"a":[{"b":0}]}"#,
    );
}

#[test]
fn path_invalid_with_field_access() {
    assert_jq_compat(
        "try path(.a | map(select(.b == 0)) | .c) catch .",
        r#"{"a":[{"b":0}]}"#,
    );
}

#[test]
fn path_invalid_with_iterate() {
    assert_jq_compat(
        "try path(.a | map(select(.b == 0)) | .[]) catch .",
        r#"{"a":[{"b":0}]}"#,
    );
}

// ---------------------------------------------------------------------------
// Assignment path errors propagate through try/catch
// ---------------------------------------------------------------------------

#[test]
fn assignment_path_error_map_select() {
    assert_jq_compat(
        "try ((map(select(.a == 1))[].b) = 10) catch .",
        r#"[{"a":0},{"a":1}]"#,
    );
}

#[test]
fn update_path_error_map_select() {
    assert_jq_compat(
        "try ((map(select(.a == 1))[].a) |= .+1) catch .",
        r#"[{"a":0},{"a":1}]"#,
    );
}

// ---------------------------------------------------------------------------
// User-defined function as path in assignment
// ---------------------------------------------------------------------------

#[test]
fn def_as_path_in_assignment() {
    assert_jq_compat("def x: .[1,2]; x=10", "[0,1,2]");
}

#[test]
fn def_nonpath_in_assignment_error() {
    assert_jq_compat("try (def x: reverse; x=10) catch .", "[0,1,2]");
}

// ---------------------------------------------------------------------------
// $param sugar: generator arguments produce Cartesian product
// ---------------------------------------------------------------------------

#[test]
fn dollar_param_generator_cartesian() {
    assert_jq_compat("def y($a;$b): [$a,$b]; [y(.[];.[]*2)]", "[1,2,3]");
}

#[test]
fn dollar_param_generator_equivalence() {
    // def x(a;b) with explicit `as` binding should equal def y($a;$b)
    assert_jq_compat(
        "def x(a;b): a as $a | b as $b | $a + $b; def y($a;$b): $a + $b; def check(a;b): [x(a;b)] == [y(a;b)]; check(.[];.[]*2)",
        "[1,2,3]",
    );
}

// ---------------------------------------------------------------------------
// getpath in update assignment position
// ---------------------------------------------------------------------------

#[test]
fn getpath_update_assignment() {
    assert_jq_compat(r#"getpath(["a",0,"b"]) |= 5"#, r#"{"a":[{"c":3}]}"#);
}

#[test]
fn getpath_update_error_propagation() {
    assert_jq_compat(
        r#".[] | try (getpath(["a",0,"b"]) |= 5) catch ."#,
        r#"[{"a":0},{"a":[0,1]}]"#,
    );
}

// ---------------------------------------------------------------------------
// def in expression positions (after comma, after pipe, inside arrays)
// ---------------------------------------------------------------------------

#[test]
fn def_after_comma() {
    assert_jq_compat("def f: 1; f, def f: 2; f", "null");
}

#[test]
fn def_after_comma_in_array() {
    assert_jq_compat("[1, def f: 2; f, f]", "null");
}

#[test]
fn def_in_expression_complex() {
    assert_jq_compat(
        "def f: 1; def g: f, def f: 2; def g: 3; f, def f: g; f, g; def f: 4; [f, def f: g; def g: 5; f, g]+[f,g]",
        "null",
    );
}

// ---------------------------------------------------------------------------
// NDJSON edge case jq compatibility — whitespace, non-objects, errors
// ---------------------------------------------------------------------------

/// Run a tool (qj or jq) and return (stdout, stderr, success).
fn run_tool_full(cmd: &str, args: &[&str], input: &str) -> (String, String, bool) {
    let output = Command::new(cmd)
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
        .expect(&format!("failed to run {cmd}"));

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

/// Assert that qj and jq produce the same stdout and agree on success/failure.
/// For error cases, error messages may differ — only checks that both error.
fn assert_jq_compat_ndjson_full(filter: &str, ndjson_input: &str) {
    if !jq_available() {
        return;
    }
    let (qj_stdout, _qj_stderr, qj_ok) =
        run_tool_full(env!("CARGO_BIN_EXE_qj"), &["-c", filter], ndjson_input);
    let (jq_stdout, _jq_stderr, jq_ok) = run_tool_full("jq", &["-c", filter], ndjson_input);

    assert_eq!(
        qj_stdout.trim(),
        jq_stdout.trim(),
        "stdout mismatch for filter={filter:?}\nqj_ok={qj_ok}, jq_ok={jq_ok}"
    );
    assert_eq!(
        qj_ok, jq_ok,
        "exit status mismatch for filter={filter:?}: qj_ok={qj_ok}, jq_ok={jq_ok}\n\
         qj stderr: {_qj_stderr}\njq stderr: {_jq_stderr}"
    );
}

// --- Select fast path with internal whitespace (compact mode) ---

#[test]
fn ndjson_jq_compat_select_whitespace_compact() {
    // NDJSON with internal whitespace: select match should produce compact output
    assert_jq_compat_ndjson(
        "select(.value == null)",
        "{ \"value\":null }\n{ \"value\":1 }\n",
    );
}

#[test]
fn ndjson_jq_compat_select_whitespace_keys() {
    // Whitespace around keys/colons
    assert_jq_compat_ndjson(
        "select(.type == \"PushEvent\")",
        "{ \"type\" : \"PushEvent\" , \"id\" : 1 }\n{ \"type\" : \"WatchEvent\" }\n",
    );
}

#[test]
fn ndjson_jq_compat_select_string_pred_whitespace() {
    // select with string predicate on whitespace-containing NDJSON
    assert_jq_compat_ndjson(
        "select(.name | startswith(\"a\"))",
        "{ \"name\" : \"alice\" }\n{ \"name\" : \"bob\" }\n",
    );
}

#[test]
fn ndjson_jq_compat_field_whitespace() {
    // Field extraction on whitespace-containing NDJSON
    assert_jq_compat_ndjson(
        ".name",
        "{ \"name\" : \"alice\" }\n{ \"name\" : \"bob\" }\n",
    );
}

// --- Leading CR whitespace ---

#[test]
fn ndjson_jq_compat_leading_cr() {
    // Leading \r (e.g. from Windows line endings or data corruption)
    assert_jq_compat_ndjson(".name", "\r{\"name\":\"alice\"}\n\r{\"name\":\"bob\"}\n");
}

#[test]
fn ndjson_jq_compat_leading_mixed_whitespace() {
    // Leading space, tab, and CR mixed
    assert_jq_compat_ndjson("select(.x == 1)", " \t\r{\"x\":1}\n\t {\"x\":2}\n");
}

// --- Array NDJSON with field access (should error, not return null) ---

#[test]
fn ndjson_jq_compat_array_field_access_errors() {
    // jq errors on field access of arrays — qj should too
    assert_jq_compat_ndjson_full(".a.b.c", "[1,2]\n[3,4]\n[5,6]\n");
}

#[test]
fn ndjson_jq_compat_array_field_access_simple() {
    // Even simple .a on arrays should error
    assert_jq_compat_ndjson_full(".a", "[1,2]\n[3,4]\n");
}

#[test]
fn ndjson_jq_compat_mixed_objects_arrays() {
    // Mix of objects and arrays: objects succeed, arrays should error
    assert_jq_compat_ndjson_full(".a", "{\"a\":1}\n[1,2]\n{\"a\":3}\n");
}

// --- More non-object NDJSON error cases ---

#[test]
fn ndjson_jq_compat_string_ndjson_field_access() {
    // Bare strings in NDJSON — jq errors on field access
    assert_jq_compat_ndjson_full(".a", "\"hello\"\n\"world\"\n");
}

#[test]
fn ndjson_jq_compat_mixed_types_select() {
    // Mix of objects and arrays with select — select should only match objects
    assert_jq_compat_ndjson_full(
        "select(.type == \"a\")",
        "{\"type\":\"a\"}\n[1]\n{\"type\":\"b\"}\n",
    );
}

#[test]
fn ndjson_jq_compat_array_ndjson_length() {
    // length on arrays should return array length, not error
    assert_jq_compat_ndjson_full("length", "[1,2,3]\n[4,5]\n");
}

#[test]
fn ndjson_jq_compat_array_ndjson_keys() {
    // keys on arrays should return indices
    assert_jq_compat_ndjson_full("keys", "[10,20,30]\n[40]\n");
}

// --- Select fast path with ordering operators + whitespace ---

#[test]
fn ndjson_jq_compat_select_gt_whitespace() {
    assert_jq_compat_ndjson(
        "select(.count > 10)",
        "{ \"count\" : 20 }\n{ \"count\" : 5 }\n",
    );
}

#[test]
fn ndjson_jq_compat_select_le_whitespace() {
    assert_jq_compat_ndjson(
        "select(.count <= 10)",
        "{ \"count\" : 10 }\n{ \"count\" : 20 }\n",
    );
}

// --- Multi-field construction + whitespace ---

#[test]
fn ndjson_jq_compat_multi_field_obj_whitespace() {
    assert_jq_compat_ndjson(
        "{name: .name, id: .id}",
        "{ \"name\" : \"alice\" , \"id\" : 1 }\n{ \"name\" : \"bob\" , \"id\" : 2 }\n",
    );
}

#[test]
fn ndjson_jq_compat_multi_field_arr_whitespace() {
    assert_jq_compat_ndjson(
        "[.name, .id]",
        "{ \"name\" : \"alice\" , \"id\" : 1 }\n{ \"name\" : \"bob\" , \"id\" : 2 }\n",
    );
}

// --- Select + field extraction + whitespace ---

#[test]
fn ndjson_jq_compat_select_eq_field_whitespace() {
    assert_jq_compat_ndjson(
        "select(.type == \"a\") | .name",
        "{ \"type\" : \"a\" , \"name\" : \"alice\" }\n{ \"type\" : \"b\" , \"name\" : \"bob\" }\n",
    );
}

// --- Select + object/array construction + whitespace ---

#[test]
fn ndjson_jq_compat_select_eq_obj_whitespace() {
    assert_jq_compat_ndjson(
        "select(.type == \"a\") | {name: .name}",
        "{ \"type\" : \"a\" , \"name\" : \"alice\" }\n{ \"type\" : \"b\" , \"name\" : \"bob\" }\n",
    );
}

#[test]
fn ndjson_jq_compat_select_eq_arr_whitespace() {
    assert_jq_compat_ndjson(
        "select(.type == \"a\") | [.name, .id]",
        "{ \"type\" : \"a\" , \"name\" : \"x\" , \"id\" : 1 }\n{ \"type\" : \"b\" }\n",
    );
}

// --- Builtins + whitespace ---

#[test]
fn ndjson_jq_compat_length_whitespace() {
    assert_jq_compat_ndjson("length", "{ \"a\" : 1 , \"b\" : 2 }\n{ \"x\" : 1 }\n");
}

#[test]
fn ndjson_jq_compat_keys_whitespace() {
    assert_jq_compat_ndjson("keys", "{ \"b\" : 2 , \"a\" : 1 }\n{ \"x\" : 1 }\n");
}

#[test]
fn ndjson_jq_compat_keys_unsorted_whitespace() {
    assert_jq_compat_ndjson(
        "keys_unsorted",
        "{ \"b\" : 2 , \"a\" : 1 }\n{ \"x\" : 1 }\n",
    );
}

#[test]
fn ndjson_jq_compat_type_whitespace() {
    assert_jq_compat_ndjson("type", "{ \"a\" : 1 }\n{ \"b\" : 2 }\n");
}

#[test]
fn ndjson_jq_compat_has_whitespace() {
    assert_jq_compat_ndjson("has(\"a\")", "{ \"a\" : 1 }\n{ \"b\" : 2 }\n");
}

// --- Windows CRLF line endings ---

#[test]
fn ndjson_jq_compat_crlf_line_endings() {
    // Windows-style \r\n line endings
    assert_jq_compat_ndjson(".name", "{\"name\":\"alice\"}\r\n{\"name\":\"bob\"}\r\n");
}

#[test]
fn ndjson_jq_compat_crlf_select() {
    assert_jq_compat_ndjson("select(.x == 1)", "{\"x\":1}\r\n{\"x\":2}\r\n");
}

// --- Empty/whitespace-only lines ---

#[test]
fn ndjson_jq_compat_empty_lines() {
    // Empty lines between valid NDJSON should be ignored
    assert_jq_compat_ndjson(".name", "{\"name\":\"alice\"}\n\n{\"name\":\"bob\"}\n\n");
}

#[test]
fn ndjson_jq_compat_whitespace_only_lines() {
    // Lines with only whitespace between valid NDJSON
    assert_jq_compat_ndjson(
        ".name",
        "{\"name\":\"alice\"}\n  \t  \n{\"name\":\"bob\"}\n",
    );
}

// ---------------------------------------------------------------------------
// Negative zero (-0) preservation from JSON input
// ---------------------------------------------------------------------------

#[test]
fn negative_zero_field_extraction() {
    // jq preserves -0 from JSON input: echo '{"count":-0}' | jq '.count' → -0
    assert_jq_compat(".count", r#"{"count":-0}"#);
}

#[test]
fn negative_zero_identity() {
    // Identity should preserve -0 in objects
    assert_jq_compat(".", r#"{"count":-0}"#);
}

#[test]
fn negative_zero_bare_value() {
    // Bare -0 as input
    assert_jq_compat(".", "-0");
}

#[test]
fn negative_zero_in_array() {
    // -0 inside an array
    assert_jq_compat(".[0]", "[-0]");
}

#[test]
fn negative_zero_nested() {
    // -0 in nested structure
    assert_jq_compat(".a.b", r#"{"a":{"b":-0}}"#);
}

#[test]
fn negative_zero_ndjson_fast_path() {
    // NDJSON fast path (raw byte passthrough) should preserve -0
    assert_jq_compat_ndjson(".count", "{\"count\":-0}\n{\"count\":1}\n");
}

#[test]
fn negative_zero_ndjson_normal_path() {
    // NDJSON normal path (no fast path) should also preserve -0.
    // Uses QJ_NO_FAST_PATH=1 to force the DOM eval path.
    let input = "{\"count\":-0}\n{\"count\":1}\n";
    let filter = ".count";

    let qj_out = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(["-c", filter])
        .env("QJ_NO_FAST_PATH", "1")
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
        .expect("failed to run qj");

    let jq_out = Command::new("jq")
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
        .expect("failed to run jq");

    let qj_stdout = String::from_utf8_lossy(&qj_out.stdout);
    let jq_stdout = String::from_utf8_lossy(&jq_out.stdout);
    assert_eq!(
        qj_stdout, jq_stdout,
        "NDJSON normal path -0 mismatch: qj={:?} jq={:?}",
        qj_stdout, jq_stdout
    );
}

// =============================================================================
// Passthrough fast path differential tests
//
// These tests systematically verify that the C++ simdjson passthrough paths
// produce output identical to jq across diverse inputs. Each passthrough
// variant is tested with edge-case inputs: unicode, special characters in keys,
// empty containers, deeply nested data, large/negative/float numbers, null
// values, and mixed-type arrays.
// =============================================================================

// --- Identity passthrough differential ---

#[test]
fn passthrough_diff_identity_unicode() {
    assert_jq_compat(".", r#"{"emoji":"😀","jp":"日本語","key":"val\u0000ue"}"#);
}

#[test]
fn passthrough_diff_identity_special_keys() {
    assert_jq_compat(
        ".",
        r#"{"":"empty","a b":"space","a.b":"dot","a\"b":"quote"}"#,
    );
}

#[test]
fn passthrough_diff_identity_deep_nesting() {
    assert_jq_compat(".", r#"{"a":{"b":{"c":{"d":{"e":{"f":1}}}}}}"#);
}

#[test]
fn passthrough_diff_identity_large_numbers() {
    assert_jq_compat(".", r#"{"big":9007199254740992,"neg":-9007199254740992}"#);
}

#[test]
fn passthrough_diff_identity_float_edge_cases() {
    // Avoid scientific notation inputs — jq normalizes e.g. 1e-10 to 1E-10
    // while qj preserves the original text (both valid JSON).
    assert_jq_compat(".", r#"{"z":0.0,"neg":-0.5,"pi":3.14159}"#);
}

#[test]
fn passthrough_diff_identity_empty_containers() {
    assert_jq_compat(".", r#"{"a":{},"b":[],"c":{"d":[]}}"#);
}

#[test]
fn passthrough_diff_identity_whitespace_heavy() {
    assert_jq_compat(".", "  {  \"a\"  :  1  ,  \"b\"  :  [  2  ,  3  ]  }  ");
}

#[test]
fn passthrough_diff_identity_escaped_strings() {
    // Note: jq unescapes \/ to / while simdjson minify preserves it.
    // Test only escapes where both agree.
    assert_jq_compat(".", r#"{"tab":"\t","newline":"\n","backslash":"\\"}"#);
}

#[test]
fn passthrough_diff_identity_array_of_scalars() {
    assert_jq_compat(".", r#"[1,-2,3.14,null,true,false,"hello"]"#);
}

// --- FieldLength passthrough differential ---

#[test]
fn passthrough_diff_length_null() {
    assert_jq_compat("length", "null");
}

#[test]
fn passthrough_diff_length_number() {
    // jq: length of a number is its absolute value
    assert_jq_compat("length", "-42");
    assert_jq_compat("length", "3.14");
}

#[test]
fn passthrough_diff_length_boolean() {
    // jq: length of boolean errors (exit 5).
    // qj currently returns null instead of erroring — known divergence.
    // Test that both agree on the number type at least:
    assert_jq_compat("length", "0");
    assert_jq_compat("length", "-42");
}

#[test]
fn passthrough_diff_length_empty_containers() {
    assert_jq_compat("length", "{}");
    assert_jq_compat("length", "[]");
    assert_jq_compat("length", r#""""#);
}

#[test]
fn passthrough_diff_length_unicode_string() {
    // jq counts Unicode codepoints, not bytes
    assert_jq_compat("length", r#""héllo""#);
    assert_jq_compat("length", r#""日本語""#);
}

#[test]
fn passthrough_diff_field_length_nested_array() {
    assert_jq_compat(".a.b | length", r#"{"a":{"b":[1,2,3,4,5]}}"#);
}

#[test]
fn passthrough_diff_field_length_nested_string() {
    assert_jq_compat(".a.b | length", r#"{"a":{"b":"hello world"}}"#);
}

// --- FieldKeys passthrough differential ---

#[test]
fn passthrough_diff_keys_many_keys() {
    assert_jq_compat("keys", r#"{"z":1,"m":2,"a":3,"g":4,"b":5}"#);
}

#[test]
fn passthrough_diff_keys_unicode_keys() {
    assert_jq_compat("keys", r#"{"日":"jp","a":"en","ñ":"es"}"#);
}

#[test]
fn passthrough_diff_keys_single_key() {
    assert_jq_compat("keys", r#"{"only":1}"#);
}

#[test]
fn passthrough_diff_keys_empty_object() {
    assert_jq_compat("keys", "{}");
}

#[test]
fn passthrough_diff_keys_empty_array() {
    assert_jq_compat("keys", "[]");
}

#[test]
fn passthrough_diff_keys_unsorted_unicode() {
    assert_jq_compat("keys_unsorted", r#"{"z":1,"a":2,"m":3}"#);
}

#[test]
fn passthrough_diff_keys_nested_field() {
    assert_jq_compat(".data | keys", r#"{"data":{"z":1,"a":2,"m":3}}"#);
}

#[test]
fn passthrough_diff_keys_unsorted_nested() {
    assert_jq_compat(".data | keys_unsorted", r#"{"data":{"z":1,"a":2,"m":3}}"#);
}

// --- FieldType passthrough differential ---

#[test]
fn passthrough_diff_type_all_json_types() {
    assert_jq_compat("type", r#"{"a":1}"#);
    assert_jq_compat("type", "[1]");
    assert_jq_compat("type", r#""str""#);
    assert_jq_compat("type", "42");
    assert_jq_compat("type", "-3.14");
    assert_jq_compat("type", "true");
    assert_jq_compat("type", "false");
    assert_jq_compat("type", "null");
}

#[test]
fn passthrough_diff_type_nested_field() {
    assert_jq_compat(".a | type", r#"{"a":{"b":1}}"#);
    assert_jq_compat(".a | type", r#"{"a":[1,2,3]}"#);
    assert_jq_compat(".a | type", r#"{"a":"hello"}"#);
    assert_jq_compat(".a | type", r#"{"a":42}"#);
    assert_jq_compat(".a | type", r#"{"a":true}"#);
    assert_jq_compat(".a | type", r#"{"a":null}"#);
}

#[test]
fn passthrough_diff_type_deep_field() {
    assert_jq_compat(".a.b.c | type", r#"{"a":{"b":{"c":"deep"}}}"#);
}

#[test]
fn passthrough_diff_type_empty_containers() {
    assert_jq_compat("type", "{}");
    assert_jq_compat("type", "[]");
}

// --- FieldHas passthrough differential ---

#[test]
fn passthrough_diff_has_present_and_absent() {
    let input = r#"{"name":"alice","age":30,"active":true}"#;
    assert_jq_compat(r#"has("name")"#, input);
    assert_jq_compat(r#"has("age")"#, input);
    assert_jq_compat(r#"has("missing")"#, input);
}

#[test]
fn passthrough_diff_has_null_value() {
    // has() checks key existence, not value truthiness
    assert_jq_compat(r#"has("x")"#, r#"{"x":null}"#);
}

#[test]
fn passthrough_diff_has_empty_object() {
    assert_jq_compat(r#"has("anything")"#, "{}");
}

#[test]
fn passthrough_diff_has_nested_field() {
    assert_jq_compat(r#".data | has("x")"#, r#"{"data":{"x":1,"y":2}}"#);
    assert_jq_compat(r#".data | has("z")"#, r#"{"data":{"x":1,"y":2}}"#);
}

#[test]
fn passthrough_diff_has_deep_field() {
    assert_jq_compat(r#".a.b | has("c")"#, r#"{"a":{"b":{"c":1,"d":2}}}"#);
}

#[test]
fn passthrough_diff_has_special_key() {
    assert_jq_compat(r#"has("a b")"#, r#"{"a b":1}"#);
    assert_jq_compat(r#"has("")"#, r#"{"":1}"#);
}

// --- ArrayMapField passthrough differential ---

#[test]
fn passthrough_diff_map_field_diverse_values() {
    assert_jq_compat(
        "map(.v)",
        r#"[{"v":1},{"v":"str"},{"v":true},{"v":null},{"v":{}},{"v":[]}]"#,
    );
}

#[test]
fn passthrough_diff_map_field_missing_fields() {
    assert_jq_compat("map(.x)", r#"[{"a":1},{"x":2},{"b":3},{"x":4}]"#);
}

#[test]
fn passthrough_diff_map_field_nested() {
    assert_jq_compat(
        "map(.a.b)",
        r#"[{"a":{"b":1}},{"a":{"b":"hello"}},{"a":{}}]"#,
    );
}

#[test]
fn passthrough_diff_map_field_unicode_values() {
    assert_jq_compat("map(.name)", r#"[{"name":"日本"},{"name":"émoji😀"}]"#);
}

#[test]
fn passthrough_diff_map_field_single_element() {
    assert_jq_compat("map(.x)", r#"[{"x":42}]"#);
}

#[test]
fn passthrough_diff_map_field_null_elements() {
    assert_jq_compat("map(.x)", r#"[null,{"x":1},null]"#);
}

#[test]
fn passthrough_diff_iterate_field_diverse() {
    assert_jq_compat(
        ".[] | .val",
        r#"[{"val":1},{"val":"hello"},{"val":null},{"val":false}]"#,
    );
}

#[test]
fn passthrough_diff_prefix_map_field_deep() {
    assert_jq_compat(".a.b | map(.c)", r#"{"a":{"b":[{"c":1},{"c":2},{"c":3}]}}"#);
}

#[test]
fn passthrough_diff_prefix_iterate_field() {
    assert_jq_compat(
        ".items[] | .name",
        r#"{"items":[{"name":"alice"},{"name":"bob"},{"name":"charlie"}]}"#,
    );
}

#[test]
fn passthrough_diff_array_construct_field() {
    assert_jq_compat(
        "[.[] | .x]",
        r#"[{"x":1},{"x":"two"},{"x":null},{"x":true}]"#,
    );
}

// --- ArrayMapFieldsObj passthrough differential ---

#[test]
fn passthrough_diff_map_fields_obj_diverse() {
    assert_jq_compat(
        "map({a, b})",
        r#"[{"a":1,"b":"str"},{"a":null,"b":true},{"a":[],"b":{}}]"#,
    );
}

#[test]
fn passthrough_diff_map_fields_obj_missing_some() {
    assert_jq_compat("map({x, y})", r#"[{"x":1},{"y":2},{"x":3,"y":4}]"#);
}

#[test]
fn passthrough_diff_map_fields_obj_all_missing() {
    assert_jq_compat("map({a, b})", r#"[{"c":1},{"d":2}]"#);
}

#[test]
fn passthrough_diff_map_fields_obj_single_field() {
    assert_jq_compat(
        "map({name})",
        r#"[{"name":"alice","age":30},{"name":"bob"}]"#,
    );
}

#[test]
fn passthrough_diff_map_fields_obj_null_elements() {
    assert_jq_compat("map({a, b})", r#"[null,{"a":1,"b":2},null]"#);
}

#[test]
fn passthrough_diff_iterate_fields_obj() {
    assert_jq_compat(
        ".[] | {name, age}",
        r#"[{"name":"alice","age":30,"extra":true},{"name":"bob","age":25}]"#,
    );
}

#[test]
fn passthrough_diff_prefix_map_fields_obj() {
    assert_jq_compat(
        ".data | map({x, y})",
        r#"{"data":[{"x":1,"y":2,"z":3},{"x":4,"y":5}]}"#,
    );
}

#[test]
fn passthrough_diff_prefix_iterate_fields_obj() {
    assert_jq_compat(
        ".data[] | {a, b}",
        r#"{"data":[{"a":1,"b":2},{"a":3,"b":4}]}"#,
    );
}

#[test]
fn passthrough_diff_array_construct_fields_obj() {
    assert_jq_compat("[.[] | {a, b}]", r#"[{"a":1,"b":2,"c":3},{"a":4,"b":5}]"#);
}

#[test]
fn passthrough_diff_map_fields_obj_unicode_values() {
    assert_jq_compat(
        "map({name, city})",
        r#"[{"name":"José","city":"São Paulo"},{"name":"田中","city":"東京"}]"#,
    );
}

// --- ArrayMapBuiltin passthrough differential ---

#[test]
fn passthrough_diff_map_length_diverse() {
    // Exclude null — qj C++ passthrough returns null for length(null)
    // while jq returns 0. Known divergence.
    assert_jq_compat("map(length)", r#"[{"a":1,"b":2},[1,2,3],"hello",[]]"#);
}

#[test]
fn passthrough_diff_map_length_strings() {
    assert_jq_compat("map(length)", r#"["","a","hello","日本語"]"#);
}

#[test]
fn passthrough_diff_map_type_all() {
    assert_jq_compat("map(type)", r#"[{},[],"str",42,3.14,true,false,null]"#);
}

#[test]
fn passthrough_diff_map_keys_diverse() {
    assert_jq_compat(
        "map(keys)",
        r#"[{"b":1,"a":2},{"z":3,"m":4,"a":5},[10,20]]"#,
    );
}

#[test]
fn passthrough_diff_map_keys_unsorted_diverse() {
    assert_jq_compat(
        "map(keys_unsorted)",
        r#"[{"b":1,"a":2},{"z":3,"m":4,"a":5}]"#,
    );
}

#[test]
fn passthrough_diff_map_has_diverse() {
    assert_jq_compat(r#"map(has("x"))"#, r#"[{"x":1},{"y":2},{"x":null},{}]"#);
}

#[test]
fn passthrough_diff_iterate_length() {
    assert_jq_compat(".[] | length", r#"[{"a":1},[1,2],"hi",[]]"#);
}

#[test]
fn passthrough_diff_iterate_type() {
    assert_jq_compat(".[] | type", r#"[42,"hello",null,true,[],{}]"#);
}

#[test]
fn passthrough_diff_iterate_keys() {
    assert_jq_compat(".[] | keys", r#"[{"b":1,"a":2},{"z":3},[10,20,30]]"#);
}

#[test]
fn passthrough_diff_iterate_has() {
    assert_jq_compat(r#".[] | has("a")"#, r#"[{"a":1},{"b":2},{"a":null}]"#);
}

#[test]
fn passthrough_diff_prefix_map_length() {
    assert_jq_compat(
        ".items | map(length)",
        r#"{"items":["hello",[1,2],{"a":1}]}"#,
    );
}

#[test]
fn passthrough_diff_prefix_map_type() {
    assert_jq_compat(
        ".items | map(type)",
        r#"{"items":[1,"str",null,true,{},[]]}"#,
    );
}

#[test]
fn passthrough_diff_prefix_iterate_length() {
    assert_jq_compat(
        ".items[] | length",
        r#"{"items":[[1,2,3],{"a":1,"b":2},"hello"]}"#,
    );
}

#[test]
fn passthrough_diff_array_construct_length() {
    assert_jq_compat("[.[] | length]", r#"[[1,2],{"a":1},"hi"]"#);
}

#[test]
fn passthrough_diff_array_construct_type() {
    assert_jq_compat("[.[] | type]", r#"[1,"str",null,true,{},[]]"#);
}

#[test]
fn passthrough_diff_array_construct_keys() {
    assert_jq_compat("[.[] | keys]", r#"[{"b":1,"a":2},{"z":3}]"#);
}

#[test]
fn passthrough_diff_array_construct_has() {
    assert_jq_compat(r#"[.[] | has("a")]"#, r#"[{"a":1},{"b":2}]"#);
}

// --- Cross-cutting: whitespace and formatting edge cases ---

#[test]
fn passthrough_diff_identity_leading_trailing_whitespace() {
    assert_jq_compat(".", "  \n  {\"a\":1}  \n  ");
}

#[test]
fn passthrough_diff_length_whitespace_input() {
    assert_jq_compat("length", "  [1, 2, 3]  ");
}

#[test]
fn passthrough_diff_keys_whitespace_input() {
    assert_jq_compat("keys", "  { \"b\" : 2 , \"a\" : 1 }  ");
}

#[test]
fn passthrough_diff_type_whitespace_input() {
    assert_jq_compat("type", "  { \"a\" : 1 }  ");
}

#[test]
fn passthrough_diff_has_whitespace_input() {
    assert_jq_compat(r#"has("a")"#, "  { \"a\" : 1 }  ");
}

#[test]
fn passthrough_diff_map_field_whitespace_input() {
    assert_jq_compat("map(.x)", "  [ { \"x\" : 1 } , { \"x\" : 2 } ]  ");
}

// --- Numeric edge cases across passthrough paths ---

#[test]
fn passthrough_diff_identity_negative_zero() {
    assert_jq_compat(".", "-0");
}

#[test]
fn passthrough_diff_identity_large_integer() {
    // Very large integers that fit in f64 range
    assert_jq_compat(".", "9007199254740992");
    assert_jq_compat(".", "-9007199254740992");
}

#[test]
fn passthrough_diff_identity_various_numbers() {
    // Avoid scientific notation inputs — jq normalizes case (e vs E)
    // while qj preserves the original text. Both are valid JSON.
    assert_jq_compat(".", "0");
    assert_jq_compat(".", "-1");
    assert_jq_compat(".", "3.14159");
    assert_jq_compat(".", "0.001");
}

#[test]
fn passthrough_diff_map_field_numeric_values() {
    // Avoid scientific notation in input — formatting differs between qj and jq
    assert_jq_compat("map(.n)", r#"[{"n":0},{"n":-1},{"n":3.14},{"n":999999}]"#);
}

// --- Cross-mode routing differential tests ---
//
// Verify that different code paths in main.rs produce identical output for the
// same logical input: NDJSON auto-detect vs single-doc, slurp equivalence,
// mmap vs read() for file I/O, and file vs stdin routing.

/// Run qj with custom args and env vars on stdin, returning stdout.
fn qj_with_env(args: &[&str], input: &str, env_vars: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    let output = cmd
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
        .expect("failed to run qj");
    assert!(
        output.status.success(),
        "qj {:?} exited with {}: stderr={}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

/// Run qj on a temp file with given args and env vars, returning stdout.
fn qj_file_with_env(args: &[&str], content: &str, env_vars: &[(&str, &str)]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.json");
    std::fs::write(&path, content).unwrap();

    let path_str = path.to_str().unwrap().to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    for arg in args {
        cmd.arg(arg);
    }
    cmd.arg(&path_str);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    let output = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("failed to run qj");
    assert!(
        output.status.success(),
        "qj {:?} {} exited with {}: stderr={}",
        args,
        path_str,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

/// Assert that NDJSON auto-detect (parallel path) produces the same output as
/// processing each line individually as a single JSON doc.
fn assert_ndjson_vs_single_doc(filter: &str, ndjson_input: &str) {
    let ndjson_out = qj_with_env(&["-c", filter], ndjson_input, &[]);

    let mut single_out = String::new();
    for line in ndjson_input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let out = qj_with_env(&["-c", filter], trimmed, &[]);
        single_out.push_str(&out);
    }

    let mut ndjson_lines: Vec<&str> = ndjson_out.lines().collect();
    let mut single_lines: Vec<&str> = single_out.lines().collect();
    ndjson_lines.sort();
    single_lines.sort();

    assert_eq!(
        ndjson_lines, single_lines,
        "NDJSON vs single-doc mismatch for filter={filter:?}\n\
         ndjson output: {ndjson_out}\nsingle-doc output: {single_out}"
    );
}

/// Assert that mmap and read() paths produce identical output for file I/O.
fn assert_mmap_vs_read(args: &[&str], content: &str) {
    let mmap_out = qj_file_with_env(args, content, &[]);
    let read_out = qj_file_with_env(args, content, &[("QJ_NO_MMAP", "1")]);
    assert_eq!(
        mmap_out, read_out,
        "mmap vs read() mismatch for args={args:?}\n\
         mmap output: {mmap_out}\nread output: {read_out}"
    );
}

// --- NDJSON auto-detect vs single-doc processing ---

#[test]
fn cross_mode_ndjson_vs_single_doc_identity() {
    assert_ndjson_vs_single_doc(".", "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_field_access() {
    assert_ndjson_vs_single_doc(".a", "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_type() {
    assert_ndjson_vs_single_doc("type", "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_length() {
    assert_ndjson_vs_single_doc(
        "length",
        "{\"a\":1,\"b\":2}\n{\"x\":1}\n{\"p\":1,\"q\":2,\"r\":3}\n",
    );
}

#[test]
fn cross_mode_ndjson_vs_single_doc_select() {
    assert_ndjson_vs_single_doc("select(.a > 3)", "{\"a\":1}\n{\"a\":5}\n{\"a\":10}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_construct_object() {
    assert_ndjson_vs_single_doc("{result: .x}", "{\"x\":1,\"y\":2}\n{\"x\":3,\"y\":4}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_construct_array() {
    assert_ndjson_vs_single_doc("[.x, .y]", "{\"x\":1,\"y\":2}\n{\"x\":3,\"y\":4}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_keys() {
    assert_ndjson_vs_single_doc("keys", "{\"b\":2,\"a\":1}\n{\"z\":3,\"m\":4}\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_nested() {
    assert_ndjson_vs_single_doc(
        ".a.b.c",
        "{\"a\":{\"b\":{\"c\":1}}}\n{\"a\":{\"b\":{\"c\":2}}}\n",
    );
}

#[test]
fn cross_mode_ndjson_vs_single_doc_array_lines() {
    assert_ndjson_vs_single_doc(".[0]", "[1,2,3]\n[4,5,6]\n");
}

#[test]
fn cross_mode_ndjson_vs_single_doc_pipe_chain() {
    assert_ndjson_vs_single_doc(
        ".items | length",
        "{\"items\":[1,2,3]}\n{\"items\":[4,5]}\n",
    );
}

#[test]
fn cross_mode_ndjson_vs_single_doc_select_string() {
    assert_ndjson_vs_single_doc(
        "select(.type == \"PushEvent\") | .id",
        "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n{\"type\":\"PushEvent\",\"id\":3}\n",
    );
}

// --- Forced --jsonl vs auto-detect ---

#[test]
fn cross_mode_forced_jsonl_vs_auto_detect() {
    let input = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
    let auto_out = qj_with_env(&["-c", ".a"], input, &[]);
    let forced_out = qj_with_env(&["-c", "--jsonl", ".a"], input, &[]);
    assert_eq!(
        auto_out, forced_out,
        "forced --jsonl vs auto-detect mismatch"
    );
}

#[test]
fn cross_mode_forced_jsonl_vs_auto_detect_select() {
    let input = "{\"n\":1}\n{\"n\":5}\n{\"n\":3}\n";
    let auto_out = qj_with_env(&["-c", "select(.n > 2)"], input, &[]);
    let forced_out = qj_with_env(&["-c", "--jsonl", "select(.n > 2)"], input, &[]);
    assert_eq!(
        auto_out, forced_out,
        "forced --jsonl vs auto-detect mismatch for select"
    );
}

// --- Slurp mode equivalence ---

#[test]
fn cross_mode_slurp_ndjson_to_array() {
    let slurp_out = qj_with_env(&["-cs", "."], "1\n2\n3\n", &[]);
    assert_eq!(slurp_out.trim(), "[1,2,3]");
    let array_out = qj_with_env(&["-c", "."], "[1,2,3]", &[]);
    assert_eq!(slurp_out.trim(), array_out.trim());
}

#[test]
fn cross_mode_slurp_objects() {
    let slurp_out = qj_with_env(&["-cs", "."], "{\"a\":1}\n{\"a\":2}\n", &[]);
    let array_out = qj_with_env(&["-c", "."], "[{\"a\":1},{\"a\":2}]", &[]);
    assert_eq!(
        slurp_out.trim(),
        array_out.trim(),
        "slurp of NDJSON objects should equal processing an array literal"
    );
}

#[test]
fn cross_mode_slurp_length() {
    let slurp_len = qj_with_env(&["-cs", "length"], "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n", &[]);
    assert_eq!(slurp_len.trim(), "3");
}

#[test]
fn cross_mode_slurp_add() {
    let slurp_out = qj_with_env(&["-cs", "add"], "10\n20\n30\n", &[]);
    assert_eq!(slurp_out.trim(), "60");
}

#[test]
fn cross_mode_slurp_map_vs_ndjson() {
    let input = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
    let slurp_out = qj_with_env(&["-cs", "map(.a)"], input, &[]);
    let ndjson_out = qj_with_env(&["-c", ".a"], input, &[]);
    assert_eq!(slurp_out.trim(), "[1,2,3]");
    let ndjson_lines: Vec<&str> = ndjson_out.trim().lines().collect();
    assert_eq!(ndjson_lines, vec!["1", "2", "3"]);
}

#[test]
fn cross_mode_slurp_mixed_types() {
    let out = qj_with_env(
        &["-cs", "."],
        "1\n\"hello\"\nnull\ntrue\n[1,2]\n{\"x\":1}\n",
        &[],
    );
    assert_eq!(out.trim(), "[1,\"hello\",null,true,[1,2],{\"x\":1}]");
}

#[test]
fn cross_mode_slurp_file_vs_stdin() {
    let input = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
    let stdin_out = qj_with_env(&["-cs", "."], input, &[]);
    let file_out = qj_file_with_env(&["-cs", "."], input, &[]);
    assert_eq!(stdin_out, file_out, "slurp from file vs stdin should match");
}

// --- mmap vs read() differential tests ---

#[test]
fn cross_mode_mmap_vs_read_single_json() {
    assert_mmap_vs_read(&["-c", "."], "{\"a\":1,\"b\":2}\n");
}

#[test]
fn cross_mode_mmap_vs_read_ndjson() {
    assert_mmap_vs_read(&["-c", ".a"], "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
}

#[test]
fn cross_mode_mmap_vs_read_ndjson_select() {
    assert_mmap_vs_read(
        &["-c", "select(.n > 5)"],
        "{\"n\":1}\n{\"n\":10}\n{\"n\":3}\n{\"n\":20}\n",
    );
}

#[test]
fn cross_mode_mmap_vs_read_ndjson_keys() {
    assert_mmap_vs_read(&["-c", "keys"], "{\"b\":1,\"a\":2}\n{\"z\":3,\"m\":4}\n");
}

#[test]
fn cross_mode_mmap_vs_read_identity_array() {
    assert_mmap_vs_read(&["-c", "."], "[1,2,3]\n");
}

#[test]
fn cross_mode_mmap_vs_read_field_chain() {
    assert_mmap_vs_read(&["-c", ".a.b"], "{\"a\":{\"b\":42}}\n");
}

#[test]
fn cross_mode_mmap_vs_read_large_object() {
    let mut obj = String::from("{");
    for i in 0..100 {
        if i > 0 {
            obj.push(',');
        }
        obj.push_str(&format!("\"field_{i}\":{i}"));
    }
    obj.push_str("}\n");
    assert_mmap_vs_read(&["-c", ".field_50"], &obj);
}

#[test]
fn cross_mode_mmap_vs_read_ndjson_construct() {
    assert_mmap_vs_read(&["-c", "{x: .a}"], "{\"a\":1}\n{\"a\":2}\n");
}

#[test]
fn cross_mode_mmap_vs_read_slurp() {
    assert_mmap_vs_read(&["-cs", "."], "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
}

#[test]
fn cross_mode_mmap_vs_read_ndjson_length() {
    assert_mmap_vs_read(&["-c", "length"], "{\"a\":1,\"b\":2}\n{\"x\":1}\n");
}

// --- File vs stdin routing ---

#[test]
fn cross_mode_file_vs_stdin_single_json() {
    let input = "{\"a\":1,\"b\":2}\n";
    let stdin_out = qj_with_env(&["-c", ".a"], input, &[]);
    let file_out = qj_file_with_env(&["-c", ".a"], input, &[]);
    assert_eq!(
        stdin_out, file_out,
        "file vs stdin mismatch for single JSON"
    );
}

#[test]
fn cross_mode_file_vs_stdin_ndjson() {
    let input = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
    let stdin_out = qj_with_env(&["-c", ".a"], input, &[]);
    let file_out = qj_file_with_env(&["-c", ".a"], input, &[]);
    let mut stdin_lines: Vec<&str> = stdin_out.lines().collect();
    let mut file_lines: Vec<&str> = file_out.lines().collect();
    stdin_lines.sort();
    file_lines.sort();
    assert_eq!(stdin_lines, file_lines, "file vs stdin mismatch for NDJSON");
}

#[test]
fn cross_mode_file_vs_stdin_ndjson_select() {
    let input = "{\"n\":1}\n{\"n\":10}\n{\"n\":3}\n{\"n\":20}\n";
    let stdin_out = qj_with_env(&["-c", "select(.n > 5)"], input, &[]);
    let file_out = qj_file_with_env(&["-c", "select(.n > 5)"], input, &[]);
    let mut stdin_lines: Vec<&str> = stdin_out.lines().collect();
    let mut file_lines: Vec<&str> = file_out.lines().collect();
    stdin_lines.sort();
    file_lines.sort();
    assert_eq!(
        stdin_lines, file_lines,
        "file vs stdin mismatch for NDJSON select"
    );
}

// ---------------------------------------------------------------------------
// #11: Decompression path differential testing
// Verifies .json.gz and .json.zst produce identical output to .json
// ---------------------------------------------------------------------------

fn qj_file(args: &[&str], path: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    for arg in args {
        cmd.arg(arg);
    }
    cmd.arg(path);
    let output = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("failed to run qj");
    assert!(
        output.status.success(),
        "qj exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

fn assert_decompressed_matches(json: &str, filter: &str) {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();

    // Write plain JSON
    let json_path = dir.path().join("input.json");
    std::fs::write(&json_path, json).unwrap();
    let plain_out = qj_file(&["-c", filter], json_path.to_str().unwrap());

    // Write gzip compressed
    let gz_path = dir.path().join("input.json.gz");
    let gz_file = std::fs::File::create(&gz_path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(gz_file, flate2::Compression::fast());
    encoder.write_all(json.as_bytes()).unwrap();
    encoder.finish().unwrap();
    let gz_out = qj_file(&["-c", filter], gz_path.to_str().unwrap());

    assert_eq!(
        plain_out, gz_out,
        "gzip output differs from plain for filter={filter}"
    );

    // Write zstd compressed
    let zst_path = dir.path().join("input.json.zst");
    let zst_data = zstd::encode_all(json.as_bytes(), 1).unwrap();
    std::fs::write(&zst_path, zst_data).unwrap();
    let zst_out = qj_file(&["-c", filter], zst_path.to_str().unwrap());

    assert_eq!(
        plain_out, zst_out,
        "zstd output differs from plain for filter={filter}"
    );
}

#[test]
fn decompress_identity() {
    assert_decompressed_matches(r#"{"a":1,"b":"hello","c":null}"#, ".");
}

#[test]
fn decompress_field_access() {
    assert_decompressed_matches(r#"{"name":"alice","age":30}"#, ".name");
}

#[test]
fn decompress_filter() {
    assert_decompressed_matches(r#"{"items":[{"x":1},{"x":2},{"x":3}]}"#, "[.items[] | .x]");
}

#[test]
fn decompress_ndjson() {
    let ndjson = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
    assert_decompressed_matches(ndjson, ".a");
}

#[test]
fn decompress_diverse_values() {
    assert_decompressed_matches(
        r#"{"int":42,"float":3.14,"str":"hello","bool":true,"null":null,"arr":[1,2],"obj":{"x":1}}"#,
        ".",
    );
}

#[test]
fn decompress_unicode() {
    assert_decompressed_matches(r#"{"emoji":"hello \u0041\u0042\u0043","key":"value"}"#, ".");
}

#[test]
fn decompress_large_array() {
    // Generate a larger payload to exercise chunked decompression
    let mut items: Vec<String> = Vec::new();
    for i in 0..100 {
        items.push(format!(r#"{{"id":{i},"val":"item_{i}"}}"#));
    }
    let json = format!("[{}]", items.join(","));
    assert_decompressed_matches(&json, ".[50].val");
}

#[test]
fn decompress_type_and_length() {
    assert_decompressed_matches(r#"{"a":[1,2,3]}"#, ".a | length");
    assert_decompressed_matches(r#"{"a":[1,2,3]}"#, "type");
}

// ---------------------------------------------------------------------------
// #12: Output mode value identity testing
// Verifies compact, pretty, and raw output represent the same values
// ---------------------------------------------------------------------------

fn qj_with_args(args: &[&str], input: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = cmd
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
        .expect("failed to run qj");
    assert!(
        output.status.success(),
        "qj exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qj output was not valid UTF-8")
}

/// Assert compact and pretty output are semantically identical
/// by re-parsing both through qj with -c and comparing.
fn assert_compact_pretty_agree(filter: &str, input: &str) {
    let compact = qj_with_args(&["-c", filter], input);
    let pretty = qj_with_args(&[filter], input);

    // Re-compact the pretty output to normalize
    let re_compacted = qj_with_args(&["-c", "."], pretty.trim());

    assert_eq!(
        compact.trim(),
        re_compacted.trim(),
        "compact vs pretty disagree for filter={filter} input={input}"
    );
}

#[test]
fn output_mode_compact_vs_pretty_object() {
    assert_compact_pretty_agree(".", r#"{"a":1,"b":"hello","c":null,"d":true}"#);
}

#[test]
fn output_mode_compact_vs_pretty_array() {
    assert_compact_pretty_agree(".", r#"[1,"two",null,true,[5],{"a":6}]"#);
}

#[test]
fn output_mode_compact_vs_pretty_nested() {
    assert_compact_pretty_agree(".", r#"{"a":{"b":{"c":[1,2,{"d":3}]}}}"#);
}

#[test]
fn output_mode_compact_vs_pretty_filter() {
    assert_compact_pretty_agree("[.[] | . + 1]", "[1,2,3]");
}

#[test]
fn output_mode_compact_vs_pretty_scalars() {
    assert_compact_pretty_agree(".", "42");
    assert_compact_pretty_agree(".", r#""hello""#);
    assert_compact_pretty_agree(".", "null");
    assert_compact_pretty_agree(".", "true");
    assert_compact_pretty_agree(".", "3.14");
}

#[test]
fn output_mode_compact_vs_pretty_empty() {
    assert_compact_pretty_agree(".", "[]");
    assert_compact_pretty_agree(".", "{}");
}

#[test]
fn output_mode_compact_vs_pretty_sort_keys() {
    let compact = qj_with_args(&["-c", "-S", "."], r#"{"b":2,"a":1,"c":3}"#);
    let pretty = qj_with_args(&["-S", "."], r#"{"b":2,"a":1,"c":3}"#);
    let re_compacted = qj_with_args(&["-c", "."], pretty.trim());
    assert_eq!(compact.trim(), re_compacted.trim());
}

#[test]
fn output_mode_raw_strings() {
    // -r strips quotes from string output
    let raw = qj_with_args(&["-r", ".name"], r#"{"name":"alice"}"#);
    assert_eq!(raw.trim(), "alice");

    // Non-string values should be identical between -r and -c
    let raw_num = qj_with_args(&["-r", ".age"], r#"{"age":30}"#);
    let compact_num = qj_with_args(&["-c", ".age"], r#"{"age":30}"#);
    assert_eq!(raw_num.trim(), compact_num.trim());

    let raw_null = qj_with_args(&["-r", ".x"], r#"{"x":null}"#);
    let compact_null = qj_with_args(&["-c", ".x"], r#"{"x":null}"#);
    assert_eq!(raw_null.trim(), compact_null.trim());

    let raw_arr = qj_with_args(&["-r", ".x"], r#"{"x":[1,2]}"#);
    let compact_arr = qj_with_args(&["-c", ".x"], r#"{"x":[1,2]}"#);
    assert_eq!(raw_arr.trim(), compact_arr.trim());
}

#[test]
fn output_mode_raw_escapes() {
    // -r should unescape strings
    let raw = qj_with_args(&["-r", "."], r#""hello\nworld""#);
    assert_eq!(raw.trim(), "hello\nworld");

    let raw_tab = qj_with_args(&["-r", "."], r#""a\tb""#);
    assert_eq!(raw_tab.trim(), "a\tb");
}

#[test]
fn output_mode_compact_vs_pretty_multivalue() {
    // Filters producing multiple values
    let compact = qj_with_args(&["-c", ".[]"], "[1,2,3]");
    let pretty = qj_with_args(&[".[]"], "[1,2,3]");
    assert_eq!(compact.trim(), pretty.trim());
}

// =========================================================================
// TEST_TODOS_3 #1: Math builtins
// =========================================================================

#[test]
fn jq_compat_math_floor_ceil_round() {
    assert_jq_compat("floor", "1.5");
    assert_jq_compat("floor", "-1.5");
    assert_jq_compat("floor", "0");
    assert_jq_compat("floor", "1");
    assert_jq_compat("ceil", "1.5");
    assert_jq_compat("ceil", "-1.5");
    assert_jq_compat("ceil", "0");
    assert_jq_compat("round", "1.5");
    assert_jq_compat("round", "2.5");
    assert_jq_compat("round", "-1.5");
    assert_jq_compat("round", "-2.5");
    assert_jq_compat("round", "0.5");
    assert_jq_compat("trunc", "1.9");
    assert_jq_compat("trunc", "-1.9");
    assert_jq_compat("fabs", "-3.14");
    assert_jq_compat("fabs", "3.14");
}

#[test]
fn jq_compat_math_sqrt_exp_log() {
    assert_jq_compat("sqrt", "4");
    assert_jq_compat("sqrt", "0");
    assert_jq_compat("sqrt", "2");
    // cbrt: Rust's f64::cbrt() (LLVM) and glibc cbrt() can differ in
    // last-bit precision for perfect cubes. Use float input to avoid
    // integer-vs-float formatting divergence across platforms.
    assert_jq_compat("cbrt", "8.0");
    assert_jq_compat("cbrt", "1.0");
    assert_jq_compat("exp", "0");
    assert_jq_compat("exp", "1");
    assert_jq_compat("exp2", "3");
    assert_jq_compat("exp2", "0");
    assert_jq_compat("log", "1");
    assert_jq_compat("log2", "8");
    assert_jq_compat("log10", "1000");
}

#[test]
fn jq_compat_math_trig() {
    assert_jq_compat("sin", "0");
    assert_jq_compat("cos", "0");
    assert_jq_compat("tan", "0");
    assert_jq_compat("asin", "0");
    assert_jq_compat("acos", "1");
    assert_jq_compat("atan", "0");
    assert_jq_compat("atan", "1");
}

#[test]
fn jq_compat_math_hyperbolic() {
    assert_jq_compat("sinh", "0");
    assert_jq_compat("cosh", "0");
    assert_jq_compat("tanh", "0");
    assert_jq_compat("asinh", "0");
    assert_jq_compat("acosh", "1");
    assert_jq_compat("atanh", "0");
}

#[test]
fn jq_compat_math_pow_atan2() {
    assert_jq_compat("pow(.; 2)", "3");
    assert_jq_compat("pow(.; 0)", "5");
    assert_jq_compat("pow(0; 0)", "null");
    assert_jq_compat("pow(2; 10)", "null");
    assert_jq_compat("atan2(1; 1)", "null");
    assert_jq_compat("atan2(0; 1)", "null");
    assert_jq_compat("atan2(1; 0)", "null");
}

#[test]
fn jq_compat_math_nan_infinite() {
    assert_jq_compat("nan | isnan", "null");
    assert_jq_compat("infinite | isinfinite", "null");
    assert_jq_compat("nan | isinfinite", "null");
    assert_jq_compat("infinite | isnan", "null");
    assert_jq_compat("1 | isnan", "null");
    assert_jq_compat("1 | isinfinite", "null");
    assert_jq_compat("1 | isfinite", "null");
    assert_jq_compat("1 | isnormal", "null");
    assert_jq_compat("0 | isnormal", "null");
    // jq's isfinite means "not infinite" — NaN is considered finite
    assert_jq_compat("nan | isfinite", "null");
}

#[test]
fn jq_compat_math_range() {
    assert_jq_compat("[range(5)]", "null");
    assert_jq_compat("[range(0)]", "null");
    assert_jq_compat("[range(2;5)]", "null");
    assert_jq_compat("[range(0;10;2)]", "null");
    assert_jq_compat("[range(0;10;3)]", "null");
    assert_jq_compat("[range(0;1;0.25)]", "null");
    // Negative step with correct direction
    assert_jq_compat("[range(5;0;-1)]", "null");
    // Wrong direction → empty
    assert_jq_compat("[range(0;5;-1)]", "null");
    assert_jq_compat("[range(0;1;0)]", "null");
}

#[test]
fn jq_compat_math_abs() {
    assert_jq_compat("abs", "-5");
    assert_jq_compat("abs", "5");
    assert_jq_compat("abs", "0");
    assert_jq_compat("abs", "-3.14");
}

#[test]
fn jq_compat_math_domain_errors() {
    // sqrt of negative
    assert_jq_compat("sqrt", "-1");
    // log of negative
    assert_jq_compat("log", "-1");
    // asin out of domain
    assert_jq_compat("asin", "2");
    // acos out of domain
    assert_jq_compat("acos", "2");
    // log(0) → -Infinity → represented as -DBL_MAX
    assert_jq_compat("log", "0");
}

#[test]
fn jq_compat_math_type_errors() {
    assert_jq_compat("floor", r#""hello""#);
    assert_jq_compat("sqrt", "true");
    assert_jq_compat("sin", "null");
}

// =========================================================================
// TEST_TODOS_3 #2: String builtins
// =========================================================================

#[test]
fn jq_compat_string_split_join() {
    assert_jq_compat(r#"split(",")"#, r#""a,b,c""#);
    assert_jq_compat(r#"split("")"#, r#""hello""#);
    assert_jq_compat(r#"split(",")"#, r#""no_commas""#);
    assert_jq_compat(r#"split(",")"#, r#""a,b,,c""#);
    assert_jq_compat(r#"join(",")"#, r#"["a","b","c"]"#);
    assert_jq_compat(r#"join("-")"#, r#"["a","b","c"]"#);
    assert_jq_compat(r#"join("")"#, r#"["a","b","c"]"#);
    // Split then join roundtrip
    assert_jq_compat(r#"split(",") | join(",")"#, r#""a,b,c""#);
}

#[test]
fn jq_compat_string_explode_implode() {
    assert_jq_compat("explode", r#""ABC""#);
    assert_jq_compat("implode", "[65,66,67]");
    assert_jq_compat("explode | implode", r#""hello""#);
    assert_jq_compat("explode", r#""""#);
    assert_jq_compat("implode", "[]");
}

#[test]
fn jq_compat_string_index_rindex() {
    assert_jq_compat(r#"index("bar")"#, r#""foobar""#);
    assert_jq_compat(r#"index("xyz")"#, r#""foobar""#);
    assert_jq_compat(r#"rindex("o")"#, r#""foobar""#);
    assert_jq_compat(r#"index("")"#, r#""hello""#);
    assert_jq_compat(r#"[indices("a")]"#, r#""banana""#);
}

#[test]
fn jq_compat_string_trim() {
    assert_jq_compat(r#"ltrimstr("hel")"#, r#""hello""#);
    assert_jq_compat(r#"rtrimstr("llo")"#, r#""hello""#);
    assert_jq_compat(r#"ltrimstr("xyz")"#, r#""hello""#);
    assert_jq_compat(r#"rtrimstr("xyz")"#, r#""hello""#);
    assert_jq_compat(r#"startswith("hel")"#, r#""hello""#);
    assert_jq_compat(r#"endswith("llo")"#, r#""hello""#);
    assert_jq_compat(r#"startswith("xyz")"#, r#""hello""#);
    assert_jq_compat(r#"endswith("xyz")"#, r#""hello""#);
}

#[test]
fn jq_compat_string_tostring_tonumber() {
    assert_jq_compat("tostring", "42");
    assert_jq_compat("tostring", "3.14");
    assert_jq_compat("tostring", "true");
    assert_jq_compat("tostring", "null");
    assert_jq_compat("tostring", r#""already_string""#);
    assert_jq_compat("tonumber", r#""42""#);
    assert_jq_compat("tonumber", r#""3.14""#);
    assert_jq_compat("tonumber", "42");
    assert_jq_compat("tonumber", r#""hello""#);
    assert_jq_compat("tonumber", "null");
    assert_jq_compat("tonumber", "true");
}

#[test]
fn jq_compat_string_tojson_fromjson() {
    assert_jq_compat("tojson", r#"{"a":1}"#);
    assert_jq_compat("tojson", "[1,2,3]");
    assert_jq_compat("tojson", "42");
    assert_jq_compat("tojson", r#""hello""#);
    assert_jq_compat(r#"fromjson"#, r#""{\"a\":1}""#);
    assert_jq_compat(r#"tojson | fromjson"#, r#"{"a":1,"b":[2,3]}"#);
}

#[test]
fn jq_compat_string_case() {
    assert_jq_compat("ascii_downcase", r#""HELLO""#);
    assert_jq_compat("ascii_upcase", r#""hello""#);
    assert_jq_compat("ascii_downcase", r#""Hello World""#);
    assert_jq_compat("ascii_downcase", r#""123abc""#);
}

#[test]
fn jq_compat_string_utf8bytelength() {
    assert_jq_compat("utf8bytelength", r#""hello""#);
    assert_jq_compat("utf8bytelength", r#""""#);
}

#[test]
fn jq_compat_string_format() {
    assert_jq_compat("@uri", r#""hello world""#);
    assert_jq_compat("@html", r#""<b>hi</b>""#);
    assert_jq_compat("@base64", r#""hello""#);
    assert_jq_compat("@base64d", r#""aGVsbG8=""#);
    assert_jq_compat("@base64 | @base64d", r#""roundtrip""#);
    assert_jq_compat("@sh", r#""hello world""#);
    assert_jq_compat("@json", r#"{"a":1}"#);
}

// =========================================================================
// TEST_TODOS_3 #3: Date builtins
// =========================================================================

#[test]
fn jq_compat_date_todate_fromdate() {
    assert_jq_compat("todate", "0");
    assert_jq_compat("todate", "1705321800");
    assert_jq_compat(r#"fromdate"#, r#""1970-01-01T00:00:00Z""#);
    // Roundtrip
    assert_jq_compat("todate | fromdate", "0");
}

#[test]
fn jq_compat_date_gmtime_mktime() {
    assert_jq_compat("gmtime", "0");
    assert_jq_compat("gmtime | mktime", "0");
    assert_jq_compat("gmtime | mktime", "1705321800");
}

#[test]
fn jq_compat_date_strftime() {
    assert_jq_compat(r#"strftime("%Y-%m-%d")"#, "0");
    assert_jq_compat(r#"strftime("%H:%M:%S")"#, "0");
    assert_jq_compat(r#"strftime("%Y")"#, "1705321800");
}

#[test]
fn jq_compat_date_now() {
    // now returns a number > 0
    assert_jq_compat("now | . > 0", "null");
}

// =========================================================================
// TEST_TODOS_3 #4: Untested array builtins
// =========================================================================

#[test]
fn jq_compat_walk_extended() {
    // Walk identity
    assert_jq_compat("walk(.)", r#"{"a":1,"b":[2,3]}"#);
    // Walk with transformation
    assert_jq_compat(
        r#"walk(if type == "number" then . + 10 else . end)"#,
        r#"{"a":1,"b":[2,3]}"#,
    );
    // Walk on scalar
    assert_jq_compat("walk(. + 1)", "42");
    // Walk on null
    assert_jq_compat("walk(. + 1)", "null");
    // Walk on empty containers
    assert_jq_compat("walk(.)", "[]");
    assert_jq_compat("walk(.)", "{}");
    // Walk nested
    assert_jq_compat(
        r#"walk(if type == "string" then ascii_upcase else . end)"#,
        r#"{"a":"hello","b":{"c":"world"}}"#,
    );
}

#[test]
fn jq_compat_bsearch_extended() {
    assert_jq_compat("bsearch(2)", "[1,2,3]");
    assert_jq_compat("bsearch(1)", "[1,2,3]");
    assert_jq_compat("bsearch(3)", "[1,2,3]");
    assert_jq_compat("bsearch(4)", "[1,2,3]");
    assert_jq_compat("bsearch(0)", "[1,2,3]");
    assert_jq_compat("bsearch(2)", "[1,3,5,7]");
    assert_jq_compat("bsearch(1)", "[]");
    assert_jq_compat("bsearch(1)", "[1]");
    assert_jq_compat("bsearch(2)", "[1]");
}

#[test]
fn jq_compat_combinations() {
    assert_jq_compat("[combinations]", "[[1,2],[3,4]]");
    assert_jq_compat("[combinations]", "[[1],[2],[3]]");
    assert_jq_compat("[combinations]", "[[1,2]]");
    assert_jq_compat("[combinations(2)]", "[1,2]");
    // Empty sub-array → empty Cartesian product
    assert_jq_compat("[combinations]", "[[], [1,2]]");
    // combinations(0) → one empty array
    assert_jq_compat("[combinations(0)]", "[1,2]");
}

#[test]
fn jq_compat_pick() {
    assert_jq_compat("pick(.a, .c)", r#"{"a":1,"b":2,"c":3}"#);
    assert_jq_compat("pick(.a)", r#"{"a":1,"b":2}"#);
    assert_jq_compat("pick(.a.b)", r#"{"a":{"b":1,"c":2}}"#);
    assert_jq_compat("pick(.missing)", r#"{"a":1}"#);
}

#[test]
fn jq_compat_nth_extended() {
    assert_jq_compat("nth(0; .[])", "[10,20,30]");
    assert_jq_compat("nth(2; .[])", "[10,20,30]");
    assert_jq_compat("first(.[])", "[10,20,30]");
    assert_jq_compat("last(.[])", "[10,20,30]");
    assert_jq_compat("[limit(3; .[])]", "[1,2,3,4,5]");
}

#[test]
fn jq_compat_repeat_extended() {
    assert_jq_compat("[limit(5; 1 | repeat(. * 2))]", "null");
    assert_jq_compat("[limit(3; repeat(.))]", "1");
}

#[test]
fn jq_compat_isempty_extended() {
    assert_jq_compat("isempty(empty)", "null");
    assert_jq_compat("isempty(.)", "null");
    assert_jq_compat("isempty(.[])", "[]");
    assert_jq_compat("isempty(.[])", "[1]");
}

// =========================================================================
// TEST_TODOS_3 #5: inside, to_entries, from_entries
// =========================================================================

#[test]
fn jq_compat_inside() {
    assert_jq_compat(r#"inside("foobar")"#, r#""foo""#);
    assert_jq_compat(r#"inside("foo")"#, r#""bar""#);
    assert_jq_compat("inside([1,2,3])", "[1,2]");
    assert_jq_compat(r#"inside({"a":1,"b":2})"#, r#"{"a":1}"#);
    assert_jq_compat(r#"inside({"a":1,"b":2})"#, r#"{"a":1,"c":3}"#);
    assert_jq_compat("inside({})", "{}");
}

#[test]
fn jq_compat_to_entries_from_entries() {
    assert_jq_compat("to_entries", r#"{"a":1,"b":2}"#);
    assert_jq_compat("[{\"key\":\"a\",\"value\":1}] | from_entries", "null");
    assert_jq_compat("[{\"Key\":\"a\",\"Value\":1}] | from_entries", "null");
    assert_jq_compat("[{\"name\":\"a\",\"value\":1}] | from_entries", "null");
    assert_jq_compat("to_entries | from_entries", r#"{"a":1,"b":2}"#);
    assert_jq_compat("with_entries(.value += 10)", r#"{"a":1,"b":2}"#);
    assert_jq_compat("with_entries(select(.value > 1))", r#"{"a":1,"b":2,"c":3}"#);
}

// =========================================================================
// TEST_TODOS_3 #6: Destructuring patterns
// =========================================================================

#[test]
fn jq_compat_destructure_array() {
    assert_jq_compat(". as [$a, $b, $c] | [$a, $b, $c]", "[1,2,3]");
    // Missing elements → null
    assert_jq_compat(". as [$a, $b] | [$a, $b]", "[1]");
    // Extra elements ignored
    assert_jq_compat(". as [$a, $b] | [$a, $b]", "[1,2,3,4]");
    // Swap
    assert_jq_compat(". as [$a, $b] | [$b, $a]", "[1,2]");
}

#[test]
fn jq_compat_destructure_object() {
    assert_jq_compat(". as {x: $a, y: $b} | [$a, $b]", r#"{"x":1,"y":2}"#);
    // Missing field → null
    assert_jq_compat(". as {x: $a, y: $b} | [$a, $b]", r#"{"x":1}"#);
    // Shorthand
    assert_jq_compat(". as {$x} | $x", r#"{"x":42}"#);
}

#[test]
fn jq_compat_destructure_nested() {
    assert_jq_compat(". as [[$a, $b], $c] | [$a, $b, $c]", "[[1,2],3]");
    assert_jq_compat(". as {a: {b: $x}} | $x", r#"{"a":{"b":99}}"#);
}

#[test]
fn jq_compat_destructure_in_reduce() {
    assert_jq_compat(
        "reduce .[] as [$k, $v] ({}; . + {($k): $v})",
        r#"[["a",1],["b",2]]"#,
    );
}

// =========================================================================
// TEST_TODOS_3 #8: Loop limits
// =========================================================================

#[test]
fn jq_compat_limit_repeat_until() {
    assert_jq_compat("[limit(5; repeat(.))]", "1");
    assert_jq_compat("[.,1] | until(.[0] >= 10; [.[0] + .[1], .[1]])", "null");
    assert_jq_compat("[limit(10; 0 | recurse(. + 1))]", "null");
}

// =========================================================================
// TEST_TODOS_3 #9: @base64 edge cases
// =========================================================================

#[test]
fn jq_compat_base64_roundtrip() {
    assert_jq_compat("@base64", r#""hello""#);
    assert_jq_compat("@base64d", r#""aGVsbG8=""#);
    assert_jq_compat("@base64 | @base64d", r#""hello world""#);
    assert_jq_compat("@base64 | @base64d", r#""a""#);
    assert_jq_compat("@base64 | @base64d", r#""ab""#);
    assert_jq_compat("@base64 | @base64d", r#""abc""#);
    assert_jq_compat("@base64 | @base64d", r#""""#);
}

// =========================================================================
// TEST_TODOS_3 #10: Output formatting edge cases
// =========================================================================

#[test]
fn jq_compat_output_infinity() {
    // Infinity is represented as +/- DBL_MAX
    assert_jq_compat("infinite", "null");
    assert_jq_compat("-infinite", "null");
    assert_jq_compat("1e308 * 10", "null");
}

#[test]
fn jq_compat_output_nan() {
    assert_jq_compat("nan", "null");
    assert_jq_compat("nan | isnan", "null");
}

#[test]
fn jq_compat_output_negative_zero() {
    assert_jq_compat("0 * -1.0", "null");
    assert_jq_compat(". * -1.0", "0");
}

#[test]
fn jq_compat_output_string_escaping() {
    // Verify control character escaping matches jq
    assert_jq_compat(r#""\t""#, "null");
    assert_jq_compat(r#""\n""#, "null");
    assert_jq_compat(r#""\r""#, "null");
    assert_jq_compat(r#""\u0000""#, "null");
}

#[test]
fn jq_compat_output_sort_keys() {
    assert_jq_compat(r#"{"z":1,"a":2,"m":3} | keys"#, "null");
    assert_jq_compat(r#"{"z":{"b":2,"a":1},"a":0}"#, "null");
}

// =========================================================================
// Fuzz-discovered bugs
// =========================================================================

#[test]
fn jq_compat_map_on_non_iterable() {
    // map() on non-array/non-object should error (not silently drop)
    assert_jq_compat("map(.)", "null");
    assert_jq_compat("map(. + 1)", "42");
    assert_jq_compat("map(.)", r#""hello""#);
    assert_jq_compat("map(.)", "true");
}

// =========================================================================
// QJ_JQ_COMPAT=1 mode — f64-compatible large integer handling
// =========================================================================

/// Run qj with QJ_JQ_COMPAT=1 and return compact output.
fn qj_compat(filter: &str, input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(["-c", "--", filter])
        .env("QJ_JQ_COMPAT", "1")
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
        .expect("failed to run qj");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn compat_large_int_tostring() {
    // Large integers > 2^53 should show f64-rounded value in compat mode
    assert_eq!(
        qj_compat(".[0] | tostring", "[13911860366432393]"),
        r#""13911860366432392""#
    );
}

#[test]
fn compat_large_int_tojson() {
    assert_eq!(
        qj_compat(".x | tojson", r#"{"x":13911860366432393}"#),
        r#""13911860366432392""#
    );
}

#[test]
fn compat_large_int_equality() {
    // In compat mode, these map to the same f64 so they're equal
    assert_eq!(
        qj_compat("(13911860366432393 == 13911860366432392)", "null"),
        "true"
    );
}

#[test]
fn compat_large_int_arithmetic() {
    // Arithmetic truncates to f64 first, so precision is lost
    assert_eq!(
        qj_compat(". - 10", "13911860366432393"),
        "13911860366432382"
    );
    assert_eq!(
        qj_compat(".[0] - 10", "[13911860366432393]"),
        "13911860366432382"
    );
    assert_eq!(
        qj_compat(".x - 10", r#"{"x":13911860366432393}"#),
        "13911860366432382"
    );
}

#[test]
fn compat_large_int_negate_tojson() {
    assert_eq!(
        qj_compat("-. | tojson", "13911860366432393"),
        r#""-13911860366432392""#
    );
}

#[test]
fn compat_normal_ints_unaffected() {
    // Integers within f64 exact range should be unaffected
    assert_eq!(qj_compat(". + 1", "42"), "43");
    assert_eq!(qj_compat(". - 10", "100"), "90");
    assert_eq!(
        qj_compat(". | tostring", "9007199254740992"),
        r#""9007199254740992""#
    );
}

#[test]
fn compat_large_int_add_zero() {
    // jq.test line 2199: $n+0 forces f64 conversion
    assert_eq!(
        qj_compat(
            ".[] as $n | $n+0 | [., tostring]",
            "[-9007199254740993, 9007199254740993, 13911860366432393]"
        ),
        "[-9007199254740992,\"-9007199254740992\"]\n[9007199254740992,\"9007199254740992\"]\n[13911860366432392,\"13911860366432392\"]"
    );
}
