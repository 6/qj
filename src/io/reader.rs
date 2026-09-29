//! jq's input loop (`util.c`: `jq_util_input_next_input` and `read_more`)
//! with a simdjson fast path.
//!
//! # What jq does
//!
//! jq reads every input with `fgets` into a 4096-byte buffer, so it sees
//! "chunks": one line, or 4095 bytes of a longer line. Each chunk goes to one
//! `jv_parser` shared by all inputs, which means:
//!
//! * texts continue across file boundaries (`printf 1 > a; printf 2 > b;
//!   jq . a b` prints `12`), and the parser's line/column counters used in
//!   error messages run on across files;
//! * a chunk without a newline is cut at its first NUL (`strlen`);
//! * after a parse error the rest of the *chunk* is discarded;
//! * `input_line_number` is the number of newlines in the chunks read so far
//!   from the current input (reset when the next input is opened), and
//!   `input_filename` the current input's name (`<stdin>` for `-`);
//! * the next input is opened only when the parser needs more bytes. A file
//!   that fails to open prints `Could not open file ...` and counts as a
//!   failure; `input_filename` still becomes its name.
//! * raw input (`-R`) converts each chunk to a string on its own, so a
//!   multibyte character split at a 4095-byte boundary becomes two U+FFFD.
//!
//! # How this reproduces it quickly
//!
//! While jq's parser would be between texts, a fast path consumes whole texts
//! itself: whitespace, top-level literals (jq's `check_literal`), and
//! containers/strings that simdjson accepts as one document (see
//! [`super::simd`]). For those, jq's parser is fully determined, and the fast
//! path computes the same value, the parser's position counters, and
//! `input_line_number` (from the chunk containing the byte where jq's parser
//! would emit the value). Everything else, starting at the text where the
//! fast path gives up, is fed to jq's parser port ([`Parser`]) in exactly
//! the chunks jq's `fgets` produces. The fast path takes over again once that
//! parser is idle. `--seq` and `--stream` always use jq's parser.
//!
//! The fast path never consumes a NUL byte, so its view of a chunk and jq's
//! (`strlen`-truncated) view agree up to wherever it hands over.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};

use super::simd::{Rejected, SimdParser};
use super::source::{FsOpener, InputMessage, Opened, Opener, SharedBytes, os_bytes};
use crate::jq::value::{Array, Error, Number, ParseFlags, Parser, Str, Value, unicode};
use crate::simdjson::tape_error;

/// `fgets(buf, 4096, f)` reads at most this many bytes per chunk.
pub const CHUNK: usize = 4095;

/// Bytes kept readable after the data of a stream buffer, so simdjson can
/// parse texts in place.
const PAD: usize = 64;

/// Largest single read from a stream (and the initial buffer size).
const READ_SIZE: usize = 1 << 20;

/// Room guaranteed for each read from a stream.
const MIN_READ: usize = 64 << 10;

/// Input options (`jq_util_input_set_parser` and the parser flags).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderOptions {
    /// `-R`: each line is a string.
    pub raw: bool,
    /// `-s`: one array of all texts (with `-R`: one string of all input).
    pub slurp: bool,
    /// `--seq`
    pub seq: bool,
    /// `--stream`
    pub stream: bool,
    /// `--stream-errors` (implies `stream`)
    pub stream_errors: bool,
}

impl ReaderOptions {
    fn parse_flags(&self) -> ParseFlags {
        ParseFlags {
            seq: self.seq,
            streaming: self.stream || self.stream_errors,
            stream_errors: self.stream_errors,
        }
    }
}

/// The current input's bytes and read state.
struct FileData {
    kind: DataKind,
    /// Absolute offset of the first buffered byte (streams drop consumed
    /// bytes).
    base: usize,
    /// Absolute end of the bytes available so far.
    end: usize,
    /// No more bytes will arrive.
    eof: bool,
    /// The read error that ended this input (`ferror`), reported when the
    /// input is left, like jq's next `read_more`.
    error: Option<io::Error>,
    /// Start of the line containing `end` (the last line seen so far).
    last_line_start: usize,
}

enum DataKind {
    Whole(SharedBytes),
    Stream {
        reader: Box<dyn Read>,
        /// Buffered bytes `[base, end)`, followed by `PAD` readable bytes.
        buf: Vec<u8>,
        fd: Option<i32>,
    },
}

impl FileData {
    fn new(opened: Opened) -> FileData {
        match opened {
            Opened::Whole(bytes) => {
                let len = bytes.data().len();
                let last_line_start = memchr::memrchr(b'\n', bytes.data()).map_or(0, |i| i + 1);
                FileData {
                    kind: DataKind::Whole(bytes),
                    base: 0,
                    end: len,
                    eof: true,
                    error: None,
                    last_line_start,
                }
            }
            Opened::Stream { reader, fd } => FileData {
                kind: DataKind::Stream {
                    reader,
                    buf: Vec::new(),
                    fd,
                },
                base: 0,
                end: 0,
                eof: false,
                error: None,
                last_line_start: 0,
            },
        }
    }

    /// The buffered bytes, starting at absolute offset `self.base`, plus
    /// padding when there is some (`PAD` bytes for streams, a page for
    /// memory maps).
    #[inline]
    fn buf(&self) -> &[u8] {
        match &self.kind {
            DataKind::Whole(b) => b.padded(),
            DataKind::Stream { buf, .. } => buf,
        }
    }

    /// Byte at absolute offset `p` (`base <= p < end`).
    #[inline]
    fn at(&self, p: usize) -> u8 {
        self.buf()[p - self.base]
    }

    /// Absolute range `[a, b)` as a slice.
    #[inline]
    fn slice(&self, a: usize, b: usize) -> &[u8] {
        &self.buf()[a - self.base..b - self.base]
    }

    /// End of the last complete `fgets` chunk: everything if at EOF,
    /// otherwise up to the last newline or the last full 4095-byte chunk of
    /// the unfinished line. jq emits nothing from a chunk it hasn't finished
    /// reading, and neither does the fast path.
    #[inline]
    fn avail_end(&self) -> usize {
        if self.eof {
            self.end
        } else {
            let ls = self.last_line_start;
            ls + (self.end - ls) / CHUNK * CHUNK
        }
    }

