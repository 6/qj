//! The parallel record engine: jq's main loop (`main.c`: `process()` for
//! each input) with records processed on worker threads.
//!
//! The calling thread owns the [`InputReader`]. While the reader is between
//! records with whole lines buffered (NDJSON-like input, or `-R` lines), the
//! calling thread cuts the lines ahead into jobs at line boundaries and
//! hands them to worker threads, keeping up to
//! [`EngineOptions::window_bytes`] in flight. Each worker has its own
//! [`RecordWorker`] (its own compiled program: values are `Rc`, not `Send`)
//! and its own simdjson parser.
//!
//! A job runs in two phases. First the worker parses its lines, taking every
//! line that is exactly one text the reader's fast path would take (see
//! [`super::reader`]) and stopping at the first that isn't (a parse error,
//! `nan`, a text spanning lines, two texts on a line, ...). It reports
//! whether it took every line. Only then, when the calling thread confirms
//! that the job starts where the reader will be idle, does it run the
//! program on those values. A job is confirmed when the reader is idle at
//! its start, or when the job before it was confirmed and parsed completely.
//! So user code never runs on a line that isn't really a record, such as a
//! line inside a pretty-printed text a job happened to start in.
//!
//! When a job stops at a line, the calling thread reads from that line on
//! with the reader itself, one record at a time, until the reader is idle
//! at the start of a later job. Jobs it read into are cancelled. So the
//! records, their `input_filename`/`input_line_number`, parse errors and
//! everything else are exactly what the sequential reader gives.
//!
//! Results come back to the calling thread in input order through a
//! [`RecordSink`]: output bytes, stderr bytes, and the worker's status for
//! each record. jq's exit status depends on the order (the last record's
//! `process()` result wins), so the sink can fold them exactly as `main.c`
//! does.
//!
//! **When not to use it.** The engine is only correct when records are
//! independent. The CLI must process sequentially (`threads: 0`, or its
//! own loop over a [`super::SharedReader`]) for programs that read input
//! themselves (`input`, `inputs`), stop the run (`halt`, `halt_error`), or
//! whose output interleaving or state spans records (`debug`, `stderr`,
//! `input_line_number`... unless the worker answers them from
//! [`RecordMeta`], `$__loc__`, `limit` over `inputs`, `-s`, `-n`, `--seq`,
//! `--stream`). It is also pointless for a single large document (there's
//! one record), and gains little on inputs whose texts mostly span lines.

use std::collections::{HashMap, VecDeque};
use std::ops::ControlFlow;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

use super::reader::{CatchUp, Cut, InputReader, window_line, window_line_number};
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

    /// Called after each [`RecordWorker::process`]: appends to `marks` the
    /// spans of `out` (offset in `out`, length) that the sink must write as
    /// single writes rather than piecemeal (a stdio `fwrite` of a large `-r`
    /// string flushes differently). Most workers have none.
    fn take_marks(&mut self, _marks: &mut Vec<(usize, usize)>) {}
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
    /// [`RecordSink::record`] with the record's marks from
    /// [`RecordWorker::take_marks`]: spans of `out` as (offset in `out`,
    /// length). By default the marks are ignored.
    fn record_marked(
        &mut self,
        out: &[u8],
        err: &[u8],
        _marks: &[(usize, usize)],
        status: i32,
    ) -> ControlFlow<()> {
        self.record(out, err, status)
    }
    /// A parse error in the input (jq's main loop prints
    /// `jq: parse error: ...` and stops; with `--seq` it continues).
    fn parse_error(&mut self, error: Error) -> ControlFlow<()>;
}

/// Engine settings.
#[derive(Clone, Copy, Debug)]
pub struct EngineOptions {
    /// Worker threads. `0` processes everything on the calling thread.
    pub threads: usize,
    /// Most input bytes handed to workers and not yet consumed.
    pub window_bytes: usize,
    /// Fewest bytes of complete lines worth a job; with less available, the
    /// calling thread reads one record at a time (so a slow producer's
    /// records are processed as they come).
    pub min_window: usize,
    /// Largest job (bytes). Jobs are smaller when little input is available,
    /// so that every thread gets some.
    pub max_job_bytes: usize,
    /// Stack size of worker threads. jq accepts 10000-deep input and
    /// recurses on its 8 MB main stack; only touched pages are committed.
    pub stack_size: usize,
}

