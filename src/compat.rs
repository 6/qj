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
//! jq frees, compares, merges and walks paths through values by recursing in
//! C, so a value or path deep enough overflows its stack and the process dies
//! of `SIGSEGV`. qj's value layer does all of it iteratively and has no such
//! limit, so in compat mode it measures the depth jq's recursion would have
//! reached and raises the same signal at the same point in the program.
//!
//! [`Site`] lists the recursions, with the bytes a level and the bytes
//! reserved measured for each; `docs/COMPATIBILITY.md` has the tables and the
//! sites that cannot overflow at all.

mod depth;

use std::os::raw::c_int;
use std::sync::OnceLock;

use crate::jq::value::{Object, Value};

/// The environment variable that turns compat mode on.
pub const ENV_VAR: &str = "QJ_JQ_COMPAT";

/// Whether [`ENV_VAR`] asks for exactly jq's behavior.
///
/// Set to anything but the empty string or `0`. Read once: the answer is a
/// property of the process, and has to be the same on every thread and in
/// every worker.
///
/// Inlined, because the hooks below call it on every comparison of two values
/// and it is all a run that isn't compat mode does for them.
#[inline]
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
    no_core_dump();
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

/// Before qj dies on purpose, of the signal jq dies of: tells a Linux kernel
/// not to dump its core.
///
/// The crash reproduces jq's, which is the signal and the lost output; a core
/// would be of qj doing that, not of anything wrong, and a big one. mimalloc
/// reserves about 1 GB of address space up front, and a core dump handler
/// such as systemd-coredump or apport reads all of it through a pipe, which
/// took about 1.5 s a crash on GitHub's runners (jq's cores take 50 ms), and
/// is where a pipe handler ignores `ulimit -c`. Elsewhere core dumps are off
/// unless `ulimit -c` asks for them, and nothing changes.
pub fn no_core_dump() {
    // SAFETY: prctl with PR_SET_DUMPABLE only changes this process's flag.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
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

/// A recursion in jq 1.8.1 that a deep enough value or path can drive past
/// its C stack, killing the process with `SIGSEGV`.
///
/// Every one of them is a loop with an explicit stack in qj, so in compat
/// mode qj measures the depth jq's recursion would have reached and dies
/// where jq does. The model of each is a number of bytes a level and a
/// number of bytes reserved ([`Site::model`]), bisected against the jq
/// binary; `docs/COMPATIBILITY.md` lists them, and the recursions that
/// cannot overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    /// `jv_free` → `jvp_array_free`/`jvp_object_free`: value nesting.
    Free,
    /// `jv_equal` → `jvp_array_equal`/`jvp_object_equal`, and `jv_cmp`:
    /// the nesting the comparison reaches before the first difference.
    /// One model for both, at the smaller (`jv_cmp`'s) depth.
    Compare,
    /// `jv_contains` → `jvp_array_contains`/`jvp_object_contains`.
    Contains,
    /// `jv_object_merge_recursive` (object `*`): the nesting shared by both
    /// operands.
    Merge,
    /// `jv_setpath` (`setpath`, `=`, `|=`, …): the length of the path.
    Setpath,
    /// `delpaths_sorted` (`delpaths`, `del`): the length of the paths, which
    /// jq walks a level at a time in groups.
    Delpaths,
}

