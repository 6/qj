/// jq conformance test runner.
///
/// Parses the official jq test suite (`tests/jq_compat/jq.test` from jqlang/jq)
/// and runs each test case against jx. Reports pass/fail percentage.
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

fn run_test_case(case: &TestCase) -> TestResult {
    let jx = common::Tool {
        name: "jx".to_string(),
        path: env!("CARGO_BIN_EXE_jx").to_string(),
    };
    match common::run_tool(&jx, &case.filter, &case.input, &["-c", "--"]) {
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
