//! The optimized code (regions and frameless calls, `region.rs`) against jq's
//! instructions as they are, in process: every program runs twice, optimized and not,
//! and everything observable must be the same: outputs, the `debug`/`stderr` stream,
//! errors, halts and exit codes. Programs come from hand-written edge cases and from
//! the bounded generator in `opt_cases.rs` (the out-of-process test,
//! `tests/vm_opt_diff.rs`, runs the unbounded one).

use super::driver::{Options, Output, run};
use super::opt_cases::Gen;
use super::region::{DIRECT_RUNS, REGION_RUNS};

fn run_with(program: &str, input: &str, optimize: bool, natives: bool) -> Output {
    let opts = Options {
        optimize,
        natives,
        ..Options::default()
    };
    run(program, input.as_bytes(), &opts)
}

/// What a run shows, as text.
fn show(o: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}\nerror: {:?}\ncompile: {:?}\nparse: {:?}\nhalted: {:?}\nexit: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
        o.error,
        o.compile_error,
        o.parse_error,
        o.halted,
        o.exit
    )
}

/// Runs `program` optimized and not; panics with both results if they differ.
fn check(program: &str, input: &str, natives: bool) {
    let opt = show(&run_with(program, input, true, natives));
    let orig = show(&run_with(program, input, false, natives));
    assert_eq!(
        opt, orig,
        "optimized vs original differ (natives {natives})\nprogram: {program}\ninput: {input}"
    );
}

