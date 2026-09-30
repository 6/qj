//! Parser for jq's `--run-tests` file format (`jq.test`, `man.test`, ...).
//!
//! Mirrors `run_jq_tests` in jq 1.8.1's `src/jq_test.c`:
//! - A line is skipped when, after leading spaces/tabs, it is empty or starts
//!   with `#` (`skipline`).
//! - `%%FAIL` or `%%FAIL IGNORE MSG` (exact lines) mark the next program as one
//!   that must fail to compile; the lines after it, up to the next skipped
//!   line, are the expected error message.
//! - Otherwise a program line is followed by exactly one input line, read raw
//!   (it is *not* subject to `skipline`), then expected output lines up to the
//!   next skipped line.
//!
//! jq_diff never uses the expected lines (jq itself is the oracle); they are
//! kept for display. The parser also reads qj-specific directives, which jq
//! treats as ordinary comments:
//!
//! ```text
//! # jq_diff: modes=compact,file
//! # jq_diff: os=linux
//! ```
//!
//! `modes` restricts the modes in which the file's filter/input cases run;
//! `os` the operating systems (`std::env::consts::OS`: `linux`, `macos`), for
//! behaviour jq has on one platform and not on another.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Program + one input line (+ expected outputs, unused).
    Normal {
        input: String,
        expected: Vec<String>,
    },
    /// `%%FAIL` block: the program must fail to compile.
    Fail {
        ignore_msg: bool,
        expected: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// 1-based line number of the program line (what jq prints as
    /// "at line number N").
    pub line: usize,
    pub program: String,
    pub kind: Kind,
}

#[derive(Debug, Default)]
pub struct TestFile {
    pub cases: Vec<Case>,
    /// Modes from a `# jq_diff: modes=...` directive, if present.
    pub modes: Option<Vec<String>>,
    /// Operating systems from a `# jq_diff: os=...` directive, if present.
    pub os: Option<Vec<String>>,
}

fn skipline(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    rest.is_empty() || rest.starts_with('#') || rest.starts_with('\n')
}

fn strip_newline(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

/// Split into lines the way `fgets` sees them: each line keeps its `\n`,
/// the last one may not have it.
fn fgets_lines(content: &str) -> Vec<&str> {
    content.split_inclusive('\n').collect()
}

pub fn parse(content: &str) -> Result<TestFile, String> {
    let lines = fgets_lines(content);
    let mut out = TestFile::default();
    let mut i = 0;
    let mut must_fail: Option<bool> = None; // Some(ignore_msg)

    while i < lines.len() {
        let raw = lines[i];
        let lineno = i + 1;
        i += 1;

        if skipline(raw) {
            if let Some(rest) = raw.trim_end().strip_prefix("# jq_diff:") {
                for kv in rest.split_whitespace() {
                    match kv.split_once('=') {
                        Some(("modes", v)) => {
                            out.modes = Some(v.split(',').map(str::to_string).collect())
                        }
                        Some(("os", v)) => {
                            let os: Vec<String> = v.split(',').map(str::to_string).collect();
                            if let Some(bad) = os.iter().find(|o| !crate::cli::known_os(o)) {
                                return Err(format!("line {lineno}: unknown os {bad:?}"));
                            }
                            out.os = Some(os)
                        }
                        _ => {
                            return Err(format!("line {lineno}: unknown jq_diff directive {kv:?}"));
                        }
                    }
                }
            }
            continue;
        }
        if raw == "%%FAIL\n" || raw == "%%FAIL IGNORE MSG\n" {
            must_fail = Some(raw.starts_with("%%FAIL IGNORE"));
            continue;
        }

        let program = strip_newline(raw).to_string();

        if let Some(ignore_msg) = must_fail.take() {
            let mut expected = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                i += 1;
                if skipline(l) {
                    break;
                }
                expected.push(strip_newline(l).to_string());
            }
            out.cases.push(Case {
                line: lineno,
                program,
                kind: Kind::Fail {
                    ignore_msg,
                    expected,
                },
            });
            continue;
        }

        let Some(input_raw) = lines.get(i) else {
            return Err(format!(
                "line {lineno}: program {program:?} has no input line"
            ));
        };
        i += 1;
        let input = strip_newline(input_raw).to_string();

        let mut expected = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            i += 1;
            if skipline(l) {
                break;
            }
            expected.push(strip_newline(l).to_string());
        }
        out.cases.push(Case {
            line: lineno,
            program,
            kind: Kind::Normal { input, expected },
        });
    }
    Ok(out)
}

