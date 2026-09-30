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
        "qj: error: Could not open file g*.json: No such file or directory\n"
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
        stderr(&compat).starts_with("qj: parse error:"),
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
            stderr(&compat).starts_with(&format!("qj: Unknown option {}", args[0])),
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
    let budget = match qj::compat::free_frame_budget() {
        Some(b) => b,
        // An unlimited stack: jq doesn't overflow either.
        None => return,
    };
    assert!(budget > 1000, "implausible frame budget {budget}");
    // `reduce range(n) as $i (null;[.])` nests n arrays around a null, so
    // freeing it takes n + 1 frames: the deepest n that fits is budget - 1.
    let deepest = budget - 1;
    for (n, dies) in [(deepest, false), (deepest + 1, true), (budget + 1000, true)] {
        let prog = format!("reduce range({n}) as $i (null;[.]) | length");
        let o = run(true, &["-nc", &prog]);
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
    for &(site, name, extra) in SITES {
        let Some(budget) = site.frame_budget() else {
            continue; // an unlimited stack: jq doesn't overflow either
        };
        assert!(budget > 1000, "{name}: implausible budget {budget}");
        let deepest = budget - extra;
        for (n, dies) in [(deepest, false), (deepest + 1, true)] {
            let prog = program(site, n);
            let o = run(true, &["-nc", &prog]);
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
            let plain = run(false, &["-nc", &prog]);
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
