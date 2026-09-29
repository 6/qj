//! `QJ_JQ_COMPAT=1`: be exactly jq 1.8.1, bugs included.
//!
//! qj is a faithful port of jq 1.8.1, but it deliberately differs in a few
//! places where jq crashes, hangs, or has nothing at all:
//!
//! * where jq 1.8.1 dies or never finishes, qj returns a sane result
//!   (`docs/COMPATIBILITY.md`, "Exemptions");
//! * qj adds glob expansion of file arguments, transparent `.gz`/`.zst`
//!   decompression, and the `--threads`, `--jsonl` and `--debug-timing`
//!   options.
//!
//! With `QJ_JQ_COMPAT=1` both go away: qj reproduces jq's crashes and hangs,
//! and rejects its own extensions the way jq rejects an unknown option. The
//! variable is read once, from the environment, so every thread and every
//! worker sees the same answer.
//!
//! qj's help, version and build-configuration text and the `qj:` name in
//! messages stay qj's own in both modes; `docs/JQ_PORT_PLAN.md` exempts them
//! by policy.
//!
//! # Stack overflow
//!
//! jq frees, compares and copies values by recursing in C, so a value nested
//! deep enough overflows its stack and the process dies of `SIGSEGV`. qj's
//! value layer does the same work iteratively and has no such limit, so in
//! compat mode it measures the nesting depth where jq would have recursed and
//! raises the same signal at the same depth ([`free_depth_limit`]).

use std::os::raw::c_int;
use std::sync::OnceLock;

/// Whether `QJ_JQ_COMPAT` asks for exactly jq's behavior.
///
/// Set to anything but the empty string or `0`. Read once: the value is a
/// property of the process.
pub fn exactly_jq() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("QJ_JQ_COMPAT").is_some_and(|v| !v.is_empty() && v != "0"))
}

/// Dies of `sig` exactly as an unhandled fatal signal would, without running
/// any cleanup: no destructors, no `atexit` handlers, and in particular no
/// flush of the buffered stdout, which is what jq loses when it crashes.
pub fn die_by_signal(sig: c_int) -> ! {
    // SAFETY: restoring the default disposition of a signal and raising it.
    // The default action for SIGSEGV and SIGABRT terminates the process, so
    // `raise` does not return; `_exit` is there for the impossible case of a
    // blocked or unresettable signal, and never runs any cleanup either.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
        libc::_exit(128 + sig)
    }
}

/// jq's death from a C stack overflow: `SIGSEGV`, with whatever it had
/// buffered on stdout lost.
pub fn die_of_stack_overflow() -> ! {
    die_by_signal(libc::SIGSEGV)
}

/// Loops forever, holding on to `keep`, as jq does when it cannot make
/// progress.
///
/// jq's loops of this kind are not idle spins: `delpaths_sorted` appends the
/// key it failed to skip to an array on every turn, so jq's memory grows
/// without bound (about 1.2 GB/s on an M4) until the process is killed.
/// Reproduce that, so that a memory limit ends both tools the same way.
pub fn spin_forever<T: Clone>(keep: &T) -> ! {
    let mut grow: Vec<T> = Vec::new();
    loop {
        grow.push(keep.clone());
    }
}

// ---------------------------------------------------------------------------
// jq's C stack
// ---------------------------------------------------------------------------

/// `getrlimit(RLIMIT_STACK)`: the size of the main thread's stack, which is
/// what `ulimit -s` sets and what limits jq's recursion. `None` when it is
/// unlimited or unreadable.
fn stack_limit_bytes() -> Option<u64> {
    static BYTES: OnceLock<Option<u64>> = OnceLock::new();
    *BYTES.get_or_init(|| {
        // SAFETY: getrlimit writes an rlimit into a valid out-pointer.
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut lim) } != 0 {
            return None;
        }
        // `rlim_t` is `u64` on macOS and Linux.
        let cur: u64 = lim.rlim_cur;
        if cur == libc::RLIM_INFINITY || cur == 0 {
            None
        } else {
            Some(cur)
        }
    })
}

/// Bytes of stack one `jv_free` frame uses on the way down a nested value
/// (measured on macOS/arm64, see [`free_depth_limit`]).
const FREE_FRAME_BYTES: u64 = 64;

/// Bytes of the stack already in use when jq starts freeing a value, plus
/// the guard page it cannot touch (measured, see [`free_depth_limit`]).
const FREE_RESERVED_BYTES: u64 = 9728;

/// The deepest value jq 1.8.1 can free without overflowing its stack.
///
/// jq's `jv_free` recurses once per level of nesting, so the limit is set by
/// `ulimit -s`. Bisected against jq 1.8.1 on macOS/arm64, freeing
/// `reduce range($n) as $i (null; [.]) | length`:
///
/// | `ulimit -s` | deepest value jq frees | this model |
/// |---|---:|---:|
/// | 1024 KB | 16,233 | 16,232 |
/// | 4096 KB | 65,385 | 65,384 |
/// | 8176 KB (this machine's default) | 130,664 | **130,664** |
/// | 16384 KB | 261,993 | 261,992 |
///
/// so the threshold is linear in the stack size at 64 bytes a level, and
/// `(stack_bytes - 9728) / 64` is exact at the default limit and one level
/// early at the others — no single constant fits all four, because 8176 KB
/// is the one limit that isn't a whole number of 64 KB blocks and jq loses
/// one more frame there. Matching the default exactly is what matters: it is
/// the limit the differential harness runs under.
///
/// Which operation frees the value also shifts the threshold by a frame or
/// two (130,664 from a builtin such as `length`, 130,667 from `main.c`'s
/// output path, 130,661 through `tojson`); the model follows the first.
///
/// `None` when the stack is unlimited, where jq doesn't crash either.
pub fn free_depth_limit() -> Option<u64> {
    let bytes = stack_limit_bytes()?;
    Some(bytes.saturating_sub(FREE_RESERVED_BYTES) / FREE_FRAME_BYTES)
}

