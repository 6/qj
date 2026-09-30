//! Port of jq 1.8.1's `util.c` input layer (`jq_util_input_*`) on the ported
//! JSON parser ([`Parser`]).
//!
//! jq reads every input with `fgets` into a 4096-byte buffer and hands each
//! chunk (one line, or 4095 bytes of a longer one) to a single parser shared
//! by all inputs. [`UtilInput`] reproduces that exactly, because it is
//! observable:
//!
//! * texts continue across file boundaries (`printf 1 > a; printf 2 > b;
//!   jq . a b` prints `12`), and the parser's line and column counters in
//!   error messages run on across files;
//! * a chunk without a newline is cut at its first NUL (jq takes its
//!   `strlen`), except with `-R`, where the whole chunk is used;
//! * after a parse error the parser drops the rest of the current chunk;
//! * `input_line_number` counts the chunks that ended with a newline since
//!   the current input was opened, and `input_filename` is its name
//!   (`<stdin>` for `-`);
//! * inputs are opened lazily, when the parser needs more bytes. A file that
//!   can't be opened prints `Could not open file ...` and counts as a
//!   failure (`input_filename` still becomes its name); a read error is
//!   reported only when the next input is opened;
//! * with `-R`, each chunk becomes a string on its own, so a character split
//!   by a 4095-byte boundary becomes two U+FFFD, and lines join across files.
//!
//! This is the CLI's input seam: `src/io`'s reader (the same interface, plus
//! a simdjson fast path) replaces it when it lands, by implementing
//! [`Reader`] and being returned from [`open_inputs`]. [`UtilInput`]'s
//! methods are named after that reader's.
//!
//! qj extension: a file whose name ends in `.gz`/`.gzip` or `.zst`/`.zstd`
//! and that starts with that format's magic bytes is decompressed as it is
//! read. Anything else is read as is, like jq.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::rc::Rc;

use crate::jq::value::{Array, Error, ParseFlags, Parser, Str, Value};

/// `fgets(buf, 4096, f)` reads at most this many bytes.
const FGETS_MAX: usize = 4095;

/// How much a [`Stream`] reads from its source at once.
const READ_SIZE: usize = 64 * 1024;

/// What util.c's `jq_util_input_set_parser` configures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputOptions {
    /// `-R`: each line is a string (jq's parser is `NULL`).
    pub raw: bool,
    /// `-s`: all inputs as one array (with `-R`, one string).
    pub slurp: bool,
    /// The parser's flags (`--seq`, `--stream`, `--stream-errors`).
    pub flags: ParseFlags,
    /// Build every value with jq's parser port, without the simdjson fast
    /// path. Its values are equal, but it shares object keys between records,
    /// and `--debug-trace` prints refcounts.
    pub parser_only: bool,
}

/// Messages util.c prints on stderr while reading.
#[derive(Debug)]
pub enum InputMessage {
    /// `fprinter`: `jq: error: Could not open file <name>: <strerror>`.
    OpenFailed { name: Vec<u8>, error: io::Error },
    /// `read_more`: `jq: error: <strerror>`, for the read error that ended the
    /// previous input.
    ReadFailed { error: io::Error },
}

impl InputMessage {
    /// The line jq prints, with `prog` in place of `jq`.
    pub fn render(&self, prog: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(prog.as_bytes());
        out.extend_from_slice(b": error: ");
        match self {
            InputMessage::OpenFailed { name, error } => {
                out.extend_from_slice(b"Could not open file ");
                out.extend_from_slice(name);
                out.extend_from_slice(b": ");
                out.extend_from_slice(&error_text(error));
            }
            InputMessage::ReadFailed { error } => out.extend_from_slice(&error_text(error)),
        }
        out.push(b'\n');
        out
    }
}

/// `strerror(errno)` for an OS error; the error's own text otherwise (a
/// decompression error has no errno).
fn error_text(e: &io::Error) -> Vec<u8> {
    match e.raw_os_error() {
        Some(errno) => super::args::strerror(errno),
        None => e.to_string().into_bytes(),
    }
}

