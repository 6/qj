//! Differential checks for fuzzing (`fuzz/fuzz_targets/fuzz_io_reader.rs`)
//! and tests: the reader with and without its fast path, whole and
//! streamed, and the parallel engine, must agree on every input.

use std::ffi::OsString;
use std::io::{self, Read};
use std::ops::ControlFlow;
use std::sync::Arc;

use super::parallel::{
    self, DumpFactory, EngineOptions, RecordMeta, RecordSink, RecordWorker, WorkerFactory,
};
use super::reader::{InputReader, ReaderOptions};
use super::source::{Opened, Opener};
use crate::jq::value::{DumpOptions, Error, Value};

/// Strict structural identity: same kinds, same number literal text and
/// double bits, same string bytes, same key order (iterative: inputs nest
/// up to jq's 10000 levels).
pub fn same(a: &Value, b: &Value) -> bool {
    let mut todo: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((a, b)) = todo.pop() {
        let ok = match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Number(x), Value::Number(y)) => {
                x.literal() == y.literal()
                    && x.is_literal() == y.is_literal()
                    && (x.value().to_bits() == y.value().to_bits()
                        || (x.value().is_nan() && y.value().is_nan()))
            }
            (Value::String(x), Value::String(y)) => x.as_bytes() == y.as_bytes(),
            (Value::Array(x), Value::Array(y)) => {
                todo.extend(x.iter().zip(y.iter()));
                x.len() == y.len()
            }
            (Value::Object(x), Value::Object(y)) => {
                for ((k1, v1), (k2, v2)) in x.iter().zip(y.iter()) {
                    if k1 != k2 {
                        return false;
                    }
                    todo.push((v1, v2));
                }
                x.len() == y.len()
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

struct Split {
    files: Vec<(OsString, Arc<Vec<u8>>)>,
    /// `None`: whole; `Some(n)`: streamed in reads of 1..=n bytes.
    stream: Option<usize>,
    seed: u64,
    opened: u64,
}

struct Chunked {
    data: Arc<Vec<u8>>,
    pos: usize,
    max: usize,
    seed: u64,
}

impl Read for Chunked {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.data.len() {
            return Ok(0);
        }
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        let n = (1 + (self.seed % self.max as u64) as usize)
            .min(buf.len())
            .min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Opener for Split {
    fn open(&mut self, name: &std::ffi::OsStr) -> io::Result<Opened> {
        self.opened += 1;
        let Some((_, data)) = self.files.iter().find(|(n, _)| n == name) else {
            return Err(io::Error::from_raw_os_error(2));
        };
        Ok(match self.stream {
            None => Opened::Whole(data.clone()),
            Some(max) => Opened::Stream {
                reader: Box::new(Chunked {
                    data: data.clone(),
                    pos: 0,
                    max,
                    seed: self
                        .seed
                        .wrapping_add(self.opened.wrapping_mul(0x9E37_79B9))
                        | 1,
                }),
                fd: None,
            },
        })
    }
}

#[derive(Debug)]
enum Ev {
    Value(Value, Value, u64, usize),
    Error(String, Value, u64, usize),
    End(Value, u64, usize),
}

fn ev_eq(a: &Ev, b: &Ev) -> bool {
    match (a, b) {
        (Ev::Value(x, f, l, n), Ev::Value(y, g, m, o)) => {
            same(x, y) && same(f, g) && l == m && n == o
        }
        (Ev::Error(x, f, l, n), Ev::Error(y, g, m, o)) => x == y && same(f, g) && l == m && n == o,
        (Ev::End(f, l, n), Ev::End(g, m, o)) => same(f, g) && l == m && n == o,
        _ => false,
    }
}

fn events(r: &mut InputReader, limit: usize) -> Vec<Ev> {
    let mut evs = Vec::new();
    for _ in 0..limit {
        let next = r.next();
        let (f, l, n) = (r.current_filename(), r.current_line(), r.failures());
        match next {
            Some(Ok(v)) => evs.push(Ev::Value(v, f, l, n)),
            Some(Err(e)) => evs.push(Ev::Error(e.to_string(), f, l, n)),
            None => {
                evs.push(Ev::End(f, l, n));
                break;
            }
        }
    }
    evs
}

struct Collect {
    out: Vec<u8>,
    statuses: Vec<i32>,
    errors: Vec<String>,
}

impl RecordSink for Collect {
    fn record(&mut self, out: &[u8], _err: &[u8], status: i32) -> ControlFlow<()> {
        self.out.extend_from_slice(out);
        self.statuses.push(status);
        ControlFlow::Continue(())
    }
    fn parse_error(&mut self, e: Error) -> ControlFlow<()> {
        self.errors.push(e.to_string());
        ControlFlow::Break(())
    }
}

/// Runs `data` through the reader in several configurations and panics if
/// they disagree. The first bytes choose the options, how the rest is split
/// into inputs, and how streams are read.
pub fn check_reader_equivalence(data: &[u8]) {
    if data.len() < 3 {
        return;
    }
    let (cfg, rest) = (data[0], &data[3..]);
    let opts = match cfg % 8 {
        5 => ReaderOptions {
            raw: true,
            ..Default::default()
        },
        6 => ReaderOptions {
            slurp: true,
            ..Default::default()
        },
        7 => ReaderOptions {
            raw: true,
            slurp: true,
            ..Default::default()
        },
        _ => ReaderOptions::default(),
    };
    // Up to three inputs, split at positions taken from bytes 1 and 2.
    let parts = 1 + (cfg as usize >> 3) % 3;
    let mut cuts: Vec<usize> = [data[1], data[2]]
        .iter()
        .take(parts - 1)
        .map(|&b| (b as usize * rest.len()) / 255)
        .collect();
    cuts.sort_unstable();
    let mut files = Vec::new();
    let mut prev = 0;
    for (i, &c) in cuts.iter().chain(std::iter::once(&rest.len())).enumerate() {
        files.push((
            OsString::from(format!("f{i}")),
            Arc::new(rest[prev..c].to_vec()),
        ));
        prev = c;
    }
    let names: Vec<OsString> = files.iter().map(|(n, _)| n.clone()).collect();
    let reader = |stream: Option<usize>, fast: bool| {
        let split = Split {
            files: files.clone(),
            stream,
            seed: u64::from(cfg) + 1,
            opened: 0,
        };
        let mut r = InputReader::with_opener(names.clone(), opts, Box::new(split));
        r.set_message_sink(Box::new(|_| {}));
        r.set_fast_path(fast);
        r
    };
    const LIMIT: usize = 1000;
    let want = events(&mut reader(None, false), LIMIT);
    for (stream, fast) in [
        (None, true),
        (Some(1 + cfg as usize % 7), true),
        (Some(4096), false),
        (Some(300), true),
    ] {
        let got = events(&mut reader(stream, fast), LIMIT);
        let first = got.iter().zip(want.iter()).position(|(a, b)| !ev_eq(a, b));
        assert!(
            got.len() == want.len() && first.is_none(),
            "stream={stream:?} fast={fast} differs at event {first:?}: {:?} vs {:?}",
            first.map(|i| &got[i]),
            first.map(|i| &want[i])
        );
    }
    if opts.slurp {
        return;
    }
    // The engine against the sequential reader.
    let factory = DumpFactory {
        opts: DumpOptions::compact(),
        with_position: true,
    };
    let mut seq = Collect {
        out: Vec::new(),
        statuses: Vec::new(),
        errors: Vec::new(),
    };
    let mut r = reader(None, true);
    let mut w = factory.new_worker();
    while r.failures() == 0 {
        match r.next() {
            Some(Ok(v)) => {
                let name = r.current_filename();
                let meta = RecordMeta {
                    filename: name.as_str(),
                    line: r.current_line(),
                };
                let mut out = Vec::new();
                let st = w.process(v, &meta, &mut out, &mut Vec::new());
                let _ = seq.record(&out, &[], st);
            }
            Some(Err(e)) => {
                let _ = seq.parse_error(e);
                break;
            }
            None => break,
        }
    }
    let mut par = Collect {
        out: Vec::new(),
        statuses: Vec::new(),
        errors: Vec::new(),
    };
    let engine = EngineOptions {
        threads: 2,
        window_bytes: 256 + cfg as usize * 16,
        min_window: 0,
        max_job_bytes: 1 + cfg as usize,
        stack_size: 64 << 20,
    };
    parallel::run(&mut reader(Some(100), true), &factory, &mut par, &engine);
    assert!(
        par.out == seq.out && par.statuses == seq.statuses && par.errors == seq.errors,
        "engine differs: {:?} vs {:?}, errors {:?} vs {:?}",
        String::from_utf8_lossy(&par.out),
        String::from_utf8_lossy(&seq.out),
        par.errors,
        seq.errors
    );
}

/// Programs for [`check_tape_equivalence`]: every construct the tape
/// evaluator takes, in combinations.
pub const TAPE_PROGRAMS: &[&str] = &[
    ".",
    ".a",
    ".a.b",
    ".b.a",
    ".[\"a\"]",
    ".[]",
    ".[].a",
    ".a[]",
    ".[][]",
    ".[] | .[] | .a",
    "length",
    ".a | length",
    ".[] | length",
    "keys",
    "keys_unsorted",
    ".[] | keys",
    "[.[]]",
    "[.[] | .a]",
    "map(.a)",
    "map(length)",
    "map(keys_unsorted)",
    "map({a, b})",
    "{a, b: .b.c}",
    "{a: [.[]], b: {c: .a}}",
    "{a} | .[]",
    "select(.a == 1)",
    "select(.a == -1)",
    "select(.a != \"a\")",
    "select(.a == null)",
    "select(.a == false)",
    "select(.b)",
    ".[] | select(.a == 1.0) | .b",
    "map(select(.a))",
    "select(length == 1)",
    "[.[] | select(. == 0)]",
    ".a?",
    ".a.b?",
    ".[]?",
    ".[][]?",
    "[.[].a?]",
    "map(.a[]?)",
    "def f: .a; f",
    "def keys: .b; [keys]",
    "def t: .a == 1; map(select(t))",
    "select(.a > 1)",
    "map(select(. <= \"b\"))",
    "select(0 < .a)",
    "select(.a and .b)",
    "map(select(. == 1 or (. | not)))",
    "add",
    "map(.a) | add",
];

/// The tape evaluator (`super::tape_eval`) against the VM: `data[0]` picks
/// a program from [`TAPE_PROGRAMS`], `data[1]` the output layout, and the
/// rest is the input. Where the VM raises an error the evaluator must
/// decline; where it doesn't decline, its outputs must print exactly as the
/// VM's.
pub fn check_tape_equivalence(data: &[u8]) {
    use super::simd::SimdParser;
    use super::tape::{Layout, Scratch};
    use super::tape_eval::{Output, TapeProgram};
    use crate::jq::lang::execute::Jq;
    use crate::jq::lang::{CompileOptions, jq_compile_args};
    use crate::jq::value::Indent;
    use crate::jq::value::print::dump_to_vec;

    let [p, opts, text @ ..] = data else {
        return;
    };
    let program = TAPE_PROGRAMS[*p as usize % TAPE_PROGRAMS.len()];
    let dump = DumpOptions {
        indent: match opts % 4 {
            0 => Indent::Compact,
            1 => Indent::Spaces(2),
            2 => Indent::Tab,
            _ => Indent::Spaces(opts / 4 % 8),
        },
        sort_keys: opts & 0x40 != 0,
        ascii: opts & 0x80 != 0,
        colors: None,
    };
    let mut padded = text.to_vec();
    padded.resize(text.len() + crate::simdjson::padding(), 0);
    let mut simd = SimdParser::new();
    let Ok(value) = simd.parse(&padded, 0, text.len()) else {
        return;
    };
    let bc = jq_compile_args(program.as_bytes(), &CompileOptions::new(".")).expect("compiles");
    let mut jq = Jq::new(bc);
    jq.start(value, 0);
    let mut want = Vec::new();
    let mut vm_failed = false;
    for r in jq.by_ref() {
        match r {
            Ok(v) => want.push(v),
            Err(_) => {
                vm_failed = true;
                break;
            }
        }
    }
    let prog = TapeProgram::new(program.as_bytes()).expect("qualifies");
    let layout = Layout::new(&dump).expect("no colors");
    let got = simd
        .parse_with(&padded, 0, text.len(), |p| {
            let doc = p.doc();
            let mut scratch = Scratch::default();
            let mut results = Vec::new();
            prog.eval(&doc, &mut scratch, &mut results).ok()?;
            Some(
                results
                    .iter()
                    .map(|val| {
                        let o = Output { doc: &doc, val };
                        let mut out = Vec::new();
                        o.dump(&layout, &mut scratch, &mut out);
                        (out, o.is_null_or_false(), o.as_str().map(str::to_owned))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .expect("parsed before");
    let Some(got) = got else {
        return; // declined: the VM runs instead
    };
    assert!(
        !vm_failed,
        "{program}: jq errors, the tape evaluator doesn't"
    );
    assert_eq!(got.len(), want.len(), "{program}: output count");
    for (w, (g, null_or_false, s)) in want.iter().zip(got) {
        let mut wd = Vec::new();
        dump_to_vec(w, &dump, &mut wd);
        assert!(
            wd == g,
            "{program} ({dump:?}): got {:?}, want {:?}",
            String::from_utf8_lossy(&g),
            String::from_utf8_lossy(&wd)
        );
        assert_eq!(null_or_false, matches!(w, Value::Null | Value::Bool(false)));
        assert_eq!(s.as_deref(), w.as_str());
    }
}
