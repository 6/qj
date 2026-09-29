//! Port of jq 1.8.1's `main.c` after option parsing: setup, compilation, the
//! `process()` loop over inputs, and exit codes, on the ported core
//! (`src/jq`). [`main`] is what the `qj` binary runs by default.
//!
//! In main.c's order:
//!
//! 1. The option loop ([`args::parse`], in the environment's locale).
//! 2. Output flags: color for a terminal unless `NO_COLOR`, then `-S`, `-a`,
//!    `-C`, `-M`; `JQ_COLORS` (a warning when invalid).
//! 3. The attributes the linker and `get_*` builtins read: the library
//!    search list (`-L`, or jq's default), `$ORIGIN` (the directory of
//!    `argv[0]`) and the program's origin.
//! 4. The program (or `.` when stdin or stdout isn't a terminal), read with
//!    `-f` like `jv_load_file`, and compiled with the named arguments,
//!    `$ARGS` and `$JQ_BUILD_CONFIGURATION` (`$ENV` comes from the
//!    environment). Compile errors exit 3.
//! 5. `--debug-dump-disasm`; then the inputs ([`super::input`], util.c's
//!    reader) and the `debug`/`stderr` callbacks.
//! 6. `process()` for `null` (`-n`) or each input, until a parse error
//!    (skipped with `--seq`), a halt, or an input that failed to open.
//! 7. Closing stdout, and the exit status: 0, the last input's result with
//!    `-e` (1 for `false`/`null`, 4 for no output), 2 for system errors
//!    (unreadable inputs, a failed write), 3 for compile errors, 5 for
//!    uncaught errors and parse errors, or `halt_error`'s code.
//!
//! Messages say `qj:` where jq's say `jq:`.
//!
//! Output goes through one buffer on this thread, which `--debug-trace` also
//! writes to (jq prints the trace on stdout, interleaved with the results),
//! and which is flushed when a builtin aborts on macOS, where jq's `abort()`
//! flushes stdio ([`crate::jq::platform::set_before_abort`]).

use std::cell::RefCell;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::rc::Rc;

use super::args::{self, Action, ArgError, ArgValue, Options, print_flags};
use super::input::{InputOptions, Reader};
use crate::jq::lang::execute::{InputSource, Jq};
use crate::jq::lang::linker::JqAttrs;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::print::dump_to_vec;
use crate::jq::value::{
    Array, Colors, DumpOptions, Error, Indent, Object, ParseFlags, Parser, Str, Value, dump_string,
    parse_sized, unicode,
};

/// The program name in messages.
pub(super) const PROG: &str = "qj";

// main.c's return codes.
const JQ_OK: i32 = 0;
/// exit 0 if --exit-status is not set
const JQ_OK_NULL_KIND: i32 = -1;
const JQ_ERROR_SYSTEM: i32 = 2;
const JQ_ERROR_COMPILE: i32 = 3;
/// exit 0 if --exit-status is not set
const JQ_OK_NO_OUTPUT: i32 = -4;
const JQ_ERROR_UNKNOWN: i32 = 5;

/// Runs qj on the new core with this process's arguments and exits.
pub fn main() -> ! {
    let argv = args::argv_bytes();
    let code = run(&argv);
    std::process::exit(code)
}

// ---------------------------------------------------------------------------
// stdout
// ---------------------------------------------------------------------------

/// jq's `stdout` FILE, buffered as stdio buffers it. That is observable
/// whenever stdout and stderr go to the same place (`>out 2>&1`), because
/// stderr isn't buffered: each error, `debug` or `stderr` message lands after
/// the stdout bytes written out so far.
///
/// The buffer has the size stdio gives it at the first write
/// ([`stdio_buffer_size`]: 4096 for a regular file on macOS, 16384 for a
/// pipe). stdio writes it out when a write doesn't fit, so output reaches the
/// file in whole buffers, the last one (even when exactly full) staying until
/// the next write; also after each output with `--unbuffered`, at each newline
/// when stdout is a terminal (line buffering), when a builtin aborts on macOS,
/// before a terminal is read, and when stdout is closed at the end. jq writes
/// JSON a character or token at a time, which [`Stdout::write`] models, and
/// a `-r` string with one `fwrite` ([`Stdout::fwrite`]).
pub(super) struct Stdout {
    buf: Vec<u8>,
    /// The buffer size; 0 until the first write, when stdio allocates it.
    size: usize,
    line_buffered: bool,
    /// `ferror(stdout)`: the last write error.
    error: Option<io::Error>,
}

thread_local! {
    static STDOUT: RefCell<Stdout> = const {
        RefCell::new(Stdout {
            buf: Vec::new(),
            size: 0,
            line_buffered: false,
            error: None,
        })
    };
}

/// The buffer size stdio picks for stdout (fd 1) when it's first written to.
/// macOS's `__swhatbuf`: `st_blksize` up to 64 KB (`MAXBUFSIZE`), or `BUFSIZ`
/// when fstat fails or reports none, and `__smakebuf` caps a terminal's at
/// 4096 (`TTYBUFSIZE`: a tty's `st_blksize` is 64-128 KB). glibc's
/// `_IO_file_doallocate`: `BUFSIZ`, or `st_blksize` when that is smaller.
fn stdio_buffer_size(tty: bool) -> usize {
    let bufsiz = libc::BUFSIZ as usize;
    // SAFETY: fstat writes a `stat` into `st`, which is valid for writes.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let blksize = if unsafe { libc::fstat(1, &mut st) } == 0 && st.st_blksize > 0 {
        Some(st.st_blksize as usize)
    } else {
        None
    };
    if cfg!(target_os = "linux") {
        blksize.filter(|&b| b < bufsiz).unwrap_or(bufsiz)
    } else {
        let size = blksize.map_or(bufsiz, |b| b.min(1 << 16));
        if tty { size.min(4096) } else { size }
    }
}