/// A C stdio input stream as util.c uses it: `fgets` and the `feof` and
/// `ferror` flags.
pub(super) struct Stream {
    reader: Box<dyn Read>,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    /// `feof`: a read returned no bytes (sticky until `clearerr`, as in
    /// glibc and Apple's libc).
    eof: bool,
    /// `ferror`, with the error for the message.
    error: Option<io::Error>,
}

impl Stream {
    pub(super) fn new(reader: Box<dyn Read>) -> Stream {
        Stream {
            reader,
            buf: vec![0; READ_SIZE].into_boxed_slice(),
            pos: 0,
            len: 0,
            eof: false,
            error: None,
        }
    }

    /// `clearerr`.
    fn clearerr(&mut self) {
        self.eof = false;
        self.error = None;
    }

    /// Refills the buffer; false at end of file or on an error. Interrupted
    /// reads are retried (jq retries `fgets` on `EINTR`).
    fn fill(&mut self) -> bool {
        if self.eof || self.error.is_some() {
            return false;
        }
        loop {
            match self.reader.read(&mut self.buf) {
                Ok(0) => {
                    self.eof = true;
                    return false;
                }
                Ok(n) => {
                    self.pos = 0;
                    self.len = n;
                    return true;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.error = Some(e);
                    return false;
                }
            }
        }
    }

    /// `fgets(out, 4096, stream)`: the next line (with its newline), or the
    /// next 4095 bytes of a longer line, or the rest of the input. `false`
    /// is `NULL`: nothing left, or a read error (whatever was read is lost,
    /// as in C).
    pub(super) fn fgets(&mut self, out: &mut Vec<u8>) -> bool {
        out.clear();
        loop {
            if self.pos == self.len && !self.fill() {
                break;
            }
            let avail = &self.buf[self.pos..self.len];
            let take = avail.len().min(FGETS_MAX - out.len());
            if let Some(i) = memchr::memchr(b'\n', &avail[..take]) {
                out.extend_from_slice(&avail[..=i]);
                self.pos += i + 1;
                return true;
            }
            out.extend_from_slice(&avail[..take]);
            self.pos += take;
            if out.len() == FGETS_MAX {
                return true;
            }
        }
        if self.error.is_some() {
            out.clear();
            return false;
        }
        !out.is_empty()
    }
}

/// Standard input, read straight from fd 0 (the stream buffers).
pub(super) struct StdinReader {
    /// stdio makes a terminal line-buffered, and flushes line-buffered
    /// output before reading one.
    tty: bool,
}

impl StdinReader {
    pub(super) fn new() -> StdinReader {
        // SAFETY: isatty has no memory-safety preconditions.
        StdinReader {
            tty: unsafe { libc::isatty(0) } != 0,
        }
    }
}

impl Read for StdinReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.tty {
            super::run::flush_line_buffered_stdout();
        }
        // SAFETY: fd 0 stays open for the life of the process, and `buf` is
        // valid for writes of `buf.len()` bytes.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

/// Opens an input by name when util.c would: `-` (standard input, opened
/// once), or `fopen(name, "r")`.
pub type Opener = Box<dyn FnMut(&[u8]) -> io::Result<Box<dyn Read>>>;

/// The file system and standard input.
fn default_open(name: &[u8]) -> io::Result<Box<dyn Read>> {
    if name == b"-" {
        Ok(Box::new(StdinReader::new()))
    } else {
        open_file(name)
    }
}

