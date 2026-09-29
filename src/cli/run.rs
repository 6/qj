//! Port of jq 1.8.1's `main.c` after option parsing: setup, compilation, the
//! `process()` loop over inputs, and exit codes, on the ported core
//! (`src/jq`). [`main`] is what the `qj` binary runs with `QJ_CORE=port`.
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

use super::args::{self, Action, ArgError, ArgValue, Options, ProgramArgument, print_flags};
use super::input::{InputOptions, UtilInput};
use crate::jq::lang::execute::{InputSource, Jq};
use crate::jq::lang::linker::JqAttrs;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::print::dump_to_vec;
use crate::jq::value::{
    Array, Colors, DumpOptions, Error, Indent, Object, ParseFlags, Parser, Str, Value, dump_string,
    parse_sized, unicode,
};

/// The program name in messages.
const PROG: &str = "qj";

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

/// jq's `stdout` FILE: a buffer, flushed when full, after each output with
/// `--unbuffered`, up to the last newline when stdout is a terminal (line
/// buffering), and when closed at the end.
struct Stdout {
    buf: Vec<u8>,
    line_buffered: bool,
    /// `ferror(stdout)`: the last write error.
    error: Option<io::Error>,
}

/// Flush at this size.
const STDOUT_BUFFER: usize = 64 * 1024;

thread_local! {
    static STDOUT: RefCell<Stdout> = const {
        RefCell::new(Stdout {
            buf: Vec::new(),
            line_buffered: false,
            error: None,
        })
    };
}

