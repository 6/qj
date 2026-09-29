//! VM tests. Most compare with the jq 1.8.1 binary (they are skipped when `jq` on
//! PATH isn't jq 1.8.1), running each program twice: on jq's own bytecode (rebuilt
//! from `--debug-dump-disasm` by [`super::disasm`]), which isolates the VM, and on the
//! bytecode from our compiler ([`jq_compile_args`]), which checks the whole core.
//! Outputs, errors and `--debug-trace` output (refcounts included) must match.

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use super::disasm;
use super::{JQ_DEBUG_TRACE, JQ_DEBUG_TRACE_ALL, Jq};
use crate::jq::builtins::CFn;
use crate::jq::lang::bytecode::Bytecode;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::{DumpOptions, Error, Value, dump_string, parse_sized};

/// Test-only replacements for C builtins, by name (none are needed now that every
/// builtin is ported; kept for bisecting builtin/VM interactions).
pub(super) const OVERRIDES: &[(&str, CFn)] = &[];

// ---- running jq ------------------------------------------------------------------

fn jq_ok() -> bool {
    thread_local! {
        static OK: bool = Command::new("jq")
            .arg("--version")
            .output()
            .map(|o| o.stdout.starts_with(b"jq-1.8.1"))
            .unwrap_or(false);
    }
    OK.with(|ok| *ok)
}

