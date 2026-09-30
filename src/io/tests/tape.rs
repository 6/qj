//! The tape printer against the builder and the value printer: for every
//! document simdjson accepts and every layout, `Doc::print` must write
//! exactly what `dump_to_vec` writes for the built value.

use super::Rng;
use super::generate::Gen;
use crate::io::simd::SimdParser;
use crate::io::tape::{Layout, Scratch};
use crate::io::tape_eval::{Output, TapeProgram};
use crate::jq::lang::execute::Jq;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::print::dump_to_vec;
use crate::jq::value::{DumpOptions, Indent, Value};

/// Random rounds: all of them in release builds, a tenth in debug builds
/// (`cargo test`'s fast suite).
fn rounds(n: usize) -> usize {
    if cfg!(debug_assertions) { n / 10 } else { n }
}

fn layouts() -> Vec<DumpOptions> {
    let mut all = Vec::new();
    for indent in [
        Indent::Compact,
        Indent::Spaces(2),
        Indent::Spaces(0),
        Indent::Spaces(1),
        Indent::Spaces(7),
        Indent::Tab,
    ] {
        for sort_keys in [false, true] {
            for ascii in [false, true] {
                all.push(DumpOptions {
                    indent,
                    sort_keys,
                    ascii,
                    colors: None,
                });
            }
        }
    }
    all
}

/// Checks one text (skipped when simdjson rejects it).
fn check(simd: &mut SimdParser, text: &[u8], layouts: &[DumpOptions]) -> bool {
    let mut padded = text.to_vec();
    padded.resize(text.len() + crate::simdjson::padding(), 0);
    let Ok(value) = simd.parse(&padded, 0, text.len()) else {
        return false;
    };
    for opts in layouts {
        let mut want = Vec::new();
        dump_to_vec(&value, opts, &mut want);
        let layout = Layout::new(opts).expect("no colors");
        let got = simd
            .parse_with(&padded, 0, text.len(), |p| {
                let doc = p.doc();
                let mut out = Vec::new();
                let mut scratch = Scratch::default();
                doc.print(doc.root(), 0, &layout, &mut scratch, &mut out);
                out
            })
            .expect("parsed before");
        assert!(
            got == want,
            "{opts:?} on {}:\n got {}\nwant {}",
            String::from_utf8_lossy(text),
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&want)
        );
    }
    true
}

#[test]
fn prints_edge_cases_like_the_value_printer() {
    let mut simd = SimdParser::new();
    let layouts = layouts();
    let many_keys = |n: usize, dup: Option<usize>| {
        let mut s = String::from("{");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("\"k{i}\":{i}"));
        }
        if let Some(d) = dup {
            s.push_str(&format!(",\"k{d}\":\"again\""));
        }
        s.push('}');
        s
    };
    let deep = |d: usize| format!("{}1{}", "[".repeat(d), "]".repeat(d));
    let deep_obj = |d: usize| format!("{}1{}", "{\"a\":".repeat(d), "}".repeat(d));
    let cases: Vec<String> = [
        "0",
        "-0",
        "-0.0",
        "0.0",
        "1.50",
        "1e2",
        "1e308",
        "-1.7976931348623157e308",
        "2.2250738585072014e-308",
        "4.9e-324",
        "123456789012345678",
        "-123456789012345678",
        "999999999999999",
        "1000000000000000",
        "18446744073709551615",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "0.1",
        "3.14159265358979323846264338327950288",
        "[0,-0,1,-1,0.0,-0.0]",
        "\"\"",
        r#""\u0000\u001f\"\\\/\b\f\n\r\t\u007f \u0080 é😀 😀""#,
        "[]",
        "{}",
        "[[],{},[[]],{\"a\":{}}]",
        r#"{"a":1,"b":2,"a":3}"#,
        r#"{"a":1,"a":2,"a":3}"#,
        r#"{"b":1,"a":2,"c":{"z":1,"y":2,"z":3},"a":[{"q":1,"q":2}]}"#,
        r#"{"":1,"":2}"#,
        r#"{"é":1,"e":2,"😀":3,"E":4}"#,
        r#"[{"k":"v","k":"w"},{"k":"x"}]"#,
        "  {\"a\" : [ 1 , 2.50 , true , false , null ] }  ",
        "[1,[2,[3,[4,[5]]]]]",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([
        many_keys(9, None),
        many_keys(9, Some(0)),
        many_keys(9, Some(8)),
        many_keys(40, Some(17)),
        many_keys(2047, None),
        many_keys(2047, Some(5)),
        many_keys(2049, None),
        many_keys(2049, Some(2048)),
        many_keys(5000, Some(1)),
        deep(255),
        deep(256),
        deep(257),
        deep(300),
        deep(1000),
        deep_obj(258),
        format!("[{}]", deep(300)),
    ])
    .collect();
    for c in &cases {
        assert!(check(&mut simd, c.as_bytes(), &layouts), "rejected: {c}");
    }
    // simdjson rejects numbers outside double range (jq's parser takes them),
    // and maybe some tiny ones: nothing to compare then.
    for c in [
        "1E+400",
        "-1e400",
        "1e-400",
        "[1e-400, 0]",
        "100000000000000000001",
        "18446744073709551616",
    ] {
        check(&mut simd, c.as_bytes(), &layouts);
    }
}

