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
//! of `SIGSEGV`; so does a program, a chain of modules or a regex nested deep
//! enough, and on a small enough stack jq can't even start. qj's value layer
//! does all of it iteratively, and everything else runs on a thread with a
//! large stack of its own (`src/main.rs`), so in compat mode qj measures the
//! depth jq's recursion would have reached and raises the same signal at the
//! same point in the program.
//!
//! jq's stack is `RLIMIT_STACK` as the kernel applies it, less what argv and
//! the environment take at its top ([`area_of`]), and each model is the bytes
//! a level and the bytes the stack holds before the first, measured against
//! the jq binary. [`Site`] lists the recursions; [`starting`], [`compiling`]
//! and [`running_tests`] are the fixed needs of jq's start-up, its compiler
//! and its test loop; `docs/COMPATIBILITY.md` has the tables and the sites
//! that cannot overflow at all.

mod depth;
mod regex;

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

/// `getrlimit(RLIMIT_STACK)`: what `ulimit -s` sets, which is the size of a
/// process's main thread's stack. `None` when it is unlimited or unreadable.
fn rlimit_stack() -> Option<u64> {
    // SAFETY: getrlimit writes an rlimit into a valid out-pointer.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut lim) } != 0 {
        return None;
    }
    // `rlim_t` is `u64` on macOS and Linux, and `i64` on FreeBSD, where a
    // limit is never negative.
    #[allow(clippy::unnecessary_cast)]
    let cur = lim.rlim_cur as u64;
    (lim.rlim_cur != libc::RLIM_INFINITY && cur != 0).then_some(cur)
}

/// `RLIMIT_STACK` as the kernel applies it to a main thread's stack.
///
/// Linux grows the stack's mapping a page at a time for as long as the whole
/// mapping fits in the limit, so the limit rounds down to a page. macOS
/// reserves the stack and makes what is past the limit, rounded down to a
/// page, inaccessible, so the limit rounds *up* to a page: on arm64, with
/// 16 KB pages, `ulimit -s 17` gives a 32 KB stack.
pub fn effective_limit(rlimit: u64) -> u64 {
    // SAFETY: sysconf has no preconditions.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .ok()
        .filter(|&p| p > 0)
        .unwrap_or(4096);
    if cfg!(target_os = "linux") {
        rlimit / page * page
    } else {
        rlimit.div_ceil(page) * page
    }
}

/// What `argv` and the environment take at the top of a main thread's stack:
/// every string with its NUL, and the `NULL`-terminated arrays of pointers to
/// them.
///
/// The kernel puts more there — the executable's path, the auxiliary vector
/// (Linux) or the apple strings (macOS), alignment — but that is the same for
/// every run of the same binary, and the models' bases include it (for jq's
/// release binaries at the paths mise installs them to).
pub fn area_of<'a>(
    argv: impl IntoIterator<Item = &'a [u8]>,
    env: impl IntoIterator<Item = &'a [u8]>,
) -> u64 {
    const PTR: u64 = size_of::<usize>() as u64;
    let mut bytes = 0;
    for strings in [
        argv.into_iter().collect::<Vec<_>>(),
        env.into_iter().collect::<Vec<_>>(),
    ] {
        bytes += (strings.len() as u64 + 1) * PTR;
        bytes += strings.iter().map(|s| s.len() as u64 + 1).sum::<u64>();
    }
    bytes
}

/// The C environment, as `environ` holds it: `NAME=value` strings.
fn environ_strings() -> Vec<Vec<u8>> {
    #[cfg(target_vendor = "apple")]
    // SAFETY: _NSGetEnviron returns the address of the process's `environ`.
    let mut p = unsafe { *libc::_NSGetEnviron() } as *const *const libc::c_char;
    #[cfg(not(target_vendor = "apple"))]
    let mut p = {
        unsafe extern "C" {
            static environ: *const *const libc::c_char;
        }
        // SAFETY: reading the C runtime's `environ` pointer.
        unsafe { environ }
    };
    let mut out = Vec::new();
    if p.is_null() {
        return out;
    }
    // SAFETY: `environ` is a NULL-terminated array of NUL-terminated strings,
    // and nothing changes it while this runs (it runs at start-up).
    unsafe {
        while !(*p).is_null() {
            out.push(std::ffi::CStr::from_ptr(*p).to_bytes().to_vec());
            p = p.add(1);
        }
    }
    out
}

