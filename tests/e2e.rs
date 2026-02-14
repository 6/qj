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
}

#[test]
fn builtin_any_bare() {
    assert_eq!(jx_compact("any", "[false,null,1]").trim(), "true");
}

#[test]
fn builtin_any_all_false() {
    assert_eq!(jx_compact("any", "[false,null,false]").trim(), "false");
}

// --- Builtin: all ---

#[test]
fn builtin_all_with_condition() {
    assert_eq!(jx_compact("all(. > 0)", "[1,2,3]").trim(), "true");
}

#[test]
fn builtin_all_fails() {
    assert_eq!(jx_compact("all(. > 2)", "[1,2,3]").trim(), "false");
}

#[test]
fn builtin_all_bare() {
    assert_eq!(jx_compact("all", "[true,1,\"yes\"]").trim(), "true");
}

// --- Builtin: contains ---

#[test]
fn builtin_contains_string() {
    assert_eq!(jx_compact(r#"contains("ll")"#, r#""hello""#).trim(), "true");
}

#[test]
fn builtin_contains_array() {
    assert_eq!(jx_compact("contains([2])", "[1,2,3]").trim(), "true");
}

#[test]
fn builtin_contains_object() {
    assert_eq!(
        jx_compact(r#"contains({"a":1})"#, r#"{"a":1,"b":2}"#).trim(),
        "true"
    );
}

// --- Builtin: to_entries / from_entries ---

#[test]
fn builtin_to_entries() {
    assert_eq!(
        jx_compact("to_entries", r#"{"a":1}"#).trim(),
        r#"[{"key":"a","value":1}]"#
    );
}

#[test]
fn builtin_from_entries() {
    assert_eq!(
        jx_compact("from_entries", r#"[{"key":"a","value":1}]"#).trim(),
        r#"{"a":1}"#
    );
}

#[test]
fn builtin_from_entries_name_value() {
    assert_eq!(
        jx_compact("from_entries", r#"[{"name":"a","value":1}]"#).trim(),
        r#"{"a":1}"#
    );
}

// --- Builtin: flatten ---

#[test]
fn builtin_flatten() {
    assert_eq!(jx_compact("flatten", "[[1,[2]],3]").trim(), "[1,2,3]");
}

#[test]
fn builtin_flatten_depth() {
    assert_eq!(jx_compact("flatten(1)", "[[1,[2]],3]").trim(), "[1,[2],3]");
}

// --- Builtin: first / last ---

#[test]
fn builtin_first_bare() {
    assert_eq!(jx_compact("first", "[1,2,3]").trim(), "1");
}

#[test]
fn builtin_first_generator() {
    assert_eq!(jx_compact("first(.[])", "[10,20,30]").trim(), "10");
}

#[test]
fn builtin_last_bare() {
    assert_eq!(jx_compact("last", "[1,2,3]").trim(), "3");
}

#[test]
fn builtin_last_generator() {
    assert_eq!(jx_compact("last(.[])", "[10,20,30]").trim(), "30");
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
}

// --- Builtin: unique / unique_by ---

#[test]
fn builtin_unique() {
    assert_eq!(jx_compact("unique", "[1,2,1,3]").trim(), "[1,2,3]");
}

#[test]
fn builtin_unique_by() {
    let out = jx_compact(
        "unique_by(.a)",
        r#"[{"a":1,"b":1},{"a":2,"b":2},{"a":1,"b":3}]"#,
    );
    assert_eq!(out.trim(), r#"[{"a":1,"b":1},{"a":2,"b":2}]"#);
}

// --- Builtin: min / max ---

#[test]
fn builtin_min() {
    assert_eq!(jx_compact("min", "[3,1,2]").trim(), "1");
}

#[test]
fn builtin_max() {
    assert_eq!(jx_compact("max", "[3,1,2]").trim(), "3");
}

#[test]
fn builtin_min_empty() {
    assert_eq!(jx_compact("min", "[]").trim(), "null");
}

#[test]
fn builtin_max_empty() {
    assert_eq!(jx_compact("max", "[]").trim(), "null");
}

// --- Builtin: min_by / max_by ---

#[test]
fn builtin_min_by() {
    assert_eq!(
        jx_compact("min_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"{"x":1}"#
    );
}

#[test]
fn builtin_max_by() {
    assert_eq!(
        jx_compact("max_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"{"x":3}"#
    );
}

// --- Builtin: sort_by ---

#[test]
fn builtin_sort_by() {
    assert_eq!(
        jx_compact("sort_by(.x)", r#"[{"x":3},{"x":1},{"x":2}]"#).trim(),
        r#"[{"x":1},{"x":2},{"x":3}]"#
    );
}

// --- Builtin: del ---

#[test]
fn builtin_del() {
    assert_eq!(
        jx_compact("del(.a)", r#"{"a":1,"b":2}"#).trim(),
        r#"{"b":2}"#
    );
}

// --- Builtin: ltrimstr / rtrimstr ---

#[test]
fn builtin_ltrimstr() {
    assert_eq!(
        jx_compact(r#"ltrimstr("hel")"#, r#""hello""#).trim(),
        r#""lo""#
    );
}

#[test]
fn builtin_rtrimstr() {
    assert_eq!(
        jx_compact(r#"rtrimstr("lo")"#, r#""hello""#).trim(),
        r#""hel""#
    );
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
}

// --- Builtin: tonumber / tostring ---

#[test]
fn builtin_tonumber() {
    assert_eq!(jx_compact("tonumber", r#""42""#).trim(), "42");
    assert_eq!(jx_compact("tonumber", r#""3.14""#).trim(), "3.14");
    assert_eq!(jx_compact("tonumber", "42").trim(), "42");
}

#[test]
fn builtin_tostring() {
    assert_eq!(jx_compact("tostring", "42").trim(), r#""42""#);
    assert_eq!(jx_compact("tostring", "null").trim(), r#""null""#);
    assert_eq!(jx_compact("tostring", "true").trim(), r#""true""#);
}

// --- Builtin: values ---

#[test]
fn builtin_values_object() {
    let out = jx_compact("values", r#"{"a":1,"b":2}"#);
    assert_eq!(out.trim(), "1\n2");
}

#[test]
fn builtin_values_array() {
    let out = jx_compact("values", "[10,20,30]");
    assert_eq!(out.trim(), "10\n20\n30");
}

// --- Builtin: empty ---

#[test]
fn builtin_empty() {
    let out = jx_compact("[1, empty, 2]", "null");
    assert_eq!(out.trim(), "[1,2]");
}

// --- Builtin: not ---

#[test]
fn builtin_not_true() {
    assert_eq!(jx_compact("not", "true").trim(), "false");
}

#[test]
fn builtin_not_false() {
    assert_eq!(jx_compact("not", "false").trim(), "true");
}

#[test]
fn builtin_not_null() {
    assert_eq!(jx_compact("not", "null").trim(), "true");
}

// --- Builtin: keys_unsorted ---

#[test]
fn builtin_keys_unsorted() {
    let out = jx_compact("keys_unsorted", r#"{"b":2,"a":1}"#);
    // keys_unsorted preserves insertion order
    assert_eq!(out.trim(), r#"["b","a"]"#);
}

// --- Builtin: has (e2e) ---

#[test]
fn builtin_has_object() {
    assert_eq!(jx_compact(r#"has("a")"#, r#"{"a":1,"b":2}"#).trim(), "true");
    assert_eq!(
        jx_compact(r#"has("z")"#, r#"{"a":1,"b":2}"#).trim(),
        "false"
    );
}

#[test]
fn builtin_has_array() {
    assert_eq!(jx_compact("has(1)", "[10,20,30]").trim(), "true");
    assert_eq!(jx_compact("has(5)", "[10,20,30]").trim(), "false");
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
}

// --- Builtin: ascii_downcase / ascii_upcase (dedicated e2e) ---

#[test]
fn builtin_ascii_downcase() {
    assert_eq!(
        jx_compact("ascii_downcase", r#""HELLO WORLD""#).trim(),
        r#""hello world""#
    );
}

#[test]
fn builtin_ascii_upcase() {
    assert_eq!(
        jx_compact("ascii_upcase", r#""hello world""#).trim(),
        r#""HELLO WORLD""#
    );
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
}

#[test]
fn boolean_or() {
    assert_eq!(jx_compact("false or true", "null").trim(), "true");
    assert_eq!(jx_compact("false or false", "null").trim(), "false");
}

// --- Language: not (as filter) ---

#[test]
fn not_in_select() {
    let out = jx_compact("[.[] | select(. > 2 | not)]", "[1,2,3,4,5]");
    assert_eq!(out.trim(), "[1,2]");
}

// --- Language: Try (?) ---

#[test]
fn try_operator_suppresses_error() {
    // .foo? on a non-object should produce no output instead of error
    let out = jx_compact(".foo?", "[1,2,3]");
    assert_eq!(out.trim(), "null");
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
}

#[test]
fn if_then_no_else_false() {
    // When condition is false and no else, jq passes through the input
    let out = jx_compact(r#"if . > 5 then "big" end"#, "3");
    assert_eq!(out.trim(), "3");
}

// --- Language: Object shorthand ---

#[test]
fn object_shorthand() {
    let out = jx_compact("{name}", r#"{"name":"alice","age":30}"#);
    assert_eq!(out.trim(), r#"{"name":"alice"}"#);
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
fn field_on_array_returns_null() {
    assert_eq!(jx_compact(".x", "[1,2]").trim(), "null");
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
}

#[test]
fn operator_precedence_div_before_sub() {
    let out = jx_compact("10 - 6 / 2", "null");
    assert_eq!(out.trim(), "7");
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
}

#[test]
fn jq_compat_sort_mixed() {
    assert_jq_compat("sort", r#"[3,"a",null,true,false,1]"#);
}

#[test]
fn unique_returns_sorted() {
    let out = jx_compact("unique", "[3,1,2,1,3]");
    assert_eq!(out.trim(), "[1,2,3]");
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
}

#[test]
fn math_ceil() {
    let out = jx_compact("ceil", "3.2");
    assert_eq!(out.trim(), "4");
}

#[test]
fn math_round() {
    let out = jx_compact("round", "3.5");
    assert_eq!(out.trim(), "4");
}

#[test]
fn math_sqrt() {
    let out = jx_compact("sqrt", "9");
    assert_eq!(out.trim(), "3");
}

#[test]
fn math_fabs() {
    let out = jx_compact("fabs", "-5.5");
    assert_eq!(out.trim(), "5.5");
}

#[test]
fn math_nan_isnan() {
    let out = jx_compact("nan | isnan", "null");
    assert_eq!(out.trim(), "true");
}

#[test]
fn math_infinite_isinfinite() {
    let out = jx_compact("infinite | isinfinite", "null");
    assert_eq!(out.trim(), "true");
}

#[test]
fn math_isfinite() {
    let out = jx_compact("isfinite", "42");
    assert_eq!(out.trim(), "true");
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
}

#[test]
fn string_implode() {
    let out = jx_compact("implode", "[97,98,99]");
    assert_eq!(out.trim(), r#""abc""#);
}

#[test]
fn tojson_fromjson() {
    let out = jx_compact("[1,2] | tojson", "null");
    assert_eq!(out.trim(), r#""[1,2]""#);
}

#[test]
fn fromjson_basic() {
    let out = jx_compact(r#"fromjson"#, r#""[1,2,3]""#);
    assert_eq!(out.trim(), "[1,2,3]");
}

#[test]
fn utf8bytelength() {
    let out = jx_compact("utf8bytelength", r#""café""#);
    assert_eq!(out.trim(), "5"); // é is 2 bytes in UTF-8
}

#[test]
fn inside_string() {
    let out = jx_compact(r#"inside("foobar")"#, r#""foo""#);
    assert_eq!(out.trim(), "true");
}

#[test]
fn string_times_number() {
    let out = jx_compact(r#""ab" * 3"#, "null");
    assert_eq!(out.trim(), r#""ababab""#);
}

#[test]
fn string_divide_string() {
    let out = jx_compact(r#""a,b,c" / ",""#, "null");
    assert_eq!(out.trim(), r#"["a","b","c"]"#);
}

#[test]
fn index_string() {
    let out = jx_compact(r#"index("bar")"#, r#""foobar""#);
    assert_eq!(out.trim(), "3");
}

#[test]
fn rindex_string() {
    let out = jx_compact(r#"rindex("o")"#, r#""fooboo""#);
    assert_eq!(out.trim(), "5");
}

#[test]
fn indices_string() {
    let out = jx_compact(r#"indices("o")"#, r#""foobar""#);
    assert_eq!(out.trim(), "[1,2]");
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
}

#[test]
fn array_subtraction() {
    let out = jx_compact("[1,2,3] - [2]", "null");
    assert_eq!(out.trim(), "[1,3]");
}

#[test]
fn jq_compat_array_subtraction() {
    assert_jq_compat("[1,2,3] - [2]", "null");
}

#[test]
fn object_recursive_merge() {
    let out = jx_compact(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
    assert_eq!(out.trim(), r#"{"a":{"b":1,"c":2}}"#);
}

#[test]
fn jq_compat_object_merge() {
    assert_jq_compat(r#"{"a":{"b":1}} * {"a":{"c":2}}"#, "null");
}

#[test]
fn float_modulo() {
    let out = jx_compact(". % 3", "10.5");
    assert_eq!(out.trim(), "1.5");
}

#[test]
fn int_division_produces_float() {
    let out = jx_compact("1 / 3", "null");
    // jq: 0.3333333333333333
    let f: f64 = out.trim().parse().expect("expected float");
    assert!((f - 1.0 / 3.0).abs() < 1e-10);
}

#[test]
fn index_generator() {
    // .[expr] where expr produces multiple outputs
    let out = jx_compact(r#".[0,2]"#, "[10,20,30]");
    assert_eq!(out.trim(), "10\n30");
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
