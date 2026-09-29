//! Tests for the `_match_impl` port. Every expectation below was produced by the jq
//! 1.8.1 binary (`jq -nc '$in | _match_impl($re; $flags; $test)'`).

use super::*;

/// jq's `-c` rendering of a string (`jv_print.c: jvp_dump_string` without `-a`).
fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn opt_string(s: &Option<String>, out: &mut String) {
    match s {
        Some(s) => json_string(s, out),
        None => out.push_str("null"),
    }
}

fn capture_json(c: &Capture, out: &mut String) {
    out.push('{');
    for (i, key) in c.keys().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_string(key, out);
        out.push(':');
        match *key {
            "offset" => out.push_str(&c.offset.to_string()),
            "length" => out.push_str(&c.length.to_string()),
            "string" => opt_string(&c.string, out),
            "name" => opt_string(&c.name, out),
            _ => unreachable!(),
        }
    }
    out.push('}');
}

fn match_json(m: &Match, out: &mut String) {
    out.push('{');
    for (i, key) in Match::KEYS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_string(key, out);
        out.push(':');
        match *key {
            "offset" => out.push_str(&m.offset.to_string()),
            "length" => out.push_str(&m.length.to_string()),
            "string" => json_string(&m.string, out),
            "captures" => {
                out.push('[');
                for (j, c) in m.captures.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    capture_json(c, out);
                }
                out.push(']');
            }
            _ => unreachable!(),
        }
    }
    out.push('}');
}

/// The result as jq `-c` prints it, or the error message.
fn run(input: &str, re: &str, flags: Option<&str>, test: bool) -> Result<String, String> {
    match match_impl(input, re, flags, test) {
        Ok(MatchResult::Test(b)) => Ok(b.to_string()),
        Ok(MatchResult::Matches(ms)) => {
            let mut out = String::from("[");
            for (i, m) in ms.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match_json(m, &mut out);
            }
            out.push(']');
            Ok(out)
        }
        Err(Error::Msg(m)) => Err(m),
        Err(Error::Abort(m)) => panic!("unexpected abort: {m}"),
    }
}

fn ok(input: &str, re: &str, flags: Option<&str>) -> String {
    run(input, re, flags, false).unwrap_or_else(|e| panic!("{re:?}: unexpected error {e}"))
}

fn err(input: &str, re: &str, flags: Option<&str>) -> String {
    run(input, re, flags, false).expect_err("expected an error")
}

#[test]
fn named_captures_and_global() {
    assert_eq!(
        ok("foo bar", "(?<x>o+)", Some("g")),
        r#"[{"offset":1,"length":2,"string":"oo","captures":[{"offset":1,"length":2,"string":"oo","name":"x"}]}]"#
    );
    assert_eq!(
        ok("xyzzy-14", "(?<x>[a-z]+)-(?<n>[0-9]+)", None),
        r#"[{"offset":0,"length":8,"string":"xyzzy-14","captures":[{"offset":0,"length":5,"string":"xyzzy","name":"x"},{"offset":6,"length":2,"string":"14","name":"n"}]}]"#
    );
}

#[test]
fn unmatched_group_is_offset_minus_one_with_other_key_order() {
    assert_eq!(
        ok("foo bar", "(?<x>o+)|(z)", Some("g")),
        r#"[{"offset":1,"length":2,"string":"oo","captures":[{"offset":1,"length":2,"string":"oo","name":"x"},{"offset":-1,"string":null,"length":0,"name":null}]}]"#
    );
    assert_eq!(
        ok("abc", "(a)(x?)(b)", None),
        r#"[{"offset":0,"length":2,"string":"ab","captures":[{"offset":0,"length":1,"string":"a","name":null},{"offset":1,"string":"","length":0,"name":null},{"offset":1,"length":1,"string":"b","name":null}]}]"#
    );
}

