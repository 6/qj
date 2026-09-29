//! Port of jq 1.8.1's `jq_test.c` (`--run-tests`): runs a jq test file
//! (`.test` format: program, input, expected outputs, blank line;
//! `%%FAIL` blocks for programs that must not compile) and prints jq's
//! report.
//!
//! Like jq, every test is compiled into one state, so label numbers keep
//! counting from test to test; lines are read with `fgets` (so a line
//! longer than 4095 bytes counts as several); expected outputs are compared
//! with `jv_equal`; after a test with too few results, the rest of its
//! expected lines are read as new tests. `jv_test()`, jq's internal
//! assertions about its value API, is not ported (it prints nothing).
//!
//! Exit status: 1 when a test failed, 2 when `--skip` went past the end of
//! the file, else 0 (then main.c closes stdout as usual).

use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;

use super::input::{StdinReader, Stream};
use super::run::{TraceOut, default_err_cb, with_prog_name, with_stdout, write_stderr};
use crate::jq::lang::execute::{JQ_DEBUG_TRACE, Jq};
use crate::jq::lang::linker::JqAttrs;
use crate::jq::lang::{CompileOptions, jq_compile_args};
use crate::jq::value::{DumpOptions, Object, Value, dump_string, parse_sized};

/// How `jq_testsuite` ends: `exit()` inside the runner (stdout is flushed,
/// without main.c's checks), or a return to main.c.
pub enum Outcome {
    Exit(i32),
    Return(i32),
}

fn print(s: &[u8]) {
    with_stdout(|out| out.write(s));
}

/// C `%s`: up to the first NUL.
fn c_bytes(b: &[u8]) -> &[u8] {
    match memchr::memchr(0, b) {
        Some(i) => &b[..i],
        None => b,
    }
}

/// jq_test.c `skipline`.
fn skipline(buf: &[u8]) -> bool {
    let p = buf
        .iter()
        .position(|&c| c != b' ' && c != b'\t')
        .unwrap_or(buf.len());
    matches!(buf.get(p), None | Some(b'#' | b'\n' | 0))
}

/// `jv_parse(buf)`: the text up to its first NUL.
fn jv_parse(buf: &[u8]) -> Option<Value> {
    parse_sized(c_bytes(buf)).ok()
}

/// `jv_dump(v, 0)` to stdout.
fn dump(v: &Value) {
    print(dump_string(v, &DumpOptions::default()).as_bytes());
}

/// C `atoi`.
fn atoi(s: &[u8]) -> i32 {
    let c = CString::new(c_bytes(s)).expect("no NUL");
    // SAFETY: `c` is a valid NUL-terminated string.
    unsafe { libc::atoi(c.as_ptr()) }
}

/// Port of `jq_testsuite(libdirs, verbose, argc, argv)`.
pub fn jq_testsuite(lib_dirs: Option<&[Vec<u8>]>, verbose: bool, args: &[Vec<u8>]) -> Outcome {
    let mut testdata: Box<dyn Read> = Box::new(StdinReader::new());
    let mut skip = -1;
    let mut take = -1;
    let mut i = 0;
    while i < args.len() {
        if args[i] == b"--skip" || args[i] == b"--take" {
            let Some(n) = args.get(i + 1) else {
                // atoi(argv[argc]) is atoi(NULL): jq dies of a segfault.
                // SAFETY: restoring the default action and raising the signal.
                unsafe {
                    libc::signal(libc::SIGSEGV, libc::SIG_DFL);
                    libc::raise(libc::SIGSEGV);
                }
                return Outcome::Exit(139);
            };
            if args[i] == b"--skip" {
                skip = atoi(n);
            } else {
                take = atoi(n);
            }
            i += 1;
        } else {
            match std::fs::File::open(std::ffi::OsStr::from_bytes(&args[i])) {
                Ok(f) => testdata = Box::new(f),
                Err(e) => {
                    // perror("fopen")
                    let mut msg = b"fopen: ".to_vec();
                    msg.extend_from_slice(&super::args::strerror(e.raw_os_error().unwrap_or(0)));
                    msg.push(b'\n');
                    write_stderr(&msg);
                    return Outcome::Exit(1);
                }
            }
        }
        i += 1;
    }
    if let Some(code) = run_jq_tests(lib_dirs, verbose, Stream::new(testdata), skip, take) {
        return Outcome::Exit(code);
    }
    run_jq_start_state_tests();
    run_jq_pthread_tests();
    Outcome::Return(0)
}