/// [`area_of`] this process's arguments and environment as they were when it
/// started, which [`starting`] reads before anything could change them.
fn own_area() -> u64 {
    static AREA: OnceLock<u64> = OnceLock::new();
    *AREA.get_or_init(|| {
        use std::os::unix::ffi::OsStringExt;
        let argv: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
        let env = environ_strings();
        area_of(
            argv.iter().map(Vec::as_slice),
            env.iter().map(Vec::as_slice),
        )
    })
}

/// The stack jq's own code would have in this process: the limit as the
/// kernel applies it, less what argv and the environment take at its top.
/// `None` when the stack is unlimited, where jq doesn't overflow either.
///
/// Counting the environment is what lets one model fit every environment: a
/// larger one moves jq's thresholds down by exactly its size (a level of
/// `jv_free` per 64 bytes on macOS), and an interactive shell's is several KB
/// larger than a test harness's.
fn stack_bytes() -> Option<u64> {
    static BYTES: OnceLock<Option<u64>> = OnceLock::new();
    *BYTES.get_or_init(|| Some(effective_limit(rlimit_stack()?).saturating_sub(own_area())))
}

/// A recursion in jq 1.8.1 that a deep enough value, path, program or regex
/// can drive past its C stack, killing the process with `SIGSEGV`.
///
/// Every one of them is a loop with an explicit stack in qj, or a recursion
/// of qj's own on its own large stack, so in compat mode qj measures the
/// depth jq's recursion would have reached and dies where jq does. The model
/// of each is a number of bytes a level and a number of bytes reserved
/// ([`Site::model`]), measured against the jq binary; `docs/COMPATIBILITY.md`
/// lists them, and the recursions that cannot overflow.
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
    /// `jv_dump_term` (`jv_print.c`) from `main.c`'s output path: one frame
    /// per level of the value being printed, at most `MAX_PRINT_DEPTH + 2`
    /// (258) of them — the level past `MAX_PRINT_DEPTH` writes `<skipped: too
    /// deep>` instead of descending.
    ///
    /// The cap means this can only overflow on a stack of about 80 KB or less;
    /// it is the one recursion here whose depth is bounded by jq's own code
    /// rather than by the value.
    Print,
    /// `jv_dump_term` while the program runs: `tojson`, `tostring`, `@json`,
    /// `@text`, string interpolation, `debug`, `stderr`, error messages. The
    /// same frames as [`Site::Print`], from deeper in jq's stack (the VM, the
    /// builtin, `jv_dump_string`): the worst of those call sites.
    Dump,
    /// Oniguruma's parser (`prs_alts` → `prs_branch` → `prs_exp` → `prs_bag`
    /// …), compiling a regex for `test`, `match`, `capture`, `sub` and the
    /// rest: one level for the pattern and one per group open at once, of any
    /// kind (`src/compat/regex.rs`).
    RegexParse,
    /// Oniguruma's walks over the parsed pattern (`tune_tree`,
    /// `compile_tree`, …): one level per node on the way down, where a
    /// capturing group, a lookaround, a quantifier, an alternation and a
    /// sequence are each a node.
    RegexTree,
}

