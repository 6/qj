//! A small jq-like driver over the ported core: compiles a program with
//! [`jq_compile_args`] and runs it over JSON text the way jq's `main` loop does (each
//! input in turn, `input`/`inputs` reading ahead, stopping at an error or halt),
//! collecting what jq would print. It exists to test the compiler and VM end to end;
//! the real command line is the CLI track's port of `main.c`, which also owns exact
//! stderr text (error positions, colors, `--seq`, ...).
//!
//! ```
//! use qj::jq::lang::execute::driver::{Options, run};
//!
//! let out = run("[limit(3; .[])]", b"[1,2,3,4] [5]", &Options::default());
//! assert_eq!(out.stdout_str(), "[1,2,3]\n[5]\n");
//! assert_eq!(out.exit, 0);
//!
//! let out = run(".a", b"1", &Options::default());
//! assert_eq!(out.error.unwrap().as_str(), Some("Cannot index number with string \"a\""));
//! assert_eq!(out.exit, 5);
//! ```

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::{InputSource, Jq};
use crate::jq::lang::linker::JqAttrs;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::print::dump_to_vec;
use crate::jq::value::{DumpOptions, Error, ParseFlags, Parser, Value};

/// How to run (a small subset of jq's options).
#[derive(Clone, Debug)]
pub struct Options {
    /// Output format (`-c` by default; [`DumpOptions::pretty`] is jq's default).
    pub dump: DumpOptions,
    /// `-n`: run once on `null`; the inputs are only read by `input`/`inputs`.
    pub null_input: bool,
    /// `-L` directories (`JQ_LIBRARY_PATH`); `None` for jq's default search list.
    pub lib_dirs: Option<Vec<String>>,
    /// `jq_start` flags ([`super::JQ_DEBUG_TRACE`]): the trace goes to `stdout`,
    /// interleaved with the results, as in jq.
    pub trace: u32,
    /// Named arguments (`--arg`/`--argjson`): `$name` values.
    pub args: Vec<(String, Value)>,
    /// Whether native builtins may run ([`Jq::set_natives`]).
    pub natives: bool,
    /// Whether the optimized code runs ([`Jq::set_optimize`]).
    pub optimize: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            dump: DumpOptions::compact(),
            null_input: false,
            lib_dirs: None,
            trace: 0,
            args: Vec::new(),
            natives: true,
            optimize: true,
        }
    }
}

/// What a run printed and how it ended.
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// Results, each followed by `\n` (with the trace, if enabled).
    pub stdout: Vec<u8>,
    /// What `debug` and `stderr` printed (`["DEBUG:",v]` lines and raw `stderr` text).
    pub stderr: Vec<u8>,
    /// Compile errors, as jq prints them.
    pub compile_error: Option<String>,
    /// The uncaught error's message value (`jq: error (at ...): <msg>`).
    pub error: Option<Value>,
    /// An input that isn't valid JSON (`jq: parse error: <msg>`).
    pub parse_error: Option<String>,
    /// `Some((exit_code, error_message))` after `halt`/`halt_error`.
    pub halted: Option<(Option<Value>, Option<Value>)>,
    /// jq's exit status.
    pub exit: i32,
}

