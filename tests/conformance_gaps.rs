/// Conformance gap tests — 174 jq.test cases that jx currently fails.
///
/// Auto-generated from jq.test failure analysis. Each test documents
/// a specific gap with category and fix suggestion. Run individual tests
/// or categories as features are implemented:
///
///   cargo test --release conformance_gaps -- --ignored              # all gaps
///   cargo test --release gap_label_break -- --ignored               # one category
///   cargo test --release gap_foreach_line341 -- --ignored            # one test
///
/// As each gap is fixed, remove the test (it will be covered by jq_conformance).
mod common;

/// Run jx with a filter and input, return stdout lines.
fn run_jx(filter: &str, input: &str) -> Vec<String> {
    let jx = common::Tool {
        name: "jx".to_string(),
        path: env!("CARGO_BIN_EXE_jx").to_string(),
    };
    match common::run_tool(&jx, filter, input, &["-c", "--"]) {
        Some(output) => output
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        None => vec!["<jx failed to run>".to_string()],
    }
}

/// Check if jx output matches expected (JSON-aware comparison).
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
// Category: Format strings: @text, @csv, @tsv, @html, @uri, @urid, @sh
// 4 test(s)
//
// Fix: Implement missing format builtins in src/filter/builtins/. @text=identity for strings, @csv/@tsv=array-to-delimited, @html=entity-encode, @uri/@urid=percent-encode/decode, @sh=shell-quote.
// ======================================================================

/// jq.test line 72: `@text,@json,([1,.]|@csv,@tsv),@html,(@uri|.,@urid),@sh,(@base64|.,@base64d)`
#[test]
#[ignore]
fn gap_format_strings_line72_text_json_1_csv_tsv_html_ur() {
    assert_gap(
        "@text,@json,([1,.]|@csv,@tsv),@html,(@uri|.,@urid),@sh,(@base64|.,@base64d)",
        "\"!()<>&'\\\"\\t\"",
        &[
            "\"!()<>&'\\\"\\t\"",
            "\"\\\"!()<>&'\\\\\\\"\\\\t\\\"\"",
            "\"1,\\\"!()<>&'\\\"\\\"\\t\\\"\"",
            "\"1\\t!()<>&'\\\"\\\\t\"",
            "\"!()&lt;&gt;&amp;&apos;&quot;\\t\"",
            "\"%21%28%29%3C%3E%26%27%22%09\"",
            "\"!()<>&'\\\"\\t\"",
            "\"'!()<>&'\\\\''\\\"\\t'\"",
            "\"ISgpPD4mJyIJ\"",
            "\"!()<>&'\\\"\\t\"",
        ],
    );
}

/// jq.test line 98: `@urid`
#[test]
#[ignore]
fn gap_format_strings_line98_urid() {
    assert_gap("@urid", "\"%CE%BC\"", &["\"\\u03bc\""]);
}

/// jq.test line 102: `@html "<b>\(.)</b>"`
#[test]
#[ignore]
fn gap_format_strings_line102_html_b_b() {
    assert_gap(
        "@html \"<b>\\(.)</b>\"",
        "\"<script>hax</script>\"",
        &["\"<b>&lt;script&gt;hax&lt;/script&gt;</b>\""],
    );
}

/// jq.test line 2506: `strflocaltime("" | ., @uri)`
#[test]
#[ignore]
fn gap_format_strings_line2506_strflocaltime_uri() {
    assert_gap("strflocaltime(\"\" | ., @uri)", "0", &["\"\"", "\"\""]);
}

// ======================================================================
// Category: Object construction with shorthand keys: {"a", b, "a$\(expr)"}
// 1 test(s)
//
// Fix: In parser, allow bare identifiers and string-interpolated keys in object construction without explicit `:value`. The value is `.identifier` for bare idents.
// ======================================================================

/// jq.test line 122: `{"a",b,"a$\(1+1)"}`
#[test]
#[ignore]
fn gap_object_shorthand_line122_a_b_a_1_1() {
    assert_gap(
        "{\"a\",b,\"a$\\(1+1)\"}",
        "{\"a\":1, \"b\":2, \"c\":3, \"a$2\":4}",
        &["{\"a\":1, \"b\":2, \"a$2\":4}"],
    );
}

// ======================================================================
// Category: Try-catch edge cases
// 2 test(s)
//
// Fix: Fix try-catch to properly catch errors from sub-expressions. Ensure error messages match jq format. Handle `try (expr) catch handler` where expr produces multiple outputs or errors mid-stream.
// ======================================================================

/// jq.test line 200: `map(try .a[] catch ., try .a.[] catch ., .a[]?, .a.[]?)`
#[test]
#[ignore]
fn gap_try_catch_line200_map_try_a_catch_try_a_catch() {
    assert_gap(
        "map(try .a[] catch ., try .a.[] catch ., .a[]?, .a.[]?)",
        "[{\"a\": [1,2]}, {\"a\": 123}]",
        &[
            "[1,2,1,2,1,2,1,2,\"Cannot iterate over number (123)\",\"Cannot iterate over number (123)\"]",
        ],
    );
}

/// jq.test line 205: `try ["OK", (.[] | error)] catch ["KO", .]`
#[test]
#[ignore]
fn gap_try_catch_line205_try_ok_error_catch_ko() {
    assert_gap(
        "try [\"OK\", (.[] | error)] catch [\"KO\", .]",
        "{\"a\":[\"b\"],\"c\":[\"d\"]}",
        &["[\"KO\",[\"b\"]]"],
    );
}

// ======================================================================
// Category: Label-break ($here) for early loop exit
// 4 test(s)
//
// Fix: Implement `label $name | expr` and `break $name` in parser and evaluator. label creates a catch point; break unwinds to it via a special error/signal.
// ======================================================================

/// jq.test line 315: `[(label $here | .[] | if .>1 then break $here else . end), "hi!"]`
#[test]
#[ignore]
fn gap_label_break_line315_label_here_if_1_then_break() {
    assert_gap(
        "[(label $here | .[] | if .>1 then break $here else . end), \"hi!\"]",
        "[0,1,2]",
        &["[0,1,\"hi!\"]"],
    );
}

/// jq.test line 319: `[(label $here | .[] | if .>1 then break $here else . end), "hi!"]`
#[test]
#[ignore]
fn gap_label_break_line319_label_here_if_1_then_break() {
    assert_gap(
        "[(label $here | .[] | if .>1 then break $here else . end), \"hi!\"]",
        "[0,2,1]",
        &["[0,\"hi!\"]"],
    );
}

/// jq.test line 333: `[label $out | foreach .[] as $item ([3, null]; if .[0] < 1 then break $out else ...`
#[test]
#[ignore]
fn gap_label_break_line333_label_out_foreach_as_item_3() {
    assert_gap(
        "[label $out | foreach .[] as $item ([3, null]; if .[0] < 1 then break $out else [.[0] -1, $item] end; .[1])]",
        "[11,22,33,44,55,66,77,88,99]",
        &["[11,22,33]"],
    );
}

/// jq.test line 2243: `[ label $if | range(10) | ., (select(. == 5) | break $if) ]`
#[test]
#[ignore]
fn gap_label_break_line2243_label_if_range_10_select() {
    assert_gap(
        "[ label $if | range(10) | ., (select(. == 5) | break $if) ]",
        "null",
        &["[0,1,2,3,4,5]"],
    );
}

// ======================================================================
// Category: Foreach expression edge cases
// 4 test(s)
//
// Fix: Fix foreach to support: destructuring patterns in `as`, generator expressions in init/update, multiple init values (foreach .[] as $x (0, 1; ...)), and division expressions as generators.
// ======================================================================

/// jq.test line 341: `[foreach .[] as [$i, $j] (0; . + $i - $j)]`
#[test]
#[ignore]
fn gap_foreach_line341_foreach_as_i_j_0_i_j() {
    assert_gap(
        "[foreach .[] as [$i, $j] (0; . + $i - $j)]",
        "[[2,1], [5,3], [6,4]]",
        &["[1,3,5]"],
    );
}

/// jq.test line 345: `[foreach .[] as {a:$a} (0; . + $a; -.)]`
#[test]
#[ignore]
fn gap_foreach_line345_foreach_as_a_a_0_a() {
    assert_gap(
        "[foreach .[] as {a:$a} (0; . + $a; -.)]",
        "[{\"a\":1}, {\"b\":2}, {\"a\":3, \"b\":4}]",
        &["[-1, -1, -4]"],
    );
}

/// jq.test line 353: `[foreach .[] / .[] as $i (0; . + $i)]`
#[test]
#[ignore]
fn gap_foreach_line353_foreach_as_i_0_i() {
    assert_gap(
        "[foreach .[] / .[] as $i (0; . + $i)]",
        "[1,2]",
        &["[1,3,3.5,4.5]"],
    );
}

/// jq.test line 2496: `foreach .[] as $x (0, 1; . + $x)`
#[test]
#[ignore]
fn gap_foreach_line2496_foreach_as_x_0_1_x() {
    assert_gap(
        "foreach .[] as $x (0, 1; . + $x)",
        "[1, 2]",
        &["1", "3", "2", "4"],
    );
}

// ======================================================================
// Category: Advanced generator builtins: limit, skip, nth with multiple args
// 1 test(s)
//
// Fix: Implement multi-arg variants of limit/skip/nth. `limit(n; expr)` already works; add `limit(n,m; expr)` producing limit-n then limit-m. Same for skip and nth.
// ======================================================================

/// jq.test line 405: `[nth(0,5,9,10,15; range(.)), try nth(-1; range(.)) catch .]`
#[test]
#[ignore]
fn gap_advanced_generators_line405_nth_0_5_9_10_15_range_try_nth() {
    assert_gap(
        "[nth(0,5,9,10,15; range(.)), try nth(-1; range(.)) catch .]",
        "10",
        &["[0,5,9,\"nth doesn't support negative indices\"]"],
    );
}

// ======================================================================
// Category: Alternative pattern matching (?//)
// 13 test(s)
//
// Fix: Implement the ?// (alternative destructuring) operator in parser and evaluator. `expr as pattern1 ?// pattern2 ?// pattern3 | body` tries each pattern, using the first that matches.
// ======================================================================