/// Edge cases, each checked on every input of [`INPUTS`], with and without natives.
const PROGRAMS: &[&str] = &[
    // arithmetic and comparisons, numbers and not
    ". + 1",
    ". - 1",
    "1 - .",
    ". * 2",
    ". / 2",
    ". % 3",
    ". % 0",
    ". / 0",
    "-.",
    ". == 1",
    ". != null",
    ". < 1",
    ". <= 1",
    ". > 1",
    ". >= \"a\"",
    "nan < .",
    ". < nan",
    "[. < nan, nan > ., nan == nan, . == .]",
    ". + .",
    ". * .",
    "(. + [1]) | .[0:1] | .[3] = 9",
    "[., .] as [$a, $b] | $a + $b",
    ". as $x | $x + $x",
    ". as $x | [$x] + [$x]",
    "1.000 + 0, 1.000 + null, null + 1.000",
    "100000000000000000001 == 100000000000000000000",
    "100000000000000000001 < 100000000000000000002",
    "9007199254740993 == 9007199254740992",
    ". % 3 == 0",
    "(.a + 1) * 2 > 3",
    "(\"a\" * .)?",
    "\"abc\" * 0, \"abc\" * 1.5, \"abc\" * -1",
    // conditionals
    "if . then 1 else 2 end",
    "if . then 1 end",
    "if .a then .b elif .c then .d else . end",
    "if . == 1 then \"one\" elif . == 2 then \"two\" else empty end",
    ".a and .b",
    ".a or .b",
    "(. and true) or false",
    "not",
    ". | not | not",
    "if (.a | not) then 1 else 2 end",
    // construction
    "{a: ., b: [.]}",
    "{(tostring): .}",
    "{(.): 1}",
    "{a: 1, a: 2}",
    "{a} | .a",
    "\"x\\(.)y\\(. + 1)\"",
    "\"\\(.a)-\\(.b)\"",
    "@base64 \"\\(.)\"",
    "[., ., .]",
    "[.[]?]",
    // indexing
    ".a",
    ".a.b",
    ".[0]",
    ".[-1]",
    ".[1:]",
    ".[:1]",
    ".a?",
    ".[0]?",
    ".[.a]",
    ".[.a]?",
    ".a.b.c?",
    ".[\"a\"]",
    "try .a catch .",
    "try .[0] catch .",
    // variables, reduce, foreach
    ". as $x | . as $y | [$x, $y]",
    "reduce .[]? as $x (0; . + $x)",
    "reduce .[]? as $x (null; . + $x)",
    "reduce .[]? as $x ([]; . + [$x])",
    "reduce .[]? as $x ({}; .[$x | tostring] = $x)",
    "reduce range(5) as $i (.; [.])",
    "reduce range(5) as $i (0; . + $i)",
    "reduce range(3) as $i ([]; . + [$i]) | .[0:1] | .[3] = 1",
    "[foreach .[]? as $x (0; . + 1; [$x, .])]",
    "[foreach range(5) as $i (null; $i; . * 2)]",
    "[limit(3; foreach range(10) as $i (0; . + $i))]",
    // generators and fork points around regions
    "[.[]? | . + 1]",
    "[.[]? | select(. != null)]",
    "[range(10) | select(. % 3 == 0)]",
    "[range(5) | . * 2 | tostring]",
    "[range(0; 10; 3)]",
    "[range(5; 0; -2)]",
    "[range(0; 1; 0.25)]",
    "[range(1.50; 4)]",
    "[limit(5; repeat(1))]",
    "[limit(5; repeat(. + 1))]",
    "first(range(10) | select(. > 3))",
    "[.[]? as $x | $x + 1]",
    "[.. | numbers | . + 1]",
    "(.. | numbers) |= . + 1",
    ".[]? |= . + 1",
    "map(. + 1)?",
    "map(select(. != null))?",
    "map_values(. // 0)?",
    "to_entries?",
    "with_entries(.value |= tostring)?",
    "walk(if type == \"number\" then . + 1 else . end)",
    "[paths]",
    "[paths(type == \"number\")]",
    "add?",
    "any",
    "all",
    "[.[]? | tostring] | join(\",\")",
    "sort_by(.a)?",
    "group_by(. % 2)?",
    "min_by(.)?",
    "unique_by(length)?",
    // try, labels, errors
    "try error(\"x\") catch .",
    "try (. + {}) catch .",
    "try error catch .",
    "[.[]? | try (. * {}) catch \"E\"]",
    "(try (1, error(\"x\"), 3) catch .)",
    "[label $f | range(5) | ., break $f]",
    "[label $f | .[]? | if . == 1 then break $f else . end]",
    "label $f | . + 1 | ., break $f",
    "[range(3) | label $x | . + 1]",
    "[label $l | try break $l catch .]",
    // paths
    "path(.a)",
    "path(.a.b)",
    "path(.[0])",
    "try path(. + 1) catch .",
    "try path(.a | . + 1) catch .",
    "try path(if .a then .b else .c end) catch .",
    "try path(1) catch .",
    "try path(.a // .b) catch .",
    "try path(getpath([\"a\"])) catch .",
    "path(.. | select(type == \"number\"))",
    "[paths(. == 1)]",
    "del(.a)?",
    "(.a, .b) |= . + 1",
    ".a += 1",
    ".a //= 3",
    ".a = .b",
    "try ((.a | . + 1) |= 2) catch .",
    // functions and closures
    "def f: . + 1; f",
    "def f: . + 1; [.[]? | f]",
    "def f: . + 1; def g: f | f; g",
    "def f(g): g | g; f(. + 1)",
    "def f($x): $x + .; f(2)",
    "def f: if . > 3 then . else . + 1 | f end; f?",
    "def f: .a; path(f)",
    "def f: .a; try path(f | . + 1) catch .",
    "def f: empty; [f]",
    "def f: error(\"e\"); try f catch .",
    "def f: $__loc__; f",
    "[.[]? | (def f: . * 2; f)]",
    "def f(x): x as $v | $v + 1; f(.)",
    // inlined calls of functions whose body is a region
    "def f: . + 1; def g: f * 2; [.[]? | g]",
    "def f: not; [.[]? | select(f)]",
    "[.[]? | select(. == 1 | not)]",
    ". as $x | def f: $x; [.[]? | f]",
    ". as $x | def f: [$x, .]; def g: . as $y | f; [.[]? | g]",
    "def f($a): def g: $a + .; g; f(1)?",
    "def f($a): def g: [$a, .]; [.[]? | g]; f(2)",
    "def f: def g: . + 1; g | g; f?",
    "def f: def g: .a?; g; [.[]? | f]",
    "reduce .[]? as $x (0; . + ($x | def f: . * 2; f)?)",
    "def f: .a; path(f)?",
    "def f: .[0]; try path(f | f) catch \"E\"",
    "def f: if . then .a else .b end; try path(f) catch \"E\"",
    "def f: . + 1; try path(f) catch \"E\"",
    "def f: getpath([\"a\"]); try path(f) catch \"E\"",
    "def f: input; [f]?",
    "def f: debug; [.[]? | f]",
    "def f: error; try f catch .",
    "def f: error({\"__jq\":0}); [first(f)]",
    "def f: $__loc__; [f, (1 | f)]",
    "def f: if . > 0 then . - 1 | f else . end; 3 | f",
    "def f: {a: ., b: (. | tostring)}; [.[]? | f]",
    "def f: \"<\\(.)>\"; [.[]? | f]",
    "def e: empty; [.[]? | e, 1]",
    "def f: .[]?; [f]",
    "def f: . as [$a] | $a; [.[]? | f?]",
    "def f: length?; [.[]? | select(f)]",
    "def z: 0; def f: . + z; def g: f + z; [.[]? | g?]",
    "def f: [., .][0:1]; [.[]? | f | .[3] = 1]",
    "[.[]? as $v | def f: $v + .; 10 | f?]",
    "def f: . == 1; any(.[]?; f)",
    "def f: tostring; map(f)?",
    "def f: . * 2; .[]? |= f",
    "def f: type == \"number\"; walk(if f then . + 1 else . end)",
    "def f: select(. != null); [.[]? | f]",
    // destructuring and ?//
    ". as [$a, $b] | {a: $a, b: $b}",
    ". as {a: $a} | $a",
    "[.[]? as [$a] ?// $a | $a]",
    "[.[]? as [$a] ?// $a | $a + 1]",
    "[[1], 2] | first(.[] as [$x] ?// $x | $x)",
    // side effects: order of debug/stderr/input
    "(. | debug) + 1",
    "[.[]? | debug | . + 1]",
    // (Caught: an error message right after `stderr` output would put qj's name
    // mid-line, where jq_diff doesn't rewrite it.)
    "try (debug | stderr | . + 1) catch \"E\"",
    "(input? // 0) + 1",
    "[.[]? | (input? // 0)]",
    "if . == 1 then halt_error else . end",
    "(\"bye\\n\" | halt_error(3)), 1",
    // labels allocated by natives and regions
    "first(.[]?) | [label $f | try break $f catch .]",
    "[limit(2; .[]?)] | [label $f | try break $f catch .]",
    // values kept alive by fork points
    "[., ., ., .][0:2] as $v | $v | . + [1] | $$$$v | .[3] = 9",
    "[., ., ., .][0:2] | reduce range(2) as $j (.; .[3] = 9)",
    "[., ., ., .][0:2] as $v | $v | (. | tostring) | $$$$v | .[3] = 9",
    "[1,2,3,4] | .[0:2] | . as $x | ($x | length) as $n | $x | .[3] = $n",
    "[range(4)+1] | .[0:2] | .[3] = 9",
    "[., .] | .[0] as $a | .[1] | try path($a) catch \"E\"",
    "(. + 0) as $a | (. + 0) | try path($a) catch \"E\"",
    ". as $a | . | try path($a) catch \"E\"",
    "[.[]? | {a: .}] | .[0] as $a | .[0] | try path($a) catch \"E\"",
];

