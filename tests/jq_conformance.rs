/// jq conformance test runner.
///
/// Parses the official jq test suite (`tests/jq_compat/jq.test` from jqlang/jq)
/// and runs each test case against qj. Reports pass/fail percentage.
///
/// This test always passes — it's a measurement tool, not a gate.
/// Run with `--nocapture` to see the summary:
///
///   cargo test jq_conformance -- --nocapture
///
/// To see each failing test case, run the ignored verbose test:
///
///   cargo test jq_conformance_verbose -- --nocapture --ignored
mod common;

extern crate serde_json;

struct TestCase {
    filter: String,
    input: String,
    expected: Vec<String>,
    line_no: usize,
}

enum TestResult {
    Pass,
    Fail {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    Error,
}

fn parse_jq_test_file(content: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Skip blank lines and comments
        if line.trim().is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }

        // Skip %%FAIL blocks — they test jq-specific error messages
        if line.starts_with("%%FAIL") {
            i += 1;
            // Skip until next blank line
            while i < lines.len() && !lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        }

        // This should be the filter line
        let filter_line = i + 1; // 1-indexed for display
        let filter = line.to_string();
        i += 1;

        // Next non-blank, non-comment line is input
        if i >= lines.len() {
            break;
        }
        let input = lines[i].to_string();
        i += 1;

        // Collect expected output lines until blank line or EOF
        let mut expected = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].starts_with('#') {
            expected.push(lines[i].to_string());
            i += 1;
        }

        if !expected.is_empty() {
            cases.push(TestCase {
                filter,
                input,
                expected,
                line_no: filter_line,
            });
        }
    }

    cases
}

fn needs_module_flag(filter: &str) -> bool {
    let trimmed = filter.trim_start();
    trimmed.starts_with("import ")
        || trimmed.starts_with("include ")
        || trimmed.starts_with("modulemeta")
}

fn run_test_case(case: &TestCase) -> TestResult {
    let qj = common::Tool {
        name: "qj".to_string(),
        path: env!("CARGO_BIN_EXE_qj").to_string(),
    };
    let modules_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/modules");
    let modules_str = modules_dir.to_str().unwrap();
    let extra_args: Vec<&str> = if needs_module_flag(&case.filter) {
        vec!["-c", "-L", modules_str, "--"]
    } else {
        vec!["-c", "--"]
    };
    match common::run_tool(&qj, &case.filter, &case.input, &extra_args) {
        Some(output) => {
            let actual_lines: Vec<&str> = output.lines().filter(|l| !l.is_empty()).collect();
            let expected_lines: Vec<&str> = case.expected.iter().map(|s| s.as_str()).collect();

            if common::json_lines_equal(&actual_lines, &expected_lines) {
                TestResult::Pass
            } else {
                TestResult::Fail {
                    expected: case.expected.clone(),
                    actual: actual_lines.into_iter().map(String::from).collect(),
                }
            }
        }
        None => TestResult::Error,
    }
}

fn run_all_cases() -> (Vec<TestCase>, Vec<TestResult>) {
    let test_file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/jq.test");
    let content = std::fs::read_to_string(&test_file).expect("failed to read jq.test");
    let cases = parse_jq_test_file(&content);
    let results: Vec<TestResult> = cases.iter().map(|c| run_test_case(c)).collect();
    (cases, results)
}

