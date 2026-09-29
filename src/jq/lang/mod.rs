//! jq language: port of `lexer.l`, `parser.y`, `compile.c`, `execute.c`, `linker.c`.
//!
//! # Front-end (Track P): source text → AST
//!
//! ```ignore
//! use qj::jq::lang::{parse, parse_program, NoHooks, ParseError, ParseHooks};
//! use qj::jq::lang::locfile::{LocFile, compile_errors_summary};
//!
//! match parse_program(".a | {b: .c}") {
//!     Ok(program) => { /* program: ast::Program */ }
//!     Err(errors) => {
//!         let locfile = LocFile::new("<top-level>", src);
//!         for e in &errors {
//!             eprintln!("{}", e.render(&locfile)); // jq: error: ... at <top-level>, line 1, column 4: ...
//!         }
//!         eprintln!("{}", compile_errors_summary(errors.len())); // plus compile errors, if any
//!     }
//! }
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
pub mod lexer;
pub mod locfile;
pub mod parser;
mod parser_tables;

pub use parser::{NoHooks, ParseError, ParseHooks, parse, parse_library, parse_program};
