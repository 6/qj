//! `QJ_JQ_COMPAT=1` (see `src/compat.rs`) and the closed-descriptor
//! behaviour, from outside the process.
//!
//! jq is the expectation for compat mode, and `tests/jq_compat/corpus/
//! compat_mode.toml` checks that against the jq binary case by case. These
//! tests are the other half: that **without** the variable qj keeps its own
//! extensions and its sane answers, and that the two modes differ in exactly
//! the documented places. They need no jq.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use qj::compat::Site;

/// Runs qj with `args` in `dir`, optionally in compat mode.
fn run_in(dir: &Path, compat: bool, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if compat {
        cmd.env(qj::compat::ENV_VAR, "1");
    } else {
        cmd.env_remove(qj::compat::ENV_VAR);
    }
    cmd.output().expect("failed to run qj")
}

fn run(compat: bool, args: &[&str]) -> Output {
    run_in(Path::new("."), compat, args)
}

/// The environment of every qj that [`run_exact`] and [`run_stack`] start,
/// exactly: compat mode counts what argv and the environment take on the
/// stack ([`qj::compat::area_of`]), so a test that sizes a program from the
/// model has to know both.
fn exact_env(dir: &Path, compat: bool) -> Vec<String> {
    let mut env = vec![
        "PATH=/usr/bin:/bin".to_string(),
        format!("HOME={}", dir.display()),
    ];
    if compat {
        env.push(format!("{}=1", qj::compat::ENV_VAR));
    }
    let pad = PAD.with(std::cell::Cell::get);
    if pad > 0 {
        env.push(format!("PAD={}", "x".repeat(pad)));
    }
    env
}

thread_local! {
    /// Bytes of padding [`exact_env`] adds, in a variable of its own: a test
    /// that wants less of the stack for jq than a page-sized limit leaves can
    /// take it with the environment, which sits on the stack too.
    static PAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The stack compat mode sees as jq's in a qj started with `args` by
/// [`run_exact`] or [`run_stack`] under an `RLIMIT_STACK` of `rlimit` bytes:
/// the limit as the kernel applies it, less argv and the environment.
fn jq_stack(dir: &Path, compat: bool, rlimit: u64, args: &[&str]) -> u64 {
    let argv: Vec<&[u8]> = std::iter::once(env!("CARGO_BIN_EXE_qj"))
        .chain(args.iter().copied())
        .map(str::as_bytes)
        .collect();
    let env = exact_env(dir, compat);
    let area = qj::compat::area_of(argv, env.iter().map(|e| e.as_bytes()));
    qj::compat::effective_limit(rlimit) - area
}

/// This process's `RLIMIT_STACK`, which a child started without a shell
/// inherits; `None` when it is unlimited.
fn own_rlimit() -> Option<u64> {
    // SAFETY: getrlimit writes an rlimit into a valid out-pointer.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    let read = unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut lim) } == 0;
    // `rlim_t` is `u64` on macOS and Linux, and `i64` on FreeBSD, where a
    // limit is never negative.
    #[allow(clippy::unnecessary_cast)]
    let cur = lim.rlim_cur as u64;
    (read && lim.rlim_cur != libc::RLIM_INFINITY).then_some(cur)
}

/// [`run_in`] with exactly [`exact_env`] for an environment, so that
/// [`jq_stack`] knows what compat mode sees.
fn run_exact(dir: &Path, compat: bool, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qj"));
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for kv in exact_env(dir, compat) {
        let (k, v) = kv.split_once('=').expect("NAME=value");
        cmd.env(k, v);
    }
    cmd.output().expect("failed to run qj")
}

/// A directory holding `program` as `p.jq`, and the arguments that run it:
/// with the program in a file, argv is the same whatever its depth.
fn program_file(program: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("p.jq"), program).expect("write the program");
    dir
}

const PROGRAM_ARGS: &[&str] = &["-nc", "-f", "p.jq"];

/// [`run_exact`] with the child's `RLIMIT_STACK` set to `stack_kb`, so that a
/// test can reach a stack-overflow threshold without a huge value or a huge
/// chain of modules.
///
/// The limit is set by a shell rather than by `Command::pre_exec`, because
/// Darwin refuses `setrlimit(RLIMIT_STACK)` in a process forked from a
/// multi-threaded one (`EINVAL`), which every test harness is. The shell execs
/// qj through `env -i`, because a shell adds variables of its own (bash adds
/// `PWD` and `SHLVL`), and both keep argv, so qj sees exactly the arguments
/// below and [`exact_env`].
fn run_stack(dir: &Path, compat: bool, stack_kb: u64, args: &[&str]) -> Output {
    let script =
        format!("ulimit -s {stack_kb} || exit 99; ulimit -c 0; exec /usr/bin/env -i \"$@\"");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .arg("sh")
        .args(exact_env(dir, compat))
        .arg(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    cmd.output().expect("failed to run qj")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> Option<i32> {
    o.status.code()
}

fn signal(o: &Output) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    o.status.signal()
}

// ---------------------------------------------------------------------------
// qj's extensions
// ---------------------------------------------------------------------------

/// A directory with two files a glob can match and one gzip file.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("g1.json"), "{\"a\":1}\n").unwrap();
    std::fs::write(dir.path().join("g2.json"), "{\"b\":2}\n").unwrap();
    // `{"x":1}\n` gzipped (zero mtime, so the bytes are fixed).
    let gz: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xab, 0x56, 0xaa, 0x50, 0xb2,
        0x32, 0xac, 0xe5, 0x02, 0x00, 0x15, 0xf0, 0x31, 0x50, 0x08, 0x00, 0x00, 0x00,
    ];
    std::fs::write(dir.path().join("c.json.gz"), gz).unwrap();
    dir
}

#[test]
fn globs_expand_by_default_and_not_in_compat_mode() {
    let dir = fixture();
    let plain = run_in(dir.path(), false, &["-c", ".", "g*.json"]);
    assert_eq!(code(&plain), Some(0), "{}", stderr(&plain));
    assert_eq!(stdout(&plain), "{\"a\":1}\n{\"b\":2}\n");

    let compat = run_in(dir.path(), true, &["-c", ".", "g*.json"]);
    assert_eq!(code(&compat), Some(2));
    assert_eq!(
        stderr(&compat),
        "jq: error: Could not open file g*.json: No such file or directory\n"
    );
    assert_eq!(stdout(&compat), "");
}

#[test]
fn gzip_is_decompressed_by_default_and_not_in_compat_mode() {
    let dir = fixture();
    let plain = run_in(dir.path(), false, &["-c", ".", "c.json.gz"]);
    assert_eq!(code(&plain), Some(0), "{}", stderr(&plain));
    assert_eq!(stdout(&plain), "{\"x\":1}\n");

    let compat = run_in(dir.path(), true, &["-c", ".", "c.json.gz"]);
    assert_eq!(code(&compat), Some(5));
    assert!(
        stderr(&compat).starts_with("jq: parse error:"),
        "{}",
        stderr(&compat)
    );
}

