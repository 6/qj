//! The input layer for the jq port: jq 1.8.1's input handling (`util.c`),
//! byte for byte, made fast with simdjson, memory mapping and parallel
//! processing of line-delimited records.
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
//! # How exactness is checked
//!
//! See `src/io/tests/`: track V's jq-recorded CLI parse cases under several
//! read patterns; a line-by-line port of `util.c` (with its 4096-byte
//! `fgets` buffer) as a differential oracle over generated adversarial
//! multi-file inputs; live comparisons with the jq binary; the engine
//! against the sequential reader; and real-pipe streaming tests.

pub mod parallel;
pub mod reader;
pub mod simd;
pub mod source;

#[cfg(test)]
mod tests;

pub use reader::{InputReader, ReaderOptions, ReaderStats, input_names};
pub use source::{FsOpener, InputMessage, MemoryOpener, Opened, Opener};