/// The attributes jq_testsuite sets: only `JQ_LIBRARY_PATH` (`-L`, or none).
fn attrs(lib_dirs: Option<&[Vec<u8>]>) -> JqAttrs {
    let mut a = JqAttrs::new(".");
    a.lib_dirs = Value::from(
        lib_dirs
            .unwrap_or_default()
            .iter()
            .map(|d| Value::string_from_bytes(d))
            .collect::<Vec<_>>(),
    );
    a.jq_origin = Value::Null;
    a.prog_origin = Value::Null;
    a
}

/// Port of `run_jq_tests`; `Some(status)` where jq calls `exit`.
fn run_jq_tests(
    lib_dirs: Option<&[Vec<u8>]>,
    verbose: bool,
    mut testdata: Stream,
    mut skip: i32,
    mut take: i32,
) -> Option<i32> {
    let mut prog: Vec<u8> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    // test_err_cb's buffer: the last "jq: error" message (never cleared).
    let mut err_msg: Vec<u8> = Vec::new();
    let (mut tests, mut passed, mut invalid) = (0, 0, 0);
    let mut lineno: u32 = 0;
    let mut must_fail = false;
    let mut check_msg = false;
    let tests_to_skip = skip.max(0);
    let tests_to_take = take;

    let opts = CompileOptions {
        args: Object::new(),
        env: None,
        attrs: attrs(lib_dirs),
    };
    let mut jq: Option<Jq> = None;

    loop {
        if !testdata.fgets(&mut prog) {
            break;
        }
        lineno += 1;
        if skipline(&prog) {
            continue;
        }
        if prog == b"%%FAIL\n" || prog == b"%%FAIL IGNORE MSG\n" {
            must_fail = true;
            check_msg = prog == b"%%FAIL\n";
            continue;
        }
        let mut program = c_bytes(&prog).to_vec();
        if program.last() == Some(&b'\n') {
            program.pop();
        }

        // `goto next`, `goto fail`, or on to the next test.
        enum Then {
            Next,
            Fail,
            Continue,
        }
        let then = 'test: {
            if skip > 0 {
                skip -= 1;
                break 'test Then::Next;
            } else if skip == 0 {
                print(format!("Skipped {tests_to_skip} tests\n").as_bytes());
                skip = -1;
            }
            if take > 0 {
                take -= 1;
            } else if take == 0 {
                print(
                    format!("Hit the number of tests limit ({tests_to_take}), breaking\n")
                        .as_bytes(),
                );
                return finish(tests, passed, invalid, tests_to_skip, skip);
            }

            let mut pass = true;
            tests += 1;
            let mut line = format!("Test #{}: '", tests + tests_to_skip).into_bytes();
            line.extend_from_slice(&program);
            line.extend_from_slice(format!("' at line number {lineno}\n").as_bytes());
            print(&line);
            let compiled = match jq_compile_args(&program, &opts) {
                Ok(bc) => {
                    match &mut jq {
                        Some(jq) => jq.set_bytecode(bc),
                        None => {
                            let mut state = Jq::new(bc);
                            state.set_attr("JQ_LIBRARY_PATH", opts.attrs.lib_dirs.clone());
                            state.set_trace_writer(Some(Box::new(TraceOut)));
                            state.set_error_cb(Some(default_err_cb()));
                            jq = Some(state);
                        }
                    }
                    true
                }
                Err(e) => {
                    for m in &e.messages {
                        if must_fail {
                            // test_err_cb
                            if m.starts_with("jq: error") {
                                err_msg = m.as_bytes()[..m.len().min(4095)].to_vec();
                            }
                        } else {
                            // default_err_cb
                            write_stderr(format!("{}\n", with_prog_name(m)).as_bytes());
                        }
                    }
                    false
                }
            };

            if must_fail {
                if compiled {
                    let mut line = format!(
                        "*** Test program compiled successfully, but should fail at line number {lineno}: "
                    )
                    .into_bytes();
                    line.extend_from_slice(&program);
                    line.push(b'\n');
                    print(&line);
                    break 'test Then::Fail;
                }
                let mut err_buf: &[u8] = c_bytes(&err_msg);
                while testdata.fgets(&mut buf) {
                    lineno += 1;
                    if skipline(&buf) {
                        break;
                    }
                    if check_msg {
                        let mut expected = c_bytes(&buf);
                        if expected.last() == Some(&b'\n') {
                            expected = &expected[..expected.len() - 1];
                        }
                        if !err_buf.starts_with(expected) {
                            let shown = match memchr::memchr(b'\n', err_buf) {
                                Some(nl) => &err_buf[..nl],
                                None => err_buf,
                            };
                            let mut line = b"*** Erroneous program failed with '".to_vec();
                            line.extend_from_slice(shown);
                            line.extend_from_slice(b"', but expected '");
                            line.extend_from_slice(expected);
                            line.extend_from_slice(
                                format!("' at line number {lineno}: ").as_bytes(),
                            );
                            line.extend_from_slice(&program);
                            line.push(b'\n');
                            print(&line);
                            break 'test Then::Fail;
                        }
                        err_buf = &err_buf[expected.len()..];
                        if err_buf.first() == Some(&b'\n') {
                            err_buf = &err_buf[1..];
                        }
                    }
                }
                if check_msg && !err_buf.is_empty() {
                    let shown = match memchr::memchr(b'\n', err_buf) {
                        Some(nl) => &err_buf[..nl],
                        None => err_buf,
                    };
                    let mut line = b"*** Erroneous program failed with extra message '".to_vec();
                    line.extend_from_slice(shown);
                    line.extend_from_slice(format!("' at line {lineno}: ").as_bytes());
                    line.extend_from_slice(&program);
                    line.push(b'\n');
                    print(&line);
                    invalid += 1;
                    pass = false;
                }
                must_fail = false;
                check_msg = false;
                passed += i32::from(pass);
                break 'test Then::Continue;
            }

            if !compiled {
                let mut line =
                    format!("*** Test program failed to compile at line {lineno}: ").into_bytes();
                line.extend_from_slice(&program);
                line.push(b'\n');
                print(&line);
                break 'test Then::Fail;
            }
            let jq = jq.as_mut().expect("compiled");
            if verbose {
                print(b"Disassembly:\n");
                print(jq.dump_disassembly(2).as_bytes());
                print(b"\n");
            }
            if !testdata.fgets(&mut buf) {
                invalid += 1;
                return finish(tests, passed, invalid, tests_to_skip, skip);
            }
            lineno += 1;
            let Some(input) = jv_parse(&buf) else {
                let mut line = format!("*** Input is invalid on line {lineno}: ").into_bytes();
                line.extend_from_slice(c_bytes(&buf));
                line.push(b'\n');
                print(&line);
                break 'test Then::Fail;
            };
            jq.start(input, if verbose { JQ_DEBUG_TRACE } else { 0 });

            while testdata.fgets(&mut buf) {
                lineno += 1;
                if skipline(&buf) {
                    break;
                }
                let Some(expected) = jv_parse(&buf) else {
                    let mut line =
                        format!("*** Expected result is invalid on line {lineno}: ").into_bytes();
                    line.extend_from_slice(c_bytes(&buf));
                    line.push(b'\n');
                    print(&line);
                    break 'test Then::Fail;
                };
                match jq.next() {
                    Some(Ok(actual)) => {
                        if !expected.equal(&actual) {
                            print(b"*** Expected ");
                            dump(&expected);
                            print(b", but got ");
                            dump(&actual);
                            let mut line =
                                format!(" for test at line number {lineno}: ").into_bytes();
                            line.extend_from_slice(&program);
                            line.push(b'\n');
                            print(&line);
                            pass = false;
                        }
                        assert_reparses(&expected);
                    }
                    _ => {
                        let mut line =
                            format!("*** Insufficient results for test at line number {lineno}: ")
                                .into_bytes();
                        line.extend_from_slice(&program);
                        line.push(b'\n');
                        print(&line);
                        pass = false;
                        break;
                    }
                }
            }
            if pass && let Some(Ok(extra)) = jq.next() {
                print(b"*** Superfluous result: ");
                dump(&extra);
                let mut line = format!(" for test at line number {lineno}, ").into_bytes();
                line.extend_from_slice(&program);
                line.push(b'\n');
                print(&line);
                invalid += 1;
                pass = false;
            }
            passed += i32::from(pass);
            Then::Continue
        };
        match then {
            Then::Continue => continue,
            Then::Fail => invalid += 1,
            Then::Next => {}
        }
        // next:
        while testdata.fgets(&mut buf) {
            lineno += 1;
            if skipline(&buf) {
                break;
            }
        }
        must_fail = false;
        check_msg = false;
    }
    finish(tests, passed, invalid, tests_to_skip, skip)
}

