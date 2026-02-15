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
    // Mix of valid and invalid JSON lines
    let input = "{\"a\":1}\nnot json\n{\"b\":2}\n";
    let (stdout, _stderr, _success) = qj_stdin_lossy(&["-c", "."], input);
    // Valid lines should still produce output
    assert!(stdout.contains("{\"a\":1}"));
    assert!(stdout.contains("{\"b\":2}"));
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
    // \u0041 is "A" — should match
    let input = "{\"s\":\"\\u0041\",\"id\":1}\n{\"s\":\"B\",\"id\":2}\n";
    let out = qj_stdin(&["-c", "select(.s == \"A\")"], &input);
    assert_eq!(out, "{\"s\":\"\\u0041\",\"id\":1}\n");
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

// --- Iterate ---

#[test]
fn ndjson_iterate() {
    let input = r#"{"a":1,"b":2}
{"c":3}
"#;
    let out = qj_stdin(&["-c", ".[]"], input);
    assert_eq!(out, "1\n2\n3\n");
}