    /// Reads more bytes, keeping everything from `keep` on. With `wait`,
    /// the first read may block (the reader needs data). Other reads happen
    /// only while more is available right away (a pipe with data waiting,
    /// or a stream without a descriptor, like a decompressor), until `max`
    /// bytes are buffered. So a fast producer gives large windows, and a
    /// slow one never delays records that are complete.
    fn fill(&mut self, keep: usize, wait: bool, max: usize) {
        let DataKind::Stream { reader, buf, fd } = &mut self.kind else {
            return;
        };
        if self.eof {
            return;
        }
        if !wait && self.end - self.base >= max {
            return;
        }
        // Drop consumed bytes when they dominate the buffer.
        let drop = keep.saturating_sub(self.base);
        if drop > 0 && drop >= (self.end - self.base) / 2 {
            let live = self.end - keep;
            buf.copy_within(drop..drop + live, 0);
            self.base = keep;
        }
        let mut first = true;
        loop {
            if !(first && wait) && fd.is_some_and(|fd| !super::source::readable_now(fd)) {
                return;
            }
            first = false;
            // The buffer only grows (by doubling); bytes past the data are
            // stale or zero, which is fine as simdjson padding.
            let len = self.end - self.base;
            if buf.len() < len + MIN_READ + PAD {
                let want = (len + MIN_READ + PAD).max(buf.len() * 2);
                let mut grown = vec![0u8; want];
                grown[..len].copy_from_slice(&buf[..len]);
                *buf = grown;
            }
            let room = (buf.len() - PAD - len).min(READ_SIZE.max(len));
            let r = reader.read(&mut buf[len..len + room]);
            match r {
                Ok(0) => {
                    self.eof = true;
                }
                Ok(n) => {
                    if let Some(i) = memchr::memrchr(b'\n', &buf[len..len + n]) {
                        self.last_line_start = self.end + i + 1;
                    }
                    self.end += n;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.error = Some(e);
                    self.eof = true;
                }
            }
            if self.eof || self.end - self.base >= max {
                return;
            }
        }
    }
}

/// Position bookkeeping within the current input.
#[derive(Clone, Copy, Default)]
struct FilePos {
    /// Absolute parse position (fast path), or the start of the chunk last
    /// fed to jq's parser.
    pos: usize,
    /// Newlines in `[0, pos)`.
    nl: u64,
    /// Start of the line containing `pos`.
    line_start: usize,
}

/// A chunk handed to jq's parser.
#[derive(Clone, Copy)]
struct FedChunk {
    start: usize,
    /// End of the chunk in the input (before any NUL truncation).
    end: usize,
    /// Bytes given to the parser.
    fed: usize,
}

enum Json {
    /// jq's parser would be idle; the fast path is in charge.
    Fast,
    /// jq's parser port has the stream.
    Slow {
        parser: Box<Parser>,
        chunk: Option<FedChunk>,
        /// Stay with jq's parser (always for `--seq`/`--stream`; also after
        /// "Malformed BOM", which it then reports on every call).
        pinned: bool,
    },
}

enum Fast {
    Value(Value),
    /// Hand the stream to jq's parser at the current position.
    Slow,
    /// Read more of the current (stream) input, then retry.
    NeedData,
    /// The current input is fully consumed.
    Exhausted,
}

/// Where a scan for the end of a text (container or string) stands.
#[derive(Clone, Copy)]
struct ExtentScan {
    start: usize,
    pos: usize,
    depth: u32,
    in_string: bool,
}

/// jq's input state (`jq_util_input_state`), reading JSON texts or raw lines
/// from a list of inputs.
pub struct InputReader {
    opts: ReaderOptions,
    flags: ParseFlags,
    opener: Box<dyn Opener>,
    files: Vec<OsString>,
    next_file: usize,
    cur: Option<FileData>,
    fp: FilePos,
    /// jq's `feof(current_input) || ferror(current_input)` (used with jq's
    /// parser, which must see exactly jq's sequence of buffers).
    jq_eof: bool,
    failures: usize,
    filename: Option<Str>,
    current_line: u64,
    json: Json,
    /// jq's parser position counters while the fast path is in charge.
    pline: i32,
    pcol: i32,
    /// The input's first bytes have been checked for a BOM.
    bom_done: bool,
    /// Everything has been read (and the end reported).
    ended: bool,
    /// `-s`: the array (or `-R -s` string) being collected; `None` once
    /// returned.
    slurped: Option<Value>,
    simd: SimdParser,
    fast: bool,
    pending_scan: Option<ExtentScan>,
    /// The line (ending here) holds more than one text or a text that isn't
    /// valid JSON: find text ends by scanning, not by trying the line.
    no_line_try_until: usize,
    /// Whether this input's rest was tried as a single text already.
    tried_rest: bool,
    on_message: Box<dyn FnMut(InputMessage)>,
    stats: ReaderStats,
    /// Incremented for every input opened (see `catch_up`).
    generation: u64,
}

/// How the reader produced its values (for diagnostics and tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderStats {
    /// Values produced by the fast path (simdjson or top-level literals).
    pub fast_values: u64,
    /// Values and errors produced by jq's parser port.
    pub parser_results: u64,
    /// Times the fast path handed the stream to jq's parser.
    pub handovers: u64,
}

impl InputReader {
    /// A reader over the given inputs (`"-"` is standard input), opened
    /// lazily from the file system. jq reads standard input when there are
    /// no file arguments; the caller passes `["-"]` then.
    pub fn new(files: Vec<OsString>, opts: ReaderOptions) -> InputReader {
        InputReader::with_opener(files, opts, Box::new(FsOpener::default()))
    }

