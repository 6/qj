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

/// The environment variable that turns compat mode on.
pub const ENV_VAR: &str = "QJ_JQ_COMPAT";

/// Whether [`ENV_VAR`] asks for exactly jq's behavior.
///
/// Set to anything but the empty string or `0`. Read once: the answer is a
/// property of the process, and has to be the same on every thread and in
/// every worker.
pub fn exactly_jq() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| is_on(std::env::var_os(ENV_VAR).as_deref()))
}

/// [`exactly_jq`]'s rule, for one value of the variable.
fn is_on(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
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

/// How much [`spin_forever`] allocates before it stops growing. Well above
/// any memory limit a caller is likely to impose (the differential harness
/// caps each process at 2 GB), and low enough not to drive the machine into
/// swap when nothing stops the process.
const SPIN_GROWTH_CAP: usize = 4 << 30;

/// Loops forever, holding on to `keep`, as jq does when it cannot make
/// progress.
///
/// jq's loops of this kind are not idle spins: `delpaths_sorted` appends the
/// key it failed to skip to an array on every turn, so jq's memory grows
/// without bound (about 1.2 GB/s on an M4) until something kills it.
/// Reproduce that, so that a memory limit ends both tools the same way —
/// but stop growing at [`SPIN_GROWTH_CAP`] and spin from there, which is the
/// one place this is deliberately gentler than jq.
pub fn spin_forever<T: Clone>(keep: &T) -> ! {
    let cap = SPIN_GROWTH_CAP / size_of::<T>().max(1);
    let mut grow: Vec<T> = Vec::new();
    while grow.len() < cap {
        grow.push(keep.clone());
    }
    loop {
        std::hint::spin_loop();
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
/// (measured on macOS/arm64, see [`free_frame_budget`]).
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const FREE_FRAME_BYTES: u64 = 64;

/// Bytes of the stack already in use when jq starts freeing a value, plus
/// the guard page it cannot touch (measured, see [`free_frame_budget`]).
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const FREE_RESERVED_BYTES: u64 = 9664;

/// On Linux/x86-64 (jq's release binary, built by gcc): 48 bytes a frame.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const FREE_FRAME_BYTES: u64 = 48;

/// On Linux/x86-64: what the start of the stack takes, plus the most the
/// kernel's randomization of the initial stack pointer can take (see
/// [`free_frame_budget`]).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const FREE_RESERVED_BYTES: u64 = 10240;

/// How many nested `jv_free` calls jq 1.8.1 can make before its stack
/// overflows: one per level of nesting of the value being freed, so this is
/// the value's nesting depth plus one for the scalar at the bottom.
///
/// On Linux/x86-64, jq's release binary uses 48 bytes a frame, and the
/// kernel starts the main thread's stack up to 8 KB below its top at random
/// (`arch_align_stack`), so the deepest value jq survives changes from run
/// to run, over about 170 levels. Measured 8 times per depth with
/// `reduce range($n) as $i (null; [.]) | length`, in jq_diff's environment:
///
/// | `ulimit -s` | jq survives every run to | and no run from | this model |
/// |---|---:|---:|---:|
/// | 1024 KB | 21,650 | 21,810 | 21,631 |
/// | 4096 KB | 87,170 | 87,330 | 87,167 |
/// | 8192 KB (the usual default) | 174,575 | 174,725 | 174,548 |
/// | 16384 KB (GitHub's runners) | 349,300 | 349,500 | 349,311 |
///
/// `(stack_bytes - 10240) / 48` takes the whole 8 KB, so, as on macOS, qj
/// never survives where jq dies; in the window, it dies where jq only
/// sometimes does. Other Linux targets use the macOS model, unmeasured.
///
/// Bisected against jq 1.8.1 on macOS/arm64 with
/// `reduce range($n) as $i (null; [.]) | length`, whose value needs `n + 1`
/// frames:
///
/// | `ulimit -s` | deepest `n` jq survives | this model |
/// |---|---:|---:|
/// | 1024 KB | 16,233 | 16,232 |
/// | 4096 KB | 65,385 | 65,384 |
/// | 8176 KB (this machine's default) | 130,664 | **130,664** |
/// | 16384 KB | 261,993 | 261,992 |
///
/// The threshold is linear in the stack size at 64 bytes a frame, and
/// `(stack_bytes - 9664) / 64` is exact at the default limit and one frame
/// short at the others: no single constant fits all four, because 8176 KB is
/// the one limit that isn't a whole number of 64 KB blocks and jq loses one
/// more frame there. Being exact at the default is what matters — it is the
/// limit the differential harness runs under — and erring short means qj
/// never survives where jq dies.
///
/// Two further reasons the two can't agree to the last frame, both jq's:
///
/// * which operation frees the value shifts the threshold by a frame or two
///   (130,664 through a builtin such as `length`, 130,667 from `main.c`'s
///   output path, 130,661 through `tojson`); the model follows the first;
/// * argv and the environment sit on top of the stack, so jq's threshold
///   moves with them, about one frame per 64 bytes (130,664 with this
///   shell's environment, 130,762 under `env -i`). qj's doesn't.
///
/// `None` when the stack is unlimited, where jq doesn't overflow either.
pub fn free_frame_budget() -> Option<u64> {
    let bytes = stack_limit_bytes()?;
    Some(bytes.saturating_sub(FREE_RESERVED_BYTES) / FREE_FRAME_BYTES)
}

/// Bytes per frame and reserved bytes for `jv_equal`/`jv_cmp`, which recurse
/// with a larger frame than `jv_free`. Bisected the same way with
/// `[reduce range($n) as $i (null;[.])] == [reduce range($n) as $i (null;[.])]`:
/// jq survives 8,115 at 1024 KB, 32,691 at 4096 KB and 65,330 at 8176 KB, so
/// `(stack_bytes - 9856) / 128`, again exact at the default limit and one
/// frame short at the others.
///
/// qj does not emulate this one: its comparison walks iteratively and stops
/// at the first difference, so there is no faithful place to put the check
/// without restructuring the comparison itself. Programs that make jq
/// overflow while comparing still crash qj when the values are freed, but
/// only past [`free_frame_budget`]. See `docs/COMPATIBILITY.md`.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub const COMPARE_MODEL: (u64, u64) = (128, 9856);

/// On Linux/x86-64, `jv_equal` uses 144 bytes a frame: one bisection per
/// limit found 7,249 at 1024 KB, 29,082 at 4096 KB, 58,210 at 8192 KB and
/// 116,449 at 16384 KB, each somewhere in a window of about 60 levels that
/// the randomized start of the stack moves it in (see [`free_frame_budget`]).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const COMPARE_MODEL: (u64, u64) = (144, 10240);

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
    let Some(budget) = free_frame_budget() else {
        return;
    };
    if native_frames + free_frames(items) > budget {
        die_of_stack_overflow();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_is_on_unless_it_is_empty_or_zero() {
        let on = |v: &str| is_on(Some(std::ffi::OsStr::new(v)));
        assert!(on("1"));
        assert!(on("yes"));
        assert!(on("00"));
        assert!(!on(""));
        assert!(!on("0"));
        assert!(!is_on(None));
    }

    /// The models above, against the depths bisected from jq 1.8.1. The
    /// budget is in frames, and `reduce range(n) as $i (null;[.])` needs
    /// `n + 1` of them, so the deepest `n` the model survives is
    /// `budget - 1`: exact at the default stack limit, one short at the
    /// others.
    #[test]
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    fn stack_models_match_the_measurements() {
        let free = |kb: u64| (kb * 1024 - FREE_RESERVED_BYTES) / FREE_FRAME_BYTES - 1;
        assert_eq!(free(8176), 130_664); // jq: 130,664
        assert_eq!(free(1024), 16_232); // jq: 16,233
        assert_eq!(free(4096), 65_384); // jq: 65,385
        assert_eq!(free(16384), 261_992); // jq: 261,993
        let (frame, reserved) = COMPARE_MODEL;
        let cmp = |kb: u64| (kb * 1024 - reserved) / frame - 1;
        assert_eq!(cmp(8176), 65_330); // jq: 65,330
        assert_eq!(cmp(1024), 8_114); // jq: 8,115
        assert_eq!(cmp(4096), 32_690); // jq: 32,691
    }

    /// On Linux/x86-64 jq's threshold moves between runs (see
    /// [`free_frame_budget`]): the model stays at or below the depth every
    /// measured run survived, by less than the width of the window.
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn stack_models_match_the_measurements() {
        let free = |kb: u64| (kb * 1024 - FREE_RESERVED_BYTES) / FREE_FRAME_BYTES - 1;
        // (ulimit -s, jq survived every run to, and no run from)
        for (kb, always, never) in [
            (1024, 21_650, 21_810),
            (4096, 87_170, 87_330),
            (8192, 174_575, 174_725),
            (16384, 349_300, 349_500),
        ] {
            let deepest = free(kb);
            assert!(deepest <= always + 25, "{kb} KB: {deepest} vs {always}");
            assert!(never - deepest <= 200, "{kb} KB: {deepest} vs {never}");
        }
        let (frame, reserved) = COMPARE_MODEL;
        let cmp = |kb: u64| (kb * 1024 - reserved) / frame - 1;
        // One run each, somewhere in a window of about 60 levels.
        for (kb, jq) in [
            (1024, 7_249),
            (4096, 29_082),
            (8192, 58_210),
            (16384, 116_449),
        ] {
            assert!(
                cmp(kb) <= jq && jq - cmp(kb) <= 60,
                "{kb} KB: {} vs {jq}",
                cmp(kb)
            );
        }
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
    fn a_limited_stack_gives_a_frame_budget() {
        // The test binary's stack is the shell's, which is never unlimited
        // on macOS; on Linux CI it could be, so accept either answer.
        match free_frame_budget() {
            Some(d) => assert!(d > 1000, "implausible depth limit {d}"),
            None => assert!(stack_limit_bytes().is_none()),
        }
    }
}