struct JqResult {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_jq(args: &[&str], stdin: &str) -> JqResult {
    let mut child = Command::new("jq")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq");
    {
        use std::io::Write;
        let mut si = child.stdin.take().unwrap();
        si.write_all(stdin.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    JqResult {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// jq's bytecode for `program` (compiled with `jq -n --debug-dump-disasm`, which also
/// runs it on `null`; the output after the disassembly is ignored).
fn jq_bytecode(program: &str) -> Rc<Bytecode> {
    let r = run_jq(&["-n", "--debug-dump-disasm", program], "");
    let text = match r.stdout.split_once("\n\n") {
        Some((d, _)) => d,
        None => panic!("no disassembly for {program}: {}", r.stderr),
    };
    disasm::load(text, OVERRIDES)
}

/// Our compiler's bytecode for `program`.
fn our_bytecode(program: &str) -> Rc<Bytecode> {
    match jq_compile_args(program.as_bytes(), &CompileOptions::new(".")) {
        Ok(bc) => bc,
        Err(e) => panic!("{program} doesn't compile:\n{}", e.render()),
    }
}

/// Both bytecodes for `program`, labeled.
fn bytecodes(program: &str) -> [(&'static str, Rc<Bytecode>); 2] {
    [
        ("jq's bytecode", jq_bytecode(program)),
        ("our bytecode", our_bytecode(program)),
    ]
}

/// What jq prints: result lines, then the uncaught error (`jq: error (at ...): msg`
/// normalized to `error: msg`).
fn jq_expect(program: &str, input: &str) -> Vec<String> {
    let r = run_jq(&["-c", program], input);
    let mut lines: Vec<String> = r.stdout.lines().map(str::to_string).collect();
    if r.code == 5 {
        // "jq: error (at <stdin>:N): msg" or "jq: error (at <stdin>:N) (not a string): v"
        let e = r.stderr.trim_end_matches('\n');
        let rest = e.strip_prefix("jq: error (at ").expect("error line");
        let close = rest.find(')').expect("position");
        let rest = &rest[close + 1..];
        let rest = rest.strip_prefix(": ").unwrap_or(rest).trim_start();
        lines.push(format!("error: {rest}"));
    } else {
        assert_eq!(r.code, 0, "jq failed on {program}: {}", r.stderr);
    }
    lines
}

fn error_line(e: &Error) -> String {
    match e.value() {
        Value::String(s) => format!("error: {}", s.as_str()),
        v => format!(
            "error: (not a string): {}",
            dump_string(v, &DumpOptions::default())
        ),
    }
}

/// Runs the VM on `bc` over each input (like jq's main loop).
fn vm_outputs(bc: Rc<Bytecode>, input: &str) -> Vec<String> {
    let mut jq = Jq::new(bc);
    let mut out = Vec::new();
    let mut parser = crate::jq::value::Parser::new(crate::jq::value::ParseFlags::default());
    parser.set_buf(input.as_bytes(), false);
    let mut inputs = Vec::new();
    while let Some(v) = parser.next() {
        inputs.push(v.expect("valid test input"));
    }
    for v in inputs {
        jq.start(v, 0);
        for r in &mut jq {
            match r {
                Ok(v) => out.push(v.to_json()),
                Err(e) => {
                    out.push(error_line(&e));
                    return out;
                }
            }
        }
    }
    out
}

fn check(program: &str, input: &str) {
    if !jq_ok() {
        return;
    }
    let want = jq_expect(program, input);
    for (which, bc) in bytecodes(program) {
        let got = vm_outputs(bc, input);
        assert_eq!(got, want, "program: {program}\ninput: {input}\n({which})");
    }
}

// ---- comparisons with jq -----------------------------------------------------------

#[test]
fn basic_generators_and_backtracking() {
    check(".[] | . + 1", "[1,2,3]");
    check("[.[] | . * 2]", "[1,2,3]");
    check("1, 2, (3, 4) | . + 10", "null");
    check("[.[] | select(. > 1)]", "[1,2,3]");
    check("if . then 1 else 2 end", "false");
    check(
        "[.[] | if . > 1 then \"big\" elif . == 1 then \"one\" else empty end]",
        "[0,1,2]",
    );
    check("{a: 1, b: .}", "[1]");
    check("{(.[]|tojson): .}", "[1,2]");
    check("[.[] as $x | $x * $x]", "[1,2,3]");
    check(". as [$a, $b, {c: $c}] | $a + $b + $c", "[1,2,{\"c\":3}]");
    check("$__loc__", "null");
    check("[.a, .b?, .[\"c\"]]", "{\"a\":1,\"c\":3}");
    check("[.[1:], .[:-1], .[1:2]]", "[1,2,3]");
    check("[..]", "[[1,[2]],{\"a\":3}]");
    check("[.[]?]", "3");
    check(".[]", "{\"a\":1,\"b\":[2]}");
    check("[.. | numbers?]", "[1,[2]]");
}

#[test]
fn ranges() {
    check("[range(5)]", "null");
    check("[range(1.0; 3)]", "null");
    check("[range(0; 10; 3)]", "null");
    check("[range(5; 0; -2)]", "null");
    check("[range(.)]", "3");
    check("[range(1, 2; 3, 4)]", "null");
    check("range(\"a\"; 3)", "null");
}

#[test]
fn reduce_and_foreach() {
    check("reduce .[] as $x (0; . + $x)", "[1,2,3]");
    check("reduce .[] as $x ([]; . + [$x])", "[1,2,3]");
    check(
        "reduce .[] as [$a, $b] ({}; .[$a] = $b)",
        "[[\"x\",1],[\"y\",2]]",
    );
    check("[foreach .[] as $x (0; . + $x)]", "[1,2,3]");
    check("[foreach .[] as $x (0; . + $x; [$x, .])]", "[1,2,3]");
    check(
        "[foreach range(5) as $x (0; . + $x; select(. > 3))]",
        "null",
    );
    check("reduce empty as $x (0; . + 1)", "null");
    check("[foreach (1,2) as $x (0; empty; .)]", "null");
    check("reduce .[] as $x (0; ., 10)", "[1,2]");
    check("add", "[1,2,3]");
    check("[limit(3; .[])]", "[1,2,3,4,5]");
    check("[limit(0; 1, 2)]", "null");
    check("[limit(-1; 1, 2)]", "null");
    check("[first(range(10))]", "null");
    check("[first(empty)]", "null");
    check("[.[] | first(range(.))]", "[1,2,3]");
    check("[until(. > 100; . * 2)]", "1");
    check("[while(. < 100; . * 2)]", "1");
}

#[test]
fn try_catch() {
    check("try error(\"x\") catch .", "null");
    check("try (1, error(\"x\"), 3) catch .", "null");
    check("[.[] | try error catch .]", "[1,\"a\",null,{\"b\":2}]");
    check("(try (1, 2)) | error", "null");
    check("try ((1, 2) | error) catch .", "null");
    check("try error({a: 1}) catch .a", "null");
    check(
        "[.[] | try if . == 2 then error(\"x\") else . end catch \"caught\"]",
        "[1,2,3]",
    );
    check("try (try error(\"x\") catch error(\"y\")) catch .", "null");
    check("try error(null) catch .", "null");
    check("error(null)", "null");
    check("error({a:1})", "null");
    check("[.[] | (1 / .)?]", "[1,0,2]");
    check("[.[] | try (1 / .) catch .]", "[1,0,2]");
    check(".a[]", "{\"a\":5}");
    check("try .a[] catch .", "{\"a\":5}");
    check("[.[] | .a?]", "[1,{\"a\":2}]");
    check("[(1, 2) | try error catch .]", "null");
    check("[try (1, error(\"x\")) catch .]", "null");
    check("try error(\"\\u0000x\") catch .", "null");
    check("def f: try error(\"x\") catch .; [f, f]", "null");
    check("[.[] | try error(.) catch . ] | length", "[1,2]");
    check(
        "(1, 2) as $x | try (if $x == 1 then error(\"e\") else $x end) catch \"c\"",
        "null",
    );
}

#[test]
fn label_break() {
    check("[label $f | try break $f catch .]", "null");
    check("[label $f | 1, break $f, 2]", "null");
    check("[label $a | label $b | 1, break $b, 2], 3", "null");
    check(
        "[label $out | .[] | if . > 2 then ., break $out else . end]",
        "[1,2,3,4]",
    );
    check("[label $a | (label $b | 1, break $a, 2), 3]", "null");
    check(
        "[range(3) as $i | label $f | range(10) | if . > $i then break $f else . end]",
        "null",
    );
}

/// Outputs that depend on refcounts: jq writes into a uniquely owned array view in
/// place (revealing storage past the view's end), so these only match when the VM
/// holds exactly jq's references.
#[test]
fn refcount_dependent_outputs() {
    check("[1,2,3,4] | .[0:2] == .[2:4]", "null");
    check("[range(4)+1] | .[0:2] | .[3] = 9", "null");
    check("[range(4)+1] | .[0:2] as $x | $x | .[3] = 9", "null");
    check("[range(4)+1] | . as $keep | .[0:2] | .[3] = 9", "null");
    check("[range(4)+1] | (.[0:2] | .[3] = 9), .", "null");
    check("[range(4)+1] | .[1:3] | .[2] = 0", "null");
    check(".[0:2] | .[3] = 9", "[1,2,3,4]");
    check("reduce (.[0:2] | .[3] = 9) as $x (0; . + 1)", "[1,2,3,4]");
    check("[.[0:2][] ]", "[1,2,3,4]");
    check(".[0:1] | . + [7] | . as $a | $a", "[1,2,3]");
}

#[test]
fn deep_recursion() {
    // Tail calls reuse the frame; other recursion grows the (heap) stack.
    check(
        "def f: if . > 0 then . - 1 | f else \"done\" end; f",
        "200000",
    );
    check(
        "def f: if . > 0 then . - 1 | f | . + 1 else 0 end; f",
        "100000",
    );
    check("[limit(3; recurse(. + 1))]", "0");
    check("last(range(200000))", "null");
    check(
        "[recurse(if . < 50000 then . + 1 else empty end)] | length",
        "0",
    );
    check("reduce range(100000) as $x (0; . + $x)", "null");
}

#[test]
fn destructuring_alternatives() {
    check("[.[] as [$a] ?// $a | $a]", "[[1],2]");
    check("[.[] as [$a] ?// $a | $a]", "null");
    check(".[] as {a: $a} ?// [$a] ?// $a | $a", "[{\"a\":1},[2],3]");
    check(
        "[.[] as [$a] ?// $a | if $a == 1 then error(\"x\") else $a end]",
        "[[1],2]",
    );
    check(". as [$a] ?// $a | $a", "{\"b\":1}");
    check("[[3] | .[] as [$a] ?// $a | $a]", "null");
    // An error anywhere in the body (the rest of the pipeline) moves on to the next
    // alternative, even from outside an enclosing try's body.
    check("(. as [$a] ?// $a | $a) | error(tojson)", "[1]");
    check("try (. as [$a] ?// $a | $a) | error(tojson)", "[1]");
    check("[.[] as [$a] ?// $a | $a | error(tojson)]", "[1]");
    check("[.[] as [$a] ?// $a | $a] | error(tojson)", "[[1]]");
}

#[test]
fn paths() {
    check("path(.a[0].b)", "null");
    check("[path(..)]", "{\"a\":[1,{\"b\":2}]}");
    check("[paths]", "{\"a\":[1,{\"b\":2}]}");
    check("path(1)", "null");
    check("path(.a | . + 1)", "{\"a\":1}");
    check("try path(.a | . + 1) catch .", "{\"a\":1}");
    check("[path(.a[].b?)]", "{\"a\":[{\"b\":1},2]}");
    check("path(getpath([\"a\",\"b\"]))", "null");
    check("[paths(type == \"number\")]", "[1,[2]]");
    check(".a.b.c = 1", "null");
    check(".a[1:2] = [\"x\"]", "{\"a\":[1,2,3]}");
    check("del(.a, .b)", "{\"a\":1,\"b\":2,\"c\":3}");
    check("to_entries", "{\"a\":1,\"b\":2}");
    check("with_entries(.value += 1)", "{\"a\":1,\"b\":2}");
    check("(.. | numbers) |= . + 1", "[1,[2,{\"a\":3}]]");
    check(".[] += 10", "[1,2]");
    check("path(.[] | select(. > 1))", "[1,2,3]");
    check("[path(first(.a, .b))]", "null");
    check("path(.a as $x | .b)", "null");
    check("path(1 as $x | .b)", "null");
    check("try path(.[] | reverse) catch .", "[[1]]");
    check("path(reverse | .a)", "[]");
    check("[path(if .a then .b else .c end)]", "{\"a\":true}");
    check("path(empty)", "null");
    check("[path(.a // .b)]", "{\"a\":null}");
    check("[path(getpath([\"x\"]) | getpath([\"y\"]))]", "null");
    check("path(getpath([\"a\"]) | .b)", "null");
    check("getpath([\"a\",0,\"b\"])", "{\"a\":[{\"b\":7}]}");
}

#[test]
fn functions_and_recursion() {
    check("def f: . + 1; def g: f | f; [.[] | g]", "[1,2]");
    check("def f(g): [g, g]; f(.[])", "[1,2]");
    check("def f($a; $b): $a + $b; f(.[0]; .[1])", "[1,2]");
    check(
        "def fac: if . <= 1 then 1 else . * (. - 1 | fac) end; [.[] | fac]",
        "[1,5,10]",
    );
    check("def f: if . < 1000 then . + 1 | f else . end; f", "0");
    check("[recurse(if . < 3 then . + 1 else empty end)]", "0");
    check("def f(x): x * 2; f(f(.))", "3");
    check("def g: def h: . * 3; h + 1; g", "2");
    check(
        "def f: reduce .[] as $x (0; . + $x); [.[] | f]",
        "[[1,2],[3]]",
    );
    check("[.[] | tostream]", "[[1]]");
    check("def r: if . > 0 then . - 1 | r, . else . end; [r]", "3");
    check("[limit(5; repeat(1))]", "null");
    check("def f(a; b): a + b; f(.[]; 10, 20)", "[1,2]");
    check(
        "def outer(g): def f(x): x; def k: f(g); k; outer(1)",
        "null",
    );
}

#[test]
fn errors_inside_generators() {
    check(".[] | error", "[1,2]");
    check("[.[] | .a]", "[{\"a\":1},2]");
    check(".[] | .a", "[{\"a\":1},2]");
    check("{(.[]): 1}", "[\"a\",1]");
    check("[.[] | {(.): 1}]", "[\"a\",1]");
    check("reduce .[] as $x (0; . + $x)", "[1,\"a\"]");
    check("[foreach .[] as $x (0; . + $x)]", "[1,\"a\"]");
    check("first(error(\"x\"), 1)", "null");
    check("[limit(2; 1, error(\"x\"))]", "null");
    check("[limit(3; 1, error(\"x\"))]", "null");
    check("range(1; \"a\")", "null");
    check(".a + 1", "\"s\"");
}

#[test]
fn halting() {
    if !jq_ok() {
        return;
    }
    // halt stops the program; halt_error records its exit code and message.
    for (program, code, msg) in [
        ("1, halt, 2", None, None),
        ("\"bye\" | halt_error(3)", Some(3.0), Some("\"bye\"")),
        ("[1, halt_error(1)]", Some(1.0), Some("{\"a\":1}")),
    ] {
        for (_, bc) in bytecodes(program) {
            let mut jq = Jq::new(bc);
            jq.start(parse_sized(b"{\"a\":1}").unwrap(), 0);
            let outs: Vec<String> = (&mut jq).map(|r| r.unwrap().to_json()).collect();
            assert!(jq.halted(), "{program}");
            assert_eq!(jq.exit_code().and_then(Value::as_f64), code, "{program}");
            assert_eq!(
                jq.error_message().map(|v| v.to_json()),
                msg.map(str::to_string),
                "{program}"
            );
            if program.starts_with('1') {
                assert_eq!(outs, vec!["1".to_string()]);
            }
            // A new start resets the halt.
            jq.start(Value::Null, 0);
            assert!(!jq.halted());
        }
    }
}

#[test]
fn input_and_debug_callbacks() {
    if !jq_ok() {
        return;
    }
    for (_, bc) in bytecodes("[., input, (try input catch .)] | debug") {
        let mut jq = Jq::new(bc);
        let mut queue = vec![Value::from(2)];
        jq.set_input(Some(Box::new(move || queue.pop().map(Ok))));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen2 = seen.clone();
        jq.set_debug_cb(Some(Box::new(move |v: &Value| {
            seen2.borrow_mut().push(v.to_json())
        })));
        jq.start(Value::from(1), 0);
        let outs: Vec<String> = (&mut jq).map(|r| r.unwrap().to_json()).collect();
        assert_eq!(outs, vec!["[1,2,\"break\"]"]);
        assert_eq!(*seen.borrow(), vec!["[1,2,\"break\"]"]);
    }
}

#[test]
fn labels_count_across_inputs() {
    // next_label is never reset: the second input sees label 1.
    if !jq_ok() {
        return;
    }
    let want = jq_expect("[label $f | try break $f catch .]", "null null");
    for (_, bc) in bytecodes("[label $f | try break $f catch .]") {
        let mut jq = Jq::new(bc);
        let mut outs = Vec::new();
        for _ in 0..2 {
            jq.start(Value::Null, 0);
            outs.extend((&mut jq).map(|r| r.unwrap().to_json()));
        }
        assert_eq!(outs, want);
    }
}

// ---- traces ----------------------------------------------------------------------

/// Removes `JV_PRINT_REFCOUNT` annotations (` (<n>)` after a string, array or object).
pub(super) fn strip_refcounts(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b' '
            && i > 0
            && matches!(b[i - 1], b'"' | b']' | b'}')
            && b.get(i + 1) == Some(&b'(')
        {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 && b.get(j) == Some(&b')') {
                i = j + 1;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

/// A trace writer shared with the test (jq prints traces and results to stdout).
#[derive(Clone, Default)]
pub(super) struct Shared(pub(super) Rc<RefCell<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn check_trace(program: &str, input: &str, flags: u32) {
    if !jq_ok() {
        return;
    }
    let flag = if flags == JQ_DEBUG_TRACE_ALL {
        "--debug-trace=all"
    } else {
        "--debug-trace"
    };
    let want = run_jq(&["-c", flag, program], input);
    for (which, bc) in bytecodes(program) {
        let mut jq = Jq::new(bc);
        let out = Shared::default();
        jq.set_trace_writer(Some(Box::new(out.clone())));
        jq.start(parse_sized(input.as_bytes()).unwrap(), flags);
        let mut err = None;
        for r in &mut jq {
            match r {
                Ok(v) => {
                    let mut o = out.0.borrow_mut();
                    o.extend_from_slice(v.to_json().as_bytes());
                    o.push(b'\n');
                }
                Err(e) => err = Some(e),
            }
        }
        let got = String::from_utf8(out.0.borrow().clone()).unwrap();
        // Refcounts included: the VM holds the same references as jq. (Set
        // TRACE_IGNORE_REFCOUNTS to compare without them.) `type` returns shared
        // kind-name strings (jq allocates a fresh one each time), so programs using it
        // are compared without refcounts.
        let uses_type = ["type", "numbers", "strings", "arrays", "objects"]
            .iter()
            .any(|w| program.contains(w));
        let (want_stdout, got) =
            if uses_type || std::env::var_os("TRACE_IGNORE_REFCOUNTS").is_some() {
                (strip_refcounts(&want.stdout), strip_refcounts(&got))
            } else {
                (want.stdout.clone(), got)
            };
        if got != want_stdout {
            let g: Vec<&str> = got.lines().collect();
            let w: Vec<&str> = want_stdout.lines().collect();
            let first = g
                .iter()
                .zip(&w)
                .position(|(a, b)| a != b)
                .unwrap_or(g.len().min(w.len()));
            panic!(
                "trace differs for {program} ({which}) at line {first}:\n got: {:?}\nwant: {:?}\n(got {} lines, want {})",
                g.get(first),
                w.get(first),
                g.len(),
                w.len()
            );
        }
        assert_eq!(err.is_some(), want.code == 5, "{program}: {}", want.stderr);
    }
}

#[test]
fn traces_match_jq() {
    for (program, input) in [
        (".[] | . + 1", "[1,2]"),
        ("[limit(3; range(10))]", "null"),
        ("first(range(10))", "null"),
        ("try (1, error(\"x\"), 3) catch .", "null"),
        ("[label $f | try break $f catch .]", "null"),
        ("(try (1, 2)) | error", "null"),
        ("[.[] as [$a] ?// $a | $a]", "[[1],2]"),
        ("[paths]", "{\"a\":[1,{\"b\":2}]}"),
        ("path(.a | . + 1)", "{\"a\":1}"),
        ("reduce .[] as $x (0; . + $x)", "[1,2,3]"),
        ("[foreach .[] as $x (0; . + $x; [$x, .])]", "[1,2]"),
        ("def f: if . < 5 then . + 1 | f else . end; f", "0"),
        ("[.[] | (1 / .)?]", "[1,0]"),
        ("(.. | numbers) |= . + 1", "[1,[2]]"),
        ("{a: 1, b: .[]}", "[1,2]"),
        ("[range(0; 10; 3)]", "null"),
        ("$__loc__", "null"),
        ("[.[] | .a?]", "[1,{\"a\":2}]"),
    ] {
        check_trace(program, input, JQ_DEBUG_TRACE);
    }
    check_trace("[.[] | . * 2]", "[1,2]", JQ_DEBUG_TRACE_ALL);
    check_trace("reduce .[] as $x (0; . + $x)", "[1,2]", JQ_DEBUG_TRACE_ALL);
}

// ---- performance ------------------------------------------------------------------

/// `reduce` appending to an array or updating an object must stay linear, as in jq:
/// uniquely owned values are updated in place (LOADVN moves the accumulator out of its
/// variable, builtins receive their arguments by value).
#[test]
fn reduce_is_linear() {
    if !jq_ok() {
        return;
    }
    for (program, n, want) in [
        (
            "reduce range(.) as $x ([]; . + [$x]) | length",
            300_000,
            300_000,
        ),
        (
            "reduce range(.) as $x ([]; .[$x] = $x) | length",
            300_000,
            300_000,
        ),
        (
            "reduce range(.) as $x ({}; .[$x | tojson] = $x) | length",
            100_000,
            100_000,
        ),
        (
            "reduce range(.) as $x ({}; .[$x % 7 | tojson] += 1) | length",
            100_000,
            7,
        ),
        ("[range(.)] | length", 300_000, 300_000),
        (
            "[foreach range(.) as $x ([]; . + [$x]; length)] | length",
            100_000,
            100_000,
        ),
    ] {
        for (which, bc) in bytecodes(program) {
            let mut jq = Jq::new(bc);
            let t = std::time::Instant::now();
            jq.start(Value::from(n), 0);
            let outs: Vec<String> = (&mut jq).map(|r| r.unwrap().to_json()).collect();
            let dt = t.elapsed();
            assert_eq!(outs, vec![want.to_string()], "{program} ({which})");
            // Quadratic copying would take minutes; linear is well under a second in
            // release builds and a few seconds in debug builds.
            assert!(dt.as_secs() < 30, "{program} ({which}) took {dt:?}");
        }
    }
}
