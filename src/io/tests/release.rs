//! Releasing memory-mapped input as it's consumed (`InputBytes::release`,
//! the reader's `release_consumed`, the engine's pins): released pages
//! fault, the reader releases right behind itself, and the engine never
//! releases what a job is still reading, even a job it has given up on.
//! (Every test with `Delivery::Whole` also runs on released mappings.)

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::time::Duration;

use super::generate::Gen;
use super::{Delivery, MemFile, Rng, events, mem_reader};
use crate::io::parallel::{self, DumpFactory, EngineOptions, RecordSink, RecordTape};
use crate::io::parallel::{RecordMeta, RecordWorker, WorkerFactory};
use crate::io::reader::ReaderOptions;
use crate::io::source::{InputBytes, Mmap, page_size};
use crate::io::tape::{Doc, NodeKind};
use crate::io::tape_eval::Decline;
use crate::jq::value::{DumpOptions, Error};

/// Whether reading the byte at `p` faults, checked in a forked child (which
/// only reads it and exits, from a signal handler if it faults).
fn read_faults(p: *const u8) -> bool {
    extern "C" fn on_fault(_: libc::c_int) {
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(42) };
    }
    // SAFETY: the child calls only async-signal-safe functions (sigaction,
    // a volatile read, _exit) before exiting.
    unsafe {
        let pid = libc::fork();
        assert!(pid >= 0, "fork");
        if pid == 0 {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_fault as extern "C" fn(libc::c_int) as usize;
            libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGBUS, &sa, std::ptr::null_mut());
            std::ptr::read_volatile(p);
            libc::_exit(0);
        }
        let mut status = 0;
        assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
        assert!(libc::WIFEXITED(status), "child status {status}");
        libc::WEXITSTATUS(status) == 42
    }
}

#[test]
fn released_pages_fault() {
    let page = page_size();
    let data: Vec<u8> = (0..5 * page).map(|i| (i % 251) as u8).collect();
    let skip = 100;
    let map = Mmap::copy_of(&data, skip).unwrap();
    let base = map.padded().as_ptr(); // data offset 0
    assert!(map.releasable());
    assert!(!read_faults(base));
    // Data offset `page - skip` starts the second page of the mapping.
    let second = page - skip;
    let upto = second + page + 10; // into the third page
    assert_eq!(map.release(upto), upto);
    // The first two pages are gone; the rest of the third page stays.
    assert!(read_faults(base));
    assert!(read_faults(base.wrapping_add(second + page - 1)));
    assert!(!read_faults(base.wrapping_add(second + page)));
    assert!(!read_faults(base.wrapping_add(upto)));
    let rest = map.padded_from(upto);
    assert_eq!(&rest[..data.len() - upto], &data[upto..]);
    // Releasing less is a no-op; the start only moves forward.
    assert_eq!(map.release(5), upto);
    assert_eq!(map.release(upto + 1), upto + 1);
    // Up to the end of the data: the last page (padding after it) stays
    // readable, and so does what follows.
    assert_eq!(map.release(usize::MAX), data.len());
    assert!(read_faults(base.wrapping_add(data.len() - page)));
    assert!(!read_faults(base.wrapping_add(data.len())));
    assert!(map.padded_from(data.len()).iter().all(|&b| b == 0));
}

#[test]
fn memory_bytes_are_never_released() {
    let v: Vec<u8> = b"[1]\n".repeat(10_000);
    assert!(!v.releasable());
    assert_eq!(v.release(1000), 0);
    assert_eq!(v.padded_from(0), &v[..]);
}

fn ndjson(seed: u64, len: usize, pretty_every: usize) -> Vec<u8> {
    let mut r = Rng(seed);
    let mut g = Gen {
        r: &mut r,
        weird: 0,
    };
    let mut out = Vec::new();
    let mut i = 0;
    while out.len() < len {
        g.value(&mut out, 0, pretty_every != 0 && i % pretty_every == 1);
        out.push(b'\n');
        i += 1;
    }
    out
}

/// The reader releases what it has read, in every mode, reading exactly
/// what it reads from memory.
#[test]
fn reader_releases_behind_itself() {
    let data = ndjson(5, 1 << 20, 7);
    // (--seq texts start with RS.)
    let mut seq = Vec::new();
    for line in data.split_inclusive(|&c| c == b'\n') {
        seq.push(0x1e);
        seq.extend_from_slice(line);
    }
    for opts in [
        ReaderOptions::default(),
        ReaderOptions {
            raw: true,
            ..Default::default()
        },
        ReaderOptions {
            stream: true,
            ..Default::default()
        },
        ReaderOptions {
            seq: true,
            ..Default::default()
        },
    ] {
        let data = if opts.seq { &seq } else { &data };
        let files = vec![(OsString::from("f"), MemFile::Data(data.clone()))];
        for fast in [true, false] {
            let (mut r, _m) = mem_reader(&["f"], files.clone(), opts, Delivery::Whole, fast);
            let mut max_released = 0;
            let mut n = 0;
            while let Some(v) = r.next() {
                if v.is_err() && !opts.seq {
                    break;
                }
                n += 1;
                max_released = max_released.max(r.released());
            }
            assert!(n > 100, "{opts:?} fast={fast}: {n} values");
            // (Released up to the last record's start at least.)
            assert!(
                max_released > data.len() - (64 << 10),
                "{opts:?} fast={fast}: released {max_released} of {}",
                data.len()
            );
            let (mut r, m) = mem_reader(&["f"], files.clone(), opts, Delivery::Whole, fast);
            let (mut b, mb) = mem_reader(&["f"], files.clone(), opts, Delivery::Bytes, fast);
            assert!(
                events(&mut r, &m, 1 << 20) == events(&mut b, &mb, 1 << 20),
                "{opts:?} fast={fast}"
            );
        }
    }
}

