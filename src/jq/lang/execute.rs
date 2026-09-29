//! The jq virtual machine: port of jq 1.8.1's `execute.c` (and `exec_stack.h`).
//!
//! # Usage
//!
//! [`Jq`] is jq's `jq_state` minus compilation: it runs a program compiled to
//! [`Bytecode`] by [`jq_compile_args`](super::jq_compile_args). Values are `Rc`, so a
//! `Jq` never crosses threads: each worker thread compiles and runs its own instance
//! (compile once per thread, then `start`/`next` per input; `start` is cheap and reuses
//! the stack's memory).
//!
//! ```
//! use qj::jq::lang::execute::Jq;
//! use qj::jq::lang::{CompileOptions, jq_compile_args};
//! use qj::jq::value::{Value, parse_sized};
//!
//! let opts = CompileOptions::new(".");
//! let bc = jq_compile_args(b".[] | try (10 / .) catch \"oops\"", &opts).unwrap();
//! let mut jq = Jq::new(bc); // jq_init + the compiled jq->bc
//! jq.set_jq_attrs(&opts.attrs); // what main.c sets (search list, origins)
//! jq.set_debug_cb(Some(Box::new(|v: &Value| eprintln!("[\"DEBUG:\",{v}]"))));
//! jq.start(parse_sized(b"[1, 0, 4]").unwrap(), 0); // jq_start(jq, value, flags)
//! let mut out = Vec::new();
//! for r in &mut jq {
//!     // jq_next: Jq is an Iterator
//!     match r {
//!         Ok(v) => out.push(v.to_json()),
//!         Err(e) => panic!("jq: error (at <stdin>:1): {e}"), // uncaught: the run ends
//!     }
//! }
//! assert_eq!(out, ["10", "\"oops\"", "2.5"]);
//! assert!(!jq.halted()); // else see exit_code() and error_message()
//! ```
//!
//! [`driver::run`] wraps compile + run with main.c's loop over inputs, for tests.
//!
//! | jq.h | here |
//! |---|---|
//! | `jq_init` + `jq_compile` | [`Jq::new`] (from the compiler's `Rc<Bytecode>`) |
//! | `jq_start(jq, input, flags)` | [`Jq::start`]; flags [`JQ_DEBUG_TRACE`], [`JQ_DEBUG_TRACE_DETAIL`], [`JQ_DEBUG_TRACE_ALL`] |
//! | `jq_next` | [`Jq::next`] (`Iterator`): `Some(Ok(v))` per output, then `None` when the program is done or halted, or one `Some(Err(e))` for an uncaught error (after which the run is over) |
//! | `jq_halt`, `jq_halted`, `jq_get_exit_code`, `jq_get_error_message` | [`Jq::halt`], [`Jq::halted`], [`Jq::exit_code`], [`Jq::error_message`] (`None` is jq's `jv_invalid()`) |
//! | `jq_set_input_cb` (+ `jq_util_input_get_current_filename`/`_line`) | [`Jq::set_input`] with an [`InputSource`] |
//! | `jq_set_debug_cb`, `jq_set_stderr_cb` | [`Jq::set_debug_cb`], [`Jq::set_stderr_cb`] |
//! | `jq_set_error_cb`, `jq_report_error`, `jq_format_error` | [`Jq::set_error_cb`], [`Jq::report_error`], [`format_error`] |
//! | `jq_set_attrs`, `jq_set_attr`, `jq_get_attr` | [`Jq::set_attrs`], [`Jq::set_attr`], [`Jq::get_attr`]; [`Jq::set_jq_attrs`] sets main.c's three from the compiler's [`JqAttrs`] |
//! | `jq_get_lib_dirs`, `jq_get_prog_origin`, `jq_get_jq_origin` | [`Jq::lib_dirs`], [`Jq::prog_origin`], [`Jq::jq_origin`] |
//! | `--debug-trace` output (stdout) | [`Jq::set_trace_writer`] |
//! | `jq_dump_disassembly` (`--debug-dump-disasm`) | [`Jq::dump_disassembly`] |
//!
//! The C builtins reach the VM through the [`Host`] trait, which `Jq` implements.
//!
//! # Semantics kept from execute.c
//!
//! * One stack of blocks holds the data stack, call frames and fork points; blocks are
//!   only freed when they are the last allocated one, so fork points can resume old
//!   data stacks and frames (see `stack.rs`). Values popped from blocks that stay alive
//!   are copies; others are moved, and `LOADVN`/`DUPN` move instead of copying, so a
//!   uniquely owned value stays unique and builtins update it in place (this is what
//!   keeps `reduce ... (.; . + [$x])` linear).
//! * Errors unwind by backtracking with `jq->error` set. `TRY_END` wraps an error raised
//!   after the `try` body produced a value, and the matching `TRY_BEGIN` unwraps and
//!   re-raises it instead of catching it. `?//` (`DESTRUCTURE_ALT`) catches any error.
//!   `break` is an error carrying the label object `{"__jq": n}`.
//! * Path expressions track `jq->path` and `jq->value_at_path`, raising
//!   `Invalid path expression ...` when a value doesn't come from the path.