impl Default for EngineOptions {
    /// [`default_threads`] (not rayon's pool: asking rayon would start it).
    fn default() -> Self {
        let threads = default_threads();
        EngineOptions {
            threads,
            window_bytes: (threads * (8 << 20)).clamp(16 << 20, 128 << 20),
            min_window: 64 << 10,
            max_job_bytes: 1 << 20,
            stack_size: 256 << 20,
        }
    }
}

/// qj's default thread count: the available parallelism, but only the
/// non-efficiency cores on Apple Silicon (efficiency cores add contention
/// without throughput for this work).
pub fn default_threads() -> usize {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if let Some(n) = apple_non_efficiency_cpus() {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Logical CPUs across every perflevel not named "Efficiency" (M1-M4 have
/// "Performance" + "Efficiency"; M5 Pro/Max "Super" + "Performance").
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn apple_non_efficiency_cpus() -> Option<usize> {
    fn sysctl_raw(name: &str, buf: &mut [u8]) -> Option<usize> {
        let name = std::ffi::CString::new(name).ok()?;
        let mut size = buf.len();
        // SAFETY: sysctlbyname writes at most `size` bytes into `buf`.
        let ret = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (ret == 0).then_some(size)
    }
    fn sysctl_u32(name: &str) -> Option<u32> {
        let mut buf = [0u8; 4];
        (sysctl_raw(name, &mut buf)? == 4).then(|| u32::from_ne_bytes(buf))
    }
    let levels = sysctl_u32("hw.nperflevels")?;
    let mut total = 0;
    for i in 0..levels {
        let mut name = [0u8; 64];
        let n = sysctl_raw(&format!("hw.perflevel{i}.name"), &mut name)?;
        let name = String::from_utf8_lossy(&name[..n]);
        if name.trim_end_matches('\0') != "Efficiency" {
            total += sysctl_u32(&format!("hw.perflevel{i}.logicalcpu"))?;
        }
    }
    (total > 0).then_some(total as usize)
}

/// What the engine did (for diagnostics and tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineStats {
    /// Jobs handed to workers.
    pub jobs: u64,
    /// Records processed by workers.
    pub worker_records: u64,
    /// Records (and parse errors) read one at a time on the calling thread.
    pub sequential: u64,
    /// Jobs cancelled because reading on from a line a worker didn't take
    /// went past their start (their records were read one at a time).
    pub discarded_jobs: u64,
}

struct Job {
    id: u64,
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
    marks_end: usize,
    status: i32,
}

struct JobResult {
    out: Vec<u8>,
    err: Vec<u8>,
    /// Every record's marks, offsets in `out` (see RecordWorker::take_marks).
    marks: Vec<(usize, usize)>,
    recs: Vec<Rec>,
    /// The line the worker didn't take (the rest of the job is unread).
    failed_at: Option<usize>,
}

enum FromWorker {
    /// Phase 1 done: whether every line was taken.
    Parsed { id: u64, complete: bool },
    /// Phase 2 done.
    Done { id: u64, result: JobResult },
    /// The program panicked on this job.
    Panicked {
        id: u64,
        payload: Box<dyn std::any::Any + Send>,
    },
}

/// Go/cancel decisions for parsed jobs, and the shutdown flag.
#[derive(Default)]
struct Board {
    state: Mutex<BoardState>,
    cv: Condvar,
}

#[derive(Default)]
struct BoardState {
    decisions: HashMap<u64, bool>,
    closed: bool,
}

impl Board {
    fn decide(&self, id: u64, go: bool) {
        self.state.lock().expect("board").decisions.insert(id, go);
        self.cv.notify_all();
    }

    fn close(&self) {
        self.state.lock().expect("board").closed = true;
        self.cv.notify_all();
    }

    fn closed(&self) -> bool {
        self.state.lock().expect("board").closed
    }

    /// Waits for job `id`'s decision (`false` once the board is closed).
    fn wait(&self, id: u64) -> bool {
        let mut st = self.state.lock().expect("board");
        loop {
            if let Some(go) = st.decisions.remove(&id) {
                return go;
            }
            if st.closed {
                return false;
            }
            st = self.cv.wait(st).expect("board");
        }
    }
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
    run_with(reader, factory, factory.new_worker(), sink, opts)
}

/// Like [`run`], with the calling thread's worker given (for a caller that
/// has already set up a program on this thread).
pub fn run_with<F: WorkerFactory, S: RecordSink>(
    reader: &mut InputReader,
    factory: &F,
    mut main: F::Worker,
    sink: &mut S,
    opts: &EngineOptions,
) -> EngineStats {
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
    let (msg_tx, msg_rx) = mpsc::channel::<FromWorker>();
    let board = Board::default();
    let mut panic_payload = None;
    /// Releases the workers however the calling thread leaves the scope
    /// (including by panicking, so that the scope's join can't hang).
    struct Release<'a>(&'a Board);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    std::thread::scope(|scope| {
        let job_tx = job_tx; // owned here: dropped (closing the channel) on unwind
        let _release = Release(&board);
        // Workers start with the first job: small inputs never need them.
        let mut msg_tx = Some(msg_tx);
        let job_rx = &job_rx;
        let board_ref = &board;
        let mut spawn = move || {
            let Some(tx) = msg_tx.take() else {
                return;
            };
            for i in 0..opts.threads {
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name(format!("qj-worker-{i}"))
                    .stack_size(opts.stack_size)
                    .spawn_scoped(scope, move || worker_thread(factory, job_rx, board_ref, tx))
                    .expect("spawn worker thread");
            }
        };
        let mut ctx = Ctx {
            reader,
            main: &mut main,
            sink,
            out: &mut out,
            err: &mut err,
            job_tx: &job_tx,
            msg_rx: &msg_rx,
            board: &board,
            opts,
            stats: &mut stats,
            queue: VecDeque::new(),
            in_flight: 0,
            cursor: None,
            next_id: 0,
            next_go: 0,
            panic: None,
            spawn: &mut spawn,
            started: false,
        };
        ctx.run();
        panic_payload = ctx.panic.take();
        // Release waiting workers and stop the rest.
        board.close();
        drop(job_tx);
    });
    if let Some(p) = panic_payload {
        panic::resume_unwind(p);
    }
    stats
}

fn worker_thread<F: WorkerFactory>(
    factory: &F,
    jobs: &Mutex<mpsc::Receiver<Job>>,
    board: &Board,
    msgs: mpsc::Sender<FromWorker>,
) {
    let mut worker = None;
    let mut simd = SimdParser::new();
    loop {
        let job = match jobs.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(job) = job else { return };
        if board.closed() {
            continue;
        }
        let id = job.id;
        // Phase 1: parse (bounded work, no user code).
        let parsed = panic::catch_unwind(AssertUnwindSafe(|| parse_job(&mut simd, &job)));
        let (values, failed_at) = match parsed {
            Ok(p) => p,
            Err(payload) => {
                simd = SimdParser::new();
                if msgs.send(FromWorker::Panicked { id, payload }).is_err() {
                    return;
                }
                continue;
            }
        };
        let complete = failed_at.is_none();
        if msgs.send(FromWorker::Parsed { id, complete }).is_err() {
            return;
        }
        if !board.wait(id) {
            continue; // cancelled
        }
        // Phase 2: the program, on values that are known to be records.
        let w = worker.get_or_insert_with(|| factory.new_worker());
        let filename = job.filename.as_deref();
        let run = panic::catch_unwind(AssertUnwindSafe(|| {
            let mut r = JobResult {
                out: Vec::new(),
                err: Vec::new(),
                marks: Vec::new(),
                recs: Vec::with_capacity(values.len()),
                failed_at,
            };
            for (value, line) in values {
                let meta = RecordMeta { filename, line };
                let status = w.process(value, &meta, &mut r.out, &mut r.err);
                w.take_marks(&mut r.marks);
                r.recs.push(Rec {
                    out_end: r.out.len(),
                    err_end: r.err.len(),
                    marks_end: r.marks.len(),
                    status,
                });
            }
            r
        }));
        let msg = match run {
            Ok(result) => FromWorker::Done { id, result },
            Err(payload) => {
                worker = None; // don't reuse a worker that panicked
                FromWorker::Panicked { id, payload }
            }
        };
        if msgs.send(msg).is_err() {
            return;
        }
    }
}

/// Phase 1: the values (with their `input_line_number`) of a job's lines,
/// up to the first line the fast path wouldn't take.
fn parse_job(simd: &mut SimdParser, job: &Job) -> (Vec<(Value, u64)>, Option<usize>) {
    let buf = job.data.padded();
    let mut values = Vec::new();
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
            Ok(Some((value, e))) => values.push((value, window_line_number(nl, ls, b, e))),
            Err(()) => return (values, Some(a)),
        }
        nl += 1;
        a = b;
        ls = b;
    }
    (values, None)
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
            let mut marks = Vec::new();
            worker.take_marks(&mut marks);
            if marks.is_empty() {
                sink.record(out, err, status)
            } else {
                sink.record_marked(out, err, &marks, status)
            }
        }
        Some(Err(e)) => sink.parse_error(e),
        None => ControlFlow::Break(()),
    }
}