    /// Like [`InputReader::new`] with a custom way of opening inputs.
    pub fn with_opener(
        files: Vec<OsString>,
        opts: ReaderOptions,
        opener: Box<dyn Opener>,
    ) -> InputReader {
        let flags = opts.parse_flags();
        let slurped = match (opts.slurp, opts.raw) {
            (true, true) => Some(Value::String(Str::new())),
            (true, false) => Some(Value::Array(Array::new())),
            _ => None,
        };
        let fast = !flags.seq && !flags.streaming && std::env::var_os("QJ_NO_SIMD_INPUT").is_none();
        let mut r = InputReader {
            opts,
            flags,
            opener,
            files,
            next_file: 0,
            cur: None,
            fp: FilePos::default(),
            jq_eof: false,
            failures: 0,
            filename: None,
            current_line: 0,
            json: Json::Fast,
            pline: 1,
            pcol: 0,
            bom_done: false,
            ended: false,
            slurped,
            simd: SimdParser::new(),
            fast,
            pending_scan: None,
            no_line_try_until: 0,
            tried_rest: false,
            on_message: Box::new(default_message_sink),
            stats: ReaderStats::default(),
            generation: 0,
        };
        if !r.fast {
            r.enter_slow();
        }
        r
    }

    /// Where `Could not open file` and read-error messages go. The default
    /// prints them to stderr with the `qj` prefix.
    pub fn set_message_sink(&mut self, sink: Box<dyn FnMut(InputMessage)>) {
        self.on_message = sink;
    }

    /// Disables (or re-enables) the simdjson fast path, leaving everything to
    /// jq's parser port. Must be called before reading.
    pub fn set_fast_path(&mut self, enabled: bool) {
        let enabled = enabled && !self.flags.seq && !self.flags.streaming;
        if enabled == self.fast {
            return;
        }
        self.fast = enabled;
        self.json = Json::Fast;
        if !enabled {
            self.enter_slow();
        }
    }

    /// `jq_util_input_add_input`.
    pub fn add_input(&mut self, name: impl Into<OsString>) {
        self.files.push(name.into());
    }

    /// `jq_util_input_errors`: inputs that failed to open or read.
    pub fn failures(&self) -> usize {
        self.failures
    }

    /// `input_filename`: the current input's name (`<stdin>` for `-`), or
    /// `null` before any input was opened.
    pub fn current_filename(&self) -> Value {
        match &self.filename {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }

    /// Counts of how values were produced so far.
    pub fn stats(&self) -> ReaderStats {
        self.stats
    }

    /// `input_line_number`.
    pub fn current_line(&self) -> u64 {
        self.current_line
    }

    /// `jq_util_input_get_position`: `<file>:<line>` for error messages, or
    /// `<unknown>` before any input was opened.
    pub fn position(&self) -> String {
        match &self.filename {
            // (`%s` stops at a NUL.)
            Some(s) => format!("{}:{}", s.as_c_str(), self.current_line),
            None => "<unknown>".to_owned(),
        }
    }

    /// `jq_util_input_next_input`: the next input value, a parse error, or
    /// `None` when all input has been read. With `-s` the single slurped
    /// value comes at the end (parse errors are still returned as they
    /// occur).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Result<Value, Error>> {
        if self.opts.raw {
            return self.next_raw();
        }
        if self.slurped.is_some() {
            loop {
                match self.next_json() {
                    Some(Ok(v)) => {
                        if let Some(Value::Array(a)) = &mut self.slurped {
                            a.push(v);
                        }
                    }
                    Some(Err(e)) => return Some(Err(e)),
                    None => return self.slurped.take().map(Ok),
                }
            }
        }
        self.next_json()
    }

    // ---- files -------------------------------------------------------

    /// Opens the next input, like the second half of jq's `read_more`: the
    /// name becomes `input_filename` and the line count restarts, even if
    /// opening fails. At most one attempt; `false` if no input was left.
    fn open_one(&mut self) -> bool {
        if self.next_file >= self.files.len() {
            return false;
        }
        let name = self.files[self.next_file].clone();
        self.next_file += 1;
        self.generation += 1;
        self.current_line = 0;
        self.fp = FilePos::default();
        self.jq_eof = false;
        self.pending_scan = None;
        self.no_line_try_until = 0;
        self.tried_rest = false;
        self.filename = Some(if name == "-" {
            Str::from("<stdin>")
        } else {
            Str::from_bytes(os_bytes(&name))
        });
        match self.opener.open(&name) {
            Ok(opened) => self.cur = Some(FileData::new(opened)),
            Err(error) => {
                self.failures += 1;
                (self.on_message)(InputMessage::OpenFailed { name, error });
            }
        }
        true
    }

    /// Leaves the current input (jq's `read_more` at `feof`/`ferror`): a
    /// read error is printed now; it was counted when the read failed.
    fn leave_current(&mut self) {
        if let Some(cur) = self.cur.take()
            && let Some(error) = cur.error
        {
            (self.on_message)(InputMessage::ReadFailed { error });
        }
    }

    /// Opens inputs until one opens. Returns whether one is open. Only for
    /// the fast path, whose parser is idle: then jq's parser would return
    /// nothing between these `read_more` calls.
    fn advance_file(&mut self) -> bool {
        self.leave_current();
        while self.open_one() {
            if self.cur.is_some() {
                return true;
            }
        }
        false
    }

    /// The fast path has consumed the current input.
    fn close_current(&mut self) {
        if self.cur.as_ref().is_some_and(|c| c.error.is_some()) {
            self.failures += 1; // counted by jq's failing fgets
        }
        self.current_line = self.fp.nl;
        self.leave_current();
    }

    /// Reads more of the current input (blocking).
    fn fill(&mut self) {
        let keep = match self.pending_scan {
            Some(s) => s.start.min(self.fp.pos),
            None => self.fp.pos,
        };
        if let Some(cur) = &mut self.cur {
            cur.fill(keep, true, 0);
        }
    }

