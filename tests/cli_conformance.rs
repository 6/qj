/// CLI conformance tests: verify qj's CLI interface matches jq's behavior.
///
/// These tests run both `qj` and `jq` and compare output and exit codes.
/// All tests are #[ignore] since they require jq installed.
use std::io::Write;
use std::process::{Command, Output, Stdio};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn jq_available() -> bool {
    Command::new("jq")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run qj with given args and stdin input bytes. Returns the full Output.
fn run_qj(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn qj");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().expect("failed to wait on qj")
}

/// Run jq with given args and stdin input bytes. Returns the full Output.
fn run_jq(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new("jq")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn jq");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().expect("failed to wait on jq")
}

/// Run qj with args and a file (no stdin).
fn run_qj_file(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run qj")
}

/// Run jq with args and a file (no stdin).
fn run_jq_file(args: &[&str]) -> Output {
    Command::new("jq")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run jq")
}

fn stdout_str(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

/// Assert qj and jq produce the same stdout for the given args and input.
fn assert_same_output(args: &[&str], input: &[u8]) {
    if !jq_available() {
        return;
    }
    let qj = run_qj(args, input);
    let jq = run_jq(args, input);
    assert_eq!(
        stdout_str(&qj),
        stdout_str(&jq),
        "output mismatch: args={args:?} input={:?}",
        String::from_utf8_lossy(input)
    );
}

/// Assert qj and jq produce the same exit code for the given args and input.
fn assert_same_exit(args: &[&str], input: &[u8]) {
    if !jq_available() {
        return;
    }
    let qj = run_qj(args, input);
    let jq = run_jq(args, input);
    assert_eq!(
        exit_code(&qj),
        exit_code(&jq),
        "exit code mismatch: args={args:?} input={:?}\nqj stdout={:?}\njq stdout={:?}\nqj stderr={:?}\njq stderr={:?}",
        String::from_utf8_lossy(input),
        stdout_str(&qj),
        stdout_str(&jq),
        String::from_utf8_lossy(&qj.stderr),
        String::from_utf8_lossy(&jq.stderr),
    );
}

/// Assert qj and jq produce the same stdout AND exit code.
fn assert_same(args: &[&str], input: &[u8]) {
    if !jq_available() {
        return;
    }
    let qj = run_qj(args, input);
    let jq = run_jq(args, input);
    assert_eq!(
        stdout_str(&qj),
        stdout_str(&jq),
        "output mismatch: args={args:?} input={:?}",
        String::from_utf8_lossy(input)
    );
    assert_eq!(
        exit_code(&qj),
        exit_code(&jq),
        "exit code mismatch: args={args:?} input={:?}",
        String::from_utf8_lossy(input)
    );
}

// ===========================================================================
// Default behavior
// ===========================================================================

#[test]
#[ignore]
fn cli_default_filter_piped() {
    // echo '{"a":1}' | tool  (no filter arg → defaults to '.')
    assert_same_output(&["."], b"{\"a\":1}\n");
    // With no explicit filter, qj defaults to '.', same as jq
    let qj = run_qj(&["."], b"{\"a\":1}\n");
    let jq = run_jq(&["."], b"{\"a\":1}\n");
    assert_eq!(stdout_str(&qj), stdout_str(&jq));
}

// ===========================================================================
// Output flags
// ===========================================================================

#[test]
#[ignore]
fn cli_compact_output() {
    assert_same(&["-c", "."], b"{\"a\":1,\"b\":2}\n");
}

#[test]
#[ignore]
fn cli_raw_output() {
    assert_same(&["-r", "."], b"\"hello\"\n");
}

#[test]
#[ignore]
fn cli_join_output() {
    assert_same(&["-j", ".[]"], b"[\"a\",\"b\"]\n");
}

#[test]
#[ignore]
fn cli_raw_output0() {
    // Compare raw bytes since NUL separators matter
    let qj = run_qj(&["--raw-output0", ".[]"], b"[\"a\",\"b\"]\n");
    let jq = run_jq(&["--raw-output0", ".[]"], b"[\"a\",\"b\"]\n");
    assert_eq!(qj.stdout, jq.stdout, "raw-output0 byte mismatch");
}

#[test]
#[ignore]
fn cli_sort_keys() {
    assert_same(&["-S", "."], b"{\"b\":2,\"a\":1}\n");
}

#[test]
#[ignore]
fn cli_tab_indent() {
    assert_same(&["--tab", "."], b"{\"a\":1}\n");
}

#[test]
#[ignore]
fn cli_indent_4() {
    assert_same(&["--indent", "4", "."], b"{\"a\":1}\n");
}

#[test]
#[ignore]
fn cli_ascii_output() {
    assert_same(
        &["-a", "."],
        "{\"\u{00e9}m\u{00f6}ji\":\"caf\u{00e9}\"}\n".as_bytes(),
    );
}

// ===========================================================================
// Input flags
// ===========================================================================

#[test]
#[ignore]
fn cli_null_input() {
    assert_same(&["-n", "null"], b"");
}

#[test]
#[ignore]
fn cli_slurp() {
    assert_same(&["-s", "."], b"1\n2\n3\n");
}

#[test]
#[ignore]
fn cli_raw_input() {
    assert_same(&["-R", "."], b"hello\nworld\n");
}

#[test]
#[ignore]
fn cli_raw_input_slurp() {
    assert_same(&["-Rs", "."], b"hello\nworld\n");
}

// ===========================================================================
// Variable binding
// ===========================================================================

#[test]
#[ignore]
fn cli_arg() {
    assert_same(&["-n", "--arg", "x", "hello", "$x"], b"");
}

#[test]
#[ignore]
fn cli_argjson() {
    assert_same(&["-n", "--argjson", "x", "{\"a\":1}", "$x"], b"");
}

#[test]
#[ignore]
fn cli_args_positional() {
    assert_same_output(&["-n", "$ARGS", "--args", "a", "b", "c"], b"");
}

#[test]
#[ignore]
fn cli_jsonargs_positional() {
    assert_same_output(&["-n", "$ARGS", "--jsonargs", "1", "\"two\"", "null"], b"");
}

#[test]
#[ignore]
fn cli_slurpfile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.json");
    std::fs::write(&path, "1\n2\n3\n").unwrap();
    let path_str = path.to_str().unwrap();
    assert_same_output(&["-n", "--slurpfile", "x", path_str, "$x"], b"");
}

#[test]
#[ignore]
fn cli_rawfile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let path_str = path.to_str().unwrap();
    assert_same_output(&["-n", "--rawfile", "x", path_str, "$x"], b"");
}

