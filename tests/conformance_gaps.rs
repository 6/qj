/// Conformance gap tests — 43 jq.test cases that qj currently fails.
///
/// Auto-generated from jq.test failure analysis. Each test documents
/// a specific gap with category and fix suggestion. Run individual tests
/// or categories as features are implemented:
///
///   cargo test --release conformance_gaps -- --ignored              # all gaps
///   cargo test --release gap_label_break -- --ignored               # one category
///   cargo test --release gap_foreach_line353 -- --ignored            # one test
///
/// As each gap is fixed, remove the test (it will be covered by jq_conformance).
mod common;

/// Run qj with a filter and input, return stdout lines.
fn run_jx(filter: &str, input: &str) -> Vec<String> {
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
// Category: Path expression edge cases
// 5 test(s)
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

// ======================================================================
// Category: Assignment operator edge cases
// 3 test(s)
//
// Fix: Fix compound assignment (.[] +=, -=, *=, /=, %=) to work with array iteration. Fix .foo += .foo to not double-evaluate. Handle update-assignment with def-based paths and getpath-based update assignments.
// ======================================================================

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
// 1 test(s)
//
// Fix: Fix join to produce errors on non-string elements. Fix trimstr to only trim prefixes/suffixes (not substrings). Fix implode/explode for edge cases. String multiplication: handle negative/float multipliers and error on too-large values.
// ======================================================================

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

// ======================================================================
// Category: Pick builtin
// 1 test(s)
//
// Fix: Implement `pick(pathexpr)` — constructs an object containing only the specified paths. Handle null input, nested paths, and generator paths like first/last.
// ======================================================================

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
// Category: fromjson edge cases
// 1 test(s)
//
// Fix: Fix fromjson to handle NaN strings, reject single-quoted JSON. Match jq error messages for invalid input.
// ======================================================================

/// jq.test line 2277: `tojson | fromjson`
#[test]
#[ignore]
fn gap_fromjson_edge_cases_line2277_tojson_fromjson() {
    assert_gap("tojson | fromjson", "{\"a\":nan}", &["{\"a\":null}"]);
}

// ======================================================================
// Category: NaN and Infinity edge cases
// 2 test(s)
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
// 12 test(s)
//
// Fix: qj uses i64/f64; jq with decnum uses arbitrary precision. Most of these tests check `have_decnum` conditionals. Consider implementing the non-decnum branch or skipping.
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
// 1 test(s)
//
// Fix: Various edge cases that do not fit neatly into other categories.
// ======================================================================

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
