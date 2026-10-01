//! Unit tests for the main.c port. Expected results, messages and exit codes
//! come from running jq 1.8.1 (in the C locale, as the Rust test harness runs).

use super::print_flags::{ASCII, COLOR, ISATTY, PRETTY, SORTED, TAB, indent_flags};
use super::*;

/// A host whose values are the JSON texts themselves, validated by
/// serde_json, recording what it was asked to parse.
#[derive(Default)]
struct TestHost {
    parsed: Vec<String>,
}

impl ArgHost for TestHost {
    type Value = String;

    fn parse_json(&mut self, text: &[u8]) -> Result<String, String> {
        let s = String::from_utf8_lossy(text).into_owned();
        self.parsed.push(s.clone());
        serde_json::from_str::<serde_json::Value>(&s)
            .map(|_| s.trim().to_string())
            .map_err(|e| e.to_string())
    }

    fn slurp_json(&mut self, data: &[u8]) -> Result<String, String> {
        let mut items = Vec::new();
        for v in serde_json::Deserializer::from_slice(data).into_iter::<serde_json::Value>() {
            items.push(v.map_err(|e| e.to_string())?.to_string());
        }
        Ok(format!("[{}]", items.join(",")))
    }
}

fn argv(args: &[&str]) -> Vec<Vec<u8>> {
    std::iter::once("jq")
        .chain(args.iter().copied())
        .map(|a| a.as_bytes().to_vec())
        .collect()
}

fn try_parse(args: &[&str]) -> Result<Action<String>, ArgError> {
    parse(&argv(args), &mut TestHost::default())
}

fn run(args: &[&str]) -> Options<String> {
    match try_parse(args) {
        Ok(Action::Run(o)) => o,
        other => panic!("{args:?}: expected Run, got {other:?}"),
    }
}

fn err(args: &[&str]) -> ArgError {
    match try_parse(args) {
        Err(e) => e,
        Ok(a) => panic!("{args:?}: expected an error, got {a:?}"),
    }
}

/// jq's stderr for an error, as the jq binary prints it.
fn jq_stderr(args: &[&str]) -> String {
    String::from_utf8(err(args).render("jq")).unwrap()
}

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn text(s: &str) -> ArgValue<String> {
    ArgValue::Text(b(s))
}

fn json(s: &str) -> ArgValue<String> {
    ArgValue::Json(s.to_string())
}

const HINT: &str = "Use jq --help for help with command-line options,\n\
                    or see the jq manpage, or online docs  at https://jqlang.org\n";

// ---------------------------------------------------------------------------
// Which arguments are options
// ---------------------------------------------------------------------------

#[test]
fn optish_arguments() {
    assert!(isoptish(b"-n"));
    assert!(isoptish(b"--x"));
    assert!(isoptish(b"--"));
    assert!(!isoptish(b"-"));
    assert!(!isoptish(b""));
    assert!(!isoptish(b"-1"));
    assert!(!isoptish(b"-.a"));
    // Not a letter in the C locale.
    assert!(!isoptish("-é".as_bytes()));
}

#[test]
fn dash_alone_and_negative_numbers_are_arguments() {
    // `jq -` compiles the program "-"; `jq . -` reads stdin.
    assert_eq!(run(&["-"]).program, Some(b("-")));
    let o = run(&[".", "-"]);
    assert_eq!(o.files, vec![b("-")]);
    // `jq -1n`: the program "-1n" (a syntax error later, exit 3).
    assert_eq!(run(&["-1n"]).program, Some(b("-1n")));
    assert_eq!(run(&["-.a"]).program, Some(b("-.a")));
}

#[test]
fn first_non_option_is_the_program_rest_are_files() {
    let o = run(&["-c", ".a", "x.json", "-", "y.json"]);
    assert_eq!(o.program, Some(b(".a")));
    assert_eq!(o.files, vec![b("x.json"), b("-"), b("y.json")]);
    // Options may appear anywhere.
    let o = run(&[".a", "x.json", "-c"]);
    assert_eq!(o.program, Some(b(".a")));
    assert_eq!(o.files, vec![b("x.json")]);
    assert_eq!(o.dumpopts & PRETTY, 0);
}

#[test]
fn no_arguments() {
    let o = run(&[]);
    assert_eq!(o.program, None);
    assert!(o.files.is_empty());
    assert_eq!(o.argv0, b("jq"));
}