    /// Advances `fp` to `to`, counting newlines. `pline`/`pcol` follow when
    /// `count_parser` is set (bytes the fast path consumes).
    fn advance(&mut self, to: usize, count_parser: bool) {
        let from = self.fp.pos;
        if to <= from {
            return;
        }
        let cur = self.cur.as_ref().expect("an open input");
        let bytes = cur.slice(from, to);
        let mut n = 0u64;
        let mut last = None;
        for i in memchr::memchr_iter(b'\n', bytes) {
            n += 1;
            last = Some(i);
        }
        self.fp.pos = to;
        if let Some(i) = last {
            self.fp.nl += n;
            self.fp.line_start = from + i + 1;
            if count_parser {
                self.pline = self.pline.wrapping_add(n as i32);
                self.pcol = (to - (from + i + 1)) as i32;
            }
        } else if count_parser {
            self.pcol = self.pcol.wrapping_add((to - from) as i32);
        }
    }

    /// `input_line_number` when jq's parser emits a value at byte `e`: the
    /// newlines up to the end of the chunk containing `e`. `fp` must be
    /// valid for `e` (no newline in `[fp.pos, e)`).
    fn emission_line(&self, e: usize) -> u64 {
        let cur = self.cur.as_ref().expect("an open input");
        let ls = self.fp.line_start;
        let limit = ls + ((e - ls) / CHUNK + 1) * CHUNK;
        let stop = limit.min(cur.end);
        let has_nl = e < stop && memchr::memchr(b'\n', cur.slice(e, stop)).is_some();
        self.fp.nl + has_nl as u64
    }

    // ---- JSON --------------------------------------------------------

    fn enter_slow(&mut self) {
        let parser = if self.bom_done {
            Parser::resume(self.flags, self.pline, self.pcol)
        } else {
            Parser::new(self.flags)
        };
        self.bom_done = true;
        self.pending_scan = None;
        self.jq_eof = false;
        self.json = Json::Slow {
            parser: Box::new(parser),
            chunk: None,
            pinned: !self.fast,
        };
    }

    fn next_json(&mut self) -> Option<Result<Value, Error>> {
        loop {
            if self.ended {
                return None;
            }
            if !matches!(self.json, Json::Fast) {
                return self.next_slow();
            }
            if self.cur.is_none() && !self.advance_file() {
                // All inputs done; jq's parser is idle, so its final
                // buffer yields nothing.
                self.ended = true;
                return None;
            }
            match self.fast_step() {
                Fast::Value(v) => {
                    self.stats.fast_values += 1;
                    return Some(Ok(v));
                }
                Fast::Slow => {
                    self.stats.handovers += 1;
                    self.enter_slow();
                }
                Fast::NeedData => self.fill(),
                Fast::Exhausted => self.close_current(),
            }
        }
    }

    /// jq's `next_input` loop with jq's parser port: `read_more` whenever
    /// the parser has consumed its buffer, then `jv_parser_next`.
    fn next_slow(&mut self) -> Option<Result<Value, Error>> {
        let mut is_last = false;
        loop {
            let Json::Slow { parser, .. } = &mut self.json else {
                unreachable!()
            };
            if parser.remaining() == 0 {
                is_last = self.slow_read_more();
            }
            let Json::Slow {
                parser,
                chunk,
                pinned,
            } = &mut self.json
            else {
                unreachable!()
            };
            match parser.next() {
                Some(r) => {
                    if let Err(e) = &r
                        && e.as_str() == Some("Malformed BOM")
                    {
                        *pinned = true; // reported on every call from now on
                    }
                    if !*pinned && !is_last && parser.is_idle() {
                        // Back to the fast path, right after what the
                        // parser consumed (or after the whole chunk if it
                        // was all consumed, or discarded after an error).
                        let (line, column) = parser.position();
                        let rem = parser.remaining();
                        let resume = match chunk {
                            Some(c) if rem > 0 => c.start + c.fed - rem,
                            Some(c) => c.end,
                            None => self.fp.pos,
                        };
                        self.pline = line;
                        self.pcol = column;
                        self.json = Json::Fast;
                        if self.cur.is_some() {
                            self.advance(resume, false);
                        }
                    }
                    self.stats.parser_results += 1;
                    return Some(r);
                }
                None if is_last => {
                    self.ended = true;
                    return None;
                }
                None => {}
            }
        }
    }

    /// jq's `read_more` followed by `jv_parser_set_buf`: leave an input at
    /// EOF and open the next (one per call), then give the parser the next
    /// `fgets` chunk. Returns `is_last`.
    fn slow_read_more(&mut self) -> bool {
        // The previous chunk is fully consumed now.
        if let Json::Slow { chunk, .. } = &mut self.json
            && let Some(c) = chunk.take()
        {
            self.advance(c.end, false);
        }
        if self.cur.is_none() || self.jq_eof {
            self.leave_current();
            self.jq_eof = false;
            self.open_one();
        }
        let mut fed: Option<FedChunk> = None;
        while let Some(cur) = self.cur.as_ref() {
            let p = self.fp.pos;
            match next_chunk(cur, p, self.fp.line_start) {
                ChunkAt::Chunk { end, has_nl } => {
                    let bytes = cur.slice(p, end);
                    let len = if has_nl {
                        bytes.len()
                    } else {
                        // `strlen`: a chunk without a newline stops at NUL.
                        memchr::memchr(0, bytes).unwrap_or(bytes.len())
                    };
                    fed = Some(FedChunk {
                        start: p,
                        end,
                        fed: len,
                    });
                    self.current_line = self.fp.nl + has_nl as u64;
                    let ls = self.fp.line_start;
                    let limit = ls + ((p - ls) / CHUNK + 1) * CHUNK;
                    if !has_nl && end == cur.end && cur.eof && end < limit {
                        // fgets hit the end while reading this chunk.
                        self.jq_eof = true;
                    }
                    break;
                }
                ChunkAt::NeedData => self.fill(),
                ChunkAt::Exhausted => {
                    // fgets returns NULL: EOF, or a read error (counted now,
                    // printed when the input is left).
                    self.jq_eof = true;
                    if cur.error.is_some() {
                        self.failures += 1;
                    }
                    break;
                }
            }
        }
        let is_last = self.next_file >= self.files.len() && self.cur.is_none();
        let Json::Slow { parser, chunk, .. } = &mut self.json else {
            unreachable!()
        };
        match fed {
            Some(c) => {
                let cur = self.cur.as_ref().expect("an open input");
                parser.set_buf(cur.slice(c.start, c.start + c.fed), !is_last);
                *chunk = Some(c);
            }
            None => parser.set_buf(&[], !is_last),
        }
        is_last
    }