impl Site {
    /// The bytes of stack a level of this recursion costs, and the bytes the
    /// stack holds before its first level: from the top of the stack, beyond
    /// what argv and the environment take ([`area_of`]), and at the worst of
    /// the depths and call sites measured.
    ///
    /// The frame bytes are exact: they are what the prologue of jq's own
    /// function reserves in the release binary's disassembly, and the stack
    /// jq needs is linear in the depth with that slope at every depth
    /// measured. The bases were measured by padding the environment at a
    /// fixed limit until jq no longer answered, at 16 random depths per site;
    /// where the base moves from one depth to the next (by up to 600 bytes: a
    /// slow path of the allocator at the deepest level), it is the largest
    /// seen. The six recursions over values share one base, the largest of
    /// theirs ([`VALUE_BASE_BYTES`]), so that one driven from inside another
    /// ([`Descent`]) is charged it, and [`STACK_MARGIN`], once.
    const fn model(self) -> (u64, u64) {
        // macOS/arm64 (jq's release binary, built by Apple clang).
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let m = match self {
            Site::Free => (64, VALUE_BASE_BYTES),
            Site::Compare => (128, VALUE_BASE_BYTES),
            Site::Contains => (176, VALUE_BASE_BYTES),
            Site::Merge => (112, VALUE_BASE_BYTES),
            Site::Setpath => (144, VALUE_BASE_BYTES),
            Site::Delpaths => (240, VALUE_BASE_BYTES),
            Site::Modules => (416, 11_136),
            Site::Bind => (112, 8_704),
            Site::Compile => (176, 3_440),
            Site::ExpandArgs => (192, 3_440),
            Site::Print => (256, 2_976),
            Site::Dump => (256, 4_064),
            Site::RegexParse => (784, 4_848),
            Site::RegexTree => (624, 4_400),
        };
        // Linux/x86-64 (jq's release binary, built by gcc, glibc linked
        // statically), measured with the kernel's stack randomization off.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let m = match self {
            Site::Free => (48, VALUE_BASE_BYTES),
            Site::Compare => (144, VALUE_BASE_BYTES),
            Site::Contains => (176, VALUE_BASE_BYTES),
            Site::Merge => (128, VALUE_BASE_BYTES),
            Site::Setpath => (160, VALUE_BASE_BYTES),
            Site::Delpaths => (240, VALUE_BASE_BYTES),
            Site::Modules => (464, 7_776),
            Site::Bind => (112, 7_328),
            Site::Compile => (224, 1_696),
            Site::ExpandArgs => (224, 1_696),
            Site::Print => (304, 1_424),
            Site::Dump => (304, 2_176),
            Site::RegexParse => (784, 3_600),
            Site::RegexTree => (560, 2_912),
        };
        m
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
    /// The two bases cover the same start of the stack — both begin at the top
    /// — so only the larger is charged, and [`STACK_MARGIN`] only once.
    /// Charging both would take the margin twice, and at the outer site's own
    /// threshold that leaves the inner one nothing at all.
    fn budget_below(self, outer: u64, outer_base: u64) -> Option<u64> {
        Some(self.budget_at(stack_bytes()?, outer, outer_base))
    }

    /// [`Site::budget_below`] for `stack_bytes` of stack beyond argv and the
    /// environment.
    fn budget_at(self, stack_bytes: u64, outer: u64, outer_base: u64) -> u64 {
        let (frame, base) = self.model();
        let reserved = base.max(outer_base) + STACK_MARGIN;
        let levels = stack_bytes.saturating_sub(reserved.saturating_add(outer)) / frame;
        levels.saturating_sub(self.headroom_levels())
    }

    /// [`Site::frame_budget`] for `stack_bytes` of stack beyond argv and the
    /// environment (the effective limit less [`area_of`] them), with `outer`
    /// bytes of it already held. Public so that a test can size a value, a
    /// program or a chain of modules for a process it starts with a different
    /// `RLIMIT_STACK`.
    pub fn frame_budget_at(self, stack_bytes: u64, outer: u64) -> u64 {
        self.budget_at(stack_bytes, outer, 0)
    }
}

/// Bytes held back from every model, on top of what the measurements found,
/// so that qj dies no later than jq would.
///
/// On macOS it covers what the models don't count: jq's executable path, which
/// the kernel puts on its stack too (`executable_path=`), and allocator slow
/// paths deeper than any the measurements met. With the environment counted
/// ([`stack_bytes`]) the rest is deterministic: the same limit, environment
/// and program give jq the same threshold in every run.
///
/// On Linux it covers the kernel's randomization of the initial stack pointer
/// as well (`arch_align_stack`: up to 8,191 bytes, then 16-byte alignment),
/// which moves jq's thresholds from run to run: the models sit below the
/// bottom of that window, so qj never survives where jq can die, and inside
/// the window it dies where jq only sometimes does.
const STACK_MARGIN: u64 = MARGIN_BYTES + RANDOMIZATION_BYTES;