/// jq.test line 929: `.[] | . as {$a, b: [$c, {$d}]} ?// [$a, {$b}, $e] ?// $f | [$a, $b, $c, $d, $e, ...`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line929_as_a_b_c_d_a() {
    assert_gap(
        ".[] | . as {$a, b: [$c, {$d}]} ?// [$a, {$b}, $e] ?// $f | [$a, $b, $c, $d, $e, $f]",
        "[{\"a\":1, \"b\":[2,{\"d\":3}]}, [4, {\"b\":5, \"c\":6}, 7, 8, 9], \"foo\"]",
        &[
            "[1, null, 2, 3, null, null]",
            "[4, 5, null, null, 7, null]",
            "[null, null, null, null, null, \"foo\"]",
        ],
    );
}

/// jq.test line 952: `.[] | . as {a:$a} ?// {a:$a} ?// $a | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line952_as_a_a_a_a_a_a() {
    assert_gap(
        ".[] | . as {a:$a} ?// {a:$a} ?// $a | $a",
        "[[3],[4],[5],6]",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 959: `.[] as {a:$a} ?// {a:$a} ?// $a | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line959_as_a_a_a_a_a_a() {
    assert_gap(
        ".[] as {a:$a} ?// {a:$a} ?// $a | $a",
        "[[3],[4],[5],6]",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 966: `[[3],[4],[5],6][] | . as {a:$a} ?// {a:$a} ?// $a | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line966_3_4_5_6_as_a_a_a() {
    assert_gap(
        "[[3],[4],[5],6][] | . as {a:$a} ?// {a:$a} ?// $a | $a",
        "null",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 973: `[[3],[4],[5],6] | .[] as {a:$a} ?// {a:$a} ?// $a | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line973_3_4_5_6_as_a_a_a() {
    assert_gap(
        "[[3],[4],[5],6] | .[] as {a:$a} ?// {a:$a} ?// $a | $a",
        "null",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 980: `.[] | . as {a:$a} ?// $a ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line980_as_a_a_a_a_a_a() {
    assert_gap(
        ".[] | . as {a:$a} ?// $a ?// {a:$a} | $a",
        "[[3],[4],[5],6]",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 987: `.[] as {a:$a} ?// $a ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line987_as_a_a_a_a_a_a() {
    assert_gap(
        ".[] as {a:$a} ?// $a ?// {a:$a} | $a",
        "[[3],[4],[5],6]",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 994: `[[3],[4],[5],6][] | . as {a:$a} ?// $a ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line994_3_4_5_6_as_a_a_a() {
    assert_gap(
        "[[3],[4],[5],6][] | . as {a:$a} ?// $a ?// {a:$a} | $a",
        "null",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 1001: `[[3],[4],[5],6] | .[] as {a:$a} ?// $a ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line1001_3_4_5_6_as_a_a_a() {
    assert_gap(
        "[[3],[4],[5],6] | .[] as {a:$a} ?// $a ?// {a:$a} | $a",
        "null",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 1008: `.[] | . as $a ?// {a:$a} ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line1008_as_a_a_a_a_a_a() {
    assert_gap(
        ".[] | . as $a ?// {a:$a} ?// {a:$a} | $a",
        "[[3],[4],[5],6]",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 1015: `.[] as $a ?// {a:$a} ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line1015_as_a_a_a_a_a_a() {
    assert_gap(
        ".[] as $a ?// {a:$a} ?// {a:$a} | $a",
        "[[3],[4],[5],6]",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 1022: `[[3],[4],[5],6][] | . as $a ?// {a:$a} ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line1022_3_4_5_6_as_a_a_a() {
    assert_gap(
        "[[3],[4],[5],6][] | . as $a ?// {a:$a} ?// {a:$a} | $a",
        "null",
        &["[3]", "[4]", "[5]", "6"],
    );
}

/// jq.test line 1029: `[[3],[4],[5],6] | .[] as $a ?// {a:$a} ?// {a:$a} | $a`
#[test]
#[ignore]
fn gap_alternative_pattern_match_line1029_3_4_5_6_as_a_a_a() {
    assert_gap(
        "[[3],[4],[5],6] | .[] as $a ?// {a:$a} ?// {a:$a} | $a",
        "null",
        &["[3]", "[4]", "[5]", "6"],
    );
}

// ======================================================================
// Category: Destructuring bind patterns in `as`
// 11 test(s)
//
// Fix: Implement array and object destructuring in `as` patterns: `. as [$a, {$b}] | ...`. Handle nested patterns, optional fields, and pattern variable shadowing.
// ======================================================================

/// jq.test line 524: `[1, {c:3, d:4}] as [$a, {c:$b, b:$c}] | $a, $b, $c`
#[test]
#[ignore]
fn gap_destructuring_bind_line524_1_c_3_d_4_as_a_c_b_b_c() {
    assert_gap(
        "[1, {c:3, d:4}] as [$a, {c:$b, b:$c}] | $a, $b, $c",
        "null",
        &["1", "3", "null"],
    );
}

/// jq.test line 530: `. as {as: $kw, "str": $str, ("e"+"x"+"p"): $exp} | [$kw, $str, $exp]`
#[test]
#[ignore]
fn gap_destructuring_bind_line530_as_as_kw_str_str_e_x_p() {
    assert_gap(
        ". as {as: $kw, \"str\": $str, (\"e\"+\"x\"+\"p\"): $exp} | [$kw, $str, $exp]",
        "{\"as\": 1, \"str\": 2, \"exp\": 3}",
        &["[1, 2, 3]"],
    );
}

/// jq.test line 534: `.[] as [$a, $b] | [$b, $a]`
#[test]
#[ignore]
fn gap_destructuring_bind_line534_as_a_b_b_a() {
    assert_gap(
        ".[] as [$a, $b] | [$b, $a]",
        "[[1], [1, 2, 3]]",
        &["[null, 1]", "[2, 1]"],
    );
}

/// jq.test line 539: `. as $i | . as [$i] | $i`
#[test]
#[ignore]
fn gap_destructuring_bind_line539_as_i_as_i_i() {
    assert_gap(". as $i | . as [$i] | $i", "[0]", &["0"]);
}

/// jq.test line 543: `. as [$i] | . as $i | $i`
#[test]
#[ignore]
fn gap_destructuring_bind_line543_as_i_as_i_i() {
    assert_gap(". as [$i] | . as $i | $i", "[0]", &["[0]"]);
}

/// jq.test line 894: `reduce .[] as [$i, {j:$j}] (0; . + $i - $j)`
#[test]
#[ignore]
fn gap_destructuring_bind_line894_reduce_as_i_j_j_0_i() {
    assert_gap(
        "reduce .[] as [$i, {j:$j}] (0; . + $i - $j)",
        "[[2,{\"j\":1}], [5,{\"j\":3}], [6,{\"j\":4}]]",
        &["5"],
    );
}

/// jq.test line 898: `reduce [[1,2,10], [3,4,10]][] as [$i,$j] (0; . + $i * $j)`
#[test]
#[ignore]
fn gap_destructuring_bind_line898_reduce_1_2_10_3_4_10_as_i_j() {
    assert_gap(
        "reduce [[1,2,10], [3,4,10]][] as [$i,$j] (0; . + $i * $j)",
        "null",
        &["14"],
    );
}

/// jq.test line 920: `. as {$a, b: [$c, {$d}]} | [$a, $c, $d]`
#[test]
#[ignore]
fn gap_destructuring_bind_line920_as_a_b_c_d_a_c_d() {
    assert_gap(
        ". as {$a, b: [$c, {$d}]} | [$a, $c, $d]",
        "{\"a\":1, \"b\":[2,{\"d\":3}]}",
        &["[1,2,3]"],
    );
}

/// jq.test line 924: `. as {$a, $b:[$c, $d]}| [$a, $b, $c, $d]`
#[test]
#[ignore]
fn gap_destructuring_bind_line924_as_a_b_c_d_a_b_c_d() {
    assert_gap(
        ". as {$a, $b:[$c, $d]}| [$a, $b, $c, $d]",
        "{\"a\":1, \"b\":[2,{\"d\":3}]}",
        &["[1,[2,{\"d\":3}],2,{\"d\":3}]"],
    );
}

/// jq.test line 2474: `.[] as [$x, $y] | try ["ok", ($x | ltrimstr($y))] catch ["ko", .]`
#[test]
#[ignore]
fn gap_destructuring_bind_line2474_as_x_y_try_ok_x_ltrim() {
    assert_gap(
        ".[] as [$x, $y] | try [\"ok\", ($x | ltrimstr($y))] catch [\"ko\", .]",
        "[[\"hi\",1],[1,\"hi\"],[\"hi\",\"hi\"],[1,1]]",
        &[
            "[\"ko\",\"startswith() requires string inputs\"]",
            "[\"ko\",\"startswith() requires string inputs\"]",
            "[\"ok\",\"\"]",
            "[\"ko\",\"startswith() requires string inputs\"]",
        ],
    );
}

/// jq.test line 2481: `.[] as [$x, $y] | try ["ok", ($x | rtrimstr($y))] catch ["ko", .]`
#[test]
#[ignore]
fn gap_destructuring_bind_line2481_as_x_y_try_ok_x_rtrim() {
    assert_gap(
        ".[] as [$x, $y] | try [\"ok\", ($x | rtrimstr($y))] catch [\"ko\", .]",
        "[[\"hi\",1],[1,\"hi\"],[\"hi\",\"hi\"],[1,1]]",
        &[
            "[\"ko\",\"endswith() requires string inputs\"]",
            "[\"ko\",\"endswith() requires string inputs\"]",
            "[\"ok\",\"\"]",
            "[\"ko\",\"endswith() requires string inputs\"]",
        ],
    );
}

// ======================================================================
// Category: Path expression edge cases
// 9 test(s)
//
// Fix: Fix path() to error on non-path expressions (map, select inside path). Fix delpaths for negative indices and type errors.
// ======================================================================

/// jq.test line 1114: `try path(.a | map(select(.b == 0))) catch .`
#[test]
#[ignore]
fn gap_path_expressions_line1114_try_path_a_map_select_b_0_catc() {
    assert_gap(
        "try path(.a | map(select(.b == 0))) catch .",
        "{\"a\":[{\"b\":0}]}",
        &["\"Invalid path expression with result [{\\\"b\\\":0}]\""],
    );
}

/// jq.test line 1118: `try path(.a | map(select(.b == 0)) | .[0]) catch .`
#[test]
#[ignore]
fn gap_path_expressions_line1118_try_path_a_map_select_b_0_0() {
    assert_gap(
        "try path(.a | map(select(.b == 0)) | .[0]) catch .",
        "{\"a\":[{\"b\":0}]}",
        &["\"Invalid path expression near attempt to access element 0 of [{\\\"b\\\":0}]\""],
    );
}

/// jq.test line 1122: `try path(.a | map(select(.b == 0)) | .c) catch .`
#[test]
#[ignore]
fn gap_path_expressions_line1122_try_path_a_map_select_b_0_c() {
    assert_gap(
        "try path(.a | map(select(.b == 0)) | .c) catch .",
        "{\"a\":[{\"b\":0}]}",
        &[
            "\"Invalid path expression near attempt to access element \\\"c\\\" of [{\\\"b\\\":0}]\"",
        ],
    );
}

/// jq.test line 1126: `try path(.a | map(select(.b == 0)) | .[]) catch .`
#[test]
#[ignore]
fn gap_path_expressions_line1126_try_path_a_map_select_b_0() {
    assert_gap(
        "try path(.a | map(select(.b == 0)) | .[]) catch .",
        "{\"a\":[{\"b\":0}]}",
        &["\"Invalid path expression near attempt to iterate through [{\\\"b\\\":0}]\""],
    );
}

/// jq.test line 1160: `delpaths([[-200]])`
#[test]
#[ignore]
fn gap_path_expressions_line1160_delpaths_200() {
    assert_gap("delpaths([[-200]])", "[1,2,3]", &["[1,2,3]"]);
}

/// jq.test line 1164: `try delpaths(0) catch .`
#[test]
#[ignore]
fn gap_path_expressions_line1164_try_delpaths_0_catch() {
    assert_gap(
        "try delpaths(0) catch .",
        "{}",
        &["\"Paths must be specified as an array\""],
    );
}

/// jq.test line 1241: `.[] | try (getpath(["a",0,"b"]) |= 5) catch .`
#[test]
#[ignore]
fn gap_path_expressions_line1241_try_getpath_a_0_b_5_ca() {
    assert_gap(
        ".[] | try (getpath([\"a\",0,\"b\"]) |= 5) catch .",
        "[null,{\"b\":0},{\"a\":0},{\"a\":null},{\"a\":[0,1]},{\"a\":{\"b\":1}},{\"a\":[{}]},{\"a\":[{\"c\":3}]}]",
        &[
            "{\"a\":[{\"b\":5}]}",
            "{\"b\":0,\"a\":[{\"b\":5}]}",
            "\"Cannot index number with number\"",
            "{\"a\":[{\"b\":5}]}",
            "\"Cannot index number with string \\\"b\\\"\"",
            "\"Cannot index object with number\"",
            "{\"a\":[{\"b\":5}]}",
            "{\"a\":[{\"c\":3,\"b\":5}]}",
        ],
    );
}

/// jq.test line 2452: `try ["ok", setpath([1]; 1)] catch ["ko", .]`
#[test]
#[ignore]
fn gap_path_expressions_line2452_try_ok_setpath_1_1_catch_ko() {
    assert_gap(
        "try [\"ok\", setpath([1]; 1)] catch [\"ko\", .]",
        "{\"hi\":\"hello\"}",
        &["[\"ko\",\"Cannot index object with number\"]"],
    );
}

/// jq.test line 2491: `try ["OK", setpath([[1]]; 1)] catch ["KO", .]`
#[test]
#[ignore]
fn gap_path_expressions_line2491_try_ok_setpath_1_1_catch_ko() {
    assert_gap(
        "try [\"OK\", setpath([[1]]; 1)] catch [\"KO\", .]",
        "[]",
        &["[\"KO\",\"Cannot update field at array index of array\"]"],
    );
}

// ======================================================================
// Category: Del with complex/multiple path expressions
// 3 test(s)
//
// Fix: Fix del to handle multiple comma-separated paths, slice ranges, and negative indices. Handle del(.), del(empty), and del with generator expressions.
// ======================================================================

/// jq.test line 474: `del(.[2:4],.[0],.[-2:])`
#[test]
#[ignore]
fn gap_del_complex_line474_del_2_4_0_2() {
    assert_gap("del(.[2:4],.[0],.[-2:])", "[0,1,2,3,4,5,6,7]", &["[1,4,5]"]);
}

/// jq.test line 1168: `del(.), del(empty), del((.foo,.bar,.baz) | .[2,3,0]), del(.foo[0], .bar[0], .foo...`
#[test]
#[ignore]
fn gap_del_complex_line1168_del_del_empty_del_foo_bar_baz() {
    assert_gap(
        "del(.), del(empty), del((.foo,.bar,.baz) | .[2,3,0]), del(.foo[0], .bar[0], .foo, .baz.bar[0].x)",
        "{\"foo\": [0,1,2,3,4], \"bar\": [0,1]}",
        &[
            "null",
            "{\"foo\": [0,1,2,3,4], \"bar\": [0,1]}",
            "{\"foo\": [1,4], \"bar\": [1]}",
            "{\"bar\": [1]}",
        ],
    );
}

/// jq.test line 1175: `del(.[1], .[-6], .[2], .[-3:9])`
#[test]
#[ignore]
fn gap_del_complex_line1175_del_1_6_2_3_9() {
    assert_gap(
        "del(.[1], .[-6], .[2], .[-3:9])",
        "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]",
        &["[0, 3, 5, 6, 9]"],
    );
}

// ======================================================================
// Category: Assignment operator edge cases
// 13 test(s)
//
// Fix: Fix compound assignment (.[] +=, -=, *=, /=, %=) to work with array iteration. Fix .foo += .foo to not double-evaluate. Handle update-assignment with def-based paths and getpath-based update assignments.
// ======================================================================

/// jq.test line 213: `try (.foo[-1] = 0) catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line213_try_foo_1_0_catch() {
    assert_gap(
        "try (.foo[-1] = 0) catch .",
        "null",
        &["\"Out of bounds negative array index\""],
    );
}

/// jq.test line 217: `try (.foo[-2] = 0) catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line217_try_foo_2_0_catch() {
    assert_gap(
        "try (.foo[-2] = 0) catch .",
        "null",
        &["\"Out of bounds negative array index\""],
    );
}

/// jq.test line 229: `try (.[999999999] = 0) catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line229_try_999999999_0_catch() {
    assert_gap(
        "try (.[999999999] = 0) catch .",
        "null",
        &["\"Array index too large\""],
    );
}

/// jq.test line 478: `.[2:4] = ([], ["a","b"], ["a","b","c"])`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line478_2_4_a_b_a_b_c() {
    assert_gap(
        ".[2:4] = ([], [\"a\",\"b\"], [\"a\",\"b\",\"c\"])",
        "[0,1,2,3,4,5,6,7]",
        &[
            "[0,1,4,5,6,7]",
            "[0,1,\"a\",\"b\",4,5,6,7]",
            "[0,1,\"a\",\"b\",\"c\",4,5,6,7]",
        ],
    );
}

