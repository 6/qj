//! Runs jq 1.8.1's upstream test suites (`tests/jq_compat/*.test`) through the VM on
//! jq's own bytecode (rebuilt from `--debug-dump-disasm`), comparing stdout and the
//! uncaught error with the jq binary. This measures the VM (and the builtins) without
//! depending on our compiler:
//!
//! ```text
//! cargo test --release --lib upstream_suites_on_jq_bytecode -- --ignored --nocapture
//! SUITES_VERBOSE=1 SUITES_FILTER=jq.test ...   # print failures, one suite
//! ```

use std::process::{Command, Stdio};

use super::disasm;
use super::tests::OVERRIDES;
use super::{InputSource, Jq};
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
    stdout: String,
    /// The uncaught error's message (`jq: error (at ...): <msg>` without the prefix).
    error: Option<String>,
    exit: i32,
}

/// Runs jq and returns its disassembly and outcome.
fn run_jq(case: &Case) -> Option<(String, Outcome)> {
    let mut child = Command::new("jq")
        .args(["-c", "--debug-dump-disasm", &case.program])
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
    let error = stderr
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("jq: error (at "))
        .and_then(|r| r.split_once(')'))
        .map(|(_, r)| r.strip_prefix(": ").unwrap_or(r).trim_start().to_string());
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
fn run_vm(disasm_text: &str, input: &str, overrides: bool) -> Outcome {
    let bc = disasm::load(disasm_text, if overrides { OVERRIDES } else { &[] });
    let mut jq = Jq::new(bc);
    let mut parser = Parser::new(ParseFlags::default());
    let text = format!("{input}\n");
    parser.set_buf(text.as_bytes(), false);
    let mut values = std::collections::VecDeque::new();
    while let Some(v) = parser.next() {
        values.push_back(v);
    }
    let mut stdout = String::new();
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
                // jq prints parse errors and exits 2.
                error = Some(format!("{e}"));
                exit = 2;
                break;
            }
        };
        jq.start(v, 0);
        for r in &mut jq {
            match r {
                Ok(v) => {
                    stdout.push_str(&dump_string(&v, &DumpOptions::default()));
                    stdout.push('\n');
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
            exit = match jq.exit_code() {
                None => 0,
                Some(Value::Number(n)) => n.value() as i32,
                Some(_) => 5,
            };
            break;
        }
    }
    Outcome {
        stdout,
        error,
        exit,
    }
}

#[test]
#[ignore]
fn upstream_suites_on_jq_bytecode() {
    let root = env!("CARGO_MANIFEST_DIR");
    let filter = std::env::var("SUITES_FILTER").ok();
    let verbose = std::env::var_os("SUITES_VERBOSE").is_some();
    let overrides = std::env::var_os("SUITES_NO_OVERRIDES").is_none();
    let files: &[&'static str] = &[
        "jq.test",
        "man.test",
        "manonig.test",
        "onig.test",
        "base64.test",
        "uri.test",
        "optional.test",
    ];
    let mut cases = Vec::new();
    for f in files {
        if filter.as_deref().is_some_and(|flt| !f.contains(flt)) {
            continue;
        }
        let content = std::fs::read_to_string(format!("{root}/tests/jq_compat/{f}")).unwrap();
        cases.extend(parse_cases(f, &content));
    }
    let nthreads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunks: Vec<&[Case]> = cases.chunks(cases.len().div_ceil(nthreads)).collect();
    let results: Vec<(&'static str, bool, String)> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                s.spawn(move || {
                    let mut out = Vec::new();
                    for case in chunk.iter() {
                        let Some((dis, want)) = run_jq(case) else {
                            out.push((
                                case.file,
                                false,
                                format!("{}:{}: jq failed", case.file, case.line),
                            ));
                            continue;
                        };
                        let got = std::panic::catch_unwind(|| run_vm(&dis, &case.input, overrides));
                        let (ok, detail) = match got {
                            Ok(got) => (
                                got == want,
                                format!(
                                    "{}:{}: {}\n  input: {}\n  want: {:?}\n  got:  {:?}",
                                    case.file, case.line, case.program, case.input, want, got
                                ),
                            ),
                            Err(_) => (
                                false,
                                format!("{}:{}: {} PANICKED", case.file, case.line, case.program),
                            ),
                        };
                        out.push((case.file, ok, detail));
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
    let mut summary: Vec<(&str, usize, usize)> = Vec::new();
    for (file, ok, detail) in &results {
        match summary.iter_mut().find(|(f, _, _)| f == file) {
            Some(e) => {
                e.1 += *ok as usize;
                e.2 += 1;
            }
            None => summary.push((file, *ok as usize, 1)),
        }
        if verbose && !ok {
            eprintln!("{detail}");
        }
    }
    let (mut pass, mut total) = (0, 0);
    for (file, p, t) in &summary {
        eprintln!("{file:>14}: {p}/{t}");
        pass += p;
        total += t;
    }
    eprintln!("{:>14}: {pass}/{total}", "total");
}
