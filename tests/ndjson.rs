/// Integration tests for NDJSON (newline-delimited JSON) processing.
use std::io::Write;
use std::process::Command;

fn qj_stdin(args: &[&str], input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(args)
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

/// Run the same filter with fast path enabled (default) and disabled (QJ_NO_FAST_PATH=1),
/// and assert that they produce identical output.
fn assert_fast_path_matches_normal(filter: &str, input: &str) {
    // Fast path enabled (default)
    let fast = {
        let output = Command::new(env!("CARGO_BIN_EXE_qj"))
            .args(["-c", filter])
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
            .expect("failed to run qj");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };

    // Fast path disabled
    let normal = {
        let output = Command::new(env!("CARGO_BIN_EXE_qj"))
            .args(["-c", filter])
            .env("QJ_NO_FAST_PATH", "1")
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
            .expect("failed to run qj");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };

    assert_eq!(
        fast, normal,
        "Fast path output differs from normal path for filter: {filter}"
    );
}

// --- Fast path vs normal path comparison tests ---

#[test]
fn fast_vs_normal_field_chain() {
    let input = "{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n";
    assert_fast_path_matches_normal(".name", input);
}

#[test]
fn fast_vs_normal_nested_field() {
    let input = "{\"a\":{\"b\":\"deep\"}}\n{\"a\":{\"b\":\"val\"}}\n";
    assert_fast_path_matches_normal(".a.b", input);
}

#[test]
fn fast_vs_normal_select_eq() {
    let input = "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
    assert_fast_path_matches_normal("select(.type == \"PushEvent\")", input);
}

#[test]
fn fast_vs_normal_select_ne() {
    let input = "{\"type\":\"PushEvent\",\"id\":1}\n{\"type\":\"WatchEvent\",\"id\":2}\n";
    assert_fast_path_matches_normal("select(.type != \"PushEvent\")", input);
}

#[test]
fn fast_vs_normal_select_gt() {
    let input = "{\"n\":5}\n{\"n\":15}\n{\"n\":10}\n";
    assert_fast_path_matches_normal("select(.n > 10)", input);
}

#[test]
fn fast_vs_normal_select_le() {
    let input = "{\"n\":5}\n{\"n\":15}\n{\"n\":10}\n";
    assert_fast_path_matches_normal("select(.n <= 10)", input);
}

#[test]
fn fast_vs_normal_select_eq_extract() {
    let input = "{\"type\":\"A\",\"x\":1}\n{\"type\":\"B\",\"x\":2}\n";
    assert_fast_path_matches_normal("select(.type == \"A\") | .x", input);
}

#[test]
fn fast_vs_normal_select_eq_obj() {
    let input = "{\"type\":\"A\",\"x\":1,\"y\":2}\n{\"type\":\"B\",\"x\":3,\"y\":4}\n";
    assert_fast_path_matches_normal("select(.type == \"A\") | {x: .x, y: .y}", input);
}

#[test]
fn fast_vs_normal_select_eq_arr() {
    let input = "{\"type\":\"A\",\"x\":1,\"y\":2}\n{\"type\":\"B\",\"x\":3,\"y\":4}\n";
    assert_fast_path_matches_normal("select(.type == \"A\") | [.x, .y]", input);
}

#[test]
fn fast_vs_normal_multi_field_obj() {
    let input = "{\"a\":1,\"b\":2,\"c\":3}\n{\"a\":4,\"b\":5,\"c\":6}\n";
    assert_fast_path_matches_normal("{a: .a, b: .b}", input);
}

#[test]
fn fast_vs_normal_multi_field_arr() {
    let input = "{\"a\":1,\"b\":2}\n{\"a\":3,\"b\":4}\n";
    assert_fast_path_matches_normal("[.a, .b]", input);
}

#[test]
fn fast_vs_normal_length() {
    let input = "{\"a\":1,\"b\":2}\n{\"x\":1}\n";
    assert_fast_path_matches_normal("length", input);
}

#[test]
fn fast_vs_normal_field_length() {
    let input = "{\"items\":[1,2,3]}\n{\"items\":[4]}\n";
    assert_fast_path_matches_normal(".items | length", input);
}

#[test]
fn fast_vs_normal_keys() {
    let input = "{\"b\":2,\"a\":1}\n{\"x\":1}\n";
    assert_fast_path_matches_normal("keys", input);
}

#[test]
fn fast_vs_normal_select_test() {
    let input = "{\"msg\":\"error: disk full\"}\n{\"msg\":\"ok\"}\n{\"msg\":\"error: timeout\"}\n";
    assert_fast_path_matches_normal(r#"select(.msg | test("error"))"#, input);
}

#[test]
fn fast_vs_normal_select_startswith() {
    let input = "{\"url\":\"/api/users\"}\n{\"url\":\"/web/home\"}\n";
    assert_fast_path_matches_normal(r#"select(.url | startswith("/api"))"#, input);
}

#[test]
fn fast_vs_normal_select_endswith() {
    let input = "{\"file\":\"data.json\"}\n{\"file\":\"data.csv\"}\n";
    assert_fast_path_matches_normal(r#"select(.file | endswith(".json"))"#, input);
}

#[test]
fn fast_vs_normal_select_contains() {
    let input = "{\"desc\":\"hello alice\"}\n{\"desc\":\"hello bob\"}\n";
    assert_fast_path_matches_normal(r#"select(.desc | contains("alice"))"#, input);
}

#[test]
fn fast_vs_normal_select_test_extract() {
    let input = "{\"msg\":\"error: disk full\",\"code\":500}\n{\"msg\":\"ok\",\"code\":200}\n";
    assert_fast_path_matches_normal(r#"select(.msg | test("error")) | .code"#, input);
}

#[test]
fn fast_vs_normal_select_float_vs_int() {
    // Edge case: 1.0 == 1 should match in both paths
    let input = "{\"n\":1.0,\"id\":\"a\"}\n{\"n\":2,\"id\":\"b\"}\n";
    assert_fast_path_matches_normal("select(.n == 1)", input);
}

#[test]
fn fast_vs_normal_select_escaped_string() {
    // Escaped strings (\n) are handled correctly by both paths
    let input = "{\"s\":\"line1\\nline2\",\"id\":1}\n{\"s\":\"other\",\"id\":2}\n";
    assert_fast_path_matches_normal("select(.s == \"line1\\nline2\")", input);
}

// Note: \u0041 vs "A" intentionally differs between fast/normal paths.
// Fast path outputs the raw line (preserving \u0041), normal path re-serializes
// (normalizing to "A"). Both are semantically correct. The fast path falls back
// to normal eval for the predicate comparison (correctly matching \u0041 == A),
// but when outputting the matched line, it emits the original raw bytes.

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
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
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
    assert!(stdout.contains("{\"a\":1}"), "first valid doc should appear in output");
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

// --- FieldChain fast path edge cases ---

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

// --- select fast path ---

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

// --- length/keys fast path ---

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

// --- select fallback correctness (byte mismatch but values equal) ---

#[test]
fn ndjson_select_float_vs_int() {
    // 1.0 == 1 should match (fallback to full eval)
    let input = r#"{"n":1.0,"id":"a"}
{"n":2,"id":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.n == 1)"], input);
    assert_eq!(out, "{\"n\":1.0,\"id\":\"a\"}\n");
}

#[test]
fn ndjson_select_scientific_notation() {
    // 1e2 == 100 should match
    let input = r#"{"n":1e2,"id":"a"}
{"n":99,"id":"b"}
"#;
    let out = qj_stdin(&["-c", "select(.n == 100)"], input);
    assert_eq!(out, "{\"n\":1e2,\"id\":\"a\"}\n");
}

#[test]
fn ndjson_select_unicode_escape() {
    // \u0041 is "A" — should match. Fast path extracts raw "\u0041" bytes,
    // which don't byte-match "A", so falls back to normal eval which
    // normalizes the unicode escape. Output matches QJ_NO_FAST_PATH behavior.
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
    // Mix of lines that hit fast path and fallback
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
    // String length requires fallback from C++ fast path to normal eval
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

// --- select + field extraction fast path ---

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
// Golden differential tests: fast path vs normal path with diverse inputs
//
// These tests generate NDJSON with edge cases (type mismatches, missing fields,
// unicode, escapes, numeric edge cases) and assert fast == normal for every
// supported fast-path variant.
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
            // Numeric edge cases: floats, scientific notation, negative, trailing zeros.
            // The On-Demand raw_json() path preserves these exactly as written.
            6 => {
                // Note: -0 intentionally excluded — fast path emits the raw line
                // (preserving "-0"), normal path normalizes to "0" through Value.
                // Both are semantically correct (IEEE 754: -0.0 == 0.0).
                let vals = &["1.0", "1e2", "0.0001", "42.00", "1.5e10", "-3.14"];
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
fn golden_fast_path_field_chain() {
    let mut rng = Rng::new(1001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal(".name", &input);
    assert_fast_path_matches_normal(".actor.login", &input);
    assert_fast_path_matches_normal(".missing", &input);
    assert_fast_path_matches_normal(".meta.x", &input);
}

#[test]
fn golden_fast_path_select_eq_string() {
    let mut rng = Rng::new(2001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.type == \"PushEvent\")", &input);
    assert_fast_path_matches_normal("select(.type == \"nonexistent\")", &input);
    assert_fast_path_matches_normal("select(.name == \"alice\")", &input);
}

#[test]
fn golden_fast_path_select_eq_int() {
    let mut rng = Rng::new(3001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.count == 42)", &input);
    assert_fast_path_matches_normal("select(.count == 0)", &input);
    assert_fast_path_matches_normal("select(.count == -1)", &input);
}

#[test]
fn golden_fast_path_select_eq_bool_null() {
    let mut rng = Rng::new(4001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.active == true)", &input);
    assert_fast_path_matches_normal("select(.active == false)", &input);
    assert_fast_path_matches_normal("select(.type == null)", &input);
    assert_fast_path_matches_normal("select(.active == null)", &input);
}

#[test]
fn golden_fast_path_select_ne() {
    let mut rng = Rng::new(5001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.type != \"PushEvent\")", &input);
    assert_fast_path_matches_normal("select(.count != 0)", &input);
}

#[test]
fn golden_fast_path_select_ordering() {
    let mut rng = Rng::new(6001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.count > 10)", &input);
    assert_fast_path_matches_normal("select(.count < 50)", &input);
    assert_fast_path_matches_normal("select(.count >= 0)", &input);
    assert_fast_path_matches_normal("select(.count <= -1)", &input);
    assert_fast_path_matches_normal("select(.name > \"charlie\")", &input);
    assert_fast_path_matches_normal("select(.name < \"bob\")", &input);
}

#[test]
fn golden_fast_path_select_eq_field() {
    let mut rng = Rng::new(7001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.type == \"PushEvent\") | .name", &input);
    assert_fast_path_matches_normal("select(.type == \"PushEvent\") | .actor.login", &input);
    assert_fast_path_matches_normal("select(.count > 10) | .name", &input);
    assert_fast_path_matches_normal("select(.active == true) | .count", &input);
}

#[test]
fn golden_fast_path_select_eq_obj() {
    let mut rng = Rng::new(8001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal(
        "select(.type == \"PushEvent\") | {name: .name, count: .count}",
        &input,
    );
    assert_fast_path_matches_normal(
        "select(.count > 0) | {type: .type, login: .actor.login}",
        &input,
    );
}

#[test]
fn golden_fast_path_select_eq_arr() {
    let mut rng = Rng::new(9001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("select(.type == \"PushEvent\") | [.name, .count]", &input);
    assert_fast_path_matches_normal("select(.active == true) | [.type, .name]", &input);
}

#[test]
fn golden_fast_path_multi_field_obj() {
    let mut rng = Rng::new(10001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("{name: .name, count: .count}", &input);
    assert_fast_path_matches_normal("{type: .type, login: .actor.login}", &input);
}

#[test]
fn golden_fast_path_multi_field_arr() {
    let mut rng = Rng::new(11001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("[.name, .count]", &input);
    assert_fast_path_matches_normal("[.type, .actor.login, .active]", &input);
}

#[test]
fn golden_fast_path_length_keys() {
    let mut rng = Rng::new(12001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal("length", &input);
    assert_fast_path_matches_normal("keys", &input);
    assert_fast_path_matches_normal(".meta | length", &input);
    assert_fast_path_matches_normal(".meta | keys", &input);
    assert_fast_path_matches_normal(".items | length", &input);
}

#[test]
fn golden_fast_path_select_string_pred() {
    let mut rng = Rng::new(13001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal(r#"select(.name | test("^a"))"#, &input);
    assert_fast_path_matches_normal(r#"select(.name | startswith("al"))"#, &input);
    assert_fast_path_matches_normal(r#"select(.name | endswith("ce"))"#, &input);
    assert_fast_path_matches_normal(r#"select(.name | contains("ob"))"#, &input);
}

#[test]
fn golden_fast_path_select_string_pred_field() {
    let mut rng = Rng::new(14001);
    let input = generate_diverse_ndjson(&mut rng, 100);
    assert_fast_path_matches_normal(r#"select(.name | contains("alice")) | .count"#, &input);
    assert_fast_path_matches_normal(r#"select(.name | test("^b")) | .type"#, &input);
}

/// Test with >1MB of NDJSON to trigger parallel chunk splitting and rayon,
/// exercising the SharedFilter unsafe Send+Sync wrapper.
#[test]
fn golden_fast_path_parallel_large_input() {
    let mut rng = Rng::new(99001);
    // ~1.5MB of NDJSON (enough to trigger >1 chunk).
    let input = generate_diverse_ndjson(&mut rng, 20000);
    assert!(
        input.len() > 1_000_000,
        "Input should be >1MB to trigger parallel processing, got {} bytes",
        input.len()
    );

    // Test several fast-path variants with large parallel input.
    assert_fast_path_matches_normal(".name", &input);
    assert_fast_path_matches_normal("select(.type == \"PushEvent\")", &input);
    assert_fast_path_matches_normal("select(.count > 50)", &input);
    assert_fast_path_matches_normal("{name: .name, count: .count}", &input);
    assert_fast_path_matches_normal("[.type, .name]", &input);
    assert_fast_path_matches_normal("length", &input);
    assert_fast_path_matches_normal("keys", &input);
    assert_fast_path_matches_normal(r#"select(.name | contains("alice"))"#, &input);
    assert_fast_path_matches_normal("select(.type == \"PushEvent\") | .name", &input);
    assert_fast_path_matches_normal(
        "select(.type == \"PushEvent\") | {name: .name, count: .count}",
        &input,
    );
}

/// Verify that the fast path preserves the original number representation
/// (scientific notation, trailing zeros, etc.) identically to the normal path.
#[test]
fn golden_fast_path_number_preservation() {
    let input = r#"{"n":1.5e10,"s":"x"}
{"n":1e2,"s":"y"}
{"n":42.00,"s":"z"}
{"n":-3.14,"s":"w"}
{"n":0,"s":"v"}
"#;
    assert_fast_path_matches_normal(".n", input);
    assert_fast_path_matches_normal("select(.s == \"x\") | .n", input);
    assert_fast_path_matches_normal("{n: .n, s: .s}", input);
    assert_fast_path_matches_normal("[.n, .s]", input);
    assert_fast_path_matches_normal("select(.n == 0) | .s", input);
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