/// Run with: cargo test --release jq_conformance -- --nocapture --ignored
#[test]
#[ignore]
fn jq_conformance() {
    let (_cases, results) = run_all_cases();

    let mut passed = 0;
    let mut failed = 0;
    let mut errored = 0;

    for r in &results {
        match r {
            TestResult::Pass => passed += 1,
            TestResult::Fail { .. } => failed += 1,
            TestResult::Error => errored += 1,
        }
    }

    let total = passed + failed + errored;
    let pct = if total > 0 {
        passed as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    // Use eprintln so summary is visible even without --nocapture
    eprintln!();
    eprintln!("jq conformance: {passed}/{total} passed ({pct:.1}%)");
    eprintln!("  passed:  {passed}");
    eprintln!("  failed:  {failed}");
    eprintln!("  errored: {errored}");

    // Regression gate: conformance must not drop below this threshold.
    // Current baseline: 454/497 (91.3%). Set 3 below for small tolerance.
    assert!(
        passed >= 451,
        "jq conformance regression: {passed}/497 (was >= 451)"
    );
}

/// Run jq conformance cases through the NDJSON code path and compare against
/// single-doc output. This catches any filter where the NDJSON path diverges
/// from the normal eval path.
///
/// Each test case's input is duplicated to trigger NDJSON detection (which
/// requires 2+ lines starting with `{`/`[`), and the output is expected to
/// be the single-doc output repeated twice.
///
/// Cases are skipped when:
/// - Input doesn't start with `{`/`[` (won't trigger NDJSON detection)
/// - Filter uses `input`/`inputs` (stream-dependent, behaves differently in NDJSON)
/// - Filter uses `$__loc__` (reports source location, irrelevant)
///
/// Run with: cargo test --release jq_conformance_ndjson -- --nocapture --ignored
#[test]
#[ignore]
fn jq_conformance_ndjson() {
    use std::io::Write;
    use std::process::Command;

    let test_file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/jq.test");
    let content = std::fs::read_to_string(&test_file).expect("failed to read jq.test");
    let cases = parse_jq_test_file(&content);

    let qj_path = env!("CARGO_BIN_EXE_qj");

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut errored = 0;

    for case in &cases {
        let input_trimmed = case.input.trim();

        // Skip non-object/non-array inputs — they don't trigger NDJSON detection.
        if !input_trimmed.starts_with('{') && !input_trimmed.starts_with('[') {
            skipped += 1;
            continue;
        }

        // Skip stream-dependent filters.
        if case.filter.contains("input")
            || case.filter.contains("$__loc__")
            || case.filter.contains("debug")
            || case.filter.contains("stderr")
        {
            skipped += 1;
            continue;
        }

        // Skip inputs with non-standard JSON tokens (Infinity, NaN).
        // simdjson (used by the NDJSON path) strictly follows the JSON spec and
        // rejects these, while the single-doc path has special handling.
        if case.input.contains("Infinity")
            || case.input.contains("NaN")
            || case.input.contains("nan")
        {
            skipped += 1;
            continue;
        }

        // Run single-doc mode.
        let single_out = match common::run_tool(
            &common::Tool {
                name: "qj".to_string(),
                path: qj_path.to_string(),
            },
            &case.filter,
            &case.input,
            &["-c", "--"],
        ) {
            Some(o) => o,
            None => {
                errored += 1;
                continue;
            }
        };

        // Build NDJSON input: duplicate the line to trigger NDJSON detection.
        let ndjson_input = format!("{}\n{}\n", case.input, case.input);

        // Run NDJSON mode.
        let ndjson_output = Command::new(qj_path)
            .args(["-c", "--", &case.filter])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(ndjson_input.as_bytes())
                    .unwrap();
                child.wait_with_output()
            });

        let ndjson_out = match ndjson_output {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(_) => {
                errored += 1;
                continue;
            }
        };

        // Expected NDJSON output: single-doc output repeated twice.
        let expected_ndjson = if single_out.is_empty() {
            String::new()
        } else {
            let trimmed = single_out.trim_end_matches('\n');
            format!("{trimmed}\n{trimmed}\n")
        };

        if ndjson_out == expected_ndjson {
            passed += 1;
        } else {
            failed += 1;
            eprintln!("NDJSON DIVERGENCE (line {}): {}", case.line_no, case.filter);
            eprintln!("  input:          {}", case.input);
            eprintln!("  single-doc out: {:?}", single_out);
            eprintln!("  ndjson out:     {:?}", ndjson_out);
            eprintln!("  expected ndjson:{:?}", expected_ndjson);
        }
    }

    let tested = passed + failed + errored;
    let pct = if tested > 0 {
        passed as f64 / tested as f64 * 100.0
    } else {
        0.0
    };
    eprintln!();
    eprintln!("jq conformance (NDJSON mode): {passed}/{tested} passed ({pct:.1}%)");
    eprintln!("  passed:  {passed}");
    eprintln!("  failed:  {failed}");
    eprintln!("  errored: {errored}");
    eprintln!("  skipped: {skipped} (non-object input or stream-dependent filter)");
}

/// Run with: cargo test jq_conformance_verbose -- --nocapture --ignored
#[test]
#[ignore]
fn jq_conformance_verbose() {
    let (cases, results) = run_all_cases();

    let mut passed = 0;
    let mut failed = 0;
    let mut errored = 0;

    for (case, result) in cases.iter().zip(results.iter()) {
        match result {
            TestResult::Pass => passed += 1,
            TestResult::Fail { expected, actual } => {
                failed += 1;
                eprintln!(
                    "FAIL (line {}): {} | input: {}",
                    case.line_no, case.filter, case.input
                );
                eprintln!("  expected: {expected:?}");
                eprintln!("  actual:   {actual:?}");
            }
            TestResult::Error => {
                errored += 1;
                eprintln!(
                    "ERROR (line {}): {} | input: {}",
                    case.line_no, case.filter, case.input
                );
            }
        }
    }

    let total = passed + failed + errored;
    let pct = if total > 0 {
        passed as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    println!();
    println!("jq conformance: {passed}/{total} passed ({pct:.1}%)");
    println!("  passed:  {passed}");
    println!("  failed:  {failed}");
    println!("  errored: {errored}");
}
