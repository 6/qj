//! Runs jq 1.8.1's upstream test suites (`tests/jq_compat/*.test`, and qj's corpus with
//! `SUITES_CORPUS=1`) through the new core and compares stdout, the uncaught error and
//! the exit code with the jq binary. Two bytecode sources:
//!
//! * `*_on_jq_bytecode`: jq's own bytecode, rebuilt from `--debug-dump-disasm`, which
//!   measures the VM and builtins independently of our compiler;
//! * `*_on_our_compiler`: our compiler's bytecode, i.e. the whole ported core.
//!
//! The `traces` variants compare the entire `--debug-trace` output instead (every
//! instruction, its stack inputs and their refcounts, interleaved with the results).
//!
//! ```text
//! cargo test --release --lib upstream_suites_on_our_compiler -- --ignored --nocapture
//! cargo test --release --lib upstream_traces_on_our_compiler -- --ignored --nocapture
//! SUITES_VERBOSE=1 SUITES_FILTER=jq.test,man.test ...   # print failures, some files
//! SUITES_CORPUS=1 ...                                    # add tests/jq_compat/corpus
//! ```

use std::process::{Command, Stdio};
use std::rc::Rc;

use super::disasm;
use super::tests::{OVERRIDES, Shared, strip_refcounts};
use super::{InputSource, JQ_DEBUG_TRACE, Jq};
use crate::jq::lang::bytecode::Bytecode;
use crate::jq::lang::linker::JqAttrs;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::{DumpOptions, Error, ParseFlags, Parser, Value, dump_string};

/// One `program / input / outputs` case (`%%FAIL` blocks are compile-time tests and
/// are skipped).
struct Case {
    file: &'static str,
    line: usize,
    program: String,
    input: String,
}

fn skipline(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    rest.is_empty() || rest.starts_with('#')
}