/// True when `input` is exactly one JSON object or array (surrounded only by
/// whitespace). Only brackets and strings are tracked, so non-standard
/// content such as `nan` inside the container still counts: whether a tool
/// accepts it is exactly what the ndjson mode should compare.
pub fn is_single_container(input: &str) -> bool {
    let t = input.trim_matches([' ', '\t', '\r', '\n']);
    let bytes = t.as_bytes();
    if !matches!(bytes.first(), Some(b'{') | Some(b'[')) {
        return false;
    }
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (pos, &b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                if depth == 0 {
                    return pos == bytes.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(line: usize, program: &str, input: &str, expected: &[&str]) -> Case {
        Case {
            line,
            program: program.into(),
            kind: Kind::Normal {
                input: input.into(),
                expected: expected.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    #[test]
    fn parses_normal_cases_with_comments_and_blank_lines() {
        let src = "# comment\n\n.a\n{\"a\":1}\n1\n\n  # indented comment\n.[]\n[1,2]\n1\n2\n";
        let f = parse(src).unwrap();
        assert_eq!(
            f.cases,
            vec![
                normal(3, ".a", "{\"a\":1}", &["1"]),
                normal(8, ".[]", "[1,2]", &["1", "2"]),
            ]
        );
        assert_eq!(f.modes, None);
    }

    #[test]
    fn input_line_is_read_raw() {
        // jq_test.c reads the input with a bare fgets: a blank or '#' line
        // right after the program is the input, not a separator.
        let f = parse(".\n\n\n[.]\n# not a comment\n").unwrap();
        assert_eq!(
            f.cases,
            vec![
                normal(1, ".", "", &[]),
                normal(4, "[.]", "# not a comment", &[])
            ]
        );
    }

    #[test]
    fn expected_block_ends_at_comment() {
        let f = parse(".\n1\n1\n# trailing note\n").unwrap();
        assert_eq!(f.cases, vec![normal(1, ".", "1", &["1"])]);
    }

    #[test]
    fn parses_fail_blocks() {
        let src = "%%FAIL\n{\njq: error: syntax error\n  more\n\n%%FAIL IGNORE MSG\n\n# c\nimport \"x\" as y; .\nwhatever\n";
        let f = parse(src).unwrap();
        assert_eq!(
            f.cases,
            vec![
                Case {
                    line: 2,
                    program: "{".into(),
                    kind: Kind::Fail {
                        ignore_msg: false,
                        expected: vec!["jq: error: syntax error".into(), "  more".into()],
                    },
                },
                Case {
                    line: 9,
                    program: "import \"x\" as y; .".into(),
                    kind: Kind::Fail {
                        ignore_msg: true,
                        expected: vec!["whatever".into()],
                    },
                },
            ]
        );
    }

    #[test]
    fn fail_marker_must_be_exact() {
        // "%%FAIL " with a trailing space is an ordinary program line, as in jq.
        let f = parse("%%FAIL \nnull\n").unwrap();
        assert_eq!(f.cases, vec![normal(1, "%%FAIL ", "null", &[])]);
    }

    #[test]
    fn missing_input_is_an_error() {
        assert!(parse(".\n").is_err());
    }

    #[test]
    fn last_line_without_newline() {
        let f = parse(".\n1\n1").unwrap();
        assert_eq!(f.cases, vec![normal(1, ".", "1", &["1"])]);
    }

    #[test]
    fn os_directive() {
        let f = parse("# jq_diff: os=linux\n.\n1\n").unwrap();
        assert_eq!(f.os, Some(vec!["linux".into()]));
        assert_eq!(parse(".\n1\n").unwrap().os, None);
        assert!(parse("# jq_diff: os=plan9\n").is_err());
    }

    #[test]
    fn modes_directive() {
        let f = parse("# jq_diff: modes=compact,file\n.\n1\n").unwrap();
        assert_eq!(f.modes, Some(vec!["compact".into(), "file".into()]));
        assert!(parse("# jq_diff: bogus=1\n").is_err());
    }

    #[test]
    fn upstream_suites_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat");
        let mut total = 0;
        for name in [
            "jq.test",
            "man.test",
            "manonig.test",
            "onig.test",
            "base64.test",
            "uri.test",
            "optional.test",
        ] {
            let content = std::fs::read_to_string(dir.join(name)).unwrap();
            let f = parse(&content).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!f.cases.is_empty(), "{name} has no cases");
            total += f.cases.len();
        }
        // jq 1.8.1's suites; update when the vendored version changes.
        assert_eq!(total, 838);
    }

    #[test]
    fn single_container_detection() {
        assert!(is_single_container("{\"a\":1}"));
        assert!(is_single_container("  [1,[2],{\"b\":\"]\"}] "));
        assert!(is_single_container("[nan, \"\\\"]\"]"));
        assert!(!is_single_container("1"));
        assert!(!is_single_container("\"[1]\""));
        assert!(!is_single_container("[1] [2]"));
        assert!(!is_single_container("{\"a\":1}{\"b\":2}"));
        assert!(!is_single_container("[1"));
        assert!(!is_single_container("\u{feff}[1]"));
    }
}
