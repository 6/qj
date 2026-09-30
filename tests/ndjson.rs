/// Integration tests for NDJSON (newline-delimited JSON) processing.
use std::io::Write;
use std::process::Command;

/// Writes `input` to the child's stdin from another thread while collecting
/// its output: a reader that streams (as jq does) fills the stdout pipe
/// before it has read all of a large input, so writing everything first
/// would deadlock.
fn feed(mut child: std::process::Child, input: &str) -> std::io::Result<std::process::Output> {
    let mut stdin = child.stdin.take().unwrap();
    let input = input.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        // The child may exit without reading everything.
        let _ = stdin.write_all(&input);
    });
    let output = child.wait_with_output();
    writer.join().unwrap();
    output
}

fn qj_stdin(args: &[&str], input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|child| feed(child, input))
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

fn qj_file(args: &[&str], content: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.jsonl");
    std::fs::write(&path, content).unwrap();

    let full_args: Vec<&str> = args.to_vec();
    let path_str = path.to_str().unwrap().to_string();
    // We need to own the string for the lifetime
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    for arg in &full_args {
        cmd.arg(arg);
    }
    cmd.arg(&path_str);

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

/// Assert that qj and jq, given the same arguments and stdin, produce the
/// same stdout and exit code. Skipped when jq isn't installed.
fn assert_jq_compat(args: &[&str], input: &str) {
    let run = |cmd: &str| {
        Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|child| feed(child, input))
    };
    let Ok(jq) = run("jq") else {
        return;
    };
    let qj = run(env!("CARGO_BIN_EXE_qj")).expect("failed to run qj");
    assert_eq!(
        (qj.status.code(), String::from_utf8_lossy(&qj.stdout)),
        (jq.status.code(), String::from_utf8_lossy(&jq.stdout)),
        "qj vs jq (exit code, stdout): args={args:?} input={input:?}"
    );
}

