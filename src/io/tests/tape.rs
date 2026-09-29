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
    ];
    let mut counts = std::collections::HashMap::new();
    let mut declined = Vec::new();
    let mut tally = |o: Outcome, program: &str, text: &[u8]| {
        *counts.entry(o).or_insert(0usize) += 1;
        if o == Outcome::Declined && declined.len() < 20 {
            declined.push(format!("{program} on {}", String::from_utf8_lossy(text)));
        }
    };
    for program in PROGRAMS {
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
        for program in PROGRAMS {
            let o = check_eval(&mut simd, program, &text, &layouts);
            tally(o, program, &text);
        }
    }
    let evaluated = counts.get(&Outcome::Evaluated).copied().unwrap_or(0);
    // Declining where jq succeeds is only a missed shortcut, but it should be
    // rare: only documents with huge containers.
    assert!(declined.is_empty(), "{counts:?}: {declined:#?}");
    assert!(evaluated > rounds(3000) * 13, "{counts:?}");
}

/// Containers of 2^24 - 1 elements or more, whose count simdjson saturates:
/// counted by walking them, with the structural cursor exact after them
/// (the `-0` after the big array is printed from its text).
#[test]
#[ignore = "builds a 34 MB document; run with --release"]
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
    let mut simd = SimdParser::new();
    let compact = [DumpOptions::default()];
    assert!(check(&mut simd, &text, &compact));
    for program in [".b", ".a | length", "length", ".c", "[.c[]]", "{b, c}"] {
        assert_eq!(
            check_eval(&mut simd, program, &text, &compact),
            Outcome::Evaluated,
            "{program}"
        );
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
    for p in PROGRAMS {
        assert!(TapeProgram::new(p.as_bytes()).is_some(), "{p}");
    }
    for p in [
        ".a?",
        ".[]?",
        ".[0]",
        ".a, .b",
        "def f: .; f",
        "map(.a, .b)",
        "{a: .[]}",
        "{a: select(.b)}",
        "{(.a): 1}",
        "{$x}",
        "{a: 1}",
        "{a: .x, a: .y}",
        "select(.a > 1)",
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
    ] {
        assert!(TapeProgram::new(p.as_bytes()).is_none(), "{p}");
    }
}
