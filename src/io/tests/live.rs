//! The input layer against the jq 1.8.1 binary (skipped when `jq` on PATH
//! isn't 1.8.1): real files in a temporary directory, missing files,
//! directories, and standard input.
//!
//! Two programs observe the reader:
//! * main loop: `jq -c '[., input_filename, input_line_number]' FILES...`,
//!   which stops at the first parse error (or after an input fails);
//! * input loop: `jq -nc` with a program that calls `input` until it
//!   reports "break", catching parse errors (so reading continues after
//!   them, like `try input`).

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use super::reference::RefInput;
use super::{Delivery, MemFile, Rng, mem_reader, show};
use crate::io::reader::{InputReader, ReaderOptions};
use crate::jq::value::{DumpOptions, ParseFlags, Value, dump_string};

const LOOP: &str = r#"def loop($n):
  if $n > 300 then empty
  else (try [input, input_filename, input_line_number]
        catch ["ERR", ., input_filename, input_line_number]) as $x
    | $x, (if $x[0] == "ERR" and $x[1] == "break" then empty else loop($n + 1) end)
  end;
loop(0)"#;

fn jq() -> Option<&'static str> {
    static JQ: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *JQ.get_or_init(|| {
        let out = Command::new("jq").arg("--version").output().ok()?;
        (String::from_utf8_lossy(&out.stdout).trim() == "jq-1.8.1").then_some("jq")
    })
}