#[test]
fn prints_random_documents_like_the_value_printer() {
    let mut simd = SimdParser::new();
    let layouts = layouts();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut checked = 0;
    for case in 0..rounds(6000) {
        let mut text = Vec::new();
        let mut g = Gen {
            r: &mut rng,
            weird: if case % 5 == 0 { 20 } else { 0 },
        };
        g.value(&mut text, 0, case % 2 == 0);
        if check(&mut simd, &text, &layouts) {
            checked += 1;
        }
    }
    assert!(checked > rounds(6000) * 5 / 6, "{checked}");
}

/// The VM's outputs for `program` on `value`, or `None` for an error.
fn vm_outputs(program: &str, value: Value) -> Option<Vec<Value>> {
    let bc = jq_compile_args(program.as_bytes(), &CompileOptions::new(".")).expect("compiles");
    let mut jq = Jq::new(bc);
    jq.start(value, 0);
    let mut out = Vec::new();
    for r in jq.by_ref() {
        match r {
            Ok(v) => out.push(v),
            Err(_) => return None,
        }
    }
    Some(out)
}

const PROGRAMS: &[&str] = &[
    ".",
    ".a",
    ".a.b",
    ".\"a\"",
    ".[\"b\"]",
    ".a.b.c",
    ".[]",
    ".[] | .a",
    ".[].a",
    ".a[]",
    ".a[].b",
    ".[] | .[]",
    ".[][]",
    "length",
    ".a | length",
    ".[] | length",
    "keys",
    "keys_unsorted",
    ".[] | keys",
    ".a | keys_unsorted",
    "[.[] | .a]",
    "[.[]]",
    "[.a]",
    "map(.a)",
    "map(.)",
    "map(length)",
    "map(keys)",
    "map({a, b})",
    "map({a: .b, \"x y\": .c})",
    "{a}",
    "{a, b: .c}",
    "{\"a\"}",
    "{a: .a.b, c: [.[]]}",
    "{a: {b: .a}}",
    "{a} | .a",
    "{a, b} | length",
    "{a} | .[]",
    "[.[]] | length",
    "select(.a == 1)",
    "select(.a == 1.0)",
    "select(.a != \"b\")",
    "select(.a == null)",
    "select(.a == -1)",
    "select(1 == .a)",
    "select(.a == true)",
    "select(.a)",
    "select(.a.b)",
    ".[] | select(.a == \"x\")",
    ".[] | select(.b != null) | .b",
    "select(.a == 1) | .b",
    "map(select(.a))",
    "[.[] | select(. == 1)]",
    "select(length == 2)",
    ".a?",
    ".a.b?",
    ".a?.b",
    ".[]?",
    ".a[]?",
    ".[].a?",
    ".[][]?",
    "[.[]?]",
    "[.[].a?]",
    "[.a[]?.b]",
    "map(.a?)",
    ".a[]? | .b",
    "[.[] | .[]?]",
    "[.[]? | length]",
    ".[]? | keys",
    "{a} | .a[]?",
    "def f: .a; f",
    "def f: .a; def g: f | .b; g",
    "def keys: .a; keys",
    "def f: length; def length: .a; f",
    "def f: length; def length: .a; [f] | length",
    "def f: .a; def f: .b; f",
    "def f: def g: .a; g | length; f",
    "def is_one: .a == 1; select(is_one)",
    "def is_x: .a != \"x\"; .[] | select(is_x)",
    "def f: .[]; [f]",
    "def select: .; select(.a)",
    "def f: .a; map(f)",
    "def f: .a?; [.[] | f]",
    "def f: {a}; f | .a",
    "def keys: .a; map(keys)",
    "def f: .a; {x: f}",
    "select(.a > 1)",
    "select(.a >= 1)",
    "select(.a < 1.0)",
    "select(.a <= -1)",
    "select(.a > 1e2)",
    "select(.a > 12345678901234567889)",
    "select(.a < \"x\")",
    "select(.a >= \"\")",
    "select(.a <= null)",
    "select(.a > null)",
    "select(.a < true)",
    "select(.a >= false)",
    "select(1 < .a)",
    "select(\"b\" >= .a)",
    "select(length > 2)",
    ".[] | select(.b < 2)",
    "map(select(. > 1))",
    "[.[] | select(. <= \"a\")]",
    "def big: .a > 1; select(big)",
    "select(.a and .b)",
    "select(.a or .b)",
    "select(.a | not)",
    "select(not)",
    "map(select(not))",
    "select(.a == 1 and .b != null)",
    "select(.a == 1 or .a == 2)",
    "select((.a | not) and .b)",
    "select(.a > 1 and .a < 3)",
    ".[] | select(.a or .b == 1)",
    "def not: .a; select(not)",
    "select(.a == 1 | not)",
    "select(.a and .a.b)",
    "select(.a or .a.b)",
    "select(.a | not | not)",
    "select(.b and .c and .a)",
    "type",
    ".a | type",
    ".[] | type",
    ".[]? | type",
    "map(type)",
    "[.[] | type]",
    "{a: (.a | type), t: type}",
    "[.[]] | type",
    "{a} | type",
    "type | length",
    "type | type",
    "length | type",
    "keys | type",
    "[.[]? | type] | length",
    "select(type == \"object\")",
    "select(type != \"array\")",
    ".[]? | select(type == \"string\")",
    "select(.a | type == \"number\")",
    "select(.a | type == \"string\" and length > 1)",
    "select(.a | . == 1)",
    "select(.a | length > 1)",
    "select(.a | .b)",
    "select(.a | .b | not)",
    "def t: type; map(t)",
    "def type: .a; type",
    "def is_obj: type == \"object\"; .[]? | select(is_obj)",
    "has(\"a\")",
    "has(\"\")",
    ".a | has(\"b\")",
    ".[]? | has(\"a\")",
    "map(has(\"a\"))",
    "{a} | has(\"a\")",
    "{a} | has(\"b\")",
    "has(\"a\") | not",
    "{h: has(\"b\"), t: type}",
    "not",
    ".a | not",
    "map(not)",
    "[.[]? | not]",
    "map(not) | add",
    ".a == 1",
    ".a != null",
    "map(. == 1)",
    "map(. > 1)",
    "[.[]? | . < \"x\"]",
    "{x: (.a > 1), y: (.b == \"x\" and .a)}",
    ".a and .b",
    ".a or .b",
    "(.a == 1) == true",
    "type == \"object\"",
    "map(type == \"string\")",
    "select(.a | has(\"b\"))",
    "select(has(\"a\"))",
    "select(has(\"a\") and (.b | type == \"string\"))",
    "def f: has(\"a\"); {x: f, y: (.b | type)}",
];

