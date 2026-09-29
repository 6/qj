//! End to end against the jq 1.8.1 binary: NDJSON-like files through the
//! reader and the parallel engine, each worker running its own compiled
//! program on the port's VM, with `main.c`'s `process()` output, error
//! messages and exit status. (A model for the CLI's worker.)

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::process::Command;
use std::rc::Rc;

use super::generate::Gen;
use super::{Rng, show};
use crate::io::parallel::{
    self, EngineOptions, RecordMeta, RecordSink, RecordWorker, WorkerFactory,
};
use crate::io::reader::{InputReader, ReaderOptions};
use crate::jq::lang::bytecode::Bytecode;
use crate::jq::lang::execute::{InputSource, Jq};
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::print::dump_to_vec;
use crate::jq::value::{DumpOptions, Error, Value};

const JQ_OK_NULL_KIND: i32 = -1;
const JQ_OK_NO_OUTPUT: i32 = -4;
const JQ_ERROR_UNKNOWN: i32 = 5;

struct JqWorker {
    jq: Jq,
    /// What `input_filename`/`input_line_number` report for the current
    /// record (`input` itself isn't parallel-safe, so it sees no inputs).
    position: Rc<std::cell::RefCell<(Option<Value>, u64)>>,
}

struct RecordPosition(Rc<std::cell::RefCell<(Option<Value>, u64)>>);

impl InputSource for RecordPosition {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        None
    }
    fn current_filename(&self) -> Option<Value> {
        self.0.borrow().0.clone()
    }
    fn current_line(&self) -> Value {
        Value::from(self.0.borrow().1 as f64)
    }
}

impl RecordWorker for JqWorker {
    /// main.c `process()` with `-c`, without halt (not parallel-safe).
    fn process(
        &mut self,
        value: Value,
        meta: &RecordMeta<'_>,
        out: &mut Vec<u8>,
        err: &mut Vec<u8>,
    ) -> i32 {
        let mut ret = JQ_OK_NO_OUTPUT;
        *self.position.borrow_mut() = (meta.filename.map(Value::from), meta.line);
        self.jq.start(value, 0);
        while let Some(r) = self.jq.next() {
            match r {
                Ok(v) => {
                    ret = if matches!(v, Value::Null | Value::Bool(false)) {
                        JQ_OK_NULL_KIND
                    } else {
                        0
                    };
                    dump_to_vec(&v, &DumpOptions::compact(), out);
                    out.push(b'\n');
                }
                Err(e) => {
                    let pos = match meta.filename {
                        Some(f) => format!("{f}:{}", meta.line),
                        None => "<unknown>".to_owned(),
                    };
                    match e.value() {
                        Value::String(s) => {
                            err.extend_from_slice(format!("jq: error (at {pos}): ").as_bytes());
                            err.extend_from_slice(s.as_bytes());
                            err.push(b'\n');
                        }
                        v => err.extend_from_slice(
                            format!("jq: error (at {pos}) (not a string): {}\n", v.to_json())
                                .as_bytes(),
                        ),
                    }
                    ret = JQ_ERROR_UNKNOWN;
                    break;
                }
            }
        }
        ret
    }
}

struct JqFactory {
    program: Vec<u8>,
}

impl WorkerFactory for JqFactory {
    type Worker = JqWorker;
    fn new_worker(&self) -> JqWorker {
        let bc: Rc<Bytecode> =
            jq_compile_args(&self.program, &CompileOptions::new("/usr/bin")).expect("compiles");
        let mut jq = Jq::new(bc);
        let position = Rc::new(std::cell::RefCell::new((None, 0)));
        jq.set_input(Some(Box::new(RecordPosition(position.clone()))));
        JqWorker { jq, position }
    }
}

/// main.c's loop state: `ret` and `last_result`, and the output.
struct MainLoop {
    out: Vec<u8>,
    err: Vec<u8>,
    ret: i32,
}

impl RecordSink for MainLoop {
    fn record(&mut self, out: &[u8], err: &[u8], status: i32) -> ControlFlow<()> {
        self.out.extend_from_slice(out);
        self.err.extend_from_slice(err);
        self.ret = status;
        ControlFlow::Continue(())
    }
    fn parse_error(&mut self, e: Error) -> ControlFlow<()> {
        self.ret = JQ_ERROR_UNKNOWN;
        self.err
            .extend_from_slice(format!("jq: parse error: {e}\n").as_bytes());
        ControlFlow::Break(())
    }
}