    // ---- the fast path -----------------------------------------------

    fn fast_step(&mut self) -> Fast {
        let Some(cur) = self.cur.as_ref() else {
            return Fast::Exhausted;
        };
        if !self.bom_done {
            let p = self.fp.pos;
            if p == cur.end {
                return if cur.eof {
                    Fast::Exhausted
                } else {
                    Fast::NeedData
                };
            }
            if cur.at(p) != 0xEF {
                self.bom_done = true;
            } else if cur.end - p >= 3 {
                if cur.slice(p, p + 3) != b"\xEF\xBB\xBF" {
                    return Fast::Slow; // malformed BOM: jq's parser reports it
                }
                // The BOM is skipped before jq's parser sees any byte: it
                // counts for fgets chunks but not for line/column.
                self.fp.pos = p + 3;
                self.bom_done = true;
            } else if !cur.eof {
                return Fast::NeedData;
            } else {
                return Fast::Slow; // a BOM prefix at the end of an input
            }
        }
        let cur = self.cur.as_ref().expect("checked");
        let avail = cur.avail_end();
        // Whitespace between texts.
        let start = self.fp.pos;
        let mut p = start;
        let buf = cur.buf();
        let base = cur.base;
        while p < avail && matches!(buf[p - base], b' ' | b'\t' | b'\r' | b'\n') {
            p += 1;
        }
        if p > start {
            self.advance(p, true);
        }
        let cur = self.cur.as_ref().expect("checked");
        if p >= avail {
            // (Past a BOM, the position can be beyond the complete chunks.)
            return if cur.eof {
                Fast::Exhausted
            } else {
                Fast::NeedData
            };
        }
        match cur.at(p) {
            b'{' | b'[' => self.fast_container(avail),
            b'"' => self.fast_string(avail),
            b']' | b'}' | b',' | b':' | 0 => Fast::Slow,
            _ => self.fast_literal(avail),
        }
    }

    /// A top-level container starting at `fp.pos`.
    fn fast_container(&mut self, avail: usize) -> Fast {
        let p = self.fp.pos;
        let cur = self.cur.as_ref().expect("an open input");
        let close = if cur.at(p) == b'[' { b']' } else { b'}' };
        let resumed = self.pending_scan.filter(|s| s.start == p);
        if resumed.is_none() && p >= self.no_line_try_until {
            // NDJSON: the rest of the line is probably exactly this text.
            let line = cur.slice(p, avail);
            let line_end = p + memchr::memchr(b'\n', line).unwrap_or(line.len());
            let mut t = line_end;
            while t > p && matches!(cur.at(t - 1), b' ' | b'\t' | b'\r') {
                t -= 1;
            }
            if cur.at(t - 1) == close {
                match self.simd.parse(cur.buf(), p - cur.base, t - cur.base) {
                    Ok(v) => {
                        self.pcol = self.pcol.wrapping_add((t - p) as i32);
                        self.fp.pos = t;
                        self.current_line = self.emission_line(t - 1);
                        return Fast::Value(v);
                    }
                    Err(Rejected(code)) if !is_extent_error(code) => return Fast::Slow,
                    Err(_) => {}
                }
            }
            // More than one text on this line, or a text spanning lines.
            self.no_line_try_until = line_end;
            // A large input is often one document: try the whole rest once
            // before scanning for the text's end.
            if cur.eof && !self.tried_rest && cur.end - p >= 1 << 16 {
                self.tried_rest = true;
                let mut t = cur.end;
                while t > p && matches!(cur.at(t - 1), b' ' | b'\t' | b'\r' | b'\n') {
                    t -= 1;
                }
                if t > line_end
                    && cur.at(t - 1) == close
                    && let Ok(v) = self.simd.parse(cur.buf(), p - cur.base, t - cur.base)
                {
                    self.advance(t, true);
                    self.current_line = self.emission_line(t - 1);
                    return Fast::Value(v);
                }
            }
        }
        let cur = self.cur.as_ref().expect("an open input");
        let mut scan = resumed.unwrap_or(ExtentScan {
            start: p,
            pos: p,
            depth: 0,
            in_string: false,
        });
        self.pending_scan = None;
        match scan_extent(cur, &mut scan, avail) {
            Some(e) => self.fast_text(p, e),
            None if cur.eof => Fast::Slow,
            None => {
                self.pending_scan = Some(scan);
                Fast::NeedData
            }
        }
    }

    /// A top-level string starting at `fp.pos`.
    fn fast_string(&mut self, avail: usize) -> Fast {
        let p = self.fp.pos;
        let cur = self.cur.as_ref().expect("an open input");
        let mut scan = self
            .pending_scan
            .filter(|s| s.start == p)
            .unwrap_or(ExtentScan {
                start: p,
                pos: p + 1,
                depth: 0,
                in_string: true,
            });
        self.pending_scan = None;
        match scan_extent(cur, &mut scan, avail) {
            Some(e) => self.fast_text(p, e),
            None if cur.eof => Fast::Slow,
            None => {
                self.pending_scan = Some(scan);
                Fast::NeedData
            }
        }
    }

    /// The text `[p, e]` (a container or string) through simdjson.
    fn fast_text(&mut self, p: usize, e: usize) -> Fast {
        let cur = self.cur.as_ref().expect("an open input");
        match self.simd.parse(cur.buf(), p - cur.base, e + 1 - cur.base) {
            Ok(v) => {
                self.advance(e + 1, true);
                self.current_line = self.emission_line(e);
                Fast::Value(v)
            }
            Err(_) => Fast::Slow,
        }
    }

