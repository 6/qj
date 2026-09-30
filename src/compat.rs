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
//! Compat mode is jq's identity too: messages start with `jq:` ([`prog_name`]),
//! and `-h`, the usage after errors, `--version` and `--build-configuration`
//! print jq 1.8.1's text (`src/cli/usage.rs`). By default they are qj's own,
//! which `docs/JQ_PORT_PLAN.md` exempts from comparison.
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

/// The name at the start of qj's messages (`qj: error: ...`): qj's own, or
/// with `QJ_JQ_COMPAT=1` jq's.
///
/// jq 1.8.1 never prints its `argv[0]`: every message in `main.c`, `util.c`
/// and the library spells out `jq` (`argv[0]` only gives `$ORIGIN`), so
/// neither does compat mode. The one line that does carry `argv[0]` comes
/// from glibc, not jq: `assert()`'s (see `src/jq/platform/mod.rs`).
pub fn prog_name() -> &'static str {
    if exactly_jq() { "jq" } else { "qj" }
}

/// Dies of `sig` exactly as an unhandled fatal signal would, without running
/// any cleanup: no destructors, no `atexit` handlers, and in particular no
/// flush of the buffered stdout, which is what jq loses when it crashes.
pub fn die_by_signal(sig: c_int) -> ! {
    small_core_dump();
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

/// Before qj dies on purpose, of the signal jq dies of: keeps the kernel's
/// core dump, but makes it tiny (Linux).
///
/// The kernel decides whether to dump a core as it does for jq — the same
/// signal, `ulimit -c`, `core_pattern` — so the wait status carries jq's
/// core-dump flag and a shell reports `Segmentation fault (core dumped)` where
/// it does for jq. What goes in the core is qj's own business: a core of qj
/// reproducing jq's crash shows nothing wrong, and a full one is big. mimalloc
/// reserves about 1 GB of address space up front, and once any of a mapping is
/// touched the kernel dumps all of it, writing the untouched pages as zeros
/// through a pipe handler such as systemd-coredump or apport, which ignore
/// `ulimit -c`: 1.5 s a crash on GitHub's runners, where jq's take 50 ms.
///
/// So this writes `0` to `/proc/self/coredump_filter`: no anonymous or
/// file-backed memory at all, which leaves the ELF header, the notes (each
/// thread's registers, the signal, the auxiliary vector, the mapped files)
/// and the vDSO — a few KB, however much memory qj has. If the filter can't
/// be written (no `/proc`), it falls back to no core at all, which is at
/// least as quick. Elsewhere nothing changes: macOS dumps a core only when
/// `ulimit -c` asks for one, with no pipe handler to ignore it.
pub fn small_core_dump() {
    #[cfg(target_os = "linux")]
    {
        // Only system calls, no allocation: this runs at an arbitrary point
        // of the program, maybe with other threads holding the allocator's
        // locks.
        const PATH: &std::ffi::CStr = c"/proc/self/coredump_filter";
        // SAFETY: open/write/close on a NUL-terminated path and a static
        // buffer; prctl with PR_SET_DUMPABLE only changes this process's flag.
        unsafe {
            let fd = libc::open(PATH.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            let written = fd >= 0 && libc::write(fd, b"0\n".as_ptr().cast(), 2) == 2;
            if fd >= 0 {
                libc::close(fd);
            }
            if !written {
                libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            }
        }
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
    /// `load_library` ↔ `process_dependencies` (`linker.c`): the length of a
    /// chain of `import`s or `include`s, one module a level.
    ///
    /// The only site that isn't about values, and the one with much the
    /// largest base cost: whichever module is being parsed when the stack runs
    /// out has bison's three `YYINITDEPTH` (200) arrays on it, about 7 KB (see
    /// [`Site::model`]).
    Modules,
    /// `block_bind_subblock_inner` (`compile.c`): binding descends a program's
    /// closure bodies and argument lists, one frame a level.
    ///
    /// The deepest of the compiler's recursions for every kind of nesting but
    /// nested `def`s, and the one with the largest base: the binding walks that
    /// go deepest are parser.y's own actions (`gen_function`, `gen_lambda`,
    /// `block_bind_referenced`), which run inside `yyparse`, with bison's three
    /// `YYINITDEPTH` arrays on the stack.
    Bind,
    /// `compile` (`compile.c`): one frame per nested closure, emitting each
    /// subfunction's bytecode.
    ///
    /// What nested `def`s reach: each one is a closure inside the previous
    /// one's body, and binding doesn't descend them (a bound definition's
    /// `any_unbound` is 0), so this is the only recursion their nesting drives.
    Compile,
    /// `expand_call_arglist` (`compile.c`): one frame per nested argument of a
    /// C function, inside [`Site::Compile`].
    ExpandArgs,
    /// `jv_dump_term` (`jv_print.c`): one frame per level of the value being
    /// printed, at most `MAX_PRINT_DEPTH + 1` (257) of them — below that depth
    /// jq writes `<skipped: too deep>` instead of descending.
    ///
    /// The cap means this can only overflow on a stack of about 80 KB or less,
    /// which is why `docs/COMPATIBILITY.md` used to call it safe; it is the one
    /// recursion here whose depth is bounded by jq's own code rather than by
    /// the value.
    Print,
}

impl Site {
    /// The bytes of stack a level of this recursion costs, and the bytes the
    /// start of the stack takes before any of them starts ([`STACK_MARGIN`]
    /// not included).
    ///
    /// The frame bytes are exact: they are what the prologue of jq's own
    /// function reserves in the release binary's disassembly, and the deepest
    /// value or program jq survives is linear in `ulimit -s` with that slope at
    /// every limit measured. The base is [`WORST_BASE_BYTES`] for the six
    /// recursions over values, which share one figure; the other four have
    /// their own, because what they hold where the stack runs out is different
    /// enough that folding it into the shared figure would cost every site
    /// levels for nothing.
    const fn model(self) -> (u64, u64) {
        // macOS/arm64 (jq's release binary, built by Apple clang), bisected
        // with the programs in `docs/COMPATIBILITY.md` at `ulimit -s` 1024,
        // 2048, 4096, 8176 and 16384 KB.
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let (frame, base) = match self {
            Site::Free => (64, WORST_BASE_BYTES),
            Site::Compare => (128, WORST_BASE_BYTES),
            Site::Contains => (176, WORST_BASE_BYTES),
            Site::Merge => (112, WORST_BASE_BYTES),
            Site::Setpath => (144, WORST_BASE_BYTES),
            Site::Delpaths => (240, WORST_BASE_BYTES),
            Site::Modules => (MODULE_FRAME_BYTES, MODULE_BASE_BYTES),
            Site::Bind => (112, BIND_BASE_BYTES),
            Site::Compile => (176, COMPILE_BASE_BYTES),
            Site::ExpandArgs => (192, COMPILE_BASE_BYTES),
            Site::Print => (256, PRINT_BASE_BYTES),
        };
        // Linux/x86-64 (jq's release binary, built by gcc), bisected the same
        // way at 1024, 4096, 8192 and 16384 KB with the stack randomization
        // off, so that the thresholds are deterministic; STACK_MARGIN then
        // covers the randomization.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let (frame, base) = match self {
            Site::Free => (48, WORST_BASE_BYTES),
            Site::Compare => (144, WORST_BASE_BYTES),
            Site::Contains => (176, WORST_BASE_BYTES),
            Site::Merge => (128, WORST_BASE_BYTES),
            Site::Setpath => (160, WORST_BASE_BYTES),
            Site::Delpaths => (240, WORST_BASE_BYTES),
            Site::Modules => (MODULE_FRAME_BYTES, MODULE_BASE_BYTES),
            Site::Bind => (112, BIND_BASE_BYTES),
            Site::Compile => (224, COMPILE_BASE_BYTES),
            Site::ExpandArgs => (224, COMPILE_BASE_BYTES),
            Site::Print => (304, PRINT_BASE_BYTES),
        };
        (frame, base)
    }

    /// The recursions jq drives from inside this one: comparing a path element
    /// or freeing a value it deleted or replaced, a level at a time, and —
    /// inside the module chain — parsing and binding each module.
    const fn drives(self) -> &'static [Site] {
        match self {
            // `delpaths_sorted` compares path elements and frees what it
            // deletes; `jv_setpath` and `jv_object_merge_recursive` free the
            // value they replace.
            Site::Delpaths => &[Site::Compare, Site::Free],
            Site::Setpath | Site::Merge => &[Site::Free],
            // `load_library` reads and parses a module at every level, which
            // binds its definitions and frees its text.
            Site::Modules => &[Site::Bind, Site::Free],
            // `compile` calls `expand_call_arglist` at every level, and frees
            // the block when it is done with it.
            Site::Compile => &[Site::ExpandArgs, Site::Free],
            _ => &[],
        }
    }

    /// Levels this site gives up so that whatever it drives can still start
    /// where it stops: enough for one frame of the deepest thing it drives,
    /// plus however much that thing's base is above this one's.
    ///
    /// Without it the inner recursion would look like an overflow at a level jq
    /// reaches happily, and qj would die where jq answers.
    fn headroom_levels(self) -> u64 {
        let (frame, base) = self.model();
        let mut need = 0;
        for &inner in self.drives() {
            let (inner_frame, inner_base) = inner.model();
            need = need.max(inner_base.saturating_sub(base) + inner_frame);
        }
        need.div_ceil(frame)
    }

    /// How deep jq's recursion can go here, from the start of the run, in the
    /// levels [`depth`] counts. `None` when the stack is unlimited, where jq
    /// doesn't overflow either.
    pub fn frame_budget(self) -> Option<u64> {
        self.budget_below(0, 0)
    }

    /// The bytes of stack one level of this recursion costs jq, from the
    /// disassembly of jq 1.8.1's release binary for this platform (see
    /// `docs/COMPATIBILITY.md`). Public so that a test can work out where a
    /// recursion this one drives runs out.
    pub fn frame_bytes(self) -> u64 {
        self.model().0
    }

    /// [`Site::frame_budget`] with `outer` bytes of the stack already held by a
    /// recursion this one runs inside, whose own base was `outer_base`.
    ///
    /// The two bases cover the same start of the stack — both begin at `main` —
    /// so only the larger is charged, and [`STACK_MARGIN`] only once. Charging
    /// both would take the margin twice, and at the outer site's own threshold
    /// that leaves the inner one nothing at all.
    fn budget_below(self, outer: u64, outer_base: u64) -> Option<u64> {
        Some(self.budget_at(stack_limit_bytes()?, outer, outer_base))
    }

    /// [`Site::budget_below`] for a stack of `stack_bytes`.
    fn budget_at(self, stack_bytes: u64, outer: u64, outer_base: u64) -> u64 {
        let (frame, base) = self.model();
        let reserved = base.max(outer_base) + STACK_MARGIN;
        let levels = stack_bytes.saturating_sub(reserved.saturating_add(outer)) / frame;
        levels.saturating_sub(self.headroom_levels())
    }

    /// [`Site::frame_budget`] for a stack of `stack_bytes`, with `outer` bytes
    /// of it already held. Public so that a test can size a value, a program or
    /// a chain of modules for a process it starts under a different
    /// `RLIMIT_STACK`.
    pub fn frame_budget_at(self, stack_bytes: u64, outer: u64) -> u64 {
        self.budget_at(stack_bytes, outer, 0)
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

/// Bytes of stack that are gone before any of the six recursions over values
/// starts: what the start of the stack takes where the bisections measured the
/// most of it (`jv_cmp` from `sort`, 4,096 bytes on macOS and 3,856 on Linux).
///
/// One number for the six, rather than each site's own measurement, so that a
/// recursion jq drives from inside another ([`Descent`]) is charged the same
/// base — and so [`STACK_MARGIN`] — once. Charged twice it would run out where
/// jq still has the margin's worth of stack, and qj would die where jq answers.
/// The cost is that a site whose base is smaller than the worst one loses a few
/// more levels: at most 576 bytes on macOS and 2,000 on Linux, which is 9 and
/// 42 levels of `jv_free`.
///
/// The four recursions that are not over values keep their own bases, which are
/// different enough to be worth the arithmetic in [`Site::budget_below`].
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const WORST_BASE_BYTES: u64 = 4096;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const WORST_BASE_BYTES: u64 = 3856;

/// [`Site::Modules`]: bytes of stack a module in a chain of imports costs jq,
/// and what the level where the stack runs out holds — `find_lib`, reading the
/// file, and above all bison's three `YYINITDEPTH` (200) arrays for parsing it.
/// Measured on both platforms (see `docs/COMPATIBILITY.md`); nothing else jq
/// recurses over has a base cost anywhere near this.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const MODULE_FRAME_BYTES: u64 = 416;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const MODULE_BASE_BYTES: u64 = 12000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MODULE_FRAME_BYTES: u64 = 464;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MODULE_BASE_BYTES: u64 = 8304;

/// [`Site::Bind`]: what the stack holds when jq's deepest binding walk starts.
///
/// Those walks are parser.y's actions, so this is `main` → `jq_compile_args` →
/// `load_program` → `jq_parse` → `yyparse` → the action → `block_bind_subblock`,
/// and `yyparse` alone is 5,616 bytes on macOS and 5,696 on Linux: its three
/// `YYINITDEPTH` (200) arrays for the states, values and locations. Bisected
/// against jq's binaries (see `docs/COMPATIBILITY.md`): the deepest binding
/// walk jq survives is `(stack - this) / 112` frames at every stack limit
/// measured, which pins it to 8,976 bytes on macOS and 7,536 on Linux.
///
/// A program whose deepest binding walk is `builtins_bind`'s rather than a
/// parser action's has about 8 KB more stack than this (no `yyparse` frame),
/// and there qj gives up 74 levels it needn't.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const BIND_BASE_BYTES: u64 = 8976;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BIND_BASE_BYTES: u64 = 7536;

/// [`Site::Compile`] and [`Site::ExpandArgs`]: what the stack holds when
/// `block_compile` calls `compile` for the top-level function — `main` →
/// `jq_compile_args` → `block_compile`, plus what the deepest level of
/// `compile` itself calls below the recursion (`expand_call_arglist`'s own
/// callees, `jv_mem_calloc`, `block_free`).
///
/// This is the stack held *before* `compile`'s first frame. `compile` starts by
/// calling `expand_call_arglist`, so at its deepest level there is always a
/// frame of [`Site::ExpandArgs`] on top of it, and that is where jq's stack
/// actually runs out: the two together reproduce the depth of nested `def`s jq
/// survives at every limit measured, which pins this to 3,552 bytes on macOS.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const COMPILE_BASE_BYTES: u64 = 3552;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const COMPILE_BASE_BYTES: u64 = 1856;

/// [`Site::Print`]: what the stack holds when `jv_dump_term` starts, from
/// `main` through the output path. Bisected with `reduce range(n) as $i
/// (0;[.])`, whose value takes `n + 1` frames to print: on macOS the deepest
/// jq survives is 113 at 32 KB of stack, 177 at 48 KB and 241 at 64 KB, which
/// is 256 bytes a level over a base of 3,584. (The steps are 16 KB because
/// that is the page size on arm64 macOS, which `RLIMIT_STACK` is rounded up
/// to.) On Linux it is 304 bytes a level over a base of 1,696: 101 levels at
/// 32 KB, 155 at 48 KB, 209 at 64 KB and 235 at 72 KB.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const PRINT_BASE_BYTES: u64 = 3584;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PRINT_BASE_BYTES: u64 = 1696;

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
/// | 1024 KB | 16,328 | 16,223 |
/// | 4096 KB | 65,480 | 65,375 |
/// | 8176 KB (this machine's default) | 130,760 | 130,655 |
/// | 16384 KB | 262,088 | 261,983 |
///
/// The threshold is exactly linear in the stack size, and the model sits
/// [`RESERVED_BYTES`] below it — 105 levels here, of which 96 are
/// [`STACK_MARGIN`] — because argv and the environment sit on top of jq's
/// stack and take about a level per 64 bytes: with an interactive shell's
/// environment jq survives 130,664 at the default limit rather than 130,760.
/// Erring short means qj never survives where jq dies.
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
/// takes the whole 8 KB, so it lands below the bottom of that window
/// (21,589 / 87,125 / 174,506 / 349,269): qj never survives where jq dies,
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
    let Some(budget) = budget_of(Site::Free) else {
        return;
    };
    if native_frames + free_frames(items) > budget {
        die_of_stack_overflow();
    }
}

// ---------------------------------------------------------------------------
// The hooks: jq's other recursions
// ---------------------------------------------------------------------------

thread_local! {
    /// Bytes of jq's stack an outer recursion is holding while an inner one
    /// runs, and the largest base any of those outer recursions reserved — see
    /// [`Descent`]. `(0, 0)` except inside one.
    static OUTER: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// The budget of `site` here, or `None` when nothing can overflow (compat mode
/// is off, or the stack is unlimited).
///
/// "Here" is what makes this different from [`Site::frame_budget`]: jq runs
/// some of these recursions from inside another one (`delpaths_sorted` frees a
/// value and compares path elements a level at a time, `jv_object_merge_
/// recursive` frees the value it replaces), and the stack the outer one holds
/// is gone from the inner one's budget.
///
/// Inlined into every hook, so that a run that isn't compat mode does one
/// load of the cached flag and nothing else.
#[inline]
fn budget_of(site: Site) -> Option<u64> {
    if !exactly_jq() {
        return None;
    }
    let (bytes, base) = OUTER.with(std::cell::Cell::get);
    site.budget_below(bytes, base)
}

/// How deep `site`'s recursion can go here, as a plain number a walk can
/// compare its depth against: [`u64::MAX`] when nothing can overflow (compat
/// mode is off, or the stack is unlimited), so that the walk's per-level test
/// is one comparison.
///
/// Read once per walk, not per level: it depends on what an outer recursion
/// holds ([`Descent`]), which doesn't change inside one.
#[inline]
pub fn frames_available(site: Site) -> u64 {
    budget_of(site).unwrap_or(u64::MAX)
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

/// A recursion qj walks with a loop, where jq recurses once per level.
///
/// The caller counts its levels from 1, as the measurements do, and calls
/// [`Descent::at`] with the level it is working at, which dies when jq's stack
/// would have run out — and records how much of the stack jq holds there, so
/// that a recursion driven from inside this one (freeing a value, comparing
/// two path elements) gets the smaller budget jq has left.
///
/// Outside compat mode it holds nothing and every call is a branch on `None`.
pub struct Descent {
    /// `(bytes a level, levels this recursion has, the base it reserved)`, or
    /// `None` outside compat mode and where jq cannot overflow.
    model: Option<(u64, u64, u64)>,
    /// What an outer recursion was holding when this one started.
    outer: (u64, u64),
}

impl Descent {
    /// jq is entering this recursion: work out its budget here.
    #[inline]
    pub fn new(site: Site) -> Descent {
        let Some(budget) = budget_of(site) else {
            return Descent {
                model: None,
                outer: (0, 0),
            };
        };
        let (frame, base) = site.model();
        Descent {
            model: Some((frame, budget, base)),
            outer: OUTER.with(std::cell::Cell::get),
        }
    }

    /// jq is `level` frames into this recursion: die if its stack has run out,
    /// and leave the stack those frames hold to the recursions this level
    /// drives.
    #[inline]
    pub fn at(&self, level: u64) {
        if let Some((frame, budget, base)) = self.model {
            if level > budget {
                die_of_stack_overflow();
            }
            let (bytes, outer_base) = self.outer;
            OUTER.with(|b| b.set((bytes + level * frame, outer_base.max(base))));
        }
    }
}

impl Drop for Descent {
    fn drop(&mut self) {
        if self.model.is_some() {
            OUTER.with(|b| b.set(self.outer));
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
        site.frame_budget_at(kb * 1024, 0) - 1
    }

    /// Every site's model against the depths bisected from the jq 1.8.1
    /// binary, at the worst of the call sites measured (see
    /// `docs/COMPATIBILITY.md` for the programs and the full tables), with the
    /// bytes each site's base cost measured.
    ///
    /// `(site, bytes a level, base bytes, deepest n jq survives at each of the
    /// stack limits in [`LIMITS`])`.
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    const LIMITS: &[u64] = &[1024, 2048, 4096, 8176, 16384];
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    const MEASURED: &[(Site, u64, u64, &[u64])] = &[
        (
            Site::Free,
            64,
            3520,
            &[16_328, 32_712, 65_480, 130_760, 262_088],
        ),
        (
            Site::Compare,
            128,
            4096,
            &[8_159, 16_351, 32_735, 65_375, 131_039],
        ),
        (
            Site::Contains,
            176,
            3840,
            &[5_935, 11_893, 23_809, 47_547, 95_303],
        ),
        (
            Site::Merge,
            112,
            3616,
            &[9_329, 18_692, 37_416, 74_719, 149_764],
        ),
        (
            Site::Setpath,
            144,
            3808,
            &[7_255, 14_537, 29_101, 58_114, 116_481],
        ),
        (
            Site::Delpaths,
            240,
            3664,
            &[4_353, 8_722, 17_460, 34_868, 69_889],
        ),
        (
            Site::Modules,
            416,
            12000,
            &[2_491, 5_012, 10_053, 20_096, 40_300],
        ),
    ];

    /// On Linux/x86-64 the kernel's randomization of the initial stack pointer
    /// moves jq's threshold from run to run (see [`free_frame_budget`]), so
    /// the bisections switch it off (`setarch -R`) and [`STACK_MARGIN`] covers
    /// the whole 8 KB: the models sit at or under the bottom of jq's window.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const LIMITS: &[u64] = &[1024, 4096, 8192, 16384];
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const MEASURED: &[(Site, u64, u64, &[u64])] = &[
        (Site::Free, 48, 1856, &[21_806, 87_342, 174_723, 349_486]),
        (Site::Compare, 144, 3856, &[7_254, 29_100, 58_227, 116_481]),
        (Site::Contains, 176, 2128, &[5_945, 23_819, 47_650, 95_312]),
        (Site::Merge, 128, 2304, &[8_173, 32_749, 65_517, 131_053]),
        (Site::Setpath, 160, 2176, &[6_539, 26_200, 52_415, 104_843]),
        (Site::Delpaths, 240, 2224, &[4_359, 17_466, 34_943, 69_895]),
        (Site::Modules, 464, 8304, &[2_241, 9_021, 18_060, 36_139]),
    ];

    /// Each model is short of jq by the margin, plus what this site's base
    /// cost is below the worst one, plus the rounding of a level — which is
    /// what keeps qj from surviving where jq dies.
    #[test]
    fn stack_models_match_the_measurements() {
        for &(site, frame, base, measured) in MEASURED {
            assert_eq!(site.model().0, frame, "{site:?}: bytes a level");
            let reserved = site.model().1 + STACK_MARGIN;
            assert!(base <= reserved, "{site:?}: base above what it reserves");
            // What the model holds back beyond this site's own base cost: the
            // margin, what its base is below the figure it shares (nothing for
            // `Modules`, which has its own), and the levels a driving site
            // gives up for what it drives.
            let allowance = (reserved - base) + site.headroom_levels() * frame;
            for (kb, jq) in LIMITS.iter().zip(measured) {
                let model = deepest(site, *kb);
                assert!(model <= *jq, "{site:?} at {kb} KB: {model} > jq's {jq}");
                let short = (jq - model) * frame;
                assert!(
                    short >= allowance && short < allowance + 2 * frame,
                    "{site:?} at {kb} KB: {model} is {short} B below jq's {jq}, \
                     not {allowance}..{}",
                    allowance + 2 * frame
                );
            }
        }
    }

    /// Every site whose recursion jq drives from inside another one, and where
    /// it does: `delpaths_sorted` compares path elements and frees the values it
    /// deletes, `jv_setpath` and `jv_object_merge_recursive` free the value they
    /// replace, `load_library` parses and binds a module at every level, and
    /// `compile` expands a call list and frees a block at every level.
    const DRIVEN: &[(Site, &[Site])] = &[
        (Site::Delpaths, &[Site::Compare, Site::Free]),
        (Site::Setpath, &[Site::Free]),
        (Site::Merge, &[Site::Free]),
        (Site::Modules, &[Site::Bind, Site::Free]),
        (Site::Compile, &[Site::ExpandArgs, Site::Free]),
    ];

    /// At the deepest level a driving site allows, whatever it drives has to be
    /// able to start — otherwise the inner recursion would look like an overflow
    /// at a level jq reaches happily, and qj would die where jq answers. This
    /// checks that the levels the outer site gives up
    /// ([`Site::headroom_levels`]) buy the room at every stack limit measured,
    /// for the inner site's *own* base as well as its frame.
    #[test]
    fn a_driving_site_leaves_room_for_what_it_drives() {
        for &(outer, inners) in DRIVEN {
            assert_eq!(
                outer.drives(),
                inners,
                "{outer:?}: drives() disagrees with DRIVEN"
            );
            assert!(
                outer.headroom_levels() > 0,
                "{outer:?} should give up a level"
            );
            for kb in LIMITS {
                let deepest = outer.frame_budget_at(kb * 1024, 0);
                let held = deepest * outer.model().0;
                for &inner in inners {
                    let left = inner.budget_at(kb * 1024, held, outer.model().1);
                    assert!(
                        left >= 1,
                        "{outer:?} at {kb} KB, {deepest} levels deep: no room for {inner:?}"
                    );
                }
            }
        }
        for &(site, _, _, _) in MEASURED {
            assert_eq!(
                !site.drives().is_empty(),
                DRIVEN.iter().any(|(s, _)| *s == site),
                "{site:?}: drives() disagrees with DRIVEN"
            );
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