/// The assertion jq_test.c makes after comparing an expected value with a result
/// (`#ifdef USE_DECNUM`, and jq 1.8.1 is built with decNumber).
#[cfg(target_vendor = "apple")]
const ASSERT_REPARSED: &str = "Assertion failed: (jv_equal(jv_copy(expected), jv_copy(reparsed))), function run_jq_tests, file jq_test.c, line 204.";
#[cfg(not(target_vendor = "apple"))]
const ASSERT_REPARSED: &str = "jq: src/jq_test.c:204: run_jq_tests: Assertion `jv_equal(jv_copy(expected), jv_copy(reparsed))' failed.";

/// jq_test.c dumps the expected value (with random print flags, none of which change
/// what the text parses back to), parses the text again, and asserts that the result
/// equals the expected value. It doesn't for a NaN, which prints as `null`, so an
/// expected `nan` (or `[nan]`, ...) makes jq die of SIGABRT once the test has a result;
/// the port dies the same way.
fn assert_reparses(expected: &Value) {
    let text = dump_string(expected, &DumpOptions::default());
    if !parse_sized(text.as_bytes()).is_ok_and(|reparsed| expected.equal(&reparsed)) {
        crate::jq::platform::Error::Abort(ASSERT_REPARSED.to_owned()).abort_process();
    }
}

