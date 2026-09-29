//! Tests for the input layer. Expectations come from jq 1.8.1 (fixtures
//! recorded from the binary, or live runs when `jq` is on PATH), or from
//! jq's parser port fed exactly as jq's `util.c` feeds it (`reference`).

mod diff;
mod engine;
mod files;
mod generate;
mod live;
mod reader;
mod reference;
mod simd;
mod stream;
mod vm;

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::rc::Rc;
use std::sync::Arc;

use crate::io::reader::{InputReader, ReaderOptions};
use crate::io::source::{InputMessage, Opened, Opener};
use crate::jq::value::{DumpOptions, Value, dump_string};

/// Strict structural identity: same kinds, same number literal text and
/// double bits, same string bytes, same key order.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    // Iterative: inputs nest up to jq's 10000 levels.
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

/// A deterministic xorshift generator.
#[derive(Clone)]
pub(crate) struct Rng(pub u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in `0..n` (n > 0).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub(crate) fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

/// An in-memory input.
#[derive(Clone, Debug)]
pub(crate) enum MemFile {
    Data(Vec<u8>),
    /// Fails to open with this errno.
    Missing(i32),
    /// Yields the bytes, then a read error with this errno.
    ReadError(Vec<u8>, i32),
}

/// How an in-memory input is delivered to the reader.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Delivery {
    /// All at once, like a memory-mapped file.
    Whole,
    /// As a stream, in reads of 1..=max bytes (pseudo-random).
    Stream { seed: u64, max: usize },
}

/// Serves named in-memory inputs.
pub(crate) struct MemOpener {
    pub(crate) files: Vec<(OsString, MemFile)>,
    pub(crate) delivery: Delivery,
    opened: usize,
    stdin_done: bool,
}

impl MemOpener {
    pub(crate) fn new(files: Vec<(OsString, MemFile)>, delivery: Delivery) -> MemOpener {
        MemOpener {
            files,
            delivery,
            opened: 0,
            stdin_done: false,
        }
    }
}

struct ChunkedReader {
    data: Vec<u8>,
    pos: usize,
    rng: Rng,
    max: usize,
    error: Option<i32>,
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.data.len() {
            return match self.error.take() {
                Some(code) => Err(io::Error::from_raw_os_error(code)),
                None => Ok(0),
            };
        }
        let n = (1 + self.rng.below(self.max))
            .min(buf.len())
            .min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Opener for MemOpener {
    fn open(&mut self, name: &OsStr) -> io::Result<Opened> {
        self.opened += 1;
        let Some((_, file)) = self.files.iter().find(|(n, _)| n == name) else {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        };
        if name == "-" {
            // Standard input is read once; a second `-` starts at its EOF.
            if self.stdin_done {
                return Ok(Opened::bytes(Vec::new()));
            }
            self.stdin_done = true;
        }
        let (data, error) = match file.clone() {
            MemFile::Missing(code) => return Err(io::Error::from_raw_os_error(code)),
            MemFile::Data(d) => (d, None),
            MemFile::ReadError(d, code) => (d, Some(code)),
        };
        match (self.delivery, error) {
            (Delivery::Whole, None) => Ok(Opened::Whole(Arc::new(data))),
            (Delivery::Whole, Some(_)) => Ok(Opened::Stream {
                reader: Box::new(ChunkedReader {
                    data,
                    pos: 0,
                    rng: Rng(1),
                    max: 1 << 20,
                    error,
                }),
                fd: None,
            }),
            (Delivery::Stream { seed, max }, _) => Ok(Opened::Stream {
                reader: Box::new(ChunkedReader {
                    data,
                    pos: 0,
                    rng: Rng(seed.wrapping_add(self.opened as u64 * 7919) | 1),
                    max,
                    error,
                }),
                fd: None,
            }),
        }
    }
}

/// A reader over in-memory inputs, collecting its stderr messages.
pub(crate) fn mem_reader(
    names: &[&str],
    files: Vec<(OsString, MemFile)>,
    opts: ReaderOptions,
    delivery: Delivery,
    fast: bool,
) -> (InputReader, Rc<RefCell<Vec<u8>>>) {
    let names: Vec<OsString> = names.iter().map(OsString::from).collect();
    let mut r = InputReader::with_opener(names, opts, Box::new(MemOpener::new(files, delivery)));
    r.set_fast_path(fast);
    let msgs = Rc::new(RefCell::new(Vec::new()));
    let sink = msgs.clone();
    r.set_message_sink(Box::new(move |m: InputMessage| {
        sink.borrow_mut().extend_from_slice(&m.render("jq"))
    }));
    (r, msgs)
}

/// jq's main loop (`main.c`) printing each input with `-c`: stops at the
/// first parse error unless `--seq`, and before reading more once an input
/// failed to open or read.
pub(crate) fn simulate_cli(
    r: &mut InputReader,
    msgs: &Rc<RefCell<Vec<u8>>>,
    seq: bool,
) -> (Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let opts = DumpOptions::compact();
    while r.failures() == 0 {
        let next = r.next();
        err.append(&mut msgs.borrow_mut());
        match next {
            Some(Ok(v)) => {
                if seq {
                    out.push(0x1e);
                }
                out.extend_from_slice(dump_string(&v, &opts).as_bytes());
                out.push(b'\n');
            }
            Some(Err(e)) => {
                if !seq {
                    err.extend_from_slice(format!("jq: parse error: {e}\n").as_bytes());
                    break;
                }
                err.extend_from_slice(format!("jq: ignoring parse error: {e}\n").as_bytes());
            }
            None => break,
        }
    }
    err.append(&mut msgs.borrow_mut());
    (out, err)
}

/// Everything observable about a sequence of `next()` calls, continuing
/// after errors (as `try input` does).
#[derive(Debug)]
pub(crate) enum Ev {
    Value(Value, Value, u64, usize),
    Error(String, Value, u64, usize),
    End(Value, u64, usize),
    Message(Vec<u8>),
}

impl PartialEq for Ev {
    fn eq(&self, other: &Ev) -> bool {
        match (self, other) {
            (Ev::Value(a, f, l, n), Ev::Value(b, g, m, o)) => {
                same(a, b) && same(f, g) && l == m && n == o
            }
            (Ev::Error(a, f, l, n), Ev::Error(b, g, m, o)) => {
                a == b && same(f, g) && l == m && n == o
            }
            (Ev::End(f, l, n), Ev::End(g, m, o)) => same(f, g) && l == m && n == o,
            (Ev::Message(a), Ev::Message(b)) => a == b,
            _ => false,
        }
    }
}

pub(crate) fn events(r: &mut InputReader, msgs: &Rc<RefCell<Vec<u8>>>, limit: usize) -> Vec<Ev> {
    let mut evs = Vec::new();
    for _ in 0..limit {
        let next = r.next();
        let m = std::mem::take(&mut *msgs.borrow_mut());
        if !m.is_empty() {
            evs.push(Ev::Message(m));
        }
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

pub(crate) fn show(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