#[test]
fn double_dash_ends_options() {
    // `jq -nc --args '$ARGS' -- -n` → positional ["-n"]
    let o = run(&["-nc", "--args", "$ARGS", "--", "-n"]);
    assert_eq!(o.positional, vec![text("-n")]);
    // `jq -nc -- '$ARGS' --args -n`: --args is a file after --.
    let o = run(&["-nc", "--", "$ARGS", "--args", "-n"]);
    assert_eq!(o.program, Some(b("$ARGS")));
    assert_eq!(o.files, vec![b("--args"), b("-n")]);
    assert!(o.positional.is_empty());
    // `jq -nc -- -- 1`: the second -- is the program.
    let o = run(&["-nc", "--", "--", "1"]);
    assert_eq!(o.program, Some(b("--")));
    assert_eq!(o.files, vec![b("1")]);
    // shtest #2919: the program may come after --.
    let o = run(&["--args", "-rn", "--", "$ARGS.positional[0]", "bar"]);
    assert_eq!(o.program, Some(b("$ARGS.positional[0]")));
    assert_eq!(o.positional, vec![text("bar")]);
    let o = run(&["--args", "-rn", "1", "--", "$ARGS.positional[0]", "bar"]);
    assert_eq!(o.program, Some(b("1")));
    assert_eq!(o.positional, vec![text("$ARGS.positional[0]"), text("bar")]);
}

// ---------------------------------------------------------------------------
// Short and long options
// ---------------------------------------------------------------------------

#[test]
fn short_options_bundle() {
    let o = run(&["-nr", "."]);
    assert!(o.null_input && o.raw_output);
    let o = run(&["-rn", "."]);
    assert!(o.null_input && o.raw_output);
    let o = run(&["-sR", "."]);
    assert!(o.slurp && o.raw_input);
    let o = run(&["-scCMaSRnfej", "."]);
    assert!(o.slurp && o.color_output && o.no_color_output && o.ascii_output);
    assert!(o.sorted_output && o.raw_input && o.null_input && o.from_file);
    assert!(o.exit_status && o.raw_output && o.raw_no_lf);
    assert_eq!(o.dumpopts & PRETTY, 0);
}

#[test]
fn long_options() {
    let o = run(&[
        "--slurp",
        "--raw-output",
        "--compact-output",
        "--color-output",
        "--monochrome-output",
        "--ascii-output",
        "--unbuffered",
        "--sort-keys",
        "--raw-input",
        "--null-input",
        "--from-file",
        "--seq",
        "--exit-status",
        "--debug-dump-disasm",
        "p",
    ]);
    assert!(o.slurp && o.raw_output && o.color_output && o.no_color_output);
    assert!(o.ascii_output && o.unbuffered_output && o.sorted_output);
    assert!(o.raw_input && o.null_input && o.from_file && o.seq);
    assert!(o.exit_status && o.dump_disasm);
    assert!(!o.raw_no_lf && !o.raw_output0);
    assert_eq!(o.program, Some(b("p")));
}

#[test]
fn raw_output_variants() {
    let o = run(&["-j", "."]);
    assert!(o.raw_output && o.raw_no_lf && !o.raw_output0);
    let o = run(&["--join-output", "."]);
    assert!(o.raw_output && o.raw_no_lf && !o.raw_output0);
    let o = run(&["--raw-output0", "."]);
    assert!(o.raw_output && o.raw_no_lf && o.raw_output0);
}

#[test]
fn stream_options() {
    let o = run(&["--stream", "."]);
    assert!(o.stream && !o.stream_errors);
    assert_eq!(o.parser_flags(), parse_flags::STREAMING);
    let o = run(&["--stream-errors", "."]);
    assert!(o.stream && o.stream_errors);
    let o = run(&["--seq", "--stream-errors", "."]);
    assert_eq!(
        o.parser_flags(),
        parse_flags::SEQ | parse_flags::STREAMING | parse_flags::STREAM_ERRORS
    );
}

#[test]
fn debug_options() {
    assert_eq!(run(&["--debug-trace", "."]).jq_flags, debug_flags::TRACE);
    assert_eq!(
        run(&["--debug-trace=all", "."]).jq_flags,
        debug_flags::TRACE_ALL
    );
    assert_eq!(
        err(&["--debug-trace=foo"]),
        ArgError::UnknownLongOption(b("debug-trace=foo"))
    );
}