/// jq.test line 1216: `.[] += 2, .[] *= 2, .[] -= 2, .[] /= 2, .[] %=2`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line1216_2_2_2_2() {
    assert_gap(
        ".[] += 2, .[] *= 2, .[] -= 2, .[] /= 2, .[] %=2",
        "[1,3,5]",
        &[
            "[3,5,7]",
            "[2,6,10]",
            "[-1,1,3]",
            "[0.5, 1.5, 2.5]",
            "[1,1,1]",
        ],
    );
}

/// jq.test line 1228: `.foo += .foo`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line1228_foo_foo() {
    assert_gap(".foo += .foo", "{\"foo\":2}", &["{\"foo\":4}"]);
}

/// jq.test line 1236: `def inc(x): x |= .+1; inc(.[].a)`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line1236_def_inc_x_x_1_inc_a() {
    assert_gap(
        "def inc(x): x |= .+1; inc(.[].a)",
        "[{\"a\":1,\"b\":2},{\"a\":2,\"b\":4},{\"a\":7,\"b\":8}]",
        &["[{\"a\":2,\"b\":2},{\"a\":3,\"b\":4},{\"a\":8,\"b\":8}]"],
    );
}

/// jq.test line 1273: `try ((map(select(.a == 1))[].b) = 10) catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line1273_try_map_select_a_1_b_10_ca() {
    assert_gap(
        "try ((map(select(.a == 1))[].b) = 10) catch .",
        "[{\"a\":0},{\"a\":1}]",
        &["\"Invalid path expression near attempt to iterate through [{\\\"a\\\":1}]\""],
    );
}

/// jq.test line 1277: `try ((map(select(.a == 1))[].a) |= .+1) catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line1277_try_map_select_a_1_a_1() {
    assert_gap(
        "try ((map(select(.a == 1))[].a) |= .+1) catch .",
        "[{\"a\":0},{\"a\":1}]",
        &["\"Invalid path expression near attempt to iterate through [{\\\"a\\\":1}]\""],
    );
}

/// jq.test line 1357: `.[] //= .[0]`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line1357_0() {
    assert_gap(
        ".[] //= .[0]",
        "[\"hello\",true,false,[false],null]",
        &["[\"hello\",true,\"hello\",[false],\"hello\"]"],
    );
}

/// jq.test line 2417: `[range(5)] | .[1.1] = 5`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line2417_range_5_1_1_5() {
    assert_gap("[range(5)] | .[1.1] = 5", "null", &["[0,5,2,3,4]"]);
}

/// jq.test line 2437: `try ("foobar" | .[1.5:3.5] = "xyz") catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line2437_try_foobar_1_5_3_5_xyz_catc() {
    assert_gap(
        "try (\"foobar\" | .[1.5:3.5] = \"xyz\") catch .",
        "null",
        &["\"Cannot update string slices\""],
    );
}

/// jq.test line 2441: `try ([range(10)] | .[1.5:3.5] = ["xyz"]) catch .`
#[test]
#[ignore]
fn gap_assignment_edge_cases_line2441_try_range_10_1_5_3_5_xyz() {
    assert_gap(
        "try ([range(10)] | .[1.5:3.5] = [\"xyz\"]) catch .",
        "null",
        &["[0,\"xyz\",4,5,6,7,8,9]"],
    );
}