/// Declines every record (so workers build values), after sleeping in
/// worker threads on a document that is only ever a line inside a larger
/// text (`{"x":1}`): the job starting there is still reading its bytes long
/// after the reader has read on past it and cancelled it.
struct SlowTape;

impl RecordTape for SlowTape {
    fn run(
        &mut self,
        doc: &Doc<'_>,
        _out: &mut Vec<u8>,
        _marks: &mut Vec<(usize, usize)>,
    ) -> Result<i32, Decline> {
        let root = doc.root();
        let worker = std::thread::current()
            .name()
            .is_some_and(|n| n.starts_with("qj-worker"));
        if worker && doc.kind(root) == NodeKind::Object && doc.get(root, "x").is_some() {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(Decline)
    }
}

struct SlowFactory(DumpFactory);

impl WorkerFactory for SlowFactory {
    type Worker = <DumpFactory as WorkerFactory>::Worker;
    fn new_worker(&self) -> Self::Worker {
        self.0.new_worker()
    }
    fn new_tape(&self) -> Option<Box<dyn RecordTape>> {
        Some(Box::new(SlowTape))
    }
}

struct Collect(Vec<u8>, Vec<i32>);

impl RecordSink for Collect {
    fn record(&mut self, out: &[u8], _err: &[u8], status: i32) -> ControlFlow<()> {
        self.0.extend_from_slice(out);
        self.1.push(status);
        ControlFlow::Continue(())
    }
    fn parse_error(&mut self, e: Error) -> ControlFlow<()> {
        self.0
            .extend_from_slice(format!("parse error: {e}\n").as_bytes());
        ControlFlow::Break(())
    }
}

/// A job cut inside a text is cancelled once the reader reads past its
/// start, but its worker may still be parsing it; the reader must not
/// release its bytes until the worker is done (its pin). Here such jobs
/// are slow, and the reader moves pages ahead meanwhile: without the pin,
/// the worker reads released pages and faults.
#[test]
fn engine_keeps_what_cancelled_jobs_read() {
    let mut data = Vec::new();
    let mut i = 0;
    while data.len() < 2 << 20 {
        // A text whose every other line (`{"x":k}`) is a text of its own,
        // then enough records to move the reader a few pages on.
        data.extend_from_slice(b"{\"k\":[\n");
        for k in 0..60 {
            data.extend_from_slice(format!("{{\"x\":{k}}}\n],\"k{k}\":[\n").as_bytes());
        }
        data.extend_from_slice(b"1]}\n");
        let target = data.len() + (40 << 10);
        while data.len() < target {
            data.extend_from_slice(format!("{{\"id\":{i},\"t\":\"record\"}}\n").as_bytes());
            i += 1;
        }
    }
    let files = vec![(OsString::from("f"), MemFile::Data(data))];
    let factory = SlowFactory(DumpFactory {
        opts: DumpOptions::compact(),
        with_position: true,
    });
    let (mut r, _m) = mem_reader(
        &["f"],
        files.clone(),
        ReaderOptions::default(),
        Delivery::Whole,
        true,
    );
    let mut want = Collect(Vec::new(), Vec::new());
    let mut w = factory.new_worker();
    while let Some(v) = r.next() {
        let v = v.unwrap();
        let name = r.current_filename();
        let meta = RecordMeta {
            filename: name.as_str(),
            line: r.current_line(),
        };
        let mut out = Vec::new();
        let st = w.process(v, &meta, &mut out, &mut Vec::new());
        let _ = want.record(&out, &[], st);
    }
    let mut total = parallel::EngineStats::default();
    for max_job_bytes in [300, 2000, 9000] {
        let (mut r, _m) = mem_reader(
            &["f"],
            files.clone(),
            ReaderOptions::default(),
            Delivery::Whole,
            true,
        );
        let mut got = Collect(Vec::new(), Vec::new());
        let opts = EngineOptions {
            threads: 4,
            window_bytes: 256 << 10,
            min_window: 0,
            max_job_bytes,
            stack_size: 8 << 20,
        };
        let stats = parallel::run(&mut r, &factory, &mut got, &opts);
        assert!(
            got.0 == want.0 && got.1 == want.1,
            "max_job_bytes {max_job_bytes}"
        );
        total.discarded_jobs += stats.discarded_jobs;
    }
    // (Jobs were cut inside those texts and cancelled.)
    assert!(total.discarded_jobs > 10, "{total:?}");
}