/// Bytes per level and reserved bytes for `jv_equal`/`jv_cmp`, which recurse
/// with a larger frame than `jv_free`. Bisected the same way with
/// `[reduce range($n) as $i (null;[.])] == [reduce range($n) as $i (null;[.])]`:
/// 8,115 at 1024 KB, 32,691 at 4096 KB and 65,330 at 8176 KB, i.e.
/// `(stack_bytes - 9984) / 128` (exact at the default limit, one level early
/// at the other two, as above).
///
/// qj does not emulate this one: its comparison walks iteratively and stops
/// at the first difference, so there is no faithful place to put the check
/// without restructuring the comparison itself. Programs that make jq
/// overflow while comparing still crash qj when the values are freed, but
/// only past [`free_depth_limit`]. See `docs/COMPATIBILITY.md`.
pub const COMPARE_MODEL: (u64, u64) = (128, 9984);

/// The nesting depth of the deepest value in `items`, counted as jq counts
/// `jv_free` frames: one for the value itself, plus the deepest child.
///
/// Walks iteratively, so it cannot overflow the stack itself. Only called in
/// compat mode, and only for values already too deep for qj's own native
/// recursion, so it never runs on ordinary data.
fn free_frames(items: &[crate::jq::value::Value]) -> u64 {
    use crate::jq::value::Value;
    // (value, frames already counted above it)
    let mut work: Vec<(&Value, u64)> = items.iter().map(|v| (v, 1)).collect();
    let mut deepest = 0;
    while let Some((v, above)) = work.pop() {
        deepest = deepest.max(above);
        match v {
            Value::Array(a) => work.extend(a.iter().map(|c| (c, above + 1))),
            Value::Object(o) => work.extend(o.iter().map(|(_, c)| (c, above + 1))),
            _ => {}
        }
    }
    deepest
}

/// Hook for `src/jq/value/array.rs`: `items` are the contents of a container
/// `native_frames` levels below the value being freed, about to be freed
/// iteratively because they are too deeply nested for native recursion.
///
/// jq would have recursed all the way down, so in compat mode this checks
/// whether the whole chain fits in jq's stack and dies of `SIGSEGV` if it
/// doesn't, before any of the value is freed and with jq's buffered output
/// lost.
pub fn freeing_iteratively(items: &[crate::jq::value::Value], native_frames: u64) {
    if !exactly_jq() {
        return;
    }
    let Some(limit) = free_depth_limit() else {
        return;
    };
    if native_frames + free_frames(items) > limit {
        die_of_stack_overflow();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The models above, against the depths bisected from jq 1.8.1: exact at
    /// the default stack limit, one level early at the others.
    #[test]
    fn stack_models_match_the_measurements() {
        let free = |kb: u64| (kb * 1024 - FREE_RESERVED_BYTES) / FREE_FRAME_BYTES;
        assert_eq!(free(8176), 130_664); // jq: 130,664
        assert_eq!(free(1024), 16_232); // jq: 16,233
        assert_eq!(free(4096), 65_384); // jq: 65,385
        assert_eq!(free(16384), 261_992); // jq: 261,993
        let (frame, reserved) = COMPARE_MODEL;
        let cmp = |kb: u64| (kb * 1024 - reserved) / frame;
        assert_eq!(cmp(8176), 65_330); // jq: 65,330
        assert_eq!(cmp(1024), 8_114); // jq: 8,115
        assert_eq!(cmp(4096), 32_690); // jq: 32,691
    }

    /// The frame count is jq's `jv_free` recursion depth, and the walk is
    /// iterative so it survives values deeper than the stack.
    #[test]
    fn free_frames_counts_nesting() {
        use crate::jq::value::parse_sized;
        let d = |text: &str| {
            let v = parse_sized(text.as_bytes()).expect("valid JSON");
            free_frames(std::slice::from_ref(&v))
        };
        assert_eq!(d("1"), 1);
        assert_eq!(d("[]"), 1);
        assert_eq!(d("[1]"), 2);
        assert_eq!(d("[[1]]"), 3);
        assert_eq!(d(r#"{"a":[1]}"#), 3);
        // The deepest branch wins, whichever side it is on.
        assert_eq!(d("[[[1]],2]"), 4);
        assert_eq!(d("[2,[[1]]]"), 4);
        let deep = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(d(&deep), 5001);
    }

    #[test]
    fn a_limited_stack_gives_a_depth_limit() {
        // The test binary's stack is the shell's, which is never unlimited
        // on macOS; on Linux CI it could be, so accept either answer.
        match free_depth_limit() {
            Some(d) => assert!(d > 1000, "implausible depth limit {d}"),
            None => assert!(stack_limit_bytes().is_none()),
        }
    }
}
