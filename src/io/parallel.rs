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

use super::reader::{
    CatchUp, Cut, InputReader, Record, TapeSink, window_line_number, window_line_record,
};
use super::simd::SimdParser;
use super::source::SharedBytes;
use super::tape::Doc;
use super::tape_eval::Decline;
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

    /// Whether [`RecordWorker::process_direct`] works: the calling thread's
    /// worker is asked, for the records that thread reads itself.
    fn writes_direct(&self) -> bool {
        false
    }

    /// Processes a record the calling thread read itself, writing its stdout
    /// and stderr where the sink writes them, as the sink would (such a
    /// record comes after everything the sink has been handed), so that a
    /// large output needn't be held; the sink then gets the record, with no
    /// output, and this status. Only called when
    /// [`RecordWorker::writes_direct`] says so.
    fn process_direct(&mut self, _value: Value, _meta: &RecordMeta<'_>) -> i32 {
        unreachable!("the worker doesn't write direct")
    }
}

/// Makes workers: called once on each worker thread, and once on the
/// calling thread (for the records it reads itself).
pub trait WorkerFactory: Sync {
    type Worker: RecordWorker;
    fn new_worker(&self) -> Self::Worker;

    /// A [`RecordTape`] for a worker thread, if the program can run on
    /// records as simdjson parses them (see [`crate::io::tape_eval`]).
    fn new_tape(&self) -> Option<Box<dyn RecordTape>> {
        None
    }

    /// The calling thread's tape program, for the records it reads itself,
    /// if it writes their outputs where the sink writes them, as
    /// [`RecordWorker::process_direct`] does (instead of the
    /// [`WorkerFactory::new_tape`] one, which writes into buffers). Only
    /// asked when [`WorkerFactory::new_tape`] gives one.
    fn new_direct_tape(&self) -> Option<Box<dyn TapeSink>> {
        None
    }
}

/// Runs the program on a record's text as simdjson parsed it, instead of
/// on its value. It runs while a job's lines are parsed, before the job is
/// known to hold records, so it must have no effect but its output.
pub trait RecordTape {
    /// Appends the record's stdout to `out`, with its marks (see
    /// [`RecordWorker::take_marks`]; offsets in `out`), and returns its
    /// status (as [`RecordWorker::process`] would); or declines, having
    /// written nothing, so that the record's value is processed instead.
    fn run(
        &mut self,
        doc: &Doc<'_>,
        out: &mut Vec<u8>,
        marks: &mut Vec<(usize, usize)>,
    ) -> Result<i32, Decline>;
}

/// Where the calling thread's [`RecordTape`] writes a record's output
/// (bytes and marks) for [`step`] to hand to the sink.
type TapeBuf = std::rc::Rc<std::cell::RefCell<(Vec<u8>, Vec<(usize, usize)>)>>;

/// The calling thread's [`RecordTape`], as the reader's [`TapeSink`].
struct ReaderTape {
    tape: Box<dyn RecordTape>,
    buf: TapeBuf,
}

impl TapeSink for ReaderTape {
    fn run(&mut self, doc: &Doc<'_>) -> Result<i32, Decline> {
        let (out, marks) = &mut *self.buf.borrow_mut();
        let (n, m) = (out.len(), marks.len());
        let r = self.tape.run(doc, out, marks);
        if r.is_err() {
            out.truncate(n);
            marks.truncate(m);
        }
        r
    }
}

/// A [`RecordTape`] writing into a job's phase-1 buffers.
struct TapeLines<'a, 't> {
    tape: &'a mut (dyn RecordTape + 't),
    out: &'a mut Vec<u8>,
    marks: &'a mut Vec<(usize, usize)>,
}

impl TapeSink for TapeLines<'_, '_> {
    fn run(&mut self, doc: &Doc<'_>) -> Result<i32, Decline> {
        self.tape.run(doc, self.out, self.marks)
    }
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
    /// Phase 1 done: whether every line was taken, and how many newlines
    /// the job holds (the calling thread doesn't count them).
    Parsed { id: u64, complete: bool, lines: u64 },
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
    /// `Some(n)`: go, the job starting after `n` newlines of its input
    /// (for `input_line_number`); `None`: cancelled.
    decisions: HashMap<u64, Option<u64>>,
    closed: bool,
}

