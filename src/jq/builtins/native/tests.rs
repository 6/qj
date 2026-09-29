//! Natives against their `builtin.jq` definitions, in process: every program runs
//! twice, with natives and on the bytecode alone, and everything observable must be
//! the same: outputs, the `debug`/`stderr` stream (the order of side effects), errors,
//! halts and exit codes. Programs come from a hand-written list of edge cases and from
//! a generator that wraps each native in probes for laziness, errors, labels, value
//! identity (`path($x)` accepts only a `jv_identical` value) and array storage (writes
//! past the end of a unique view bring back stale elements).

use super::cases::Gen;
use crate::jq::lang::execute::driver::{Options, Output, run};
use crate::jq::lang::execute::native::CALLS;

fn run_with(program: &str, input: &str, natives: bool) -> Output {
    let opts = Options {
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

/// Runs `program` both ways; panics with both results if they differ. Returns how many
/// native calls ran.
fn check(program: &str, input: &str) -> u64 {
    let before = CALLS.with(|c| c.get());
    let native = show(&run_with(program, input, true));
    let calls = CALLS.with(|c| c.get()) - before;
    let bytecode = show(&run_with(program, input, false));
    assert_eq!(
        native, bytecode,
        "native vs bytecode differ\nprogram: {program}\ninput: {input}"
    );
    calls
}

/// Edge cases, each checked on every input of [`INPUTS`].
const PROGRAMS: &[&str] = &[
    // to_entries / from_entries / with_entries
    "to_entries",
    "to_entries | from_entries",
    "from_entries",
    "with_entries(.)",
    "with_entries(empty)",
    "with_entries(., .)",
    "with_entries(.value |= tostring)",
    "with_entries(.key |= ascii_upcase)",
    "with_entries(select(.value != null))",
    "with_entries(error)",
    "with_entries(debug)",
    "with_entries({key: (.key|tostring), value: 1})",
    "with_entries(.key = null)",
    "with_entries(.value)",
    "with_entries(1)",
    "[to_entries, from_entries] | .[0] as $a | .[1]",
    "to_entries as $a | $a | path($a)",
    "{\"a\":1,\"b\":2} | to_entries | (.[0] | keys_unsorted[0]) as $a | .[1] | keys_unsorted[0] | path($a)",
    "[] | from_entries as $a | [] | from_entries | path($a)",
    "from_entries as $a | from_entries | try path($a) catch \"E\"",
    "to_entries | .[0:1] | .[3] = 1",
    "from_entries | [label $f | try break $f catch .]",
    "with_entries(first(label $x | ., break $x)) | [label $f | try break $f catch .]",
    "[.[]? | from_entries?]",
    "[.[]? | to_entries?]",
    // walk
    "walk(.)",
    "walk(., .)",
    "[walk(empty)]",
    "walk(if type == \"object\" then empty else . end)",
    "walk(if type == \"array\" then empty else . end)",
    "walk(if type == \"number\" then . + 1 else . end)",
    "walk(if type == \"string\" then ascii_downcase else . end)",
    "walk(debug)",
    "try walk(error) catch .",
    "try walk(if type == \"number\" then error else . end) catch .",
    "walk(tostring)",
    "walk(length)",
    "walk([.])",
    "[walk(.[]?)]",
    "walk(first(.[]?, .))",
    "walk(.) | [label $f | try break $f catch .]",
    "walk(label $x | try break $x catch .)",
    "[[],[]] | walk(.) | .[0] as $a | .[1] | path($a)",
    "walk(.) as $a | walk(.) | try path($a) catch \"E\"",
    "walk(if type == \"array\" then .[0:1] else . end) | .. |= .",
    "[walk(select(type != \"null\"))]",
    "walk(input? // 0)",
    "walk(if type == \"number\" then halt_error(1) else . end)",
    "first(walk(., .))",
    "[limit(1; walk(1, 2))]",
    // paths / paths(f)
    "[paths]",
    "[paths(scalars)]",
    "[paths(type == \"number\")]",
    "[paths(..)]",
    "[paths(empty)]",
    "[paths(error)]",
    "[paths(true, false, true)]",
    "[paths(debug)]",
    "[paths(input? // 1)]",
    "try [paths(if length? == 0 then error else true end)] catch .",
    "[limit(3; paths)]",
    "first(paths)",
    "[paths] | length",
    "last(paths) | if type == \"array\" then .[length + 2] = 0 else . end",
    "last(paths | select(length == 2)) | .[3] = 9",
    "[paths | select(length == 2)] | .[1] | .[3] = 9",
    "last(paths(scalars)) | .[length + 3] = 1",
    "last(paths(type == \"object\")) | .[length + 3] = 1",
    "[paths] | .[0] as $a | .[-1] | try path($a) catch \"E\"",
    "[paths | .[0:1] | .[3] = 1]",
    "paths | [label $q | try break $q catch .]",
    "label $f | paths | ., break $f",
    "[paths(path(..) | length > 1)]",
    "[paths(paths)]",
    "[paths(first(paths))]",
    "try (paths | error) catch .",
    "[paths | try error catch .]",
    "[paths(label $l | ., break $l)]",
    "[paths(. as $x | $x)]",
    "reduce paths as $p (null; [$p, .])",
    "[paths] | unique | length",
    "path(paths)?",
    "[path(..)]",
    "[..]",
    "[paths] as $p | [paths] | . == $p",
    // tostream
    "[tostream]",
    "first(tostream)",
    "[limit(2; tostream)]",
    "[tostream] | length",
    "last(tostream) | .[0] | .[length + 2] = 1",
    "last(tostream) | .[length + 2] = 1",
    "[tostream | .[0]] | map(.[length + 2] = 1)",
    "[tostream | .[0] | .[0:1] | .[3] = 9]",
    "fromstream(tostream)",
    "[tostream] | .[0] as $a | .[-1] | try path($a) catch \"E\"",
    "tostream | [label $q | try break $q catch .]",
    "try (tostream | error) catch .",
    "[tostream | select(length == 2)]",
    "path(tostream)?",
    // ascii_downcase / ascii_upcase
    "ascii_downcase",
    "ascii_upcase",
    "[.[]? | ascii_downcase?]",
    "try ascii_downcase catch .",
    "(tostring | ascii_downcase) as $a | ascii_downcase? | try path($a) catch \"E\"",
    // _modify / _assign (|=, =, +=, //=, map_values) and join
    ".a |= .+1",
    ".[]? |= .",
    ".[]? |= empty",
    ".[]? |= (., .)",
    ".x.y |= 1",
    ".a += 1",
    ".a //= 5",
    "map_values(.)",
    "map_values(empty)",
    "map_values(tostring)",
    ".. |= .",
    "(.. | numbers) |= .+1",
    "try (.a |= error) catch .",
    "try (.[]? |= error) catch .",
    ".a = 1",
    ".a = (1, 2)",
    ".[]? = 1",
    ".x.y = .a",
    "(.a, .b) = 9",
    ".a = empty",
    "try (.a.b = 1) catch .",
    "try (.a.b |= 1) catch .",
    "try (.[0] = 1) catch .",
    "try (paths |= 1) catch .",
    "try ((.a | tostring) |= 1) catch .",
    ".[1:]? |= [9]",
    "[.[]? | tostring] | join(\",\")",
    "[.[]? | tostring] | join(null)",
    "try join(\"-\") catch .",
    "try join(1) catch .",
    "try join([]) catch .",
    "[] | join(\",\") as $a | [] | join(\",\") | path($a)",
    "\"s\" as $s | [.[]? | tostring] | join($s)",
    "[.[]? | tostring] | join(\",\", \"-\")",
    ".[]? |= (label $f | ., break $f)",
    ".[]? |= first(., 2)",
    "[label $f | .[]? |= (., break $f)]",
    "map_values(.) | [label $f | try break $f catch .]",
    ".a |= . | [label $f | try break $f catch .]",
    ".[]? |= input?",
    "(.. | select(type == \"array\")) |= .[0:1] | .. |= .",
    ".[]? |= (. as [$a] ?// $a | $a)",
    "path(.a |= 1)?",
    "reduce .[]? as $x (.; .[0]? |= $x)",
    ".[] |= 9",
    ".[]? |= 9",
    "try (.[] |= 9) catch .",
    ".[]? |= empty",
    ".[]? |= (., 1)",
    ".[]? |= error",
    "try (.[]? |= error) catch .",
    ".[]? |= (.[]? |= 9)",
    "[.[]? |= input?]",
    // .., type filters, add, _flatten, first, limit, isempty, any/all, IN
    "[..]",
    "[.. | numbers]",
    "[.. | scalars]",
    "[.. | values, nulls, booleans, strings, arrays, objects, iterables]",
    "try add catch .",
    "try add(.[]?) catch .",
    "try add(.[]? | .[]?) catch .",
    "try flatten catch .",
    "try flatten(1) catch .",
    "try flatten(0) catch .",
    "try flatten(1E-400) catch .",
    "try flatten(\"a\") catch .",
    "[[]] | flatten as $a | [[[]]] | flatten | path($a)",
    "first(.[]?)",
    "first(empty)",
    "try first(error) catch .",
    "[limit(2; .[]?)]",
    "[limit(0; .[]?)]",
    "try [limit(-1; .[]?)] catch .",
    "[limit(1E-400; 1, 2)]",
    "[limit(1.5; .[]?, .[]?)]",
    "try [limit(\"a\"; .[]?)] catch .",
    "isempty(.[]?)",
    "isempty(empty)",
    "try any catch .",
    "try all catch .",
    "try any(. == 1) catch .",
    "try all(. != null) catch .",
    "any(.[]?; . == false)",
    "all(.[]?; type == \"number\")",
    "IN(1, 2)",
    "IN(.[]?; 1, 2)",
    "[isempty(1, error(\"x\")), (try isempty(error(\"y\")) catch .)]",
    "[any(1, error(\"x\"); . == 1)]",
    "[all(0, error(\"x\"); . == 1)]",
    "first(.[]?) | [label $f | try break $f catch .]",
    "[.[]? | first(.[]?)]",
    "[limit(3; repeat(.))]",
    "[first(.[]?), first(.[]?)] | .[0] as $x | .[1] | try path($x) catch \"E\"",
    "label $f | first(.[]?) | ., break $f",
    "[.. | numbers] as $a | [.. | numbers] | . == $a",
    // mixed, and ?// (natives that abandon closures fall back)
    "walk(if type == \"object\" then with_entries(.key |= ascii_upcase) else . end)",
    "[paths(type == \"array\")] | map(tostream)",
    "walk(. as [$a] ?// $a | $a)",
    "[paths(. as [$a] ?// $a | $a)]",
    "to_entries | .[] as {key: $k} ?// $k | $k",
    "[.[]? as [$a] ?// $a | $a] | walk(.)",
    "[limit(2; paths)] | .[0] | tostream",
    "def f: walk(if type == \"array\" then map(f) else . end); f",
    "def w(f): walk(f); w(.) | w(tojson)",
    "[recurse | tostream?]",
    "[.. | paths?]",
];

const INPUTS: &[&str] = &[
    "null",
    "1",
    "\"AbC\u{e9}Z\"",
    "[]",
    "{}",
    "[1,[2,[3]],{\"a\":[]}]",
    "{\"a\":1,\"b\":[2,{\"c\":3}],\"d\":{},\"e\":[]}",
    "[{\"key\":\"a\",\"value\":1},{\"k\":\"b\",\"value\":2},{\"name\":\"c\",\"Value\":3}]",
    "[{\"key\":\"a\",\"value\":1},{\"key\":\"a\",\"value\":2},{\"Key\":\"b\"}]",
    "[{\"key\":1,\"value\":1}]",
    "[{\"key\":false,\"name\":\"n\",\"value\":1}]",
    "[null]",
    "[[1,2]]",
    "{\"x\":{\"key\":\"y\",\"value\":[1]}}",
    "{\"A\":\"xY\",\"b\":\"Q\",\"c\":[\"Mm\",{\"D\":\"eF\"}]}",
    "[[[[[]]]],{\"a\":{\"b\":{\"c\":{}}}}]",
    "{\"x\":{\"a\":{\"b\":1},\"c\":2}}",
];

#[test]
fn edge_cases_match_the_definitions() {
    let mut calls = 0;
    for p in PROGRAMS {
        for i in INPUTS {
            calls += check(p, i);
        }
    }
    assert!(calls > 1000, "natives ran only {calls} times");
}

/// Natives whose reference lifetimes the probes of [`lifetimes_match_the_definitions`]
/// check.
///
/// (No `?`: a `try` holds its input itself, which would hide a native's early release.)
const LIFETIME_NATIVES: &[&str] = &[
    "to_entries",
    "from_entries",
    "with_entries(.)",
    "walk(.)",
    "paths",
    "paths(true)",
    "tostream",
    "ascii_downcase",
    "([.[] | tostring] | join(\",\"))",
    "join(\",\")",
    "join(null)",
    "(.[0] |= .)",
    "(.[0] = 1)",
    "(.[1:] = [])",
    "map_values(.)",
    ".. |= .",
    "(.[] |= empty)",
    "((.. | strings) |= empty)",
    "((.. | numbers) |= first(.[]?))",
    "(.[0] = tostring)",
    "(.[0] = (1, 2))",
    "(.[0] = .[0])",
    "((.[0:1]) | paths)",
    "((.[0:1]) | paths(true))",
    "((.[0:1]) | tostream)",
    "..",
    "(.. | arrays)",
    "arrays",
    "add",
    "add(.[])",
    "flatten",
    "first(.[])",
    "first(.)",
    "limit(1; .[])",
    "[limit(1; .[])]",
    "isempty(.[])",
    "any(.[]; true)",
    "all(.[]; true)",
    "any",
    "IN(.[0])",
    "(.[] |= 9)",
    "(.[]? |= 9)",
    "(.[] |= (., 1))",
    "map_values(empty)",
];

/// Where jq's definition still holds a value (in a suspended fork point) when an output
/// reaches the caller, the native must too, and must not hold it longer: a slice of
/// fresh storage is uniquely owned once `$$$$v` moves the variable out or `reduce`
/// moves its state into the update, and then a write past its end brings back stale
/// elements. `[., ., ., .][0:2]` is such a slice (its storage has room for 6, so `.[3] = 9` writes in place when unique).
#[test]
fn lifetimes_match_the_definitions() {
    const PROBES: &[&str] = &[
        "V as $v | $v | N | $$$$v | .[3] = 9",
        "V | reduce (N) as $j (.; .[3] = 9)",
        "V | N | if type == \"array\" then .[3] = 9 else . end",
        "V as $v | $v | [N] | $$$$v | .[3] = 9",
        "V as $v | $v | last(N) | $$$$v | .[3] = 9",
        "V as $v | $v | first(N) | $$$$v | .[3] = 9",
        "[V, .] | N | if type == \"array\" then .[length + 1] = 0 else . end",
        "[V, .] | reduce (N) as $j (.; .[0][3] = 9)",
        "[V] | reduce (N) as $j (.; .[0][3] = 9)",
        "{a: V} | reduce (N) as $j (.; .a[3] = 9)",
        "{a: V, b: .} | N | if type == \"array\" then .[length + 1] = 0 else . end",
    ];
    let mut calls = 0;
    for n in LIFETIME_NATIVES {
        for probe in PROBES {
            let program = probe.replace('V', "[., ., ., .][0:2]").replace('N', n);
            for input in [
                "\"a\"",
                "[1]",
                "{\"key\":\"k\",\"value\":[2]}",
                "null",
                "[\"a\",1,[\"b\"]]",
            ] {
                calls += check(&program, input);
            }
        }
    }
    assert!(calls > 100, "natives ran only {calls} times");
}

/// Generated programs, bounded so they are safe to run in process (see `cases.rs`); the
/// long, unbounded run is out of process (`tests/native_diff.rs`).
#[test]
fn random_programs_match_the_definitions() {
    let mut g = Gen::bounded(0x9E37_79B9_7F4A_7C15);
    let n = 3000;
    let mut calls = 0;
    for _ in 0..n {
        let (program, input) = g.case();
        calls += check(&program, &input);
    }
    assert!(calls > n / 4, "natives ran only {calls} times");
}