impl Stdout {
    fn size(&mut self) -> usize {
        if self.size == 0 {
            self.size = stdio_buffer_size(self.line_buffered).max(1);
        }
        self.size
    }

    /// Writes `bytes` to fd 1. A failed write loses the data and sets the
    /// error, as stdio does.
    fn write_fd(&mut self, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() {
            let chunk = &bytes[done..];
            // SAFETY: `chunk` is valid for reads of its length.
            let r = unsafe { libc::write(1, chunk.as_ptr().cast(), chunk.len()) };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                self.error = Some(e);
                break;
            }
            done += r as usize;
        }
    }

    /// Writes `self.buf[..n]` to fd 1 and drops it from the buffer.
    fn write_out(&mut self, n: usize) {
        let buf = std::mem::take(&mut self.buf);
        self.write_fd(&buf[..n]);
        self.buf = buf;
        self.buf.drain(..n);
    }

    fn flush(&mut self) {
        let n = self.buf.len();
        if n > 0 {
            self.write_out(n);
        }
    }

    /// After bytes were appended to the buffer as jq writes them, a character
    /// or token at a time: every write that didn't fit wrote out the full
    /// buffer, so all whole buffers but the last one are out; a terminal also
    /// gets every complete line.
    fn settle(&mut self) {
        if self.line_buffered
            && let Some(nl) = memchr::memrchr(b'\n', &self.buf)
        {
            self.write_out(nl + 1);
        }
        let size = self.size();
        if self.buf.len() > size {
            self.write_out((self.buf.len() - 1) / size * size);
        }
    }

    /// Output written a character or token at a time (`jv_dumpf`, `printf`).
    pub(super) fn write(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        self.settle();
    }

    /// Output appended by `f`, as by [`Stdout::write`] (without a copy).
    fn write_with(&mut self, f: impl FnOnce(&mut Vec<u8>)) {
        f(&mut self.buf);
        self.settle();
    }

    /// One `fwrite` of `data`, as stdio does it on a fully buffered stream
    /// (on a terminal, as [`Stdout::write`]). Large writes differ from the
    /// same bytes written piecemeal in when the last buffer goes out.
    fn fwrite(&mut self, data: &[u8]) {
        let size = self.size();
        // (Anything shorter than the buffer goes out as if piecemeal; so does
        // everything on a line-buffered glibc stream, approximately.)
        if data.len() < size || (self.line_buffered && cfg!(target_os = "linux")) {
            return self.write(data);
        }
        if self.line_buffered {
            // FreeBSD/macOS __sfvwrite, line buffered: the same, a line at a
            // time, and a buffer holding a newline is written out.
            let mut p = data;
            let mut nldist = None;
            while !p.is_empty() {
                let nd = *nldist
                    .get_or_insert_with(|| memchr::memchr(b'\n', p).map_or(p.len() + 1, |i| i + 1));
                let s = p.len().min(nd);
                let space = size - self.buf.len();
                let w = if !self.buf.is_empty() && s > space {
                    self.buf.extend_from_slice(&p[..space]);
                    self.flush();
                    space
                } else if s >= size {
                    self.write_fd(&p[..size]);
                    size
                } else {
                    self.buf.extend_from_slice(&p[..s]);
                    s
                };
                if nd == w {
                    self.flush();
                    nldist = None;
                } else {
                    nldist = Some(nd - w);
                }
                p = &p[w..];
            }
            return;
        }
        if cfg!(target_os = "linux") {
            // glibc _IO_new_file_xsputn: fill the buffer; if more is left,
            // write the full buffer, then whole blocks directly, and buffer
            // the rest.
            let fill = (size - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..fill]);
            let rest = &data[fill..];
            if !rest.is_empty() {
                self.flush();
                let direct = if size >= 128 {
                    rest.len() - rest.len() % size
                } else {
                    rest.len()
                };
                self.write_fd(&rest[..direct]);
                self.buf.extend_from_slice(&rest[direct..]);
            }
        } else {
            // FreeBSD/macOS __sfvwrite: fill a partly full buffer and write it
            // out when the data doesn't fit; from an empty buffer, write whole
            // buffers directly; buffer what fits.
            let mut p = data;
            while !p.is_empty() {
                let space = size - self.buf.len();
                if !self.buf.is_empty() && p.len() > space {
                    self.buf.extend_from_slice(&p[..space]);
                    p = &p[space..];
                    self.flush();
                } else if p.len() >= size {
                    let direct = p.len() / size * size;
                    self.write_fd(&p[..direct]);
                    p = &p[direct..];
                } else {
                    self.buf.extend_from_slice(p);
                    break;
                }
            }
        }
    }

    /// After an output: `fflush(stdout)` with `--unbuffered`.
    fn after_output(&mut self, unbuffered: bool) {
        if unbuffered {
            self.flush();
        }
    }
}

pub(super) fn with_stdout<R>(f: impl FnOnce(&mut Stdout) -> R) -> R {
    STDOUT.with(|s| f(&mut s.borrow_mut()))
}

/// Flushes the stdout buffer before a builtin aborts (see the module docs).
fn flush_before_abort() {
    STDOUT.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.flush();
        }
    });
}

/// What stdio does before it reads a line-buffered input (a terminal on
/// stdin): flush the line-buffered output streams, which stdout is when it's
/// a terminal. So `jq -j .` shows each result as soon as it's typed.
pub(super) fn flush_line_buffered_stdout() {
    STDOUT.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut()
            && s.line_buffered
        {
            s.flush();
        }
    });
}

/// The `--debug-trace` writer: into the stdout buffer.
pub(super) struct TraceOut;