/// `fopen(name, "r")`, plus qj's transparent decompression.
fn open_file(name: &[u8]) -> io::Result<Box<dyn Read>> {
    let path = OsStr::from_bytes(name);
    let mut file = std::fs::File::open(path)?;
    let lossy = String::from_utf8_lossy(name);
    if !crate::decompress::is_compressed(&lossy) {
        return Ok(Box::new(file));
    }
    // Only decompress what really is compressed; anything else is read as
    // jq reads it.
    let mut head = [0u8; 4];
    let mut n = 0;
    while n < head.len() {
        match file.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // Let the first read report it (a directory, for instance).
            Err(_) => break,
        }
    }
    let head = head[..n].to_vec();
    let is_gzip = lossy.ends_with(".gz") || lossy.ends_with(".gzip");
    let magic_ok = if is_gzip {
        head.starts_with(&[0x1f, 0x8b])
    } else {
        head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    };
    let whole = io::Cursor::new(head).chain(file);
    if !magic_ok {
        return Ok(Box::new(whole));
    }
    let buffered = io::BufReader::with_capacity(256 * 1024, whole);
    let (inner, format): (Box<dyn Read>, &str) = if is_gzip {
        // Concatenated members decompress as one stream, like gzip(1).
        (
            Box::new(flate2::read::MultiGzDecoder::new(buffered)),
            "gzip",
        )
    } else {
        (
            Box::new(zstd::stream::read::Decoder::with_buffer(buffered)?),
            "zstd",
        )
    };
    Ok(Box::new(Decompressing {
        inner,
        what: format!("{lossy}: {format} decompression failed"),
    }))
}

/// A decompressing reader whose errors name the file (they have no errno for
/// jq's `strerror` message).
struct Decompressing {
    inner: Box<dyn Read>,
    what: String,
}

impl Read for Decompressing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).map_err(|e| {
            if e.raw_os_error().is_some() || e.kind() == io::ErrorKind::Interrupted {
                e
            } else {
                io::Error::new(e.kind(), format!("{}: {e}", self.what))
            }
        })
    }
}

/// `state->current_input`.
enum Current {
    Stdin,
    File(Stream),
}

/// The input reader main.c's loop and the `input` builtin share (util.c's
/// `jq_util_input_*` API). This is the seam for replacing [`UtilInput`]:
/// implement it and return the new reader from [`open_inputs`].
pub trait Reader {
    /// `jq_util_input_next_input`: the next value, a parse error, or `None`
    /// at the end.
    fn next(&mut self) -> Option<Result<Value, Error>>;
    /// `jq_util_input_errors`.
    fn failures(&self) -> usize;
    /// `input_filename` (`null` before any input was opened).
    fn current_filename(&self) -> Value;
    /// `input_line_number`.
    fn current_line(&self) -> u64;
    /// `jq_util_input_get_position`, for error messages.
    fn position(&self) -> String;
    /// [`crate::io::InputReader::next_record`]: like [`Reader::next`], but
    /// texts the tape sink takes are processed already. Readers without one
    /// give values.
    fn next_record(&mut self) -> Option<Result<crate::io::reader::Record, Error>> {
        self.next().map(|r| r.map(crate::io::reader::Record::Value))
    }
    /// [`crate::io::InputReader::set_tape_sink`]; a no-op for readers that
    /// have none.
    fn set_tape_sink(&mut self, _sink: Box<dyn crate::io::reader::TapeSink>) {}
}

/// The reader for main.c's inputs (`-` is standard input): `src/io`'s
/// [`crate::io::InputReader`] (util.c exactly, with a simdjson fast path and
/// memory-mapped files), or with `QJ_INPUT=util` this module's plain port.
pub fn open_inputs(files: Vec<Vec<u8>>, opts: InputOptions) -> Rc<RefCell<dyn Reader>> {
    if std::env::var_os("QJ_INPUT").is_some_and(|v| v == "util") {
        return Rc::new(RefCell::new(UtilInput::new(files, opts)));
    }
    Rc::new(RefCell::new(open_reader(files, opts)))
}