/// Natives that evaluate direct closures (`select`, `map`, `repeat`: `native/direct.rs`),
/// and natives whose closures now run as direct regions, on containers.
const NATIVE_PROGRAMS: &[&str] = &[
    "[.[]? | select(. != null)]",
    "[.[]? | select(type == \"number\")]",
    "[.[]? | select(.a?)]",
    "[.[]? | try select(. + 1 > 1) catch \"E\"]",
    "[.[]? | select(., .)]",
    "[.[]? | select(debug)]",
    "[.[]? | select(input? // false)]",
    "select(true) as $a | select(true) | try path($a) catch \"E\"",
    "[., ., ., .][0:2] as $v | $v | select(true) | $$$$v | .[3] = 9",
    "map(. + 1)?",
    "map(.a?)?",
    "map(tostring)?",
    "try map(error) catch .",
    "try map(. * {}) catch .",
    "map(select(. != null))?",
    "map(debug)?",
    "map(.) as $a | map(.) | try path($a) catch \"E\"",
    "[] | map(.) as $a | [] | map(.) | path($a)",
    "[., ., ., .][0:2] | map(.) | .[3] = 9",
    "[.[]? | [.] | map(. + [1])? | .[0:1] | .[3] = 1]",
    "[limit(3; repeat(1))]",
    "[limit(3; repeat(. + 1))?]",
    "try [limit(3; repeat(error))] catch .",
    "[limit(3; repeat(input))]",
    "first(repeat(1)) | [label $f | try break $f catch .]",
    "[limit(2; repeat([.]))] | .[0] as $a | .[1] | try path($a) catch \"E\"",
    "[., ., ., .][0:2] as $v | $v | first(repeat(.)) | $$$$v | .[3] = 9",
    "[inputs]",
    "try [inputs, error(\"x\")] catch .",
    "walk(if type == \"number\" then . * 10 else . end)",
    "[paths(type == \"number\")]",
    ".[]? |= . + 1",
    "map_values(. // 0)?",
    "with_entries(.value |= tostring)?",
    "[.[]? | first(.a?)]",
    "any(.[]?; . == 1)",
    "sort_by(.a)?",
    "group_by(.)? | map(length)",
    "[limit(2; .[]? | select(. != null))]",
    // `.[] |=` over views: copies and in-place writes as in jq.
    "[., ., ., .][0:2] | (.[] |= 1) | .[3] = 9",
    "[., ., ., .][0:2] as $v | $v | (.[] |= 1) | $$$$v | .[3] = 9",
    "[range(4)] | .[0:2] | (.[] |= . + 1) | .[3] = 9",
    "[[1,2,3,4][0:2]] | (.[] |= (.[0:1])) | .[0][3] = 9",
    "{a: [1,2,3,4][0:2]} | (.[] |= .) | .a[3] = 9",
    // An error equal to a native's own label is swallowed by the label's handler.
    "[first(error({\"__jq\":0}))]",
    "[first(1, error({\"__jq\":0}))]",
    "[isempty(error({\"__jq\":0}))]",
    "[any(.[]?; error({\"__jq\":0}))]",
    "[all(error({\"__jq\":0}); .)]",
    "[IN(.[]?; error({\"__jq\":0}))]",
    "[limit(3; .[]?, error({\"__jq\":0}))]",
    "[limit(3; .[]?, error({\"__jq\":1}))]",
    ".a |= error({\"__jq\":0})",
    "(.a, .b) |= (if . == null then error({\"__jq\":0}) else empty end)",
    ".[]? |= (if . == 1 then error({\"__jq\":1}) else . end)",
    "(.. | numbers) |= error({\"__jq\":0})",
    "walk(if type == \"number\" then error({\"__jq\":0}) else . end)",
    "walk(if type == \"number\" then error({\"__jq\":1}) else . end)",
    "walk(if . == 1 then error({\"__jq\":0}) elif . == null then empty else . end)",
    "walk(if type == \"object\" then error({\"__jq\":1}) else . end)",
];