#[test]
fn binary_is_accepted_and_ignored() {
    // Windows-only in jq, but accepted everywhere: `jq -b -n 1` prints 1.
    let o = run(&["-b", "-n", "1"]);
    assert!(o.null_input);
    assert_eq!(o.program, Some(b("1")));
    run(&["--binary", "."]);
}

#[test]
fn long_options_match_exactly() {
    assert_eq!(err(&["--s", "-n"]), ArgError::UnknownLongOption(b("s")));
    assert_eq!(
        err(&["--library-path=d"]),
        ArgError::UnknownLongOption(b("library-path=d"))
    );
    assert_eq!(
        err(&["--foo=bar"]),
        ArgError::UnknownLongOption(b("foo=bar"))
    );
    assert_eq!(err(&["---"]), ArgError::UnknownLongOption(b("-")));
    // Long-only options have no short form.
    assert_eq!(err(&["-t"]), ArgError::UnknownShortOption(b't'));
}

#[test]
fn unknown_options() {
    assert_eq!(err(&["-nx", "1"]), ArgError::UnknownShortOption(b'x'));
    assert_eq!(err(&["-n1"]), ArgError::UnknownShortOption(b'1'));
    assert_eq!(err(&["-Z", "."]), ArgError::UnknownShortOption(b'Z'));
    assert_eq!(
        jq_stderr(&["-nx", "1"]),
        format!("jq: Unknown option -x\n{HINT}")
    );
    assert_eq!(
        jq_stderr(&["--foo", "."]),
        format!("jq: Unknown option --foo\n{HINT}")
    );
    assert_eq!(err(&["-nx"]).exit_code(), 2);
}

#[test]
fn errors_stop_at_the_first_bad_argument() {
    // `jq -nc --argjson x '{bad' --foo`: the JSON error comes first.
    assert_eq!(
        err(&["-nc", "--argjson", "x", "{bad", "--foo"]),
        ArgError::InvalidArgjson
    );
    assert_eq!(
        err(&["-nc", "--foo", "--argjson", "x", "{bad"]),
        ArgError::UnknownLongOption(b("foo"))
    );
    assert!(matches!(
        err(&["-nc", "--rawfile", "x", "/nonexistent/qj", "--foo"]),
        ArgError::BadFile { .. }
    ));
}

#[test]
fn help_version_and_build_configuration_stop_parsing() {
    assert_eq!(try_parse(&["-nh"]), Ok(Action::Help));
    assert_eq!(try_parse(&["-h", "--foo"]), Ok(Action::Help));
    assert_eq!(try_parse(&["--help"]), Ok(Action::Help));
    assert_eq!(
        err(&["-n", "--foo", "-h"]),
        ArgError::UnknownLongOption(b("foo"))
    );
    assert_eq!(try_parse(&["-nV"]), Ok(Action::Version));
    assert_eq!(try_parse(&["-V", "--foo"]), Ok(Action::Version));
    assert_eq!(try_parse(&["--version"]), Ok(Action::Version));
    assert_eq!(
        try_parse(&["--build-configuration", "--foo"]),
        Ok(Action::BuildConfiguration)
    );
}

