//! The parallel engine against the sequential reader: same records, same
//! `input_filename`/`input_line_number`, same statuses and messages, in the
//! same order, over NDJSON-like inputs with every kind of line the workers
//! must hand back.

use std::cell::RefCell;
use std::ffi::OsString;
use std::ops::ControlFlow;
use std::rc::Rc;

use super::generate::Gen;
use super::{Delivery, MemFile, Rng, mem_reader, show};
use crate::io::parallel::{
    self, DumpFactory, EngineOptions, EngineStats, RecordSink, RecordWorker, WorkerFactory,
};
use crate::io::reader::ReaderOptions;
use crate::jq::value::{DumpOptions, Error};

struct Collect {
    out: Vec<u8>,
    err: Rc<RefCell<Vec<u8>>>,
    statuses: Vec<i32>,
}

impl RecordSink for Collect {
    fn record(&mut self, out: &[u8], err: &[u8], status: i32) -> ControlFlow<()> {
        self.out.extend_from_slice(out);
        self.err.borrow_mut().extend_from_slice(err);
        self.statuses.push(status);
        ControlFlow::Continue(())
    }
    fn parse_error(&mut self, e: Error) -> ControlFlow<()> {
        self.err
            .borrow_mut()
            .extend_from_slice(format!("jq: parse error: {e}\n").as_bytes());
        ControlFlow::Break(())
    }
}

/// NDJSON with oddities: blank lines, CRLF, pretty-printed texts, two
/// texts on a line, jq extensions, errors, long lines, NULs.
fn ndjson(r: &mut Rng) -> Vec<u8> {
    let weird = [0, 0, 10, 50, 150][r.below(5)];
    let mut out = Vec::new();
    let lines = 1 + r.below(60);
    for _ in 0..lines {
        let mut g = Gen { r, weird };
        match g.r.below(1000) {
            k if k < weird / 3 => {
                // two texts on one line
                g.value(&mut out, 0, false);
                out.push(b' ');
                g.value(&mut out, 0, false);
            }
            k if k < weird / 2 => g.value(&mut out, 0, true), // spans lines
            k if k < weird => match g.r.below(8) {
                0 => out.extend_from_slice(b"nan"),
                1 => out.extend_from_slice(b"[1,2"),
                2 => out.extend_from_slice(b"]"),
                3 => out.push(0),
                4 => out.extend_from_slice(b"{\"a\":1}}"),
                5 => out.extend_from_slice(b"100000000000000000001"),
                6 => out.extend_from_slice(b"\"\\ud800\""),
                _ => out.extend_from_slice(b"1 2 3"),
            },
            k if k < 1000 - 950 + weird => {} // blank line
            _ => g.value(&mut out, 0, false),
        }
        match r.below(10) {
            0 => out.extend_from_slice(b"\r\n"),
            1 => out.extend_from_slice(b"  \n"),
            _ => out.push(b'\n'),
        }
    }
    if r.chance(1, 4) && out.last() == Some(&b'\n') {
        out.pop();
    }
    out
}