impl Write for TraceOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        with_stdout(|s| s.write(buf));
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// main.c's `out:` label for stdout: `fclose(stdout)` (flush, then close)
/// and the "writing output failed" message, which makes the status 2.
fn close_stdout(ret: i32) -> i32 {
    let error = with_stdout(|s| {
        s.flush();
        let pending = s.error.take();
        // SAFETY: closing fd 1; nothing writes to it afterwards.
        let closed = unsafe { libc::close(1) } == 0;
        match (pending, closed) {
            (Some(e), _) => Some(e),
            (None, false) => Some(io::Error::last_os_error()),
            (None, true) => None,
        }
    });
    match error {
        None => ret,
        Some(e) => {
            let reason = args::strerror(e.raw_os_error().unwrap_or(0));
            let mut msg = format!("{PROG}: error: writing output failed: ").into_bytes();
            msg.extend_from_slice(&reason);
            msg.push(b'\n');
            let _ = io::stderr().write_all(&msg);
            JQ_ERROR_SYSTEM
        }
    }
}

pub(super) fn write_stderr(bytes: &[u8]) {
    let _ = io::stderr().write_all(bytes);
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// The value layer's side of the option loop.
struct PortArgs;

impl args::ArgHost for PortArgs {
    type Value = Value;

    /// `jv_parse(argv[i])`.
    fn parse_json(&mut self, text: &[u8]) -> Result<Value, String> {
        parse_sized(text).map_err(|e| e.to_string())
    }

    fn slurp_json(&mut self, data: &[u8]) -> Result<Value, String> {
        load_file_data(data, false).map_err(|e| e.to_string())
    }

    fn raw_file(&mut self, data: Vec<u8>) -> ArgValue<Value> {
        match load_file_data(&data, true) {
            Ok(v) => ArgValue::Json(v),
            Err(_) => ArgValue::Text(data),
        }
    }
}

/// The parsing half of jv_file.c `jv_load_file(file, raw)` over the file's
/// bytes: jq `fread`s 4096 bytes at a time (plus whatever completes a UTF-8
/// sequence cut at the end), and each chunk is partial until a read comes up
/// short. So a file whose size is a multiple of 4096 never gets a final
/// buffer, and a trailing top-level number without whitespace after it is
/// dropped, as in jq. With `raw`, each chunk is repaired as UTF-8 on its own.
pub fn load_file_data(data: &[u8], raw: bool) -> Result<Value, Error> {
    const CHUNK: usize = 4096;
    let mut s = Str::new();
    let mut a = Array::new();
    let mut parser = (!raw).then(|| Parser::new(ParseFlags::default()));
    let mut pos = 0;
    let mut eof = false;
    while !eof {
        let mut n = CHUNK.min(data.len() - pos);
        eof = n < CHUNK;
        if n == 0 {
            continue;
        }
        let mut missing = 0usize;
        if !eof
            && unicode::utf8_backtrack(&data[pos..pos + n], n - 1, 0, Some(&mut missing)).is_some()
            && missing > 0
        {
            let m = missing.min(data.len() - pos - n);
            eof = m < missing;
            n += m;
        }
        let chunk = &data[pos..pos + n];
        pos += n;
        match &mut parser {
            None => s.push_bytes(chunk),
            Some(p) => {
                p.set_buf(chunk, !eof);
                loop {
                    match p.next() {
                        Some(Ok(v)) => a.push(v),
                        Some(Err(e)) => return Err(e),
                        None => break,
                    }
                }
            }
        }
    }
    Ok(if raw {
        Value::String(s)
    } else {
        Value::Array(a)
    })
}

/// `ARGS` and `program_arguments` as main.c builds them for
/// `jq_compile_args`, with main.c's sharing, which `--debug-trace` shows in
/// refcounts. Each named value is a single value, held by `ARGS.named` (the
/// object `--arg` and friends built) and by the compile's arguments (main.c's
/// `jv_object_set(program_arguments, "ARGS", ...)` unshares that object into a
/// copy, then adds `ARGS` and `JQ_BUILD_CONFIGURATION`). The values move out of
/// `opts`, which would otherwise hold another reference. Returns the arguments
/// and `ARGS` itself, which main.c keeps in a variable until it exits.
fn program_arguments(opts: &mut Options<Value>) -> (Object, Value) {
    fn take(v: &mut ArgValue<Value>) -> Value {
        match v {
            ArgValue::Text(bytes) => Value::string_from_bytes(bytes),
            ArgValue::Json(v) => std::mem::replace(v, Value::Null),
        }
    }
    let mut named = Object::new();
    for (name, value) in &mut opts.named {
        named.insert(Str::from_bytes(name), take(value));
    }
    let positional: Vec<Value> = opts.positional.iter_mut().map(take).collect();
    let mut a = Object::new();
    a.insert(Str::from("positional"), Value::from(positional));
    a.insert(Str::from("named"), Value::Object(named.clone()));
    let args = Value::Object(a);
    // `named` is shared with ARGS.named: inserting copies it (a named `ARGS`
    // keeps its position and gets the real one).
    let mut vars = named;
    vars.insert(Str::from("ARGS"), args.clone());
    if !vars.contains_key("JQ_BUILD_CONFIGURATION") {
        vars.insert(
            Str::from("JQ_BUILD_CONFIGURATION"),
            Value::from(super::usage::BUILD_CONFIGURATION),
        );
    }
    (vars, args)
}

/// `-f`: `jv_load_file(program, 1)`, which `jq_compile_args` reads as a C
/// string (up to its first NUL).
fn load_program_text(path: &[u8]) -> Result<Vec<u8>, ArgError> {
    let data = args::load_file(path).map_err(ArgError::ProgramFile)?;
    let text = match load_file_data(&data, true) {
        Ok(Value::String(s)) => s.as_bytes().to_vec(),
        _ => data,
    };
    Ok(match memchr::memchr(0, &text) {
        Some(nul) => text[..nul].to_vec(),
        None => text,
    })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// The printer options for jq's `dumpopts` bits.
fn dump_options(flags: u32, colors: &Colors) -> DumpOptions {
    use print_flags::{ASCII, COLOR, PRETTY, SORTED, TAB};
    DumpOptions {
        indent: if flags & PRETTY == 0 {
            Indent::Compact
        } else if flags & TAB != 0 {
            Indent::Tab
        } else {
            Indent::Spaces(print_flags::indent_width(flags) as u8)
        },
        sort_keys: flags & SORTED != 0,
        ascii: flags & ASCII != 0,
        colors: (flags & COLOR != 0).then(|| colors.clone()),
    }
}

/// What `process()` needs from main.c's options.
#[derive(Clone)]
struct Process {
    dump: DumpOptions,
    raw_output: bool,
    raw_output0: bool,
    raw_no_lf: bool,
    ascii_output: bool,
    seq: bool,
    unbuffered: bool,
    jq_flags: u32,
}

/// The input state, shared by the main loop and the `input` builtin.
type SharedInput = Rc<RefCell<dyn Reader>>;

/// `jq_util_input_next_input_cb` and the `jq_util_input_get_current_*`
/// functions.
struct InputCb(SharedInput);

impl InputSource for InputCb {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        self.0.borrow_mut().next()
    }
    fn current_filename(&self) -> Option<Value> {
        match self.0.borrow().current_filename() {
            Value::Null => None,
            v => Some(v),
        }
    }
    fn current_line(&self) -> Value {
        Value::number(self.0.borrow().current_line() as f64)
    }
}

/// C's `(int)` conversion of a double, as `ret = jv_number_value(exit_code)`
/// does it: truncation, and what the hardware gives out of range (arm64
/// saturates and maps NaN to 0, like Rust; x86-64 gives `INT_MIN`).
fn c_double_to_int(d: f64) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        if d.is_nan() || d >= 2147483648.0 || d <= -2147483649.0 {
            return i32::MIN;
        }
    }
    d as i32
}