/// Programs the tape evaluates only on some inputs, declining on others
/// where jq succeeds (`add` of strings, arrays or objects): whenever they
/// don't decline, their outputs must be the VM's.
const PARTIAL_PROGRAMS: &[&str] = &[
    "add",
    "map(.a) | add",
    "[.[] | length] | add",
    "add(.[] | .a?)",
    "[.[] | .b] | add | length",
    "{s: add(.[]?)}",
    "def add: .a; add",
    "map(type) | add",
    "[.[]? | type] | add",
    "add | type",
    "select(add | type == \"number\")",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Outcome {
    /// simdjson rejected the text.
    Rejected,
    /// The VM raised an error, and the tape evaluator declined.
    Error,
    /// The VM succeeded, and the tape evaluator declined anyway.
    Declined,
    /// Same outputs.
    Evaluated,
}

/// Checks one program on one text: when the tape evaluator doesn't
/// decline, its outputs (and status-relevant kinds) are the VM's, in every
/// layout; and it declines whenever the VM raises an error.
fn check_eval(
    simd: &mut SimdParser,
    program: &str,
    text: &[u8],
    layouts: &[DumpOptions],
) -> Outcome {
    let mut padded = text.to_vec();
    padded.resize(text.len() + crate::simdjson::padding(), 0);
    let Ok(value) = simd.parse(&padded, 0, text.len()) else {
        return Outcome::Rejected;
    };
    let want = vm_outputs(program, value);
    let prog = TapeProgram::new(program.as_bytes()).expect("qualifies");
    type Dump = (Vec<u8>, bool, Option<String>);
    let got: Option<Vec<Vec<Dump>>> = simd
        .parse_with(&padded, 0, text.len(), |p| {
            let doc = p.doc();
            let mut scratch = Scratch::default();
            let mut results = Vec::new();
            prog.eval(&doc, &mut scratch, &mut results).ok()?;
            let mut dumps = Vec::new();
            for opts in layouts {
                let layout = Layout::new(opts).expect("no colors");
                let mut per_layout = Vec::new();
                for v in &results {
                    let o = Output { doc: &doc, val: v };
                    let mut out = Vec::new();
                    o.dump(&layout, &mut scratch, &mut out);
                    per_layout.push((out, o.is_null_or_false(), o.as_str().map(str::to_owned)));
                }
                dumps.push(per_layout);
            }
            Some(dumps)
        })
        .expect("parsed before");
    let text = String::from_utf8_lossy(text);
    match (want, got) {
        (None, Some(_)) => panic!("{program} on {text}: jq errors, the tape evaluator doesn't"),
        (None, None) => Outcome::Error,
        (Some(_), None) => Outcome::Declined,
        (Some(want), Some(got)) => {
            for (opts, got) in layouts.iter().zip(got) {
                assert_eq!(got.len(), want.len(), "{program} on {text}: output count");
                for (w, (g, null_or_false, s)) in want.iter().zip(got) {
                    let mut wd = Vec::new();
                    dump_to_vec(w, opts, &mut wd);
                    assert!(
                        wd == g,
                        "{program} on {text} ({opts:?}):\n got {}\nwant {}",
                        String::from_utf8_lossy(&g),
                        String::from_utf8_lossy(&wd)
                    );
                    assert_eq!(
                        null_or_false,
                        matches!(w, Value::Null | Value::Bool(false)),
                        "{program} on {text}"
                    );
                    assert_eq!(s.as_deref(), w.as_str(), "{program} on {text}");
                }
            }
            Outcome::Evaluated
        }
    }
}

#[test]
fn evaluates_programs_like_the_vm() {
    let mut simd = SimdParser::new();
    let layouts: Vec<DumpOptions> = layouts().into_iter().step_by(3).collect();
    let inputs: &[&str] = &[
        "null",
        "true",
        "1",
        "-0",
        "\"s\"",
        "[]",
        "{}",
        "[1,2,3]",
        "[null,1,\"x\",[1],{\"a\":1}]",
        "{\"a\":1,\"b\":2}",
        "{\"a\":1.0,\"b\":\"x\",\"c\":[1,2]}",
        "{\"a\":{\"b\":{\"c\":5}},\"b\":null}",
        "{\"a\":[{\"b\":1},{\"b\":2},{\"c\":3}]}",
        "{\"a\":1,\"a\":2,\"b\":1,\"a\":-1}",
        "{\"a\":null,\"b\":false}",
        "{\"a\":true,\"b\":\"b\"}",
        "[{\"a\":\"x\",\"b\":1},{\"a\":\"y\",\"b\":null},{\"a\":\"x\"}]",
        "{\"a\":\"é😀\",\"b\":[],\"c\":{}}",
        "{\"a\":[1,[2,[3]]],\"b\":-1}",
        "{\"b\":[1,2],\"a\":1e2}",
        "[[1,2],[3],[]]",
        "{\"k0\":0,\"k1\":1,\"k2\":2,\"k3\":3,\"k4\":4,\"k5\":5,\"k6\":6,\"k7\":7,\"k8\":8,\"a\":9,\"k0\":10}",
        "{\"a\":12345678901234567890,\"b\":-0.0}",
        "[\"a\",\"b\"]",
        "false",
        "\"é\\u0000\\\"\"",
        "[true,false,null,0,-0,1.5,1e308,4.9e-324,2.2250738585072014e-308,\"\",\"s\",[],{}]",
        "{\"a\":true,\"b\":false,\"c\":null}",
        "{\"a\":\"xy\",\"b\":\"s\"}",
        "{\"a\":{\"b\":null},\"b\":[]}",
        "{\"a\":{\"a\":1,\"b\":2,\"a\":{}},\"b\":{\"b\":1}}",
        "{\"b\":1,\"b\":\"x\",\"a\":[1,{\"a\":1}]}",
        "{\"\":1,\"a\\u0000\":2}",
        "[[],{},[{}],{\"a\":[]}]",
    ];
    let mut counts = std::collections::HashMap::new();
    let mut declined = Vec::new();
    let mut evaluated_programs = std::collections::HashSet::new();
    let mut tally = |o: Outcome, program: &'static str, text: &[u8]| {
        let partial = PARTIAL_PROGRAMS.contains(&program);
        *counts.entry((o, partial)).or_insert(0usize) += 1;
        if o == Outcome::Declined && !partial && declined.len() < 20 {
            declined.push(format!("{program} on {}", String::from_utf8_lossy(text)));
        }
        if o == Outcome::Evaluated {
            evaluated_programs.insert(program);
        }
    };
    let programs = || PROGRAMS.iter().chain(PARTIAL_PROGRAMS);
    for program in programs() {
        for input in inputs {
            let o = check_eval(&mut simd, program, input.as_bytes(), &layouts);
            tally(o, program, input.as_bytes());
        }
    }
    let mut rng = Rng(0xfeed_beef_1234_5678);
    for case in 0..rounds(3000) {
        let mut text = Vec::new();
        let mut g = Gen {
            r: &mut rng,
            weird: 0,
        };
        g.value(&mut text, 0, case % 2 == 0);
        for program in programs() {
            let o = check_eval(&mut simd, program, &text, &layouts);
            tally(o, program, &text);
        }
    }
    let evaluated = counts
        .get(&(Outcome::Evaluated, false))
        .copied()
        .unwrap_or(0);
    // The partial programs are evaluated sometimes.
    assert!(
        counts
            .get(&(Outcome::Evaluated, true))
            .copied()
            .unwrap_or(0)
            > rounds(3000),
        "{counts:?}"
    );
    // Declining where jq succeeds is only a missed shortcut, but it should be
    // rare: only documents with huge containers.
    assert!(declined.is_empty(), "{counts:?}: {declined:#?}");
    assert!(evaluated > rounds(3000) * 13, "{counts:?}");
    // Every program is evaluated on some input (not only jq's errors), but
    // for one that checks a shadowing rule on an error (the last `length`
    // is `.a`, of an array).
    let always_errors = "def f: length; def length: .a; [f] | length";
    let never: Vec<_> = programs()
        .filter(|p| !evaluated_programs.contains(*p) && **p != always_errors)
        .collect();
    assert!(never.is_empty(), "never evaluated: {never:?}");
}

