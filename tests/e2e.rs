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

// --- File input ---

#[test]
fn file_input() {
    // twitter.json is a real test file
    let twitter = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/data/twitter.json");
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