/// [`STACK_MARGIN`] less the randomization.
const MARGIN_BYTES: u64 = 512;

/// The most `arch_align_stack` can take from the top of the stack on Linux:
/// `sp -= get_random_u32_below(8192); sp &= ~0xf`.
#[cfg(target_os = "linux")]
const RANDOMIZATION_BYTES: u64 = 8191 + 15;
#[cfg(not(target_os = "linux"))]
const RANDOMIZATION_BYTES: u64 = 0;

/// The base the six recursions over values share: the largest any of them
/// was measured with, at any depth, so that one run inside another is
/// charged the same base once. On macOS that is `jv_object_merge_recursive`'s
/// 3,896 bytes, where the allocator took a slow path at the deepest level (it
/// is 3,304 at most depths); on Linux `jv_cmp`'s from `sort`, 3,148 (glibc's
/// allocator never moved a base). The cost is that a site measured with a
/// smaller base loses up to 752 bytes on macOS (12 levels of `jv_free`) and
/// 1,604 on Linux (33).
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const VALUE_BASE_BYTES: u64 = 3_904;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const VALUE_BASE_BYTES: u64 = 3_152;

/// How many nested `jv_free` calls jq 1.8.1 can make before its stack
/// overflows: one per level of nesting of the value being freed, so this is
/// the value's nesting depth plus one for the scalar at the bottom.
///
/// On macOS/arm64 it is 64 bytes a frame, on Linux/x86-64 48. Other targets
/// use the macOS model, unmeasured.
///
/// `None` when the stack is unlimited, where jq doesn't overflow either.
pub fn free_frame_budget() -> Option<u64> {
    Site::Free.frame_budget()
}

// ---------------------------------------------------------------------------
// jq's start
// ---------------------------------------------------------------------------

/// What jq needs before it does anything at all, beyond argv and the
/// environment. On Linux, where jq is linked statically, glibc's start-up and
/// `main.c`'s option handling up to the help text, the version or a usage
/// error (5,172 bytes). On macOS, the platform's start-up for jq's release
/// binary, which depends on the release ([`MACOS_START_BYTES`]).
fn start_bytes() -> u64 {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        5_184
    } else {
        static START: OnceLock<u64> = OnceLock::new();
        *START.get_or_init(|| macos_start_bytes(macos_release()))
    }
}

/// jq's start-up on each macOS release measured, newest first: 19,272 bytes on
/// macOS 27, dyld's, which every program needs alike (jq's own start needs
/// less); 28,704 to 28,712 on macOS 26, where a program that does nothing needs
/// 21,400, and qj, linked as `build.rs` says, what jq does. Both at the same
/// executable path's length; the margin covers a longer one for jq.
const MACOS_START_BYTES: [(u32, u64); 2] = [(27, 19_280), (26, 28_720)];

/// [`MACOS_START_BYTES`] for a release: the most of them for one that wasn't
/// measured.
fn macos_start_bytes(release: Option<u32>) -> u64 {
    MACOS_START_BYTES
        .iter()
        .find(|&&(r, _)| Some(r) == release)
        .or_else(|| MACOS_START_BYTES.iter().max_by_key(|&&(_, b)| b))
        .map_or(0, |&(_, b)| b)
}

/// Targets other than macOS and Linux/x86-64 take the newest macOS's model,
/// unmeasured.
#[cfg(not(target_os = "macos"))]
fn macos_release() -> Option<u32> {
    Some(MACOS_START_BYTES[0].0)
}

