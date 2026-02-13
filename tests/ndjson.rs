/// Integration tests for NDJSON (newline-delimited JSON) processing.
use std::io::Write;
use std::process::Command;

fn jx_stdin(args: &[&str], input: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jx"))
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

fn jx_file(args: &[&str], content: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.jsonl");
    std::fs::write(&path, content).unwrap();

    let full_args: Vec<&str> = args.to_vec();
    let path_str = path.to_str().unwrap().to_string();
    // We need to own the string for the lifetime
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_jx"));
    for arg in &full_args {
        cmd.arg(arg);
    }
    cmd.arg(&path_str);

    let output = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("failed to run jx");

    assert!(
        output.status.success(),
        "jx {:?} {} exited with {}: stderr={}",
        args,
        path_str,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("jx output was not valid UTF-8")
}

// --- Basic NDJSON processing ---

#[test]
fn ndjson_field_extraction() {
    let input = r#"{"name":"alice","age":30}
{"name":"bob","age":25}
{"name":"charlie","age":35}
"#;
    let out = jx_stdin(&["-c", ".name"], input);
    assert_eq!(out, "\"alice\"\n\"bob\"\n\"charlie\"\n");
}

#[test]
fn ndjson_identity() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = jx_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_complex_filter() {
    let input = r#"{"name":"alice","score":90}
{"name":"bob","score":40}
{"name":"charlie","score":85}
"#;
    let out = jx_stdin(&["-c", "select(.score > 50) | .name"], input);
    assert_eq!(out, "\"alice\"\n\"charlie\"\n");
}

#[test]
fn ndjson_pipe_builtin() {
    let input = r#"{"items":[1,2,3]}
{"items":[4,5]}
"#;
    let out = jx_stdin(&["-c", ".items | length"], input);
    assert_eq!(out, "3\n2\n");
}

#[test]
fn ndjson_object_construct() {
    let input = r#"{"first":"alice","last":"smith"}
{"first":"bob","last":"jones"}
"#;
    let out = jx_stdin(&["-c", "{name: .first}"], input);
    assert_eq!(out, "{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n");
}

// --- Edge cases ---

#[test]
fn ndjson_empty_lines() {
    let input = "{\"a\":1}\n\n{\"b\":2}\n\n";
    let out = jx_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_trailing_newline() {
    let input = "{\"a\":1}\n{\"b\":2}\n";
    let out = jx_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_no_trailing_newline() {
    let input = "{\"a\":1}\n{\"b\":2}";
    let out = jx_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_single_line_not_detected() {
    // Single JSON object should NOT be treated as NDJSON
    let input = r#"{"a":1}"#;
    let out = jx_stdin(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n");
}

// --- --jsonl flag ---

#[test]
fn jsonl_flag_forces_ndjson() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = jx_stdin(&["--jsonl", "-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

// --- File input ---

#[test]
fn ndjson_file_field_extraction() {
    let input = r#"{"name":"alice"}
{"name":"bob"}
"#;
    let out = jx_file(&["-c", ".name"], input);
    assert_eq!(out, "\"alice\"\n\"bob\"\n");
}

#[test]
fn ndjson_file_identity() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = jx_file(&["-c", "."], input);
    assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
}

#[test]
fn ndjson_file_with_jsonl_flag() {
    let input = r#"{"x":1}
{"x":2}
"#;
    let out = jx_file(&["--jsonl", "-c", ".x"], input);
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
    let out = jx_stdin(&["-c", ".i"], &input);
    let expected: String = (0..100).map(|i| format!("{i}\n")).collect();
    assert_eq!(out, expected);
}

// --- Array NDJSON ---

#[test]
fn ndjson_arrays() {
    let input = "[1,2,3]\n[4,5,6]\n";
    let out = jx_stdin(&["-c", ".[0]"], input);
    assert_eq!(out, "1\n4\n");
}

// --- Raw output ---

#[test]
fn ndjson_raw_output() {
    let input = r#"{"name":"alice"}
{"name":"bob"}
"#;
    let out = jx_stdin(&["-r", ".name"], input);
    assert_eq!(out, "alice\nbob\n");
}

// --- Pretty output ---

#[test]
fn ndjson_pretty_output() {
    let input = r#"{"a":1}
{"b":2}
"#;
    let out = jx_stdin(&["."], input);
    assert_eq!(out, "{\n  \"a\": 1\n}\n{\n  \"b\": 2\n}\n");
}

// --- Iterate ---

#[test]
fn ndjson_iterate() {
    let input = r#"{"a":1,"b":2}
{"c":3}
"#;
    let out = jx_stdin(&["-c", ".[]"], input);
    assert_eq!(out, "1\n2\n3\n");
}
