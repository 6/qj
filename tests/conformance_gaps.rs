/// Former conformance gaps: the 9 jq.test cases the old core failed unless
/// `QJ_JQ_COMPAT=1` was set.
///
/// All of them are about jq 1.8.1's number model: jq is built with decNumber,
/// so a number literal keeps its exact decimal value (for printing, tostring,
/// tojson, negation, and comparisons between literals), arithmetic is f64,
/// and `have_decnum` is true. qj's jq port follows that model by default, so
/// these pass with no environment at all. They stay as regression tests,
/// compared byte for byte with jq.test's expected lines (jq_diff runs the
/// same cases against the jq binary).
///
///   cargo test --release conformance_gaps -- --ignored    # all
///   cargo test --release gap_bignum -- --ignored          # bignum category
mod common;

/// Run qj with a filter and input, return stdout lines.
fn run_qj(filter: &str, input: &str) -> Vec<String> {
    let qj = common::Tool {
        name: "qj".to_string(),
        path: env!("CARGO_BIN_EXE_qj").to_string(),
    };
    match common::run_tool(&qj, filter, input, &["-c", "--"]) {
        Some(output) => output
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        None => vec!["<qj failed to run>".to_string()],
    }
}

/// Check qj's output lines against jq.test's expected lines, exactly: a
/// JSON-level comparison would take 13911860366432383 for 13911860366432382.
fn assert_gap(filter: &str, input: &str, expected: &[&str]) {
    let actual = run_qj(filter, input);
    assert_eq!(
        actual, expected,
        "filter: {filter}\ninput: {input}\nexpected: {expected:?}\nactual:   {actual:?}"
    );
}

// ======================================================================
// Category: Big number / arbitrary precision (have_decnum)
// 9 test(s)
// ======================================================================

/// jq.test line 661: exponents beyond f64's range, kept as decimal literals
/// and printed in canonical form
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

/// jq.test line 2154: tostring on a large literal keeps its exact value
#[test]
#[ignore]
fn gap_bignum_line2154_tostring_large_int() {
    assert_gap(
        ".[0] | tostring | . == if have_decnum then \"13911860366432393\" else \"13911860366432392\" end",
        "[13911860366432393]",
        &["true"],
    );
}

/// jq.test line 2158: tojson on a large literal keeps its exact value
#[test]
#[ignore]
fn gap_bignum_line2158_tojson_large_int() {
    assert_gap(
        ".x | tojson | . == if have_decnum then \"13911860366432393\" else \"13911860366432392\" end",
        "{\"x\":13911860366432393}",
        &["true"],
    );
}

/// jq.test line 2162: adjacent large literals compare exactly, so unequal
#[test]
#[ignore]
fn gap_bignum_line2162_large_int_equality() {
    assert_gap(
        "(13911860366432393 == 13911860366432392) | . == if have_decnum then false else true end",
        "null",
        &["true"],
    );
}

/// jq.test line 2169: subtraction is f64 arithmetic, so the result is
/// ...382, not the exact ...383
#[test]
#[ignore]
fn gap_bignum_line2169_large_int_subtract() {
    assert_gap(". - 10", "13911860366432393", &["13911860366432382"]);
}

/// jq.test line 2173: the same, on an array element
#[test]
#[ignore]
fn gap_bignum_line2173_array_large_int_subtract() {
    assert_gap(".[0] - 10", "[13911860366432393]", &["13911860366432382"]);
}

/// jq.test line 2177: the same, on an object field
#[test]
#[ignore]
fn gap_bignum_line2177_object_large_int_subtract() {
    assert_gap(
        ".x - 10",
        "{\"x\":13911860366432393}",
        &["13911860366432382"],
    );
}

/// jq.test line 2182: negation keeps the literal's exact value
#[test]
#[ignore]
fn gap_bignum_line2182_negate_large_int() {
    assert_gap(
        "-. | tojson == if have_decnum then \"-13911860366432393\" else \"-13911860366432392\" end",
        "13911860366432393",
        &["true"],
    );
}

/// jq.test line 2199: `$n+0` converts to f64, which still equals the
/// literal it came from
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