    /// A top-level literal (`true`, a number, `nan`, ...) at `fp.pos`: jq
    /// emits it at the byte that ends its token.
    fn fast_literal(&mut self, avail: usize) -> Fast {
        let p = self.fp.pos;
        let cur = self.cur.as_ref().expect("an open input");
        let buf = cur.buf();
        let base = cur.base;
        let mut q = p;
        while q < avail && !LITERAL_STOP[buf[q - base] as usize] {
            q += 1;
        }
        if q == avail {
            // The token may continue in the next chunk (or input).
            return if cur.eof { Fast::Slow } else { Fast::NeedData };
        }
        let delim = buf[q - base];
        if !matches!(delim, b' ' | b'\t' | b'\r' | b'\n' | b'[' | b'{' | b'"') {
            // `]`, `}`, `,`, `:` right after a literal is an error that also
            // drops the literal; NUL needs jq's chunk view.
            return Fast::Slow;
        }
        let Some(v) = check_literal(&buf[p - base..q - base]) else {
            return Fast::Slow;
        };
        self.pcol = self.pcol.wrapping_add((q - p) as i32);
        self.fp.pos = q;
        self.current_line = self.emission_line(q);
        if matches!(delim, b' ' | b'\t' | b'\r' | b'\n') {
            self.advance(q + 1, true);
        }
        // Otherwise the delimiter opens the next text, which starts there.
        Fast::Value(v)
    }

    // ---- raw input ---------------------------------------------------

    /// jq's raw-input loop: lines (chunk by chunk), or everything with `-s`.
    fn next_raw(&mut self) -> Option<Result<Value, Error>> {
        let mut value: Option<String> = None;
        loop {
            match self.raw_chunk() {
                Some((a, b, has_nl)) => {
                    let cur = self.cur.as_ref().expect("an open input");
                    let bytes = cur.slice(a, b);
                    if let Some(Value::String(s)) = &mut self.slurped {
                        unicode::push_lossy(s.make_mut(), bytes);
                    } else if has_nl {
                        let line = &bytes[..bytes.len() - 1];
                        let s = match value {
                            None => Str::from_bytes(line),
                            Some(mut v) => {
                                unicode::push_lossy(&mut v, line);
                                Str::from(v)
                            }
                        };
                        return Some(Ok(Value::String(s)));
                    } else {
                        unicode::push_lossy(value.get_or_insert_with(String::new), bytes);
                    }
                }
                None => {
                    if let Some(s) = self.slurped.take() {
                        return Some(Ok(s));
                    }
                    return value.map(|v| Ok(Value::String(Str::from(v))));
                }
            }
        }
    }

    /// The next raw `fgets` chunk `(start, end, has_newline)` (consumed), or
    /// `None` when all inputs are exhausted.
    fn raw_chunk(&mut self) -> Option<(usize, usize, bool)> {
        loop {
            if self.ended {
                return None;
            }
            if self.cur.is_none() && !self.advance_file() {
                self.ended = true;
                return None;
            }
            let cur = self.cur.as_ref().expect("an open input");
            let p = self.fp.pos;
            match next_chunk(cur, p, self.fp.line_start) {
                ChunkAt::Chunk { end, has_nl } => {
                    self.advance(end, false);
                    self.current_line = self.fp.nl;
                    return Some((p, end, has_nl));
                }
                ChunkAt::NeedData => self.fill(),
                ChunkAt::Exhausted => {
                    self.current_line = self.fp.nl;
                    self.close_current();
                }
            }
        }
    }
    // ---- windows for the parallel engine -------------------------------

    /// Whether the reader is between records in a state where whole lines
    /// can be processed without it: raw lines (not slurped), or JSON with
    /// the fast path idle.
    fn windowable(&self) -> bool {
        if self.opts.slurp || self.ended {
            return false;
        }
        if self.opts.raw {
            return true;
        }
        self.fast && matches!(self.json, Json::Fast) && self.bom_done && self.pending_scan.is_none()
    }

    /// Where a job of whole lines could start now: the reader's position,
    /// if it is idle between records in a mode that allows jobs.
    pub(crate) fn cut_start(&self) -> Option<Cut> {
        if !self.windowable() {
            return None;
        }
        self.cur.as_ref()?;
        Some(Cut {
            generation: self.generation,
            pos: self.fp.pos,
            nl: self.fp.nl,
            line_start: self.fp.line_start,
        })
    }

    /// The complete lines of the current input from `at` (at or after the
    /// reader's position; the reader needn't be idle there, this is
    /// speculative): at most `max` bytes (a longer first line whole), if at
    /// least `min` are available now (or, with `small_rest`, all the rest
    /// of a complete input). Reads more of a stream only if it is available
    /// without waiting. Returns the lines and where the next cut starts.
    pub(crate) fn cut(
        &mut self,
        at: Cut,
        min: usize,
        max: usize,
        small_rest: bool,
    ) -> Option<(Window, Cut)> {
        if self.generation != at.generation
            || self.opts.slurp
            || self.ended
            || !(self.opts.raw || self.fast)
            || at.pos < self.fp.pos
        {
            return None;
        }
        let keep = match self.pending_scan {
            Some(s) => s.start.min(self.fp.pos),
            None => self.fp.pos,
        };
        let cur = self.cur.as_mut()?;
        if matches!(cur.kind, DataKind::Stream { .. }) && !cur.eof {
            // (Room for a job of `max` and for extending one to `min`.)
            let want = max.max(min).saturating_mul(2);
            cur.fill(keep, false, (at.pos - keep).saturating_add(want));
        }
        let start = at.pos;
        let avail = cur.avail_end();
        if avail <= start {
            return None;
        }
        let limit = avail.min(start.saturating_add(max));
        let mut end = match memchr::memrchr(b'\n', cur.slice(start, limit)) {
            Some(i) => start + i + 1,
            None => start + memchr::memchr(b'\n', cur.slice(start, avail))? + 1,
        };
        if end - start < min && start + min < avail {
            // Snapping back to a line end made it too small: go forward.
            if let Some(i) = memchr::memchr(b'\n', cur.slice(start + min, avail)) {
                end = start + min + i + 1;
            }
        }
        // With `small_rest`, the rest of a complete input is taken however
        // small (it won't grow by waiting).
        if end - start < min && !(small_rest && cur.eof && end == avail) {
            return None;
        }
        let lines = memchr::memchr_iter(b'\n', cur.slice(start, end)).count() as u64;
        let (data, base) = match &cur.kind {
            DataKind::Whole(b) => (b.clone(), 0),
            DataKind::Stream { buf, .. } => {
                let mut copy = Vec::with_capacity(end - start + PAD);
                copy.extend_from_slice(&buf[start - cur.base..end - cur.base]);
                copy.resize(end - start + PAD, 0);
                (std::sync::Arc::new(copy) as SharedBytes, start)
            }
        };
        let window = Window {
            data,
            base,
            start,
            end,
            nl: at.nl,
            line_start: at.line_start,
            generation: at.generation,
            raw: self.opts.raw,
        };
        let next = Cut {
            generation: at.generation,
            pos: end,
            nl: at.nl + lines,
            line_start: end,
        };
        Some((window, next))
    }