impl Site {
    /// The bytes of stack a level of this recursion costs, and the bytes
    /// that are gone before it starts.
    ///
    /// The frame bytes are exact: the deepest value jq survives is linear in
    /// `ulimit -s` with this slope, over every limit measured (1 MB to 16 MB).
    /// The reserved bytes are what is left over, at the *worst* of the call
    /// sites measured, plus a margin ([`STACK_MARGIN`]).
    const fn model(self) -> (u64, u64) {
        // macOS/arm64 (jq's release binary, built by Apple clang), bisected
        // with the programs in `docs/COMPATIBILITY.md` at `ulimit -s` 1024,
        // 2048, 4096, 8176 and 16384 KB. `reserved` is
        // `stack - (deepest + 1) * frame` at the worst call site, rounded up,
        // plus STACK_MARGIN.
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let (frame, measured) = match self {
            Site::Free => (64, 3520),
            Site::Compare => (128, 4096),
            Site::Contains => (176, 3840),
            Site::Merge => (112, 3616),
            Site::Setpath => (144, 3808),
            Site::Delpaths => (240, 3664),
        };
        // Linux/x86-64 (jq's release binary, built by gcc), bisected the same
        // way at 1024, 4096, 8192 and 16384 KB with the stack randomization
        // off, so that the thresholds are deterministic; STACK_MARGIN then
        // covers the randomization.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let (frame, measured) = match self {
            Site::Free => (48, 1856),
            Site::Compare => (144, 3856),
            Site::Contains => (176, 2128),
            Site::Merge => (128, 2304),
            Site::Setpath => (160, 2176),
            Site::Delpaths => (240, 2224),
        };
        (frame, measured + STACK_MARGIN)
    }

    /// How deep jq's recursion can go here before its stack runs out, in the
    /// levels [`depth`] counts. `None` when the stack is unlimited, where jq
    /// doesn't overflow either.
    pub fn frame_budget(self) -> Option<u64> {
        let bytes = stack_limit_bytes()?;
        let (frame, reserved) = self.model();
        Some(bytes.saturating_sub(reserved) / frame)
    }
}

/// Bytes held back from every site's budget, on top of what the bisections
/// measured, so that qj dies no later than jq would.
///
/// On macOS it covers the environment: argv and the environment sit on top of
/// jq's stack, so its threshold drops by about a level per 64 bytes of them
/// (a `jv_free` threshold of 130,760 in the harness's five variables, 130,664
/// in an interactive shell — 6 KB more). The bisections use the harness's
/// environment, so the margin is what a larger one can take.
///
/// On Linux it covers the kernel's randomization of the initial stack
/// pointer (`arch_align_stack`), which is up to 8 KB and moves jq's threshold
/// from run to run; the bisections switch it off (`setarch -R`). The
/// environment is not covered there as well: jq's window already moves by
/// more than a large environment costs.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const STACK_MARGIN: u64 = 6144;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const STACK_MARGIN: u64 = 8384;

/// How many nested `jv_free` calls jq 1.8.1 can make before its stack
/// overflows: one per level of nesting of the value being freed, so this is
/// the value's nesting depth plus one for the scalar at the bottom.
///
/// On macOS/arm64 it is 64 bytes a frame. Bisected against jq 1.8.1 with
/// `reduce range($n) as $i (null; [.]) | length`, whose value needs `n + 1`
/// frames, in the differential harness's environment:
///
/// | `ulimit -s` | deepest `n` jq survives | this model |
/// |---|---:|---:|
/// | 1024 KB | 16,328 | 16,232 |
/// | 4096 KB | 65,480 | 65,384 |
/// | 8176 KB (this machine's default) | 130,760 | 130,664 |
/// | 16384 KB | 262,088 | 261,992 |
///
/// The threshold is exactly linear in the stack size, and the model sits
/// [`STACK_MARGIN`] below it — 96 levels here — because argv and the
/// environment sit on top of jq's stack and take about a level per 64 bytes:
/// with an interactive shell's environment jq survives 130,664 at the default
/// limit rather than 130,760. Erring short means qj never survives where jq
/// dies.
///
/// Which operation frees the value shifts jq's threshold by a frame or two
/// as well (130,760 through a builtin such as `length`, 130,763 from
/// `main.c`'s output path, 130,757 through `tojson`); the model follows the
/// first.
///
/// On Linux/x86-64, jq's release binary uses 48 bytes a frame, and the kernel
/// starts the main thread's stack up to 8 KB below its top at random
/// (`arch_align_stack`), so the deepest value jq survives moves from run to
/// run, over about 170 levels. With the randomization off (`setarch -R`) the
/// threshold is 21,806 at 1024 KB, 87,342 at 4096 KB, 174,723 at 8192 KB and
/// 349,486 at 16384 KB; sampling 10 runs per depth with it on, jq survived
/// every run up to about 160 levels below that and no run above it. The model
/// takes the whole 8 KB, so it lands at or below the bottom of that window
/// (21,631 / 87,167 / 174,548 / 349,311): qj never survives where jq dies,
/// and inside the window it dies where jq only sometimes does.
///
/// Other targets use the macOS model, unmeasured.
///
/// `None` when the stack is unlimited, where jq doesn't overflow either.
pub fn free_frame_budget() -> Option<u64> {
    Site::Free.frame_budget()
}