/// The end of `run_jq_tests`: the summary, and jq's exits.
fn finish(tests: i32, passed: i32, invalid: i32, tests_to_skip: i32, skip: i32) -> Option<i32> {
    let total_skipped = if skip > 0 {
        tests_to_skip - skip
    } else {
        tests_to_skip
    };
    print(
        format!(
            "{passed} of {tests} tests passed ({invalid} malformed, {total_skipped} skipped)\n"
        )
        .as_bytes(),
    );
    if skip > 0 {
        print(b"WARN: skipped past the end of file, exiting with status 2\n");
        return Some(2);
    }
    if passed != tests {
        return Some(1);
    }
    None
}

/// Port of `run_jq_start_state_tests`: `jq_start` must reset the error, exit
/// code and halt state.
fn run_jq_start_state_tests() {
    for (prog, input) in [
        (".[]", "[1,2,3]"),
        (".[] | if .%2 == 0 then halt_error else . end", "[1,2,3]"),
    ] {
        print(format!("Test jq_state: {prog}\n").as_bytes());
        let opts = CompileOptions::new(".");
        let bc = jq_compile_args(prog.as_bytes(), &opts).expect("compiles");
        let mut jq = Jq::new(bc);
        let parsed = parse_sized(input.as_bytes()).expect("valid");
        jq.start(parsed.clone(), 0);
        assert!(start_state_ok(&jq));
        for _ in &mut jq {}
        jq.start(parsed, 0);
        assert!(start_state_ok(&jq));
    }
}

fn start_state_ok(jq: &Jq) -> bool {
    jq.error_message().is_none() && jq.exit_code().is_none() && !jq.halted()
}

/// Port of `run_jq_pthread_tests`: three threads each compile and run a
/// program.
fn run_jq_pthread_tests() {
    let threads: Vec<_> = (0..3)
        .map(|_| {
            std::thread::spawn(|| {
                let opts = CompileOptions::new(".");
                let Ok(bc) = jq_compile_args(b".data", &opts) else {
                    return 0;
                };
                let mut jq = Jq::new(bc);
                let mut parser = crate::jq::value::Parser::new(Default::default());
                parser.set_buf(b"{ \"data\": 1 }", false);
                while let Some(Ok(v)) = parser.next() {
                    jq.start(v, 0);
                    for _ in &mut jq {}
                }
                0
            })
        })
        .collect();
    for t in threads {
        assert_eq!(t.join().ok(), Some(0));
    }
}