impl Stdout {
    /// Writes `self.buf[..n]` to fd 1 and drops it from the buffer. A failed
    /// write loses the data and sets the error, as stdio does.
    fn write_out(&mut self, n: usize) {
        let mut done = 0;
        while done < n {
            let chunk = &self.buf[done..n];
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
        self.buf.drain(..n);
    }

    fn flush(&mut self) {
        let n = self.buf.len();
        if n > 0 {
            self.write_out(n);
        }
    }

    /// After an output: flush what buffering requires.
    fn after_output(&mut self, unbuffered: bool) {
        if unbuffered || self.buf.len() >= STDOUT_BUFFER {
            self.flush();
        } else if self.line_buffered
            && let Some(nl) = memchr::memrchr(b'\n', &self.buf)
        {
            self.write_out(nl + 1);
        }
    }
}

fn with_stdout<R>(f: impl FnOnce(&mut Stdout) -> R) -> R {
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

/// The `--debug-trace` writer: into the stdout buffer.
struct TraceOut;

impl Write for TraceOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        with_stdout(|s| {
            s.buf.extend_from_slice(buf);
            if s.buf.len() >= STDOUT_BUFFER {
                s.flush();
            }
        });
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

fn write_stderr(bytes: &[u8]) {
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

/// A named or positional argument as a value: `jv_string` for text.
fn arg_value(v: &ArgValue<Value>) -> Value {
    match v {
        ArgValue::Text(bytes) => Value::string_from_bytes(bytes),
        ArgValue::Json(v) => v.clone(),
    }
}

/// `ARGS` and `program_arguments` as main.c builds them for
/// `jq_compile_args`.
fn program_arguments(opts: &Options<Value>) -> Object {
    let mut vars = Object::new();
    for var in opts.program_arguments() {
        match var {
            ProgramArgument::Named(name, value) => {
                vars.insert(Str::from_bytes(name), arg_value(value));
            }
            ProgramArgument::Args => {
                let positional: Vec<Value> = opts.positional.iter().map(arg_value).collect();
                let mut named = Object::new();
                for (name, value) in &opts.named {
                    named.insert(Str::from_bytes(name), arg_value(value));
                }
                let mut a = Object::new();
                a.insert(Str::from("positional"), Value::from(positional));
                a.insert(Str::from("named"), Value::Object(named));
                vars.insert(Str::from("ARGS"), Value::Object(a));
            }
            ProgramArgument::BuildConfiguration => {
                vars.insert(
                    Str::from("JQ_BUILD_CONFIGURATION"),
                    Value::from(super::usage::BUILD_CONFIGURATION),
                );
            }
        }
    }
    vars
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
type SharedInput = Rc<RefCell<UtilInput>>;

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
        let stop = with_stdout(|out| {
            match &result {
                Value::String(s) if p.raw_output => {
                    if p.ascii_output {
                        let ascii = DumpOptions {
                            ascii: true,
                            ..DumpOptions::default()
                        };
                        dump_to_vec(&result, &ascii, &mut out.buf);
                    } else if p.raw_output0 && memchr::memchr(0, s.as_bytes()).is_some() {
                        return true;
                    } else {
                        out.buf.extend_from_slice(s.as_bytes());
                    }
                    ret = JQ_OK;
                }
                _ => {
                    ret = match result {
                        Value::Null | Value::Bool(false) => JQ_OK_NULL_KIND,
                        _ => JQ_OK,
                    };
                    if p.seq {
                        out.buf.push(0x1e);
                    }
                    dump_to_vec(&result, &p.dump, &mut out.buf);
                }
            }
            if !p.raw_no_lf {
                out.buf.push(b'\n');
            }
            if p.raw_output0 {
                out.buf.push(0);
            }
            out.after_output(p.unbuffered);
            false
        });
        if stop {
            error = Some(Value::from(
                "Cannot dump a string containing NUL with --raw-output0 option",
            ));
            break;
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
        let line = match &msg {
            Value::String(s) => format!("{PROG}: error (at {pos}): {}\n", c_str(s.as_str())),
            _ => format!(
                "{PROG}: error (at {pos}) (not a string): {}\n",
                dump_plain(&msg)
            ),
        };
        write_stderr(line.as_bytes());
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

    let opts = match args::with_environment_locale(|| args::parse(argv, &mut PortArgs)) {
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
        Ok(Action::RunTests { .. }) => {
            write_stderr(format!("{PROG}: error: --run-tests is not supported\n").as_bytes());
            return JQ_ERROR_SYSTEM;
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
    let ret = run_program(&opts, stdout_is_tty);
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
fn run_program(opts: &Options<Value>, stdout_is_tty: bool) -> (i32, i32) {
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

    let copts = CompileOptions {
        args: program_arguments(opts),
        env: None,
        attrs,
    };
    let bc = match jq_compile_args(&program, &copts) {
        Ok(bc) => bc,
        Err(e) => {
            let mut text = Vec::new();
            for m in &e.messages {
                let m = m
                    .strip_prefix("jq:")
                    .map_or_else(|| m.clone(), |rest| format!("{PROG}:{rest}"));
                text.extend_from_slice(m.as_bytes());
                text.push(b'\n');
            }
            write_stderr(&text);
            return (close_stdout(JQ_ERROR_COMPILE), -1);
        }
    };
    let mut jq = Jq::new(bc);
    jq.set_jq_attrs(&copts.attrs);
    jq.set_attr("VERSION_DIR", Value::from("1.8.1"));
    jq.set_trace_writer(Some(Box::new(TraceOut)));

    if opts.dump_disasm {
        let text = format!("{}\n", jq.dump_disassembly(0));
        with_stdout(|s| {
            s.buf.extend_from_slice(text.as_bytes());
            s.after_output(false);
        });
    }

    let files = if opts.files.is_empty() {
        vec![b"-".to_vec()]
    } else {
        args::expand_file_globs(&opts.files)
    };
    let input: SharedInput = Rc::new(RefCell::new(UtilInput::new(
        files,
        InputOptions {
            raw: opts.raw_input,
            slurp: opts.slurp,
            flags: ParseFlags::from_bits(opts.parser_flags()),
        },
    )));
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

#[cfg(test)]
mod tests;