#[test]
fn run_tests_takes_the_rest() {
    match try_parse(&["-L", "/nonexistent-lib", "--run-tests", "a.test", "--foo"]) {
        Ok(Action::RunTests { options, args }) => {
            assert_eq!(options.lib_search_paths, Some(vec![b("/nonexistent-lib")]));
            assert_eq!(args, vec![b("a.test"), b("--foo")]);
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// --args / --jsonargs
// ---------------------------------------------------------------------------

#[test]
fn args_before_and_after_the_program() {
    let o = run(&["-n", "--args", "$ARGS", "a", "b"]);
    assert_eq!(o.program, Some(b("$ARGS")));
    assert_eq!(o.positional, vec![text("a"), text("b")]);
    let o = run(&["-n", "$ARGS", "--args", "a", "b"]);
    assert_eq!(o.positional, vec![text("a"), text("b")]);
    // `jq --args -n '$ARGS.positional' x y`
    let o = run(&["--args", "-n", "$ARGS.positional", "x", "y"]);
    assert!(o.null_input);
    assert_eq!(o.program, Some(b("$ARGS.positional")));
    assert_eq!(o.positional, vec![text("x"), text("y")]);
}

#[test]
fn options_after_args_are_still_options() {
    let o = run(&["-n", "$ARGS", "--args", "a", "-c", "b"]);
    assert_eq!(o.positional, vec![text("a"), text("b")]);
    assert_eq!(o.dumpopts & PRETTY, 0);
    let o = run(&["-nc", "--args", "$ARGS.positional", "a", "", "b"]);
    assert_eq!(o.positional, vec![text("a"), text(""), text("b")]);
}

#[test]
fn files_before_args_stay_files() {
    // `jq -nc '$ARGS' file1 --args a`
    let o = run(&["-nc", "$ARGS", "file1", "--args", "a"]);
    assert_eq!(o.files, vec![b("file1")]);
    assert_eq!(o.positional, vec![text("a")]);
}

#[test]
fn args_and_jsonargs_switch() {
    // `jq -nc '$ARGS' --args a --jsonargs 1 --args b` → ["a",1,"b"]
    let o = run(&[
        "-nc",
        "$ARGS",
        "--args",
        "a",
        "--jsonargs",
        "1",
        "--args",
        "b",
    ]);
    assert_eq!(o.positional, vec![text("a"), json("1"), text("b")]);
    // shtest: `--args foo 1 --jsonargs 2 '{}' --args 3 4`
    let o = run(&[
        "-n",
        "-c",
        "$ARGS.positional",
        "--args",
        "foo",
        "1",
        "--jsonargs",
        "2",
        "{}",
        "--args",
        "3",
        "4",
    ]);
    assert_eq!(
        o.positional,
        vec![
            text("foo"),
            text("1"),
            json("2"),
            json("{}"),
            text("3"),
            text("4")
        ]
    );
    // shtest: `'$ARGS.positional' --args --jsonargs` → []
    assert!(
        run(&["-n", "-c", "$ARGS.positional", "--args", "--jsonargs"])
            .positional
            .is_empty()
    );
}

#[test]
fn jsonargs_values_and_errors() {
    let o = run(&["-n", "--jsonargs", "$ARGS.positional", "1", "{}"]);
    assert_eq!(o.positional, vec![json("1"), json("{}")]);
    assert_eq!(
        err(&["-n", "$ARGS", "--jsonargs", "1", "{bad"]),
        ArgError::InvalidJsonargs
    );
    assert_eq!(
        jq_stderr(&["-n", "$ARGS", "--jsonargs", "1", "{bad"]),
        format!("jq: invalid JSON text passed to --jsonargs\n{HINT}")
    );
    // shtest #2572: after `--` too.
    assert_eq!(
        err(&["-n", "--jsonargs", "null", "--", "invalid"]),
        ArgError::InvalidJsonargs
    );
    // The program itself is never parsed as JSON.
    assert_eq!(
        run(&["--jsonargs", "{bad", "1"]).positional,
        vec![json("1")]
    );
}

// ---------------------------------------------------------------------------
// Named arguments
// ---------------------------------------------------------------------------

#[test]
fn named_arguments_in_order_first_definition_wins() {
    let mut host = TestHost::default();
    let parsed = parse(
        &argv(&[
            "-n",
            "--arg",
            "x",
            "1",
            "--argjson",
            "y",
            "[2]",
            "--arg",
            "x",
            "2",
            "--argjson",
            "y",
            "{bad",
            "$x",
        ]),
        &mut host,
    );
    let Ok(Action::Run(o)) = parsed else {
        panic!("{parsed:?}");
    };
    assert_eq!(o.named, vec![(b("x"), text("1")), (b("y"), json("[2]"))]);
    // The duplicate's JSON was never parsed (so it wasn't an error).
    assert_eq!(host.parsed, vec!["[2]".to_string()]);
    assert_eq!(o.program, Some(b("$x")));
}

#[test]
fn named_parameters_are_taken_verbatim() {
    // `jq -nc --arg x --y '$x'` → "--y"
    let o = run(&["-nc", "--arg", "x", "--y", "$x"]);
    assert_eq!(o.named, vec![(b("x"), text("--y"))]);
    // Any name is accepted, even ones no program can reference.
    let o = run(&["-n", "--arg", "a b", "x", "--arg", "", "y", "."]);
    assert_eq!(o.named, vec![(b("a b"), text("x")), (b(""), text("y"))]);
}

#[test]
fn named_arguments_need_two_parameters() {
    for (opt, what) in [
        ("arg", "value"),
        ("argjson", "text"),
        ("rawfile", "filename"),
        ("slurpfile", "filename"),
    ] {
        let flag = format!("--{opt}");
        for args in [vec!["-n", flag.as_str()], vec!["-n", flag.as_str(), "x"]] {
            assert_eq!(
                jq_stderr(&args),
                format!("jq: --{opt} takes two parameters (e.g. --{opt} varname {what})\n{HINT}"),
                "{args:?}"
            );
        }
    }
}

#[test]
fn argjson_invalid() {
    assert_eq!(
        jq_stderr(&["-n", "--argjson", "x", "{bad", "$x"]),
        format!("jq: invalid JSON text passed to --argjson\n{HINT}")
    );
}

#[test]
fn rawfile_and_slurpfile() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.json");
    std::fs::write(&data, "1 [2]\n\"x\"").unwrap();
    let data = data.to_str().unwrap();
    let o = run(&["-n", "--rawfile", "r", data, "--slurpfile", "s", data, "."]);
    assert_eq!(
        o.named,
        vec![
            (b("r"), text("1 [2]\n\"x\"")),
            (b("s"), json("[1,[2],\"x\"]"))
        ]
    );
}

#[test]
fn rawfile_and_slurpfile_errors() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let missing = format!("{d}/missing");
    assert_eq!(
        jq_stderr(&["-nc", "--rawfile", "x", &missing, "$x"]),
        format!(
            "jq: Bad JSON in --rawfile x {missing}: Could not open {missing}: No such file or directory\n"
        )
    );
    assert_eq!(
        jq_stderr(&["-nc", "--slurpfile", "x", d, "$x"]),
        format!("jq: Bad JSON in --slurpfile x {d}: Could not open {d}: It's a directory\n")
    );
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{\"a\":").unwrap();
    let bad = bad.to_str().unwrap();
    let e = err(&["-nc", "--slurpfile", "s", bad, "$s"]);
    assert!(
        matches!(
            &e,
            ArgError::BadFile {
                option: NamedOption::Slurpfile,
                ..
            }
        ),
        "{e:?}"
    );
    assert_eq!(e.exit_code(), 2);
    // The file of a name defined earlier isn't read at all.
    let o = run(&["-nc", "--arg", "x", "1", "--slurpfile", "x", &missing, "$x"]);
    assert_eq!(o.named, vec![(b("x"), text("1"))]);
}

#[test]
fn program_arguments_bind_args_and_build_configuration() {
    let o = run(&["-n", "--arg", "ARGS", "x", "--arg", "a", "1", "."]);
    assert_eq!(
        o.program_arguments(),
        vec![
            ProgramArgument::Named(b"a", &text("1")),
            ProgramArgument::Args,
            ProgramArgument::BuildConfiguration
        ]
    );
    // $ARGS.named keeps the user's ARGS.
    assert_eq!(o.named[0], (b("ARGS"), text("x")));
    let o = run(&["-n", "--arg", "JQ_BUILD_CONFIGURATION", "x", "."]);
    assert_eq!(
        o.program_arguments(),
        vec![
            ProgramArgument::Named(b"JQ_BUILD_CONFIGURATION", &text("x")),
            ProgramArgument::Args
        ]
    );
}

// ---------------------------------------------------------------------------
// -L
// ---------------------------------------------------------------------------

#[test]
fn library_path_forms() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let real = real.to_str().unwrap();
    let (joined, bundled) = (format!("-L{d}"), format!("-nL{d}"));
    for args in [
        vec!["-L", d, "."],
        vec![joined.as_str(), "."],
        vec!["--library-path", d, "."],
        vec![bundled.as_str(), "."],
    ] {
        let o = run(&args);
        assert_eq!(o.lib_search_paths, Some(vec![b(real)]), "{args:?}");
        assert_eq!(o.program, Some(b(".")));
    }
    // Paths that don't exist are kept as given; -L accumulates.
    let o = run(&["-L", d, "-L", "nonexist", "."]);
    assert_eq!(o.lib_search_paths, Some(vec![b(real), b("nonexist")]));
    assert_eq!(o.library_paths(), vec![b(real), b("nonexist")]);
    // `-Ln` takes "n" as the directory, not -n.
    let o = run(&["-Ln", "."]);
    assert_eq!(o.lib_search_paths, Some(vec![b("n")]));
    assert!(!o.null_input);
    // The next argument is taken even if it looks like an option.
    let o = run(&["-L", "-n", "."]);
    assert_eq!(o.lib_search_paths, Some(vec![b("-n")]));
}