/// Replaces a message's leading `jq:` with qj's name.
pub(super) fn with_prog_name(message: &str) -> String {
    match message.strip_prefix("jq:") {
        Some(rest) => format!("{PROG}:{rest}"),
        None => message.to_owned(),
    }
}

/// execute.c `default_err_cb` (main.c sets no error callback): the message as
/// `jq_format_error` formats it, and a newline, on stderr. It reports errors
/// found while running, such as a module's syntax errors for `modulemeta`.
pub(super) fn default_err_cb() -> crate::jq::lang::execute::MsgCallback {
    Box::new(|msg: &Value| {
        let text = crate::jq::lang::execute::format_error(Ok(msg.clone()));
        if let Some(s) = text.as_str() {
            write_stderr(format!("{}\n", c_str(&with_prog_name(s))).as_bytes());
        }
    })
}

/// The dump of `v` with flags 0 (`jv_dump_string(v, 0)`).
fn dump_plain(v: &Value) -> String {
    dump_string(v, &DumpOptions::default())
}

/// C `%s` of a string: up to the first NUL.
fn c_str(s: &str) -> &str {
    match memchr::memchr(0, s.as_bytes()) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// Where `process()` writes results: stdout, or a record's buffer in the
/// parallel engine (which [`MainLoop`] writes to stdout in order).
trait ResultOut {
    /// Bytes jq writes a character or token at a time (`jv_dumpf`, `"\n"`).
    fn put(&mut self, f: impl FnOnce(&mut Vec<u8>));
    /// A `-r` string, which jq writes with a single `fwrite`.
    fn put_raw(&mut self, bytes: &[u8]);
}

impl ResultOut for Stdout {
    fn put(&mut self, f: impl FnOnce(&mut Vec<u8>)) {
        self.write_with(f);
    }
    fn put_raw(&mut self, bytes: &[u8]) {
        self.fwrite(bytes);
    }
}

impl ResultOut for RecordOut<'_> {
    fn put(&mut self, f: impl FnOnce(&mut Vec<u8>)) {
        f(self.out);
    }
    fn put_raw(&mut self, bytes: &[u8]) {
        // Only writes at least a buffer long go out differently from
        // piecemeal ones (see Stdout::fwrite).
        if bytes.len() >= RAW_MARK_MIN {
            self.marks.push((self.out.len(), bytes.len()));
        }
        self.out.extend_from_slice(bytes);
    }
}

/// A record's stdout bytes in the parallel engine, with where its large `-r`
/// strings are (offset, length), for [`MainLoop`] to write them out as
/// [`Stdout::fwrite`] does.
struct RecordOut<'a> {
    out: &'a mut Vec<u8>,
    marks: &'a mut Vec<(usize, usize)>,
}

/// Raw strings shorter than any stdio buffer (BUFSIZ is 1024 on macOS, and
/// `st_blksize` is at least 512 in practice) need no mark.
const RAW_MARK_MIN: usize = 512;

/// One result as main.c's `process()` prints it, with the status it sets.
/// `None` for a string containing NUL with `--raw-output0` (nothing is
/// written; `process()` raises an error instead).
fn write_result(result: &Value, p: &Process, out: &mut impl ResultOut) -> Option<i32> {
    let end = |b: &mut Vec<u8>| {
        if !p.raw_no_lf {
            b.push(b'\n');
        }
        if p.raw_output0 {
            b.push(0);
        }
    };
    let ret = match result {
        Value::String(s) if p.raw_output => {
            if p.ascii_output {
                let ascii = DumpOptions {
                    ascii: true,
                    ..DumpOptions::default()
                };
                out.put(|b| {
                    dump_to_vec(result, &ascii, b);
                    end(b);
                });
            } else if p.raw_output0 && memchr::memchr(0, s.as_bytes()).is_some() {
                return None;
            } else {
                out.put_raw(s.as_bytes());
                if !p.raw_no_lf || p.raw_output0 {
                    out.put(end);
                }
            }
            JQ_OK
        }
        _ => {
            out.put(|b| {
                if p.seq {
                    b.push(0x1e);
                }
                dump_to_vec(result, &p.dump, b);
                end(b);
            });
            match result {
                Value::Null | Value::Bool(false) => JQ_OK_NULL_KIND,
                _ => JQ_OK,
            }
        }
    };
    Some(ret)
}