#[test]
fn qj_only_options_are_unknown_in_compat_mode() {
    let dir = fixture();
    for args in [
        &["--threads", "2", "-c", ".", "g1.json"][..],
        &["--threads=2", "-c", ".", "g1.json"][..],
        &["--jsonl", "-c", ".", "g1.json"][..],
        &["--debug-timing", "-c", ".", "g1.json"][..],
    ] {
        let plain = run_in(dir.path(), false, args);
        assert_eq!(code(&plain), Some(0), "{args:?}: {}", stderr(&plain));
        assert_eq!(stdout(&plain), "{\"a\":1}\n", "{args:?}");

        let compat = run_in(dir.path(), true, args);
        assert_eq!(code(&compat), Some(2), "{args:?}");
        let name = args[0].split('=').next().unwrap();
        assert!(
            stderr(&compat).starts_with(&format!("jq: Unknown option {}", args[0])),
            "{args:?} ({name}): {}",
            stderr(&compat)
        );
    }
}

#[test]
fn compat_mode_leaves_ordinary_runs_alone() {
    let dir = fixture();
    for compat in [false, true] {
        let o = run_in(dir.path(), compat, &["-c", ".a", "g1.json"]);
        assert_eq!(code(&o), Some(0), "compat={compat}: {}", stderr(&o));
        assert_eq!(stdout(&o), "1\n", "compat={compat}");
    }
}

// ---------------------------------------------------------------------------
// Identity: qj's name and text by default, jq's in compat mode
// ---------------------------------------------------------------------------

#[test]
fn version_and_build_configuration_are_jqs_in_compat_mode() {
    let plain = run(false, &["--version"]);
    assert_eq!(
        stdout(&plain),
        format!("qj {}\n", env!("CARGO_PKG_VERSION"))
    );
    let compat = run(true, &["--version"]);
    assert_eq!(
        (code(&compat), stdout(&compat).as_str()),
        (Some(0), "jq-1.8.1\n")
    );

    let plain = run(false, &["--build-configuration"]);
    assert!(stdout(&plain).starts_with("qj "), "{}", stdout(&plain));
    let compat = run(true, &["--build-configuration"]);
    assert!(
        stdout(&compat).starts_with("--host="),
        "{}",
        stdout(&compat)
    );
    // `$JQ_BUILD_CONFIGURATION` is the same text.
    let var = run(true, &["-nr", "$JQ_BUILD_CONFIGURATION"]);
    assert_eq!(stdout(&var), stdout(&compat));
}

#[test]
fn help_and_usage_are_jqs_in_compat_mode() {
    let plain = run(false, &["-h"]);
    assert!(stdout(&plain).starts_with("qj - "), "{}", stdout(&plain));
    let compat = run(true, &["-h"]);
    assert_eq!(code(&compat), Some(0));
    let help = stdout(&compat);
    assert!(help.starts_with("jq - commandline JSON processor [version 1.8.1]\n\nUsage:\tjq "));
    assert!(!help.contains("qj"), "{help}");

    // `usage(2, 1)`: no program, `-f` given.
    let compat = run(true, &["-f"]);
    assert_eq!(code(&compat), Some(2));
    assert!(stderr(&compat).starts_with("jq - commandline JSON processor"));
    assert!(stderr(&compat).ends_with("For listing the command options, use jq --help.\n"));

    // die()'s hint.
    let plain = run(false, &["--bogus"]);
    assert_eq!(
        stderr(&plain),
        "qj: Unknown option --bogus\nUse qj --help for help with command-line options,\n\
         or see the jq manpage, or online docs  at https://jqlang.org\n"
    );
    let compat = run(true, &["--bogus"]);
    assert_eq!(
        stderr(&compat),
        "jq: Unknown option --bogus\nUse jq --help for help with command-line options,\n\
         or see the jq manpage, or online docs  at https://jqlang.org\n"
    );
}

#[test]
fn messages_say_jq_in_compat_mode() {
    for (args, qj, jq) in [
        (
            &["-n", "error(\"x\")"][..],
            "qj: error (at <unknown>): x\n",
            "jq: error (at <unknown>): x\n",
        ),
        (
            &["-n", "1 +"][..],
            "qj: error: syntax error, unexpected end of file at <top-level>, line 1, column 3:\n    1 +\n      ^\nqj: 1 compile error\n",
            "jq: error: syntax error, unexpected end of file at <top-level>, line 1, column 3:\n    1 +\n      ^\njq: 1 compile error\n",
        ),
    ] {
        assert_eq!(stderr(&run(false, args)), qj, "{args:?}");
        assert_eq!(stderr(&run(true, args)), jq, "{args:?}");
    }
}

// ---------------------------------------------------------------------------
// jq's crashes
// ---------------------------------------------------------------------------

#[test]
fn a_module_import_cycle_is_an_error_by_default_and_a_segfault_in_compat_mode() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.jq"), "import \"a\" as a;\ndef f: 1;\n").unwrap();
    let args = &["-nc", "-L", ".", "include \"a\"; 1"];

    let plain = run_in(dir.path(), false, args);
    assert_eq!(code(&plain), Some(3), "{}", stderr(&plain));
    assert!(
        stderr(&plain).contains("imports itself (import cycle)"),
        "{}",
        stderr(&plain)
    );

    let compat = run_in(dir.path(), true, args);
    assert_eq!(signal(&compat), Some(libc::SIGSEGV));
    assert_eq!(stdout(&compat), "");
    assert_eq!(stderr(&compat), "");
}

/// A directory with `n` modules, each importing the next, and the program that
/// enters the chain: `m0` imports `m1` … imports `m{n-1}`, which imports
/// nothing. `f` is defined in every one of them, so the chain is also bound and
/// referenced end to end.
fn module_chain(n: u64) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    for i in 0..n {
        let body = if i + 1 < n {
            format!("import \"m{}\" as m;\ndef f: 1;\n", i + 1)
        } else {
            "def f: 1;\n".to_string()
        };
        std::fs::write(dir.path().join(format!("m{i}.jq")), body).expect("write module");
    }
    (dir, "include \"m0\"; f".to_string())
}

/// A small stack, so that a chain long enough to reach jq's threshold is a few
/// hundred modules rather than twenty thousand. qj's own recursion over one
/// module's syntax needs far less than this.
const SMALL_STACK_KB: u64 = 256;

/// jq's `load_library` and `process_dependencies` call each other once per
/// module, so a long enough chain of imports overflows its stack. qj links with
/// a loop, so by default it answers however long the chain is — including
/// chains far past the threshold where jq dies, and on a stack far too small
/// for jq to get there.
#[test]
fn a_long_module_chain_works_by_default() {
    let budget = Site::Modules.frame_budget_at(SMALL_STACK_KB * 1024, 0);
    // Twice what jq could do on this stack, to show the loop has no threshold
    // of its own.
    for n in [1, 2, budget + budget / 2, budget * 2] {
        let (dir, prog) = module_chain(n);
        let o = run_stack(
            dir.path(),
            false,
            SMALL_STACK_KB,
            &["-nc", "-L", ".", &prog],
        );
        assert_eq!(code(&o), Some(0), "{n} modules: {}", stderr(&o));
        assert_eq!(stdout(&o), "1\n", "{n} modules");
    }
    // And a long chain on the stack the test process has.
    let (dir, prog) = module_chain(3000);
    let o = run_in(dir.path(), false, &["-nc", "-L", ".", &prog]);
    assert_eq!(code(&o), Some(0), "3000 modules: {}", stderr(&o));
    assert_eq!(stdout(&o), "1\n");
}

