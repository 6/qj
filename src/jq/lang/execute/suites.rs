//! Runs jq 1.8.1's upstream test suites (`tests/jq_compat/*.test`) through the VM on
//! jq's own bytecode (rebuilt from `--debug-dump-disasm`), comparing stdout, the
//! uncaught error and the exit code with the jq binary. This measures the VM (and the
//! builtins) independently of our compiler. The trace variant compares the whole
//! `--debug-trace` output (every instruction, stack and refcount) instead:
//!
//! ```text
//! cargo test --release --lib upstream_suites_on_jq_bytecode -- --ignored --nocapture
//! cargo test --release --lib upstream_traces_on_jq_bytecode -- --ignored --nocapture
//! SUITES_VERBOSE=1 SUITES_FILTER=jq.test ...   # print failures, one suite
//! SUITES_NO_OVERRIDES=1 ...                    # no stand-ins for unported builtins
//! ```

use std::process::{Command, Stdio};

use super::disasm;
use super::tests::{OVERRIDES, Shared};
use super::{InputSource, JQ_DEBUG_TRACE, Jq};
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
    /// The uncaught error's message (`jq: error (at ...): <msg>` without the prefix).
    error: Option<String>,
    exit: i32,
}

/// Runs jq and returns its disassembly and outcome.
fn run_jq(case: &Case, trace: bool) -> Option<(String, Outcome)> {
    let mut args = vec!["-c", "--debug-dump-disasm"];
    if trace {
        args.push("--debug-trace");
    }
    args.push(&case.program);
    let mut child = Command::new("jq")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        let mut si = child.stdin.take()?;
        let _ = si.write_all(case.input.as_bytes());
        let _ = si.write_all(b"\n");
    }
    let out = child.wait_with_output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let (disasm, rest) = stdout.split_once("\n\n")?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let exit = out.status.code().unwrap_or(-1);
    let error = stderr.lines().rev().find_map(|l| {
        if let Some(m) = l.strip_prefix("jq: parse error: ") {
            return Some(format!("parse error: {m}"));
        }
        let (_, r) = l.strip_prefix("jq: error (at ")?.split_once(')')?;
        Some(r.strip_prefix(": ").unwrap_or(r).trim_start().to_string())
    });
    Some((
        disasm.to_string(),
        Outcome {
            stdout: rest.to_string(),
            error: if exit == 5 { error } else { None },
            exit,
        },
    ))
}

/// The rest of the input values, as `input`/`inputs` see them.
struct Inputs(std::collections::VecDeque<Result<Value, Error>>);

impl InputSource for Inputs {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        self.0.pop_front()
    }
    fn current_filename(&self) -> Option<Value> {
        Some(Value::Null)
    }
}

/// Runs the VM like jq's main loop: each input value in turn, `input` reading ahead.
fn run_vm(disasm_text: &str, input: &str, overrides: bool, trace: bool) -> Outcome {
    let bc = disasm::load(disasm_text, if overrides { OVERRIDES } else { &[] });
    let mut jq = Jq::new(bc);
    let mut parser = Parser::new(ParseFlags::default());
    let text = format!("{input}\n");
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
    jq.set_input(Some(Box::new(Inputs(values))));
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

fn run_suites(trace: bool) {
    let root = env!("CARGO_MANIFEST_DIR");
    let filter = std::env::var("SUITES_FILTER").ok();
    let verbose = std::env::var_os("SUITES_VERBOSE").is_some();
    let overrides = std::env::var_os("SUITES_NO_OVERRIDES").is_none();
    let mut files: Vec<&'static str> = vec![
        "jq.test",
        "man.test",
        "manonig.test",
        "onig.test",
        "base64.test",
        "uri.test",
        "optional.test",
    ];
    // qj's corpus (same format, no expected outputs) with SUITES_CORPUS=1.
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
    // (file, Some(passed) or None when jq itself doesn't compile the program, detail)
    let results: Vec<(&'static str, Option<bool>, String)> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                s.spawn(move || {
                    let mut out = Vec::new();
                    for case in chunk.iter() {
                        let head = format!(
                            "{}:{}: {}\n  input: {}",
                            case.file, case.line, case.program, case.input
                        );
                        let Some((dis, want)) = run_jq(case, trace) else {
                            out.push((case.file, None, format!("{head}\n  jq failed")));
                            continue;
                        };
                        let got = std::panic::catch_unwind(|| {
                            run_vm(&dis, &case.input, overrides, trace)
                        });
                        let (ok, detail) = match got {
                            Ok(got) => (
                                got == want,
                                format!("{head}\n  {}", first_difference(&want, &got)),
                            ),
                            Err(_) => (false, format!("{head}\n  PANICKED")),
                        };
                        out.push((case.file, Some(ok), detail));
                    }
                    out
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
            format!(" ({s} not compiled by jq)")
        } else {
            String::new()
        };
        eprintln!("{file:>14}: {p}/{t}{note}");
        pass += p;
        total += t;
        skipped += s;
    }
    eprintln!(
        "{:>14}: {pass}/{total} ({skipped} not compiled by jq)",
        "total"
    );
}

#[test]
#[ignore]
fn upstream_suites_on_jq_bytecode() {
    run_suites(false);
}

#[test]
#[ignore]
fn upstream_traces_on_jq_bytecode() {
    run_suites(true);
}