/// The error `process()` raises for a string with a NUL under `--raw-output0`.
const RAW_OUTPUT0_NUL: &str = "Cannot dump a string containing NUL with --raw-output0 option";

/// main.c's message for an uncaught error, at input position `pos`.
fn uncaught_error_line(msg: &Value, pos: &str) -> String {
    match msg {
        Value::String(s) => format!("{PROG}: error (at {pos}): {}\n", c_str(s.as_str())),
        _ => format!(
            "{PROG}: error (at {pos}) (not a string): {}\n",
            dump_plain(msg)
        ),
    }
}

/// Port of main.c `process()`: runs the program on one input, printing its
/// results, and returns jq's status for it.
fn process(jq: &mut Jq, value: Value, p: &Process, input: &SharedInput) -> i32 {
    let mut ret = JQ_OK_NO_OUTPUT; // No valid results && -e -> exit(4)
    jq.start(value, p.jq_flags);
    let mut error: Option<Value> = None;
    for result in jq.by_ref() {
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                error = Some(e.into_value());
                break;
            }
        };
        let written = with_stdout(|out| {
            let r = write_result(&result, p, out);
            if r.is_some() {
                out.after_output(p.unbuffered);
            }
            r
        });
        match written {
            Some(r) => ret = r,
            None => {
                error = Some(Value::from(RAW_OUTPUT0_NUL));
                break;
            }
        }
    }
    if jq.halted() {
        // jq program invoked `halt` or `halt_error`
        ret = match jq.exit_code() {
            None => JQ_OK,
            Some(Value::Number(n)) => c_double_to_int(n.value()),
            Some(_) => JQ_ERROR_UNKNOWN,
        };
        match jq.error_message() {
            // No prefix should be added to the output of `halt_error`.
            Some(Value::String(s)) => write_stderr(s.as_bytes()),
            // Halt with no output
            Some(Value::Null) | None => {}
            Some(v) => write_stderr(format!("{}\n", dump_plain(v)).as_bytes()),
        }
    } else if let Some(msg) = error {
        // Uncaught jq exception
        let pos = input.borrow().position();
        write_stderr(uncaught_error_line(&msg, &pos).as_bytes());
        ret = JQ_ERROR_UNKNOWN;
    }
    ret
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn isatty(fd: i32) -> bool {
    // SAFETY: isatty has no memory-safety preconditions.
    unsafe { libc::isatty(fd) != 0 }
}

/// Port of main.c's `main()` from the option loop on; returns the exit
/// status.
pub fn run(argv: &[Vec<u8>]) -> i32 {
    crate::jq::platform::set_before_abort(flush_before_abort);
    let stdout_is_tty = isatty(1);
    with_stdout(|s| s.line_buffered = stdout_is_tty);

    let mut opts = match args::with_environment_locale(|| args::parse(argv, &mut PortArgs)) {
        Ok(Action::Run(opts)) => opts,
        Ok(Action::Help) => {
            // usage(0, 0): qj's own text.
            with_stdout(|s| s.buf.extend_from_slice(super::usage::help().as_bytes()));
            with_stdout(Stdout::flush);
            return 0;
        }
        Ok(Action::Version) => {
            with_stdout(|s| s.buf.extend_from_slice(super::usage::version().as_bytes()));
            return close_stdout(JQ_OK);
        }
        Ok(Action::BuildConfiguration) => {
            let text = format!("{}\n", super::usage::BUILD_CONFIGURATION);
            with_stdout(|s| s.buf.extend_from_slice(text.as_bytes()));
            return close_stdout(JQ_OK);
        }
        Ok(Action::RunTests { options, args }) => {
            let verbose = options.dump_disasm || options.jq_flags & args::debug_flags::TRACE != 0;
            let lib_dirs = options.lib_search_paths.as_deref();
            return match super::run_tests::jq_testsuite(lib_dirs, verbose, &args) {
                // exit() flushes stdout.
                super::run_tests::Outcome::Exit(code) => {
                    with_stdout(Stdout::flush);
                    code
                }
                super::run_tests::Outcome::Return(ret) => {
                    exit_status(&options, (close_stdout(ret), -1))
                }
            };
        }
        Err(e @ (ArgError::BadFile { .. } | ArgError::ProgramFile(_))) => {
            // ret = JQ_ERROR_SYSTEM; goto out;
            write_stderr(&e.render(PROG));
            return close_stdout(JQ_ERROR_SYSTEM);
        }
        Err(e) => {
            // die() or usage(2, 1): exit(2) right away.
            write_stderr(&e.render(PROG));
            return e.exit_code();
        }
    };
    let ret = run_program(&mut opts, stdout_is_tty);
    exit_status(&opts, ret)
}

/// main.c's exit: with `-e`, the last output's kind decides; otherwise
/// negative statuses are 0.
fn exit_status(opts: &Options<Value>, (ret, last_result): (i32, i32)) -> i32 {
    if opts.exit_status {
        if ret != JQ_OK_NO_OUTPUT {
            ret.wrapping_abs()
        } else {
            match last_result {
                -1 => JQ_OK_NO_OUTPUT.wrapping_abs(),
                0 => JQ_OK_NULL_KIND.wrapping_abs(),
                _ => JQ_OK,
            }
        }
    } else if ret > 0 {
        ret
    } else {
        0
    }
}