/// Containers of 2^24 - 1 elements or more, whose count simdjson saturates:
/// counted by walking them, with the structural cursor exact after them
/// (the `-0` after the big array is printed from its text). The document is
/// compact and canonical, so the expected outputs are known without building
/// its value (which would take gigabytes): about 400 MB at most.
#[test]
#[ignore = "parses a 34 MB document; run with --release"]
fn huge_containers() {
    let n = (1 << 24) + 3;
    let mut text = Vec::with_capacity(2 * n + 64);
    text.extend_from_slice(b"{\"a\":[");
    for i in 0..n {
        if i > 0 {
            text.push(b',');
        }
        text.push(b'0' + (i % 10) as u8);
    }
    text.extend_from_slice(b"],\"b\":-0,\"c\":[[1.50,-0]]}");
    let mut padded = text.clone();
    padded.resize(text.len() + crate::simdjson::padding(), 0);
    let mut simd = SimdParser::new();
    let layout = Layout::new(&DumpOptions::default()).expect("no colors");
    let expected: [(&str, &[u8]); 7] = [
        (".", &text),
        (".b", b"-0"),
        (".a | length", b"16777219"),
        ("length", b"3"),
        (".c", b"[[1.50,-0]]"),
        ("[.c[]]", b"[[1.50,-0]]"),
        ("{b, c}", b"{\"b\":-0,\"c\":[[1.50,-0]]}"),
    ];
    for (program, want) in expected {
        let prog = TapeProgram::new(program.as_bytes()).expect("qualifies");
        let got = simd
            .parse_with(&padded, 0, text.len(), |p| {
                let doc = p.doc();
                let mut scratch = Scratch::default();
                let mut results = Vec::new();
                prog.eval(&doc, &mut scratch, &mut results)
                    .expect("evaluates");
                assert_eq!(results.len(), 1, "{program}");
                let mut out = Vec::new();
                Output {
                    doc: &doc,
                    val: &results[0],
                }
                .dump(&layout, &mut scratch, &mut out);
                out
            })
            .expect("parses");
        assert!(got == want, "{program}");
    }
}