impl Output {
    /// `stdout` as text.
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// The remaining inputs, shared by the main loop and `input`.
#[derive(Clone)]
struct Inputs(Rc<RefCell<VecDeque<Result<Value, Error>>>>);

impl InputSource for Inputs {
    fn next_input(&mut self) -> Option<Result<Value, Error>> {
        self.0.borrow_mut().pop_front()
    }
    fn current_filename(&self) -> Option<Value> {
        // util.c names standard input "<stdin>".
        Some(Value::from("<stdin>"))
    }
}

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The attributes jq's `main` sets for a program given on the command line.
fn attrs(opts: &Options) -> JqAttrs {
    let mut a = JqAttrs::new(".");
    if let Some(dirs) = &opts.lib_dirs {
        a.lib_dirs = Value::from(
            dirs.iter()
                .map(|d| Value::from(d.as_str()))
                .collect::<Vec<_>>(),
        );
    }
    a
}

/// Compiles `program` and runs it over the JSON texts in `input`.
pub fn run(program: &str, input: &[u8], opts: &Options) -> Output {
    let mut out = Output::default();
    let mut copts = CompileOptions::new(".");
    copts.attrs = attrs(opts);
    for (k, v) in &opts.args {
        copts.args.insert(k.as_str().into(), v.clone());
    }
    let bc = match jq_compile_args(program.as_bytes(), &copts) {
        Ok(bc) => bc,
        Err(e) => {
            out.compile_error = Some(e.render());
            out.exit = 3;
            return out;
        }
    };
    let mut jq = Jq::new(bc);
    jq.set_jq_attrs(&copts.attrs);
    if !opts.natives {
        jq.set_natives(false);
    }
    if !opts.optimize {
        jq.set_optimize(false);
    }

    let stdout = Sink::default();
    let stderr = Sink::default();
    if opts.trace != 0 {
        jq.set_trace_writer(Some(Box::new(stdout.clone())));
    }
    {
        let e = stderr.clone();
        jq.set_debug_cb(Some(Box::new(move |v: &Value| {
            // main.c debug_cb: ["DEBUG:",v] compactly, then a newline.
            let mut b = e.0.borrow_mut();
            let msg = Value::from(vec![Value::from("DEBUG:"), v.clone()]);
            dump_to_vec(&msg, &DumpOptions::compact(), &mut b);
            b.push(b'\n');
        })));
        let e = stderr.clone();
        jq.set_stderr_cb(Some(Box::new(move |v: &Value| {
            // main.c stderr_cb: strings raw, anything else as compact JSON.
            let mut b = e.0.borrow_mut();
            match v {
                Value::String(s) => b.extend_from_slice(s.as_bytes()),
                _ => dump_to_vec(v, &DumpOptions::compact(), &mut b),
            }
        })));
    }

    let mut parser = Parser::new(ParseFlags::default());
    parser.set_buf(input, false);
    let mut values = VecDeque::new();
    while let Some(v) = parser.next() {
        let bad = v.is_err();
        values.push_back(v);
        if bad {
            break;
        }
    }
    let inputs = Inputs(Rc::new(RefCell::new(values)));
    jq.set_input(Some(Box::new(inputs.clone())));

    let mut first = true;
    loop {
        let value = if opts.null_input {
            if !first {
                break;
            }
            Value::Null
        } else {
            match inputs.0.borrow_mut().pop_front() {
                None => break,
                Some(Ok(v)) => v,
                Some(Err(e)) => {
                    out.parse_error = Some(e.to_string());
                    out.exit = 2;
                    break;
                }
            }
        };
        first = false;
        jq.start(value, opts.trace);
        let mut failed = false;
        for r in &mut jq {
            match r {
                Ok(v) => {
                    let mut b = stdout.0.borrow_mut();
                    dump_to_vec(&v, &opts.dump, &mut b);
                    b.push(b'\n');
                }
                Err(e) => {
                    out.error = Some(e.into_value());
                    failed = true;
                }
            }
        }
        if jq.halted() {
            let code = jq.exit_code().cloned();
            // main.c: exit with the number if positive, else 0; a non-number is 5.
            out.exit = match &code {
                None => 0,
                Some(Value::Number(n)) => (n.value() as i32).max(0),
                Some(_) => 5,
            };
            out.halted = Some((code, jq.error_message().cloned()));
            break;
        }
        if failed {
            out.exit = 5;
        }
    }
    drop(jq);
    out.stdout = std::mem::take(&mut *stdout.0.borrow_mut());
    out.stderr = std::mem::take(&mut *stderr.0.borrow_mut());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs(program: &str, input: &str) -> String {
        let out = run(program, input.as_bytes(), &Options::default());
        let mut s = out.stdout_str();
        if let Some(e) = out.error {
            match e.as_str() {
                Some(m) => s.push_str(&format!("error: {m}\n")),
                None => s.push_str(&format!("error: {e}\n")),
            }
        }
        if let Some(e) = out.compile_error {
            s.push_str(&e);
        }
        s
    }

    /// End-to-end checks that don't need the jq binary (expectations from jq 1.8.1).
    #[test]
    fn compiled_programs_run() {
        for (program, input, want) in [
            (".[] | . + 1", "[1,2]", "2\n3\n"),
            ("[limit(3; range(10))]", "null", "[0,1,2]\n"),
            ("first(range(10; 0; -1))", "null", "10\n"),
            (
                "[label $f | try break $f catch .]",
                "null",
                "[{\"__jq\":0}]\n",
            ),
            ("try (1, error(\"x\"), 3) catch .", "null", "1\n\"x\"\n"),
            ("(try (1, 2)) | error", "null", "error: 1\n"),
            ("[.[] as [$a] ?// $a | $a]", "[[1],2]", "[1,2]\n"),
            ("[paths]", "{\"a\":[1]}", "[[\"a\"],[\"a\",0]]\n"),
            (
                "path(.a | . + 1)",
                "{\"a\":1}",
                "error: Invalid path expression with result 2\n",
            ),
            ("reduce .[] as $x (0; . + $x)", "[1,2,3]", "6\n"),
            (
                "[foreach .[] as $x (0; . + $x; [$x, .])]",
                "[1,2]",
                "[[1,1],[2,3]]\n",
            ),
            ("def f: if . < 5 then . + 1 | f else . end; f", "0", "5\n"),
            ("(.. | numbers) |= . + 1", "[1,[2]]", "[2,[3]]\n"),
            ("[range(4)+1] | .[0:2] | .[3] = 9", "null", "[1,2,3,9]\n"),
            ("to_entries", "{\"a\":1}", "[{\"key\":\"a\",\"value\":1}]\n"),
            (
                "[.[] | tostring]",
                "[1,\"a\",[2]]",
                "[\"1\",\"a\",\"[2]\"]\n",
            ),
            ("\"\\(.a)-\\(.b)\"", "{\"a\":1,\"b\":\"x\"}", "\"1-x\"\n"),
            ("[splits(\", \")]", "\"a, b\"", "[\"a\",\"b\"]\n"),
            (
                "$__loc__",
                "null",
                "{\"file\":\"<top-level>\",\"line\":1}\n",
            ),
            ("[., input]", "1 2", "[1,2]\n"),
            ("[inputs]", "1 2 3", "[2,3]\n"),
            (
                "$x",
                "null",
                "jq: error: $x is not defined at <top-level>, line 1, column 1:\n    $x\n    ^^\njq: 1 compile error\n",
            ),
        ] {
            assert_eq!(outputs(program, input), want, "{program}");
        }
    }

    /// Nested closures give deeply nested subfunctions (each argument is a lambda
    /// inside the enclosing one); they resolve through `level` frame links.
    #[test]
    fn nested_closures() {
        let depth = 150;
        let program = format!(
            "def f(x): x + 1; . as $a | {}$a{}",
            "f(".repeat(depth),
            ")".repeat(depth)
        );
        assert_eq!(outputs(&program, "0"), format!("{depth}\n"));
        let program = format!("[{}.{}]", "(.[] | ".repeat(40), ")".repeat(40));
        let input = format!("{}1{}", "[".repeat(40), "]".repeat(40));
        assert_eq!(outputs(&program, &input), "[1]\n");
    }

    #[test]
    fn halting_and_callbacks() {
        let out = run(
            "1, (\"bye\\n\" | halt_error(3)), 2",
            b"null",
            &Options::default(),
        );
        assert_eq!(out.stdout_str(), "1\n");
        assert_eq!(out.exit, 3);
        let (code, msg) = out.halted.unwrap();
        assert_eq!(code.and_then(|c| c.as_f64()), Some(3.0));
        assert_eq!(msg.unwrap().as_str(), Some("bye\n"));

        let out = run(
            "debug | stderr | empty",
            b"{\"a\":\"x\"}",
            &Options::default(),
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "[\"DEBUG:\",{\"a\":\"x\"}]\n{\"a\":\"x\"}"
        );
        let out = run("halt", b"1 2", &Options::default());
        assert_eq!((out.stdout_str().as_str(), out.exit), ("", 0));
    }
}