    /// Bytes of complete lines available from `at` without waiting.
    pub(crate) fn available_after(&self, at: &Cut) -> usize {
        match &self.cur {
            Some(cur) if self.generation == at.generation => cur.avail_end().saturating_sub(at.pos),
            _ => 0,
        }
    }

    /// Records up to `upto` (a line start within the last job) were
    /// processed by the engine: move past them as the fast path would have.
    pub(crate) fn commit(&mut self, upto: usize) {
        self.advance(upto, true);
    }

    /// After records were read one at a time from a window's line: whether
    /// the reader has reached `target` (a line start of window
    /// `generation`), consuming whitespace up to it (JSON only) when it is
    /// idle there.
    pub(crate) fn catch_up(&mut self, generation: u64, target: usize) -> CatchUp {
        if self.generation != generation || self.cur.is_none() || self.ended {
            return CatchUp::Left;
        }
        let p = self.fp.pos;
        if p > target {
            return CatchUp::Beyond;
        }
        if !self.windowable() {
            return CatchUp::Behind;
        }
        if p == target {
            return CatchUp::Reached;
        }
        if self.opts.raw {
            return CatchUp::Behind;
        }
        let cur = self.cur.as_ref().expect("checked");
        if cur
            .slice(p, target)
            .iter()
            .all(|&c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.advance(target, true);
            CatchUp::Reached
        } else {
            CatchUp::Behind
        }
    }

    /// Bytes held for the current input's stream buffer (0 for inputs
    /// read whole).
    #[cfg(test)]
    pub(crate) fn buffer_capacity(&self) -> usize {
        match self.cur.as_ref().map(|c| &c.kind) {
            Some(DataKind::Stream { buf, .. }) => buf.len(),
            _ => 0,
        }
    }

    /// `input_filename` as text (for workers on other threads).
    pub(crate) fn filename_text(&self) -> Option<String> {
        self.filename.as_ref().map(|s| s.as_str().to_owned())
    }
}

/// A run of complete lines for the parallel engine (see
/// [`InputReader::cut`]).
pub(crate) struct Window {
    /// The bytes; absolute offset `p` is at `data[p - base]`, and at least
    /// `PAD` readable bytes follow `end` unless it is the end of a whole
    /// input.
    pub(crate) data: SharedBytes,
    pub(crate) base: usize,
    pub(crate) start: usize,
    /// Just after a newline.
    pub(crate) end: usize,
    /// Newlines before `start` in this input.
    pub(crate) nl: u64,
    /// Start of the line containing `start`.
    pub(crate) line_start: usize,
    /// Which input (see [`InputReader::catch_up`]).
    pub(crate) generation: u64,
    /// `-R`: lines are strings.
    pub(crate) raw: bool,
}

/// A position in an input where a job of whole lines starts, with its
/// line bookkeeping (see [`InputReader::cut`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cut {
    pub(crate) generation: u64,
    pub(crate) pos: usize,
    /// Newlines before `pos`.
    pub(crate) nl: u64,
    /// Start of the line containing `pos`.
    pub(crate) line_start: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatchUp {
    /// Idle at the target.
    Reached,
    /// Records remain before the target.
    Behind,
    /// Past the target (a text read on from before it ended after it).
    Beyond,
    /// The reader moved on to another input (or ended).
    Left,
}

/// The value jq reads for one line of a window, and where jq's parser
/// would emit it: `Ok(None)` for a blank line, `Err(())` when the line
/// isn't exactly one text the fast path would take (read it with
/// [`InputReader::next`] instead). `line` is `[a, b)` with `b` just after
/// its newline; `buf[p - base]` is absolute offset `p`.
pub(crate) fn window_line(
    simd: &mut SimdParser,
    buf: &[u8],
    base: usize,
    a: usize,
    b: usize,
    raw: bool,
) -> Result<Option<(Value, usize)>, ()> {
    let line = &buf[a - base..b - base];
    if raw {
        // jq converts each fgets chunk (4095 bytes) of a long line on its
        // own; the value is complete when the chunk with the newline is read.
        let text = &line[..line.len() - 1];
        let s = if line.len() <= CHUNK {
            Str::from_bytes(text)
        } else {
            let mut s = String::with_capacity(text.len());
            for piece in text.chunks(CHUNK) {
                unicode::push_lossy(&mut s, piece);
            }
            Str::from(s)
        };
        return Ok(Some((Value::String(s), b - 1)));
    }
    let mut s = 0;
    while s < line.len() && matches!(line[s], b' ' | b'\t' | b'\r' | b'\n') {
        s += 1;
    }
    if s == line.len() {
        return Ok(None);
    }
    let mut t = line.len();
    while matches!(line[t - 1], b' ' | b'\t' | b'\r' | b'\n') {
        t -= 1;
    }
    match line[s] {
        b'{' | b'[' | b'"' => match simd.parse(buf, a - base + s, a - base + t) {
            Ok(v) => Ok(Some((v, a + t - 1))),
            Err(_) => Err(()),
        },
        b']' | b'}' | b',' | b':' => Err(()),
        _ => {
            let q = s + line[s..]
                .iter()
                .position(|&c| LITERAL_STOP[c as usize])
                .unwrap_or(line.len() - s);
            if q != t {
                return Err(());
            }
            check_literal(&line[s..q])
                .map(|v| Some((v, a + q)))
                .ok_or(())
        }
    }
}

