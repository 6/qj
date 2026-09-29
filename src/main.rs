//! The `qj` binary: jq 1.8.1's `main.c` (`src/cli/run.rs`) on the ported core
//! (`src/jq`), reading input through `src/io`.
//!
//! # Why `#![no_main]`
//!
//! Rust's runtime start-up (`lang_start`) runs before `fn main`, and it
//! *sanitizes the standard file descriptors*: if 0, 1 or 2 are closed, it
//! reopens them on `/dev/null` so that a later `File` can never land on one.
//! jq, being C, sees them closed, so
//!
//! ```text
//! $ jq -n 1 >&-
//! jq: error: writing output failed: Bad file descriptor      (exit 2)
//! $ jq . <&-
//! jq: error: Bad file descriptor                             (exit 2)
//! ```
//!
//! while qj used to write to `/dev/null` and exit 0. Defining `main` as a C
//! function keeps std's start-up out of the way, so the descriptors are
//! exactly as the shell left them.
//!
//! What `lang_start` does besides that, and what this does instead:
//!
//! * **SIGPIPE.** It sets `SIG_IGN`; qj restores `SIG_DFL` anyway, as jq
//!   leaves it, so that `qj . | head` dies of `SIGPIPE` instead of reporting a
//!   broken pipe.
//! * **The stack-overflow handler** (a `SIGSEGV` handler on an alternate
//!   stack, printing "has overflowed its stack"). Not installed now, so a
//!   stack overflow is a plain `SIGSEGV` — which is what jq does, and what
//!   `QJ_JQ_COMPAT=1` reproduces deliberately ([`qj::compat`]).
//! * **Flushing at exit.** It flushes `std::io::Stdout`'s buffer. qj never
//!   writes through it: results go to `src/cli/run.rs`'s own buffer, which
//!   models jq's stdio and is flushed by `fclose(stdout)` at the end of the
//!   run, and messages go to unbuffered stderr.
//! * **Argument capture.** `std::env::args_os` works without it: on macOS it
//!   reads `_NSGetArgv`, and on Linux std captures `argv` from an
//!   `.init_array` entry that the C runtime calls before `main`.
//! * **The main thread's name**, used only in panic messages ("thread
//!   '<unnamed>' panicked" instead of "thread 'main' panicked").
//!
//! A panic must not unwind out of an `extern "C"` function, so [`main`]
//! catches it and exits 101, as the runtime would.

#![no_main]

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// The process entry point, called by the C runtime.
///
/// `argc`/`argv` are ignored: `qj::cli::run::main` takes the command line
/// from `std::env::args_os`, which is the same list.
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: std::os::raw::c_int, _argv: *const *const u8) -> std::os::raw::c_int {
    // Restore default SIGPIPE behavior so piping to `head` etc. exits cleanly
    // instead of producing BrokenPipe errors. (Rust's runtime would set
    // SIG_IGN, but it is not running; some shells and launchers pass SIG_IGN
    // in, so set it explicitly.)
    #[cfg(unix)]
    // SAFETY: setting the disposition of one signal before any thread starts.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // qj is the jq 1.8.1 port: jq's main.c (src/cli/run.rs) on the ported
    // core (src/jq), reading input through src/io. It exits itself; this
    // returns only if that ever changes.
    match std::panic::catch_unwind(qj::cli::run::main) {
        Ok(never) => never,
        // The runtime's exit status for a panic that reaches the top.
        Err(_) => 101,
    }
}