// ======================================================================
// Category: Advanced def features: complex scoping, inner defs as generators
// 4 test(s)
//
// Fix: Fix nested def scoping where inner defs shadow/unshadow across semicolons. Handle def inside generators and cross-checking filter-param vs $-param equivalence.
// ======================================================================

/// jq.test line 789: `def f: 1; def g: f, def f: 2; def g: 3; f, def f: g; f, g; def f: 4; [f, def f: ...`
#[test]
#[ignore]
fn gap_def_advanced_line789_def_f_1_def_g_f_def_f_2_def_g() {
    assert_gap(
        "def f: 1; def g: f, def f: 2; def g: 3; f, def f: g; f, g; def f: 4; [f, def f: g; def g: 5; f, g]+[f,g]",
        "null",
        &["[4,1,2,3,3,5,4,1,2,3,3]"],
    );
}

/// jq.test line 869: `def x(a;b): a as $a | b as $b | $a + $b; def y($a;$b): $a + $b; def check(a;b): ...`
#[test]
#[ignore]
fn gap_def_advanced_line869_def_x_a_b_a_as_a_b_as_b_a_b() {
    assert_gap(
        "def x(a;b): a as $a | b as $b | $a + $b; def y($a;$b): $a + $b; def check(a;b): [x(a;b)] == [y(a;b)]; check(.[];.[]*2)",
        "[1,2,3]",
        &["true"],
    );
}

/// jq.test line 1281: `def x: .[1,2]; x=10`
#[test]
#[ignore]
fn gap_def_advanced_line1281_def_x_1_2_x_10() {
    assert_gap("def x: .[1,2]; x=10", "[0,1,2]", &["[0,10,10]"]);
}

/// jq.test line 1285: `try (def x: reverse; x=10) catch .`
#[test]
#[ignore]
fn gap_def_advanced_line1285_try_def_x_reverse_x_10_catch() {
    assert_gap(
        "try (def x: reverse; x=10) catch .",
        "[0,1,2]",
        &["\"Invalid path expression with result [2,1,0]\""],
    );
}

// ======================================================================
// Category: String operation edge cases: join, trimstr, trim, implode/explode, string*number
// 10 test(s)
//
// Fix: Fix join to produce errors on non-string elements. Fix trimstr to only trim prefixes/suffixes (not substrings). Fix implode/explode for edge cases. String multiplication: handle negative/float multipliers and error on too-large values.
// ======================================================================

/// jq.test line 1511: `[.[]|trimstr("foo")]`
#[test]
#[ignore]
fn gap_string_ops_line1511_trimstr_foo() {
    assert_gap(
        "[.[]|trimstr(\"foo\")]",
        "[\"fo\", \"foo\", \"barfoo\", \"foobarfoo\", \"foob\"]",
        &["[\"fo\",\"\",\"bar\",\"bar\",\"b\"]"],
    );
}

/// jq.test line 1537: `try trim catch ., try ltrim catch ., try rtrim catch .`
#[test]
#[ignore]
fn gap_string_ops_line1537_try_trim_catch_try_ltrim_catch() {
    assert_gap(
        "try trim catch ., try ltrim catch ., try rtrim catch .",
        "123",
        &[
            "\"trim input must be a string\"",
            "\"trim input must be a string\"",
            "\"trim input must be a string\"",
        ],
    );
}

/// jq.test line 1587: `[.[] * "abc"]`
#[test]
#[ignore]
fn gap_string_ops_line1587_abc() {
    assert_gap(
        "[.[] * \"abc\"]",
        "[-1.0, -0.5, 0.0, 0.5, 1.0, 1.5, 3.7, 10.0]",
        &["[null,null,\"\",\"\",\"abc\",\"abc\",\"abcabcabc\",\"abcabcabcabcabcabcabcabcabcabc\"]"],
    );
}

/// jq.test line 1603: `try (. * 1000000000) catch .`
#[test]
#[ignore]
fn gap_string_ops_line1603_try_1000000000_catch() {
    assert_gap(
        "try (. * 1000000000) catch .",
        "\"abc\"",
        &["\"Repeat string result too long\""],
    );
}

/// jq.test line 1967: `"x" * range(0; 12; 2) + "☆" * 5 | try -. catch .`
#[test]
#[ignore]
fn gap_string_ops_line1967_x_range_0_12_2_5_try() {
    assert_gap(
        "\"x\" * range(0; 12; 2) + \"☆\" * 5 | try -. catch .",
        "null",
        &[
            "\"string (\\\"☆☆☆...) cannot be negated\"",
            "\"string (\\\"xx☆☆...) cannot be negated\"",
            "\"string (\\\"xxxx☆☆...) cannot be negated\"",
            "\"string (\\\"xxxxxx☆...) cannot be negated\"",
            "\"string (\\\"xxxxxxxx...) cannot be negated\"",
            "\"string (\\\"xxxxxxxxxx...) cannot be negated\"",
        ],
    );
}

/// jq.test line 1992: `try join(",") catch .`
#[test]
#[ignore]
fn gap_string_ops_line1992_try_join_catch() {
    assert_gap(
        "try join(\",\") catch .",
        "[\"1\",\"2\",{\"a\":{\"b\":{\"c\":33}}}]",
        &["\"string (\\\"1,2,\\\") and object ({\\\"a\\\":{\\\"b\\\":{...) cannot be added\""],
    );
}

/// jq.test line 1996: `try join(",") catch .`
#[test]
#[ignore]
fn gap_string_ops_line1996_try_join_catch() {
    assert_gap(
        "try join(\",\") catch .",
        "[\"1\",\"2\",[3,4,5]]",
        &["\"string (\\\"1,2,\\\") and array ([3,4,5]) cannot be added\""],
    );
}

/// jq.test line 2361: `implode|explode`
#[test]
#[ignore]
fn gap_string_ops_line2361_implode_explode() {
    assert_gap(
        "implode|explode",
        "[-1,0,1,2,3,1114111,1114112,55295,55296,57343,57344,1.1,1.9]",
        &["[65533,0,1,2,3,1114111,65533,55295,65533,65533,57344,1,1]"],
    );
}

/// jq.test line 2365: `map(try implode catch .)`
#[test]
#[ignore]
fn gap_string_ops_line2365_map_try_implode_catch() {
    assert_gap(
        "map(try implode catch .)",
        "[123,[\"a\"],[nan]]",
        &[
            "[\"implode input must be an array\",\"string (\\\"a\\\") can't be imploded, unicode codepoint needs to be numeric\",\"number (null) can't be imploded, unicode codepoint needs to be numeric\"]",
        ],
    );
}

/// jq.test line 2369: `try 0[implode] catch .`
#[test]
#[ignore]
fn gap_string_ops_line2369_try_0_implode_catch() {
    assert_gap(
        "try 0[implode] catch .",
        "[]",
        &["\"Cannot index number with string \\\"\\\"\""],
    );
}

// ======================================================================
// Category: String/array index, rindex, indices with multiple args
// 4 test(s)
//
// Fix: Support multi-arg variants: `index(",","|")` finds first occurrence of either. Fix indices to handle overlapping matches correctly.
// ======================================================================

/// jq.test line 440: `[(index(",","|"), rindex(",","|")), indices(",","|")]`
#[test]
#[ignore]
fn gap_string_index_line440_index_rindex_indi() {
    assert_gap(
        "[(index(\",\",\"|\"), rindex(\",\",\"|\")), indices(\",\",\"|\")]",
        "\"a,b|c,d,e||f,g,h,|,|,i,j\"",
        &["[1,3,22,19,[1,5,7,12,14,16,18,20,22],[3,9,10,17,19]]"],
    );
}

/// jq.test line 1519: `[ index("aba"), rindex("aba"), indices("aba") ]`
#[test]
#[ignore]
fn gap_string_index_line1519_index_aba_rindex_aba_indices() {
    assert_gap(
        "[ index(\"aba\"), rindex(\"aba\"), indices(\"aba\") ]",
        "\"xababababax\"",
        &["[1,7,[1,3,5,7]]"],
    );
}

/// jq.test line 1547: `indices([1,2])`
#[test]
#[ignore]
fn gap_string_index_line1547_indices_1_2() {
    assert_gap("indices([1,2])", "[0,1,2,3,1,4,2,5,1,2,6,7]", &["[1,8]"]);
}

/// jq.test line 2110: `index("")`
#[test]
#[ignore]
fn gap_string_index_line2110_index() {
    assert_gap("index(\"\")", "\"\"", &["null"]);
}

// ======================================================================
// Category: Float index and slice edge cases
// 4 test(s)
//
// Fix: Handle float indices by truncating to integer (jq behavior): .[1.5] -> .[1]. Float slices: .[1.2:3.5] -> .[1:3]. Error on NaN index.
// ======================================================================

/// jq.test line 2393: `[range(10)] | .[1.2:3.5]`
#[test]
#[ignore]
fn gap_float_index_line2393_range_10_1_2_3_5() {
    assert_gap("[range(10)] | .[1.2:3.5]", "null", &["[1,2,3]"]);
}

/// jq.test line 2397: `[range(10)] | .[1.5:3.5]`
#[test]
#[ignore]
fn gap_float_index_line2397_range_10_1_5_3_5() {
    assert_gap("[range(10)] | .[1.5:3.5]", "null", &["[1,2,3]"]);
}

/// jq.test line 2401: `[range(10)] | .[1.7:3.5]`
#[test]
#[ignore]
fn gap_float_index_line2401_range_10_1_7_3_5() {
    assert_gap("[range(10)] | .[1.7:3.5]", "null", &["[1,2,3]"]);
}

/// jq.test line 2413: `[[range(10)] | .[1.1,1.5,1.7]]`
#[test]
#[ignore]
fn gap_float_index_line2413_range_10_1_1_1_5_1_7() {
    assert_gap("[[range(10)] | .[1.1,1.5,1.7]]", "null", &["[1,1,1]"]);
}