/// In compat mode the same chain dies where jq's stack runs out: one module
/// short of the budget answers, one past it is a `SIGSEGV`. Checked on the
/// small stack, and at the stack the test process has.
#[test]
fn a_long_module_chain_segfaults_in_compat_mode() {
    // Two small limits: enough to show the threshold follows `ulimit -s`, and
    // short enough that the chains are hundreds of modules, not thousands.
    for stack in [SMALL_STACK_KB * 1024, 1024 * 1024] {
        let (probe, prog) = module_chain(1);
        let args = &["-nc", "-L", ".", prog.as_str()];
        let budget = Site::Modules.frame_budget_at(jq_stack(probe.path(), true, stack, args), 0);
        assert!(
            budget > 100,
            "implausible module budget {budget} at {stack}"
        );
        // The chain runs `n` modules below `load_program`'s own frame, so the
        // deepest chain that fits is `budget - 1`.
        for (n, dies) in [(budget - 1, false), (budget, true)] {
            let (dir, prog) = module_chain(n);
            let args = &["-nc", "-L", ".", prog.as_str()];
            // (Every temporary directory's name is as long as the probe's.)
            assert_eq!(
                jq_stack(dir.path(), true, stack, args),
                jq_stack(probe.path(), true, stack, args)
            );
            let o = run_stack(dir.path(), true, stack / 1024, args);
            if dies {
                assert_eq!(
                    signal(&o),
                    Some(libc::SIGSEGV),
                    "{n} modules at {stack} B: should crash ({:?}, {})",
                    o.status,
                    stderr(&o)
                );
                assert_eq!(stdout(&o), "", "{n} modules at {stack} B");
            } else {
                assert_eq!(
                    code(&o),
                    Some(0),
                    "{n} modules at {stack} B: {}",
                    stderr(&o)
                );
                assert_eq!(stdout(&o), "1\n", "{n} modules at {stack} B");
            }
        }
    }
}

#[test]
fn a_module_imported_twice_is_not_a_cycle() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.jq"), "def f: 1;\n").unwrap();
    std::fs::write(dir.path().join("b.jq"), "include \"a\";\ndef g: f + 1;\n").unwrap();
    for compat in [false, true] {
        let o = run_in(
            dir.path(),
            compat,
            &["-nc", "-L", ".", "include \"a\"; include \"b\"; f + g"],
        );
        assert_eq!(code(&o), Some(0), "compat={compat}: {}", stderr(&o));
        assert_eq!(stdout(&o), "3\n", "compat={compat}");
    }
}

/// jq's `jv_free` recurses once per level of nesting. The depths here are far
/// from the threshold the stack limit gives (`qj::compat::free_frame_budget`:
/// about 130,000 levels at macOS's default 8 MB, twice that at the 16 MB of
/// GitHub's Linux runners), so the test doesn't depend on the exact model or
/// on `ulimit -s`.
#[test]
fn deeply_nested_values_segfault_only_in_compat_mode() {
    let Some(budget) = qj::compat::free_frame_budget() else {
        // An unlimited stack: jq doesn't overflow either.
        return;
    };
    let deep = format!(
        "reduce range({}) as $i (null;[.]) | length",
        budget + budget / 2
    );
    let shallow = format!(
        "reduce range({}) as $i (null;[.]) | length",
        (budget / 4).min(20_000)
    );

    let plain = run(false, &["-nc", &deep]);
    assert_eq!(code(&plain), Some(0), "{}", stderr(&plain));
    assert_eq!(stdout(&plain), "1\n");

    let compat = run(true, &["-nc", &deep]);
    assert_eq!(signal(&compat), Some(libc::SIGSEGV));
    // jq's stdout buffer is lost when it crashes, so nothing is printed.
    assert_eq!(stdout(&compat), "");

    // Not deep enough: compat mode changes nothing.
    let ok = run(true, &["-nc", &shallow]);
    assert_eq!(code(&ok), Some(0), "{}", stderr(&ok));
    assert_eq!(stdout(&ok), "1\n");
}

/// The threshold moves with `ulimit -s`, which the model tracks; a value
/// deeper than the limit crashes, one shallower doesn't.
#[test]
fn the_crash_depth_follows_the_stack_limit() {
    // An unlimited stack: jq doesn't overflow either.
    let Some(rlimit) = own_rlimit() else {
        return;
    };
    let probe = program_file("");
    let budget = Site::Free.frame_budget_at(jq_stack(probe.path(), true, rlimit, PROGRAM_ARGS), 0);
    assert!(budget > 1000, "implausible frame budget {budget}");
    // `reduce range(n) as $i (null;[.])` nests n arrays around a null, so
    // freeing it takes n + 1 frames: the deepest n that fits is budget - 1.
    let deepest = budget - 1;
    for (n, dies) in [(deepest, false), (deepest + 1, true), (budget + 1000, true)] {
        let dir = program_file(&format!("reduce range({n}) as $i (null;[.]) | length"));
        let o = run_exact(dir.path(), true, PROGRAM_ARGS);
        if dies {
            assert_eq!(signal(&o), Some(libc::SIGSEGV), "depth {n} should crash");
        } else {
            assert_eq!(code(&o), Some(0), "depth {n} should not crash");
        }
    }
}

// ---------------------------------------------------------------------------
// jq's other recursions over values and paths
// ---------------------------------------------------------------------------

/// A program nesting `n` arrays (or objects) around a `null` in `$v`, or
/// building a path of `n` elements in `$p`.
fn deep(n: u64, obj: bool) -> String {
    if obj {
        format!("reduce range({n}) as $i (null;{{a:.}})")
    } else {
        format!("reduce range({n}) as $i (null;[.])")
    }
}

/// Programs that drive one of jq's recursions `n` levels deep, one per
/// [`Site`], each answering `true`, a number, or nothing when it fits.
fn program(site: Site, n: u64) -> String {
    let v = deep(n, matches!(site, Site::Merge));
    match site {
        // `n + 1` frees: one per array, one for the null.
        Site::Free => format!("{v} | length"),
        Site::Compare => format!("({v}) as $a | ({v}) as $b | $a == $b"),
        Site::Contains => format!("({v}) as $x | $x | contains($x)"),
        Site::Merge => format!("({v}) as $x | ($x * $x) | length"),
        Site::Setpath => format!("[range({n})|0] as $p | null | setpath($p; 1) | length"),
        // The value has to be as deep as the path for jq to descend it.
        Site::Delpaths => format!(
            "({}) as $x | [$x] | delpaths([[0] + [range({n})|0]]) | length",
            deep(n + 2, false)
        ),
        // A chain of imports needs files, so it has its own test.
        Site::Modules => unreachable!("Site::Modules needs a module tree"),
        // The compiler's recursions are driven by the shape of a *program*, not
        // of a value: see `nested_closures_crash_where_jqs_binding_does` and
        // `nested_defs_crash_where_jqs_compile_does`.
        Site::Bind | Site::Compile | Site::ExpandArgs => {
            unreachable!("{site:?} is driven by the program, not by a value")
        }
        // The printer's depth is capped at `MAX_PRINT_DEPTH`, so it has a
        // threshold only on a tiny stack: see
        // `printing_a_deep_value_crashes_where_jqs_printer_does`.
        Site::Print | Site::Dump => unreachable!("{site:?} needs a small stack"),
        // A regex's depth is its pattern's: see
        // `a_nested_regex_crashes_where_jqs_oniguruma_does`.
        Site::RegexParse | Site::RegexTree => unreachable!("{site:?} is driven by a pattern"),
    }
}