#[test]
fn library_path_default() {
    let o = run(&["."]);
    assert_eq!(o.lib_search_paths, None);
    assert_eq!(
        o.library_paths(),
        vec![b("~/.jq"), b("$ORIGIN/../lib/jq"), b("$ORIGIN/../lib")]
    );
}

#[test]
fn library_path_missing() {
    let expected =
        format!("-L takes a parameter: (e.g. -L /search/path or -L/search/path)\n{HINT}");
    assert_eq!(jq_stderr(&["-n", "-L"]), expected);
    assert_eq!(jq_stderr(&["-nL"]), expected);
    assert_eq!(jq_stderr(&["--library-path"]), expected);
}

// ---------------------------------------------------------------------------
// Indentation
// ---------------------------------------------------------------------------

fn indent_of(args: &[&str]) -> u32 {
    run(args).dumpopts
}

#[test]
fn indent_values() {
    assert_eq!(indent_of(&["."]), indent_flags(2));
    assert_eq!(indent_of(&["--indent", "3", "."]), (3 << 8) | PRETTY);
    assert_eq!(indent_of(&["--indent", "+3", "."]), (3 << 8) | PRETTY);
    assert_eq!(indent_of(&["--indent", "03", "."]), (3 << 8) | PRETTY);
    assert_eq!(indent_of(&["--indent", "7", "."]), (7 << 8) | PRETTY);
    // 0 and -0: pretty, without indentation.
    assert_eq!(indent_of(&["--indent", "0", "."]), PRETTY);
    assert_eq!(indent_of(&["--indent", "-0", "."]), PRETTY);
    // -1: tabs.
    assert_eq!(indent_of(&["--indent", "-1", "."]), TAB | PRETTY);
}