/// A job in flight, in input order.
struct Slot {
    id: u64,
    start: usize,
    end: usize,
    generation: u64,
    /// Phase 1 reported: whether every line was taken.
    complete: Option<bool>,
    /// Confirmed and told to run the program.
    go: bool,
    result: Option<JobResult>,
}

struct Ctx<'a, W: RecordWorker, S: RecordSink> {
    reader: &'a mut InputReader,
    main: &'a mut W,
    sink: &'a mut S,
    out: &'a mut Vec<u8>,
    err: &'a mut Vec<u8>,
    job_tx: &'a mpsc::Sender<Job>,
    msg_rx: &'a mpsc::Receiver<FromWorker>,
    board: &'a Board,
    opts: &'a EngineOptions,
    stats: &'a mut EngineStats,
    queue: VecDeque<Slot>,
    /// Bytes of the jobs in `queue`.
    in_flight: usize,
    /// Where the next job starts (ahead of the reader).
    cursor: Option<Cut>,
    next_id: u64,
    /// Jobs before this id are confirmed (or gone); from it on, not yet.
    next_go: u64,
    panic: Option<Box<dyn std::any::Any + Send>>,
    /// Starts the worker threads (the first call).
    spawn: &'a mut dyn FnMut(),
    started: bool,
}

impl<W: RecordWorker, S: RecordSink> Ctx<'_, W, S> {
    fn run(&mut self) {
        while self.reader.failures() == 0 && self.panic.is_none() {
            while let Ok(m) = self.msg_rx.try_recv() {
                self.absorb(m);
            }
            self.dispatch();
            let flow = match self.queue.front() {
                None => self.sequential(),
                Some(front) => match self.reader.catch_up(front.generation, front.start) {
                    CatchUp::Reached => self.front_reached(),
                    CatchUp::Behind => self.sequential(),
                    CatchUp::Beyond => {
                        self.cancel_front();
                        ControlFlow::Continue(())
                    }
                    CatchUp::Left => {
                        while !self.queue.is_empty() {
                            self.cancel_front();
                        }
                        self.cursor = None;
                        ControlFlow::Continue(())
                    }
                },
            };
            if flow.is_break() {
                break;
            }
        }
        while !self.queue.is_empty() {
            let s = self.queue.pop_front().expect("non-empty");
            self.board.decide(s.id, false);
        }
    }

    fn sequential(&mut self) -> ControlFlow<()> {
        self.stats.sequential += 1;
        step(self.reader, self.main, self.sink, self.out, self.err)
    }

    /// The reader is idle at the front job's start: confirm it, and hand
    /// over its results once they're in.
    fn front_reached(&mut self) -> ControlFlow<()> {
        let front = self.queue.front_mut().expect("non-empty");
        if !front.go {
            front.go = true;
            self.board.decide(front.id, true);
            self.next_go = front.id + 1;
            self.confirm_chain();
        }
        let front = self.queue.front_mut().expect("non-empty");
        let Some(r) = front.result.take() else {
            // Wait for a worker message (any: they all move things along).
            // Tests turn a deadlock into a failure, with a timeout generous
            // enough for unoptimized builds on a loaded machine.
            #[cfg(test)]
            let m = self
                .msg_rx
                .recv_timeout(std::time::Duration::from_secs(120))
                .unwrap_or_else(|e| {
                    let q: Vec<_> = self
                        .queue
                        .iter()
                        .map(|s| (s.id, s.start, s.end, s.complete, s.go, s.result.is_some()))
                        .collect();
                    panic!("engine stuck ({e:?}): next_go {} queue {q:?}", self.next_go)
                });
            #[cfg(not(test))]
            let m = self.msg_rx.recv().expect("worker threads exited");
            self.absorb(m);
            return ControlFlow::Continue(());
        };
        let slot = self.queue.pop_front().expect("non-empty");
        self.in_flight -= slot.end - slot.start;
        self.stats.worker_records += r.recs.len() as u64;
        let (mut o, mut e, mut m) = (0, 0, 0);
        for rec in &r.recs {
            let (out, err) = (&r.out[o..rec.out_end], &r.err[e..rec.err_end]);
            let marks = &r.marks[m..rec.marks_end];
            let flow = if marks.is_empty() {
                self.sink.record(out, err, rec.status)
            } else {
                // Offsets in the record's own output.
                let marks: Vec<(usize, usize)> =
                    marks.iter().map(|&(off, len)| (off - o, len)).collect();
                self.sink.record_marked(out, err, &marks, rec.status)
            };
            o = rec.out_end;
            e = rec.err_end;
            m = rec.marks_end;
            flow?;
        }
        // Everything before the line the worker didn't take was consumed
        // like the fast path would; the reader reads on from there.
        self.reader.commit(r.failed_at.unwrap_or(slot.end));
        if r.failed_at.is_some() {
            // Read that line now: a job cut from here would stop at it again.
            return self.sequential();
        }
        ControlFlow::Continue(())
    }

    fn cancel_front(&mut self) {
        let s = self.queue.pop_front().expect("non-empty");
        self.in_flight -= s.end - s.start;
        self.board.decide(s.id, false);
        self.stats.discarded_jobs += 1;
    }

    /// A job starts where the reader will be idle if the job before it is
    /// confirmed and took all its lines.
    fn confirm_chain(&mut self) {
        loop {
            let Some(front) = self.queue.front() else {
                return;
            };
            // Index of the first job not confirmed yet (the front itself is
            // confirmed only when the reader reaches it).
            let i = self.next_go.saturating_sub(front.id) as usize;
            if i == 0 || i >= self.queue.len() {
                return;
            }
            let prev = &self.queue[i - 1];
            if !(prev.go && prev.complete == Some(true)) {
                return;
            }
            let s = &mut self.queue[i];
            s.go = true;
            self.board.decide(s.id, true);
            self.next_go = s.id + 1;
        }
    }

    /// The job with this id, if still in flight (ids are consecutive).
    fn slot(&mut self, id: u64) -> Option<&mut Slot> {
        let front = self.queue.front()?.id;
        if id < front {
            return None;
        }
        self.queue.get_mut((id - front) as usize)
    }

    fn absorb(&mut self, m: FromWorker) {
        match m {
            FromWorker::Parsed { id, complete } => {
                if let Some(s) = self.slot(id) {
                    s.complete = Some(complete);
                    self.confirm_chain();
                }
            }
            FromWorker::Done { id, result } => {
                if let Some(s) = self.slot(id) {
                    s.result = Some(result);
                }
            }
            FromWorker::Panicked { id, payload } => {
                if self.slot(id).is_some() {
                    self.panic = Some(payload);
                }
            }
        }
    }

    /// Keeps up to `window_bytes` of jobs in flight, cut ahead of the
    /// reader.
    fn dispatch(&mut self) {
        let max_jobs = self.opts.threads * 8;
        while self.in_flight < self.opts.window_bytes && self.queue.len() < max_jobs {
            let cursor = match self.cursor {
                Some(c)
                    if self
                        .queue
                        .back()
                        .is_none_or(|b| b.generation == c.generation) =>
                {
                    c
                }
                _ => match self.reader.cut_start() {
                    // Start over from the reader when nothing is in flight.
                    Some(c) if self.queue.is_empty() => c,
                    _ => return,
                },
            };
            let ahead = self.reader.available_after(&cursor);
            // (At least min_window, or cuts near the end would never succeed.)
            let floor = (16 << 10)
                .min(self.opts.max_job_bytes)
                .max(self.opts.min_window)
                .max(1);
            let job_bytes =
                (ahead / (self.opts.threads * 4)).clamp(floor, self.opts.max_job_bytes.max(floor));
            // (A complete input's small rest is worth a job once workers run.)
            let Some((w, next)) =
                self.reader
                    .cut(cursor, self.opts.min_window, job_bytes, self.started)
            else {
                self.cursor = None;
                return;
            };
            if !self.started {
                self.started = true;
                (self.spawn)();
            }
            let id = self.next_id;
            self.next_id += 1;
            let job = Job {
                id,
                data: w.data,
                base: w.base,
                start: w.start,
                end: w.end,
                nl: w.nl,
                line_start: w.line_start,
                raw: w.raw,
                filename: self.reader.filename_text().map(Arc::from),
            };
            if self.job_tx.send(job).is_err() {
                panic!("worker threads exited");
            }
            self.queue.push_back(Slot {
                id,
                start: w.start,
                end: w.end,
                generation: w.generation,
                complete: None,
                go: false,
                result: None,
            });
            self.in_flight += w.end - w.start;
            self.stats.jobs += 1;
            self.cursor = Some(next);
        }
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
