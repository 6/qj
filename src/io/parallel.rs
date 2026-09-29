//! The parallel record engine: jq's main loop (`main.c`: `process()` for
//! each input) with records processed on worker threads.
//!
//! The calling thread owns the [`InputReader`]. Whenever the reader is
//! between records with whole lines buffered (NDJSON-like input, or `-R`
//! lines), it takes a window of complete lines, splits it into jobs at line
//! boundaries, and hands them to worker threads. Each worker has its own
//! [`RecordWorker`] (its own compiled program: values are `Rc`, not `Send`)
//! and its own simdjson parser. A worker takes every line that is exactly
//! one text the reader's fast path would take (see
//! [`super::reader`]); at the first line that isn't (a parse error, `nan`,
//! a text spanning lines, two texts on a line, ...), it stops, and the
//! calling thread reads from that line on with the reader itself, one
//! record at a time, until the reader is idle at a job boundary again. So
//! the records, their `input_filename`/`input_line_number`, parse errors and
//! everything else are exactly what the sequential reader gives.
//!
//! Results come back to the calling thread in input order through a
//! [`RecordSink`]: output bytes, stderr bytes, and the worker's status for
//! each record. jq's exit status depends on the order (the last record's
//! `process()` result wins), so the sink can fold them exactly as `main.c`
//! does.
//!
//! **When not to use it.** The engine is only correct when records are
//! independent. The CLI must process sequentially (`threads: 0`) for
//! programs that read input themselves (`input`, `inputs`), stop the run
//! (`halt`, `halt_error`), or whose output interleaving or state spans
//! records (`debug`, `stderr`, `input_line_number`... unless the worker
//! answers them from [`RecordMeta`], `$__loc__`, `limit` over `inputs`,
//! `-s`, `-n`, `--seq`, `--stream`). It is also pointless for a single large
//! document (there's one record).

use std::ops::ControlFlow;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use super::reader::{CatchUp, InputReader, Window, window_line, window_line_number};
use super::simd::SimdParser;
use super::source::SharedBytes;
use crate::jq::value::print::dump_to_vec;
use crate::jq::value::{DumpOptions, Error, Value};

/// What a worker knows about the record it processes.
#[derive(Clone, Copy, Debug)]
pub struct RecordMeta<'a> {
    /// `input_filename` (`<stdin>` for `-`; `None` is `null`).
    pub filename: Option<&'a str>,
    /// `input_line_number` when jq read this record.
    pub line: u64,
}

/// Runs the jq program on one record at a time. One per thread.
pub trait RecordWorker {
    /// Processes one input value, appending what goes to stdout to `out`
    /// and what goes to stderr to `err`. The status is handed to
    /// [`RecordSink::record`] as is (for jq's main loop: the return value of
    /// `process()`, such as `0`, `-1` for a last output of null/false, `5`
    /// after an uncaught error).
    fn process(
        &mut self,
        value: Value,
        meta: &RecordMeta<'_>,
        out: &mut Vec<u8>,
        err: &mut Vec<u8>,
    ) -> i32;
}

/// Makes workers: called once on each worker thread, and once on the
/// calling thread (for the records it reads itself).
pub trait WorkerFactory: Sync {
    type Worker: RecordWorker;
    fn new_worker(&self) -> Self::Worker;
}

/// Receives every record's results, and the input's parse errors, in input
/// order, on the calling thread. Returning `Break` stops the run (anything
/// processed ahead is discarded).
pub trait RecordSink {
    fn record(&mut self, out: &[u8], err: &[u8], status: i32) -> ControlFlow<()>;
    /// A parse error in the input (jq's main loop prints
    /// `jq: parse error: ...` and stops; with `--seq` it continues).
    fn parse_error(&mut self, error: Error) -> ControlFlow<()>;
}

/// Engine settings.
#[derive(Clone, Copy, Debug)]
pub struct EngineOptions {
    /// Worker threads. `0` processes everything on the calling thread.
    pub threads: usize,
    /// Largest window of lines taken at once (bytes).
    pub window_bytes: usize,
    /// Smallest window worth handing to workers; less is read one record
    /// at a time (so a slow producer's records are processed as they come).
    pub min_window: usize,
    /// Largest job (bytes); windows are split into at least four jobs per
    /// thread when possible.
    pub max_job_bytes: usize,
    /// Stack size of worker threads. jq accepts 10000-deep input and
    /// recurses on its 8 MB main stack; only touched pages are committed.
    pub stack_size: usize,
}