// ======================================================================
// Category: sort_by/group_by with multiple keys, min_by/max_by edge cases
// 2 test(s)
//
// Fix: Support `sort_by(.a, .b)` as multi-key sort. Fix min_by/max_by tie-breaking. Fix group_by with expression arguments.
// ======================================================================

/// jq.test line 1639: `(sort_by(.b) | sort_by(.a)), sort_by(.a, .b), sort_by(.b, .c), group_by(.b), gro...`
#[test]
#[ignore]
fn gap_sort_group_edge_cases_line1639_sort_by_b_sort_by_a_sort_by_a() {
    assert_gap(
        "(sort_by(.b) | sort_by(.a)), sort_by(.a, .b), sort_by(.b, .c), group_by(.b), group_by(.a + .b - .c == 2)",
        "[{\"a\": 1, \"b\": 4, \"c\": 14}, {\"a\": 4, \"b\": 1, \"c\": 3}, {\"a\": 1, \"b\": 4, \"c\": 3}, {\"a\": 0, \"b\": 2, \"c\": 43}]",
        &[
            "[{\"a\": 0, \"b\": 2, \"c\": 43}, {\"a\": 1, \"b\": 4, \"c\": 14}, {\"a\": 1, \"b\": 4, \"c\": 3}, {\"a\": 4, \"b\": 1, \"c\": 3}]",
            "[{\"a\": 0, \"b\": 2, \"c\": 43}, {\"a\": 1, \"b\": 4, \"c\": 14}, {\"a\": 1, \"b\": 4, \"c\": 3}, {\"a\": 4, \"b\": 1, \"c\": 3}]",
            "[{\"a\": 4, \"b\": 1, \"c\": 3}, {\"a\": 0, \"b\": 2, \"c\": 43}, {\"a\": 1, \"b\": 4, \"c\": 3}, {\"a\": 1, \"b\": 4, \"c\": 14}]",
            "[[{\"a\": 4, \"b\": 1, \"c\": 3}], [{\"a\": 0, \"b\": 2, \"c\": 43}], [{\"a\": 1, \"b\": 4, \"c\": 14}, {\"a\": 1, \"b\": 4, \"c\": 3}]]",
            "[[{\"a\": 1, \"b\": 4, \"c\": 14}, {\"a\": 0, \"b\": 2, \"c\": 43}], [{\"a\": 4, \"b\": 1, \"c\": 3}, {\"a\": 1, \"b\": 4, \"c\": 3}]]",
        ],
    );
}

/// jq.test line 1655: `[min, max, min_by(.[1]), max_by(.[1]), min_by(.[2]), max_by(.[2])]`
#[test]
#[ignore]
fn gap_sort_group_edge_cases_line1655_min_max_min_by_1_max_by_1_m() {
    assert_gap(
        "[min, max, min_by(.[1]), max_by(.[1]), min_by(.[2]), max_by(.[2])]",
        "[[4,2,\"a\"],[3,1,\"a\"],[2,4,\"a\"],[1,3,\"a\"]]",
        &["[[1,3,\"a\"],[4,2,\"a\"],[3,1,\"a\"],[2,4,\"a\"],[4,2,\"a\"],[1,3,\"a\"]]"],
    );
}

// ======================================================================
// Category: Flatten with multiple depth args / negative depth
// 1 test(s)
//
// Fix: Support `flatten(3,2,1)` as generator producing multiple results. Error on negative depth with appropriate message.
// ======================================================================

/// jq.test line 1773: `try flatten(-1) catch .`
#[test]
#[ignore]
fn gap_flatten_edge_cases_line1773_try_flatten_1_catch() {
    assert_gap(
        "try flatten(-1) catch .",
        "[0, [1], [[2]], [[[3]]]]",
        &["\"flatten depth must not be negative\""],
    );
}

// ======================================================================
// Category: Walk builtin edge cases
// 1 test(s)
//
// Fix: Fix walk to handle multiple filter arguments: `walk(f, g)` applies each.
// ======================================================================

/// jq.test line 2383: `[walk(.,1)]`
#[test]
#[ignore]
fn gap_walk_builtin_line2383_walk_1() {
    assert_gap("[walk(.,1)]", "{\"x\":0}", &["[{\"x\":0},1]"]);
}

// ======================================================================
// Category: Pick builtin
// 4 test(s)
//
// Fix: Implement `pick(pathexpr)` — constructs an object containing only the specified paths. Handle null input, nested paths, and generator paths like first/last.
// ======================================================================

/// jq.test line 1184: `pick(.a.b.c)`
#[test]
#[ignore]
fn gap_pick_builtin_line1184_pick_a_b_c() {
    assert_gap("pick(.a.b.c)", "null", &["{\"a\":{\"b\":{\"c\":null}}}"]);
}

/// jq.test line 1188: `pick(first)`
#[test]
#[ignore]
fn gap_pick_builtin_line1188_pick_first() {
    assert_gap("pick(first)", "[1,2]", &["[1]"]);
}

/// jq.test line 1192: `pick(first|first)`
#[test]
#[ignore]
fn gap_pick_builtin_line1192_pick_first_first() {
    assert_gap("pick(first|first)", "[[10,20],30]", &["[[10]]"]);
}

/// jq.test line 1197: `try pick(last) catch .`
#[test]
#[ignore]
fn gap_pick_builtin_line1197_try_pick_last_catch() {
    assert_gap(
        "try pick(last) catch .",
        "[1,2]",
        &["\"Out of bounds negative array index\""],
    );
}

// ======================================================================
// Category: Binary search builtin
// 2 test(s)
//
// Fix: Implement `bsearch(x)` — binary search for x in sorted array. Returns index if found, or -(insertion_point)-1 if not. Support multiple search args as generator.
// ======================================================================

/// jq.test line 1789: `bsearch(0,1,2,3,4)`
#[test]
#[ignore]
fn gap_bsearch_line1789_bsearch_0_1_2_3_4() {
    assert_gap(
        "bsearch(0,1,2,3,4)",
        "[1,2,3]",
        &["-1", "0", "1", "2", "-4"],
    );
}

/// jq.test line 1801: `try ["OK", bsearch(0)] catch ["KO",.]`
#[test]
#[ignore]
fn gap_bsearch_line1801_try_ok_bsearch_0_catch_ko() {
    assert_gap(
        "try [\"OK\", bsearch(0)] catch [\"KO\",.]",
        "\"aa\"",
        &["[\"KO\",\"string (\\\"aa\\\") cannot be searched from\"]"],
    );
}

// ======================================================================
// Category: Add builtin edge cases: add(expr), add with generators
// 3 test(s)
//
// Fix: Implement `add(expr)` as a generalized form that applies expr to generate values then sums them. Fix add to work with assignment context: `.sum = add(.arr[])`.
// ======================================================================

/// jq.test line 757: `[add(null), add(range(range(10))), add(empty), add(10,range(10))]`
#[test]
#[ignore]
fn gap_add_edge_cases_line757_add_null_add_range_range_10_ad() {
    assert_gap(
        "[add(null), add(range(range(10))), add(empty), add(10,range(10))]",
        "null",
        &["[null,120,null,55]"],
    );
}

/// jq.test line 762: `.sum = add(.arr[])`
#[test]
#[ignore]
fn gap_add_edge_cases_line762_sum_add_arr() {
    assert_gap(
        ".sum = add(.arr[])",
        "{\"arr\":[]}",
        &["{\"arr\":[],\"sum\":null}"],
    );
}

/// jq.test line 766: `add({(.[]):1}) | keys`
#[test]
#[ignore]
fn gap_add_edge_cases_line766_add_1_keys() {
    assert_gap(
        "add({(.[]):1}) | keys",
        "[\"a\",\"a\",\"b\",\"a\",\"d\",\"b\",\"d\",\"a\",\"d\"]",
        &["[\"a\",\"b\",\"d\"]"],
    );
}

// ======================================================================
// Category: Missing builtins: toboolean, etc.
// 2 test(s)
//
// Fix: Implement `toboolean` — converts strings "true"/"false" to bools, passes bools through, errors on other types.
// ======================================================================

/// jq.test line 701: `map(toboolean)`
#[test]
#[ignore]
fn gap_missing_builtins_line701_map_toboolean() {
    assert_gap(
        "map(toboolean)",
        "[\"false\",\"true\",false,true]",
        &["[false,true,false,true]"],
    );
}

/// jq.test line 705: `.[] | try toboolean catch .`
#[test]
#[ignore]
fn gap_missing_builtins_line705_try_toboolean_catch() {
    assert_gap(
        ".[] | try toboolean catch .",
        "[null,0,\"tru\",\"truee\",\"fals\",\"falsee\",[],{}]",
        &[
            "\"null (null) cannot be parsed as a boolean\"",
            "\"number (0) cannot be parsed as a boolean\"",
            "\"string (\\\"tru\\\") cannot be parsed as a boolean\"",
            "\"string (\\\"truee\\\") cannot be parsed as a boolean\"",
            "\"string (\\\"fals\\\") cannot be parsed as a boolean\"",
            "\"string (\\\"falsee\\\") cannot be parsed as a boolean\"",
            "\"array ([]) cannot be parsed as a boolean\"",
            "\"object ({}) cannot be parsed as a boolean\"",
        ],
    );
}

// ======================================================================
// Category: Builtins list introspection
// 1 test(s)
//
// Fix: Implement `builtins` builtin that returns list of all available builtin names with arities as "name/arity".
// ======================================================================

/// jq.test line 2123: `all(builtins[] / "/"; .[1]|tonumber >= 0)`
#[test]
#[ignore]
fn gap_builtins_list_line2123_all_builtins_1_tonumber_0() {
    assert_gap(
        "all(builtins[] / \"/\"; .[1]|tonumber >= 0)",
        "null",
        &["true"],
    );
}

// ======================================================================
// Category: SQL-style operators: INDEX, JOIN, IN
// 4 test(s)
//
// Fix: Implement INDEX(stream; idx_expr), JOIN(idx; key_expr), IN(stream; test). These are higher-order builtins that build/query lookup tables.
// ======================================================================

