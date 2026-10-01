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
//!
//! # qj's own stack
//!
//! The main thread's stack is as big as `ulimit -s` says, and at a small
//! enough limit qj's own frames used to run out where jq's don't: jq's
//! `jq -n 1` starts at 17 KB on macOS and 12 KB on Linux, qj needed more, and
//! a few programs further up (a regex nested 60 deep, a deep `tojson`) needed
//! up to twice jq's stack. So [`main`] does no more than jq's C start-up does
//! before `main` — restore `SIGPIPE`, capture the arguments, and in compat
//! mode check that jq's start-up would fit — and runs the rest with room to
//! spare (`qj::cli::stack::run`): on the main thread's stack as it is when
//! `RLIMIT_STACK` is 8 MB or more (the default on both platforms, and a
//! hundred times what qj's deepest recursion needs), and below that on a stack
//! of [`qj::cli::run::STACK_BYTES`] mapped for it, still on the main thread.
//! qj's own frames never run out where jq's don't, and only compat mode's
//! models of jq's stack, which read the limit, decide whether a run dies of
//! it (`src/compat.rs`).
//!
//! Nothing else changes: it is the same thread, with the same thread-local
//! state (the stdout buffer that models jq's stdio, the locale installed per
//! call with `uselocale`), signal mask and exit path. A signal raised on the
//! mapped stack — compat mode's `SIGSEGV`, an `abort()`'s `SIGABRT`, a
//! `SIGPIPE` — kills the process as it would anywhere, with the same wait
//! status and core dump (`qj::compat::small_core_dump`).

#![no_main]

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// The process entry point, called by the C runtime.
///
/// # Safety
///
/// `argv` must be the C runtime's array of `argc` NUL-terminated strings,
/// which is what the C runtime passes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(
    argc: std::os::raw::c_int,
    argv: *const *const u8,
) -> std::os::raw::c_int {
    // Restore default SIGPIPE behavior so piping to `head` etc. exits cleanly
    // instead of producing BrokenPipe errors. (Rust's runtime would set
    // SIG_IGN, but it is not running; some shells and launchers pass SIG_IGN
    // in, so set it explicitly.)
    #[cfg(unix)]
    // SAFETY: setting the disposition of one signal before any thread starts.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // QJ_JQ_COMPAT=1: on a stack too small for jq to start, die as it does.
    qj::compat::starting();

    // SAFETY: argc and argv are the C runtime's, valid for this call.
    let args = unsafe { command_line(argc, argv) };
    // qj is the jq 1.8.1 port: jq's main.c (src/cli/run.rs) on the ported
    // core (src/jq), reading input through src/io, on a stack of its own (see
    // the module docs). It exits the process itself; this returns only if it
    // panicked, which the default hook has reported, and 101 is the exit
    // status the runtime would give that. (A panic must not unwind out of an
    // `extern "C"` function: `run_on` and `catch_unwind` catch it.)
    qj::cli::stack::run(qj::cli::run::STACK_BYTES, move || {
        qj::cli::run::main_with(args)
    })
    .unwrap_or(101)
}

/// The command line as bytes.
///
/// `std::env::args_os` is the same list and handles every platform detail, so
/// it is what qj uses; it works without the runtime's start-up because std
/// captures `argv` in an `.init_array` entry on Linux and reads `_NSGetArgv`
/// on macOS. `argc`/`argv` are the fallback, for the case where some future
/// platform only fills the list in from `lang_start`.
///
/// # Safety
///
/// `argv` must be the C runtime's array of `argc` NUL-terminated strings.
unsafe fn command_line(argc: std::os::raw::c_int, argv: *const *const u8) -> Vec<Vec<u8>> {
    let from_std = qj::cli::args::argv_bytes();
    if !from_std.is_empty() || argc <= 0 || argv.is_null() {
        return from_std;
    }
    (0..argc as usize)
        .map(|i| {
            // SAFETY: the caller guarantees argc entries, each either null
            // or a valid NUL-terminated string.
            let p = unsafe { *argv.add(i) };
            if p.is_null() {
                return Vec::new();
            }
            unsafe { std::ffi::CStr::from_ptr(p.cast()) }
                .to_bytes()
                .to_vec()
        })
        .collect()
}