/// jq_test.c's `run_jq_tests` reading loop.
fn parse_cases(file: &'static str, content: &str) -> Vec<Case> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut must_fail = false;
    while i < lines.len() {
        let l = lines[i];
        i += 1;
        if skipline(l) {
            continue;
        }
        if l == "%%FAIL" || l == "%%FAIL IGNORE MSG" {
            must_fail = true;
            continue;
        }
        let line = i;
        let program = l.to_string();
        if must_fail {
            must_fail = false;
            while i < lines.len() && !skipline(lines[i]) {
                i += 1;
            }
            continue;
        }
        let input = lines.get(i).copied().unwrap_or("").to_string();
        i += 1;
        while i < lines.len() && !skipline(lines[i]) {
            i += 1;
        }
        out.push(Case {
            file,
            line,
            program,
            input,
        });
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    /// Results (and, when tracing, the trace interleaved with them).
    stdout: String,
    /// The uncaught error's message (`jq: error (at ...): <msg>` without the prefix),
    /// `parse error: <msg>`, or the compile errors.
    error: Option<String>,
    exit: i32,
}

/// jq's test modules, for programs that import (jq_diff passes `-L modules` too).
fn modules_dir(program: &str) -> Option<String> {
    ["import", "include", "modulemeta", "get_search_list"]
        .iter()
        .any(|w| program.contains(w))
        .then(|| format!("{}/tests/jq_compat/modules", env!("CARGO_MANIFEST_DIR")))
}

/// Runs jq and returns its disassembly (if it compiled) and outcome.
fn run_jq(case: &Case, trace: bool) -> (Option<String>, Outcome) {
    let mut args: Vec<String> = vec!["-c".into(), "--debug-dump-disasm".into()];
    if trace {
        args.push("--debug-trace".into());
    }
    if let Some(dir) = modules_dir(&case.program) {
        args.extend(["-L".into(), dir]);
    }
    args.push(case.program.clone());
    let mut child = Command::new("jq")
        .args(&args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq");
    {
        use std::io::Write;
        let mut si = child.stdin.take().unwrap();
        let _ = si.write_all(case.input.as_bytes());
        let _ = si.write_all(b"\n");
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let exit = out.status.code().unwrap_or(-1);
    let Some((disasm, rest)) = stdout.split_once("\n\n") else {
        // Didn't compile.
        return (
            None,
            Outcome {
                stdout: String::new(),
                error: Some(stderr),
                exit,
            },
        );
    };
    let error = stderr.lines().rev().find_map(|l| {
        if let Some(m) = l.strip_prefix("jq: parse error: ") {
            return Some(format!("parse error: {m}"));
        }
        // "jq: error (at <pos>): <msg>" or "jq: error (at <pos>) (not a string): <v>"
        let (_, r) = l.strip_prefix("jq: error (at ")?.split_once(')')?;
        Some(match r.strip_prefix(": ") {
            Some(m) => m.to_string(),
            None => r.strip_prefix(' ').unwrap_or(r).to_string(),
        })
    });
    (
        Some(disasm.to_string()),
        Outcome {
            stdout: rest.to_string(),
            error: if exit == 5 { error } else { None },
            exit,
        },
    )
}

/// The rest of the input values, as `input`/`inputs` see them.
/// (jq reads a test's one-line stdin in one chunk: `input_line_number` is the number
/// of newlines in it, and `input_filename` is `"<stdin>"`.)
struct Inputs(std::collections::VecDeque<Result<Value, Error>>, usize);

impl InputSource for Inputs {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        self.0.pop_front()
    }
    fn current_filename(&self) -> Option<Value> {
        Some(Value::from("<stdin>"))
    }
    fn current_line(&self) -> Value {
        Value::from(self.1)
    }
}

/// The attributes for `case` (jq run from the repository root, maybe with `-L`).
fn attrs(case: &Case) -> JqAttrs {
    let mut a = JqAttrs::new(".");
    if let Some(dir) = modules_dir(&case.program) {
        a.lib_dirs = Value::from(vec![Value::from(dir)]);
    }
    a.prog_origin = Value::from(env!("CARGO_MANIFEST_DIR"));
    a
}

/// Runs the VM on `bc` like jq's main loop: each input value in turn, `input`
/// reading ahead, stopping at an error or halt.
fn run_vm(bc: Rc<Bytecode>, case: &Case, trace: bool) -> Outcome {
    let mut jq = Jq::new(bc);
    jq.set_jq_attrs(&attrs(case));
    let mut parser = Parser::new(ParseFlags::default());
    let text = format!("{}\n", case.input);
    parser.set_buf(text.as_bytes(), false);
    let mut values = std::collections::VecDeque::new();
    while let Some(v) = parser.next() {
        values.push_back(v);
    }
    // jq prints traces and results on stdout, in order.
    let out = Shared::default();
    if trace {
        jq.set_trace_writer(Some(Box::new(out.clone())));
    }
    let mut exit = 0;
    let mut error = None;
    let lines = text.bytes().filter(|&b| b == b'\n').count();
    jq.set_input(Some(Box::new(Inputs(values, lines))));
    loop {
        let next = jq.take_input().and_then(|mut i| {
            let v = i.next_input();
            jq.set_input(Some(i));
            v
        });
        let v = match next {
            None => break,
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                // main.c: "jq: parse error: <msg>", exit 5.
                error = Some(format!("parse error: {e}"));
                exit = 5;
                break;
            }
        };
        jq.start(v, if trace { JQ_DEBUG_TRACE } else { 0 });
        for r in &mut jq {
            match r {
                Ok(v) => {
                    let mut o = out.0.borrow_mut();
                    o.extend_from_slice(dump_string(&v, &DumpOptions::default()).as_bytes());
                    o.push(b'\n');
                }
                Err(e) => {
                    error = Some(match e.value() {
                        Value::String(s) => s.as_str().to_string(),
                        v => format!("(not a string): {}", v.to_json()),
                    });
                    exit = 5;
                }
            }
        }
        if jq.halted() {
            // main.c: jq_exit(ret) exits with ret if positive, else 0.
            exit = match jq.exit_code() {
                None => 0,
                Some(Value::Number(n)) => (n.value() as i32).max(0),
                Some(_) => 5,
            };
            break;
        }
    }
    let stdout = String::from_utf8_lossy(&out.0.borrow()).into_owned();
    Outcome {
        stdout,
        error,
        exit,
    }
}

/// Where two outcomes first differ, for the report.
fn first_difference(want: &Outcome, got: &Outcome) -> String {
    if want.stdout != got.stdout {
        let (w, g): (Vec<&str>, Vec<&str>) =
            (want.stdout.lines().collect(), got.stdout.lines().collect());
        let i = w
            .iter()
            .zip(&g)
            .position(|(a, b)| a != b)
            .unwrap_or(w.len().min(g.len()));
        format!(
            "stdout line {} (of {} / {}):\n    want: {:?}\n    got:  {:?}\n    errors: {:?} / {:?}",
            i + 1,
            w.len(),
            g.len(),
            w.get(i),
            g.get(i),
            want.error,
            got.error
        )
    } else {
        format!("want {want:?}\n    got  {got:?}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Core {
    /// jq's bytecode from `--debug-dump-disasm`.
    JqBytecode,
    /// Our compiler.
    Ours,
}

/// Runs one case; `None` when jq doesn't compile it and we're on jq's bytecode.
fn run_case(case: &Case, core: Core, trace: bool) -> Option<(bool, String)> {
    let head = format!(
        "{}:{}: {}\n  input: {}",
        case.file, case.line, case.program, case.input
    );
    let (dis, want) = run_jq(case, trace);
    let got = match core {
        Core::JqBytecode => {
            let dis = dis?;
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_vm(disasm::load(&dis, OVERRIDES), case, trace)
            }))
        }
        Core::Ours => {
            let mut opts = CompileOptions::new(".");
            opts.attrs = attrs(case);
            match jq_compile_args(case.program.as_bytes(), &opts) {
                Ok(bc) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_vm(bc, case, trace)
                })),
                Err(e) => Ok(Outcome {
                    stdout: String::new(),
                    error: Some(e.render()),
                    exit: 3,
                }),
            }
        }
    };
    // SUITES_IGNORE_REFCOUNTS=1 compares traces without the ` (<refcount>)`s.
    let (want, got) = if trace && std::env::var_os("SUITES_IGNORE_REFCOUNTS").is_some() {
        let strip = |o: Outcome| Outcome {
            stdout: strip_refcounts(&o.stdout),
            ..o
        };
        (strip(want), got.map(strip))
    } else {
        (want, got)
    };
    Some(match got {
        Ok(got) => (
            got == want,
            format!("{head}\n  {}", first_difference(&want, &got)),
        ),
        Err(_) => (false, format!("{head}\n  PANICKED")),
    })
}