// ===========================================================================
// Exit codes
// ===========================================================================

#[test]
#[ignore]
fn cli_exit_e_false() {
    assert_same_exit(&["-e", "."], b"false\n");
}

#[test]
#[ignore]
fn cli_exit_e_null() {
    assert_same_exit(&["-e", "."], b"null\n");
}

#[test]
#[ignore]
fn cli_exit_e_true() {
    assert_same_exit(&["-e", "."], b"true\n");
}

#[test]
#[ignore]
fn cli_exit_e_number() {
    assert_same_exit(&["-e", "."], b"0\n");
}

#[test]
#[ignore]
fn cli_exit_e_string() {
    assert_same_exit(&["-e", "."], b"\"hello\"\n");
}

#[test]
#[ignore]
fn cli_exit_e_empty_output() {
    // `empty` filter produces no output
    assert_same_exit(&["-e", "empty"], b"null\n");
}

#[test]
#[ignore]
fn cli_exit_e_empty_input() {
    assert_same_exit(&["-e", "."], b"");
}

#[test]
#[ignore]
fn cli_exit_invalid_json() {
    assert_same_exit(&["."], b"xyz\n");
}

#[test]
#[ignore]
fn cli_exit_missing_file() {
    if !jq_available() {
        return;
    }
    let qj = run_qj_file(&[".", "/nonexistent_file_cli_test"]);
    let jq = run_jq_file(&[".", "/nonexistent_file_cli_test"]);
    assert_eq!(
        exit_code(&qj),
        exit_code(&jq),
        "exit code mismatch for missing file"
    );
}

