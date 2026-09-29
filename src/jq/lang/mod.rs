//! jq language: port of `lexer.l`, `parser.y`, `compile.c`, `execute.c`, `linker.c`.
//!
//! # Front-end (Track P): source text → AST
//!
//! ```
//! use qj::jq::lang::locfile::{LocFile, compile_errors_summary};
//! use qj::jq::lang::{NoHooks, parse, parse_program};
//!
//! let program = parse_program(".a | {b: .c}").unwrap();
//! assert_eq!(program.to_sexpr(), r#"(| (index . "a") (object ("b" (index . "c"))))"#);
//!
//! // What jq prints for a program that doesn't parse:
//! let src = b".a b";
//! let errors = parse(src, &mut NoHooks).unwrap_err();
//! let locfile = LocFile::new("<top-level>", src);
//! let mut stderr = String::new();
//! for e in &errors {
//!     stderr += &e.render(&locfile);
//!     stderr += "\n";
//! }
//! stderr += &compile_errors_summary(errors.len()); // after any compile errors
//! assert_eq!(
//!     stderr,
//!     "jq: error: syntax error, unexpected IDENT, expecting end of file \
//!      at <top-level>, line 1, column 4:\n    .a b\n       ^\njq: 1 compile error"
//! );
//! ```
//!
//! * [`parse`]`(src: &[u8], hooks: &mut dyn ParseHooks) -> Result<ast::Program, Vec<ParseError>>`
//!   is jq's `jq_parse`: bison's LALR tables and skeleton (exact accept/reject,
//!   precedence, error recovery and messages) with parser.y's actions building an
//!   [`ast::Program`]. [`parse_program`] is the same with [`NoHooks`], and
//!   [`parse_library`] is `jq_parse_library` (builtin.jq and modules: definitions only).
//! * [`ParseHooks`] lets the compiler supply the parse-time checks that need constant
//!   folding (`check_object_key`, module metadata), so those errors interleave with
//!   syntax errors exactly as in jq.
//! * [`ast`] documents the tree; every node names the parser.y rule it comes from.
//! * [`locfile::LocFile`] (port of `locfile.c`) formats errors with line, column and
//!   carets, and maps byte offsets to lines for `$__loc__`.
//! * [`lexer`] is the scanner (port of `lexer.l`), including jq's string-escape
//!   decoding and UTF-8 replacement rules.

pub mod ast;
pub mod bytecode;
pub mod compile;
pub mod lexer;
pub mod linker;
pub mod locfile;
pub mod lower;
pub mod parser;
mod parser_tables;
pub mod program;

pub use parser::{
    NUM_RULES, NoHooks, ParseError, ParseHooks, parse, parse_library, parse_program, reductions,
    rule_name,
};
pub use program::{CompileError, CompileOptions, jq_compile_args};