/// Everything from the output flags to closing stdout. Returns `ret` and
/// `last_result` for [`exit_status`].
fn run_program(opts: &mut Options<Value>, stdout_is_tty: bool) -> (i32, i32) {
    let no_color = std::env::var_os("NO_COLOR");
    let dumpopts = opts.dumpopts(stdout_is_tty, no_color.as_deref().map(OsStrExt::as_bytes));
    let colors = match std::env::var_os("JQ_COLORS") {
        None => Colors::default(),
        Some(spec) => {
            Colors::parse(&String::from_utf8_lossy(spec.as_bytes())).unwrap_or_else(|| {
                write_stderr(args::JQ_COLORS_WARNING.as_bytes());
                Colors::default()
            })
        }
    };

    let mut attrs = JqAttrs::new(".");
    attrs.lib_dirs = Value::from(
        opts.library_paths()
            .iter()
            .map(|p| Value::string_from_bytes(p))
            .collect::<Vec<_>>(),
    );
    attrs.jq_origin = Value::string_from_bytes(&opts.jq_origin());
    attrs.prog_origin = Value::string_from_bytes(&opts.program_origin());

    let Some(program) = opts.program_or_default(isatty(0), stdout_is_tty) else {
        // usage(2, 1)
        write_stderr(&ArgError::NoProgram.render(PROG));
        std::process::exit(2);
    };
    let program = if opts.from_file {
        match load_program_text(program) {
            Ok(text) => text,
            Err(e) => {
                write_stderr(&e.render(PROG));
                return (close_stdout(JQ_ERROR_SYSTEM), -1);
            }
        }
    } else {
        program.to_vec()
    };

    // main.c's `ARGS` variable lives until the end of the run.
    let (args, _main_args) = program_arguments(opts);
    let mut copts = CompileOptions {
        args,
        env: None,
        attrs,
    };
    let bc = match jq_compile_args(&program, &copts) {
        Ok(bc) => bc,
        Err(e) => {
            let mut text = Vec::new();
            for m in &e.messages {
                text.extend_from_slice(with_prog_name(m).as_bytes());
                text.push(b'\n');
            }
            write_stderr(&text);
            return (close_stdout(JQ_ERROR_COMPILE), -1);
        }
    };
    let parallel = parallel_plan(opts, &program, &bc, &copts);
    let mut jq = Jq::new(bc);
    jq.set_jq_attrs(&copts.attrs);
    // main.c hands these values to jq_set_attr and keeps no other reference
    // (get_prog_origin's refcount shows in --debug-trace).
    copts.attrs.lib_dirs = Value::Null;
    copts.attrs.jq_origin = Value::Null;
    copts.attrs.prog_origin = Value::Null;
    jq.set_attr("VERSION_DIR", Value::from("1.8.1"));
    jq.set_trace_writer(Some(Box::new(TraceOut)));

    if opts.dump_disasm {
        let text = format!("{}\n", jq.dump_disassembly(0));
        with_stdout(|s| s.write(text.as_bytes()));
    }

    let files = if opts.files.is_empty() {
        vec![b"-".to_vec()]
    } else {
        args::expand_file_globs(&opts.files)
    };
    let input_opts = InputOptions {
        raw: opts.raw_input,
        slurp: opts.slurp,
        flags: ParseFlags::from_bits(opts.parser_flags()),
        // --debug-trace shows refcounts, and the simdjson path shares keys.
        parser_only: opts.jq_flags & args::debug_flags::TRACE != 0,
    };
    let p = Process {
        dump: dump_options(dumpopts, &colors),
        raw_output: opts.raw_output,
        raw_output0: opts.raw_output0,
        raw_no_lf: opts.raw_no_lf,
        ascii_output: opts.ascii_output,
        seq: opts.seq,
        unbuffered: opts.unbuffered_output,
        jq_flags: opts.jq_flags,
    };
    if let Some(plan) = parallel {
        return run_parallel(plan, files, input_opts, p, jq);
    }
    let input: SharedInput = super::input::open_inputs(files, input_opts);
    jq.set_input(Some(Box::new(InputCb(input.clone()))));

    // debug_cb: ["DEBUG:",v] with the output flags minus pretty-printing.
    let debug_opts = dump_options(dumpopts & !print_flags::PRETTY, &colors);
    jq.set_debug_cb(Some(Box::new(move |v: &Value| {
        let msg = Value::from(vec![Value::from("DEBUG:"), v.clone()]);
        let mut b = Vec::new();
        dump_to_vec(&msg, &debug_opts, &mut b);
        b.push(b'\n');
        write_stderr(&b);
    })));
    // stderr_cb: strings raw, anything else as compact JSON.
    jq.set_stderr_cb(Some(Box::new(|v: &Value| match v {
        Value::String(s) => write_stderr(s.as_bytes()),
        _ => write_stderr(c_str(&dump_plain(v)).as_bytes()),
    })));
    jq.set_error_cb(Some(default_err_cb()));

    let mut ret = JQ_OK_NO_OUTPUT;
    let mut last_result = -1; // -1 = no result, 0=null or false, 1=true
    if opts.null_input {
        ret = process(&mut jq, Value::Null, &p, &input);
    } else {
        loop {
            if input.borrow().failures() != 0 {
                break;
            }
            let next = input.borrow_mut().next();
            match next {
                None => break,
                Some(Ok(value)) => {
                    ret = process(&mut jq, value, &p, &input);
                    if ret <= 0 && ret != JQ_OK_NO_OUTPUT {
                        last_result = i32::from(ret != JQ_OK_NULL_KIND);
                    }
                    if jq.halted() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    // Parse error
                    let msg = e.to_string();
                    let msg = c_str(&msg);
                    if !opts.seq {
                        ret = JQ_ERROR_UNKNOWN;
                        write_stderr(format!("{PROG}: parse error: {msg}\n").as_bytes());
                        break;
                    }
                    // --seq -> errors are not fatal
                    write_stderr(format!("{PROG}: ignoring parse error: {msg}\n").as_bytes());
                }
            }
        }
    }
    if input.borrow().failures() != 0 {
        ret = JQ_ERROR_SYSTEM;
    }
    (close_stdout(ret), last_result)
}