#[test]
fn indent_errors() {
    assert_eq!(
        jq_stderr(&["-n", "--indent"]),
        format!("jq: --indent takes one parameter\n{HINT}")
    );
    for bad in [
        "8",
        "-2",
        " 3",
        "3 ",
        "",
        "0x3",
        "99999999999999999999999",
        "\t3",
        "a",
    ] {
        assert_eq!(
            jq_stderr(&["-n", "--indent", bad, "1"]),
            format!("jq: --indent takes a number between -1 and 7\n{HINT}"),
            "{bad:?}"
        );
    }
}

#[test]
fn indentation_options_apply_in_order() {
    let tab = TAB | PRETTY;
    assert_eq!(indent_of(&["-c", "--indent", "3", "."]), (3 << 8) | PRETTY);
    assert_eq!(indent_of(&["--indent", "3", "-c", "."]), 0);
    assert_eq!(indent_of(&["--tab", "-c", "."]), 0);
    assert_eq!(indent_of(&["-c", "--tab", "."]), tab);
    assert_eq!(
        indent_of(&["--tab", "--indent", "1", "."]),
        (1 << 8) | PRETTY
    );
    assert_eq!(indent_of(&["--indent", "3", "--tab", "."]), tab);
    assert_eq!(indent_of(&["--tab", "."]), tab);
}

// ---------------------------------------------------------------------------
// Output flags and colors
// ---------------------------------------------------------------------------

#[test]
fn color_defaults_and_flags() {
    let tty = |args: &[&str], no_color: Option<&str>| {
        run(args).dumpopts(Some(true), no_color.map(str::as_bytes)) & (COLOR | ISATTY)
    };
    let pipe = |args: &[&str]| run(args).dumpopts(None, None) & (COLOR | ISATTY);
    assert_eq!(tty(&["."], None), COLOR | ISATTY);
    assert_eq!(tty(&["."], Some("")), COLOR | ISATTY);
    assert_eq!(tty(&["."], Some("1")), ISATTY);
    assert_eq!(tty(&["-C", "."], Some("1")), COLOR | ISATTY);
    assert_eq!(tty(&["-M", "."], None), ISATTY);
    assert_eq!(pipe(&["."]), 0);
    assert_eq!(pipe(&["-C", "."]), COLOR);
    // -M beats -C in either order.
    assert_eq!(pipe(&["-C", "-M", "."]), 0);
    assert_eq!(pipe(&["-MC", "."]), 0);
    assert_eq!(pipe(&["-CM", "."]), 0);
}