/// `src/io`'s reader over main.c's inputs, opening them like [`UtilInput`]
/// does (see `CliOpener`).
pub fn open_reader(files: Vec<Vec<u8>>, opts: InputOptions) -> crate::io::InputReader {
    use std::os::unix::ffi::OsStringExt;
    let names = files
        .into_iter()
        .map(std::ffi::OsString::from_vec)
        .collect();
    let ropts = crate::io::ReaderOptions {
        raw: opts.raw,
        slurp: opts.slurp,
        seq: opts.flags.seq,
        stream: opts.flags.streaming,
        stream_errors: opts.flags.stream_errors,
    };
    let mut reader =
        crate::io::InputReader::with_opener(names, ropts, Box::new(CliOpener::default()));
    if opts.parser_only {
        reader.set_fast_path(false);
    }
    reader
}

/// Opens inputs for `src/io`'s reader the way [`UtilInput`] does: a terminal
/// on stdin flushes line-buffered stdout before each read (as stdio does);
/// `.gz`/`.zst` names are decompressed when they have the format's magic
/// bytes, with errors naming the file. Everything else goes through the
/// default opener, which memory-maps regular files (stdin too).
#[derive(Default)]
struct CliOpener {
    fs: crate::io::FsOpener,
}

impl crate::io::Opener for CliOpener {
    fn open(&mut self, name: &OsStr) -> io::Result<crate::io::Opened> {
        use crate::io::Opened;
        if name == "-" {
            // SAFETY: isatty has no memory-safety preconditions.
            if unsafe { libc::isatty(0) } != 0 {
                return Ok(Opened::Stream {
                    reader: Box::new(StdinReader::new()),
                    fd: Some(0),
                });
            }
            return crate::io::source::open_borrowed_fd(0);
        }
        let bytes = name.as_bytes();
        if crate::decompress::is_compressed(&String::from_utf8_lossy(bytes)) {
            return Ok(Opened::Stream {
                reader: open_file(bytes)?,
                fd: None,
            });
        }
        crate::io::Opener::open(&mut self.fs, name)
    }
}

impl Reader for crate::io::InputReader {
    fn next(&mut self) -> Option<Result<Value, Error>> {
        crate::io::InputReader::next(self)
    }
    fn failures(&self) -> usize {
        crate::io::InputReader::failures(self)
    }
    fn current_filename(&self) -> Value {
        crate::io::InputReader::current_filename(self)
    }
    fn current_line(&self) -> u64 {
        crate::io::InputReader::current_line(self)
    }
    fn position(&self) -> String {
        crate::io::InputReader::position(self)
    }
    fn next_record(&mut self) -> Option<Result<crate::io::reader::Record, Error>> {
        crate::io::InputReader::next_record(self)
    }
    fn set_tape_sink(&mut self, sink: Box<dyn crate::io::reader::TapeSink>) {
        crate::io::InputReader::set_tape_sink(self, Some(sink));
    }
}

impl Reader for UtilInput {
    fn next(&mut self) -> Option<Result<Value, Error>> {
        UtilInput::next(self)
    }
    fn failures(&self) -> usize {
        UtilInput::failures(self)
    }
    fn current_filename(&self) -> Value {
        UtilInput::current_filename(self)
    }
    fn current_line(&self) -> u64 {
        UtilInput::current_line(self)
    }
    fn position(&self) -> String {
        UtilInput::position(self)
    }
}

/// `struct jq_util_input_state`: jq's inputs and the parser they feed.
pub struct UtilInput {
    /// `NULL` for `-R`.
    parser: Option<Parser>,
    /// `-s`: the array (or `-R` string) being collected; `None` once returned
    /// (jq's `jv_invalid()`).
    slurped: Option<Value>,
    files: Vec<Vec<u8>>,
    curr_file: usize,
    current: Option<Current>,
    /// Standard input stays open (jq `clearerr`s it instead of closing it),
    /// so a second `-` continues where the first stopped.
    stdin: Option<Stream>,
    failures: usize,
    /// The chunk `fgets` read last (jq's `buf`), and how much of it counts
    /// (`buf_valid_len`).
    buf: Vec<u8>,
    buf_valid_len: usize,
    current_filename: Option<Str>,
    current_line: u64,
    open: Opener,
    on_message: Box<dyn FnMut(InputMessage)>,
}