pub mod driver;
mod program;
mod run;
mod stack;
mod trace;

#[cfg(test)]
mod disasm;
#[cfg(test)]
mod suites;
#[cfg(test)]
mod tests;

use std::io::Write;
use std::rc::Rc;

use crate::jq::builtins::{CResult, Host};
use crate::jq::lang::bytecode::Bytecode;
use crate::jq::lang::linker::{JqAttrs, load_module_meta};
use crate::jq::value::{DumpOptions, Error, Object, Str, Value, dump_string};

use program::Program;
use stack::{Stack, StackPtr};

/// `JQ_DEBUG_TRACE`: print each instruction and its stack inputs (`--debug-trace`).
pub const JQ_DEBUG_TRACE: u32 = 1;
/// `JQ_DEBUG_TRACE_DETAIL`: also print the rest of the stack.
pub const JQ_DEBUG_TRACE_DETAIL: u32 = 2;
/// `JQ_DEBUG_TRACE_ALL` (`--debug-trace=all`).
pub const JQ_DEBUG_TRACE_ALL: u32 = JQ_DEBUG_TRACE | JQ_DEBUG_TRACE_DETAIL;

/// The input side of jq's util layer (`jq_util_input_*`), as `input`, `inputs`,
/// `input_filename` and `input_line_number` see it.
pub trait InputSource {
    /// `jq_util_input_next_input_cb`: the next input, `Some(Err(..))` for an input error
    /// (such as a parse error), `None` when the inputs are exhausted.
    fn next_input(&mut self) -> Option<Result<Value, Error>>;
    /// `jq_util_input_get_current_filename`; `None` is jq's invalid (unknown), which
    /// `input_filename` turns into `null`.
    fn current_filename(&self) -> Option<Value> {
        None
    }
    /// `jq_util_input_get_current_line`.
    fn current_line(&self) -> Value {
        Value::from(0)
    }
}

impl<F: FnMut() -> Option<Result<Value, Error>>> InputSource for F {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        self()
    }
}

/// A message callback (`jq_msg_cb`): debug, stderr and error callbacks.
pub type MsgCallback = Box<dyn FnMut(&Value)>;

/// An error being raised (`jq->error` when it is an invalid with a message): the
/// message, wrapped `wraps` times by `TRY_END` (jq nests `jv_invalid_with_msg`).
#[derive(Clone, Debug)]
struct Raised {
    msg: Value,
    wraps: u32,
}

/// `struct jq_state` (the execution part): a program plus its interpreter state.
pub struct Jq {
    prog: Rc<Program>,

    // jq_state's execution fields.
    stk: Stack,
    curr_frame: StackPtr,
    stk_top: StackPtr,
    fork_top: StackPtr,
    /// `jq->error`: `None` is `jv_null()`.
    error: Option<Raised>,
    /// `jq->path`: `null`, or an array inside a path expression.
    path: Value,
    value_at_path: Value,
    subexp_nest: i32,
    debug_trace: u32,
    initial_execution: bool,
    /// Whether `jq_start` ran and the run can still be resumed (jq asserts instead).
    running: bool,
    /// `next_label`: never reset, so labels keep counting across inputs as in jq.
    next_label: u32,