// ---------------------------------------------------------------------------
// Parallel processing (src/io's record engine)
// ---------------------------------------------------------------------------

/// C builtins that make records depend on each other, on the order work is
/// done in, or on a single process-wide state: programs using them run
/// sequentially. `input` (and `inputs`, defined with it) reads records
/// itself; `halt`/`halt_error` stop the run; `debug`/`stderr` interleave
/// with other stderr output as they run; `%Z` in local times depends on the
/// order of earlier libc time calls (`localtime`, `strflocaltime`,
/// `mktime`); `strptime` and `_strindices` can abort the process like jq's
/// `assert()`, which on macOS flushes the output produced so far;
/// `modulemeta` reports module errors through the error callback; `now` would
/// come out of order across records, where jq's values never decrease.
/// (`input_filename` and `input_line_number` are fine: workers answer them
/// from each record's position.)
const SEQUENTIAL_BUILTINS: &[&str] = &[
    "input",
    "now",
    "halt",
    "halt_error",
    "debug",
    "stderr",
    "localtime",
    "strflocaltime",
    "mktime",
    "strptime",
    "_strindices",
    "modulemeta",
];

/// What each worker thread needs to compile its own copy of the program
/// (values are `Rc`, so they're rebuilt per thread: the named arguments as
/// JSON text that round-trips exactly).
struct ParallelPlan {
    threads: usize,
    program: Vec<u8>,
    args_json: String,
    lib_dirs: Vec<Vec<u8>>,
    jq_origin: Vec<u8>,
    prog_origin: Vec<u8>,
}

/// Whether the inputs can go through the parallel record engine: records
/// must be independent (see [`SEQUENTIAL_BUILTINS`]), which also rules out
/// `-n`, `-s`, `-R`, `--seq`, `--stream`, `--debug-trace`, user labels
/// (their `{"__jq": n}` values count up across inputs), `$__loc__`, and
/// modules. `--threads` 1 (or 0) also runs sequentially.
fn parallel_plan(
    opts: &Options<Value>,
    program: &[u8],
    bc: &crate::jq::lang::bytecode::Bytecode,
    copts: &CompileOptions,
) -> Option<ParallelPlan> {
    if opts.null_input
        || opts.slurp
        || opts.raw_input
        || opts.seq
        || opts.parser_flags() != 0
        || opts.jq_flags & args::debug_flags::TRACE != 0
    {
        return None;
    }
    let threads = opts
        .threads
        .unwrap_or_else(crate::io::parallel::default_threads);
    if threads <= 1 {
        return None;
    }
    if bc
        .globals
        .cfunctions
        .iter()
        .any(|c| SEQUENTIAL_BUILTINS.contains(&c.name))
    {
        return None;
    }
    let text = |needle: &[u8]| memchr::memmem::find(program, needle).is_some();
    if text(b"$__loc__") || text(b"label") || text(b"break") || text(b"import") || text(b"include")
    {
        return None;
    }
    let args = Value::Object(copts.args.clone());
    let args_json = dump_plain(&args);
    match parse_sized(args_json.as_bytes()) {
        Ok(back) if crate::io::fuzzing::same(&back, &args) => {}
        _ => return None,
    }
    Some(ParallelPlan {
        threads,
        program: program.to_vec(),
        args_json,
        lib_dirs: opts.library_paths().iter().map(|p| p.to_vec()).collect(),
        jq_origin: opts.jq_origin().to_vec(),
        prog_origin: opts.program_origin().to_vec(),
    })
}

/// The worker's `InputSource`: `input_filename`/`input_line_number` for the
/// record being processed (`input` itself never runs in parallel).
struct RecordPosition(Rc<RefCell<(Option<Value>, u64)>>);

impl InputSource for RecordPosition {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        None
    }
    fn current_filename(&self) -> Option<Value> {
        self.0.borrow().0.clone()
    }
    fn current_line(&self) -> Value {
        Value::number(self.0.borrow().1 as f64)
    }
}

struct PortFactory {
    plan: ParallelPlan,
    p: Process,
}

struct PortWorker {
    jq: Jq,
    position: Rc<RefCell<(Option<Value>, u64)>>,
    p: Process,
    /// The current record's large `-r` strings (see [`RecordOut`]).
    marks: Vec<(usize, usize)>,
}

impl crate::io::parallel::WorkerFactory for PortFactory {
    type Worker = PortWorker;

    /// The program compiled as `run_program` compiles it (it compiled there,
    /// so it compiles here).
    fn new_worker(&self) -> PortWorker {
        let plan = &self.plan;
        let args = match parse_sized(plan.args_json.as_bytes()) {
            Ok(Value::Object(o)) => o,
            _ => Object::new(),
        };
        let mut attrs = JqAttrs::new(".");
        attrs.lib_dirs = Value::from(
            plan.lib_dirs
                .iter()
                .map(|p| Value::string_from_bytes(p))
                .collect::<Vec<_>>(),
        );
        attrs.jq_origin = Value::string_from_bytes(&plan.jq_origin);
        attrs.prog_origin = Value::string_from_bytes(&plan.prog_origin);
        let copts = CompileOptions {
            args,
            env: None,
            attrs,
        };
        let bc = jq_compile_args(&plan.program, &copts)
            .unwrap_or_else(|_| unreachable!("the program compiled on the main thread"));
        let mut jq = Jq::new(bc);
        jq.set_jq_attrs(&copts.attrs);
        jq.set_attr("VERSION_DIR", Value::from("1.8.1"));
        let position = Rc::new(RefCell::new((None, 0)));
        jq.set_input(Some(Box::new(RecordPosition(position.clone()))));
        PortWorker {
            jq,
            position,
            p: self.p.clone(),
            marks: Vec::new(),
        }
    }
}