/// Every site, with a name for the failure messages and how many frames
/// [`program`] costs jq for `n` levels: `n + 1` everywhere but the merge,
/// which stops at the innermost object rather than descending into the
/// `null` below it.
const SITES: &[(Site, &str, u64)] = &[
    (Site::Free, "free", 1),
    (Site::Compare, "compare", 1),
    (Site::Contains, "contains", 1),
    (Site::Merge, "merge", 0),
    (Site::Setpath, "setpath", 1),
    (Site::Delpaths, "delpaths", 1),
];

/// Each of jq's recursions has a threshold of its own, which compat mode
/// reproduces and the default has no reason to: the deepest value the site's
/// budget allows answers, and one level more dies.
///
/// The depths come from the model rather than from a constant, so the test
/// follows `ulimit -s` (`qj::compat::Site::frame_budget`).
#[test]
fn every_recursion_has_its_own_crash_depth_in_compat_mode() {
    // An unlimited stack: jq doesn't overflow either.
    let Some(rlimit) = own_rlimit() else {
        return;
    };
    let probe = program_file("");
    let stack = jq_stack(probe.path(), true, rlimit, PROGRAM_ARGS);
    for &(site, name, extra) in SITES {
        let budget = site.frame_budget_at(stack, 0);
        assert!(budget > 1000, "{name}: implausible budget {budget}");
        let deepest = budget - extra;
        for (n, dies) in [(deepest, false), (deepest + 1, true)] {
            let prog = program(site, n);
            let dir = program_file(&prog);
            let o = run_exact(dir.path(), true, PROGRAM_ARGS);
            if dies {
                assert_eq!(
                    signal(&o),
                    Some(libc::SIGSEGV),
                    "{name} at {n}: should crash ({:?}, {})",
                    o.status,
                    stderr(&o)
                );
                // jq's buffered stdout goes with it.
                assert_eq!(stdout(&o), "", "{name} at {n}");
            } else {
                assert_eq!(code(&o), Some(0), "{name} at {n}: {}", stderr(&o));
            }
            // Without the variable qj answers at either depth.
            let plain = run_exact(dir.path(), false, PROGRAM_ARGS);
            assert_eq!(code(&plain), Some(0), "{name} at {n}: {}", stderr(&plain));
        }
    }
}

/// The sites are independent: a value deep enough to kill jq comparing it
/// still dies comparing it even though freeing it would have been fine, and
/// one that survives every site's threshold survives the whole program.
#[test]
fn a_value_between_two_thresholds_dies_at_the_lower_one() {
    let (Some(compare), Some(free)) = (Site::Compare.frame_budget(), Site::Free.frame_budget())
    else {
        return;
    };
    // jv_equal's frame is twice jv_free's, so there is always a range of
    // depths that only the comparison cannot survive.
    assert!(compare < free, "{compare} should be below {free}");
    let between = (compare + free) / 2;
    let compared = run(true, &["-nc", &program(Site::Compare, between)]);
    assert_eq!(
        signal(&compared),
        Some(libc::SIGSEGV),
        "comparing {between}"
    );
    // The same value, only freed: no crash.
    let freed = run(true, &["-nc", &program(Site::Free, between)]);
    assert_eq!(
        code(&freed),
        Some(0),
        "freeing {between}: {}",
        stderr(&freed)
    );
    assert_eq!(stdout(&freed), "1\n");
}

/// Only the depth jq's traversal reaches counts. `jv_equal` answers from the
/// pointer when both sides are the same allocation, and stops at the first
/// difference, so these never recurse however deep the value is.
#[test]
fn comparisons_that_jq_answers_without_recursing_do_not_crash() {
    let (Some(compare), Some(free)) = (Site::Compare.frame_budget(), Site::Free.frame_budget())
    else {
        return;
    };
    // Well past the comparison's threshold, and short of the one for freeing
    // the value at the end of the program (which is not what is being tested
    // here).
    let v = deep((compare + free) / 2, false);
    for (prog, want) in [
        // jv_equal's shortcut for one allocation. (jv_cmp has no such
        // shortcut, so `[$x, $x] | sort` does recurse, and crashes.)
        (format!("({v}) as $x | $x == $x"), "true\n"),
        (format!("({v}) as $x | [$x, $x] | .[0] == .[1]"), "true\n"),
        (format!("({v}) as $x | [$x] | index([$x])"), "0\n"),
        // Lengths that differ: unequal before an element is looked at.
        (
            format!("({v}) as $x | [[$x], [$x, 1]] | .[0] == .[1]"),
            "false\n",
        ),
        // jv_cmp stops at the first element that differs.
        (
            format!("({v}) as $x | [[1, $x], [2, $x]] | .[0] < .[1]"),
            "true\n",
        ),
        // jv_object_merge_recursive descends only where both sides are
        // objects.
        (
            format!("({v}) as $x | ({{a:1}} * {{a:$x}}) | length"),
            "1\n",
        ),
    ] {
        let o = run(true, &["-nc", &prog]);
        assert_eq!(code(&o), Some(0), "{prog}: {}", stderr(&o));
        assert_eq!(stdout(&o), want, "{prog}");
    }
}

/// jq compares path elements and frees values from inside `delpaths_sorted`,
/// with the frames for the levels above still on the stack, so the same value
/// kills it sooner the deeper the paths go. The depth that survives at the top
/// level must die a thousand levels down.
#[test]
fn a_recursion_inside_another_gets_the_stack_that_is_left() {
    let (Some(compare), Some(delpaths)) =
        (Site::Compare.frame_budget(), Site::Delpaths.frame_budget())
    else {
        return;
    };
    // 1,000 `delpaths_sorted` frames cost this many levels of comparison.
    let prefix = 1000;
    assert!(prefix < delpaths / 2, "{prefix} levels should fit");
    // Two paths that share a prefix of zeros and end in a deep value, so the
    // comparison that groups them runs `prefix + 1` levels down. (Deleting a
    // value at an array index is an error, which is what `catch` is for; jq
    // compares the keys first.)
    let program = |depth: u64| {
        let v = deep(depth, false);
        format!(
            "({v}) as $a | ({v}) as $b | (reduce range({prefix}) as $i ([1]; [.])) as $v | $v \
             | try delpaths([[range({prefix})|0] + [$a], [range({prefix})|0] + [$b]]) catch \"err\""
        )
    };
    // Inside the comparison's own budget, and past what is left of it that far
    // into `delpaths`: a `delpaths_sorted` frame costs more than a `jv_cmp`
    // one, so `prefix` levels of it cost more than `prefix` levels of
    // comparison.
    let between = compare - prefix;
    let deeper = run(true, &["-nc", &program(between)]);
    assert_eq!(
        signal(&deeper),
        Some(libc::SIGSEGV),
        "{:?} {}",
        deeper.status,
        stderr(&deeper)
    );
    // The same comparison at the top level answers.
    let shallow = format!("({0}) as $a | ({0}) as $b | $a == $b", deep(between, false));
    let o = run(true, &["-nc", &shallow]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "true\n");
}