fn run_suites(core: Core, trace: bool) {
    let root = env!("CARGO_MANIFEST_DIR");
    let filter = std::env::var("SUITES_FILTER").ok();
    let verbose = std::env::var_os("SUITES_VERBOSE").is_some();
    let mut files: Vec<&'static str> = vec![
        "jq.test",
        "man.test",
        "manonig.test",
        "onig.test",
        "base64.test",
        "uri.test",
        "optional.test",
    ];
    if std::env::var_os("SUITES_CORPUS").is_some() {
        let mut corpus: Vec<String> = std::fs::read_dir(format!("{root}/tests/jq_compat/corpus"))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".test"))
            .map(|n| format!("corpus/{n}"))
            .collect();
        corpus.sort();
        files.extend(corpus.into_iter().map(|s| &*Box::leak(s.into_boxed_str())));
    }
    let mut cases = Vec::new();
    for f in files {
        if filter
            .as_deref()
            .is_some_and(|flt| !flt.split(',').any(|x| f.contains(x)))
        {
            continue;
        }
        let content = std::fs::read_to_string(format!("{root}/tests/jq_compat/{f}")).unwrap();
        cases.extend(parse_cases(f, &content));
    }
    let nthreads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunks: Vec<&[Case]> = cases.chunks(cases.len().div_ceil(nthreads)).collect();
    // (file, Some(passed) or None when skipped, detail)
    let results: Vec<(&'static str, Option<bool>, String)> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|case| match run_case(case, core, trace) {
                            Some((ok, detail)) => (case.file, Some(ok), detail),
                            None => (case.file, None, String::new()),
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    // (file, passed, total, skipped)
    let mut summary: Vec<(&str, usize, usize, usize)> = Vec::new();
    for (file, ok, detail) in &results {
        let i = match summary.iter().position(|e| e.0 == *file) {
            Some(i) => i,
            None => {
                summary.push((file, 0, 0, 0));
                summary.len() - 1
            }
        };
        let e = &mut summary[i];
        match ok {
            Some(ok) => {
                e.1 += *ok as usize;
                e.2 += 1;
                if verbose && !ok {
                    eprintln!("{detail}");
                }
            }
            None => e.3 += 1,
        }
    }
    let (mut pass, mut total, mut skipped) = (0, 0, 0);
    for (file, p, t, s) in &summary {
        let note = if *s > 0 {
            format!(" ({s} not compiled by jq, skipped)")
        } else {
            String::new()
        };
        eprintln!("{file:>14}: {p}/{t}{note}");
        pass += p;
        total += t;
        skipped += s;
    }
    eprintln!("{:>14}: {pass}/{total} ({skipped} skipped)", "total");
}

#[test]
#[ignore]
fn upstream_suites_on_jq_bytecode() {
    run_suites(Core::JqBytecode, false);
}

#[test]
#[ignore]
fn upstream_traces_on_jq_bytecode() {
    run_suites(Core::JqBytecode, true);
}

#[test]
#[ignore]
fn upstream_suites_on_our_compiler() {
    run_suites(Core::Ours, false);
}

#[test]
#[ignore]
fn upstream_traces_on_our_compiler() {
    run_suites(Core::Ours, true);
}