#[test]
fn empty_matches_advance_by_one_byte() {
    assert_eq!(
        ok("foo bar", "(?<x>o*)", Some("g")),
        concat!(
            r#"[{"offset":0,"length":0,"string":"","captures":[{"offset":0,"string":"","length":0,"name":"x"}]},"#,
            r#"{"offset":1,"length":2,"string":"oo","captures":[{"offset":1,"length":2,"string":"oo","name":"x"}]},"#,
            r#"{"offset":3,"length":0,"string":"","captures":[{"offset":3,"string":"","length":0,"name":"x"}]},"#,
            r#"{"offset":4,"length":0,"string":"","captures":[{"offset":4,"string":"","length":0,"name":"x"}]},"#,
            r#"{"offset":5,"length":0,"string":"","captures":[{"offset":5,"string":"","length":0,"name":"x"}]},"#,
            r#"{"offset":6,"length":0,"string":"","captures":[{"offset":6,"string":"","length":0,"name":"x"}]},"#,
            r#"{"offset":7,"length":0,"string":"","captures":[{"offset":7,"string":"","length":0,"name":"x"}]}]"#
        )
    );
    // The byte step lands inside "é", so offsets 1 and 2 are reported twice.
    assert_eq!(
        ok("éé", "", Some("g")),
        concat!(
            r#"[{"offset":0,"length":0,"string":"","captures":[]},{"offset":1,"length":0,"string":"","captures":[]},"#,
            r#"{"offset":1,"length":0,"string":"","captures":[]},{"offset":2,"length":0,"string":"","captures":[]},"#,
            r#"{"offset":2,"length":0,"string":"","captures":[]}]"#
        )
    );
    // "(?=u)" must match "qux" only once.
    assert_eq!(
        ok("qux", "(?=u)", Some("g")),
        r#"[{"offset":1,"length":0,"string":"","captures":[]}]"#
    );
}

#[test]
fn match_starting_inside_a_character() {
    // After the empty match before "é", the search restarts on its second byte, where
    // `.` matches the lone continuation byte: offset 0 (jq never sees the match start
    // on a character boundary), length 1, and the byte becomes U+FFFD.
    assert_eq!(
        ok("éa", "(?=é)|.", Some("g")),
        concat!(
            r#"[{"offset":0,"length":0,"string":"","captures":[]},"#,
            r#"{"offset":0,"length":1,"string":"�","captures":[]},"#,
            r#"{"offset":1,"length":1,"string":"a","captures":[]}]"#
        )
    );
}

#[test]
fn captures_inside_empty_matches_are_reported_empty() {
    assert_eq!(
        ok("foo", "(?=(o+))", Some("g")),
        concat!(
            r#"[{"offset":1,"length":0,"string":"","captures":[{"offset":1,"string":"","length":0,"name":null}]},"#,
            r#"{"offset":2,"length":0,"string":"","captures":[{"offset":2,"string":"","length":0,"name":null}]}]"#
        )
    );
}

#[test]
fn codepoint_offsets() {
    assert_eq!(
        ok("😀a😀", "a", None),
        r#"[{"offset":1,"length":1,"string":"a","captures":[]}]"#
    );
    assert_eq!(
        ok("aéb日c", "(é)(b)(日)", None),
        concat!(
            r#"[{"offset":1,"length":3,"string":"éb日","captures":["#,
            r#"{"offset":1,"length":1,"string":"é","name":null},"#,
            r#"{"offset":2,"length":1,"string":"b","name":null},"#,
            r#"{"offset":3,"length":1,"string":"日","name":null}]}]"#
        )
    );
}

#[test]
fn test_mode() {
    assert_eq!(run("abc", "b", None, true), Ok("true".into()));
    assert_eq!(run("abc", "x", Some("g"), true), Ok("false".into()));
    assert_eq!(run("abc", "B", Some("i"), true), Ok("true".into()));
    assert_eq!(
        run("abc", "(", None, true),
        Err("Regex failure: end pattern with unmatched parenthesis".into())
    );
}