const INPUTS: &[&str] = &[
    "null",
    "1",
    "2.5",
    "\"a\"",
    "[1,2,3]",
    "[[1],[2]]",
    "{\"a\":1,\"b\":2}",
    "{\"a\":{\"b\":{\"c\":1}},\"c\":true}",
    "[{\"a\":2},{\"a\":1}]",
    "1.000",
    "100000000000000000001",
    "[null,false,0,\"\"]",
];

#[test]
fn optimized_matches_original_on_edge_cases() {
    let (regions, direct) = (REGION_RUNS.with(|c| c.get()), DIRECT_RUNS.with(|c| c.get()));
    for program in PROGRAMS.iter().chain(NATIVE_PROGRAMS) {
        for input in INPUTS {
            check(program, input, true);
            check(program, input, false);
        }
    }
    // Both kinds of optimized code really ran.
    assert!(REGION_RUNS.with(|c| c.get()) > regions + 1000);
    assert!(DIRECT_RUNS.with(|c| c.get()) > direct + 1000);
}

#[test]
fn optimized_matches_original_on_generated_programs() {
    let seed: u64 = std::env::var("QJ_OPT_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);
    let n: usize = std::env::var("QJ_OPT_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500);
    let mut g = Gen::bounded(seed);
    let (regions, direct) = (REGION_RUNS.with(|c| c.get()), DIRECT_RUNS.with(|c| c.get()));
    let (mut compiled, mut outputs) = (0, 0);
    for i in 0..n {
        let (program, input) = g.case();
        let natives = i % 2 == 0;
        let o = run_with(&program, &input, true, natives);
        compiled += o.compile_error.is_none() as usize;
        outputs += !o.stdout.is_empty() as usize;
        let orig = run_with(&program, &input, false, natives);
        assert_eq!(
            show(&o),
            show(&orig),
            "optimized vs original differ (natives {natives})\nprogram: {program}\ninput: {input}"
        );
    }
    if std::env::var_os("QJ_OPT_STATS").is_some() {
        eprintln!(
            "{n} cases: {compiled} compiled, {outputs} with output; {} regions and {} direct regions run",
            REGION_RUNS.with(|c| c.get()) - regions,
            DIRECT_RUNS.with(|c| c.get()) - direct
        );
    }
    // Nearly all programs compile and most produce something, and both kinds of
    // optimized code run a lot.
    assert!(compiled * 10 > n * 9, "{compiled} of {n} compiled");
    assert!(outputs * 2 > n, "{outputs} of {n} had output");
    assert!(REGION_RUNS.with(|c| c.get()) - regions > n as u64);
    assert!(DIRECT_RUNS.with(|c| c.get()) - direct > n as u64);
}

/// Writes jq_diff's `corpus/vm_opt.test` from [`PROGRAMS`], each on four of
/// [`INPUTS`] (run after changing them, then run jq_diff):
///
/// ```text
/// cargo test --lib write_vm_opt_corpus -- --ignored
/// ```
#[test]
#[ignore]
fn write_vm_opt_corpus() {
    let mut out = String::from(
        "# The VM's optimized code (src/jq/lang/execute/region.rs): straight-line regions,\n\
         # frameless calls of closures whose body is a region, natives evaluating such\n\
         # closures (select, map, repeat), and the in-place RET and RANGE. Everything must\n\
         # match jq exactly: values, errors and their order, side effects (debug, stderr,\n\
         # input), labels, path expressions, value identity (path($x)) and array storage\n\
         # (writing past the end of a view). Generated from the edge cases in\n\
         # src/jq/lang/execute/opt_tests.rs (write_vm_opt_corpus).\n\
         #\n\
         # Format: jq's .test format without expected output lines: a program line, one\n\
         # input line, and a blank line between cases.\n",
    );
    for (i, program) in PROGRAMS.iter().chain(NATIVE_PROGRAMS).enumerate() {
        for j in 0..4 {
            let input = INPUTS[(i * 5 + j * 3) % INPUTS.len()];
            out.push_str(&format!("\n{program}\n{input}\n"));
        }
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/jq_compat/corpus/vm_opt.test"
    );
    std::fs::write(path, out).unwrap();
}

fn compiled(program: &str) -> super::Jq {
    use crate::jq::lang::{CompileOptions, jq_compile_args};
    let bc = jq_compile_args(program.as_bytes(), &CompileOptions::new(".")).unwrap();
    super::Jq::new(bc)
}

/// Prints the regions of `QJ_DUMP_PROGRAM` (with `--nocapture`).
#[test]
#[ignore]
fn dump_regions() {
    let program = std::env::var("QJ_DUMP_PROGRAM").unwrap_or_else(|_| ". + 1".into());
    let jq = compiled(&program);
    eprintln!("{}", jq.dump_disassembly(0));
    eprintln!("{}", super::region::dump(&jq.prog));
}

/// Region compilation and the frameless calls see the program's shape: these compile
/// to the code they should (so the tests above test what they mean to).
#[test]
fn regions_cover_expressions() {
    let covered = |program: &str| {
        let jq = compiled(program);
        (
            jq.prog.regions.len(),
            jq.prog.funcs.iter().filter(|f| f.direct.is_some()).count(),
        )
    };
    // `. % 3 == 0`: a direct body (select's closure) and a region in select.
    let (regions, direct) = covered("[range(10) | select(. % 3 == 0)]");
    assert!(regions >= 2 && direct >= 1, "{regions} {direct}");
    // A reduce body is one region.
    let (regions, _) = covered("reduce range(5) as $i (0; . + $i)");
    assert!(regions >= 1);
}