impl Board {
    fn decide(&self, id: u64, go: Option<u64>) {
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

    /// Waits for job `id`'s decision (cancelled once the board is closed).
    fn wait(&self, id: u64) -> Option<u64> {
        let mut st = self.state.lock().expect("board");
        loop {
            if let Some(go) = st.decisions.remove(&id) {
                return go;
            }
            if st.closed {
                return None;
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
    // Records the calling thread reads go through the factory's tape
    // program too (it writes their outputs into `tape`, unless it writes
    // them directly).
    let tape: Option<TapeBuf> = factory.new_tape().map(|t| {
        let buf = TapeBuf::default();
        let sink: Box<dyn TapeSink> = match factory.new_direct_tape() {
            Some(direct) => direct,
            None => Box::new(ReaderTape {
                tape: t,
                buf: buf.clone(),
            }),
        };
        reader.set_tape_sink(Some(sink));
        buf
    });
    struct Unset<'a>(&'a mut InputReader, bool);
    impl Drop for Unset<'_> {
        fn drop(&mut self) {
            if self.1 {
                self.0.set_tape_sink(None);
            }
        }
    }
    let has_tape = tape.is_some();
    let guard = Unset(reader, has_tape);
    let reader = &mut *guard.0;
    if opts.threads == 0 {
        while reader.failures() == 0 {
            stats.sequential += 1;
            if step(reader, &mut main, sink, &mut out, &mut err, tape.as_ref()).is_break() {
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
            tape: tape.as_ref(),
            rest_read: None,
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
    let mut tape = factory.new_tape();
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
        // Phase 1: parse (bounded work, no user code; a tape program only
        // makes output).
        let parsed = panic::catch_unwind(AssertUnwindSafe(|| {
            parse_job(&mut simd, &job, tape.as_deref_mut())
        }));
        let parsed = match parsed {
            Ok(p) => p,
            Err(payload) => {
                simd = SimdParser::new();
                tape = factory.new_tape();
                if msgs.send(FromWorker::Panicked { id, payload }).is_err() {
                    return;
                }
                continue;
            }
        };
        let complete = parsed.failed_at.is_none();
        let lines = parsed.lines;
        if msgs
            .send(FromWorker::Parsed {
                id,
                complete,
                lines,
            })
            .is_err()
        {
            return;
        }
        let Some(start_nl) = board.wait(id) else {
            continue; // cancelled
        };
        // Phase 2: the program, on values that are known to be records.
        let filename = job.filename.as_deref();
        let run = panic::catch_unwind(AssertUnwindSafe(|| {
            let ParsedJob {
                items,
                failed_at,
                out: tape_out,
                marks: tape_marks,
                ..
            } = parsed;
            if items.iter().all(|i| matches!(i, Item::Done { .. })) {
                // All written in phase 1.
                let recs = items
                    .into_iter()
                    .map(|i| match i {
                        Item::Done {
                            out_end,
                            marks_end,
                            status,
                        } => Rec {
                            out_end,
                            err_end: 0,
                            marks_end,
                            status,
                        },
                        Item::Value(..) => unreachable!(),
                    })
                    .collect();
                return JobResult {
                    out: tape_out,
                    err: Vec::new(),
                    marks: tape_marks,
                    recs,
                    failed_at,
                };
            }
            let w = worker.get_or_insert_with(|| factory.new_worker());
            let mut r = JobResult {
                out: Vec::new(),
                err: Vec::new(),
                marks: Vec::new(),
                recs: Vec::with_capacity(items.len()),
                failed_at,
            };
            let (mut tape_at, mut tape_marks_at) = (0, 0);
            for item in items {
                let status = match item {
                    Item::Value(value, line) => {
                        let meta = RecordMeta {
                            filename,
                            line: start_nl + line,
                        };
                        let status = w.process(value, &meta, &mut r.out, &mut r.err);
                        w.take_marks(&mut r.marks);
                        status
                    }
                    Item::Done {
                        out_end,
                        marks_end,
                        status,
                    } => {
                        // Moved from phase 1's buffer, marks shifted along.
                        let at = r.out.len();
                        r.out.extend_from_slice(&tape_out[tape_at..out_end]);
                        r.marks.extend(
                            tape_marks[tape_marks_at..marks_end]
                                .iter()
                                .map(|&(off, len)| (off - tape_at + at, len)),
                        );
                        (tape_at, tape_marks_at) = (out_end, marks_end);
                        status
                    }
                };
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

/// A record of a job after phase 1.
enum Item {
    /// Its value and `input_line_number` (counted from the job's start:
    /// phase 2 adds the newlines before it), for phase 2.
    Value(Value, u64),
    /// Written by the job's [`RecordTape`]: where its output and marks end
    /// in [`ParsedJob::out`] and [`ParsedJob::marks`], and its status.
    Done {
        out_end: usize,
        marks_end: usize,
        status: i32,
    },
}

/// What phase 1 made of a job.
struct ParsedJob {
    items: Vec<Item>,
    /// The line the worker didn't take (the rest of the job is unread).
    failed_at: Option<usize>,
    /// Output of the records a [`RecordTape`] wrote.
    out: Vec<u8>,
    marks: Vec<(usize, usize)>,
    /// Newlines in the job (in the lines it took).
    lines: u64,
}

/// Phase 1: the values (with their `input_line_number` from the job's
/// start) of a job's lines, or their output if `tape` takes them, up to the
/// first line the fast path wouldn't take.
fn parse_job<'t>(
    simd: &mut SimdParser,
    job: &Job,
    mut tape: Option<&mut (dyn RecordTape + 't)>,
) -> ParsedJob {
    let buf = job.data.padded();
    let mut p = ParsedJob {
        items: Vec::new(),
        failed_at: None,
        out: Vec::new(),
        marks: Vec::new(),
        lines: 0,
    };
    let mut a = job.start;
    // (Counted from the job's start.)
    let mut nl = 0;
    let mut ls = job.line_start;
    while a < job.end {
        let b = a
            + memchr::memchr(b'\n', &buf[a - job.base..job.end - job.base])
                .expect("jobs end at a newline")
            + 1;
        let line = match &mut tape {
            Some(t) => {
                let mut sink = TapeLines {
                    tape: &mut **t,
                    out: &mut p.out,
                    marks: &mut p.marks,
                };
                window_line_record(simd, buf, job.base, a, b, job.raw, Some(&mut sink))
            }
            None => window_line_record(simd, buf, job.base, a, b, job.raw, None),
        };
        match line {
            Ok(None) => {}
            Ok(Some((Record::Value(value), e))) => {
                p.items
                    .push(Item::Value(value, window_line_number(nl, ls, b, e)));
            }
            Ok(Some((Record::Done(status), _))) => p.items.push(Item::Done {
                out_end: p.out.len(),
                marks_end: p.marks.len(),
                status,
            }),
            Err(()) => {
                p.failed_at = Some(a);
                return p;
            }
        }
        nl += 1;
        a = b;
        ls = b;
    }
    p.lines = nl;
    p
}

/// One record read by the reader itself, processed on the calling thread.
fn step<W: RecordWorker, S: RecordSink>(
    reader: &mut InputReader,
    worker: &mut W,
    sink: &mut S,
    out: &mut Vec<u8>,
    err: &mut Vec<u8>,
    tape: Option<&TapeBuf>,
) -> ControlFlow<()> {
    let next = match tape {
        Some(_) => reader.next_record(),
        None => reader.next().map(|r| r.map(Record::Value)),
    };
    match next {
        Some(Ok(Record::Done(status))) => {
            // The reader's sink wrote the outputs into `tape` (or directly,
            // leaving it empty).
            let buf = tape.expect("a tape sink");
            let (bytes, marks) = &mut *buf.borrow_mut();
            let flow = if marks.is_empty() {
                sink.record(bytes, &[], status)
            } else {
                sink.record_marked(bytes, &[], marks, status)
            };
            bytes.clear();
            marks.clear();
            flow
        }
        Some(Ok(Record::Value(value))) => {
            let filename = reader.filename_text();
            let meta = RecordMeta {
                filename: filename.as_deref(),
                line: reader.current_line(),
            };
            out.clear();
            err.clear();
            if worker.writes_direct() {
                let status = worker.process_direct(value, &meta);
                return sink.record(&[], &[], status);
            }
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
    /// Phase 1 reported: the job's newlines.
    lines: Option<u64>,
    /// Confirmed and told to run the program (and the newlines before the
    /// job, which confirming it determines).
    go: bool,
    start_nl: Option<u64>,
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
    /// Where the reader's tape sink writes (see [`step`]).
    tape: Option<&'a TapeBuf>,
    /// The input (generation) whose rest this thread reads itself: no jobs
    /// are cut from it.
    rest_read: Option<u64>,
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
            self.board.decide(s.id, None);
        }
    }

    fn sequential(&mut self) -> ControlFlow<()> {
        self.stats.sequential += 1;
        step(
            self.reader,
            self.main,
            self.sink,
            self.out,
            self.err,
            self.tape,
        )
    }

    /// The reader is idle at the front job's start: confirm it, and hand
    /// over its results once they're in.
    fn front_reached(&mut self) -> ControlFlow<()> {
        let nl = self.reader.newlines_read();
        let front = self.queue.front_mut().expect("non-empty");
        if !front.go {
            front.go = true;
            front.start_nl = Some(nl);
            self.board.decide(front.id, Some(nl));
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
        match r.failed_at {
            // (The worker counted the job's newlines.)
            None => self
                .reader
                .commit_job(slot.end, slot.lines.expect("parsed")),
            Some(at) => self.reader.commit(at),
        }
        if r.failed_at.is_some() {
            // Read that line now: a job cut from here would stop at it again.
            return self.sequential();
        }
        ControlFlow::Continue(())
    }

    fn cancel_front(&mut self) {
        let s = self.queue.pop_front().expect("non-empty");
        self.in_flight -= s.end - s.start;
        self.board.decide(s.id, None);
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
            // It starts where the previous job ends.
            let nl = prev.start_nl.expect("confirmed") + prev.lines.expect("parsed");
            let s = &mut self.queue[i];
            s.go = true;
            s.start_nl = Some(nl);
            self.board.decide(s.id, Some(nl));
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
            FromWorker::Parsed {
                id,
                complete,
                lines,
            } => {
                if let Some(s) = self.slot(id) {
                    s.complete = Some(complete);
                    s.lines = Some(lines);
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
                    // Start over from the reader when nothing is in flight
                    // (unless it reads the rest of this input itself).
                    Some(c) if self.queue.is_empty() && self.rest_read != Some(c.generation) => c,
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
            if self.queue.is_empty() && self.reader.ends_at(&next) {
                // A job for all the rest (one large document, say) would
                // keep this thread waiting: it reads the rest itself, and
                // can write large outputs as they're made.
                self.rest_read = Some(w.generation);
                self.cursor = None;
                return;
            }
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
                lines: None,
                go: false,
                start_nl: None,
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