/// jq.test line 2047: `INDEX(range(5)|[., "foo\(.)"]; .[0])`
#[test]
#[ignore]
fn gap_sql_style_ops_line2047_index_range_5_foo_0() {
    assert_gap(
        "INDEX(range(5)|[., \"foo\\(.)\"]; .[0])",
        "null",
        &[
            "{\"0\":[0,\"foo0\"],\"1\":[1,\"foo1\"],\"2\":[2,\"foo2\"],\"3\":[3,\"foo3\"],\"4\":[4,\"foo4\"]}",
        ],
    );
}

/// jq.test line 2051: `JOIN({"0":[0,"abc"],"1":[1,"bcd"],"2":[2,"def"],"3":[3,"efg"],"4":[4,"fgh"]}; .[...`
#[test]
#[ignore]
fn gap_sql_style_ops_line2051_join_0_0_abc_1_1_bcd_2_2() {
    assert_gap(
        "JOIN({\"0\":[0,\"abc\"],\"1\":[1,\"bcd\"],\"2\":[2,\"def\"],\"3\":[3,\"efg\"],\"4\":[4,\"fgh\"]}; .[0]|tostring)",
        "[[5,\"foo\"],[3,\"bar\"],[1,\"foobar\"]]",
        &["[[[5,\"foo\"],null],[[3,\"bar\"],[3,\"efg\"]],[[1,\"foobar\"],[1,\"bcd\"]]]"],
    );
}

/// jq.test line 2079: `IN(range(10;20); range(10))`
#[test]
#[ignore]
fn gap_sql_style_ops_line2079_in_range_10_20_range_10() {
    assert_gap("IN(range(10;20); range(10))", "null", &["false"]);
}

/// jq.test line 2083: `IN(range(5;20); range(10))`
#[test]
#[ignore]
fn gap_sql_style_ops_line2083_in_range_5_20_range_10() {
    assert_gap("IN(range(5;20); range(10))", "null", &["true"]);
}

// ======================================================================
// Category: Values builtin edge cases
// 1 test(s)
//
// Fix: Fix `values` to filter out null entries from arrays (jq behavior). Currently may be including nulls.
// ======================================================================

/// jq.test line 1745: `[.[]|values]`
#[test]
#[ignore]
fn gap_values_edge_cases_line1745_values() {
    assert_gap(
        "[.[]|values]",
        "[1,2,\"foo\",[],[3,[]],{},true,false,null]",
        &["[1,2,\"foo\",[],[3,[]],{},true,false]"],
    );
}

// ======================================================================
// Category: utf8bytelength type error handling
// 1 test(s)
//
// Fix: Fix utf8bytelength to produce catchable errors on non-string input (arrays, objects, etc.) rather than crashing or returning wrong results.
// ======================================================================

/// jq.test line 736: `[.[] | try utf8bytelength catch .]`
#[test]
#[ignore]
fn gap_utf8bytelength_edge_cases_line736_try_utf8bytelength_catch() {
    assert_gap(
        "[.[] | try utf8bytelength catch .]",
        "[[], {}, [1,2], 55, true, false]",
        &[
            "[\"array ([]) only strings have UTF-8 byte length\",\"object ({}) only strings have UTF-8 byte length\",\"array ([1,2]) only strings have UTF-8 byte length\",\"number (55) only strings have UTF-8 byte length\",\"boolean (true) only strings have UTF-8 byte length\",\"boolean (false) only strings have UTF-8 byte length\"]",
        ],
    );
}

// ======================================================================
// Category: fromjson edge cases
// 4 test(s)
//
// Fix: Fix fromjson to handle NaN strings, reject single-quoted JSON. Match jq error messages for invalid input.
// ======================================================================

/// jq.test line 2273: `fromjson | isnan`
#[test]
#[ignore]
fn gap_fromjson_edge_cases_line2273_fromjson_isnan() {
    assert_gap("fromjson | isnan", "\"nan\"", &["true"]);
}

/// jq.test line 2277: `tojson | fromjson`
#[test]
#[ignore]
fn gap_fromjson_edge_cases_line2277_tojson_fromjson() {
    assert_gap("tojson | fromjson", "{\"a\":nan}", &["{\"a\":null}"]);
}

/// jq.test line 2282: `.[] | try (fromjson | isnan) catch .`
#[test]
#[ignore]
fn gap_fromjson_edge_cases_line2282_try_fromjson_isnan_catch() {
    assert_gap(
        ".[] | try (fromjson | isnan) catch .",
        "[\"NaN\",\"-NaN\",\"NaN1\",\"NaN10\",\"NaN100\",\"NaN1000\",\"NaN10000\",\"NaN100000\"]",
        &[
            "true",
            "true",
            "\"Invalid numeric literal at EOF at line 1, column 4 (while parsing 'NaN1')\"",
            "\"Invalid numeric literal at EOF at line 1, column 5 (while parsing 'NaN10')\"",
            "\"Invalid numeric literal at EOF at line 1, column 6 (while parsing 'NaN100')\"",
            "\"Invalid numeric literal at EOF at line 1, column 7 (while parsing 'NaN1000')\"",
            "\"Invalid numeric literal at EOF at line 1, column 8 (while parsing 'NaN10000')\"",
            "\"Invalid numeric literal at EOF at line 1, column 9 (while parsing 'NaN100000')\"",
        ],
    );
}

/// jq.test line 2456: `try fromjson catch .`
#[test]
#[ignore]
fn gap_fromjson_edge_cases_line2456_try_fromjson_catch() {
    assert_gap(
        "try fromjson catch .",
        "\"{'a': 123}\"",
        &[
            "\"Invalid string literal; expected \\\", but got ' at line 1, column 5 (while parsing '{'a': 123}')\"",
        ],
    );
}

// ======================================================================
// Category: NaN and Infinity edge cases
// 6 test(s)
//
// Fix: Handle NaN/Infinity in: arithmetic (inf % n), string operations (* nan), index/slice with nan, and .[] = 1 on arrays containing special float values.
// ======================================================================

/// jq.test line 689: `[(infinite, -infinite) % (1, -1, infinite)]`
#[test]
#[ignore]
fn gap_nan_handling_line689_infinite_infinite_1_1_infinit() {
    assert_gap(
        "[(infinite, -infinite) % (1, -1, infinite)]",
        "null",
        &["[0,0,0,0,0,-1]"],
    );
}

/// jq.test line 1289: `.[] = 1`
#[test]
#[ignore]
fn gap_nan_handling_line1289_1() {
    assert_gap(
        ".[] = 1",
        "[1,null,Infinity,-Infinity,NaN,-NaN]",
        &["[1,1,1,1,1,1]"],
    );
}

/// jq.test line 1591: `[. * (nan,-nan)]`
#[test]
#[ignore]
fn gap_nan_handling_line1591_nan_nan() {
    assert_gap("[. * (nan,-nan)]", "\"abc\"", &["[null,null]"]);
}

/// jq.test line 2425: `[range(3)] | .[1:nan]`
#[test]
#[ignore]
fn gap_nan_handling_line2425_range_3_1_nan() {
    assert_gap("[range(3)] | .[1:nan]", "null", &["[1,2]"]);
}

/// jq.test line 2429: `[range(3)] | .[nan]`
#[test]
#[ignore]
fn gap_nan_handling_line2429_range_3_nan() {
    assert_gap("[range(3)] | .[nan]", "null", &["null"]);
}

/// jq.test line 2433: `try ([range(3)] | .[nan] = 9) catch .`
#[test]
#[ignore]
fn gap_nan_handling_line2433_try_range_3_nan_9_catch() {
    assert_gap(
        "try ([range(3)] | .[nan] = 9) catch .",
        "null",
        &["\"Cannot set array element at NaN index\""],
    );
}

// ======================================================================
// Category: Alternative operator (//) edge cases
// 1 test(s)
//
// Fix: Fix `//` with array iteration: `.foo[] // .bar` should try each element. Fix `//=` (alternative assignment).
// ======================================================================

/// jq.test line 1353: `[.[] | [.foo[] // .bar]]`
#[test]
#[ignore]
fn gap_alternative_edge_cases_line1353_foo_bar() {
    assert_gap(
        "[.[] | [.foo[] // .bar]]",
        "[{\"foo\":[1,2], \"bar\": 42}, {\"foo\":[1], \"bar\": null}, {\"foo\":[null,false,3], \"bar\": 18}, {\"foo\":[], \"bar\":42}, {\"foo\": [null,false,null], \"bar\": 41}]",
        &["[[1,2], [1], [3], [42], [41]]"],
    );
}

// ======================================================================
// Category: Keywords usable as identifiers: $foreach, $and, {if:0, as:1}
// 4 test(s)
//
// Fix: Allow jq keywords (if, and, or, then, else, elif, end, as, def, reduce, foreach, try, catch, label, import, include, module) as object keys and after $ in variable names.
// ======================================================================

/// jq.test line 1482: `try error("\($__loc__)") catch .`
#[test]
#[ignore]
fn gap_keyword_identifiers_line1482_try_error_loc_catch() {
    assert_gap(
        "try error(\"\\($__loc__)\") catch .",
        "null",
        &["\"{\\\"file\\\":\\\"<top-level>\\\",\\\"line\\\":1}\""],
    );
}

/// jq.test line 2000: `{if:0,and:1,or:2,then:3,else:4,elif:5,end:6,as:7,def:8,reduce:9,foreach:10,try:1...`
#[test]
#[ignore]
fn gap_keyword_identifiers_line2000_if_0_and_1_or_2_then_3_else_4() {
    assert_gap(
        "{if:0,and:1,or:2,then:3,else:4,elif:5,end:6,as:7,def:8,reduce:9,foreach:10,try:11,catch:12,label:13,import:14,include:15,module:16}",
        "null",
        &[
            "{\"if\":0,\"and\":1,\"or\":2,\"then\":3,\"else\":4,\"elif\":5,\"end\":6,\"as\":7,\"def\":8,\"reduce\":9,\"foreach\":10,\"try\":11,\"catch\":12,\"label\":13,\"import\":14,\"include\":15,\"module\":16}",
        ],
    );
}

/// jq.test line 2251: `1 as $foreach | 2 as $and | 3 as $or | { $foreach, $and, $or, a }`
#[test]
#[ignore]
fn gap_keyword_identifiers_line2251_1_as_foreach_2_as_and_3_as_or() {
    assert_gap(
        "1 as $foreach | 2 as $and | 3 as $or | { $foreach, $and, $or, a }",
        "{\"a\":4,\"b\":5}",
        &["{\"foreach\":1,\"and\":2,\"or\":3,\"a\":4}"],
    );
}