#[test]
fn flags() {
    // m: "." matches newline; s: "^"/"$" only at the string's ends; p: both.
    assert_eq!(
        ok("ab\ncd", "b.c", Some("m")),
        r#"[{"offset":1,"length":3,"string":"b\nc","captures":[]}]"#
    );
    assert_eq!(ok("ab\ncd", "b.c", None), "[]");
    assert_eq!(ok("ab\ncd", "^cd", Some("s")), "[]");
    // ONIG_SYNTAX_PERL_NG turns SINGLELINE on by default, so "^" never matches after a
    // newline even without "s".
    assert_eq!(ok("ab\ncd", "^cd", None), "[]");
    assert_eq!(
        ok("ab\ncd", "b.c", Some("p")),
        r#"[{"offset":1,"length":3,"string":"b\nc","captures":[]}]"#
    );
    // x: extended syntax.
    assert_eq!(
        ok("abc", " a b c ", Some("x")),
        r#"[{"offset":0,"length":3,"string":"abc","captures":[]}]"#
    );
    // n: skip empty matches; l: longest.
    assert_eq!(ok("aaa", "", Some("gn")), "[]");
    assert_eq!(
        ok("aaa", "a|aa|aaa", Some("gl")),
        r#"[{"offset":0,"length":3,"string":"aaa","captures":[]}]"#
    );
    // Repeated and empty modifier strings are fine.
    assert_eq!(ok("test", "t", Some("gg")), ok("test", "t", Some("g")));
    assert_eq!(
        ok("test", "t", Some("")),
        r#"[{"offset":0,"length":1,"string":"t","captures":[]}]"#
    );
}

#[test]
fn invalid_modifiers() {
    assert_eq!(
        err("test", "t", Some("q")),
        "q is not a valid modifier string"
    );
    assert_eq!(
        err("test", "t", Some("gq")),
        "gq is not a valid modifier string"
    );
    assert_eq!(
        err("test", "t", Some("G")),
        "G is not a valid modifier string"
    );
    assert_eq!(
        err("test", "t", Some("é")),
        "é is not a valid modifier string"
    );
    assert_eq!(
        err("x", "x", Some("g\0")),
        "g\0 is not a valid modifier string"
    );
    // Modifiers are checked before the regex is compiled.
    assert_eq!(
        err("test", "(", Some("q")),
        "q is not a valid modifier string"
    );
}

#[test]
fn compile_errors() {
    let cases = [
        ("(", "end pattern with unmatched parenthesis"),
        (")", "unmatched close parenthesis"),
        ("[a", "premature end of char-class"),
        ("a{2,1}", "upper is smaller than lower in repeat range"),
        ("*", "target of repeat operator is not specified"),
        ("\\k<nosuchname>", "undefined name <nosuchname> reference"),
        ("(?<1a>x)", "invalid group name <1a>"),
        ("(?<>x)", "group name is empty"),
        (
            "\\p{NoSuchProperty}",
            "invalid character property name {NoSuchProperty}",
        ),
        ("\\", "end pattern at escape"),
        ("(?z)", "undefined group option"),
        ("(a)\\2", "invalid backref number/name"),
        ("[b-a]", "empty range in char class"),
        ("x{99999999}", "too big number for repeat range"),
    ];
    for (re, msg) in cases {
        assert_eq!(
            err("test", re, None),
            format!("Regex failure: {msg}"),
            "{re}"
        );
    }
}

#[test]
fn error_text_is_truncated_like_jq() {
    // Oniguruma keeps 27 bytes of a name and appends "...".
    assert_eq!(
        err(
            "test",
            "(?<aVeryLongGroupNameThatExceedsTheLimit>x)\\k<aVeryLongGroupNameThatExceedsTheLimitToo>",
            None
        ),
        "Regex failure: undefined name <aVeryLongGroupNameThatExcee...> reference"
    );
    // ...which can split a character; jq turns the fragment into U+FFFD.
    assert_eq!(
        err("test", "\\k<ééééééééééééééééééééé>", None),
        "Regex failure: undefined name <ééééééééééééé�...> reference"
    );
    // jq reads the message as a C string, so a NUL in the name ends it.
    assert_eq!(
        err("x", "\\k<a\0b>", None),
        "Regex failure: invalid char in group name <a"
    );
}

