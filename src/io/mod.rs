//! The input layer for the jq port: jq 1.8.1's input handling (`util.c`),
//! byte for byte, made fast with simdjson, memory mapping and parallel
//! processing of line-delimited records.
//!
//! The port's CLI uses it in `src/cli/input.rs` (`open_inputs`/`open_reader`
//! return an [`InputReader`] behind the CLI's `Reader` trait, opened with a
//! `CliOpener`) and `src/cli/run.rs` (`parallel_plan` decides whether a
//! program's records are independent, and `run_parallel` runs `main.c`'s
//! loop on [`parallel::run_with`], one compiled `Jq` per thread).
//!
//! # Public API (for the CLI)
//!
//! ## Reading inputs: [`InputReader`]
//!
//! A port of `jq_util_input_state` / `jq_util_input_next_input`:
//!
//! ```ignore
//! use qj::io::{InputReader, ReaderOptions, input_names};
//! let opts = ReaderOptions { raw, slurp, seq, stream, stream_errors };
//! let mut inputs = InputReader::new(input_names(&file_args), opts); // ["-"] if none
//! // main.c's loop:
//! while inputs.failures() == 0 {
//!     match inputs.next() {
//!         Some(Ok(value)) => { /* process(value) */ }
//!         Some(Err(e)) => { /* "jq: parse error: {e}" and stop, or with --seq
//!                              "jq: ignoring parse error: {e}" and go on */ }
//!         None => break,
//!     }
//! }
//! if inputs.failures() != 0 { /* exit status 2 (JQ_ERROR_SYSTEM) */ }
//! ```
//!
//! * `InputReader::new(files: Vec<OsString>, opts: ReaderOptions) -> InputReader`
//!   (`"-"` is stdin). Inputs are opened lazily, in order, when jq would
//!   open them, so `-n` programs that never call `input` never open (or
//!   fail on) any file.
//! * `InputReader::next(&mut self) -> Option<Result<Value, Error>>`:
//!   `jq_util_input_next_input`. `input`/`inputs` call it too; it keeps
//!   working after a parse error exactly like jq (the rest of the `fgets`
//!   chunk is discarded). With `-s` the slurped value comes at the end;
//!   `-R` gives lines, `-R -s` one string.
//! * `failures(&self) -> usize`: `jq_util_input_errors` (inputs that failed
//!   to open or read).
//! * `current_filename(&self) -> Value` (`input_filename`: `"<stdin>"` for
//!   `-`, `null` before the first input is opened; after a failed open it is
//!   that file's name), `current_line(&self) -> u64`
//!   (`input_line_number`), and `position(&self) -> String` (`<file>:<line>`
//!   or `<unknown>`, for `jq: error (at ...)` messages).
//! * `set_message_sink(&mut self, Box<dyn FnMut(InputMessage)>)`: where
//!   `Could not open file ...` and read-error messages go (default: stderr
//!   with the `qj` prefix); [`InputMessage::render`] formats them like jq.
//! * `with_opener(files, opts, Box<dyn Opener>)`: read from somewhere else
//!   than the file system ([`Opener`], [`Opened`]; [`MemoryOpener`] serves
//!   bytes from memory).
//! * `set_fast_path(false)` (or `QJ_NO_SIMD_INPUT=1`): everything through
//!   jq's parser port, for A/B checks. `stats()` tells how values were
//!   produced.
//! * `set_release_step(n)`: a memory-mapped input's pages are given back
//!   once nothing can read them anymore (see "Residency" below), in steps
//!   of `n` bytes (default 8 MB, `QJ_RELEASE_STEP`; 0 never, as
//!   `QJ_NO_RELEASE=1`).
//!
//! * [`SharedReader`]: the reader shared by the main loop and the VM, as
//!   jq's single input state is. It implements
//!   [`crate::jq::lang::execute::InputSource`], so `jq.set_input(Some(Box::new(shared.clone())))`
//!   makes `input`/`inputs`/`input_filename`/`input_line_number` work, and
//!   the main loop calls `shared.borrow_mut().next()` (don't hold the borrow
//!   while the program runs). `src/io/tests/vm.rs` has a complete `main.c`
//!   loop on the VM, checked against jq for programs using `input`.
//!
//! The default opener ([`FsOpener`]) memory-maps regular files (and stdin
//! when it is a regular file), streams pipes, FIFOs and devices (a
//! directory fails with `Is a directory` on its first read, as in jq), and
//! transparently decompresses `.gz`/`.gzip` (all members) and
//! `.zst`/`.zstd` (a qj extension; `input_filename` is the name given).
//! Streams are read incrementally: a record is returned as soon as its line
//! is complete (when jq's `fgets` would have it), and memory stays bounded
//! by the largest text plus the read size.
//!
//! ## Residency
//!
//! A memory-mapped file stays mapped whole (for the kernel's read-ahead),
//! but the reader releases it as it goes: the pages before the lowest
//! position anything can still read are made inaccessible and leave the
//! resident set ([`source`]'s module docs). That position is the reader's
//! own (it never reads behind it: jq's parser copies its chunks, values own
//! their bytes, a [`reader::TapeSink`] has printed what it printed) or the
//! start of a parallel job whose worker hasn't finished parsing it (the
//! engine's jobs pin their start). So for input of many texts, the resident
//! input is the engine's window plus a release step, whatever the file's
//! size; a single text is resident whole while it's read.
//!
//! ## Parallel processing: [`parallel::run`]
//!
//! ```ignore
//! let stats = qj::io::parallel::run(&mut inputs, &factory, &mut sink, &EngineOptions::default());
//! ```
//!
//! `main.c`'s loop with records processed on worker threads: implement
//! [`parallel::WorkerFactory`] (`Sync`; makes one [`parallel::RecordWorker`]
//! per thread, each compiling its own program) and [`parallel::RecordSink`]
//! (receives each record's stdout bytes, stderr bytes and status, and the
//! input's parse errors, in input order, on the calling thread; `Break`
//! stops). Each record's [`parallel::RecordMeta`] carries its
//! `input_filename` and `input_line_number`. `EngineOptions::threads: 0`
//! runs everything on the calling thread. The engine stops like `main.c`:
//! at the end of input, when the sink breaks, or before the next record once
//! [`InputReader::failures`] is non-zero.
//!
//! **Parallel is only safe when records are independent.** The CLI must
//! use `threads: 0` (or the reader directly) for `-n`, `-s`, `--seq`,
//! `--stream`, and programs using `input`/`inputs`, `halt`/`halt_error`,
//! `$__loc__`, `debug`/`stderr` (unless their output goes to the record's
//! `err` buffer, which keeps it ordered), `input_line_number`/
//! `input_filename` (unless answered from `RecordMeta`), or anything else
//! with state across records. See [`parallel`].
//!
//! A CLI worker compiles its own program (`jq_compile_args`, `Jq::new`),
//! runs `main.c`'s `process()` for each record, and gives the VM an
//! `InputSource` whose `current_filename`/`current_line` return the
//! record's `RecordMeta` (see `JqWorker` in `src/io/tests/vm.rs`, which
//! matches jq's stdout, stderr and exit status end to end).
//!
//! Worker threads get 256 MiB stacks by default (virtual; jq accepts
//! 10000-deep input and recurses on its 8 MB main stack); the calling
//! thread's stack is the caller's business.
//!
//! ## Single documents: [`simd::SimdParser`]
//!
//! `SimdParser::parse(&mut self, buf, start, end) -> Result<Value, Rejected>`
//! builds the value of one strict JSON document with simdjson, identical to
//! jq's parser for everything it accepts, and rejects the rest (use
//! [`crate::jq::value::parse_sized`] then). Useful for `fromjson`/`--argjson`
//! style callers that want speed and can fall back.
//!
//! ## Programs on the tape: [`tape`], [`tape_eval`]
//!
//! [`tape_eval::TapeProgram`] runs a simple program (paths, `.[]`, `length`,
//! `keys`, `type`, `map`, `{...}`, `select(... == c)`) on simdjson's tape, printing
//! what jq prints without building values ([`tape::Doc::print`] is a
//! canonicalizing printer), and declines anything else so the caller runs the
//! VM. [`InputReader::next_record`] and the engine's
//! [`parallel::WorkerFactory::new_tape`] take such a program; the CLI's
//! `tape_program` (`src/cli/run.rs`) decides when a run may use one.
//!
//! # How exactness is checked
//!
//! See `src/io/tests/`: track V's jq-recorded CLI parse cases under several
//! read patterns; a line-by-line port of `util.c` (with its 4096-byte
//! `fgets` buffer) as a differential oracle over generated adversarial
//! multi-file inputs; live comparisons with the jq binary; the engine
//! against the sequential reader; and real-pipe streaming tests.

#[doc(hidden)]
pub mod fuzzing;
pub mod parallel;
pub mod reader;
pub mod simd;
pub mod source;
pub mod tape;
pub mod tape_eval;

#[cfg(test)]
mod tests;

pub use reader::{InputReader, ReaderOptions, ReaderStats, SharedReader, input_names};
pub use source::{FsOpener, InputBytes, InputMessage, MemoryOpener, Opened, Opener};