#[test]
fn sort_and_ascii_flags() {
    let d = run(&["-S", "-a", "."]).dumpopts(None, None);
    assert_eq!(d & (SORTED | ASCII), SORTED | ASCII);
    assert_eq!(d & PRETTY, PRETTY);
}

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

#[test]
fn default_program() {
    let o = run(&[]);
    assert_eq!(o.program_or_default(false, false), Some(&b"."[..]));
    assert_eq!(o.program_or_default(true, false), Some(&b"."[..]));
    assert_eq!(o.program_or_default(false, true), Some(&b"."[..]));
    assert_eq!(o.program_or_default(true, true), None);
    // `jq -f` with no program file: usage, exit 2.
    assert_eq!(run(&["-f"]).program_or_default(false, false), None);
    assert_eq!(ArgError::NoProgram.exit_code(), 2);
    let usage = String::from_utf8(ArgError::NoProgram.render("qj")).unwrap();
    assert!(usage.ends_with("use qj --help.\n"), "{usage}");
}

#[test]
fn from_file_is_a_flag_for_the_first_argument() {
    // `jq -f prog.jq in.json` and `jq prog.jq in.json -f`: same thing.
    for args in [
        vec!["-f", "prog.jq", "in.json"],
        vec!["prog.jq", "in.json", "-f"],
        vec!["-nf", "prog.jq", "in.json"],
        vec!["-fn", "prog.jq", "in.json"],
    ] {
        let o = run(&args);
        assert!(o.from_file);
        assert_eq!(o.program, Some(b("prog.jq")), "{args:?}");
        assert_eq!(o.files, vec![b("in.json")]);
    }
    // `jq in.json -f prog.jq`: in.json is the program file.
    let o = run(&["in.json", "-f", "prog.jq"]);
    assert_eq!(o.program, Some(b("in.json")));
    assert_eq!(o.files, vec![b("prog.jq")]);
}

#[test]
fn load_program_reads_up_to_nul() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("p.jq");
    std::fs::write(&p, b".a\0garbage").unwrap();
    assert_eq!(load_program(p.to_str().unwrap().as_bytes()), Ok(b(".a")));
    let d = dir.path().to_str().unwrap();
    let e = load_program(d.as_bytes()).unwrap_err();
    assert_eq!(
        String::from_utf8(e.render("jq")).unwrap(),
        format!("jq: Could not open {d}: It's a directory\n")
    );
    let missing = format!("{d}/missing.jq");
    let e = load_program(missing.as_bytes()).unwrap_err();
    assert_eq!(
        String::from_utf8(e.render("jq")).unwrap(),
        format!("jq: Could not open {missing}: No such file or directory\n")
    );
}

#[test]
fn origins() {
    let o = run(&["."]);
    assert_eq!(o.jq_origin(), b("."));
    let cwd = std::fs::canonicalize(".").unwrap();
    assert_eq!(o.program_origin(), cwd.into_os_string().into_vec());
    let dir = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let prog = dir.path().join("p.jq");
    let o = run(&["-f", prog.to_str().unwrap()]);
    assert_eq!(o.program_origin(), real.into_os_string().into_vec());
    let o = parse(&[b("/usr/local/bin/jq"), b(".")], &mut TestHost::default());
    let Ok(Action::Run(o)) = o else { panic!() };
    assert_eq!(o.jq_origin(), b("/usr/local/bin"));
}

#[test]
fn dirname_follows_posix() {
    for (path, dir) in [
        ("", "."),
        ("/", "/"),
        ("//", "/"),
        ("a", "."),
        ("a/", "."),
        ("/a", "/"),
        ("a/b", "a"),
        ("a//b", "a"),
        ("/a/b/", "/a"),
        ("./p.jq", "."),
        ("../x/p.jq", "../x"),
    ] {
        assert_eq!(dirname(path.as_bytes()), b(dir), "{path:?}");
    }
}