#[test]
fn parse_depth_limit_is_jqs() {
    let nested = |n: usize| format!("{}a{}", "(".repeat(n), ")".repeat(n));
    assert_eq!(run("a", &nested(511), None, true), Ok("true".into()));
    // Oniguruma's default limit (4096) would accept this; jq sets 1024.
    assert_eq!(
        run("a", &nested(512), None, true),
        Err("Regex failure: parse depth limit over".into())
    );
    assert_eq!(
        run("a", &format!("a{}", "?".repeat(5000)), None, true),
        Err("Regex failure: parse depth limit over".into())
    );
}

#[test]
fn search_errors() {
    // Oniguruma's default retry limit (10M) stops catastrophic backtracking...
    let input = format!("{}b!", "a".repeat(30));
    assert_eq!(
        err(&input, "(a|a)*b$", None),
        "Regex failure: retry-limit-in-match over"
    );
    assert_eq!(
        run(&input, "(a|a)*b$", None, true),
        Err("Regex failure: retry-limit-in-match over".into())
    );
    // ...unless the optimizer rules the match out first (no "c" in the input).
    assert_eq!(ok(&format!("{}b", "a".repeat(64)), "(a|a)*c", None), "[]");
}

#[test]
fn nul_bytes_are_ordinary_characters() {
    assert_eq!(
        ok("a\0b", "a.b", None),
        r#"[{"offset":0,"length":3,"string":"a\u0000b","captures":[]}]"#
    );
    assert_eq!(
        ok("a\0b", "\0", None),
        r#"[{"offset":1,"length":1,"string":"\u0000","captures":[]}]"#
    );
}

#[test]
fn duplicate_names_label_every_group() {
    assert_eq!(
        ok("abc", "(?<a>a)|(?<b>b)|(?<a>c)", Some("g")),
        concat!(
            r#"[{"offset":0,"length":1,"string":"a","captures":[{"offset":0,"length":1,"string":"a","name":"a"},{"offset":-1,"string":null,"length":0,"name":"b"},{"offset":-1,"string":null,"length":0,"name":"a"}]},"#,
            r#"{"offset":1,"length":1,"string":"b","captures":[{"offset":-1,"string":null,"length":0,"name":"a"},{"offset":1,"length":1,"string":"b","name":"b"},{"offset":-1,"string":null,"length":0,"name":"a"}]},"#,
            r#"{"offset":2,"length":1,"string":"c","captures":[{"offset":-1,"string":null,"length":0,"name":"a"},{"offset":-1,"string":null,"length":0,"name":"b"},{"offset":2,"length":1,"string":"c","name":"a"}]}]"#
        )
    );
}

#[test]
fn cached_regex_gives_the_same_results() {
    for _ in 0..3 {
        assert_eq!(
            ok("a1b22", "[0-9]+", Some("g")),
            r#"[{"offset":1,"length":1,"string":"1","captures":[]},{"offset":3,"length":2,"string":"22","captures":[]}]"#
        );
    }
}

/// Every `_match_impl` call made by the regex cases in jq 1.8.1's `onig.test` and
/// `manonig.test` (collected by running them with the regex builtins shadowed by
/// logging copies), plus curated edge cases, with jq's exact output.
#[test]
fn corpus_matches_jq() {
    let corpus = include_str!("../testdata/regex_corpus.jsonl");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let input = row["input"].as_str().unwrap();
        let re = row["re"].as_str().unwrap();
        let flags = row["flags"].as_str();
        let test = row["test"].as_bool().unwrap();
        let expected = match (row.get("out"), row.get("err")) {
            (Some(out), _) => Ok(out.as_str().unwrap().to_owned()),
            (_, Some(err)) => Err(err.as_str().unwrap().to_owned()),
            _ => panic!("corpus row without a result: {line}"),
        };
        n += 1;
        let got = run(input, re, flags, test);
        if got != expected {
            failures.push(format!(
                "input={input:?} re={re:?} flags={flags:?} test={test}\n  jq:  {expected:?}\n  got: {got:?}"
            ));
        }
    }
    assert!(n > 500, "corpus too small: {n}");
    assert!(
        failures.is_empty(),
        "{} of {n} corpus cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