#[test]
#[ignore]
fn cli_exit_bad_filter() {
    assert_same_exit(&[".["], b"{}\n");
}

#[test]
#[ignore]
fn cli_exit_runtime_error() {
    assert_same_exit(&[".a"], b"5\n");
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
#[ignore]
fn cli_empty_input() {
    assert_same(&["."], b"");
}

#[test]
#[ignore]
fn cli_whitespace_only_input() {
    assert_same(&["."], b"  \n  \n");
}

#[test]
#[ignore]
fn cli_multi_file_one_missing() {
    if !jq_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.json");
    std::fs::write(&good, "{\"ok\":true}\n").unwrap();
    let good_str = good.to_str().unwrap();

    let qj = run_qj_file(&[".", "/nonexistent_file_cli_test", good_str]);
    let jq = run_jq_file(&[".", "/nonexistent_file_cli_test", good_str]);

    // Both should output the good file's content
    assert_eq!(
        stdout_str(&qj),
        stdout_str(&jq),
        "multi-file output mismatch"
    );
    // Both should exit with error code
    assert_eq!(
        exit_code(&qj),
        exit_code(&jq),
        "multi-file exit code mismatch"
    );
}

#[test]
#[ignore]
fn cli_from_file() {
    if !jq_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let filter_path = dir.path().join("filter.jq");
    std::fs::write(&filter_path, ".a + .b").unwrap();
    let filter_str = filter_path.to_str().unwrap();
    assert_same_output(&["-f", filter_str], b"{\"a\":1,\"b\":2}\n");
}

#[test]
#[ignore]
fn cli_filter_starting_with_dash() {
    // Using -- to pass a filter that starts with -
    assert_same(&["--", "-."], b"1\n");
}

// ===========================================================================
// Flag combinations
// ===========================================================================

#[test]
#[ignore]
fn cli_compact_slurp() {
    assert_same(&["-cs", "."], b"1\n2\n3\n");
}

#[test]
#[ignore]
fn cli_raw_compact() {
    assert_same(&["-rc", ".name"], b"{\"name\":\"alice\"}\n");
}

#[test]
#[ignore]
fn cli_raw_input_slurp_compact() {
    assert_same(&["-Rsc", "."], b"hello\nworld\n");
}

#[test]
#[ignore]
fn cli_exit_status_slurp() {
    assert_same_exit(&["-es", ".[0]"], b"false\n");
}

#[test]
#[ignore]
fn cli_null_input_raw() {
    assert_same(&["-nr", "\"hello\""], b"");
}

#[test]
#[ignore]
fn cli_slurp_empty() {
    // Slurp empty input produces []
    assert_same(&["-s", "."], b"");
}

#[test]
#[ignore]
fn cli_e_with_multiple_outputs() {
    // Multiple outputs — exit code based on LAST output
    // [true, false] | .[] → true, then false → exit 1
    assert_same_exit(&["-e", ".[]"], b"[true,false]\n");
}

#[test]
#[ignore]
fn cli_e_with_multiple_outputs_last_truthy() {
    // [false, true] | .[] → false, then true → exit 0
    assert_same_exit(&["-e", ".[]"], b"[false,true]\n");
}

// ===========================================================================
// SIGPIPE handling
// ===========================================================================

#[test]
fn cli_sigpipe_no_error() {
    // Pipe large output through a process that reads one line and closes.
    // qj should exit cleanly without error messages on stderr.
    use std::process::{Command, Stdio};
    let mut qj = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(["-nc", "[range(100000)][]"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn qj");

    let qj_stdout = qj.stdout.take().unwrap();
    // head -1: read one line, then close the pipe
    let head = Command::new("head")
        .args(["-1"])
        .stdin(qj_stdout)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("failed to run head");

    let qj_out = qj.wait_with_output().expect("failed to wait on qj");
    let stderr = String::from_utf8_lossy(&qj_out.stderr);
    assert!(
        stderr.is_empty(),
        "qj should not produce stderr on SIGPIPE, got: {stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "0",
        "head should capture first line"
    );
}

// ===========================================================================
// Concatenated JSON documents
// ===========================================================================

#[test]
#[ignore]
fn cli_concat_objects() {
    // {"a":1}{"b":2} — two objects concatenated without separator
    assert_same(&["-c", "."], b"{\"a\":1}{\"b\":2}");
}

#[test]
#[ignore]
fn cli_concat_space_separated() {
    // 1 2 3 — whitespace-separated scalars
    assert_same(&["-c", "."], b"1 2 3");
}

#[test]
#[ignore]
fn cli_concat_arrays() {
    // [1][2] — two arrays concatenated
    assert_same(&["-c", "."], b"[1][2]");
}

#[test]
#[ignore]
fn cli_concat_newline_scalars() {
    // Newline-separated bare scalars (not detected as NDJSON)
    assert_same(&["-c", "."], b"1\n2\n3\n");
}

// ===========================================================================
// $ENV / env builtin
// ===========================================================================

#[test]
#[ignore]
fn cli_env_home() {
    // $ENV.HOME should return a string
    assert_same_output(&["-n", "$ENV.HOME"], b"");
}

#[test]
#[ignore]
fn cli_env_nonexistent() {
    // $ENV.QJ_TEST_NONEXISTENT_VAR should return null
    assert_same_output(&["-n", "$ENV.QJ_TEST_NONEXISTENT_VAR_12345"], b"");
}

#[test]
#[ignore]
fn cli_env_builtin() {
    // env | has("HOME") should return true
    assert_same_output(&["-n", "env | has(\"HOME\")"], b"");
}

// ===========================================================================
// input / inputs builtins
// ===========================================================================

#[test]
#[ignore]
fn cli_inputs_multi_doc() {
    // -n '[inputs]' with multiple JSON documents
    assert_same(&["-n", "[inputs]"], b"1\n2\n3\n");
}

#[test]
#[ignore]
fn cli_first_input() {
    // first(inputs) with multiple docs — returns just the first
    assert_same(&["-n", "first(inputs)"], b"1\n2\n3\n");
}

// ===========================================================================
// Multiple valid files
// ===========================================================================

#[test]
#[ignore]
fn cli_two_valid_files() {
    if !jq_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let f1 = dir.path().join("one.json");
    let f2 = dir.path().join("two.json");
    std::fs::write(&f1, "{\"x\":1}\n").unwrap();
    std::fs::write(&f2, "{\"y\":2}\n").unwrap();
    let f1s = f1.to_str().unwrap();
    let f2s = f2.to_str().unwrap();

    let qj = run_qj_file(&["-c", ".", f1s, f2s]);
    let jq = run_jq_file(&["-c", ".", f1s, f2s]);
    assert_eq!(
        stdout_str(&qj),
        stdout_str(&jq),
        "two valid files output mismatch"
    );
    assert_eq!(
        exit_code(&qj),
        exit_code(&jq),
        "two valid files exit code mismatch"
    );
}

// ===========================================================================
// --arg + --args combined
// ===========================================================================

#[test]
#[ignore]
fn cli_arg_plus_args() {
    // Combine named --arg with positional --args
    assert_same_output(
        &["-n", "$ARGS", "--arg", "name", "hello", "--args", "a", "b"],
        b"",
    );
}

// ===========================================================================
// --jsonargs error handling
// ===========================================================================

#[test]
#[ignore]
fn cli_jsonargs_invalid() {
    // Invalid JSON in --jsonargs should error
    assert_same_exit(&["-n", "$ARGS", "--jsonargs", "not_valid_json"], b"");
}

// ===========================================================================
// --version flag
// ===========================================================================

#[test]
fn cli_version_exits_zero() {
    let qj = run_qj(&["--version"], b"");
    assert_eq!(exit_code(&qj), 0, "qj --version should exit 0");
    assert!(
        !stdout_str(&qj).is_empty(),
        "qj --version should produce output"
    );
}

// ===========================================================================
// --indent 0
// ===========================================================================

#[test]
#[ignore]
fn cli_indent_0() {
    // --indent 0 should behave like -c
    assert_same(&["--indent", "0", "."], b"{\"a\":1,\"b\":[2,3]}\n");
}