/// `input_line_number` for a value emitted at `e` on the line starting at
/// `line_start` and ending with the newline at `b - 1`: the newlines before
/// the line, plus one if the `fgets` chunk holding `e` also holds that
/// newline.
pub(crate) fn window_line_number(nl_before: u64, line_start: usize, b: usize, e: usize) -> u64 {
    nl_before + ((e - line_start) / CHUNK == (b - 1 - line_start) / CHUNK) as u64
}

fn default_message_sink(m: InputMessage) {
    use std::io::Write;
    let _ = io::stderr().write_all(&m.render("qj"));
}

enum ChunkAt {
    Chunk { end: usize, has_nl: bool },
    NeedData,
    Exhausted,
}

/// The `fgets` chunk starting at `p` (on the line starting at
/// `line_start`): up to and including a newline, or 4095 bytes counted from
/// the line start in steps, or up to EOF.
fn next_chunk(cur: &FileData, p: usize, line_start: usize) -> ChunkAt {
    if p >= cur.end {
        return if cur.eof {
            ChunkAt::Exhausted
        } else {
            ChunkAt::NeedData
        };
    }
    let limit = line_start + ((p - line_start) / CHUNK + 1) * CHUNK;
    let stop = limit.min(cur.end);
    match memchr::memchr(b'\n', cur.slice(p, stop)) {
        Some(i) => ChunkAt::Chunk {
            end: p + i + 1,
            has_nl: true,
        },
        None if stop == limit || cur.eof => ChunkAt::Chunk {
            end: stop,
            has_nl: false,
        },
        None => ChunkAt::NeedData,
    }
}

/// simdjson errors that mean "not exactly one text here" (so scanning for
/// the text's end may help), as opposed to content jq's parser must judge.
fn is_extent_error(code: i32) -> bool {
    matches!(
        code,
        tape_error::TAPE_ERROR
            | tape_error::INCOMPLETE_ARRAY_OR_OBJECT
            | tape_error::UNCLOSED_STRING
    )
}

/// Bytes that end a literal token in jq's parser (whitespace, quote,
/// structure), plus NUL, which the fast path never consumes.
static LITERAL_STOP: [bool; 256] = {
    let mut t = [false; 256];
    let stops = b" \t\r\n\"[]{}:,\0";
    let mut i = 0;
    while i < stops.len() {
        t[stops[i] as usize] = true;
        i += 1;
    }
    t
};

/// Port of `check_literal` (`jv_parse.c`) for a complete top-level token:
/// `None` where jq reports an error (its parser then produces the message).
fn check_literal(token: &[u8]) -> Option<Value> {
    match token[0] {
        b't' => (token == b"true").then_some(Value::Bool(true)),
        b'f' => (token == b"false").then_some(Value::Bool(false)),
        b'\'' => None,
        b'n' if token.len() > 1 && token[1] == b'u' => (token == b"null").then_some(Value::Null),
        _ => Number::from_c_literal(token).map(Value::Number),
    }
}

/// Finds the end of the text whose scan state is `st`: the index of the
/// byte closing it (the bracket bringing the depth back to zero, or the
/// closing quote of a top-level string). `None` if it doesn't end before
/// `avail` (`st` then records where to resume).
///
/// The result is only a candidate: simdjson validates the text.
fn scan_extent(cur: &FileData, st: &mut ExtentScan, avail: usize) -> Option<usize> {
    let buf = cur.buf();
    let base = cur.base;
    let mut i = st.pos;
    while i < avail {
        if st.in_string {
            match memchr::memchr2(b'"', b'\\', &buf[i - base..avail - base]) {
                None => {
                    i = avail;
                    break;
                }
                Some(k) => {
                    let j = i + k;
                    if buf[j - base] == b'\\' {
                        i = j + 2;
                        continue;
                    }
                    st.in_string = false;
                    i = j + 1;
                    if st.depth == 0 {
                        return Some(j);
                    }
                }
            }
        } else {
            match buf[i - base] {
                b'"' => st.in_string = true,
                b'[' | b'{' => st.depth += 1,
                b']' | b'}' => {
                    st.depth = st.depth.saturating_sub(1);
                    if st.depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    st.pos = i;
    None
}

/// An [`InputReader`] shared by the CLI's main loop and the VM, like jq's
/// one `jq_util_input_state`: `input`/`inputs` in a program pull from the
/// same stream as the main loop, and `input_filename`/`input_line_number`
/// see the same state. Implements the VM's
/// [`InputSource`](crate::jq::lang::execute::InputSource) (give
/// `Jq::set_input` a clone). Don't hold [`SharedReader::borrow_mut`] while
/// the program runs.
#[derive(Clone)]
pub struct SharedReader(pub std::rc::Rc<std::cell::RefCell<InputReader>>);

impl SharedReader {
    pub fn new(reader: InputReader) -> SharedReader {
        SharedReader(std::rc::Rc::new(std::cell::RefCell::new(reader)))
    }

    /// The reader, for the main loop (`next`, `failures`, `position`).
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, InputReader> {
        self.0.borrow_mut()
    }
}

impl crate::jq::lang::execute::InputSource for SharedReader {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        self.0.borrow_mut().next()
    }

    fn current_filename(&self) -> Option<Value> {
        let f = self.0.borrow().current_filename();
        (!f.is_null()).then_some(f)
    }

    fn current_line(&self) -> Value {
        Value::from(self.0.borrow().current_line() as f64)
    }
}

/// For callers that want the reader's message rendering with another
/// program name.
pub fn render_message(m: &InputMessage, prog: &str) -> Vec<u8> {
    m.render(prog)
}

/// Names for [`InputReader::new`] from command-line arguments: jq reads
/// standard input when no file is given.
pub fn input_names<I, S>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let v: Vec<OsString> = args.into_iter().map(|s| s.as_ref().to_owned()).collect();
    if v.is_empty() {
        vec![OsString::from("-")]
    } else {
        v
    }
}