impl Default for EngineOptions {
    /// `threads` from rayon's global pool (so `--threads N` applies).
    fn default() -> Self {
        let threads = rayon::current_num_threads();
        EngineOptions {
            threads,
            window_bytes: (threads * (8 << 20)).clamp(16 << 20, 128 << 20),
            min_window: 64 << 10,
            max_job_bytes: 1 << 20,
            stack_size: 256 << 20,
        }
    }
}

/// What the engine did (for diagnostics and tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineStats {
    /// Windows of lines handed to workers.
    pub windows: u64,
    /// Jobs (parts of windows) processed by workers.
    pub jobs: u64,
    /// Records processed by workers.
    pub worker_records: u64,
    /// Records (and parse errors) read one at a time on the calling thread.
    pub sequential: u64,
    /// Jobs whose later results were discarded because reading on from a
    /// line the worker didn't take went past the job's end.
    pub discarded_jobs: u64,
}

struct Job {
    id: usize,
    data: SharedBytes,
    base: usize,
    start: usize,
    end: usize,
    /// Newlines before `start` in this input.
    nl: u64,
    /// Start of the line containing `start`.
    line_start: usize,
    raw: bool,
    filename: Option<Arc<str>>,
}

struct Rec {
    out_end: usize,
    err_end: usize,
    status: i32,
}

struct JobResult {
    id: usize,
    out: Vec<u8>,
    err: Vec<u8>,
    recs: Vec<Rec>,
    /// The line the worker didn't take (the rest of the job is unread).
    failed_at: Option<usize>,
    panic: Option<Box<dyn std::any::Any + Send>>,
}

/// Reads all input with `reader`, processing records with workers from
/// `factory`, and hands results to `sink` in order. Stops at the end of the
/// input, when the sink returns `Break`, or (like `main.c`) before reading
/// another record once an input failed to open or read
/// ([`InputReader::failures`]).
pub fn run<F: WorkerFactory, S: RecordSink>(
    reader: &mut InputReader,
    factory: &F,
    sink: &mut S,
    opts: &EngineOptions,
) -> EngineStats {
    let mut main = factory.new_worker();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut stats = EngineStats::default();
    if opts.threads == 0 {
        while reader.failures() == 0 {
            stats.sequential += 1;
            if step(reader, &mut main, sink, &mut out, &mut err).is_break() {
                break;
            }
        }
        return stats;
    }
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let job_rx = Mutex::new(job_rx);
    let (res_tx, res_rx) = mpsc::channel::<JobResult>();
    std::thread::scope(|scope| {
        for i in 0..opts.threads {
            let job_rx = &job_rx;
            let res_tx = res_tx.clone();
            std::thread::Builder::new()
                .name(format!("qj-worker-{i}"))
                .stack_size(opts.stack_size)
                .spawn_scoped(scope, move || worker_thread(factory, job_rx, res_tx))
                .expect("spawn worker thread");
        }
        drop(res_tx);
        let mut ctx = Ctx {
            reader,
            main: &mut main,
            sink,
            out: &mut out,
            err: &mut err,
            job_tx: &job_tx,
            res_rx: &res_rx,
            opts,
            stats: &mut stats,
        };
        ctx.run();
        drop(job_tx); // workers exit
    });
    stats
}

fn worker_thread<F: WorkerFactory>(
    factory: &F,
    jobs: &Mutex<mpsc::Receiver<Job>>,
    results: mpsc::Sender<JobResult>,
) {
    let mut worker = None;
    let mut simd = SimdParser::new();
    loop {
        let job = match jobs.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(job) = job else { return };
        let id = job.id;
        let w = worker.get_or_insert_with(|| factory.new_worker());
        let result = panic::catch_unwind(AssertUnwindSafe(|| run_job(w, &mut simd, &job)))
            .unwrap_or_else(|p| {
                worker = None; // don't reuse a worker that panicked
                JobResult {
                    id,
                    out: Vec::new(),
                    err: Vec::new(),
                    recs: Vec::new(),
                    failed_at: None,
                    panic: Some(p),
                }
            });
        if results.send(result).is_err() {
            return;
        }
    }
}