/// jq.test line 2262: `{ a, $__loc__, c }`
#[test]
#[ignore]
fn gap_keyword_identifiers_line2262_a_loc_c() {
    assert_gap(
        "{ a, $__loc__, c }",
        "{\"a\":[1,2,3],\"b\":\"foo\",\"c\":{\"hi\":\"hey\"}}",
        &[
            "{\"a\":[1,2,3],\"__loc__\":{\"file\":\"<top-level>\",\"line\":1},\"c\":{\"hi\":\"hey\"}}",
        ],
    );
}

// ======================================================================
// Category: Time functions: strftime, strptime, mktime, gmtime, strflocaltime
// 12 test(s)
//
// Fix: Implement time builtins using libc or chrono. strftime/strptime use C format strings. mktime converts broken-down time array to epoch. gmtime converts epoch to array.
// ======================================================================

/// jq.test line 1805: `strftime("%Y-%m-%dT%H:%M:%SZ")`
#[test]
#[ignore]
fn gap_time_functions_line1805_strftime_y_m_dt_h_m_sz() {
    assert_gap(
        "strftime(\"%Y-%m-%dT%H:%M:%SZ\")",
        "[2015,2,5,23,51,47,4,63]",
        &["\"2015-03-05T23:51:47Z\""],
    );
}

/// jq.test line 1813: `strftime("%Y-%m-%dT%H:%M:%SZ")`
#[test]
#[ignore]
fn gap_time_functions_line1813_strftime_y_m_dt_h_m_sz() {
    assert_gap(
        "strftime(\"%Y-%m-%dT%H:%M:%SZ\")",
        "[2024,2,15]",
        &["\"2024-03-15T00:00:00Z\""],
    );
}

/// jq.test line 1817: `mktime`
#[test]
#[ignore]
fn gap_time_functions_line1817_mktime() {
    assert_gap("mktime", "[2024,8,21]", &["1726876800"]);
}

/// jq.test line 1821: `gmtime`
#[test]
#[ignore]
fn gap_time_functions_line1821_gmtime() {
    assert_gap("gmtime", "1425599507", &["[2015,2,5,23,51,47,4,63]"]);
}

/// jq.test line 1826: `try strftime("%Y-%m-%dT%H:%M:%SZ") catch .`
#[test]
#[ignore]
fn gap_time_functions_line1826_try_strftime_y_m_dt_h_m_sz_cat() {
    assert_gap(
        "try strftime(\"%Y-%m-%dT%H:%M:%SZ\") catch .",
        "[\"a\",1,2,3,4,5,6,7]",
        &["\"strftime/1 requires parsed datetime inputs\""],
    );
}

/// jq.test line 1830: `try strflocaltime("%Y-%m-%dT%H:%M:%SZ") catch .`
#[test]
#[ignore]
fn gap_time_functions_line1830_try_strflocaltime_y_m_dt_h_m_s() {
    assert_gap(
        "try strflocaltime(\"%Y-%m-%dT%H:%M:%SZ\") catch .",
        "[\"a\",1,2,3,4,5,6,7]",
        &["\"strflocaltime/1 requires parsed datetime inputs\""],
    );
}

/// jq.test line 1834: `try mktime catch .`
#[test]
#[ignore]
fn gap_time_functions_line1834_try_mktime_catch() {
    assert_gap(
        "try mktime catch .",
        "[\"a\",1,2,3,4,5,6,7]",
        &["\"mktime requires parsed datetime inputs\""],
    );
}

/// jq.test line 1839: `try ["OK", strftime([])] catch ["KO", .]`
#[test]
#[ignore]
fn gap_time_functions_line1839_try_ok_strftime_catch_ko() {
    assert_gap(
        "try [\"OK\", strftime([])] catch [\"KO\", .]",
        "0",
        &["[\"KO\",\"strftime/1 requires a string format\"]"],
    );
}

/// jq.test line 1843: `try ["OK", strflocaltime({})] catch ["KO", .]`
#[test]
#[ignore]
fn gap_time_functions_line1843_try_ok_strflocaltime_catch_ko() {
    assert_gap(
        "try [\"OK\", strflocaltime({})] catch [\"KO\", .]",
        "0",
        &["[\"KO\",\"strflocaltime/1 requires a string format\"]"],
    );
}

/// jq.test line 1847: `[strptime("%Y-%m-%dT%H:%M:%SZ")|(.,mktime)]`
#[test]
#[ignore]
fn gap_time_functions_line1847_strptime_y_m_dt_h_m_sz_mktim() {
    assert_gap(
        "[strptime(\"%Y-%m-%dT%H:%M:%SZ\")|(.,mktime)]",
        "\"2015-03-05T23:51:47Z\"",
        &["[[2015,2,5,23,51,47,4,63],1425599507]"],
    );
}

/// jq.test line 1851: `[strptime("%FT%T")|(.,mktime)]`
#[test]
#[ignore]
fn gap_time_functions_line1851_strptime_ft_t_mktime() {
    assert_gap(
        "[strptime(\"%FT%T\")|(.,mktime)]",
        "\"2025-06-07T08:09:10\"",
        &["[[2025,5,7,8,9,10,6,157],1749283750]"],
    );
}

/// jq.test line 1857: `last(range(365 * 67)|("1970-03-01T01:02:03Z"|strptime("%Y-%m-%dT%H:%M:%SZ")|mkti...`
#[test]
#[ignore]
fn gap_time_functions_line1857_last_range_365_67_1970_03_01t0() {
    assert_gap(
        "last(range(365 * 67)|(\"1970-03-01T01:02:03Z\"|strptime(\"%Y-%m-%dT%H:%M:%SZ\")|mktime) + (86400 * .)|strftime(\"%Y-%m-%dT%H:%M:%SZ\")|strptime(\"%Y-%m-%dT%H:%M:%SZ\"))",
        "null",
        &["[2037,1,11,1,2,3,3,41]"],
    );
}

// ======================================================================
// Category: Module system: import, include, modulemeta
// 12 test(s)
//
// Fix: Implement module loading: import/include parse paths, load .jq files, bind definitions. modulemeta returns module metadata. This is a large feature requiring file I/O in the evaluator.
// ======================================================================

/// jq.test line 1862: `import "a" as foo; import "b" as bar; def fooa: foo::a; [fooa, bar::a, bar::b, f...`
#[test]
#[ignore]
fn gap_module_system_line1862_import_a_as_foo_import_b_as_ba() {
    assert_gap(
        "import \"a\" as foo; import \"b\" as bar; def fooa: foo::a; [fooa, bar::a, bar::b, foo::a]",
        "null",
        &["[\"a\",\"b\",\"c\",\"a\"]"],
    );
}

/// jq.test line 1866: `import "c" as foo; [foo::a, foo::c]`
#[test]
#[ignore]
fn gap_module_system_line1866_import_c_as_foo_foo_a_foo_c() {
    assert_gap(
        "import \"c\" as foo; [foo::a, foo::c]",
        "null",
        &["[0,\"acmehbah\"]"],
    );
}

/// jq.test line 1870: `include "c"; [a, c]`
#[test]
#[ignore]
fn gap_module_system_line1870_include_c_a_c() {
    assert_gap("include \"c\"; [a, c]", "null", &["[0,\"acmehbah\"]"]);
}

/// jq.test line 1874: `import "data" as $e; import "data" as $d; [$d[].this,$e[].that,$d::d[].this,$e::...`
#[test]
#[ignore]
fn gap_module_system_line1874_import_data_as_e_import_data_a() {
    assert_gap(
        "import \"data\" as $e; import \"data\" as $d; [$d[].this,$e[].that,$d::d[].this,$e::e[].that]|join(\";\")",
        "null",
        &["\"is a test;is too;is a test;is too\""],
    );
}

/// jq.test line 1879: `import "data" as $a; import "data" as $b; def f: {$a, $b}; f`
#[test]
#[ignore]
fn gap_module_system_line1879_import_data_as_a_import_data_a() {
    assert_gap(
        "import \"data\" as $a; import \"data\" as $b; def f: {$a, $b}; f",
        "null",
        &[
            "{\"a\":[{\"this\":\"is a test\",\"that\":\"is too\"}],\"b\":[{\"this\":\"is a test\",\"that\":\"is too\"}]}",
        ],
    );
}

/// jq.test line 1883: `include "shadow1"; e`
#[test]
#[ignore]
fn gap_module_system_line1883_include_shadow1_e() {
    assert_gap("include \"shadow1\"; e", "null", &["2"]);
}

/// jq.test line 1887: `include "shadow1"; include "shadow2"; e`
#[test]
#[ignore]
fn gap_module_system_line1887_include_shadow1_include_shadow() {
    assert_gap(
        "include \"shadow1\"; include \"shadow2\"; e",
        "null",
        &["3"],
    );
}

/// jq.test line 1891: `import "shadow1" as f; import "shadow2" as f; import "shadow1" as e; [e::e, f::e...`
#[test]
#[ignore]
fn gap_module_system_line1891_import_shadow1_as_f_import_sha() {
    assert_gap(
        "import \"shadow1\" as f; import \"shadow2\" as f; import \"shadow1\" as e; [e::e, f::e]",
        "null",
        &["[2,3]"],
    );
}

/// jq.test line 1931: `modulemeta`
#[test]
#[ignore]
fn gap_module_system_line1931_modulemeta() {
    assert_gap(
        "modulemeta",
        "\"c\"",
        &[
            "{\"whatever\":null,\"deps\":[{\"as\":\"foo\",\"is_data\":false,\"relpath\":\"a\"},{\"search\":\"./\",\"as\":\"d\",\"is_data\":false,\"relpath\":\"d\"},{\"search\":\"./\",\"as\":\"d2\",\"is_data\":false,\"relpath\":\"d\"},{\"search\":\"./../lib/jq\",\"as\":\"e\",\"is_data\":false,\"relpath\":\"e\"},{\"search\":\"./../lib/jq\",\"as\":\"f\",\"is_data\":false,\"relpath\":\"f\"},{\"as\":\"d\",\"is_data\":true,\"relpath\":\"data\"}],\"defs\":[\"a/0\",\"c/0\"]}",
        ],
    );
}