/// `jv_getpath` is jq's one path recursion that is a tail call, which both
/// release compilers turn into a loop: no path is long enough to overflow it,
/// however small the stack.
#[test]
fn getpath_does_not_crash_however_long_the_path_is() {
    let Some(budget) = Site::Setpath.frame_budget() else {
        return;
    };
    let n = budget * 4;
    for (prog, want) in [
        (
            format!("[range({n})|0] as $p | null | getpath($p)"),
            "null\n",
        ),
        (
            format!(
                "({}) as $x | [range({n})|0] as $p | $x | getpath($p)",
                deep(20, false)
            ),
            "null\n",
        ),
    ] {
        let o = run(true, &["-nc", &prog]);
        assert_eq!(code(&o), Some(0), "{prog}: {}", stderr(&o));
        assert_eq!(stdout(&o), want, "{prog}");
    }
}

#[test]
fn run_tests_without_a_count_segfaults_in_both_modes() {
    // jq_test.c's `atoi(argv[i+1])` is `atoi(NULL)` at the end of argv.
    for compat in [false, true] {
        for opt in ["--skip", "--take"] {
            let o = run(compat, &["--run-tests", opt]);
            assert_eq!(
                signal(&o),
                Some(libc::SIGSEGV),
                "compat={compat} {opt}: {:?}",
                o.status
            );
        }
    }
}

// ---------------------------------------------------------------------------
// jq's recursions over the program: the compiler
// ---------------------------------------------------------------------------

/// A program nesting `n` closures: each `select(f)` call holds a lambda whose
/// body is the next one, so binding descends two frames a level.
fn nested_closures(n: u64) -> String {
    format!(
        "{}true{}",
        "select(".repeat(n as usize),
        ")".repeat(n as usize)
    )
}

/// The same nesting through a C function's arguments: `. + . + ... + .` is
/// `_plus(lambda(rest); lambda(.))`, left-associated, so the chain nests in the
/// first argument. Unlike `select(...)`, bison reduces it as it goes, so this
/// one is not bounded by `YYMAXDEPTH` at all.
fn nested_binops(n: u64) -> String {
    vec!["."; n as usize + 1].join(" + ")
}

/// A chain of `n` definitions, each in the body of the one before, which jq
/// compiles by recursing once per level. Binding does *not* descend them: a
/// definition bound to itself has `any_unbound == 0`, so later walks skip it.
fn nested_defs(n: u64) -> String {
    let mut s = String::new();
    for i in 0..n {
        s += &format!("def f{i}: ");
    }
    s += "1";
    for i in (0..n).rev() {
        s += &format!("; f{i}");
    }
    s
}

/// The deepest `select(...)` nesting (or chain of binops) compat-mode qj
/// compiles, from a file, at `stack_kb`.
///
/// Binding recurses into an instruction's closure body *and* its argument list,
/// so a level costs two frames: the call at level `k` is bound at frame
/// `2k - 1`, the lambda holding the next level at `2k`. The innermost lambda's
/// body has nothing unbound in it, so jq skips it, and `n` levels reach frame
/// `2n - 1`.
fn deepest_closures(stack_kb: u64) -> u64 {
    Site::Bind.frame_budget_at(file_stack(stack_kb), 0) / 2
}

/// [`jq_stack`] at `stack_kb` for a compat-mode qj running [`program_file`]'s
/// program with [`PROGRAM_ARGS`] (every temporary directory's name is as long).
fn file_stack(stack_kb: u64) -> u64 {
    let probe = program_file("");
    jq_stack(probe.path(), true, stack_kb * 1024, PROGRAM_ARGS)
}

/// [`run_stack`] of `program` from a file.
fn run_stack_program(compat: bool, stack_kb: u64, program: &str) -> Output {
    let dir = program_file(program);
    run_stack(dir.path(), compat, stack_kb, PROGRAM_ARGS)
}

/// The deepest chain of definitions compat-mode qj compiles, from a file, at
/// `stack_kb`: `compile` recurses once per level, and a chain of `n` reaches
/// level `n + 1` (the top-level function is the first).
fn deepest_defs(stack_kb: u64) -> u64 {
    let stack = file_stack(stack_kb);
    let budget = Site::Compile.frame_budget_at(stack, 0);
    // What `compile` calls at its deepest level still has to fit, which is what
    // the budget's headroom is for: check that it does.
    assert!(
        Site::ExpandArgs.frame_budget_at(stack, budget * Site::Compile.frame_bytes()) >= 1,
        "no room for expand_call_arglist at compile level {budget}"
    );
    budget - 1
}

/// jq's `block_bind_subblock_inner` recurses over a program's closures, so a
/// deeply nested program overflows its stack while parsing. In compat mode qj
/// dies at the same nesting, one level short answers, and by default neither
/// depth is a problem.
#[test]
fn nested_closures_crash_where_jqs_binding_does() {
    for stack_kb in [SMALL_STACK_KB, 512] {
        let deepest = deepest_closures(stack_kb);
        assert!(deepest > 100, "implausible closure depth {deepest}");
        for (shape, prog) in [
            ("select", nested_closures(deepest)),
            ("binop", nested_binops(deepest)),
        ] {
            let o = run_stack_program(true, stack_kb, &prog);
            assert_eq!(
                code(&o),
                Some(0),
                "{shape} {deepest} at {stack_kb} KB should compile: {}",
                stderr(&o)
            );
        }
        for (shape, prog) in [
            ("select", nested_closures(deepest + 1)),
            ("binop", nested_binops(deepest + 1)),
        ] {
            let o = run_stack_program(true, stack_kb, &prog);
            assert_eq!(
                signal(&o),
                Some(libc::SIGSEGV),
                "{shape} {} at {stack_kb} KB should crash ({:?}, {})",
                deepest + 1,
                o.status,
                stderr(&o)
            );
            assert_eq!(stdout(&o), "", "{shape} at {stack_kb} KB");
            // By default the same program compiles and runs.
            let plain = run_stack_program(false, stack_kb, &prog);
            assert_eq!(
                code(&plain),
                Some(0),
                "{shape} {} at {stack_kb} KB by default: {}",
                deepest + 1,
                stderr(&plain)
            );
        }
    }
}

/// `compile` recurses once per nested definition, with `expand_call_arglist` on
/// top of it — the one recursion nested `def`s drive, and the only one whose
/// threshold they reach.
#[test]
fn nested_defs_crash_where_jqs_compile_does() {
    // Bison stops at `YYMAXDEPTH` (10,000 states) after about 3,330 nested
    // definitions, which is why this needs a small stack.
    let stack_kb = SMALL_STACK_KB;
    let deepest = deepest_defs(stack_kb);
    assert!(
        (100..3000).contains(&deepest),
        "implausible def depth {deepest}"
    );
    let ok = run_stack_program(true, stack_kb, &nested_defs(deepest));
    assert_eq!(
        code(&ok),
        Some(0),
        "{deepest} defs should compile: {}",
        stderr(&ok)
    );
    assert_eq!(stdout(&ok), "1\n");
    let prog = nested_defs(deepest + 1);
    let o = run_stack_program(true, stack_kb, &prog);
    assert_eq!(
        signal(&o),
        Some(libc::SIGSEGV),
        "{} defs should crash ({:?}, {})",
        deepest + 1,
        o.status,
        stderr(&o)
    );
    let plain = run_stack_program(false, stack_kb, &prog);
    assert_eq!(code(&plain), Some(0), "by default: {}", stderr(&plain));
    assert_eq!(stdout(&plain), "1\n");
}