/// Processes a job's lines until one the fast path wouldn't take.
fn run_job<W: RecordWorker>(worker: &mut W, simd: &mut SimdParser, job: &Job) -> JobResult {
    let buf = (*job.data).as_ref();
    let mut r = JobResult {
        id: job.id,
        out: Vec::new(),
        err: Vec::new(),
        recs: Vec::new(),
        failed_at: None,
        panic: None,
    };
    let mut a = job.start;
    let mut nl = job.nl;
    let mut ls = job.line_start;
    while a < job.end {
        let b = a
            + memchr::memchr(b'\n', &buf[a - job.base..job.end - job.base])
                .expect("jobs end at a newline")
            + 1;
        match window_line(simd, buf, job.base, a, b, job.raw) {
            Ok(None) => {}
            Ok(Some((value, e))) => {
                let meta = RecordMeta {
                    filename: job.filename.as_deref(),
                    line: window_line_number(nl, ls, b, e),
                };
                let status = worker.process(value, &meta, &mut r.out, &mut r.err);
                r.recs.push(Rec {
                    out_end: r.out.len(),
                    err_end: r.err.len(),
                    status,
                });
            }
            Err(()) => {
                r.failed_at = Some(a);
                return r;
            }
        }
        nl += 1;
        a = b;
        ls = b;
    }
    r
}

/// One record read by the reader itself, processed on the calling thread.
fn step<W: RecordWorker, S: RecordSink>(
    reader: &mut InputReader,
    worker: &mut W,
    sink: &mut S,
    out: &mut Vec<u8>,
    err: &mut Vec<u8>,
) -> ControlFlow<()> {
    match reader.next() {
        Some(Ok(value)) => {
            let filename = reader.filename_text();
            let meta = RecordMeta {
                filename: filename.as_deref(),
                line: reader.current_line(),
            };
            out.clear();
            err.clear();
            let status = worker.process(value, &meta, out, err);
            sink.record(out, err, status)
        }
        Some(Err(e)) => sink.parse_error(e),
        None => ControlFlow::Break(()),
    }
}

struct Ctx<'a, W: RecordWorker, S: RecordSink> {
    reader: &'a mut InputReader,
    main: &'a mut W,
    sink: &'a mut S,
    out: &'a mut Vec<u8>,
    err: &'a mut Vec<u8>,
    job_tx: &'a mpsc::Sender<Job>,
    res_rx: &'a mpsc::Receiver<JobResult>,
    opts: &'a EngineOptions,
    stats: &'a mut EngineStats,
}

