/// Conformance gap tests — 9 jq.test cases that qj currently fails.
///
/// All remaining gaps are bignum/arbitrary-precision related. qj uses i64/f64
/// while jq uses arbitrary precision (have_decnum=true). These tests check
/// `have_decnum` conditionals that qj doesn't match either branch of, or
/// require exponents beyond f64 range.
///
/// See `docs/COMPATIBILITY.md` for full analysis and options.
///
///   cargo test --release conformance_gaps -- --include-ignored    # all gaps
///   cargo test --release gap_bignum -- --include-ignored          # bignum category
///
/// As each gap is fixed, remove the test (it will be covered by jq_conformance).
mod common;

/// Run qj with a filter and input, return stdout lines.
fn run_jx(filter: &str, input: &str) -> Vec<String> {
    run_jx_with_args(filter, input, &["-c", "--"])
}

/// Run qj with a filter, input, and extra args, return stdout lines.
fn run_jx_with_args(filter: &str, input: &str, args: &[&str]) -> Vec<String> {
    let qj = common::Tool {
        name: "qj".to_string(),
        path: env!("CARGO_BIN_EXE_qj").to_string(),
    };
    match common::run_tool(&qj, filter, input, args) {
        Some(output) => output
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        None => vec!["<qj failed to run>".to_string()],
    }
}

/// Check if qj output matches expected (JSON-aware comparison).
fn assert_gap(filter: &str, input: &str, expected: &[&str]) {
    let actual = run_jx(filter, input);
    let actual_refs: Vec<&str> = actual.iter().map(|s| s.as_str()).collect();
    assert!(
        common::json_lines_equal(&actual_refs, expected),
        "filter: {}\ninput: {}\nexpected: {:?}\nactual:   {:?}",
        filter,
        input,
        expected,
        actual
    );
}

// ======================================================================
// Category: Big number / arbitrary precision (have_decnum)
// 9 test(s)
//
// qj uses i64/f64; jq with decnum uses arbitrary precision. These tests
// check `have_decnum` conditionals. qj's i64 is *more accurate* than
// jq-without-decnum for integers in the i64 range, but doesn't match
// either branch of the conditional. See docs/COMPATIBILITY.md.
// ======================================================================

/// jq.test line 661: extreme exponents that overflow/underflow f64
/// jq handles via decnum; qj: infinity/0
#[test]
#[ignore]
fn gap_bignum_line661_extreme_exponents() {
    assert_gap(
        "9E999999999, 9999999999E999999990, 1E-999999999, 0.000000001E-999999990",
        "null",
        &[
            "9E+999999999",
            "9.999999999E+999999999",
            "1E-999999999",
            "1E-999999999",
        ],
    );
}

/// jq.test line 2154: tostring on large int — qj preserves i64, test expects f64 loss
#[test]
#[ignore]
fn gap_bignum_line2154_tostring_large_int() {
    assert_gap(
        ".[0] | tostring | . == if have_decnum then \"13911860366432393\" else \"13911860366432392\" end",
        "[13911860366432393]",
        &["true"],
    );
}

/// jq.test line 2158: tojson on large int — same precision mismatch
#[test]
#[ignore]
fn gap_bignum_line2158_tojson_large_int() {
    assert_gap(
        ".x | tojson | . == if have_decnum then \"13911860366432393\" else \"13911860366432392\" end",
        "{\"x\":13911860366432393}",
        &["true"],
    );
}

/// jq.test line 2162: equality of adjacent large ints — qj: false (correct), test expects true (f64)
#[test]
#[ignore]
fn gap_bignum_line2162_large_int_equality() {
    assert_gap(
        "(13911860366432393 == 13911860366432392) | . == if have_decnum then false else true end",
        "null",
        &["true"],
    );
}

/// jq.test line 2169: subtraction on large int — qj: 383 (correct i64), test expects 382 (f64)
#[test]
#[ignore]
fn gap_bignum_line2169_large_int_subtract() {
    assert_gap(". - 10", "13911860366432393", &["13911860366432382"]);
}

/// jq.test line 2173: array element subtraction — same precision mismatch
#[test]
#[ignore]
fn gap_bignum_line2173_array_large_int_subtract() {
    assert_gap(".[0] - 10", "[13911860366432393]", &["13911860366432382"]);
}

/// jq.test line 2177: object field subtraction — same precision mismatch
#[test]
#[ignore]
fn gap_bignum_line2177_object_large_int_subtract() {
    assert_gap(
        ".x - 10",
        "{\"x\":13911860366432393}",
        &["13911860366432382"],
    );
}

/// jq.test line 2182: negation + tojson — same precision mismatch
#[test]
#[ignore]
fn gap_bignum_line2182_negate_large_int() {
    assert_gap(
        "-. | tojson == if have_decnum then \"-13911860366432393\" else \"-13911860366432392\" end",
        "13911860366432393",
        &["true"],
    );
}

/// jq.test line 2199: multiple large ints with addition — precision mismatches
#[test]
#[ignore]
fn gap_bignum_line2199_large_int_array_add() {
    assert_gap(
        ".[] as $n | $n+0 | [., tostring, . == $n]",
        "[-9007199254740993, -9007199254740992, 9007199254740992, 9007199254740993, 13911860366432393]",
        &[
            "[-9007199254740992,\"-9007199254740992\",true]",
            "[-9007199254740992,\"-9007199254740992\",true]",
            "[9007199254740992,\"9007199254740992\",true]",
            "[9007199254740992,\"9007199254740992\",true]",
            "[13911860366432392,\"13911860366432392\",true]",
        ],
    );
}