/// jq runs parser.y's actions as bison reduces, so a program that fails to
/// parse has already driven the binding recursion over everything the parser
/// did reduce: jq crashes instead of reporting the syntax error. qj parses
/// first and lowers afterwards, and in compat mode replays that lowering.
#[test]
fn a_syntax_error_after_deep_nesting_crashes_where_jqs_actions_do() {
    let stack_kb = SMALL_STACK_KB;
    // One level deeper than [`nested_closures_crash_where_jqs_binding_does`]
    // needs: the deepest walk of a program that *does* parse is the one that
    // binds the builtins to it, which starts a frame above the parse's own
    // walks, and a program that fails to parse never gets there. jq's threshold
    // is the same for both (its parse-time walk is what runs out), so this is
    // one more level of the margin, not a divergence.
    let deepest = deepest_closures(stack_kb) + 1;
    for (shape, deep) in [
        ("select", nested_closures(deepest + 1)),
        ("binop", nested_binops(deepest + 1)),
    ] {
        let prog = format!("{deep} | %%%");
        let o = run_stack_program(true, stack_kb, &prog);
        assert_eq!(
            signal(&o),
            Some(libc::SIGSEGV),
            "{shape} + a syntax error should crash ({:?}, {})",
            o.status,
            stderr(&o)
        );
        // By default qj reports the error, as it does for a shallow program.
        let plain = run_stack_program(false, stack_kb, &prog);
        assert_eq!(
            code(&plain),
            Some(3),
            "{shape} by default: {}",
            stderr(&plain)
        );
        assert!(
            stderr(&plain).starts_with("qj: error: syntax error, unexpected '%'"),
            "{shape} by default: {}",
            stderr(&plain)
        );
    }
}

/// `jv_dump_term` recurses once per level of the value it prints, and stops at
/// `MAX_PRINT_DEPTH` (256) — so it can only overflow on a stack of about 80 KB
/// or less, and there it does. qj's own printer recurses too, with a frame of
/// its own; in compat mode it dies at jq's depth instead.
#[test]
fn printing_a_deep_value_crashes_where_jqs_printer_does() {
    // qj's own frames are on its own thread's stack, so an unoptimized build,
    // whose frames are several times an optimized one's, dies where the model
    // puts it too.
    for stack_kb in [48, 64] {
        // `reduce range(n) as $i (0;[.])` nests n arrays around a 0, which
        // takes n + 1 frames to print.
        let deepest = Site::Print.frame_budget_at(file_stack(stack_kb), 0) - 1;
        assert!(
            (20..257).contains(&deepest),
            "implausible print depth {deepest} at {stack_kb} KB"
        );
        for (n, dies) in [(deepest, false), (deepest + 1, true)] {
            let prog = format!("reduce range({n}) as $i (0;[.])");
            let o = run_stack_program(true, stack_kb, &prog);
            if dies {
                assert_eq!(
                    signal(&o),
                    Some(libc::SIGSEGV),
                    "printing {n} deep at {stack_kb} KB should crash ({:?})",
                    o.status
                );
                assert_eq!(stdout(&o), "", "printing {n} deep at {stack_kb} KB");
            } else {
                assert_eq!(
                    code(&o),
                    Some(0),
                    "printing {n} deep at {stack_kb} KB: {}",
                    stderr(&o)
                );
            }
        }
    }
    // Past the cap jq stops descending, so a value of any depth prints on a
    // stack that can hold the cap.
    let o = run(
        true,
        &["-nc", "reduce range(2000) as $i (0;[.]) | tojson | length"],
    );
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
}