/// jq.test line 1935: `modulemeta | .deps | length`
#[test]
#[ignore]
fn gap_module_system_line1935_modulemeta_deps_length() {
    assert_gap("modulemeta | .deps | length", "\"c\"", &["6"]);
}

/// jq.test line 1939: `modulemeta | .defs | length`
#[test]
#[ignore]
fn gap_module_system_line1939_modulemeta_defs_length() {
    assert_gap("modulemeta | .defs | length", "\"c\"", &["2"]);
}

/// jq.test line 1955: `import "test_bind_order" as check; check::check`
#[test]
#[ignore]
fn gap_module_system_line1955_import_test_bind_order_as_chec() {
    assert_gap(
        "import \"test_bind_order\" as check; check::check",
        "null",
        &["true"],
    );
}

// ======================================================================
// Category: Big number / arbitrary precision (have_decnum)
// 13 test(s)
//
// Fix: jx uses i64/f64; jq with decnum uses arbitrary precision. Most of these tests check `have_decnum` conditionals. Consider implementing the non-decnum branch or skipping.
// ======================================================================

/// jq.test line 661: `9E999999999, 9999999999E999999990, 1E-999999999, 0.000000001E-999999990`
#[test]
#[ignore]
fn gap_bignum_line661_9e999999999_9999999999e9999999() {
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

/// jq.test line 2154: `.[0] | tostring | . == if have_decnum then "13911860366432393" else "13911860366...`
#[test]
#[ignore]
fn gap_bignum_line2154_0_tostring_if_have_decnum_th() {
    assert_gap(
        ".[0] | tostring | . == if have_decnum then \"13911860366432393\" else \"13911860366432392\" end",
        "[13911860366432393]",
        &["true"],
    );
}

/// jq.test line 2158: `.x | tojson | . == if have_decnum then "13911860366432393" else "139118603664323...`
#[test]
#[ignore]
fn gap_bignum_line2158_x_tojson_if_have_decnum_then() {
    assert_gap(
        ".x | tojson | . == if have_decnum then \"13911860366432393\" else \"13911860366432392\" end",
        "{\"x\":13911860366432393}",
        &["true"],
    );
}

/// jq.test line 2162: `(13911860366432393 == 13911860366432392) | . == if have_decnum then false else t...`
#[test]
#[ignore]
fn gap_bignum_line2162_13911860366432393_139118603664() {
    assert_gap(
        "(13911860366432393 == 13911860366432392) | . == if have_decnum then false else true end",
        "null",
        &["true"],
    );
}

/// jq.test line 2169: `. - 10`
#[test]
#[ignore]
fn gap_bignum_line2169_10() {
    assert_gap(". - 10", "13911860366432393", &["13911860366432382"]);
}

/// jq.test line 2173: `.[0] - 10`
#[test]
#[ignore]
fn gap_bignum_line2173_0_10() {
    assert_gap(".[0] - 10", "[13911860366432393]", &["13911860366432382"]);
}

/// jq.test line 2177: `.x - 10`
#[test]
#[ignore]
fn gap_bignum_line2177_x_10() {
    assert_gap(
        ".x - 10",
        "{\"x\":13911860366432393}",
        &["13911860366432382"],
    );
}

/// jq.test line 2182: `-. | tojson == if have_decnum then "-13911860366432393" else "-13911860366432392...`
#[test]
#[ignore]
fn gap_bignum_line2182_tojson_if_have_decnum_then_139() {
    assert_gap(
        "-. | tojson == if have_decnum then \"-13911860366432393\" else \"-13911860366432392\" end",
        "13911860366432393",
        &["true"],
    );
}

/// jq.test line 2186: `-. | tojson == if have_decnum then "0.12345678901234567890123456789" else "0.123...`
#[test]
#[ignore]
fn gap_bignum_line2186_tojson_if_have_decnum_then_0_1() {
    assert_gap(
        "-. | tojson == if have_decnum then \"0.12345678901234567890123456789\" else \"0.12345678901234568\" end",
        "-0.12345678901234567890123456789",
        &["true"],
    );
}

/// jq.test line 2190: `[1E+1000,-1E+1000 | tojson] == if have_decnum then ["1E+1000","-1E+1000"] else [...`
#[test]
#[ignore]
fn gap_bignum_line2190_1e_1000_1e_1000_tojson_if_have() {
    assert_gap(
        "[1E+1000,-1E+1000 | tojson] == if have_decnum then [\"1E+1000\",\"-1E+1000\"] else [\"1.7976931348623157e+308\",\"-1.7976931348623157e+308\"] end",
        "null",
        &["true"],
    );
}

/// jq.test line 2199: `.[] as $n | $n+0 | [., tostring, . == $n]`
#[test]
#[ignore]
fn gap_bignum_line2199_as_n_n_0_tostring_n() {
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

/// jq.test line 2229: `[1E+1000,-1E+1000 | abs | tojson] | unique == if have_decnum then ["1E+1000"] el...`
#[test]
#[ignore]
fn gap_bignum_line2229_1e_1000_1e_1000_abs_tojson_uni() {
    assert_gap(
        "[1E+1000,-1E+1000 | abs | tojson] | unique == if have_decnum then [\"1E+1000\"] else [\"1.7976931348623157e+308\"] end",
        "null",
        &["true"],
    );
}

/// jq.test line 2233: `[1E+1000,-1E+1000 | length | tojson] | unique == if have_decnum then ["1E+1000"]...`
#[test]
#[ignore]
fn gap_bignum_line2233_1e_1000_1e_1000_length_tojson() {
    assert_gap(
        "[1E+1000,-1E+1000 | length | tojson] | unique == if have_decnum then [\"1E+1000\"] else [\"1.7976931348623157e+308\"] end",
        "null",
        &["true"],
    );
}

// ======================================================================
// Category: Input/inputs builtins
// 1 test(s)
//
// Fix: Implement `input` (read next JSON from stdin) and `inputs` (read all remaining). Requires multi-value input stream support in the evaluator.
// ======================================================================

/// jq.test line 2295: `try input catch .`
#[test]
#[ignore]
fn gap_input_builtin_line2295_try_input_catch() {
    assert_gap("try input catch .", "null", &["\"break\""]);
}

// ======================================================================
// Category: Miscellaneous conformance gaps
// 12 test(s)
//
// Fix: Various edge cases that do not fit neatly into other categories.
// ======================================================================

/// jq.test line 195: `[.[]|.[1:3]?]`
#[test]
#[ignore]
fn gap_misc_line195_1_3() {
    assert_gap(
        "[.[]|.[1:3]?]",
        "[1,null,true,false,\"abcdef\",{},{\"a\":1,\"b\":2},[],[1,2,3,4,5],[1,2]]",
        &["[null,\"bc\",[],[2,3],[2]]"],
    );
}

/// jq.test line 906: `[reduce .[] / .[] as $i (0; . + $i)]`
#[test]
#[ignore]
fn gap_misc_line906_reduce_as_i_0_i() {
    assert_gap("[reduce .[] / .[] as $i (0; . + $i)]", "[1,2]", &["[4.5]"]);
}

/// jq.test line 1040: `. as $dot|any($dot[];not)`
#[test]
#[ignore]
fn gap_misc_line1040_as_dot_any_dot_not() {
    assert_gap(". as $dot|any($dot[];not)", "[1,2,3,4,true]", &["false"]);
}

/// jq.test line 1044: `. as $dot|all($dot[];.)`
#[test]
#[ignore]
fn gap_misc_line1044_as_dot_all_dot() {
    assert_gap(
        ". as $dot|all($dot[];.)",
        "[1,2,3,4,true,false,1,2,3,4,5]",
        &["false"],
    );
}

/// jq.test line 1053: `any(true, error; .)`
#[test]
#[ignore]
fn gap_misc_line1053_any_true_error() {
    assert_gap("any(true, error; .)", "\"badness\"", &["true"]);
}

/// jq.test line 1057: `all(false, error; .)`
#[test]
#[ignore]
fn gap_misc_line1057_all_false_error() {
    assert_gap("all(false, error; .)", "\"badness\"", &["false"]);
}

/// jq.test line 1663: `.foo[.baz]`
#[test]
#[ignore]
fn gap_misc_line1663_foo_baz() {
    assert_gap(
        ".foo[.baz]",
        "{\"foo\":{\"bar\":4},\"baz\":\"bar\"}",
        &["4"],
    );
}

/// jq.test line 1703: `[][.]`
#[test]
#[ignore]
fn gap_misc_line1703_() {
    assert_gap("[][.]", "1000000000000000000", &["null"]);
}

/// jq.test line 1707: `map([1,2][0:.])`
#[test]
#[ignore]
fn gap_misc_line1707_map_1_2_0() {
    assert_gap(
        "map([1,2][0:.])",
        "[-1, 1, 2, 3, 1000000000000000000]",
        &["[[1], [1], [1,2], [1,2], [1,2]]"],
    );
}

/// jq.test line 2088: `(.a as $x | .b) = "b"`
#[test]
#[ignore]
fn gap_misc_line2088_a_as_x_b_b() {
    assert_gap(
        "(.a as $x | .b) = \"b\"",
        "{\"a\":null,\"b\":null}",
        &["{\"a\":null,\"b\":\"b\"}"],
    );
}

/// jq.test line 2266: `1 as $x | "2" as $y | "3" as $z | { $x, as, $y: 4, ($z): 5, if: 6, foo: 7 }`
#[test]
#[ignore]
fn gap_misc_line2266_1_as_x_2_as_y_3_as_z_x() {
    assert_gap(
        "1 as $x | \"2\" as $y | \"3\" as $z | { $x, as, $y: 4, ($z): 5, if: 6, foo: 7 }",
        "{\"as\":8}",
        &["{\"x\":1,\"as\":8,\"2\":4,\"3\":5,\"if\":6,\"foo\":7}"],
    );
}

/// jq.test line 2353: `any(keys[]|tostring?;true)`
#[test]
#[ignore]
fn gap_misc_line2353_any_keys_tostring_true() {
    assert_gap(
        "any(keys[]|tostring?;true)",
        "{\"a\":\"1\",\"b\":\"2\",\"c\":\"3\"}",
        &["true"],
    );
}