impl UtilInput {
    /// `jq_util_input_init` + `jq_util_input_set_parser` + the
    /// `jq_util_input_add_input` calls: `files` in order (`-` is standard
    /// input; jq passes `["-"]` when there are no file arguments).
    pub fn new(files: Vec<Vec<u8>>, opts: InputOptions) -> UtilInput {
        UtilInput::with_opener(files, opts, Box::new(default_open))
    }

    /// Like [`UtilInput::new`], opening inputs with `open`.
    pub fn with_opener(files: Vec<Vec<u8>>, opts: InputOptions, open: Opener) -> UtilInput {
        let parser = (!opts.raw).then(|| Parser::new(opts.flags));
        let slurped = match (opts.slurp, opts.raw) {
            (true, true) => Some(Value::String(Str::new())),
            (true, false) => Some(Value::Array(Array::new())),
            (false, _) => None,
        };
        UtilInput {
            parser,
            slurped,
            files,
            curr_file: 0,
            current: None,
            stdin: None,
            failures: 0,
            buf: Vec::with_capacity(FGETS_MAX),
            buf_valid_len: 0,
            current_filename: None,
            current_line: 0,
            open,
            on_message: Box::new(|m| {
                use std::io::Write;
                let _ = io::stderr().write_all(&m.render(crate::compat::prog_name()));
            }),
        }
    }

    /// Where `Could not open file` and read-error messages go (stderr, with
    /// the `qj` prefix — `jq` in compat mode — by default).
    pub fn set_message_sink(&mut self, sink: Box<dyn FnMut(InputMessage)>) {
        self.on_message = sink;
    }

    /// `jq_util_input_errors`: inputs that failed to open or read.
    pub fn failures(&self) -> usize {
        self.failures
    }