/// Nothing about the compiler's thresholds fires at a normal stack limit: the
/// deepest program bison will parse is far inside them, and the checks cost
/// nothing there.
#[test]
fn the_deepest_program_bison_parses_compiles_at_the_default_stack() {
    // 4,990 nested `select(...)` and 3,330 nested definitions are bison's
    // limits; jq needs a 2 MB stack for the first, which every platform's
    // default exceeds.
    for prog in [
        nested_closures(4_900),
        nested_defs(3_300),
        nested_binops(9_900),
    ] {
        for compat in [false, true] {
            let o = run(compat, &["-nc", &prog]);
            assert_eq!(
                code(&o),
                Some(0),
                "compat={compat}: {}",
                &stderr(&o)[..stderr(&o).len().min(200)]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Small stacks: jq's start-up, its test loop, regexes, dumps, native frees
// ---------------------------------------------------------------------------

/// The page size the kernel rounds `RLIMIT_STACK` to.
fn page_kb() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (page as u64).max(4096) / 1024
}

/// The smallest limit (a page multiple, in KB) at which compat mode sees at
/// least `need` bytes of jq's stack for a qj started with `args` from a
/// program file.
fn limit_for(need: u64, args: &[&str]) -> u64 {
    let probe = program_file("");
    let step = page_kb();
    let mut kb = step;
    while jq_stack(probe.path(), true, kb * 1024, args) < need {
        kb += step;
    }
    kb
}

/// [`run_stack`], on Linux with the kernel's stack randomization off where
/// `setarch` is installed (`None` where it isn't): qj's own start-up (the
/// dynamic loader's) needs about 6 KB of a limit this small, which the
/// randomization would sometimes take away.
fn run_stack_norandom(dir: &Path, compat: bool, stack_kb: u64, args: &[&str]) -> Option<Output> {
    if !cfg!(target_os = "linux") {
        return Some(run_stack(dir, compat, stack_kb, args));
    }
    let setarch = ["/usr/bin/setarch", "/bin/setarch"]
        .into_iter()
        .find(|p| Path::new(p).exists())?;
    let script = format!(
        "ulimit -s {stack_kb} || exit 99; ulimit -c 0; exec {setarch} \"$(uname -m)\" -R /usr/bin/env -i \"$@\""
    );
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .arg("sh")
        .args(exact_env(dir, compat))
        .arg(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    Some(cmd.output().expect("failed to run qj"))
}

fn assert_dies(o: &Output, what: &str) {
    assert_eq!(
        signal(o),
        Some(libc::SIGSEGV),
        "{what} should crash ({:?}, {})",
        o.status,
        stderr(o)
    );
    assert_eq!(stdout(o), "", "{what}: nothing is written");
}

/// On Linux, where jq is linked statically, its start-up needs about 5 KB
/// (the help text, the version, a usage error) and compiling a program about
/// 9 KB; between the two, compat mode prints the version and dies compiling.
/// (On macOS dyld needs more than either, for jq and qj alike.)
#[test]
fn compat_mode_needs_what_jqs_start_and_compiler_do() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return;
    }
    let needs = qj::compat::fixed_needs();
    let args = ["-nc", "-f", "p.jq"];
    // The first limit where jq starts, and where its compiler still dies.
    let kb = limit_for(needs.start, &args);
    let dir = program_file("1");
    assert!(jq_stack(dir.path(), true, kb * 1024, &args) < needs.compile);
    let Some(o) = run_stack_norandom(dir.path(), true, kb, &args) else {
        return; // no setarch
    };
    assert_dies(&o, &format!("compiling at {kb} KB"));
    let version =
        run_stack_norandom(dir.path(), true, kb, &["--version"]).expect("setarch was there");
    assert_eq!(code(&version), Some(0), "{}", stderr(&version));
    assert_eq!(stdout(&version), "jq-1.8.1\n");
    // Below the start-up's need, even the version dies; by default qj answers.
    let below = kb - page_kb();
    if jq_stack(dir.path(), true, below * 1024, &["--version"]) < needs.start {
        let o = run_stack_norandom(dir.path(), true, below, &["--version"]).expect("setarch");
        assert_dies(&o, &format!("starting at {below} KB"));
    }
    let plain = run_stack_norandom(dir.path(), false, kb, &args).expect("setarch");
    assert_eq!(code(&plain), Some(0), "{}", stderr(&plain));
    assert_eq!(stdout(&plain), "1\n");
}

/// jq's `--run-tests` keeps its test loop's buffers on the stack, 28 KB on
/// macOS and 12 KB on Linux above everything it runs: on a stack that holds a
/// program but not the loop, compat mode dies where the program answers.
#[test]
fn run_tests_needs_jqs_test_loop() {
    let needs = qj::compat::fixed_needs();
    let dir = program_file("1");
    std::fs::write(dir.path().join("t.test"), ".\n1\n1\n\n").expect("write the tests");
    let args = ["--run-tests", "t.test"];
    // Where jq starts and compiles a program (dyld needs more than the
    // compiler on macOS), but not where its test loop fits.
    let kb = limit_for(needs.start.max(needs.compile), &args);
    assert!(jq_stack(dir.path(), true, kb * 1024, &args) < needs.run_tests);
    let o = run_stack(dir.path(), true, kb, &args);
    assert_dies(&o, &format!("--run-tests at {kb} KB"));
    let plain = run_stack(dir.path(), false, kb, &args);
    assert_eq!(code(&plain), Some(0), "{}", stderr(&plain));
    assert!(
        stdout(&plain).contains("1 of 1 tests passed"),
        "{}",
        stdout(&plain)
    );
    // With room for the loop, compat mode runs the tests too.
    let kb = limit_for(needs.run_tests, &args);
    let o = run_stack(dir.path(), true, kb, &args);
    assert_eq!(code(&o), Some(0), "at {kb} KB: {}", stderr(&o));
    assert!(stdout(&o).contains("1 of 1 tests passed"), "{}", stdout(&o));
}

/// Oniguruma parses a pattern by recursing once per group, and walks the
/// parsed tree once per node: both overflow jq's stack on a small one, at
/// their own depths, and compat mode dies where jq does.
#[test]
fn a_nested_regex_crashes_where_jqs_oniguruma_does() {
    let stack_kb = SMALL_STACK_KB;
    let stack = file_stack(stack_kb);
    let parse = Site::RegexParse.frame_budget_at(stack, 0);
    let tree = Site::RegexTree.frame_budget_at(stack, 0);
    // `n` groups around an alternation: the parser has a level for the
    // pattern and one per group, the tree `n + 2` nodes; the parser runs out
    // first.
    let groups = (parse - 1).min(tree - 2);
    // `n` quantified groups: two nodes a level, and one for the leaf.
    let quantified = (tree - 1) / 2;
    assert!(
        (100..500).contains(&groups) && quantified < groups,
        "implausible regex depths {groups}, {quantified} at {stack_kb} KB"
    );
    for (shape, n, open, close, mid) in [
        ("groups", groups, "(", ")", "a|b"),
        ("quantified", quantified, "(", ")*", "a"),
    ] {
        let prog = |n: u64| {
            let pattern = format!(
                "{}{mid}{}",
                open.repeat(n as usize),
                close.repeat(n as usize)
            );
            format!("\"ab\" | test(\"{pattern}\")")
        };
        let ok = run_stack_program(true, stack_kb, &prog(n));
        assert_eq!(code(&ok), Some(0), "{shape} {n}: {}", stderr(&ok));
        assert_eq!(stdout(&ok), "true\n", "{shape} {n}");
        let o = run_stack_program(true, stack_kb, &prog(n + 1));
        assert_dies(&o, &format!("{shape} {} at {stack_kb} KB", n + 1));
        let plain = run_stack_program(false, stack_kb, &prog(n + 1));
        assert_eq!(
            code(&plain),
            Some(0),
            "{shape} by default: {}",
            stderr(&plain)
        );
        assert_eq!(stdout(&plain), "true\n");
    }
}

/// jq prints a result from `main.c`, and dumps a value for `tojson` from
/// deeper in its stack, inside the VM: the same value can print and still
/// kill jq converted to a string.
#[test]
fn a_dump_inside_the_program_has_less_stack_than_the_output() {
    let stack_kb = 64;
    let stack = file_stack(stack_kb);
    let (print, dump) = (
        Site::Print.frame_budget_at(stack, 0),
        Site::Dump.frame_budget_at(stack, 0),
    );
    assert!(dump < print && print < 258, "{dump} {print}");
    // `n` arrays around a 0 take `n + 1` frames to dump.
    let n = dump;
    let printed = run_stack_program(true, stack_kb, &format!("reduce range({n}) as $i (0;[.])"));
    assert_eq!(code(&printed), Some(0), "{}", stderr(&printed));
    let dumped = run_stack_program(
        true,
        stack_kb,
        &format!("reduce range({n}) as $i (0;[.]) | tojson | length"),
    );
    assert_dies(&dumped, &format!("tojson of {n} levels at {stack_kb} KB"));
}

/// qj frees a value natively up to 256 levels deep; where jq's `jv_free` has
/// fewer frames than that, compat mode stops short of them, so that a value
/// too deep for jq is checked however shallow it is.
#[test]
fn a_shallow_value_on_a_small_stack_is_checked_as_it_is_freed() {
    // The first limit with room to start and compile, with the environment
    // taking what jq's stack would have beyond 255 frames for `jv_free` (on
    // macOS, whose pages are 16 KB, that is most of a page): the most frames
    // short of qj's 256 leave the platform's loader the most room.
    let needs = qj::compat::fixed_needs();
    let need = needs.start.max(needs.compile);
    let kb = limit_for(need, PROGRAM_ARGS);
    let unpadded = file_stack(kb);
    let pad = if Site::Free.frame_budget_at(unpadded, 0) < 256 {
        0
    } else {
        // A byte of padding makes the variable; each byte more takes one.
        PAD.with(|p| p.set(1));
        let padded = file_stack(kb);
        let Some(stack) = (need..=padded)
            .rev()
            .find(|&s| Site::Free.frame_budget_at(s, 0) < 256)
        else {
            // jq's start-up needs more than that (macOS 26's does).
            PAD.with(|p| p.set(0));
            eprintln!("skipped: jq starts on no stack with under 256 frames for jv_free");
            return;
        };
        (1 + padded - stack) as usize
    };
    PAD.with(|p| p.set(pad));
    let budget = Site::Free.frame_budget_at(file_stack(kb), 0);
    assert!(
        file_stack(kb) >= need && budget < 256,
        "{pad} bytes of padding at {kb} KB leave {} bytes, {budget} frames",
        file_stack(kb)
    );
    // What runs before qj's main is the platform's, and needs what it needs,
    // from one macOS release to the next: on one whose start-up the model
    // doesn't know, it may need more than this, and then neither jq nor qj
    // starts here. A trivial program tells: with the same argv and
    // environment, nothing of qj's can die of the limit, and qj as it is,
    // given the stack compat mode would have, answers if the loader does.
    let trivial = program_file("1");
    let Some(started) = run_stack_norandom(trivial.path(), true, kb, PROGRAM_ARGS) else {
        return; // no setarch
    };
    if code(&started) != Some(0) {
        let stack = file_stack(kb);
        // qj as it is has no QJ_JQ_COMPAT: pad its environment by as much.
        let mut plain_pad = pad;
        while jq_stack(trivial.path(), false, kb * 1024, PROGRAM_ARGS) > stack {
            plain_pad += 1;
            PAD.with(|p| p.set(plain_pad));
        }
        let plain = run_stack_norandom(trivial.path(), false, kb, PROGRAM_ARGS);
        PAD.with(|p| p.set(0));
        let plain = plain.expect("setarch");
        assert_ne!(
            code(&plain),
            Some(0),
            "compat mode dies of a trivial program at {kb} KB with {stack} bytes of stack, \
             where qj as it is answers: {:?}",
            started.status
        );
        eprintln!(
            "skipped: the platform's loader needs more than {stack} bytes beyond argv and the \
             environment ({:?})",
            plain.status
        );
        return;
    }
    // `n` arrays around a null take `n + 1` frames to free.
    for (n, dies) in [(budget - 1, false), (budget, true)] {
        let prog = format!("reduce range({n}) as $i (null;[.]) | length");
        let dir = program_file(&prog);
        let o = run_stack_norandom(dir.path(), true, kb, PROGRAM_ARGS).expect("setarch");
        if dies {
            assert_dies(&o, &format!("freeing {n} levels at {kb} KB"));
        } else {
            assert_eq!(code(&o), Some(0), "{n} at {kb} KB: {}", stderr(&o));
            assert_eq!(stdout(&o), "1\n");
        }
    }
    PAD.with(|p| p.set(0));
}

// ---------------------------------------------------------------------------
// jq's hangs
// ---------------------------------------------------------------------------

/// Runs qj, kills it after `wait`, and says whether it was still running.
fn still_running_after(compat: bool, args: &[&str], wait: Duration) -> bool {
    let mut child = Command::new(env!("CARGO_BIN_EXE_qj"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env(qj::compat::ENV_VAR, if compat { "1" } else { "0" })
        .spawn()
        .expect("failed to run qj");
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if child.try_wait().expect("wait").is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let running = child.try_wait().expect("wait").is_none();
    let _ = child.kill();
    let _ = child.wait();
    running
}

/// `jv_equal(nan, nan)` is false, so jq's `delpaths_sorted` never advances
/// past a NaN path element and loops forever. There is no output to compare,
/// only that compat mode doesn't finish either.
#[test]
fn delpaths_with_a_nan_path_hangs_only_in_compat_mode() {
    for prog in [
        "[1] | delpaths([[nan]])",
        "{\"a\":1} | delpaths([[nan]])",
        "[[1]] | try delpaths([[0,nan]]) catch .",
    ] {
        let args = ["-nc", prog];
        assert!(
            !still_running_after(false, &args, Duration::from_millis(500)),
            "{prog}: should finish without compat mode"
        );
        assert!(
            still_running_after(true, &args, Duration::from_millis(300)),
            "{prog}: should hang in compat mode"
        );
    }
}

// ---------------------------------------------------------------------------
// Closed standard descriptors (both modes; see src/main.rs)
// ---------------------------------------------------------------------------

/// Runs qj with some standard descriptors closed, through `sh` (Rust can't
/// hand a child a closed descriptor). `redirs` is the shell's, e.g. `">&-"`.
///
/// Returns qj's exit status and everything it wrote to a descriptor that is
/// still open, which the script collects on fd 3: fd 3 is never closed, so
/// `>&3` and `2>&3` survive, and `redirs` after them closes what the case
/// wants closed.
fn with_closed_fds(redirs: &str, args: &str) -> (i32, String) {
    let qj = env!("CARGO_BIN_EXE_qj");
    let script = format!("exec 3>&1; {{ {qj} {args} >&3 2>&3 {redirs}; echo \"rc=$?\" >&3; }}");
    let out = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run sh");
    assert!(out.status.success(), "sh failed: {}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, last) = text.rsplit_once("rc=").expect("no rc line");
    (
        last.trim().parse().expect("rc is a number"),
        body.to_string(),
    )
}

const BAD_FD_WRITE: &str = "qj: error: writing output failed: Bad file descriptor\n";
const BAD_FD_READ: &str = "qj: error: Bad file descriptor\n";

#[test]
fn a_closed_stdout_is_a_write_error() {
    let (rc, text) = with_closed_fds(">&-", "-n 1");
    assert_eq!((rc, text.as_str()), (2, BAD_FD_WRITE));
}

#[test]
fn a_closed_stdin_is_a_read_error() {
    let (rc, text) = with_closed_fds("<&-", ".");
    assert_eq!((rc, text.as_str()), (2, BAD_FD_READ));
}

#[test]
fn a_closed_stdin_is_fine_with_null_input() {
    let (rc, text) = with_closed_fds("<&-", "-n 1");
    assert_eq!((rc, text.as_str()), (0, "1\n"));
}

#[test]
fn a_closed_stderr_loses_the_message_but_not_the_status() {
    let (rc, text) = with_closed_fds("2>&-", "-n 'error(\"boom\")'");
    assert_eq!((rc, text.as_str()), (5, ""));
}

/// Every combination, with the results jq 1.8.1 gives (see
/// `docs/COMPATIBILITY.md`). Rust's runtime used to reopen the closed
/// descriptors on /dev/null, so qj wrote into the void and exited 0.
#[test]
fn every_closed_combination_matches_jq() {
    let both = "qj: error: Bad file descriptor\n\
                qj: error: writing output failed: Bad file descriptor\n";
    let cases: &[(&str, &str, i32, &str)] = &[
        (">&-", "-n 1", 2, BAD_FD_WRITE),
        ("2>&-", "-n 1", 0, "1\n"),
        ("<&-", "-n 1", 0, "1\n"),
        (">&- 2>&-", "-n 1", 2, ""),
        ("<&- >&-", "-n 1", 2, BAD_FD_WRITE),
        ("<&- 2>&-", "-n 1", 0, "1\n"),
        ("<&- >&- 2>&-", "-n 1", 2, ""),
        (">&-", ".", 2, BAD_FD_WRITE),
        ("2>&-", ".", 0, ""),
        ("<&-", ".", 2, BAD_FD_READ),
        (">&- 2>&-", ".", 2, ""),
        ("<&- >&-", ".", 2, both),
        ("<&- 2>&-", ".", 2, ""),
        ("<&- >&- 2>&-", ".", 2, ""),
    ];
    for &(redirs, args, rc, text) in cases {
        let (got_rc, got) = with_closed_fds(redirs, args);
        assert_eq!((got_rc, got.as_str()), (rc, text), "qj {args} {redirs}");
    }
}