/// The nesting depth of the deepest value in `items`, counted as jq counts
/// `jv_free` frames: one for the value itself, plus the deepest child.
///
/// Walks iteratively, so it cannot overflow the stack itself. Only called in
/// compat mode, and only for values already too deep for qj's own native
/// recursion, so it never runs on ordinary data.
fn free_frames(items: &[Value]) -> u64 {
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
pub fn freeing_iteratively(items: &[Value], native_frames: u64) {
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

// ---------------------------------------------------------------------------
// The hooks: jq's other recursions
// ---------------------------------------------------------------------------

/// The budget of `site`, or `None` when nothing can overflow (compat mode is
/// off, or the stack is unlimited).
///
/// Inlined into every hook, so that a run that isn't compat mode does one
/// load of the cached flag and nothing else.
#[inline]
fn budget_of(site: Site) -> Option<u64> {
    if !exactly_jq() {
        return None;
    }
    site.frame_budget()
}

/// Hook for `src/jq/value/deep.rs`: `jv_equal(a, b)`.
///
/// jq recurses into the values a level at a time and stops at the first
/// difference, so this dies only if the comparison jq would make reaches
/// deeper than its stack allows.
pub fn comparing_equal(a: &Value, b: &Value) {
    if let Some(budget) = budget_of(Site::Compare)
        && depth::equal(a, b, budget) > budget
    {
        die_of_stack_overflow();
    }
}

/// Hook for `src/jq/value/deep.rs`: `jv_cmp(a, b)`.
///
/// `jv_cmp` shares [`Site::Compare`]'s model: it recurses with the same frame
/// and, on the call sites measured, slightly deeper (an object compares its
/// sorted key arrays before its values), which is the depth the model is
/// calibrated to.
pub fn comparing_order(a: &Value, b: &Value) {
    if let Some(budget) = budget_of(Site::Compare)
        && depth::compare(a, b, budget) > budget
    {
        die_of_stack_overflow();
    }
}

/// Hook for `src/jq/value/deep.rs`: `jv_contains(a, b)`.
pub fn containing(a: &Value, b: &Value) {
    if let Some(budget) = budget_of(Site::Contains)
        && depth::contains(a, b, budget) > budget
    {
        die_of_stack_overflow();
    }
}

/// Hook for `src/jq/value/object.rs`: `jv_object_merge_recursive(a, b)`.
pub fn merging(a: &Object, b: &Object) {
    if let Some(budget) = budget_of(Site::Merge)
        && depth::merge(a, b, budget) > budget
    {
        die_of_stack_overflow();
    }
}

/// A recursion qj walks with a loop, where jq recurses once per level: the
/// budget, if compat mode can ever make it run out.
///
/// The caller counts its levels from 1, as the measurements do, and calls
/// [`Descent::level`] before each one. `None` costs nothing per level.
pub struct Descent(Option<u64>);

impl Descent {
    /// The budget of `site` in compat mode, else nothing.
    #[inline]
    pub fn new(site: Site) -> Descent {
        Descent(budget_of(site))
    }

    /// jq is about to enter frame number `level` of this recursion: die if
    /// its stack has run out.
    #[inline]
    pub fn level(&self, level: u64) {
        if self.0.is_some_and(|budget| level > budget) {
            die_of_stack_overflow();
        }
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

    /// The deepest value each model survives at `kb` KB of stack: the budget
    /// is in frames, and the programs the bisections used need `n + 1` of
    /// them for a value nested `n` deep, so it is `budget - 1`.
    fn deepest(site: Site, kb: u64) -> u64 {
        let (frame, reserved) = site.model();
        (kb * 1024 - reserved) / frame - 1
    }

    /// Every site's model against the depths bisected from the jq 1.8.1
    /// binary on macOS/arm64, at the worst of the call sites measured (see
    /// `docs/COMPATIBILITY.md` for the programs and the full tables).
    ///
    /// The model is [`STACK_MARGIN`] short of jq, which is what keeps qj from
    /// surviving where jq dies, so each row checks that it is short and by
    /// how little.
    #[test]
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    fn stack_models_match_the_measurements() {
        // (site, bytes a level, deepest n jq survives at 1024, 2048, 4096,
        //  8176 and 16384 KB)
        let jq: &[(Site, u64, [u64; 5])] = &[
            (Site::Free, 64, [16_328, 32_712, 65_480, 130_760, 262_088]),
            (Site::Compare, 128, [8_159, 16_351, 32_735, 65_375, 131_039]),
            (Site::Contains, 176, [5_935, 11_893, 23_809, 47_547, 95_303]),
            (Site::Merge, 112, [9_329, 18_692, 37_416, 74_719, 149_764]),
            (Site::Setpath, 144, [7_255, 14_537, 29_101, 58_114, 116_481]),
            (Site::Delpaths, 240, [4_353, 8_722, 17_460, 34_868, 69_889]),
        ];
        for &(site, frame, measured) in jq {
            assert_eq!(site.model().0, frame, "{site:?}: bytes a level");
            for (kb, jq) in [1024, 2048, 4096, 8176, 16384].iter().zip(measured) {
                let model = deepest(site, *kb);
                assert!(model <= jq, "{site:?} at {kb} KB: {model} > jq's {jq}");
                // Only the margin, and the rounding of one level, apart.
                let short = (jq - model) * frame;
                assert!(
                    short >= STACK_MARGIN && short < STACK_MARGIN + 2 * frame,
                    "{site:?} at {kb} KB: {model} is {short} B below jq's {jq}"
                );
            }
        }
    }

    /// On Linux/x86-64 the kernel's randomization of the initial stack
    /// pointer moves jq's threshold from run to run (see
    /// [`free_frame_budget`]), so the bisections switch it off and the models
    /// sit a whole 8 KB below them: at or under the bottom of jq's window.
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn stack_models_match_the_measurements() {
        // (site, bytes a level, deepest n jq survives at 1024, 4096, 8192 and
        //  16384 KB with `setarch -R`)
        let jq: &[(Site, u64, [u64; 4])] = &[
            (Site::Free, 48, [21_806, 87_342, 174_723, 349_486]),
            (Site::Compare, 144, [7_254, 29_100, 58_227, 116_481]),
            (Site::Contains, 176, [5_945, 23_819, 47_650, 95_312]),
            (Site::Merge, 128, [8_173, 32_749, 65_517, 131_053]),
            (Site::Setpath, 160, [6_539, 26_200, 52_415, 104_843]),
            (Site::Delpaths, 240, [4_359, 17_466, 34_943, 69_895]),
        ];
        for &(site, frame, measured) in jq {
            assert_eq!(site.model().0, frame, "{site:?}: bytes a level");
            for (kb, jq) in [1024, 4096, 8192, 16384].iter().zip(measured) {
                let model = deepest(site, *kb);
                assert!(model <= jq, "{site:?} at {kb} KB: {model} > jq's {jq}");
                let short = (jq - model) * frame;
                assert!(
                    short >= STACK_MARGIN && short < STACK_MARGIN + 2 * frame,
                    "{site:?} at {kb} KB: {model} is {short} B below jq's {jq}"
                );
            }
        }
    }

    /// Every site has a budget when the stack is limited, and they are
    /// ordered by how much stack a level costs.
    #[test]
    fn every_site_has_a_budget() {
        let sites = [
            Site::Free,
            Site::Compare,
            Site::Contains,
            Site::Merge,
            Site::Setpath,
            Site::Delpaths,
        ];
        match stack_limit_bytes() {
            // The test binary's stack is the shell's, which is never
            // unlimited on macOS; on Linux CI it could be.
            None => assert!(sites.iter().all(|s| s.frame_budget().is_none())),
            Some(_) => {
                for site in sites {
                    let budget = site.frame_budget().expect("a limited stack");
                    assert!(budget > 1000, "{site:?}: implausible budget {budget}");
                    // jv_free's frame is the smallest, so it goes deepest.
                    assert!(budget <= Site::Free.frame_budget().expect("limited"));
                }
            }
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