    /// `input_filename`: the current input's name, or `null` before any
    /// input was opened (jq's invalid, which `input_filename` turns into
    /// `null`).
    pub fn current_filename(&self) -> Value {
        match &self.current_filename {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }

    /// `input_line_number`.
    pub fn current_line(&self) -> u64 {
        self.current_line
    }

    /// `jq_util_input_get_position`: `<file>:<line>` for error messages, or
    /// `<unknown>` before any input was opened.
    pub fn position(&self) -> String {
        match &self.current_filename {
            // `%s` stops at a NUL.
            Some(s) => format!("{}:{}", s.as_c_str(), self.current_line),
            None => "<unknown>".to_owned(),
        }
    }

    fn stream(&mut self) -> Option<&mut Stream> {
        match &mut self.current {
            None => None,
            Some(Current::Stdin) => self.stdin.as_mut(),
            Some(Current::File(s)) => Some(s),
        }
    }

    /// `next_file`.
    fn next_file(&mut self) -> Option<Vec<u8>> {
        let f = self.files.get(self.curr_file)?.clone();
        self.curr_file += 1;
        Some(f)
    }

    /// Port of `jq_util_input_read_more`: moves to the next input when the
    /// current one is finished, then reads one chunk into `buf`. Returns
    /// whether this was the last read (no more inputs and none open).
    fn read_more(&mut self) -> bool {
        let finished = match self.stream() {
            None => true,
            Some(s) => s.eof || s.error.is_some(),
        };
        if finished {
            if let Some(s) = self.stream()
                && let Some(error) = s.error.take()
            {
                // System-level input error on the stream; it is closed below.
                (self.on_message)(InputMessage::ReadFailed { error });
            }
            match self.current.take() {
                // Perhaps we can read again; anyways, jq doesn't fclose(stdin).
                Some(Current::Stdin) => {
                    if let Some(s) = &mut self.stdin {
                        s.clearerr();
                    }
                }
                Some(Current::File(_)) | None => {}
            }
            if let Some(f) = self.next_file() {
                self.current_line = 0;
                if f == b"-" {
                    self.current_filename = Some(Str::from("<stdin>"));
                    if self.stdin.is_none() {
                        // (Standard input is always open in jq.)
                        let r = (self.open)(b"-").unwrap_or_else(|_| Box::new(io::empty()));
                        self.stdin = Some(Stream::new(r));
                    }
                    self.current = Some(Current::Stdin);
                } else {
                    self.current_filename = Some(Str::from_bytes(&f));
                    match (self.open)(&f) {
                        Ok(r) => self.current = Some(Current::File(Stream::new(r))),
                        Err(error) => {
                            (self.on_message)(InputMessage::OpenFailed { name: f, error });
                            self.failures += 1;
                        }
                    }
                }
            }
        }

        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        self.buf_valid_len = 0;
        let raw = self.parser.is_none();
        let mut failed = false;
        let mut newline = false;
        if let Some(s) = self.stream() {
            if !s.fgets(&mut buf) {
                buf.clear();
                failed = s.error.is_some();
            } else {
                // fgets stops after a newline, so one can only be last.
                newline = buf.last() == Some(&b'\n');
            }
        }
        if failed {
            self.failures += 1;
        }
        if newline {
            self.current_line += 1;
        }
        self.buf_valid_len = if !newline && !raw {
            // There should be no NULs in JSON texts (but JSON text sequences
            // are another story): jq takes the chunk's strlen.
            memchr::memchr(0, &buf).unwrap_or(buf.len())
        } else {
            // A whole line, or (raw) everything fgets read: jq finds its end
            // by the terminator fgets wrote.
            buf.len()
        };
        self.buf = buf;
        self.curr_file == self.files.len() && self.current.is_none()
    }

    /// Port of `jq_util_input_next_input`: the next input value (a string
    /// with `-R`), `Some(Err)` for a parse error, `None` when everything has
    /// been read (jq's `jv_invalid()`). With `-s`, the one slurped value comes
    /// once at the end; parse errors are still returned as they occur.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Result<Value, Error>> {
        let mut value: Option<Str> = None; // raw: the line so far
        loop {
            let is_last;
            if self.parser.is_none() {
                is_last = self.read_more();
                if self.buf_valid_len > 0 {
                    let chunk = &self.buf[..self.buf_valid_len];
                    if let Some(Value::String(s)) = &mut self.slurped {
                        // Slurped raw input
                        s.push_bytes(chunk);
                    } else {
                        let v = value.get_or_insert_with(Str::new);
                        if chunk.last() == Some(&b'\n') {
                            // whole line
                            v.push_bytes(&chunk[..chunk.len() - 1]);
                            return value.map(|v| Ok(Value::String(v)));
                        }
                        v.push_bytes(chunk);
                        self.buf.clear();
                        self.buf_valid_len = 0;
                    }
                }
            } else {
                let remaining = self.parser.as_ref().map_or(0, Parser::remaining);
                is_last = if remaining == 0 {
                    let last = self.read_more();
                    let chunk = &self.buf[..self.buf_valid_len];
                    if let Some(p) = &mut self.parser {
                        p.set_buf(chunk, !last);
                    }
                    last
                } else {
                    false
                };
                let result = self.parser.as_mut().and_then(Parser::next);
                match (&mut self.slurped, result) {
                    (Some(Value::Array(a)), Some(Ok(v))) => a.push(v),
                    // Not slurped parsed input
                    (_, Some(r)) => return Some(r),
                    (_, None) => {}
                }
            }
            if is_last {
                break;
            }
        }
        if let Some(s) = self.slurped.take() {
            return Some(Ok(s));
        }
        value.map(|v| Ok(Value::String(v)))
    }
}

#[cfg(test)]
mod tests;
