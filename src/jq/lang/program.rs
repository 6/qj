//! The compiler's entry point: port of execute.c's `jq_compile_args` (without the
//! VM state around it).
//!
//! ```
//! use qj::jq::lang::bytecode::dump_disassembly;
//! use qj::jq::lang::{CompileOptions, jq_compile_args};
//!
//! let opts = CompileOptions::new("/usr/local/bin");
//! let bc = jq_compile_args(b".a + 1", &opts).unwrap();
//! assert_eq!(
//!     dump_disassembly(0, &bc),
//!     "0000 TOP\n0001 PUSHK_UNDER 1\n0003 SUBEXP_BEGIN\n0004 PUSHK_UNDER \"a\"\n\
//!      0006 INDEX\n0007 SUBEXP_END\n0008 CALL_BUILTIN _plus\n0011 RET\n"
//! );
//!
//! let err = jq_compile_args(b"$x", &opts).unwrap_err();
//! assert_eq!(
//!     err.render(),
//!     "jq: error: $x is not defined at <top-level>, line 1, column 1:\n    $x\n    ^^\n\
//!      jq: 1 compile error\n"
//! );
//! ```

use std::rc::Rc;

use super::bytecode::Bytecode;
use super::compile::{Compiler, Globals};
use super::linker::{JqAttrs, load_program};
use super::locfile::{LocFile, compile_errors_summary};
use crate::jq::builtins::bind::builtins_bind;
use crate::jq::value::{Object, Value};

/// What `jq_compile_args` gets besides the program text.
#[derive(Clone, Debug)]
pub struct CompileOptions {
    /// The named arguments (jq's `args`): an unbound `$name` compiles to
    /// `LOADK args[name]`. main.c passes the `--arg`/`--argjson`/`--slurpfile`/
    /// `--rawfile` values plus `ARGS` (`{"positional": [...], "named": {...}}`) and
    /// `JQ_BUILD_CONFIGURATION`.
    pub args: Object,
    /// `$ENV` (which wins over a named argument called `ENV`); `None` builds it from
    /// the process environment on first use, like jq.
    pub env: Option<Value>,
    /// Library search path, origins and `$HOME`, for `import`/`include`.
    pub attrs: JqAttrs,
}

impl CompileOptions {
    /// No named arguments, the process environment, and [`JqAttrs::new`].
    pub fn new(jq_origin: &str) -> CompileOptions {
        CompileOptions {
            args: Object::new(),
            env: None,
            attrs: JqAttrs::new(jq_origin),
        }
    }
}

/// A program that didn't compile, with jq's messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// Every message jq reports, in order, as it hands them to its error callback
    /// (which prints each followed by `\n`); the last one is `jq: N compile error(s)`.
    /// Messages start with `jq: `; the CLI swaps in its own name.
    pub messages: Vec<String>,
    /// The error count jq reports in the last message.
    pub nerrors: usize,
}

impl CompileError {
    /// Exactly what jq prints on stderr.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for m in &self.messages {
            s.push_str(m);
            s.push('\n');
        }
        s
    }
}

/// Port of `jq_compile_args(jq, str, args)`: parses `program` (up to its first NUL
/// byte, as jq reads a C string), links its modules, binds the builtins it uses, and
/// compiles it, applying jq's tail-call optimization.
pub fn jq_compile_args(
    program: &[u8],
    opts: &CompileOptions,
) -> Result<Rc<Bytecode>, CompileError> {
    let program = match memchr::memchr(0, program) {
        Some(nul) => &program[..nul],
        None => program,
    };
    let mut c = Compiler::new();
    let lf = c.add_locfile(Rc::new(LocFile::new("<top-level>", program)));
    let nerrors = match load_program(&mut c, &opts.attrs, lf) {
        Ok(block) => {
            let block = builtins_bind(&mut c, block);
            let mut globals = Globals {
                args: &opts.args,
                env: opts.env.clone(),
            };
            match c.block_compile(block, lf, &mut globals) {
                Ok(bc) => return Ok(bc),
                Err(n) => n,
            }
        }
        Err(n) => n,
    };
    let mut messages = std::mem::take(&mut c.messages);
    messages.push(compile_errors_summary(nerrors));
    Err(CompileError { messages, nerrors })
}