/// An input for a scenario.
#[derive(Clone)]
enum F {
    Data(&'static str, Vec<u8>),
    Missing(&'static str),
    Dir(&'static str),
    /// Standard input (only as the sole input).
    Stdin(Vec<u8>),
}

impl F {
    fn name(&self) -> &'static str {
        match self {
            F::Data(n, _) | F::Missing(n) | F::Dir(n) => n,
            F::Stdin(_) => "-",
        }
    }
}

fn d(name: &'static str, s: &str) -> F {
    F::Data(name, s.as_bytes().to_vec())
}

#[derive(Clone, Copy)]
struct Flags {
    raw: bool,
    slurp: bool,
    seq: bool,
    stream: bool,
}

const JSON: Flags = Flags {
    raw: false,
    slurp: false,
    seq: false,
    stream: false,
};

impl Flags {
    fn argv(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.raw {
            v.push("-R");
        }
        if self.slurp {
            v.push("-s");
        }
        if self.seq {
            v.push("--seq");
        }
        if self.stream {
            v.push("--stream");
        }
        v
    }

    fn opts(&self) -> ReaderOptions {
        ReaderOptions {
            raw: self.raw,
            slurp: self.slurp,
            seq: self.seq,
            stream: self.stream,
            stream_errors: false,
        }
    }
}

/// What jq prints, from anything that behaves like its input state.
trait Inputs {
    fn next(&mut self) -> Option<Result<Value, String>>;
    fn filename(&self) -> Value;
    fn line(&self) -> u64;
    fn failures(&self) -> usize;
    fn messages(&mut self) -> Vec<u8>;
}

struct Ours(InputReader, std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl Inputs for Ours {
    fn next(&mut self) -> Option<Result<Value, String>> {
        self.0.next().map(|r| r.map_err(|e| e.to_string()))
    }
    fn filename(&self) -> Value {
        self.0.current_filename()
    }
    fn line(&self) -> u64 {
        self.0.current_line()
    }
    fn failures(&self) -> usize {
        self.0.failures()
    }
    fn messages(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.1.borrow_mut())
    }
}

impl Inputs for RefInput {
    fn next(&mut self) -> Option<Result<Value, String>> {
        self.next_input().map(|r| r.map_err(|e| e.to_string()))
    }
    fn filename(&self) -> Value {
        self.filename_value()
    }
    fn line(&self) -> u64 {
        self.current_line
    }
    fn failures(&self) -> usize {
        self.failures
    }
    fn messages(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.messages)
    }
}

fn triple(v: Value, f: Value, l: u64) -> Value {
    Value::from(vec![v, f, Value::from(l as f64)])
}

fn print(v: &Value, seq: bool, out: &mut Vec<u8>) {
    if seq {
        out.push(0x1e);
    }
    out.extend_from_slice(dump_string(v, &DumpOptions::compact()).as_bytes());
    out.push(b'\n');
}

/// `main.c`'s loop with `[., input_filename, input_line_number]`.
fn main_loop(r: &mut dyn Inputs, seq: bool) -> (Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while r.failures() == 0 {
        let next = r.next();
        err.extend(r.messages());
        match next {
            Some(Ok(v)) => print(&triple(v, r.filename(), r.line()), seq, &mut out),
            Some(Err(e)) if seq => {
                err.extend_from_slice(format!("jq: ignoring parse error: {e}\n").as_bytes())
            }
            Some(Err(e)) => {
                err.extend_from_slice(format!("jq: parse error: {e}\n").as_bytes());
                break;
            }
            None => break,
        }
    }
    err.extend(r.messages());
    (out, err)
}

/// `LOOP` under `-n`.
fn input_loop(r: &mut dyn Inputs, seq: bool) -> (Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    for _ in 0..=300 {
        let next = r.next();
        err.extend(r.messages());
        let (f, l) = (r.filename(), r.line());
        match next {
            Some(Ok(v)) => print(&triple(v, f, l), seq, &mut out),
            Some(Err(e)) => {
                let x = Value::from(vec![
                    Value::from("ERR"),
                    Value::from(e),
                    f,
                    Value::from(l as f64),
                ]);
                print(&x, seq, &mut out);
            }
            None => {
                let x = Value::from(vec![
                    Value::from("ERR"),
                    Value::from("break"),
                    f,
                    Value::from(l as f64),
                ]);
                print(&x, seq, &mut out);
                break;
            }
        }
    }
    (out, err)
}

fn run_jq(dir: &Path, files: &[F], flags: Flags, input_loop: bool) -> (Vec<u8>, Vec<u8>) {
    let mut cmd = Command::new(jq().unwrap());
    cmd.current_dir(dir).arg("-c").args(flags.argv());
    if input_loop {
        cmd.arg("-n").arg(LOOP);
    } else {
        cmd.arg("[., input_filename, input_line_number]");
    }
    let mut stdin = Vec::new();
    for f in files {
        match f {
            F::Stdin(data) => stdin = data.clone(),
            _ => {
                cmd.arg(f.name());
            }
        }
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn jq");
    let mut pipe = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(&stdin);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    (out.stdout, out.stderr)
}

fn mem_files(files: &[F]) -> (Vec<&'static str>, Vec<(OsString, MemFile)>) {
    let mut names = Vec::new();
    let mut mem = Vec::new();
    for f in files {
        names.push(f.name());
        match f {
            F::Data(n, data) => mem.push((OsString::from(*n), MemFile::Data(data.clone()))),
            F::Stdin(data) => mem.push((OsString::from("-"), MemFile::Data(data.clone()))),
            F::Dir(n) => mem.push((
                OsString::from(*n),
                MemFile::ReadError(Vec::new(), libc::EISDIR),
            )),
            F::Missing(_) => {}
        }
    }
    (names, mem)
}

/// Compares jq with the reference and with the reader (fast path on and
/// off, several deliveries). Returns a description of the first mismatch.
fn check(files: &[F], flags: Flags) -> Result<(), String> {
    let tmp = tempfile::tempdir().unwrap();
    for f in files {
        match f {
            F::Data(n, data) => std::fs::write(tmp.path().join(n), data).unwrap(),
            F::Dir(n) => std::fs::create_dir(tmp.path().join(n)).unwrap(),
            F::Missing(_) | F::Stdin(_) => {}
        }
    }
    let (names, mem) = mem_files(files);
    let pflags = ParseFlags {
        seq: flags.seq,
        streaming: flags.stream,
        stream_errors: false,
    };
    for input_mode in [false, true] {
        let want = run_jq(tmp.path(), files, flags, input_mode);
        let run = |r: &mut dyn Inputs| {
            if input_mode {
                input_loop(r, flags.seq)
            } else {
                main_loop(r, flags.seq)
            }
        };
        let mut reference = RefInput::new(&names, mem.clone(), flags.raw, flags.slurp, pflags);
        let mut results = vec![("reference".to_string(), run(&mut reference))];
        for (delivery, fast) in [
            (Delivery::Whole, true),
            (Delivery::Whole, false),
            (Delivery::Stream { seed: 9, max: 3 }, true),
            (Delivery::Stream { seed: 5, max: 4000 }, true),
        ] {
            let (r, msgs) = mem_reader(&names, mem.clone(), flags.opts(), delivery, fast);
            let mut ours = Ours(r, msgs);
            results.push((format!("reader {delivery:?} fast={fast}"), run(&mut ours)));
        }
        // The real file system, through the default opener.
        let paths: Vec<OsString> = files
            .iter()
            .filter(|f| !matches!(f, F::Stdin(_)))
            .map(|f| tmp.path().join(f.name()).into_os_string())
            .collect();
        if paths.len() == files.len() {
            let mut r = InputReader::new(paths, flags.opts());
            let msgs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let sink = msgs.clone();
            let prefix = format!("{}/", tmp.path().display());
            r.set_message_sink(Box::new(move |m| {
                let text = String::from_utf8_lossy(&m.render("jq")).replace(&prefix, "");
                sink.borrow_mut().extend_from_slice(text.as_bytes())
            }));
            let mut ours = Ours(r, msgs);
            let (out, err) = run(&mut ours);
            let out = String::from_utf8_lossy(&out)
                .replace(&format!("{}/", tmp.path().display()), "")
                .into_bytes();
            results.push(("reader on files".to_string(), (out, err)));
        }
        for (who, (out, err)) in results {
            if (&out, &err) != (&want.0, &want.1) {
                return Err(format!(
                    "{who} vs jq ({} flags={:?}):\n  got out {:?}\n      err {:?}\n  jq  out {:?}\n      err {:?}",
                    if input_mode {
                        "input loop"
                    } else {
                        "main loop"
                    },
                    flags.argv(),
                    show(&out[..out.len().min(600)]),
                    show(&err),
                    show(&want.0[..want.0.len().min(600)]),
                    show(&want.1),
                ));
            }
        }
    }
    Ok(())
}

fn scenarios() -> Vec<(Vec<F>, Flags)> {
    let raw = Flags { raw: true, ..JSON };
    let long = format!("{{\"a\":\"{}\"}} 7\n8\n", "x".repeat(5000));
    let long2 = format!(
        "{{\"a\":\"{}\"}} {} 7\n8\n",
        "x".repeat(4000),
        " ".repeat(200)
    );
    let euro_split = format!("{}€\n", "a".repeat(4094)); // € straddles byte 4095
    vec![
        (vec![d("a", "1"), d("b", "2\n")], JSON),
        (vec![d("a", "1"), d("b", "\n2\n")], JSON),
        (vec![d("a", "{\"a\":"), d("b", "1}\n")], JSON),
        (
            vec![d("a", "1\n2\n"), F::Missing("missing"), d("c", "3\n")],
            JSON,
        ),
        (vec![F::Missing("missing"), d("b", "1\n2\n")], JSON),
        (vec![d("a", "1\n2\n"), F::Missing("missing")], JSON),
        (vec![F::Dir("dir"), d("b", "1\n")], JSON),
        (vec![d("a", "1\n"), F::Dir("dir")], JSON),
        (vec![d("a", "\u{feff}1\n"), d("b", "\u{feff}2\n")], JSON),
        (vec![d("e", ""), d("b", "\u{feff}1\n")], JSON),
        (
            vec![
                F::Data("p1", b"\xef\xbb".to_vec()),
                F::Data("p2", b"\xbf7\n".to_vec()),
            ],
            JSON,
        ),
        (vec![F::Stdin(b"\xef\xbb\x41\n".to_vec())], JSON),
        (vec![F::Stdin(b"1 } 2\n3 ]\n4\n".to_vec())], JSON),
        (vec![F::Stdin(b"[1,\x002]\n".to_vec())], JSON),
        (vec![F::Stdin(b"[1,2]\n[3,\x004]".to_vec())], JSON),
        (vec![F::Stdin(b"1 2\x00 3".to_vec())], JSON),
        (vec![F::Stdin(long.clone().into_bytes())], JSON),
        (vec![F::Stdin(long2.into_bytes())], JSON),
        (
            vec![d("a", "{\"a\":1}\n{\"b\":\n2}\n  [3,\n4]\n\"s\" 5 6\n")],
            JSON,
        ),
        (vec![d("a", "1\n2"), d("b", "3\n")], JSON),
        (vec![d("a", "[1,2]"), d("b", "[3]")], JSON),
        (
            vec![d(
                "a",
                "nan\n[NaN,-Infinity]\n1e1000\n100000000000000000001\n",
            )],
            JSON,
        ),
        (vec![d("a", "{\"a\":1,\"a\":2}\n\"\\ud800\"\n")], JSON),
        (
            vec![d("a", "1\n2\n"), d("b", "3")],
            Flags {
                slurp: true,
                ..JSON
            },
        ),
        (
            vec![d("a", "1\n2 ]\n"), d("b", "3")],
            Flags {
                slurp: true,
                ..JSON
            },
        ),
        (vec![d("a", "x\ny"), d("b", "z\n")], raw),
        (
            vec![d("a", "x\r\ny\n\n"), F::Missing("m"), d("b", "z")],
            raw,
        ),
        (vec![F::Stdin(euro_split.clone().into_bytes())], raw),
        (
            vec![F::Stdin(euro_split.into_bytes())],
            Flags { slurp: true, ..raw },
        ),
        (vec![F::Data("a", b"a\x00b\nc\xffd".to_vec())], raw),
        (
            vec![d("a", "x\ny"), d("b", "z\n")],
            Flags { slurp: true, ..raw },
        ),
        (
            vec![F::Stdin(b"\x1e[1]\x1e2 3\x1e{\n".to_vec())],
            Flags { seq: true, ..JSON },
        ),
        (
            vec![d("a", "[1,[2]]"), d("b", "{\"a\":[]}\n")],
            Flags {
                stream: true,
                ..JSON
            },
        ),
        (
            vec![d("a", "[1,[2"), F::Dir("dir"), d("b", "]]\n")],
            Flags {
                stream: true,
                ..JSON
            },
        ),
        (
            vec![d("a", "1 2 3"), F::Missing("m"), d("b", "4")],
            Flags {
                stream: true,
                ..JSON
            },
        ),
    ]
}

#[test]
fn reader_matches_jq_binary() {
    if jq().is_none() {
        eprintln!("skipped: jq 1.8.1 not on PATH");
        return;
    }
    let mut failures = Vec::new();
    for (files, flags) in scenarios() {
        if let Err(e) = check(&files, flags) {
            let names: Vec<&str> = files.iter().map(F::name).collect();
            failures.push(format!("{names:?}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Generated multi-file inputs against the binary:
/// `cargo test --release --lib reader_matches_jq_binary_generated -- --ignored`
#[test]
#[ignore]
fn reader_matches_jq_binary_generated() {
    if jq().is_none() {
        eprintln!("skipped: jq 1.8.1 not on PATH");
        return;
    }
    let n: u64 = std::env::var("QJ_IO_LIVE_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let mut failures = Vec::new();
    for seed in 0..n {
        let mut rng = Rng(seed.wrapping_mul(0x2545F4914F6CDD1D) | 1);
        let data = super::generate::stream(&mut rng);
        let names = ["g0", "g1", "g2", "g3"];
        let parts = 1 + rng.below(3);
        let mut cuts: Vec<usize> = (1..parts).map(|_| rng.below(data.len() + 1)).collect();
        cuts.sort_unstable();
        let mut files = Vec::new();
        let mut prev = 0;
        for (i, &c) in cuts.iter().chain(std::iter::once(&data.len())).enumerate() {
            files.push(F::Data(names[i], data[prev..c].to_vec()));
            prev = c;
        }
        if rng.chance(1, 6) {
            files.insert(rng.below(files.len() + 1), F::Missing("gone"));
        }
        let flags = match rng.below(10) {
            0 => Flags { raw: true, ..JSON },
            1 => Flags {
                slurp: true,
                ..JSON
            },
            2 => Flags {
                stream: true,
                ..JSON
            },
            3 => Flags { seq: true, ..JSON },
            _ => JSON,
        };
        if let Err(e) = check(&files, flags) {
            failures.push(format!("seed {seed}: {e}"));
            if failures.len() >= 5 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