/// macOS's major version (`kern.osproductversion`, "26.6.2" for 26).
#[cfg(target_os = "macos")]
fn macos_release() -> Option<u32> {
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    // SAFETY: sysctlbyname writes at most `len` bytes into `buf` and sets
    // `len` to how many it wrote.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let version = std::str::from_utf8(&buf[..len.min(buf.len())]).ok()?;
    version
        .trim_end_matches('\0')
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// What jq needs to compile a program that doesn't nest, and to run one
/// that doesn't recurse: the deepest point of `jq_compile_args` for the
/// builtins, which every program has (9,244 to 9,252 bytes on Linux), or of
/// the syntax error it reports (9,460). Nothing in the small-stack corpus of
/// dates, formats, sorting, regex matching, `--stream`, `-s` or modules goes
/// deeper without a recursion that has a model of its own. Below the
/// start-up's need on macOS, where it matters only inside `--run-tests`.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const COMPILE_BYTES: u64 = 11_904;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const COMPILE_BYTES: u64 = 9_472;

/// What `--run-tests` holds above everything it compiles and runs: its test
/// loop's buffers (`jq_test.c`'s `prog`, `buf` and error buffer) and the
/// frames to them. Measured as the stack a deep program needs in a test file
/// less what it needs run as a program: 28,496 bytes on macOS and 12,648 on
/// Linux for the compiler's recursions, 128 and 112 less for the rest.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const RUN_TESTS_BYTES: u64 = 28_496;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUN_TESTS_BYTES: u64 = 12_656;

/// What `--run-tests` needs for a file of tests that don't nest: up to 40,392
/// bytes on macOS 27 and 40,360 on macOS 26 (it varies from one run to the
/// next there, by up to 330 bytes) and 23,796 on Linux, the same whether they
/// pass or fail.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const RUN_TESTS_FLOOR_BYTES: u64 = 40_400;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUN_TESTS_FLOOR_BYTES: u64 = 23_808;

/// Whether `bytes` of stack, at what the recursions around this point hold
/// ([`OUTER`]), fit in jq's.
fn fits(bytes: u64) -> bool {
    let Some(stack) = stack_bytes() else {
        return true;
    };
    let (outer, _) = OUTER.with(std::cell::Cell::get);
    bytes + STACK_MARGIN + outer <= stack
}

/// The `qj` binary's first act (`src/main.rs`): in compat mode, where jq's
/// start-up would overflow its stack, die as it does — of `SIGSEGV`, with
/// nothing written.
///
/// It also reads what argv and the environment take ([`own_area`]) before
/// anything can change the environment.
pub fn starting() {
    if !exactly_jq() {
        return;
    }
    own_area();
    if !fits(start_bytes()) {
        die_of_stack_overflow();
    }
    // jq frees values natively below this depth; see `native_drop_limit`.
    if let Some(budget) = Site::Free.frame_budget() {
        let limit = u32::try_from(budget.saturating_sub(1)).unwrap_or(u32::MAX);
        NATIVE_DROP_LIMIT.fetch_min(limit, std::sync::atomic::Ordering::Relaxed);
    }
}

/// jq is compiling a program (`jq_compile_args`, or a test's `jq_compile`):
/// in compat mode, die if its stack can't hold the compile of what every
/// program has, the builtins.
pub fn compiling() {
    if exactly_jq() && !fits(COMPILE_BYTES) {
        die_of_stack_overflow();
    }
}

/// `--run-tests` is starting: in compat mode, die where jq's test loop
/// wouldn't fit ([`RUN_TESTS_FLOOR_BYTES`]); everything it compiles and runs
/// from here on runs below its buffers ([`RUN_TESTS_BYTES`]).
pub fn running_tests() {
    if !exactly_jq() {
        return;
    }
    if !fits(RUN_TESTS_FLOOR_BYTES) {
        die_of_stack_overflow();
    }
    let (bytes, base) = OUTER.with(std::cell::Cell::get);
    OUTER.with(|b| b.set((bytes + RUN_TESTS_BYTES, base)));
}

/// jq compiles a regex (`f_match` → `onig_new`), with Oniguruma's options: in
/// compat mode, die where its parser or its walks over the parsed pattern
/// would overflow jq's stack.
pub fn compiling_regex(pattern: &[u8], extended: bool, ignorecase: bool) {
    if !exactly_jq() {
        return;
    }
    let parse = frames_available(Site::RegexParse);
    let tree = frames_available(Site::RegexTree);
    if parse == u64::MAX {
        return;
    }
    let d = regex::depths(pattern, extended, ignorecase);
    // The parser has a level for the pattern itself, and one per group.
    if d.parse + 1 > parse || d.tree > tree {
        die_of_stack_overflow();
    }
}

/// The deepest `src/jq/value/array.rs` frees a value natively before it hands
/// the rest to its iterative loop, which checks it against jq's stack
/// ([`freeing_iteratively`]). 256 levels by default; in compat mode on a stack
/// where jq's `jv_free` has fewer frames than that, one short of them, so
/// that a value too deep for jq always reaches the check.
static NATIVE_DROP_LIMIT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(crate::jq::value::array::MAX_DROP_RECURSION);

/// [`NATIVE_DROP_LIMIT`].
#[inline]
pub fn native_drop_limit() -> u32 {
    NATIVE_DROP_LIMIT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The stack jq's start-up, its compiler and its test loop need, margin
/// included — what [`starting`], [`compiling`] and [`running_tests`] compare
/// with the stack beyond argv and the environment. Public so that a test can
/// pick a limit between two of them.
pub fn fixed_needs() -> FixedNeeds {
    FixedNeeds {
        start: start_bytes() + STACK_MARGIN,
        compile: COMPILE_BYTES + STACK_MARGIN,
        run_tests: RUN_TESTS_FLOOR_BYTES + STACK_MARGIN,
    }
}

/// [`fixed_needs`].
#[derive(Clone, Copy, Debug)]
pub struct FixedNeeds {
    pub start: u64,
    pub compile: u64,
    pub run_tests: u64,
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

    #[test]
    fn the_area_is_the_strings_and_their_pointers() {
        let s = |v: &[&'static str]| v.iter().map(|x| x.as_bytes()).collect::<Vec<_>>();
        // Two NULL pointers and nothing else.
        assert_eq!(area_of(s(&[]), s(&[])), 16);
        // "jq\0" "-n\0" "1\0" and four pointers; "A=1\0" and two.
        assert_eq!(
            area_of(s(&["jq", "-n", "1"]), s(&["A=1"])),
            3 + 3 + 2 + 32 + 4 + 16
        );
    }

    #[test]
    fn the_limit_rounds_as_the_kernel_does() {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        assert_eq!(effective_limit(8 * page), 8 * page);
        if cfg!(target_os = "linux") {
            assert_eq!(effective_limit(8 * page + 1), 8 * page);
            assert_eq!(effective_limit(8 * page - 1), 7 * page);
        } else {
            assert_eq!(effective_limit(8 * page + 1), 9 * page);
            assert_eq!(effective_limit(8 * page - 1), 8 * page);
        }
    }

    /// What the jq 1.8.1 binary needs at each site, beyond what argv and the
    /// environment take, over 16 random depths each (`docs/COMPATIBILITY.md`
    /// has the programs): `(site, bytes a level, the base at most depths, the
    /// largest base at any depth)`. A site with more than one call site
    /// measured (`jv_cmp` from `==` and from `sort`, the printer from each
    /// builtin that dumps) lists each.
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    const MEASURED: &[(Site, u64, u64, u64)] = &[
        (Site::Free, 64, 3_160, 3_160),
        (Site::Compare, 128, 3_128, 3_128),
        (Site::Compare, 128, 3_448, 3_448),
        (Site::Contains, 176, 3_336, 3_336),
        (Site::Merge, 112, 3_304, 3_896),
        (Site::Setpath, 144, 3_192, 3_640),
        (Site::Delpaths, 240, 3_192, 3_704),
        (Site::Modules, 416, 11_128, 11_128),
        (Site::Bind, 112, 8_696, 8_696),
        (Site::Compile, 176, 3_176, 3_432),
        (Site::Print, 256, 2_920, 2_967),
        (Site::Dump, 256, 3_288, 3_336),
        (Site::Dump, 256, 4_008, 4_056),
        (Site::Dump, 256, 3_704, 3_704),
        (Site::RegexParse, 784, 4_840, 4_840),
        (Site::RegexTree, 624, 4_392, 4_392),
    ];
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const MEASURED: &[(Site, u64, u64, u64)] = &[
        (Site::Free, 48, 1_548, 1_548),
        (Site::Compare, 144, 1_548, 1_548),
        (Site::Compare, 144, 3_148, 3_148),
        (Site::Contains, 176, 1_676, 1_676),
        (Site::Merge, 128, 2_028, 2_028),
        (Site::Setpath, 160, 1_692, 1_692),
        (Site::Delpaths, 240, 1_708, 1_708),
        (Site::Modules, 464, 7_772, 7_772),
        (Site::Bind, 112, 7_324, 7_324),
        (Site::Compile, 224, 1_692, 1_692),
        (Site::Print, 304, 1_404, 1_412),
        (Site::Dump, 304, 1_644, 1_836),
        (Site::Dump, 304, 2_172, 2_172),
        (Site::Dump, 304, 1_772, 1_964),
        (Site::RegexParse, 784, 3_596, 3_596),
        (Site::RegexTree, 560, 2_908, 2_908),
    ];

    /// How far below jq a model may sit, beyond [`STACK_MARGIN`]: what the
    /// shared base of the value sites costs the one measured with the
    /// smallest base.
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    const SHARED_BASE_COST: u64 = 800;
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const SHARED_BASE_COST: u64 = 1_620;

    /// Every model against the measurements: never below what jq needs at any
    /// depth measured (so qj never survives where jq dies), and above it by no
    /// more than the margin and what a shared base costs.
    #[test]
    fn stack_models_match_the_measurements() {
        for &(site, frame, typical, worst) in MEASURED {
            let (model_frame, base) = site.model();
            assert_eq!(model_frame, frame, "{site:?}: bytes a level");
            assert!(base >= worst, "{site:?}: base {base} below jq's {worst}");
            let short = base + STACK_MARGIN - typical;
            assert!(
                short <= STACK_MARGIN + SHARED_BASE_COST,
                "{site:?}: {short} bytes below jq"
            );
            // At every depth that fits, the model's need is jq's plus that
            // much: the budget in levels is exactly what jq's need allows.
            for stack in [48 << 10, 256 << 10, 1 << 20, 8 << 20] {
                let levels = site.frame_budget_at(stack, 0) + site.headroom_levels();
                let jq_levels = stack.saturating_sub(worst) / frame;
                assert!(
                    levels <= jq_levels,
                    "{site:?} at {stack}: {levels} > {jq_levels}"
                );
            }
        }
    }

    /// The fixed needs against the measurements.
    #[test]
    fn start_compile_and_run_tests_cover_jq() {
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let (compile, run_tests_trivial, run_tests_offset) = (11_896, 40_392, 28_496);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let (compile, run_tests_trivial, run_tests_offset) = (9_460, 23_796, 12_648);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let starts = [(None, 5_172)];
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let starts = [
            (Some(27), 19_272),
            (Some(26), 28_712),
            (None, 28_712),
            (Some(15), 28_712),
        ];
        for (release, start) in starts {
            let bytes = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                start_bytes()
            } else {
                macos_start_bytes(release)
            };
            assert!(
                bytes >= start && bytes - start <= 16,
                "{release:?}: {bytes}"
            );
        }
        // Each macOS release's start-up is less than its test loop's.
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        for (_, start) in MACOS_START_BYTES {
            assert!(start < RUN_TESTS_FLOOR_BYTES);
        }
        assert!(COMPILE_BYTES >= compile && COMPILE_BYTES - compile <= 16);
        assert!(RUN_TESTS_BYTES >= run_tests_offset && RUN_TESTS_BYTES - run_tests_offset <= 16);
        assert!(RUN_TESTS_FLOOR_BYTES >= run_tests_trivial);
        assert!(RUN_TESTS_FLOOR_BYTES - run_tests_trivial <= 16);
        const { assert!(COMPILE_BYTES + RUN_TESTS_BYTES <= RUN_TESTS_FLOOR_BYTES + 16) };
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
            for kb in [256u64, 1024, 4096, 8192, 16384] {
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
        for &(site, ..) in MEASURED {
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
        match rlimit_stack() {
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
            None => assert!(rlimit_stack().is_none()),
        }
    }
}