fn run_ours(program: &str, paths: &[OsString], threads: usize) -> (Vec<u8>, Vec<u8>, i32) {
    let mut reader = InputReader::new(paths.to_vec(), ReaderOptions::default());
    let msgs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = msgs.clone();
    reader.set_message_sink(Box::new(move |m| sink.borrow_mut().extend(m.render("jq"))));
    let factory = JqFactory {
        program: program.as_bytes().to_vec(),
    };
    let mut main = MainLoop {
        out: Vec::new(),
        err: Vec::new(),
        ret: JQ_OK_NO_OUTPUT,
    };
    let opts = EngineOptions {
        threads,
        min_window: 0,
        max_job_bytes: 2000,
        ..EngineOptions::default()
    };
    parallel::run(&mut reader, &factory, &mut main, &opts);
    let mut err = main.err;
    // (Input messages and record errors can interleave in jq's stderr; these
    // tests have no failing inputs.)
    err.extend(msgs.borrow().iter());
    let rc = if reader.failures() != 0 {
        2
    } else if main.ret > 0 {
        main.ret
    } else {
        0
    };
    (main.out, err, rc)
}

fn run_jq(program: &str, dir: &std::path::Path, names: &[String]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new("jq")
        .current_dir(dir)
        .arg("-c")
        .arg(program)
        .args(names)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

fn ndjson(r: &mut Rng, weird: usize, records: usize) -> Vec<u8> {
    let mut g = Gen { r, weird };
    let mut out = Vec::new();
    for i in 0..records {
        if i % 5 == 0 {
            // objects with fields the programs look at
            out.extend_from_slice(
                format!("{{\"a\":{},\"b\":{{\"c\":\"x{}\"}}}}", i % 7, i).as_bytes(),
            );
        } else {
            g.value(&mut out, 0, false);
        }
        out.push(b'\n');
    }
    out
}

const PROGRAMS: &[&str] = &[
    ".",
    ".a",
    "select(.a? > 2)",
    "[input_line_number, input_filename]",
    "keys?",
    "type, length",
    ".b.c",
    ".[0]?",
    "if . == null then empty else 1 end",
    "tostring",
];

fn check(seed: u64, records: usize, weird: usize, threads: usize) -> Result<(), String> {
    let mut r = Rng(seed.wrapping_mul(0x94D049BB133111EB) | 1);
    let dir = tempfile::tempdir().unwrap();
    let parts = 1 + r.below(3);
    let mut names = Vec::new();
    for i in 0..parts {
        let name = format!("in{i}.ndjson");
        std::fs::write(dir.path().join(&name), ndjson(&mut r, weird, records)).unwrap();
        names.push(name);
    }
    let paths: Vec<OsString> = names
        .iter()
        .map(|n| OsString::from(dir.path().join(n)))
        .collect();
    let prefix = format!("{}/", dir.path().display());
    for program in PROGRAMS {
        let want = run_jq(program, dir.path(), &names);
        let (out, err, rc) = run_ours(program, &paths, threads);
        // input_filename is the path we were given: strip the directory.
        let out = String::from_utf8_lossy(&out)
            .replace(&prefix, "")
            .into_bytes();
        let err = String::from_utf8_lossy(&err)
            .replace(&prefix, "")
            .into_bytes();
        if (&out, &err, rc) != (&want.0, &want.1, want.2) {
            let first = out
                .iter()
                .zip(want.0.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            let from = first.saturating_sub(100);
            return Err(format!(
                "seed {seed} {program:?}: rc {rc} vs jq {}\n  out ...{:?}\n  jq  ...{:?}\n  err {:?}\n  jq  {:?}",
                want.2,
                show(&out[from..(first + 200).min(out.len())]),
                show(&want.0[from..(first + 200).min(want.0.len())]),
                show(&err[..err.len().min(500)]),
                show(&want.1[..want.1.len().min(500)]),
            ));
        }
    }
    Ok(())
}

fn jq_available() -> bool {
    Command::new("jq")
        .arg("--version")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "jq-1.8.1")
}

/// Runs `f` on a thread with a jq-sized stack (the VM runs sequential
/// records on the calling thread).
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn engine_with_vm_matches_jq() {
    if !jq_available() {
        eprintln!("skipped: jq 1.8.1 not on PATH");
        return;
    }
    let failures = big_stack(|| {
        let mut failures = Vec::new();
        for seed in 0..3 {
            if let Err(e) = check(seed, 300, [0, 20, 80][seed as usize], 3) {
                failures.push(e);
            }
        }
        failures
    });
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// `cargo test --release --lib engine_with_vm_matches_jq_long -- --ignored`
#[test]
#[ignore]
fn engine_with_vm_matches_jq_long() {
    if !jq_available() {
        eprintln!("skipped: jq 1.8.1 not on PATH");
        return;
    }
    let failures = big_stack(|| {
        let mut failures = Vec::new();
        for seed in 100..160 {
            if let Err(e) = check(
                seed,
                3000,
                [0, 5, 30, 100][seed as usize % 4],
                1 + seed as usize % 8,
            ) {
                failures.push(e);
                if failures.len() >= 3 {
                    break;
                }
            }
        }
        failures
    });
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