fn run_both(
    names: &[&str],
    files: Vec<(OsString, MemFile)>,
    opts: ReaderOptions,
    delivery: Delivery,
    engine: &EngineOptions,
) -> Result<EngineStats, String> {
    let factory = DumpFactory {
        opts: DumpOptions::compact(),
        with_position: true,
    };
    // Sequential: jq's main loop.
    let (mut r, msgs) = mem_reader(names, files.clone(), opts, delivery, true);
    let mut want = Collect {
        out: Vec::new(),
        err: msgs.clone(),
        statuses: Vec::new(),
    };
    let mut w = factory.new_worker();
    while r.failures() == 0 {
        match r.next() {
            Some(Ok(v)) => {
                let name = r.current_filename();
                let meta = parallel::RecordMeta {
                    filename: name.as_str(),
                    line: r.current_line(),
                };
                let (mut o, mut e) = (Vec::new(), Vec::new());
                let st = w.process(v, &meta, &mut o, &mut e);
                let _ = want.record(&o, &e, st);
            }
            Some(Err(e)) => {
                let _ = want.parse_error(e);
                break;
            }
            None => break,
        }
    }
    // The engine.
    let (mut r, msgs) = mem_reader(names, files, opts, delivery, true);
    let mut got = Collect {
        out: Vec::new(),
        err: msgs,
        statuses: Vec::new(),
    };
    let stats = parallel::run(&mut r, &factory, &mut got, engine);
    let (want_err, got_err) = (want.err.borrow().clone(), got.err.borrow().clone());
    if got.out != want.out || got_err != want_err || got.statuses != want.statuses {
        let first = got
            .out
            .iter()
            .zip(want.out.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(got.out.len().min(want.out.len()));
        let from = first.saturating_sub(200);
        return Err(format!(
            "{delivery:?} {engine:?}\n  got  out ...{:?}\n  want out ...{:?}\n  got  err {:?}\n  want err {:?}\n  statuses equal: {}",
            show(&got.out[from..(first + 200).min(got.out.len())]),
            show(&want.out[from..(first + 200).min(want.out.len())]),
            show(&got_err),
            show(&want_err),
            got.statuses == want.statuses
        ));
    }
    Ok(stats)
}

fn engines(r: &mut Rng) -> EngineOptions {
    EngineOptions {
        threads: 1 + r.below(4),
        window_bytes: [64, 500, 3000, 1 << 20][r.below(4)],
        min_window: [0, 0, 100][r.below(3)],
        max_job_bytes: [1, 200, 5000][r.below(3)],
        stack_size: 8 << 20,
    }
}

fn check_seed(seed: u64) -> Result<EngineStats, String> {
    let mut r = Rng(seed.wrapping_mul(0xD1B54A32D192ED03) | 1);
    let raw = r.chance(1, 6);
    let parts = 1 + r.below(3);
    let mut names = Vec::new();
    let mut files = Vec::new();
    for i in 0..parts {
        let name = ["n0", "n1", "n2"][i];
        let data = ndjson(&mut r);
        files.push((OsString::from(name), MemFile::Data(data)));
        names.push(name);
        if r.chance(1, 12) {
            names.push("absent");
        }
    }
    let opts = ReaderOptions {
        raw,
        ..Default::default()
    };
    let delivery = match r.below(3) {
        0 => Delivery::Stream {
            seed,
            max: 1 + r.below(3000),
        },
        _ => Delivery::Whole,
    };
    let engine = engines(&mut r);
    run_both(&names, files.clone(), opts, delivery, &engine).map_err(|e| {
        let inputs: Vec<String> = files
            .iter()
            .map(|(n, f)| match f {
                MemFile::Data(d) => format!("{n:?}: {:?}", show(&d[..d.len().min(400)])),
                _ => String::new(),
            })
            .collect();
        format!(
            "seed {seed} names={names:?} raw={raw}\n{}\n{e}",
            inputs.join("\n")
        )
    })
}

fn check_seeds(seeds: std::ops::Range<u64>) {
    let mut failures = Vec::new();
    let mut total = EngineStats::default();
    for seed in seeds {
        match check_seed(seed) {
            Ok(s) => {
                total.windows += s.windows;
                total.jobs += s.jobs;
                total.worker_records += s.worker_records;
                total.sequential += s.sequential;
                total.discarded_jobs += s.discarded_jobs;
            }
            Err(e) => {
                failures.push(e);
                if failures.len() >= 3 {
                    break;
                }
            }
        }
    }
    eprintln!("engine: {total:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(total.worker_records > total.sequential, "{total:?}");
    assert!(total.discarded_jobs > 0, "{total:?}");
}

#[test]
fn engine_matches_sequential_reader() {
    check_seeds(0..300);
}

/// `cargo test --release --lib engine_matches_sequential_reader_long -- --ignored`
#[test]
#[ignore]
fn engine_matches_sequential_reader_long() {
    let n: u64 = std::env::var("QJ_IO_ENGINE_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000);
    check_seeds(1_000_000..1_000_000 + n);
}

/// Large clean NDJSON through the default engine settings: every record
/// taken by workers, output identical to the sequential reader.
#[test]
fn engine_large_clean_ndjson() {
    let mut r = Rng(77);
    let mut data = Vec::new();
    let mut g = Gen {
        r: &mut r,
        weird: 0,
    };
    while data.len() < 3 << 20 {
        g.value(&mut data, 0, false);
        data.push(b'\n');
    }
    let files = vec![(OsString::from("big"), MemFile::Data(data))];
    let engine = EngineOptions {
        threads: 4,
        ..EngineOptions::default()
    };
    let stats = run_both(
        &["big"],
        files,
        ReaderOptions::default(),
        Delivery::Whole,
        &engine,
    )
    .unwrap();
    // The first record (which settles the BOM check) and the end of input.
    assert!(stats.sequential <= 2, "{stats:?}");
    assert!(stats.jobs >= 16, "{stats:?}");
}
