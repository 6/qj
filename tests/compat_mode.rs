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

/// The environment of every qj that [`run_stack`] starts,
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
/// [`run_stack`] under an `RLIMIT_STACK` of `rlimit` bytes:
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

/// A directory holding `program` as `p.jq`, and the arguments that run it:
/// with the program in a file, argv is the same whatever its depth.
fn program_file(program: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("p.jq"), program).expect("write the program");
    dir
}

const PROGRAM_ARGS: &[&str] = &["-nc", "-f", "p.jq"];

/// Runs qj with exactly [`exact_env`] for an environment, so that
/// [`jq_stack`] knows what compat mode sees, and its `RLIMIT_STACK` set to
/// `stack_kb`, so that a test can reach a stack-overflow threshold without a
/// huge value or a huge chain of modules.
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
        (Some(0), "jq-1.8.2\n")
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
    assert!(help.starts_with("jq - commandline JSON processor [version 1.8.2]\n\nUsage:\tjq "));
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
         or see the jq manpage, or online docs at https://jqlang.org\n"
    );
    let compat = run(true, &["--bogus"]);
    assert_eq!(
        stderr(&compat),
        "jq: Unknown option --bogus\nUse jq --help for help with command-line options,\n\
         or see the jq manpage, or online docs at https://jqlang.org\n"
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
fn a_module_import_cycle_is_an_error_in_both_modes() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.jq"), "import \"a\" as a;\ndef f: 1;\n").unwrap();
    let args = &["-nc", "-L", ".", "include \"a\"; 1"];
    let path = dir.path().canonicalize().unwrap().join("a.jq");
    for (compat, prog) in [(false, "qj"), (true, "jq")] {
        let o = run_in(dir.path(), compat, args);
        assert_eq!(code(&o), Some(3), "{}", stderr(&o));
        assert_eq!(
            stderr(&o),
            format!(
                "{prog}: error: circular import of {}\n\n{prog}: 1 compile error\n",
                path.display()
            )
        );
    }
}

