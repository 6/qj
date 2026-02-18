/// Property-based differential testing: generate random (filter, input) pairs,
/// run both qj and jq, assert identical stdout + exit status.
///
/// This catches silent behavioral divergences that hand-written tests miss.
/// Uses proptest for deterministic seeds, reproducible failures, and automatic
/// shrinking to minimal failing cases.
///
/// Run with: `cargo test --release jq_differential -- --ignored --nocapture`
use proptest::prelude::*;
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers: run qj / jq and capture (stdout, success)
// ---------------------------------------------------------------------------

fn run_tool(cmd: &str, args: &[&str], input: &str) -> (String, bool) {
    let output = Command::new(cmd)
        .args(args)
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
        .unwrap_or_else(|e| panic!("{cmd} failed to run: {e}"));

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    (stdout, output.status.success())
}

fn run_qj(filter: &str, input: &str) -> (String, bool) {
    run_tool(env!("CARGO_BIN_EXE_qj"), &["-c", filter], input)
}

fn run_jq(filter: &str, input: &str) -> (String, bool) {
    run_tool("jq", &["-c", filter], input)
}

fn jq_available() -> bool {
    Command::new("jq")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// JSON value generators
// ---------------------------------------------------------------------------

fn arb_scalar() -> BoxedStrategy<String> {
    prop_oneof![
        Just("null".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        (0i64..100).prop_map(|n| n.to_string()),
        // Use (0-N) for negatives to avoid CLI flag issues
        (1i64..100).prop_map(|n| format!("(0-{n})")),
        // Avoid integer-valued floats like 100.0 — jq preserves ".0" for filter
        // literals but strips it for computed results, and qj doesn't yet track
        // raw text of filter literals (known limitation).
        prop_oneof![
            Just("0.5".to_string()),
            Just("1.5".to_string()),
            Just("3.14".to_string()),
            Just("99.9".to_string()),
        ],
        "[a-z]{0,8}".prop_map(|s| format!("\"{s}\"")),
    ]
    .boxed()
}

/// JSON value as a string suitable for use as jq filter input.
fn arb_json_input() -> BoxedStrategy<String> {
    let leaf = prop_oneof![
        Just("null".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        (0i64..1000).prop_map(|n| n.to_string()),
        (-100i64..-1).prop_map(|n| n.to_string()),
        Just("0.5".to_string()),
        Just("1.5".to_string()),
        "[a-z]{0,6}".prop_map(|s| format!("\"{s}\"")),
    ];

    leaf.prop_recursive(
        2,  // max depth
        16, // max nodes
        4,  // items per collection
        |inner| {
            prop_oneof![
                // Array of values
                prop::collection::vec(inner.clone(), 0..4)
                    .prop_map(|v| format!("[{}]", v.join(","))),
                // Object with fixed keys a-d, random values
                (inner.clone(), inner.clone(), inner.clone(), inner.clone()).prop_map(
                    |(a, b, c, d)| {
                        // Use 1-4 keys based on hash of first value
                        let n = (a.len() % 4) + 1;
                        let pairs = [
                            format!("\"a\":{a}"),
                            format!("\"b\":{b}"),
                            format!("\"c\":{c}"),
                            format!("\"d\":{d}"),
                        ];
                        format!("{{{}}}", pairs[..n].join(","))
                    }
                ),
            ]
        },
    )
    .boxed()
}

// ---------------------------------------------------------------------------
// Filter generators
// ---------------------------------------------------------------------------

fn arb_arith_op() -> BoxedStrategy<String> {
    prop_oneof![
        Just("+".to_string()),
        Just("-".to_string()),
        Just("*".to_string()),
        Just("/".to_string()),
        Just("%".to_string()),
    ]
    .boxed()
}

fn arb_comparison_op() -> BoxedStrategy<String> {
    prop_oneof![
        Just("==".to_string()),
        Just("!=".to_string()),
        Just("<".to_string()),
        Just("<=".to_string()),
        Just(">".to_string()),
        Just(">=".to_string()),
    ]
    .boxed()
}

fn arb_field() -> BoxedStrategy<String> {
    prop_oneof![
        Just(".a".to_string()),
        Just(".b".to_string()),
        Just(".c".to_string()),
        Just(".d".to_string()),
        Just(".a.b".to_string()),
        Just(".a.b.c".to_string()),
    ]
    .boxed()
}

/// Simple builtin that takes no arguments and operates on `.`
fn arb_nullary_builtin() -> BoxedStrategy<String> {
    prop_oneof![
        Just("length".to_string()),
        Just("keys".to_string()),
        Just("keys_unsorted".to_string()),
        Just("values".to_string()),
        Just("type".to_string()),
        Just("not".to_string()),
        Just("empty".to_string()),
        Just("reverse".to_string()),
        Just("sort".to_string()),
        Just("flatten".to_string()),
        Just("unique".to_string()),
        Just("first".to_string()),
        Just("last".to_string()),
        Just("min".to_string()),
        Just("max".to_string()),
        Just("add".to_string()),
        Just("to_entries".to_string()),
        Just("from_entries".to_string()),
        Just("ascii_downcase".to_string()),
        Just("ascii_upcase".to_string()),
        Just("tostring".to_string()),
        Just("tojson".to_string()),
        Just("explode".to_string()),
        Just("implode".to_string()),
        Just("floor".to_string()),
        Just("ceil".to_string()),
        Just("round".to_string()),
        Just("abs".to_string()),
        Just("utf8bytelength".to_string()),
        Just("isnan".to_string()),
        Just("isinfinite".to_string()),
        Just("isnormal".to_string()),
        Just("paths".to_string()),
        Just("leaf_paths".to_string()),
        Just("any".to_string()),
        Just("all".to_string()),
        Just("transpose".to_string()),
    ]
    .boxed()
}

/// Builtin that takes a single string or value argument
fn arb_unary_builtin() -> BoxedStrategy<String> {
    prop_oneof![
        "[a-e]".prop_map(|f| format!("has(\"{f}\")")),
        "[a-e]".prop_map(|f| format!("ltrimstr(\"{f}\")")),
        "[a-e]".prop_map(|f| format!("rtrimstr(\"{f}\")")),
        "[a-e]".prop_map(|f| format!("startswith(\"{f}\")")),
        "[a-e]".prop_map(|f| format!("endswith(\"{f}\")")),
        "[a-e]".prop_map(|f| format!("contains(\"{f}\")")),
        Just("split(\",\")".to_string()),
        Just("split(\" \")".to_string()),
        Just("join(\",\")".to_string()),
        Just("join(\" \")".to_string()),
        Just("map(. + 1)".to_string()),
        Just("map(. * 2)".to_string()),
        Just("map(type)".to_string()),
        Just("map(length)".to_string()),
        Just("select(. > 0)".to_string()),
        Just("select(. != null)".to_string()),
        Just("select(type == \"string\")".to_string()),
        Just("sort_by(.)".to_string()),
        Just("group_by(.)".to_string()),
        Just("unique_by(.)".to_string()),
        Just("flatten(1)".to_string()),
        Just("limit(2; .[])".to_string()),
        Just("range(5)".to_string()),
        Just("range(1; 5)".to_string()),
    ]
    .boxed()
}

/// Format strings
fn arb_format() -> BoxedStrategy<String> {
    prop_oneof![
        Just("@json".to_string()),
        Just("@text".to_string()),
        Just("@html".to_string()),
        Just("@uri".to_string()),
        Just("@csv".to_string()),
        Just("@tsv".to_string()),
        Just("@sh".to_string()),
        Just("@base64".to_string()),
        Just("@base64d".to_string()),
    ]
    .boxed()
}

/// "Leaf" filter — no recursion needed.
fn arb_leaf_filter() -> BoxedStrategy<String> {
    prop_oneof![
        3 => Just(".".to_string()),
        3 => arb_field(),
        2 => Just(".[]".to_string()),
        2 => arb_scalar(),
        3 => arb_nullary_builtin(),
        2 => arb_unary_builtin(),
        1 => arb_format(),
    ]
    .boxed()
}

/// Composite filter with bounded recursion.
fn arb_filter() -> BoxedStrategy<String> {
    arb_leaf_filter()
        .prop_recursive(
            3,  // max depth
            12, // max nodes
            3,  // items per level
            |inner| {
                prop_oneof![
                    // Pipe: a | b
                    (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
                    // Comma (multiple outputs): a, b
                    (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a}, {b}")),
                    // Arithmetic: a op b
                    (arb_scalar(), arb_arith_op(), arb_scalar())
                        .prop_map(|(a, op, b)| format!("{a} {op} {b}")),
                    // Comparison: . op value
                    (inner.clone(), arb_comparison_op(), arb_scalar())
                        .prop_map(|(a, op, b)| format!("{a} {op} {b}")),
                    // Boolean: a and/or b
                    (
                        inner.clone(),
                        prop_oneof![Just("and"), Just("or")],
                        inner.clone()
                    )
                        .prop_map(|(a, op, b)| format!("{a} {op} {b}")),
                    // Try
                    inner.clone().prop_map(|f| format!("try ({f})")),
                    // If-then-else
                    (inner.clone(), inner.clone(), inner.clone())
                        .prop_map(|(cond, t, e)| format!("if {cond} then {t} else {e} end")),
                    // Alternative operator
                    (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} // {b}")),
                    // Array construction
                    inner.clone().prop_map(|f| format!("[{f}]")),
                    // Object construction with computed value
                    (inner.clone()).prop_map(|v| format!("{{result: {v}}}")),
                    // Variable binding
                    (inner.clone(), inner.clone())
                        .prop_map(|(bind, body)| format!("({bind}) as $x | {body}")),
                    // Reduce
                    (inner.clone()).prop_map(|f| format!("reduce .[] as $x (0; . + ($x | {f}))")),
                    // String interpolation
                    inner.clone().prop_map(|f| format!("\"val=\\({f})\"")),
                ]
            },
        )
        .boxed()
}

// ---------------------------------------------------------------------------
// Differential test config
// ---------------------------------------------------------------------------

/// proptest config: run many cases with a generous timeout per case.
fn diff_config() -> ProptestConfig {
    ProptestConfig {
        // 2000 cases — takes ~60-90s with process spawning overhead
        cases: 2000,
        // If a failure is found, shrink up to 100 iterations to minimize it
        max_shrink_iters: 100,
        // Don't persist regression files (we're #[ignore] and may run in CI)
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

// ---------------------------------------------------------------------------
// The differential tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(diff_config())]

    /// Core differential: random filter × random input, compare qj vs jq.
    #[test]
    #[ignore]
    fn differential_filter_vs_jq(
        filter in arb_filter(),
        input in arb_json_input(),
    ) {
        if !jq_available() {
            return Ok(());
        }

        let (qj_stdout, qj_ok) = run_qj(&filter, &input);
        let (jq_stdout, jq_ok) = run_jq(&filter, &input);

        // Compare stdout (trimmed — trailing newline differences don't matter)
        prop_assert_eq!(
            qj_stdout.trim(),
            jq_stdout.trim(),
            "stdout divergence: filter={:?} input={:?}",
            filter,
            input
        );

        // Compare exit status
        prop_assert_eq!(
            qj_ok,
            jq_ok,
            "exit status divergence: filter={:?} input={:?}\nqj_stdout={:?}\njq_stdout={:?}",
            filter,
            input,
            qj_stdout.trim(),
            jq_stdout.trim()
        );
    }

    /// Focused: arithmetic with all value types (supplements exhaustive e2e test
    /// with randomized operands rather than just fixed representatives).
    #[test]
    #[ignore]
    fn differential_arithmetic_vs_jq(
        a in arb_scalar(),
        op in arb_arith_op(),
        b in arb_scalar(),
        input in arb_json_input(),
    ) {
        if !jq_available() {
            return Ok(());
        }

        let filter = format!("{a} {op} {b}");
        let (qj_stdout, qj_ok) = run_qj(&filter, &input);
        let (jq_stdout, jq_ok) = run_jq(&filter, &input);

        prop_assert_eq!(
            qj_stdout.trim(),
            jq_stdout.trim(),
            "arithmetic divergence: filter={:?} input={:?}",
            filter,
            input
        );
        prop_assert_eq!(
            qj_ok,
            jq_ok,
            "arithmetic exit divergence: filter={:?} input={:?}",
            filter,
            input
        );
    }

    /// Focused: builtins applied to random inputs.
    #[test]
    #[ignore]
    fn differential_builtins_vs_jq(
        builtin in prop_oneof![arb_nullary_builtin(), arb_unary_builtin()],
        input in arb_json_input(),
    ) {
        if !jq_available() {
            return Ok(());
        }

        // Wrap in try to avoid noise from type errors (e.g., keys on a number).
        // Compare the try-wrapped output so both tools agree on suppression.
        let filter = format!("try ({builtin})");
        let (qj_stdout, qj_ok) = run_qj(&filter, &input);
        let (jq_stdout, jq_ok) = run_jq(&filter, &input);

        prop_assert_eq!(
            qj_stdout.trim(),
            jq_stdout.trim(),
            "builtin divergence: filter={:?} input={:?}",
            filter,
            input
        );
        prop_assert_eq!(
            qj_ok,
            jq_ok,
            "builtin exit divergence: filter={:?} input={:?}",
            filter,
            input
        );
    }

    /// Focused: format strings applied to random inputs.
    #[test]
    #[ignore]
    fn differential_formats_vs_jq(
        fmt in arb_format(),
        input in arb_json_input(),
    ) {
        if !jq_available() {
            return Ok(());
        }

        let filter = format!("try ({fmt})");
        let (qj_stdout, qj_ok) = run_qj(&filter, &input);
        let (jq_stdout, jq_ok) = run_jq(&filter, &input);

        prop_assert_eq!(
            qj_stdout.trim(),
            jq_stdout.trim(),
            "format divergence: filter={:?} input={:?}",
            filter,
            input
        );
        prop_assert_eq!(
            qj_ok,
            jq_ok,
            "format exit divergence: filter={:?} input={:?}",
            filter,
            input
        );
    }
}