    halted: bool,
    exit_code: Option<Value>,
    error_message: Option<Value>,

    attrs: Value,
    input: Option<Box<dyn InputSource>>,
    debug_cb: Option<MsgCallback>,
    stderr_cb: Option<MsgCallback>,
    err_cb: Option<MsgCallback>,
    /// `$HOME` for module lookups by `modulemeta` (jq calls `get_home()` there).
    home: Option<String>,
    trace_out: Option<Box<dyn Write>>,
}

impl Jq {
    /// `jq_init` with `jq->bc` set to a compiled program (the compiler has already
    /// applied jq's tail-call optimization).
    pub fn new(bc: Rc<Bytecode>) -> Jq {
        Jq {
            prog: Rc::new(Program::new(bc)),
            stk: Stack::default(),
            curr_frame: 0,
            stk_top: 0,
            fork_top: 0,
            error: None,
            path: Value::Null,
            value_at_path: Value::Null,
            subexp_nest: 0,
            debug_trace: 0,
            initial_execution: false,
            running: false,
            next_label: 0,
            halted: false,
            exit_code: None,
            error_message: None,
            attrs: Value::empty_object(),
            input: None,
            debug_cb: None,
            stderr_cb: None,
            err_cb: None,
            home: std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()),
            trace_out: None,
        }
    }

    /// What `jq_compile` does to an existing state: `jq_reset`, then the new program
    /// replaces the old one. The attributes, callbacks and the label counter stay, as
    /// when jq's test runner (`--run-tests`) compiles every test in one `jq_state`.
    pub fn set_bytecode(&mut self, bc: Rc<Bytecode>) {
        self.reset();
        self.prog = Rc::new(Program::new(bc));
    }

    /// `jq_start(jq, input, flags)`: resets the machine and prepares to run the program
    /// on `input`. `flags` may enable [`JQ_DEBUG_TRACE`] output.
    pub fn start(&mut self, input: Value, flags: u32) {
        self.reset();
        let top = stack::Closure { func: 0, env: 0 };
        // (jq passes env -1 for the top closure; it is never followed.)
        self.frame_push_top(top);
        self.push(input);
        let pos = (self.stk_top, self.curr_frame);
        self.stack_save(self.prog.funcs[0].base as usize, pos);
        self.debug_trace = flags & JQ_DEBUG_TRACE_ALL;
        self.initial_execution = true;
        self.running = true;
    }

    /// `jq_reset`: unwinds everything and clears the error, halt and path state.
    fn reset(&mut self) {
        while self.stack_restore().is_some() {}
        debug_assert_eq!(self.stk_top, 0);
        debug_assert_eq!(self.fork_top, 0);
        debug_assert_eq!(self.curr_frame, 0);
        self.stk.reset();
        self.error = None;
        self.halted = false;
        self.exit_code = None;
        self.error_message = None;
        self.path = Value::Null;
        self.value_at_path = Value::Null;
        self.subexp_nest = 0;
        self.running = false;
    }

    /// `jq_halt`: stops the program; `next` then returns `None`.
    pub fn halt(&mut self, exit_code: Option<Value>, error_message: Option<Value>) {
        debug_assert!(!self.halted);
        self.halted = true;
        self.exit_code = exit_code;
        self.error_message = error_message;
    }

    /// `jq_halted`.
    pub fn halted(&self) -> bool {
        self.halted
    }

    /// `jq_get_exit_code`: `None` is jq's `jv_invalid()` (`halt`, as opposed to
    /// `halt_error`).
    pub fn exit_code(&self) -> Option<&Value> {
        self.exit_code.as_ref()
    }

    /// `jq_get_error_message`: `None` is jq's `jv_invalid()`.
    pub fn error_message(&self) -> Option<&Value> {
        self.error_message.as_ref()
    }

    /// `jq_set_input_cb`: where `input`/`inputs` read from, and what
    /// `input_filename`/`input_line_number` report.
    pub fn set_input(&mut self, input: Option<Box<dyn InputSource>>) {
        self.input = input;
    }

    /// Takes the input source back (`jq_get_input_cb`).
    pub fn take_input(&mut self) -> Option<Box<dyn InputSource>> {
        self.input.take()
    }

    /// `jq_set_debug_cb`: receives the input of every `debug` call. The jq CLI prints
    /// `["DEBUG:",<value>]` compactly on stderr.
    pub fn set_debug_cb(&mut self, cb: Option<MsgCallback>) {
        self.debug_cb = cb;
    }

    /// `jq_set_stderr_cb`: receives the input of every `stderr` call.
    pub fn set_stderr_cb(&mut self, cb: Option<MsgCallback>) {
        self.stderr_cb = cb;
    }

    /// `jq_set_error_cb`: receives errors reported while running (jq's
    /// `jq_report_error`, e.g. syntax errors in a module loaded by `modulemeta`).
    /// `None` restores the default, which prints [`format_error`]'s text to stderr.
    pub fn set_error_cb(&mut self, cb: Option<MsgCallback>) {
        self.err_cb = cb;
    }

    /// `jq_report_error`.
    pub fn report_error(&mut self, msg: Value) {
        report_error_to(&mut self.err_cb, msg);
    }

    /// Where `--debug-trace` output goes (jq prints it to stdout, interleaved with the
    /// results). Defaults to this process's stdout.
    pub fn set_trace_writer(&mut self, w: Option<Box<dyn Write>>) {
        self.trace_out = w;
    }

    /// `jq_dump_disassembly(jq, indent)`: what `--debug-dump-disasm` prints (jq's `main`
    /// adds one more `\n` after it).
    pub fn dump_disassembly(&self, indent: usize) -> String {
        crate::jq::lang::bytecode::dump_disassembly(indent, &self.prog.funcs[0].bc)
    }

    /// `jq_set_attrs`: `attrs` must be an object.
    pub fn set_attrs(&mut self, attrs: Value) {
        assert!(
            matches!(attrs, Value::Object(_)),
            "jq_set_attrs: not an object"
        );
        self.attrs = attrs;
    }

    /// `jq_set_attr`.
    pub fn set_attr(&mut self, attr: &str, val: Value) {
        if let Value::Object(o) = &mut self.attrs {
            o.insert(Str::from(attr), val);
        }
    }

    /// `jq_get_attr`: `None` is jq's invalid (absent).
    pub fn get_attr(&self, attr: &str) -> Option<Value> {
        self.attrs.as_object().and_then(|o| o.get(attr)).cloned()
    }

    /// Sets the attributes main.c sets (`JQ_LIBRARY_PATH`, `JQ_ORIGIN`,
    /// `PROGRAM_ORIGIN`) from the ones the program was compiled with, plus the `$HOME`
    /// that `modulemeta` uses to find modules.
    pub fn set_jq_attrs(&mut self, attrs: &JqAttrs) {
        self.set_attr("JQ_LIBRARY_PATH", attrs.lib_dirs.clone());
        self.set_attr("JQ_ORIGIN", attrs.jq_origin.clone());
        self.set_attr("PROGRAM_ORIGIN", attrs.prog_origin.clone());
        self.home = attrs.home.clone();
    }

    /// The linker's view of the attributes (for `modulemeta`).
    fn linker_attrs(&self) -> JqAttrs {
        JqAttrs {
            lib_dirs: Jq::lib_dirs(self),
            jq_origin: Jq::jq_origin(self),
            prog_origin: Jq::prog_origin(self),
            home: self.home.clone(),
        }
    }

    /// `jq_get_jq_origin`: the `JQ_ORIGIN` attribute.
    pub fn jq_origin(&self) -> Value {
        self.get_attr("JQ_ORIGIN").unwrap_or(Value::Null)
    }

    /// `jq_get_prog_origin`: the `PROGRAM_ORIGIN` attribute.
    pub fn prog_origin(&self) -> Value {
        self.get_attr("PROGRAM_ORIGIN").unwrap_or(Value::Null)
    }

    /// `jq_get_lib_dirs`: the `JQ_LIBRARY_PATH` attribute, or `[]`.
    pub fn lib_dirs(&self) -> Value {
        self.get_attr("JQ_LIBRARY_PATH")
            .unwrap_or_else(Value::empty_array)
    }
}

