//! Streaming input through a real pipe: records are available as soon as
//! their line is complete (jq's `fgets` would have them), and memory stays
//! bounded however long the stream is.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::ops::ControlFlow;
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::io::parallel::{self, DumpFactory, EngineOptions, RecordSink};
use crate::io::reader::{InputReader, ReaderOptions};
use crate::io::source::{Opened, Opener};
use crate::jq::value::{DumpOptions, Error};

struct PipeOpener(Option<io::PipeReader>);

impl Opener for PipeOpener {
    fn open(&mut self, _name: &OsStr) -> io::Result<Opened> {
        let rx = self.0.take().expect("opened once");
        let fd = rx.as_raw_fd();
        Ok(Opened::Stream {
            reader: Box::new(rx),
            fd: Some(fd),
        })
    }
}

/// Writes `first`, waits (up to 10 s) for the reader to report that it
/// has the first record, then writes `rest`. Returns whether the signal
/// came before the timeout.
fn writer(
    first: &'static [u8],
    rest: &'static [u8],
) -> (io::PipeReader, mpsc::Sender<()>, thread::JoinHandle<bool>) {
    let (rx, mut tx) = io::pipe().unwrap();
    let (sig_tx, sig_rx) = mpsc::channel::<()>();
    let h = thread::spawn(move || {
        tx.write_all(first).unwrap();
        let got = sig_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let _ = tx.write_all(rest);
        got
    });
    (rx, sig_tx, h)
}

#[test]
fn reader_yields_records_before_more_input() {
    // (Like jq's fgets, nothing is taken from a line before its newline
    // arrives, so each first part ends with one.)
    for (first, rest) in [
        (&b"{\"a\":1}\n"[..], &b"{\"a\":2}\n"[..]),
        (b"1\n", b"2\n"),
        (b"[1,\n2]\n", b"3"),
        (b"\"x\"\n", b"\"y\"\n"),
    ] {
        let (rx, sig, h) = writer(first, rest);
        let mut r = InputReader::with_opener(
            vec!["-".into()],
            ReaderOptions::default(),
            Box::new(PipeOpener(Some(rx))),
        );
        let v = r.next().unwrap().unwrap();
        sig.send(()).unwrap();
        let w = r.next().unwrap().unwrap();
        assert!(r.next().is_none());
        assert!(
            h.join().unwrap(),
            "{v} from {first:?} came only after more input ({w})"
        );
    }
}

#[test]
fn raw_reader_yields_lines_before_more_input() {
    let (rx, sig, h) = writer(b"line one\n", b"line two\n");
    let opts = ReaderOptions {
        raw: true,
        ..Default::default()
    };
    let mut r = InputReader::with_opener(vec!["-".into()], opts, Box::new(PipeOpener(Some(rx))));
    assert_eq!(r.next().unwrap().unwrap().as_str(), Some("line one"));
    sig.send(()).unwrap();
    assert_eq!(r.next().unwrap().unwrap().as_str(), Some("line two"));
    assert!(h.join().unwrap());
}

struct Signal {
    sig: Option<mpsc::Sender<()>>,
    out: Vec<u8>,
}

impl RecordSink for Signal {
    fn record(&mut self, out: &[u8], _err: &[u8], _status: i32) -> ControlFlow<()> {
        self.out.extend_from_slice(out);
        if let Some(s) = self.sig.take() {
            s.send(()).unwrap();
        }
        ControlFlow::Continue(())
    }
    fn parse_error(&mut self, _e: Error) -> ControlFlow<()> {
        ControlFlow::Break(())
    }
}

#[test]
fn engine_yields_records_before_more_input() {
    let (rx, sig, h) = writer(b"{\"a\":1}\n{\"b\":2}\n", b"{\"c\":3}\n");
    let mut r = InputReader::with_opener(
        vec!["-".into()],
        ReaderOptions::default(),
        Box::new(PipeOpener(Some(rx))),
    );
    let mut sink = Signal {
        sig: Some(sig),
        out: Vec::new(),
    };
    let factory = DumpFactory {
        opts: DumpOptions::compact(),
        with_position: true,
    };
    let opts = EngineOptions {
        threads: 2,
        min_window: 0,
        ..EngineOptions::default()
    };
    parallel::run(&mut r, &factory, &mut sink, &opts);
    assert!(h.join().unwrap(), "the first record waited for more input");
    assert_eq!(
        String::from_utf8(sink.out).unwrap(),
        "[{\"a\":1},\"<stdin>\",1]\n[{\"b\":2},\"<stdin>\",2]\n[{\"c\":3},\"<stdin>\",3]\n"
    );
}

/// A long stream of records keeps the reader's buffer small.
#[test]
fn stream_memory_is_bounded() {
    let (rx, mut tx) = io::pipe().unwrap();
    let total: usize = if cfg!(debug_assertions) {
        8 << 20
    } else {
        200 << 20
    };
    let h = thread::spawn(move || {
        let line = b"{\"id\":12345,\"name\":\"a fairly ordinary record\",\"tags\":[1,2,3]}\n";
        let block: Vec<u8> = line
            .iter()
            .copied()
            .cycle()
            .take(line.len() * 1000)
            .collect();
        let mut sent = 0;
        while sent < total {
            tx.write_all(&block).unwrap();
            sent += block.len();
        }
        sent / line.len()
    });
    let mut r = InputReader::with_opener(
        vec!["-".into()],
        ReaderOptions::default(),
        Box::new(PipeOpener(Some(rx))),
    );
    let mut n = 0;
    let mut peak = 0;
    while let Some(v) = r.next() {
        v.unwrap();
        n += 1;
        if n % 4096 == 0 {
            peak = peak.max(r.buffer_capacity());
        }
    }
    assert_eq!(n, h.join().unwrap());
    assert!(peak <= 8 << 20, "stream buffer grew to {peak} bytes");
}