impl crate::io::parallel::RecordWorker for PortWorker {
    /// main.c's `process()` into buffers (no halt: programs that can halt run
    /// sequentially).
    fn process(
        &mut self,
        value: Value,
        meta: &crate::io::parallel::RecordMeta<'_>,
        out: &mut Vec<u8>,
        err: &mut Vec<u8>,
    ) -> i32 {
        *self.position.borrow_mut() = (meta.filename.map(Value::from), meta.line);
        let mut ret = JQ_OK_NO_OUTPUT;
        self.jq.start(value, self.p.jq_flags);
        let mut error: Option<Value> = None;
        for result in self.jq.by_ref() {
            match result {
                Ok(v) => match write_result(
                    &v,
                    &self.p,
                    &mut RecordOut {
                        out,
                        marks: &mut self.marks,
                    },
                ) {
                    Some(r) => ret = r,
                    None => {
                        error = Some(Value::from(RAW_OUTPUT0_NUL));
                        break;
                    }
                },
                Err(e) => {
                    error = Some(e.into_value());
                    break;
                }
            }
        }
        if let Some(msg) = error {
            let pos = match meta.filename {
                Some(f) => format!("{}:{}", c_str(f), meta.line),
                None => "<unknown>".to_owned(),
            };
            err.extend_from_slice(uncaught_error_line(&msg, &pos).as_bytes());
            ret = JQ_ERROR_UNKNOWN;
        }
        ret
    }

    fn take_marks(&mut self, marks: &mut Vec<(usize, usize)>) {
        marks.append(&mut self.marks);
    }
}

/// main.c's loop state over the engine's in-order results.
struct MainLoop {
    unbuffered: bool,
    ret: i32,
    last_result: i32,
}

impl crate::io::parallel::RecordSink for MainLoop {
    fn record(&mut self, out: &[u8], err: &[u8], status: i32) -> std::ops::ControlFlow<()> {
        self.record_marked(out, err, &[], status)
    }

    /// A record's output as jq wrote it: piecemeal, except the marked `-r`
    /// strings (one `fwrite` each); then its uncaught error, if any, which
    /// came after the outputs. (With `--unbuffered`, jq flushes after each
    /// output; stderr gets nothing in between, so once per record is the same.)
    fn record_marked(
        &mut self,
        out: &[u8],
        err: &[u8],
        marks: &[(usize, usize)],
        status: i32,
    ) -> std::ops::ControlFlow<()> {
        if !out.is_empty() {
            with_stdout(|s| {
                let mut pos = 0;
                for &(off, len) in marks {
                    s.write(&out[pos..off]);
                    s.fwrite(&out[off..off + len]);
                    pos = off + len;
                }
                s.write(&out[pos..]);
                s.after_output(self.unbuffered);
            });
        }
        if !err.is_empty() {
            write_stderr(err);
        }
        self.ret = status;
        if status <= 0 && status != JQ_OK_NO_OUTPUT {
            self.last_result = i32::from(status != JQ_OK_NULL_KIND);
        }
        std::ops::ControlFlow::Continue(())
    }

    fn parse_error(&mut self, e: Error) -> std::ops::ControlFlow<()> {
        // Parse error (no --seq here: it runs sequentially)
        let msg = e.to_string();
        self.ret = JQ_ERROR_UNKNOWN;
        write_stderr(format!("{PROG}: parse error: {}\n", c_str(&msg)).as_bytes());
        std::ops::ControlFlow::Break(())
    }
}

/// The input loop of `run_program` on src/io's parallel record engine.
fn run_parallel(
    plan: ParallelPlan,
    files: Vec<Vec<u8>>,
    input_opts: InputOptions,
    p: Process,
    mut jq: Jq,
) -> (i32, i32) {
    let mut reader = super::input::open_reader(files, input_opts);
    // The program compiled on this thread processes the records read here.
    let position = Rc::new(RefCell::new((None, 0)));
    jq.set_input(Some(Box::new(RecordPosition(position.clone()))));
    let main = PortWorker {
        jq,
        position,
        p: p.clone(),
        marks: Vec::new(),
    };
    let mut sink = MainLoop {
        unbuffered: p.unbuffered,
        ret: JQ_OK_NO_OUTPUT,
        last_result: -1,
    };
    let mut engine = crate::io::parallel::EngineOptions {
        threads: plan.threads,
        ..crate::io::parallel::EngineOptions::default()
    };
    // QJ_WINDOW_SIZE=N: at most N MB of input in flight (as for the old core).
    if let Some(mb) = std::env::var("QJ_WINDOW_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&mb| mb > 0)
    {
        engine.window_bytes = mb << 20;
    }
    let factory = PortFactory { plan, p };
    let stats = crate::io::parallel::run_with(&mut reader, &factory, main, &mut sink, &engine);
    if std::env::var_os("QJ_ENGINE_STATS").is_some() {
        write_stderr(
            format!(
                "{PROG}: {} threads, {stats:?}, {:?}\n",
                engine.threads,
                reader.stats()
            )
            .as_bytes(),
        );
    }
    let mut ret = sink.ret;
    if reader.failures() != 0 {
        ret = JQ_ERROR_SYSTEM;
    }
    (close_stdout(ret), sink.last_result)
}

#[cfg(test)]
mod tests;