impl Iterator for Jq {
    type Item = Result<Value, Error>;

    /// `jq_next`: the next output, `None` when the program is done (or halted, see
    /// [`Jq::halted`]), or an uncaught error, which ends the run.
    fn next(&mut self) -> Option<Result<Value, Error>> {
        // (jq asserts when resumed after the end; after a halt it would keep
        // unwinding fork points.)
        if !self.running || self.halted {
            return None;
        }
        let r = self.jq_next();
        if !matches!(r, Some(Ok(_))) && !self.halted {
            self.running = false;
        }
        r
    }
}

impl Drop for Jq {
    /// `jq_teardown`: unwind iteratively so deep stacks don't recurse.
    fn drop(&mut self) {
        self.reset();
    }
}

/// `jq_report_error` with the default callback (`default_err_cb`).
fn report_error_to(cb: &mut Option<MsgCallback>, msg: Value) {
    match cb {
        Some(cb) => cb(&msg),
        None => {
            let msg = format_error(Ok(msg));
            if let Some(s) = msg.as_str() {
                eprintln!("{s}");
            }
        }
    }
}

/// `jq_format_error`: the text jq prints for an error message. `Ok(string)` is printed
/// as is (it is expected to be formatted already); anything else gets `jq: error: ` in
/// front, dumped as JSON if it isn't a string. A `null` message is jq's out-of-memory
/// case (it prints `jq: error: out of memory` and returns `null`).
pub fn format_error(msg: Result<Value, Error>) -> Value {
    let msg = match msg {
        Ok(v @ Value::String(_)) => return v,
        Ok(v) => v,
        Err(e) => e.into_value(),
    };
    match &msg {
        Value::Null => {
            eprintln!("jq: error: out of memory");
            Value::Null
        }
        // `jv_string_fmt("jq: error: %s", ...)` stops at a NUL.
        Value::String(s) => {
            let s = s.as_str();
            Value::from(format!("jq: error: {}", s.split('\0').next().unwrap_or("")))
        }
        _ => Value::from(format!(
            "jq: error: {}",
            dump_string(&msg, &DumpOptions::default())
        )),
    }
}