// ---------------------------------------------------------------------------
// JQ_COLORS
// ---------------------------------------------------------------------------

#[test]
fn jq_colors_valid() {
    let esc = |s: &str| format!("\x1b[{s}m").into_bytes();
    let defaults: Vec<Vec<u8>> = DEFAULT_COLORS.iter().map(|c| b(c)).collect();
    assert_eq!(jq_colors(b"").unwrap().to_vec(), defaults);
    let c = jq_colors(b"4;31").unwrap();
    assert_eq!(c[0], esc("4;31"));
    assert_eq!(c[1..], defaults[1..]);
    // ':' → an empty first color; "4:" → the empty last field is ignored.
    assert_eq!(jq_colors(b":").unwrap()[0], esc(""));
    assert_eq!(jq_colors(b":").unwrap()[1], defaults[1]);
    assert_eq!(jq_colors(b"4:").unwrap()[0], esc("4"));
    assert_eq!(jq_colors(b"4:").unwrap()[1], defaults[1]);
    let all = jq_colors(b"0;30:0;31:0;32:0;33:0;34:1;35:1;36:1;37").unwrap();
    assert_eq!(all[3], esc("0;33"));
    assert_eq!(all[7], esc("1;37"));
    // jq stops after the eighth field: anything after it is ignored.
    let c = jq_colors(b"1:2:3:4:5:6:7:8garbage").unwrap();
    assert_eq!(c[3], esc("4"));
    assert_eq!(c[7], esc("8"));
    assert_eq!(jq_colors(b"1:2:3:4:5:6:7:8:9").unwrap()[7], esc("8"));
    assert_eq!(jq_colors(b"::::::::").unwrap()[6], esc(""));
}

#[test]
fn jq_colors_invalid() {
    // shtest's invalid values, and an invalid seventh field.
    for bad in [
        "/",
        "[30",
        "30m",
        "30:31m:32",
        "30:*:31",
        "invalid",
        "garbage",
    ] {
        assert_eq!(jq_colors(bad.as_bytes()), None, "{bad:?}");
    }
    assert_eq!(jq_colors(b"1:2:3:4:5:6:7garbage"), None);
}

// ---------------------------------------------------------------------------
// qj extensions
// ---------------------------------------------------------------------------

#[test]
fn qj_extensions() {
    let o = run(&["--threads", "4", "--jsonl", "--debug-timing", "."]);
    assert_eq!(o.threads, Some(4));
    assert!(o.jsonl && o.debug_timing);
    assert_eq!(o.program, Some(b(".")));
    assert_eq!(run(&["--threads=2", "."]).threads, Some(2));
    assert_eq!(err(&["--threads"]), ArgError::ThreadsMissing);
    assert_eq!(err(&["--threads", "x"]), ArgError::ThreadsInvalid(b("x")));
    assert_eq!(err(&["--threads=", "."]), ArgError::ThreadsInvalid(b("")));
    // Short forms don't exist.
    assert_eq!(err(&["-threads=4"]), ArgError::UnknownShortOption(b't'));
    assert_eq!(
        err(&["--jsonl=1"]),
        ArgError::UnknownLongOption(b("jsonl=1"))
    );
}

#[test]
fn file_globs() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    for name in ["b.json", "a.json", "c.txt", "lit[1].json"] {
        std::fs::write(dir.path().join(name), "1").unwrap();
    }
    let got = expand_file_globs(&[
        b("-"),
        b(&format!("{d}/*.json")),
        b(&format!("{d}/lit[1].json")),
        b(&format!("{d}/*.none")),
        b(&format!("{d}/[")),
        b(&format!("{d}/c.txt")),
    ]);
    assert_eq!(
        got,
        vec![
            b("-"),
            // Sorted matches ("lit[1].json" matches too).
            b(&format!("{d}/a.json")),
            b(&format!("{d}/b.json")),
            b(&format!("{d}/lit[1].json")),
            // Exists: kept literally.
            b(&format!("{d}/lit[1].json")),
            // No match, invalid pattern: kept, so opening fails as in jq.
            b(&format!("{d}/*.none")),
            b(&format!("{d}/[")),
            b(&format!("{d}/c.txt")),
        ]
    );
}