/// Like [`assert_jq_compat`], with `input` in a file given after `args`.
fn assert_jq_compat_file(args: &[&str], input: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.ndjson");
    std::fs::write(&path, input).unwrap();
    let run = |cmd: &str| {
        Command::new(cmd)
            .args(args)
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let Ok(jq) = run("jq") else {
        return;
    };
    let qj = run(env!("CARGO_BIN_EXE_qj")).expect("failed to run qj");
    assert_eq!(
        (qj.status.code(), String::from_utf8_lossy(&qj.stdout)),
        (jq.status.code(), String::from_utf8_lossy(&jq.stdout)),
        "qj vs jq (exit code, stdout): args={args:?} + a file, input={input:?}"
    );
}

/// Assert that `qj -c FILTER` matches jq on NDJSON `input`, both on stdin and
/// as a file argument (which qj memory-maps). Skipped when jq isn't installed.
///
/// The tests using this used to compare the old core's NDJSON fast paths with
/// its normal path (`QJ_NO_FAST_PATH`); their filters are the shapes those
/// fast paths recognized.
fn assert_ndjson_jq_compat(filter: &str, input: &str) {
    assert_jq_compat(&["-c", filter], input);
    assert_jq_compat_file(&["-c", filter], input);
}

// --- NDJSON vs jq: the shapes the old core's fast paths recognized ---

#[test]
fn ndjson_vs_jq_field_chain() {
    let input = "{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n";
    assert_ndjson_jq_compat(".name", input);
}

#[test]
fn ndjson_vs_jq_nested_field() {
    let input = "{\"a\":{\"b\":\"deep\"}}\n{\"a\":{\"b\":\"val\"}}\n";
    assert_ndjson_jq_compat(".a.b", input);
}

#[test]
fn ndjson_vs_jq_select_eq() {
    let input = "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
    assert_ndjson_jq_compat("select(.type == \"PushEvent\")", input);
}

#[test]
fn ndjson_vs_jq_select_ne() {
    let input = "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
    assert_ndjson_jq_compat("select(.type != \"PushEvent\")", input);
}

#[test]
fn ndjson_vs_jq_select_gt() {
    let input = "{\"n\":5}\n{\"n\":15}\n{\"n\":10}\n";
    assert_ndjson_jq_compat("select(.n > 10)", input);
}

#[test]
fn ndjson_vs_jq_select_le() {
    let input = "{\"n\":5}\n{\"n\":15}\n{\"n\":10}\n";
    assert_ndjson_jq_compat("select(.n <= 10)", input);
}

#[test]
fn ndjson_vs_jq_select_eq_extract() {
    let input = "{\"type\":\"A\",\"x\":1}\n{\"type\":\"B\",\"x\":2}\n";
    assert_ndjson_jq_compat("select(.type == \"A\") | .x", input);
}

#[test]
fn ndjson_vs_jq_select_eq_obj() {
    let input = "{\"type\":\"A\",\"x\":1,\"y\":2}\n{\"type\":\"B\",\"x\":3,\"y\":4}\n";
    assert_ndjson_jq_compat("select(.type == \"A\") | {x: .x, y: .y}", input);
}

#[test]
fn ndjson_vs_jq_select_eq_arr() {
    let input = "{\"type\":\"A\",\"x\":1,\"y\":2}\n{\"type\":\"B\",\"x\":3,\"y\":4}\n";
    assert_ndjson_jq_compat("select(.type == \"A\") | [.x, .y]", input);
}

#[test]
fn ndjson_vs_jq_multi_field_obj() {
    let input = "{\"a\":1,\"b\":2,\"c\":3}\n{\"a\":4,\"b\":5,\"c\":6}\n";
    assert_ndjson_jq_compat("{a: .a, b: .b}", input);
}

#[test]
fn ndjson_vs_jq_multi_field_arr() {
    let input = "{\"a\":1,\"b\":2}\n{\"a\":3,\"b\":4}\n";
    assert_ndjson_jq_compat("[.a, .b]", input);
}

#[test]
fn ndjson_vs_jq_length() {
    let input = "{\"a\":1,\"b\":2}\n{\"x\":1}\n";
    assert_ndjson_jq_compat("length", input);
}

#[test]
fn ndjson_vs_jq_field_length() {
    let input = "{\"items\":[1,2,3]}\n{\"items\":[4]}\n";
    assert_ndjson_jq_compat(".items | length", input);
}

#[test]
fn ndjson_vs_jq_keys() {
    let input = "{\"b\":2,\"a\":1}\n{\"x\":1}\n";
    assert_ndjson_jq_compat("keys", input);
}

#[test]
fn ndjson_vs_jq_select_test() {
    let input = "{\"msg\":\"error: disk full\"}\n{\"msg\":\"ok\"}\n{\"msg\":\"error: timeout\"}\n";
    assert_ndjson_jq_compat(r#"select(.msg | test("error"))"#, input);
}

#[test]
fn ndjson_vs_jq_select_startswith() {
    let input = "{\"url\":\"/api/users\"}\n{\"url\":\"/web/home\"}\n";
    assert_ndjson_jq_compat(r#"select(.url | startswith("/api"))"#, input);
}

#[test]
fn ndjson_vs_jq_select_endswith() {
    let input = "{\"file\":\"data.json\"}\n{\"file\":\"data.csv\"}\n";
    assert_ndjson_jq_compat(r#"select(.file | endswith(".json"))"#, input);
}

#[test]
fn ndjson_vs_jq_select_contains() {
    let input = "{\"desc\":\"hello alice\"}\n{\"desc\":\"hello bob\"}\n";
    assert_ndjson_jq_compat(r#"select(.desc | contains("alice"))"#, input);
}

#[test]
fn ndjson_vs_jq_select_test_extract() {
    let input = "{\"msg\":\"error: disk full\",\"code\":500}\n{\"msg\":\"ok\",\"code\":200}\n";
    assert_ndjson_jq_compat(r#"select(.msg | test("error")) | .code"#, input);
}

#[test]
fn ndjson_vs_jq_select_float_vs_int() {
    // Edge case: 1.0 == 1 should match in both paths
    let input = "{\"n\":1.0,\"id\":\"a\"}\n{\"n\":2,\"id\":\"b\"}\n";
    assert_ndjson_jq_compat("select(.n == 1)", input);
}

#[test]
fn ndjson_vs_jq_select_escaped_string() {
    // Escaped strings (\n) in the predicate and the output
    let input = "{\"s\":\"line1\\nline2\",\"id\":1}\n{\"s\":\"other\",\"id\":2}\n";
    assert_ndjson_jq_compat("select(.s == \"line1\\nline2\")", input);
}

// --- Basic NDJSON processing ---

#[test]
fn ndjson_field_extraction() {
    let input = r#"{"name":"alice","age":30}
{"name":"bob","age":25}
{"name":"charlie","age":35}
"#;
    let out = qj_stdin(&["-c", ".name"], input);
    assert_eq!(out, "\"alice\"\n\"bob\"\n\"charlie\"\n");
}

#[test]
fn ndjson_identity() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_complex_filter() {
    let input = r#"{"name":"alice","score":90}
{"name":"bob","score":40}
{"name":"charlie","score":85}
"#;
    let out = qj_stdin(&["-c", "select(.score > 50) | .name"], input);
    assert_eq!(out, "\"alice\"\n\"charlie\"\n");
}

#[test]
fn ndjson_pipe_builtin() {
    let input = r#"{"items":[1,2,3]}
{"items":[4,5]}
"#;
    let out = qj_stdin(&["-c", ".items | length"], input);
    assert_eq!(out, "3\n2\n");
}

#[test]
fn ndjson_object_construct() {
    let input = r#"{"first":"alice","last":"smith"}
{"first":"bob","last":"jones"}
"#;
    let out = qj_stdin(&["-c", "{name: .first}"], input);
    assert_eq!(out, "{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n");
}

// --- Edge cases ---

#[test]
fn ndjson_empty_lines() {
    let input = "{\"a\":1}\n\n{\"b\":2}\n\n";
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_trailing_newline() {
    let input = "{\"a\":1}\n{\"b\":2}\n";
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_no_trailing_newline() {
    let input = "{\"a\":1}\n{\"b\":2}";
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_single_line_not_detected() {
    // Single JSON object should NOT be treated as NDJSON
    let input = r#"{"a":1}"#;
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n");
}

// --- --jsonl flag ---

#[test]
fn jsonl_flag_forces_ndjson() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = qj_stdin(&["--jsonl", "-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

// --- File input ---

#[test]
fn ndjson_file_field_extraction() {
    let input = r#"{"name":"alice"}
{"name":"bob"}
"#;
    let out = qj_file(&["-c", ".name"], input);
    assert_eq!(out, "\"alice\"\n\"bob\"\n");
}

#[test]
fn ndjson_file_identity() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = qj_file(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_file_with_jsonl_flag() {
    let input = r#"{"x":1}
{"x":2}
"#;
    let out = qj_file(&["--jsonl", "-c", ".x"], input);
    assert_eq!(out, "1\n2\n");
}

// --- Output ordering ---

#[test]
fn ndjson_output_order_preserved() {
    // Generate enough lines to trigger parallel processing (> 1 chunk)
    // even though in tests with small data it stays sequential
    let mut input = String::new();
    for i in 0..100 {
        input.push_str(&format!("{{\"i\":{i}}}\n"));
    }
    let out = qj_stdin(&["-c", ".i"], &input);
    let expected: String = (0..100).map(|i| format!("{i}\n")).collect();
    assert_eq!(out, expected);
}

// --- Error handling ---

fn qj_stdin_lossy(args: &[&str], input: &str) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|child| feed(child, input))
        .expect("failed to run qj");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn ndjson_malformed_line_mixed() {
    // Mix of valid and invalid JSON lines.
    // is_ndjson returns false (second line starts with 'n', not '{'),
    // so this goes through the normal single-doc → multi-doc fallback path.
    // Like jq, parsing stops at the first invalid document.
    let input = "{\"a\":1}\nnot json\n{\"b\":2}\n";
    let (stdout, stderr, success) = qj_stdin_lossy(&["-c", "."], input);
    assert!(
        stdout.contains("{\"a\":1}"),
        "first valid doc should appear in output"
    );
    assert!(!success, "should exit with error due to invalid JSON");
    assert!(!stderr.is_empty(), "should report parse error on stderr");
}

#[test]
fn ndjson_whitespace_only_lines() {
    // Lines with only whitespace between valid JSON
    let input = "{\"a\":1}\n   \n\t\n{\"b\":2}\n";
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_large_line_count_ordering() {
    // 10,000 lines — verify ordering is preserved
    let mut input = String::new();
    for i in 0..10_000 {
        input.push_str(&format!("{{\"i\":{i}}}\n"));
    }
    let out = qj_stdin(&["-c", ".i"], &input);
    let expected: String = (0..10_000).map(|i| format!("{i}\n")).collect();
    assert_eq!(out, expected);
}

// --- Array NDJSON ---

#[test]
fn ndjson_arrays() {
    let input = "[1,2,3]\n[4,5,6]\n";
    let out = qj_stdin(&["-c", ".[0]"], input);
    assert_eq!(out, "1\n4\n");
}

// --- Raw output ---

#[test]
fn ndjson_raw_output() {
    let input = r#"{"name":"alice"}
{"name":"bob"}
"#;
    let out = qj_stdin(&["-r", ".name"], input);
    assert_eq!(out, "alice\nbob\n");
}

// --- Pretty output ---

#[test]
fn ndjson_pretty_output() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = qj_stdin(&["."], input);
    assert_eq!(out, "{\n  \"a\": 1\n}\n{\n  \"b\": 2\n}\n");
}

// --- Field chain edge cases ---

#[test]
fn ndjson_field_chain_deeply_nested() {
    let input = r#"{"a":{"b":{"c":{"d":"deep"}}}}
{"a":{"b":{"c":{"d":"val"}}}}
"#;
    let out = qj_stdin(&["-c", ".a.b.c.d"], input);
    assert_eq!(out, "\"deep\"\n\"val\"\n");
}

#[test]
fn ndjson_field_chain_missing_intermediate() {
    // .a.b where .a doesn't have .b — should produce null
    let input = r#"{"a":{"b":"yes"}}
{"a":{"c":"no"}}
{"x":1}
"#;
    let out = qj_stdin(&["-c", ".a.b"], input);
    assert_eq!(out, "\"yes\"\nnull\nnull\n");
}

#[test]
fn ndjson_field_chain_null_value() {
    let input = r#"{"x":null}
{"x":42}
"#;
    let out = qj_stdin(&["-c", ".x"], input);
    assert_eq!(out, "null\n42\n");
}

#[test]
fn ndjson_field_chain_object_value() {
    let input = r#"{"data":{"nested":true}}
{"data":{"nested":false}}
"#;
    let out = qj_stdin(&["-c", ".data"], input);
    assert_eq!(out, "{\"nested\":true}\n{\"nested\":false}\n");
}

#[test]
fn ndjson_field_chain_array_value() {
    let input = r#"{"items":[1,2,3]}
{"items":[]}
"#;
    let out = qj_stdin(&["-c", ".items"], input);
    assert_eq!(out, "[1,2,3]\n[]\n");
}

#[test]
fn ndjson_field_chain_boolean_value() {
    let input = r#"{"active":true}
{"active":false}
"#;
    let out = qj_stdin(&["-c", ".active"], input);
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn ndjson_field_chain_mixed_types() {
    // Field has different types across lines
    let input = r#"{"v":"string"}
{"v":42}
{"v":true}
{"v":null}
{"v":[1]}
{"v":{"a":1}}
"#;
    let out = qj_stdin(&["-c", ".v"], input);
    assert_eq!(out, "\"string\"\n42\ntrue\nnull\n[1]\n{\"a\":1}\n");
}

#[test]
fn ndjson_field_chain_special_chars_in_value() {
    // Values with quotes, backslashes, unicode
    let input = "{\"msg\":\"hello \\\"world\\\"\"}\n{\"msg\":\"line1\\nline2\"}\n";
    let out = qj_stdin(&["-c", ".msg"], &input);
    assert_eq!(out, "\"hello \\\"world\\\"\"\n\"line1\\nline2\"\n");
}

#[test]
fn ndjson_field_chain_empty_string_value() {
    let input = r#"{"name":""}
{"name":"bob"}
"#;
    let out = qj_stdin(&["-c", ".name"], input);
    assert_eq!(out, "\"\"\n\"bob\"\n");
}

#[test]
fn ndjson_field_chain_large_values() {
    // Ensure field extraction works with large nested objects
    let big_array: String = (0..100)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let input = format!("{{\"data\":[{big_array}]}}\n{{\"data\":[1]}}\n");
    let out = qj_stdin(&["-c", ".data | length"], &input);
    assert_eq!(out, "100\n1\n");
}

#[test]
fn ndjson_field_chain_whitespace_in_json() {
    // Lines with extra whitespace (tabs, spaces) that get trimmed
    let input = "  {\"a\":1}  \n\t{\"a\":2}\t\n";
    let out = qj_stdin(&["-c", ".a"], &input);
    assert_eq!(out, "1\n2\n");
}

#[test]
fn ndjson_field_chain_raw_output_escape() {
    // Raw output with escape sequences in the string
    let input = "{\"msg\":\"hello\\tworld\"}\n{\"msg\":\"foo\\nbar\"}\n";
    let out = qj_stdin(&["-r", ".msg"], &input);
    assert_eq!(out, "hello\tworld\nfoo\nbar\n");
}

// --- select ---

#[test]
fn ndjson_select_eq_string() {
    let input = r#"{"type":"PushEvent","id":1}
{"type":"WatchEvent","id":2}
{"type":"PushEvent","id":3}
"#;
    let out = qj_stdin(&["-c", "select(.type == \"PushEvent\")"], input);
    assert_eq!(
        out,
        "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"PushEvent\",\"id\":3}\n"
    );
}

#[test]
fn ndjson_select_ne_string() {
    let input = r#"{"type":"PushEvent","id":1}
{"type":"WatchEvent","id":2}
"#;
    let out = qj_stdin(&["-c", "select(.type != \"PushEvent\")"], input);
    assert_eq!(out, "{\"type\":\"WatchEvent\",\"id\":2}\n");
}

#[test]
fn ndjson_select_eq_int() {
    let input = r#"{"count":42,"name":"a"}
{"count":7,"name":"b"}
{"count":42,"name":"c"}
"#;
    let out = qj_stdin(&["-c", "select(.count == 42)"], input);
    assert_eq!(
        out,
        "{\"count\":42,\"name\":\"a\"}\n{\"count\":42,\"name\":\"c\"}\n"
    );
}

#[test]
fn ndjson_select_eq_bool() {
    let input = r#"{"active":true,"name":"a"}
{"active":false,"name":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.active == true)"], input);
    assert_eq!(out, "{\"active\":true,\"name\":\"a\"}\n");
}

#[test]
fn ndjson_select_eq_null() {
    let input = r#"{"x":null}
{"x":1}
{"y":2}
"#;
    let out = qj_stdin(&["-c", "select(.x == null)"], input);
    // Both {"x":null} and {"y":2} match because missing .x returns null
    assert_eq!(out, "{\"x\":null}\n{\"y\":2}\n");
}

#[test]
fn ndjson_select_eq_nested_field() {
    let input = r#"{"actor":{"login":"alice"},"id":1}
{"actor":{"login":"bob"},"id":2}
"#;
    let out = qj_stdin(&["-c", "select(.actor.login == \"alice\")"], input);
    assert_eq!(out, "{\"actor\":{\"login\":\"alice\"},\"id\":1}\n");
}

// --- length/keys ---

#[test]
fn ndjson_bare_length() {
    let input = r#"{"a":1,"b":2}
{"x":1}
"#;
    let out = qj_stdin(&["-c", "length"], input);
    assert_eq!(out, "2\n1\n");
}

#[test]
fn ndjson_field_length() {
    let input = r#"{"items":[1,2,3]}
{"items":[4,5]}
"#;
    let out = qj_stdin(&["-c", ".items | length"], input);
    assert_eq!(out, "3\n2\n");
}

#[test]
fn ndjson_bare_keys() {
    let input = r#"{"b":2,"a":1}
{"x":1}
"#;
    let out = qj_stdin(&["-c", "keys"], input);
    assert_eq!(out, "[\"a\",\"b\"]\n[\"x\"]\n");
}

#[test]
fn ndjson_field_keys() {
    let input = r#"{"data":{"b":2,"a":1}}
{"data":{"x":1}}
"#;
    let out = qj_stdin(&["-c", ".data | keys"], input);
    assert_eq!(out, "[\"a\",\"b\"]\n[\"x\"]\n");
}

// --- select edge cases ---

#[test]
fn ndjson_select_no_match() {
    let input = r#"{"type":"WatchEvent"}
{"type":"IssuesEvent"}
"#;
    let out = qj_stdin(&["-c", "select(.type == \"PushEvent\")"], input);
    assert_eq!(out, "");
}

#[test]
fn ndjson_select_with_empty_lines() {
    let input = "{\"type\":\"PushEvent\"}\n\n{\"type\":\"WatchEvent\"}\n";
    let out = qj_stdin(&["-c", "select(.type == \"PushEvent\")"], input);
    assert_eq!(out, "{\"type\":\"PushEvent\"}\n");
}

#[test]
fn ndjson_select_large_line_count() {
    let mut input = String::new();
    for i in 0..1000 {
        input.push_str(&format!(
            "{{\"i\":{i},\"type\":\"{}\"}}\n",
            if i % 3 == 0 { "A" } else { "B" }
        ));
    }
    let out = qj_stdin(&["-c", "select(.type == \"A\")"], &input);
    let count = out.lines().count();
    // i % 3 == 0: 0,3,6,...,999 → 334 lines
    assert_eq!(count, 334);
}

#[test]
fn ndjson_select_string_with_special_chars() {
    let input = r#"{"msg":"hello \"world\""}
{"msg":"normal"}
"#;
    let out = qj_stdin(&["-c", r#"select(.msg == "normal")"#], input);
    assert_eq!(out, "{\"msg\":\"normal\"}\n");
}

#[test]
fn ndjson_select_negative_int() {
    let input = r#"{"n":-1}
{"n":1}
{"n":0}
"#;
    let out = qj_stdin(&["-c", "select(.n == -1)"], input);
    assert_eq!(out, "{\"n\":-1}\n");
}

// --- select on equal values with different text ---

#[test]
fn ndjson_select_float_vs_int() {
    // 1.0 == 1 should match, and the line keeps its literal
    let input = r#"{"n":1.0,"id":"a"}
{"n":2,"id":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.n == 1)"], input);
    assert_eq!(out, "{\"n\":1.0,\"id\":\"a\"}\n");
}

#[test]
fn ndjson_select_scientific_notation() {
    // 1e2 == 100 should match, and the literal prints in jq's canonical
    // form (1E+2)
    let input = r#"{"n":1e2,"id":"a"}
{"n":99,"id":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.n == 100)"], input);
    assert_eq!(out, "{\"n\":1E+2,\"id\":\"a\"}\n");
    assert_jq_compat(&["-c", "select(.n == 100)"], input);
}

#[test]
fn ndjson_select_unicode_escape() {
    // \u0041 is "A": it matches, and prints as "A" (jq re-serializes).
    let input = "{\"s\":\"\\u0041\",\"id\":1}\n{\"s\":\"B\",\"id\":2}\n";
    let out = qj_stdin(&["-c", "select(.s == \"A\")"], &input);
    assert_eq!(out, "{\"s\":\"A\",\"id\":1}\n");
}

#[test]
fn ndjson_select_trailing_zero_float() {
    // 42.00 == 42 should match
    let input = "{\"n\":42.00}\n{\"n\":43}\n";
    let out = qj_stdin(&["-c", "select(.n == 42)"], &input);
    assert_eq!(out, "{\"n\":42.00}\n");
}

#[test]
fn ndjson_select_type_mismatch_string_vs_int() {
    // "42" (string) != 42 (int) — should NOT match
    let input = r#"{"n":"42"}
{"n":42}
"#;
    let out = qj_stdin(&["-c", "select(.n == 42)"], input);
    assert_eq!(out, "{\"n\":42}\n");
}

#[test]
fn ndjson_select_float_ne() {
    // 1.0 != 1 should NOT output (they're equal), 2 != 1 should output
    let input = "{\"n\":1.0}\n{\"n\":2}\n";
    let out = qj_stdin(&["-c", "select(.n != 1)"], &input);
    assert_eq!(out, "{\"n\":2}\n");
}

#[test]
fn ndjson_select_mixed_fallback_and_fast() {
    // Equal numbers written differently (42, 42.0), and near misses
    let input = r#"{"n":42,"id":"exact"}
{"n":42.0,"id":"float"}
{"n":1e2,"id":"sci"}
{"n":100,"id":"plain"}
{"n":99,"id":"miss"}
"#;
    let out = qj_stdin(&["-c", "select(.n == 42)"], input);
    assert_eq!(
        out,
        "{\"n\":42,\"id\":\"exact\"}\n{\"n\":42.0,\"id\":\"float\"}\n"
    );
}

// --- length/keys edge cases ---

#[test]
fn ndjson_length_empty_objects() {
    let input = "{}\n{\"a\":1}\n";
    let out = qj_stdin(&["-c", "length"], input);
    assert_eq!(out, "0\n1\n");
}

#[test]
fn ndjson_keys_empty_object() {
    let input = "{}\n{\"b\":1,\"a\":2}\n";
    let out = qj_stdin(&["-c", "keys"], input);
    assert_eq!(out, "[]\n[\"a\",\"b\"]\n");
}

#[test]
fn ndjson_length_on_arrays_ndjson() {
    let input = "[1,2,3]\n[4,5]\n[]\n";
    let out = qj_stdin(&["-c", "length"], input);
    assert_eq!(out, "3\n2\n0\n");
}

#[test]
fn ndjson_keys_on_arrays_ndjson() {
    let input = "[1,2,3]\n[4]\n";
    let out = qj_stdin(&["-c", "keys"], input);
    assert_eq!(out, "[0,1,2]\n[0]\n");
}

#[test]
fn ndjson_string_length_fallback() {
    // String length (codepoints) of a field
    let input = r#"{"name":"alice"}
{"name":"bob"}
"#;
    let out = qj_stdin(&["-c", ".name | length"], input);
    assert_eq!(out, "5\n3\n");
}

#[test]
fn ndjson_nested_field_length() {
    let input = r#"{"a":{"b":[1,2,3]}}
{"a":{"b":[4]}}
"#;
    let out = qj_stdin(&["-c", ".a.b | length"], input);
    assert_eq!(out, "3\n1\n");
}

#[test]
fn ndjson_nested_field_keys() {
    let input = r#"{"meta":{"b":2,"a":1}}
{"meta":{"z":1}}
"#;
    let out = qj_stdin(&["-c", ".meta | keys"], input);
    assert_eq!(out, "[\"a\",\"b\"]\n[\"z\"]\n");
}

#[test]
fn ndjson_length_large_line_count() {
    let mut input = String::new();
    for i in 0..500 {
        input.push_str(&format!("{{\"i\":{i}}}\n"));
    }
    let out = qj_stdin(&["-c", "length"], &input);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 500);
    // Each line is an object with 1 key
    assert!(lines.iter().all(|l| *l == "1"));
}

// --- select + field extraction ---

#[test]
fn ndjson_select_eq_field_extraction() {
    let input = r#"{"type":"PushEvent","actor":"alice"}
{"type":"WatchEvent","actor":"bob"}
{"type":"PushEvent","actor":"charlie"}
"#;
    let out = qj_stdin(&["-c", r#"select(.type == "PushEvent") | .actor"#], input);
    assert_eq!(out, "\"alice\"\n\"charlie\"\n");
}

#[test]
fn ndjson_select_eq_field_nested_output() {
    let input = r#"{"type":"PushEvent","actor":{"login":"alice"}}
{"type":"WatchEvent","actor":{"login":"bob"}}
"#;
    let out = qj_stdin(
        &["-c", r#"select(.type == "PushEvent") | .actor.login"#],
        input,
    );
    assert_eq!(out, "\"alice\"\n");
}

#[test]
fn ndjson_select_eq_field_raw_output() {
    let input = r#"{"type":"PushEvent","name":"alice"}
{"type":"WatchEvent","name":"bob"}
"#;
    let out = qj_stdin(&["-r", r#"select(.type == "PushEvent") | .name"#], input);
    assert_eq!(out, "alice\n");
}

#[test]
fn ndjson_select_ne_field_extraction() {
    let input = r#"{"type":"PushEvent","name":"a"}
{"type":"WatchEvent","name":"b"}
"#;
    let out = qj_stdin(&["-c", r#"select(.type != "PushEvent") | .name"#], input);
    assert_eq!(out, "\"b\"\n");
}

#[test]
fn ndjson_select_eq_field_no_match() {
    let input = r#"{"type":"WatchEvent","name":"a"}
{"type":"IssuesEvent","name":"b"}
"#;
    let out = qj_stdin(&["-c", r#"select(.type == "PushEvent") | .name"#], input);
    assert_eq!(out, "");
}

#[test]
fn ndjson_select_eq_field_missing_output() {
    let input = r#"{"type":"PushEvent"}
{"type":"WatchEvent","name":"b"}
"#;
    let out = qj_stdin(&["-c", r#"select(.type == "PushEvent") | .name"#], input);
    assert_eq!(out, "null\n");
}

#[test]
fn ndjson_select_eq_field_float_fallback() {
    // 1.0 == 1 should match via fallback
    let input = r#"{"n":1.0,"name":"a"}
{"n":2,"name":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.n == 1) | .name"], input);
    assert_eq!(out, "\"a\"\n");
}

// --- select + object construction ---

#[test]
fn ndjson_select_eq_obj() {
    let input = r#"{"type":"PushEvent","id":1,"actor":"alice"}
{"type":"WatchEvent","id":2,"actor":"bob"}
{"type":"PushEvent","id":3,"actor":"charlie"}
"#;
    let out = qj_stdin(
        &["-c", r#"select(.type == "PushEvent") | {id: .id, actor}"#],
        input,
    );
    assert_eq!(
        out,
        "{\"id\":1,\"actor\":\"alice\"}\n{\"id\":3,\"actor\":\"charlie\"}\n"
    );
}

// --- select + array construction ---

#[test]
fn ndjson_select_eq_arr() {
    let input = r#"{"type":"PushEvent","id":1,"actor":"alice"}
{"type":"WatchEvent","id":2,"actor":"bob"}
"#;
    let out = qj_stdin(
        &["-c", r#"select(.type == "PushEvent") | [.id, .actor]"#],
        input,
    );
    assert_eq!(out, "[1,\"alice\"]\n");
}

// --- Ordering operators in select (>, <, >=, <=) ---

#[test]
fn ndjson_select_gt_int() {
    let input = r#"{"score":90,"name":"a"}
{"score":40,"name":"b"}
{"score":85,"name":"c"}
"#;
    let out = qj_stdin(&["-c", "select(.score > 50)"], input);
    assert_eq!(
        out,
        "{\"score\":90,\"name\":\"a\"}\n{\"score\":85,\"name\":\"c\"}\n"
    );
}

#[test]
fn ndjson_select_lt_int() {
    let input = r#"{"n":10}
{"n":50}
{"n":5}
"#;
    let out = qj_stdin(&["-c", "select(.n < 10)"], input);
    assert_eq!(out, "{\"n\":5}\n");
}

#[test]
fn ndjson_select_ge_int() {
    let input = r#"{"n":10}
{"n":50}
{"n":5}
"#;
    let out = qj_stdin(&["-c", "select(.n >= 10)"], input);
    assert_eq!(out, "{\"n\":10}\n{\"n\":50}\n");
}

#[test]
fn ndjson_select_le_int() {
    let input = r#"{"n":10}
{"n":50}
{"n":5}
"#;
    let out = qj_stdin(&["-c", "select(.n <= 10)"], input);
    assert_eq!(out, "{\"n\":10}\n{\"n\":5}\n");
}

#[test]
fn ndjson_select_gt_float() {
    let input = r#"{"n":3.14}
{"n":2.71}
{"n":1.0}
"#;
    let out = qj_stdin(&["-c", "select(.n > 3)"], input);
    assert_eq!(out, "{\"n\":3.14}\n");
}

#[test]
fn ndjson_select_gt_negative() {
    let input = r#"{"n":-5}
{"n":0}
{"n":5}
"#;
    let out = qj_stdin(&["-c", "select(.n > -1)"], input);
    assert_eq!(out, "{\"n\":0}\n{\"n\":5}\n");
}

#[test]
fn ndjson_select_gt_string() {
    let input = r#"{"s":"apple"}
{"s":"banana"}
{"s":"cherry"}
"#;
    let out = qj_stdin(&["-c", r#"select(.s > "banana")"#], input);
    assert_eq!(out, "{\"s\":\"cherry\"}\n");
}

#[test]
fn ndjson_select_gt_field_extract() {
    let input = r#"{"n":20,"name":"a"}
{"n":5,"name":"b"}
{"n":100,"name":"c"}
"#;
    let out = qj_stdin(&["-c", "select(.n > 10) | .name"], input);
    assert_eq!(out, "\"a\"\n\"c\"\n");
}

#[test]
fn ndjson_select_gt_obj_extract() {
    let input = r#"{"n":20,"name":"a"}
{"n":5,"name":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.n > 10) | {name}"], input);
    assert_eq!(out, "{\"name\":\"a\"}\n");
}

#[test]
fn ndjson_select_gt_no_match() {
    let input = r#"{"n":1}
{"n":2}
"#;
    let out = qj_stdin(&["-c", "select(.n > 100)"], input);
    assert_eq!(out, "");
}

#[test]
fn ndjson_select_gt_large() {
    let mut input = String::new();
    for i in 0..1000 {
        input.push_str(&format!("{{\"i\":{i}}}\n"));
    }
    let out = qj_stdin(&["-c", "select(.i >= 990)"], &input);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 10);
}

// --- Multi-field object construction ---

#[test]
fn ndjson_multi_field_obj() {
    let input = r#"{"type":"PushEvent","id":1,"actor":"alice"}
{"type":"WatchEvent","id":2,"actor":"bob"}
"#;
    let out = qj_stdin(&["-c", "{type, id: .id, actor}"], input);
    assert_eq!(
        out,
        "{\"type\":\"PushEvent\",\"id\":1,\"actor\":\"alice\"}\n{\"type\":\"WatchEvent\",\"id\":2,\"actor\":\"bob\"}\n"
    );
}

#[test]
fn ndjson_multi_field_obj_nested() {
    let input = r#"{"actor":{"login":"alice"},"repo":{"name":"foo"}}
{"actor":{"login":"bob"},"repo":{"name":"bar"}}
"#;
    let out = qj_stdin(&["-c", "{actor: .actor.login, repo: .repo.name}"], input);
    assert_eq!(
        out,
        "{\"actor\":\"alice\",\"repo\":\"foo\"}\n{\"actor\":\"bob\",\"repo\":\"bar\"}\n"
    );
}

#[test]
fn ndjson_multi_field_obj_missing_field() {
    let input = r#"{"type":"PushEvent"}
{"type":"WatchEvent","id":2}
"#;
    let out = qj_stdin(&["-c", "{type, id: .id}"], input);
    assert_eq!(
        out,
        "{\"type\":\"PushEvent\",\"id\":null}\n{\"type\":\"WatchEvent\",\"id\":2}\n"
    );
}

#[test]
fn ndjson_multi_field_obj_large() {
    let mut input = String::new();
    for i in 0..1000 {
        input.push_str(&format!("{{\"i\":{i},\"name\":\"n{i}\"}}\n"));
    }
    let out = qj_stdin(&["-c", "{i: .i, name}"], &input);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1000);
    assert_eq!(lines[0], "{\"i\":0,\"name\":\"n0\"}");
    assert_eq!(lines[999], "{\"i\":999,\"name\":\"n999\"}");
}

// --- Multi-field array construction ---

#[test]
fn ndjson_multi_field_arr() {
    let input = r#"{"x":1,"y":2}
{"x":3,"y":4}
"#;
    let out = qj_stdin(&["-c", "[.x, .y]"], input);
    assert_eq!(out, "[1,2]\n[3,4]\n");
}

#[test]
fn ndjson_multi_field_arr_nested() {
    let input = r#"{"a":{"b":"deep"},"c":1}
{"a":{"b":"val"},"c":2}
"#;
    let out = qj_stdin(&["-c", "[.a.b, .c]"], input);
    assert_eq!(out, "[\"deep\",1]\n[\"val\",2]\n");
}

#[test]
fn ndjson_multi_field_arr_missing_field() {
    let input = r#"{"x":1}
{"x":2,"y":3}
"#;
    let out = qj_stdin(&["-c", "[.x, .y]"], input);
    assert_eq!(out, "[1,null]\n[2,3]\n");
}

#[test]
fn ndjson_multi_field_arr_large() {
    let mut input = String::new();
    for i in 0..1000 {
        input.push_str(&format!("{{\"a\":{i},\"b\":{}}}\n", i * 10));
    }
    let out = qj_stdin(&["-c", "[.a, .b]"], &input);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1000);
    assert_eq!(lines[0], "[0,0]");
    assert_eq!(lines[999], "[999,9990]");
}

// --- String predicate select ---

#[test]
fn ndjson_select_test_basic() {
    let input = r#"{"msg":"error: disk full","id":1}
{"msg":"ok","id":2}
{"msg":"error: timeout","id":3}
"#;
    let out = qj_stdin(&["-c", r#"select(.msg | test("error"))"#], input);
    assert_eq!(
        out,
        "{\"msg\":\"error: disk full\",\"id\":1}\n{\"msg\":\"error: timeout\",\"id\":3}\n"
    );
}

#[test]
fn ndjson_select_startswith() {
    let input = r#"{"url":"/api/users","id":1}
{"url":"/web/home","id":2}
{"url":"/api/items","id":3}
"#;
    let out = qj_stdin(&["-c", r#"select(.url | startswith("/api"))"#], input);
    assert_eq!(
        out,
        "{\"url\":\"/api/users\",\"id\":1}\n{\"url\":\"/api/items\",\"id\":3}\n"
    );
}

#[test]
fn ndjson_select_endswith() {
    let input = r#"{"file":"data.json","id":1}
{"file":"data.csv","id":2}
{"file":"config.json","id":3}
"#;
    let out = qj_stdin(&["-c", r#"select(.file | endswith(".json"))"#], input);
    assert_eq!(
        out,
        "{\"file\":\"data.json\",\"id\":1}\n{\"file\":\"config.json\",\"id\":3}\n"
    );
}

#[test]
fn ndjson_select_contains_string() {
    let input = r#"{"desc":"hello alice","id":1}
{"desc":"hello bob","id":2}
{"desc":"alice says hi","id":3}
"#;
    let out = qj_stdin(&["-c", r#"select(.desc | contains("alice"))"#], input);
    assert_eq!(
        out,
        "{\"desc\":\"hello alice\",\"id\":1}\n{\"desc\":\"alice says hi\",\"id\":3}\n"
    );
}

#[test]
fn ndjson_select_test_regex() {
    let input = r#"{"code":"ERR-001"}
{"code":"OK-200"}
{"code":"ERR-42"}
"#;
    let out = qj_stdin(&["-c", r#"select(.code | test("^ERR-\\d+$"))"#], input);
    assert_eq!(out, "{\"code\":\"ERR-001\"}\n{\"code\":\"ERR-42\"}\n");
}

#[test]
fn ndjson_select_test_extract_field() {
    let input = r#"{"msg":"error: disk full","code":500}
{"msg":"ok","code":200}
{"msg":"error: timeout","code":504}
"#;
    let out = qj_stdin(&["-c", r#"select(.msg | test("error")) | .code"#], input);
    assert_eq!(out, "500\n504\n");
}

#[test]
fn ndjson_select_startswith_extract() {
    let input = r#"{"url":"/api/users","method":"GET"}
{"url":"/web/home","method":"GET"}
"#;
    let out = qj_stdin(
        &["-c", r#"select(.url | startswith("/api")) | .method"#],
        input,
    );
    assert_eq!(out, "\"GET\"\n");
}

#[test]
fn ndjson_select_test_no_match() {
    let input = r#"{"msg":"ok"}
{"msg":"success"}
"#;
    let out = qj_stdin(&["-c", r#"select(.msg | test("error"))"#], input);
    assert_eq!(out, "");
}

#[test]
fn ndjson_select_test_escaped_string() {
    let input = "{\"msg\":\"line1\\nline2\",\"id\":1}\n{\"msg\":\"ok\",\"id\":2}\n";
    let out = qj_stdin(&["-c", r#"select(.msg | contains("line1"))"#], input);
    assert_eq!(out, "{\"msg\":\"line1\\nline2\",\"id\":1}\n");
}

#[test]
fn ndjson_select_contains_nested_field() {
    let input = r#"{"actor":{"login":"bot-alice"},"id":1}
{"actor":{"login":"human-bob"},"id":2}
"#;
    let out = qj_stdin(
        &["-c", r#"select(.actor.login | startswith("bot"))"#],
        input,
    );
    assert_eq!(out, "{\"actor\":{\"login\":\"bot-alice\"},\"id\":1}\n");
}

#[test]
fn ndjson_select_test_large() {
    let mut input = String::new();
    for i in 0..2000 {
        if i % 100 == 0 {
            input.push_str(&format!("{{\"msg\":\"error-{i}\",\"n\":{i}}}\n"));
        } else {
            input.push_str(&format!("{{\"msg\":\"ok-{i}\",\"n\":{i}}}\n"));
        }
    }
    let out = qj_stdin(&["-c", r#"select(.msg | test("^error"))"#], &input);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 20);
}

// --- Iterate ---

#[test]
fn ndjson_iterate() {
    let input = r#"{"a":1,"b":2}
{"c":3}
"#;
    let out = qj_stdin(&["-c", ".[]"], input);
    assert_eq!(out, "1\n2\n3\n");
}

// =============================================================================
// Golden differential tests: qj vs jq on diverse NDJSON
//
// These tests generate NDJSON with edge cases (type mismatches, missing fields,
// unicode, escapes, numeric edge cases) and compare qj with jq for each shape
// the old core had an NDJSON fast path for.
// =============================================================================

/// Simple deterministic PRNG (xorshift32) for generating diverse test data
/// without pulling in proptest. Seed is fixed for reproducibility.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        let span = (hi - lo) as u64;
        if span == 0 {
            return lo;
        }
        lo + (self.next() as u64 % span) as i64
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.next() as usize % items.len()]
    }
}

/// Build NDJSON with diverse edge-case objects.
fn generate_diverse_ndjson(rng: &mut Rng, count: usize) -> String {
    let types = &["PushEvent", "WatchEvent", "CreateEvent", "DeleteEvent"];
    let names = &["alice", "bob", "charlie", "delta", "echo"];
    let mut buf = String::new();

    for i in 0..count {
        let kind = rng.next() % 10;
        match kind {
            // Normal object with all fields
            0..=3 => {
                let ty = rng.pick(types);
                let name = rng.pick(names);
                let n = rng.range(-100, 200);
                let active = if rng.next() % 2 == 0 { "true" } else { "false" };
                buf.push_str(&format!(
                    "{{\"type\":\"{ty}\",\"name\":\"{name}\",\"count\":{n},\"active\":{active},\"actor\":{{\"login\":\"{name}\"}},\"meta\":{{\"x\":1,\"y\":2}},\"items\":[1,2,3]}}\n"
                ));
            }
            // Missing fields
            4 => {
                buf.push_str(&format!("{{\"id\":{i}}}\n"));
            }
            // Null fields
            5 => {
                buf.push_str(&format!(
                    "{{\"type\":null,\"name\":null,\"count\":null,\"active\":null}}\n"
                ));
            }
            // Numeric edge cases: floats, scientific notation, negative, trailing
            // zeros, negative zero. jq keeps each literal (printed canonically).
            6 => {
                let vals = &["1.0", "1e2", "0.0001", "42.00", "1.5e10", "-3.14", "-0"];
                let v = rng.pick(vals);
                let name = rng.pick(names);
                buf.push_str(&format!(
                    "{{\"type\":\"PushEvent\",\"name\":\"{name}\",\"count\":{v},\"active\":true}}\n"
                ));
            }
            // String with escape sequences
            7 => {
                buf.push_str(&format!(
                    "{{\"type\":\"PushEvent\",\"name\":\"line1\\nline2\",\"count\":{i},\"desc\":\"tab\\there\"}}\n"
                ));
            }
            // Type mismatches: count as string, name as number
            8 => {
                buf.push_str(&format!(
                    "{{\"type\":42,\"name\":999,\"count\":\"not_a_number\",\"active\":\"yes\"}}\n"
                ));
            }
            // Empty/minimal objects
            _ => {
                buf.push_str("{}\n");
            }
        }
    }
    buf
}

#[test]
fn golden_ndjson_field_chain() {
    let mut rng = Rng::new(1001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat(".name", &input);
    assert_ndjson_jq_compat(".actor.login", &input);
    assert_ndjson_jq_compat(".missing", &input);
    assert_ndjson_jq_compat(".meta.x", &input);
}

#[test]
fn golden_ndjson_select_eq_string() {
    let mut rng = Rng::new(2001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.type == \"PushEvent\")", &input);
    assert_ndjson_jq_compat("select(.type == \"nonexistent\")", &input);
    assert_ndjson_jq_compat("select(.name == \"alice\")", &input);
}

#[test]
fn golden_ndjson_select_eq_int() {
    let mut rng = Rng::new(3001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.count == 42)", &input);
    assert_ndjson_jq_compat("select(.count == 0)", &input);
    assert_ndjson_jq_compat("select(.count == -1)", &input);
}

#[test]
fn golden_ndjson_select_eq_bool_null() {
    let mut rng = Rng::new(4001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.active == true)", &input);
    assert_ndjson_jq_compat("select(.active == false)", &input);
    assert_ndjson_jq_compat("select(.type == null)", &input);
    assert_ndjson_jq_compat("select(.active == null)", &input);
}

#[test]
fn golden_ndjson_select_ne() {
    let mut rng = Rng::new(5001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.type != \"PushEvent\")", &input);
    assert_ndjson_jq_compat("select(.count != 0)", &input);
}

#[test]
fn golden_ndjson_select_ordering() {
    let mut rng = Rng::new(6001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.count > 10)", &input);
    assert_ndjson_jq_compat("select(.count < 50)", &input);
    assert_ndjson_jq_compat("select(.count >= 0)", &input);
    assert_ndjson_jq_compat("select(.count <= -1)", &input);
    assert_ndjson_jq_compat("select(.name > \"charlie\")", &input);
    assert_ndjson_jq_compat("select(.name < \"bob\")", &input);
}

#[test]
fn golden_ndjson_select_eq_field() {
    let mut rng = Rng::new(7001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.type == \"PushEvent\") | .name", &input);
    assert_ndjson_jq_compat("select(.type == \"PushEvent\") | .actor.login", &input);
    assert_ndjson_jq_compat("select(.count > 10) | .name", &input);
    assert_ndjson_jq_compat("select(.active == true) | .count", &input);
}

#[test]
fn golden_ndjson_select_eq_obj() {
    let mut rng = Rng::new(8001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat(
        "select(.type == \"PushEvent\") | {name: .name, count: .count}",
        &input,
    );
    assert_ndjson_jq_compat(
        "select(.count > 0) | {type: .type, login: .actor.login}",
        &input,
    );
}

#[test]
fn golden_ndjson_select_eq_arr() {
    let mut rng = Rng::new(9001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("select(.type == \"PushEvent\") | [.name, .count]", &input);
    assert_ndjson_jq_compat("select(.active == true) | [.type, .name]", &input);
}

#[test]
fn golden_ndjson_multi_field_obj() {
    let mut rng = Rng::new(10001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("{name: .name, count: .count}", &input);
    assert_ndjson_jq_compat("{type: .type, login: .actor.login}", &input);
}

#[test]
fn golden_ndjson_multi_field_arr() {
    let mut rng = Rng::new(11001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("[.name, .count]", &input);
    assert_ndjson_jq_compat("[.type, .actor.login, .active]", &input);
}

#[test]
fn golden_ndjson_length_keys() {
    let mut rng = Rng::new(12001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat("length", &input);
    assert_ndjson_jq_compat("keys", &input);
    assert_ndjson_jq_compat(".meta | length", &input);
    assert_ndjson_jq_compat(".meta | keys", &input);
    assert_ndjson_jq_compat(".items | length", &input);
}

#[test]
fn golden_ndjson_select_string_pred() {
    let mut rng = Rng::new(13001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat(r#"select(.name | test("^a"))"#, &input);
    assert_ndjson_jq_compat(r#"select(.name | startswith("al"))"#, &input);
    assert_ndjson_jq_compat(r#"select(.name | endswith("ce"))"#, &input);
    assert_ndjson_jq_compat(r#"select(.name | contains("ob"))"#, &input);
}

#[test]
fn golden_ndjson_select_string_pred_field() {
    let mut rng = Rng::new(14001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_ndjson_jq_compat(r#"select(.name | contains("alice")) | .count"#, &input);
    assert_ndjson_jq_compat(r#"select(.name | test("^b")) | .type"#, &input);
}

/// More than 1 MB of NDJSON, so the parallel engine splits it into several
/// jobs across worker threads.
#[test]
fn golden_ndjson_parallel_large_input() {
    let mut rng = Rng::new(99001);
    // ~1.5MB of NDJSON (enough to trigger >1 chunk).
    let input = generate_diverse_ndjson(&mut rng, 20000);
    assert!(
        input.len() > 1_000_000,
        "Input should be >1MB to spread over several jobs, got {} bytes",
        input.len()
    );

    // Several of the old fast-path shapes on large parallel input.
    assert_ndjson_jq_compat(".name", &input);
    assert_ndjson_jq_compat("select(.type == \"PushEvent\")", &input);
    assert_ndjson_jq_compat("select(.count > 50)", &input);
    assert_ndjson_jq_compat("{name: .name, count: .count}", &input);
    assert_ndjson_jq_compat("[.type, .name]", &input);
    assert_ndjson_jq_compat("length", &input);
    assert_ndjson_jq_compat("keys", &input);
    assert_ndjson_jq_compat(r#"select(.name | contains("alice"))"#, &input);
    assert_ndjson_jq_compat("select(.type == \"PushEvent\") | .name", &input);
    assert_ndjson_jq_compat(
        "select(.type == \"PushEvent\") | {name: .name, count: .count}",
        &input,
    );
}

/// Number literals (scientific notation, trailing zeros, etc.) print as jq
/// prints them, through each shape.
#[test]
fn golden_ndjson_number_preservation() {
    let input = r#"{"n":1.5e10,"s":"x"}
{"n":1e2,"s":"y"}
{"n":42.00,"s":"z"}
{"n":-3.14,"s":"w"}
{"n":0,"s":"v"}
"#;
    assert_ndjson_jq_compat(".n", input);
    assert_ndjson_jq_compat("select(.s == \"x\") | .n", input);
    assert_ndjson_jq_compat("{n: .n, s: .s}", input);
    assert_ndjson_jq_compat("[.n, .s]", input);
    assert_ndjson_jq_compat("select(.n == 0) | .s", input);
}

// --- Every shape the old core had an NDJSON fast path for ---

/// One filter per variant of the old core's `NdjsonFastPath` (and per
/// comparison operator, type and string predicate it special-cased), run
/// against jq.
const OLD_FAST_PATH_SHAPES: &[&str] = &[
    // FieldChain
    ".name",
    ".actor.login",
    // SelectEq (various types + ops)
    "select(.type == \"PushEvent\")",
    "select(.count == 42)",
    "select(.active == true)",
    "select(.value == null)",
    "select(.type != \"PushEvent\")",
    "select(.count > 10)",
    "select(.count < 100)",
    "select(.count >= 50)",
    "select(.count <= 50)",
    "select(.name > \"m\")",
    // Length
    "length",
    ".meta | length",
    // Keys (sorted)
    "keys",
    ".meta | keys",
    // Keys (unsorted)
    "keys_unsorted",
    ".meta | keys_unsorted",
    // Type
    "type",
    ".meta | type",
    // Has
    "has(\"name\")",
    ".meta | has(\"x\")",
    // SelectEqField
    "select(.type == \"PushEvent\") | .name",
    "select(.count > 10) | .name",
    // MultiFieldObj
    "{name: .name, count: .count}",
    "{type: .type, login: .actor.login}",
    // MultiFieldArr
    "[.name, .count]",
    "[.type, .actor.login]",
    // SelectEqObj
    "select(.type == \"PushEvent\") | {name: .name, count: .count}",
    // SelectEqArr
    "select(.type == \"PushEvent\") | [.name, .count]",
    // SelectCompound (AND / OR)
    "select(.type == \"PushEvent\" and .active == true)",
    "select(.type == \"PushEvent\" or .type == \"CreateEvent\")",
    "select(.count > 10 and .active == true)",
    "select(.type != \"PushEvent\" or .count < 100)",
    // SelectStringPred
    "select(.name | test(\"^A\"))",
    "select(.name | startswith(\"test\"))",
    "select(.name | endswith(\".com\"))",
    "select(.name | contains(\"oo\"))",
    // SelectStringPredField
    "select(.name | contains(\"oo\")) | .count",
];

#[test]
fn ndjson_old_fast_path_shapes_vs_jq() {
    // Diverse NDJSON that exercises object keys, arrays, nested fields,
    // string values, numbers, booleans, and nulls.
    let input = r#"{"name":"alice","type":"PushEvent","count":42,"active":true,"value":null,"actor":{"login":"alice"},"meta":{"x":1,"y":2},"items":[1,2,3]}
{"name":"bob.com","type":"WatchEvent","count":7,"active":false,"value":99,"actor":{"login":"bob"},"meta":{"a":10,"b":20,"c":30},"items":[]}
{"name":"test_foo","type":"PushEvent","count":100,"active":true,"value":null,"actor":{"login":"charlie"},"meta":{"x":5},"items":[10,20]}
{"name":"Aardvark","type":"CreateEvent","count":0,"active":false,"value":"hello","actor":{"login":"dave"},"meta":{"z":9,"a":1},"items":[42]}
"#;

    for filter in OLD_FAST_PATH_SHAPES {
        assert_ndjson_jq_compat(filter, input);
    }
}

// --- Leading whitespace handling ---

/// NDJSON lines with leading whitespace should produce the same output as
/// lines without.
#[test]
fn ndjson_leading_whitespace_trimmed() {
    // Input with leading spaces and tabs before the JSON objects.
    let input_with_ws = "  {\"name\":\"alice\",\"type\":\"PushEvent\"}\n\t{\"name\":\"bob\",\"type\":\"WatchEvent\"}\n";
    let input_clean =
        "{\"name\":\"alice\",\"type\":\"PushEvent\"}\n{\"name\":\"bob\",\"type\":\"WatchEvent\"}\n";

    // Both should produce identical output for each filter.
    for filter in &[
        ".name",
        "select(.type == \"PushEvent\")",
        "{name: .name}",
        "[.name]",
        "length",
        "keys",
        "type",
    ] {
        let out_ws = qj_stdin(&["-c", filter], input_with_ws);
        let out_clean = qj_stdin(&["-c", filter], input_clean);
        assert_eq!(
            out_ws, out_clean,
            "Leading whitespace caused different output for filter: {filter}"
        );
    }
}

/// A selected line is re-serialized: its leading whitespace is gone.
#[test]
fn ndjson_select_no_leading_whitespace_in_output() {
    let input = "  {\"type\":\"PushEvent\"}\n";
    let out = qj_stdin(&["-c", "select(.type == \"PushEvent\")"], input);
    assert_eq!(out, "{\"type\":\"PushEvent\"}\n");
    assert!(
        !out.starts_with(' '),
        "Output should not have leading whitespace"
    );
}

// --- Key-order preservation ---

#[test]
fn ndjson_key_order_preserved() {
    // Each NDJSON line preserves its own key order through parallel processing
    let input = "{\"z\":1,\"a\":2}\n{\"b\":3,\"a\":4}\n{\"m\":5,\"c\":6,\"a\":7}\n";
    let out = qj_stdin(&["-c", "."], input);
    assert_eq!(
        out,
        "{\"z\":1,\"a\":2}\n{\"b\":3,\"a\":4}\n{\"m\":5,\"c\":6,\"a\":7}\n"
    );
}

// --- Residency of memory-mapped input ---

/// Peak resident size in bytes of `qj args` (output discarded), from
/// `wait4`; the process is killed after `timeout`.
fn peak_rss(args: &[&str], env: &[(&str, &str)], timeout: std::time::Duration) -> u64 {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    cmd.args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("failed to run qj");
    let pid = child.id() as libc::pid_t;
    let start = std::time::Instant::now();
    loop {
        let mut status = 0;
        // SAFETY: wait4 on our own child, with valid out-pointers.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut ru) };
        if r == pid {
            assert!(
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "qj {args:?}: status {status}"
            );
            // (Bytes on macOS, KB on Linux.)
            let unit = if cfg!(target_os = "macos") { 1 } else { 1024 };
            return ru.ru_maxrss as u64 * unit;
        }
        if start.elapsed() > timeout {
            // SAFETY: killing our own child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            panic!("qj {args:?} took more than {timeout:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A memory-mapped NDJSON file is released as it's read: peak RSS stays
/// near the window (16 MB here) whatever the file's size (128 MB), on the
/// tape, on the VM, with large output and sequentially; with
/// `QJ_NO_RELEASE=1` the whole file is resident.
/// `cargo test --release --test ndjson mapped_input_residency -- --ignored`
#[test]
#[ignore]
fn mapped_input_residency_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.ndjson");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let pad = "x".repeat(150);
        let mut i = 0u64;
        while f.get_ref().metadata().unwrap().len() < 128 << 20 {
            for _ in 0..10_000 {
                writeln!(
                    f,
                    r#"{{"id":{i},"type":"PushEvent","actor":{{"login":"user{i}"}},"s":"{pad}"}}"#
                )
                .unwrap();
                i += 1;
            }
            f.flush().unwrap();
        }
    }
    let p = path.to_str().unwrap();
    let timeout = std::time::Duration::from_secs(120);
    let window = [("QJ_WINDOW_SIZE", "16")];
    for args in [
        &["--threads", "4", ".actor.login", p][..],
        &["--threads", "4", "-c", ".", p],
        &[
            "--threads",
            "4",
            "-c",
            r#"select(.actor.login | test("9$")) | .id"#,
            p,
        ],
        &["--threads", "1", ".id", p],
    ] {
        let rss = peak_rss(args, &window, timeout);
        eprintln!("{args:?}: peak RSS {} MB", rss >> 20);
        assert!(rss < 64 << 20, "{args:?}: peak RSS {} MB", rss >> 20);
    }
    let rss = peak_rss(
        &["--threads", "4", ".actor.login", p],
        &[("QJ_WINDOW_SIZE", "16"), ("QJ_NO_RELEASE", "1")],
        timeout,
    );
    eprintln!("QJ_NO_RELEASE: peak RSS {} MB", rss >> 20);
    assert!(rss > 128 << 20, "QJ_NO_RELEASE: peak RSS {} MB", rss >> 20);
}