/// The fuzz target's check, on generated inputs (every program, several
/// layouts).
#[test]
fn fuzz_check_on_generated_inputs() {
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    let programs = crate::io::fuzzing::TAPE_PROGRAMS.len();
    for case in 0..rounds(1500) {
        let mut text = Vec::new();
        let mut g = Gen {
            r: &mut rng,
            weird: 0,
        };
        g.value(&mut text, 0, case % 3 == 0);
        let mut data = vec![(case % programs) as u8, (case / programs) as u8];
        data.extend_from_slice(&text);
        crate::io::fuzzing::check_tape_equivalence(&data);
    }
}

#[test]
fn only_simple_programs_qualify() {
    for p in PROGRAMS.iter().chain(PARTIAL_PROGRAMS) {
        assert!(TapeProgram::new(p.as_bytes()).is_some(), "{p}");
    }
    for p in [
        "def f: f; f",
        "def f: .a | f; f",
        "def f(x): x; f(.a)",
        "def map(f): 1; map(.a)",
        "def f: 1; f",
        "def f: .a; f, f",
        "def f: $__loc__; f",
        "def f: input; f",
        "def f: .; def g: f | f; def h: g | g; def i: h | h; def j: i | i; \
         def k: j | j; def l: k | k; def m: l | l; m",
        "(.a)?",
        "try .a",
        ".a[]?.b?[0]",
        "{a: .b?}",
        "select(.a?)",
        ".[0]",
        ".a, .b",
        "map(.a, .b)",
        "{a: .[]}",
        "{a: select(.b)}",
        "{(.a): 1}",
        "{$x}",
        "{a: 1}",
        "{a: .x, a: .y}",
        "select(.a > .b)",
        "select(.a < [1])",
        "select(.[] == 1)",
        "select(.a == .b)",
        "select(.a == \"x\\(.b)\")",
        "$x",
        "import \"a\" as a; .",
        "include \"a\"; .",
        "length(1)",
        "tostring",
        ".a as $x | $x",
        "..",
        "[]",
        "@base64",
        "\"x\"",
        "1",
        "-.a",
        ".a + 1",
        "empty",
        "type(1)",
        "select(.[] | type == \"x\")",
        "select(.a? | type == \"x\")",
        "select(.a | .[] | . == 1)",
        "tostring | type",
        "[.[] | type] | sort",
        "error | type",
        "has(.a)",
        "has(0)",
        "has(\"a\", \"b\")",
        "has(\"a\\(.b)\")",
        "has($x)",
        "not(.a)",
        "def has(k): true; has(\"a\")",
        ".a == .b",
        "map(. == .)",
        "1 == 1",
        ".a + 1 == 2",
        ".a // .b",
        "(.a, .b) == 1",
        ".[] == 1",
    ] {
        assert!(TapeProgram::new(p.as_bytes()).is_none(), "{p}");
    }
}