/// jq 1.8.2 loads a module before the modules it imports, and then drops
/// the definitions of the third module of a chain although the second still
/// calls them: compiling the call reads freed memory, and jq aborts.
#[test]
fn a_chain_of_three_modules_aborts_in_compat_mode() {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in [
        ("m0.jq", "import \"m1\" as m;\ndef f: 0 + m::f;\n"),
        ("m1.jq", "import \"m2\" as m;\ndef f: 1 + m::f;\n"),
        ("m2.jq", "def f: 2;\n"),
    ] {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let args = &["-nc", "-L", ".", "include \"m0\"; f"];
    let plain = run_in(dir.path(), false, args);
    assert_eq!((code(&plain), stdout(&plain).as_str()), (Some(0), "3\n"));
    let compat = run_in(dir.path(), true, args);
    assert_eq!(signal(&compat), Some(libc::SIGABRT));
    assert_eq!(stdout(&compat), "");
    assert!(
        stderr(&compat).contains("Unknown function type"),
        "{}",
        stderr(&compat)
    );
    // Two deep, everything is kept.
    let compat = run_in(dir.path(), true, &["-nc", "-L", ".", "include \"m1\"; f"]);
    assert_eq!((code(&compat), stdout(&compat).as_str()), (Some(0), "3\n"));
}

/// A data import two modules below the program is freed the same way: with
/// glibc, compiling a reference to it aborts; on macOS, reading it does.
#[test]
fn a_data_import_in_a_chain_aborts_in_compat_mode() {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in [
        ("m0.jq", "import \"m1\" as m;\ndef f: m::f;\n"),
        ("m1.jq", "import \"d\" as $d;\ndef f: $d;\n"),
        ("d.json", "[1]\n"),
    ] {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let args = &["-nc", "-L", ".", "include \"m0\"; 1, f"];
    let plain = run_in(dir.path(), false, args);
    assert_eq!(
        (code(&plain), stdout(&plain).as_str()),
        (Some(0), "1\n[[1]]\n")
    );
    let compat = run_in(dir.path(), true, args);
    assert_eq!(signal(&compat), Some(libc::SIGABRT));
    let (out, assert) = if cfg!(all(target_os = "linux", target_env = "gnu")) {
        ("", "bc && bc == target->compiled")
    } else {
        ("1\n", "jv_is_valid(val)")
    };
    assert_eq!(stdout(&compat), out);
    assert!(stderr(&compat).contains(assert), "{}", stderr(&compat));
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
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
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

/// jq 1.8.2 frees values iteratively, so a value of any depth is freed without
/// a crash, in either mode and on any stack.
#[test]
fn deeply_nested_values_are_freed_in_both_modes() {
    let prog = "reduce range(300000) as $i (null;[.]) | length";
    for compat in [false, true] {
        let dir = program_file(prog);
        let o = run_stack(dir.path(), compat, 1024, PROGRAM_ARGS);
        assert_eq!(code(&o), Some(0), "compat={compat}: {}", stderr(&o));
        assert_eq!(stdout(&o), "1\n", "compat={compat}");
    }
}

/// Past 10,000 levels jq gives up on a comparison, a merge or a path instead of
/// recursing further, and says so: an error in both modes, at any stack limit
/// that holds the 10,000 levels.
#[test]
fn values_past_jqs_depth_limit_are_errors_in_both_modes() {
    let v = deep(20_000, false);
    let o = deep(20_000, true);
    for (prog, err) in [
        (
            format!("({v}) as $a | ({v}) as $b | $a == $b"),
            "Equality check too deep",
        ),
        (
            format!("({v}) as $a | ({v}) as $b | $a < $b"),
            "Comparison too deep",
        ),
        (
            format!("({v}) as $x | $x | contains($x)"),
            "Containment check too deep",
        ),
        (format!("({o}) as $x | $x * $x"), "Object merge too deep"),
        (
            "[range(10001)|0] as $p | null | setpath($p; 1)".to_string(),
            "Path too deep",
        ),
    ] {
        for compat in [false, true] {
            let out = run(compat, &["-nc", &prog]);
            assert_eq!(
                code(&out),
                Some(5),
                "compat={compat} {err}: {:?}",
                out.status
            );
            assert!(
                stderr(&out).contains(err),
                "compat={compat}: {}",
                stderr(&out)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// jq's recursions over values and paths
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
        Site::Equal => format!("({v}) as $a | ({v}) as $b | $a == $b"),
        Site::Compare => format!("({v}) as $a | ({v}) as $b | $a < $b"),
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
        // See `printing_a_deep_value_crashes_where_jqs_printer_does`.
        Site::Print | Site::Dump => unreachable!("{site:?} has a test of its own"),
        // A regex's depth is its pattern's: see
        // `a_nested_regex_crashes_where_jqs_oniguruma_does`.
        Site::RegexParse | Site::RegexTree => unreachable!("{site:?} is driven by a pattern"),
    }
}

/// Every site over values, with a name for the failure messages and how many
/// frames [`program`] costs jq for `n` levels: `n + 1` everywhere but the
/// merge, which stops at the innermost object rather than descending into the
/// `null` below it.
const SITES: &[(Site, &str, u64)] = &[
    (Site::Equal, "equal", 1),
    (Site::Compare, "compare", 1),
    (Site::Contains, "contains", 1),
    (Site::Merge, "merge", 0),
    (Site::Setpath, "setpath", 1),
    (Site::Delpaths, "delpaths", 1),
];

/// A stack small enough that every recursion over values runs out before jq's
/// 10,000-level limit stops it, and large enough for a thousand levels of each.
const VALUE_STACK_KB: u64 = 512;

/// The stack compat mode sees as jq's for [`program_file`]'s program at
/// [`VALUE_STACK_KB`].
fn value_stack() -> u64 {
    file_stack(VALUE_STACK_KB)
}

/// Each of jq's recursions has a threshold of its own, which compat mode
/// reproduces and the default has no reason to: the deepest value the site's
/// budget allows answers, and one level more dies.
///
/// The depths come from the model rather than from a constant
/// (`qj::compat::Site::frame_budget_at`).
#[test]
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
fn every_recursion_has_its_own_crash_depth_in_compat_mode() {
    let stack = value_stack();
    for &(site, name, extra) in SITES {
        let budget = site.frame_budget_at(stack, 0);
        assert!(
            (1000..10_000).contains(&budget),
            "{name}: implausible budget {budget}"
        );
        let deepest = budget - extra;
        for (n, dies) in [(deepest, false), (deepest + 1, true)] {
            let prog = program(site, n);
            let o = run_stack_program(true, VALUE_STACK_KB, &prog);
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
            let plain = run_stack_program(false, VALUE_STACK_KB, &prog);
            assert_eq!(code(&plain), Some(0), "{name} at {n}: {}", stderr(&plain));
        }
    }
}

/// The sites are independent: a value deep enough to kill jq searching it
/// still dies there even though comparing it would have been fine.
#[test]
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
fn a_value_between_two_thresholds_dies_at_the_lower_one() {
    let stack = value_stack();
    let contains = Site::Contains.frame_budget_at(stack, 0);
    let compare = Site::Compare.frame_budget_at(stack, 0);
    // jv_contains's frame is larger than jv_cmp's, so there is always a range
    // of depths that only the containment check cannot survive.
    assert!(contains < compare, "{contains} should be below {compare}");
    let between = (contains + compare) / 2;
    let searched = run_stack_program(true, VALUE_STACK_KB, &program(Site::Contains, between));
    assert_eq!(
        signal(&searched),
        Some(libc::SIGSEGV),
        "searching {between}"
    );
    let compared = run_stack_program(true, VALUE_STACK_KB, &program(Site::Compare, between));
    assert_eq!(
        code(&compared),
        Some(0),
        "comparing {between}: {}",
        stderr(&compared)
    );
    assert_eq!(stdout(&compared), "false\n");
}

/// Only the depth jq's traversal reaches counts. `jv_equal` answers from the
/// pointer when both sides are the same allocation, and stops at the first
/// difference, so these never recurse however deep the value is.
#[test]
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
fn comparisons_that_jq_answers_without_recursing_do_not_crash() {
    let compare = Site::Compare.frame_budget_at(value_stack(), 0);
    // Past every comparison's threshold, and inside jq's 10,000 levels.
    let depth = compare + 1000;
    assert!(depth < 10_000, "{depth}");
    let v = deep(depth, false);
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
        let o = run_stack_program(true, VALUE_STACK_KB, &prog);
        assert_eq!(code(&o), Some(0), "{prog}: {}", stderr(&o));
        assert_eq!(stdout(&o), want, "{prog}");
    }
}

/// jq compares path elements from inside `delpaths_sorted`, with the frames for
/// the levels above still on the stack, so the same value kills it sooner the
/// deeper the paths go. The depth that survives at the top level must die a
/// thousand levels down.
#[test]
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
fn a_recursion_inside_another_gets_the_stack_that_is_left() {
    const STACK_KB: u64 = 1024;
    let stack = file_stack(STACK_KB);
    let equal = Site::Equal.frame_budget_at(stack, 0);
    let delpaths = Site::Delpaths.frame_budget_at(stack, 0);
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
    // into `delpaths`: a `delpaths_sorted` frame costs more than a `jv_equal`
    // one, so `prefix` levels of it cost more than `prefix` levels of
    // comparison.
    let between = equal - prefix;
    assert!(between < 10_000, "{between}");
    let deeper = run_stack_program(true, STACK_KB, &program(between));
    assert_eq!(
        signal(&deeper),
        Some(libc::SIGSEGV),
        "{:?} {}",
        deeper.status,
        stderr(&deeper)
    );
    // The same comparison at the top level answers.
    let shallow = format!("({0}) as $a | ({0}) as $b | $a == $b", deep(between, false));
    let o = run_stack_program(true, STACK_KB, &shallow);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "true\n");
}

/// `jv_getpath` is jq's one path recursion that is a tail call, which both
/// release compilers turn into a loop: the longest path jq takes (10,000
/// elements) doesn't overflow it, however small the stack.
#[test]
fn getpath_does_not_crash_however_small_the_stack() {
    for (prog, want) in [
        (
            "[range(10000)|0] as $p | null | getpath($p)".to_string(),
            "null\n",
        ),
        (
            format!(
                "({}) as $x | [range(10000)|0] as $p | $x | getpath($p)",
                deep(20, false)
            ),
            "null\n",
        ),
    ] {
        let o = run_stack_program(true, 128, &prog);
        assert_eq!(code(&o), Some(0), "{prog}: {}", stderr(&o));
        assert_eq!(stdout(&o), want, "{prog}");
    }
}

#[test]
fn run_tests_without_a_count_is_an_error_in_both_modes() {
    for compat in [false, true] {
        for opt in ["--skip", "--take"] {
            let o = run(compat, &["--run-tests", opt]);
            assert_eq!(code(&o), Some(1), "compat={compat} {opt}: {:?}", o.status);
            assert_eq!(stderr(&o), format!("{opt} requires an argument\n"));
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
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
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
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
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
/// `MAX_PRINT_DEPTH` (10,000) — so it can only overflow on a stack of about
/// 3 MB or less, and there it does. qj's own printer recurses too, with a frame of
/// its own; in compat mode it dies at jq's depth instead.
#[test]
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    ignore = "compat mode's stack models were measured on macOS and glibc Linux only"
)]
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
/// randomization would sometimes take away. The limit drops only once
/// `setarch` has turned it off: `uname` and `setarch` run before, with the
/// randomization still on, and could crash at it too.
fn run_stack_norandom(dir: &Path, compat: bool, stack_kb: u64, args: &[&str]) -> Option<Output> {
    if !cfg!(target_os = "linux") {
        return Some(run_stack(dir, compat, stack_kb, args));
    }
    let setarch = ["/usr/bin/setarch", "/bin/setarch"]
        .into_iter()
        .find(|p| Path::new(p).exists())?;
    let script = format!(
        "exec {setarch} \"$(uname -m)\" -R /bin/sh -c 'ulimit -s {stack_kb} || exit 99; ulimit -c 0; exec /usr/bin/env -i \"$@\"' sh \"$@\""
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
    assert_eq!(stdout(&version), "jq-1.8.2\n");
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

// ---------------------------------------------------------------------------
// What used to hang
// ---------------------------------------------------------------------------

/// jq 1.8.1's `delpaths_sorted` never advanced past a NaN path element and
/// looped forever; jq 1.8.2 skips it, in both modes.
#[test]
fn delpaths_with_a_nan_path_finishes_in_both_modes() {
    for (prog, want) in [
        ("[1] | delpaths([[nan]])", "[1]\n"),
        (
            "{\"a\":1} | try delpaths([[nan]]) catch .",
            "\"Cannot delete number field of object\"\n",
        ),
        ("[[1]] | try delpaths([[0,nan]]) catch .", "[[1]]\n"),
    ] {
        for compat in [false, true] {
            let o = run(compat, &["-nc", prog]);
            assert_eq!(code(&o), Some(0), "compat={compat} {prog}: {}", stderr(&o));
            assert_eq!(stdout(&o), want, "compat={compat} {prog}");
        }
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

/// Every combination, with the results jq 1.8.2 gives (see
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