impl<W: RecordWorker, S: RecordSink> Ctx<'_, W, S> {
    fn run(&mut self) {
        while self.reader.failures() == 0 {
            let flow = match self
                .reader
                .window(self.opts.min_window, self.opts.window_bytes)
            {
                Some(w) => self.window(w),
                None => {
                    self.stats.sequential += 1;
                    step(self.reader, self.main, self.sink, self.out, self.err)
                }
            };
            if flow.is_break() {
                return;
            }
        }
    }

    fn window(&mut self, w: Window) -> ControlFlow<()> {
        let filename: Option<Arc<str>> = self.reader.filename_text().map(Arc::from);
        let buf = (*w.data).as_ref();
        let len = w.end - w.start;
        let floor = (16 << 10).min(self.opts.max_job_bytes).max(1);
        let job_bytes =
            (len / (self.opts.threads * 4)).clamp(floor, self.opts.max_job_bytes.max(floor));
        // Split at line boundaries, counting newlines for line numbers.
        let mut jobs = Vec::new();
        let mut s = w.start;
        let mut nl = w.nl;
        let mut ls = w.line_start;
        while s < w.end {
            let target = s + job_bytes;
            let e = if target >= w.end {
                w.end
            } else {
                match memchr::memchr(b'\n', &buf[target - 1 - w.base..w.end - w.base]) {
                    Some(i) => target + i,
                    None => w.end,
                }
            };
            let lines = memchr::memchr_iter(b'\n', &buf[s - w.base..e - w.base]).count() as u64;
            jobs.push((s, e, nl, ls));
            nl += lines;
            s = e;
            ls = e;
        }
        let n = jobs.len();
        self.stats.windows += 1;
        self.stats.jobs += n as u64;
        for (id, &(start, end, nl, line_start)) in jobs.iter().enumerate() {
            let job = Job {
                id,
                data: w.data.clone(),
                base: w.base,
                start,
                end,
                nl,
                line_start,
                raw: w.raw,
                filename: filename.clone(),
            };
            if self.job_tx.send(job).is_err() {
                panic!("worker threads exited");
            }
        }
        let mut results: Vec<Option<JobResult>> = (0..n).map(|_| None).collect();
        for _ in 0..n {
            let r = self.res_rx.recv().expect("worker threads exited");
            let id = r.id;
            results[id] = Some(r);
        }
        if let Some(p) = results
            .iter_mut()
            .find_map(|r| r.as_mut().and_then(|r| r.panic.take()))
        {
            panic::resume_unwind(p);
        }
        let mut k = 0;
        while k < n {
            let r = results[k].take().expect("every job reports");
            self.stats.worker_records += r.recs.len() as u64;
            let (mut o, mut e) = (0, 0);
            for rec in &r.recs {
                let flow =
                    self.sink
                        .record(&r.out[o..rec.out_end], &r.err[e..rec.err_end], rec.status);
                o = rec.out_end;
                e = rec.err_end;
                flow?;
            }
            let Some(failed_at) = r.failed_at else {
                self.reader.commit(jobs[k].1);
                k += 1;
                continue;
            };
            // Read on from the line the worker didn't take until the reader
            // is idle at the start of a later job, whose results are then
            // valid. Jobs it read into are skipped.
            self.reader.commit(failed_at);
            let mut next = k + 1;
            loop {
                let Some(pos) = self.reader.window_position(w.generation) else {
                    self.stats.discarded_jobs += (n - next) as u64;
                    return ControlFlow::Continue(());
                };
                while next < n && jobs[next].0 < pos {
                    next += 1;
                    self.stats.discarded_jobs += 1;
                }
                let target = if next < n { jobs[next].0 } else { w.end };
                match self.reader.catch_up(w.generation, target) {
                    CatchUp::Reached => break,
                    // Past it: look for the next job (none left: done).
                    CatchUp::Beyond if next >= n => return ControlFlow::Continue(()),
                    CatchUp::Beyond => {}
                    CatchUp::Left => {
                        self.stats.discarded_jobs += (n - next) as u64;
                        return ControlFlow::Continue(());
                    }
                    CatchUp::Behind => {
                        self.stats.sequential += 1;
                        step(self.reader, self.main, self.sink, self.out, self.err)?;
                        if self.reader.failures() > 0 {
                            return ControlFlow::Break(());
                        }
                    }
                }
            }
            k = next;
        }
        ControlFlow::Continue(())
    }
}

/// A worker that prints each record like `jq -c .` (or with other dump
/// options): useful for testing the engine and as a model for the CLI's
/// worker. The status is `-1` for `null`/`false` (`JQ_OK_NULL_KIND`),
/// otherwise `0`.
pub struct DumpWorker {
    pub opts: DumpOptions,
    /// Also print `[., input_filename, input_line_number]` instead of `.`.
    pub with_position: bool,
}

impl RecordWorker for DumpWorker {
    fn process(
        &mut self,
        value: Value,
        meta: &RecordMeta<'_>,
        out: &mut Vec<u8>,
        _err: &mut Vec<u8>,
    ) -> i32 {
        let status = if matches!(value, Value::Null | Value::Bool(false)) {
            -1
        } else {
            0
        };
        if self.with_position {
            let f = match meta.filename {
                Some(s) => Value::from(s),
                None => Value::Null,
            };
            let v = Value::from(vec![value, f, Value::from(meta.line as f64)]);
            dump_to_vec(&v, &self.opts, out);
        } else {
            dump_to_vec(&value, &self.opts, out);
        }
        out.push(b'\n');
        status
    }
}

/// Makes [`DumpWorker`]s.
pub struct DumpFactory {
    pub opts: DumpOptions,
    pub with_position: bool,
}

impl WorkerFactory for DumpFactory {
    type Worker = DumpWorker;
    fn new_worker(&self) -> DumpWorker {
        DumpWorker {
            opts: self.opts.clone(),
            with_position: self.with_position,
        }
    }
}