impl Host for Jq {
    fn next_input(&mut self) -> Option<CResult> {
        self.input.as_mut()?.next_input()
    }

    fn debug(&mut self, v: &Value) {
        if let Some(cb) = &mut self.debug_cb {
            cb(v);
        }
    }

    fn stderr(&mut self, v: &Value) {
        if let Some(cb) = &mut self.stderr_cb {
            cb(v);
        }
    }

    fn halt(&mut self, exit_code: Option<Value>, error_message: Option<Value>) {
        Jq::halt(self, exit_code, error_message);
    }

    fn lib_dirs(&self) -> Value {
        Jq::lib_dirs(self)
    }

    fn prog_origin(&self) -> Value {
        Jq::prog_origin(self)
    }

    fn jq_origin(&self) -> Value {
        Jq::jq_origin(self)
    }

    fn module_meta(&mut self, name: &Value) -> CResult {
        // linker.c load_module_meta; the module's syntax errors go to jq_report_error.
        let attrs = self.linker_attrs();
        let name = name.as_str().unwrap_or("");
        let err_cb = &mut self.err_cb;
        load_module_meta(&attrs, name, &mut |msg| {
            report_error_to(err_cb, Value::from(msg))
        })
    }

    fn current_filename(&self) -> Option<Value> {
        self.input.as_ref()?.current_filename()
    }

    fn current_line(&self) -> Value {
        match &self.input {
            Some(i) => i.current_line(),
            None => Value::from(0),
        }
    }

    fn path_append(&mut self, v: Value, p: Value, value_at_path: CResult) -> CResult {
        self.jq_path_append(v, p, value_at_path)
    }
}

/// `{"__jq": n}`, the value `GENLABEL` pushes.
fn label_object(n: u32) -> Value {
    let mut o = Object::with_capacity(1);
    o.insert(Str::from("__jq"), Value::number(n as f64));
    Value::Object(o)
}
