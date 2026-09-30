//! Programs qj runs on simdjson's tape (src/io/tape_eval.rs) through the
//! CLI, against jq: the sequential loop (`--threads 1`, which jq_diff can't
//! pass to jq), the parallel engine (the default), files and stdin, with the
//! output options. Exit code, stdout and stderr (with the program name
//! normalized) must be jq's. Skipped when jq isn't installed.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn jq_available() -> bool {
    Command::new("jq")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Exit code, stdout, stderr.
fn run(tool: &str, args: &[&str], stdin: &[u8], cwd: &Path) -> (i32, Vec<u8>, String) {
    let mut child = Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let _ = child.stdin.take().expect("stdin").write_all(stdin);
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// qj's stderr as jq's: `qj:` at the start of a line is `jq:`.
fn as_jq(stderr: &str) -> String {
    stderr
        .split_inclusive('\n')
        .map(|l| match l.strip_prefix("qj:") {
            Some(rest) => format!("jq:{rest}"),
            None => l.to_owned(),
        })
        .collect()
}

const DOC: &str = r#"{
  "a": {"b": [1, -0, 0, 0.0, -0.0, 1.50, 1e2, 1E-7, 12345678901234567890, -9223372036854775808, 3.14159], "c": "é😀\u0000\t\"\\\/\u007f"},
  "b": null,
  "a": {"b": "dup", "b": "last", "x": {}, "y": [], "z": false},
  "k1": 1, "k2": 2, "k3": 3, "k4": 4, "k5": 5, "k6": 6, "k7": 7, "k8": 8, "k9": 9, "k1": "again",
  "arr": [{"a": 1, "b": "x"}, {"a": "x", "b": [1, 2]}, {"b": 2, "a": 1.0}, {"a": null}, {"a": true}],
  "\u0001key": "ctrl",
  "s": "é\n"
}
"#;

const MULTI: &str = "{\"a\":1,\"b\":\"x\"}\n[1,2,{\"a\":3}]\n{\"a\":{\"b\":2},\n \"c\":3, \"a\":{\"b\":[4, 5]}}\n\"str\"\n{\"a\":-0,\"b\":[],\"c\":{}}\n{\"b\":null}\n{\"a\":\"x\",\"a\":\"y\",\"b\":{\"a\":1,\"a\":2}}\n[]\n{}\n";

const PROGRAMS: &[&str] = &[
    ".",
    ".a",
    ".a.b",
    ".b.c",
    ".k1",
    ".arr[]",
    ".arr[].a",
    ".[]",
    "[.[]]",
    "length",
    ".a | length",
    "keys",
    "keys_unsorted",
    "map(length)",
    ".arr | map({a, b})",
    "{a, s}",
    ".arr[] | select(.a == 1)",
    ".arr[] | select(.a)",
    "select(.a != -0)",
    ".s",
    ".a?",
    ".arr[].b?",
    ".arr[].b[]?",
    "[.[]?]",
    "[.[][]?]",
    ".s[]?",
    "def f: .arr; f[] | .a",
    "def keys: .k1; keys",
    "def is_one: .a == 1; .arr[] | select(is_one)",
    ".arr[] | select(.a > 0)",
    ".arr[] | select(.a <= \"x\") | .b",
    ".a.b[] | select(. >= 1.5)",
    ".arr[] | select(.a == 1 and .b != null)",
    ".arr[] | select(.a == \"x\" or (.b | not))",
    ".a.b | add",
    "[.arr[] | .a] | add",
    "add(.k1, .k2)",
    "[.[] | length] | add",
];

const OPTIONS: &[&[&str]] = &[
    &["-c"],
    &[],
    &["-r"],
    &["-j"],
    &["-ac"],
    &["-S"],
    &["--tab"],
    &["--indent", "1"],
    &["-e", "-c"],
    &["--unbuffered", "-c"],
];

/// Every program and option on one input (a file, or stdin when `file` is
/// `None`), with the default thread count and `--threads 1`.
fn check_input(file: Option<(&str, &str)>, stdin: &str) {
    if !jq_available() {
        return;
    }
    let qj = env!("CARGO_BIN_EXE_qj");
    let dir = tempfile::tempdir().expect("tempdir");
    if let Some((name, content)) = file {
        std::fs::write(dir.path().join(name), content).expect("write");
    }
    for program in PROGRAMS {
        for opts in OPTIONS {
            let mut args: Vec<&str> = opts.to_vec();
            args.push(program);
            if let Some((name, _)) = file {
                args.push(name);
            }
            let want = run("jq", &args, stdin.as_bytes(), dir.path());
            for threads in [None, Some("1")] {
                let mut qargs: Vec<&str> = Vec::new();
                if let Some(t) = threads {
                    qargs.extend(["--threads", t]);
                }
                qargs.extend(&args);
                let (code, stdout, stderr) = run(qj, &qargs, stdin.as_bytes(), dir.path());
                assert_eq!(
                    (code, String::from_utf8_lossy(&stdout), as_jq(&stderr)),
                    (want.0, String::from_utf8_lossy(&want.1), want.2.clone()),
                    "qj {qargs:?} (stdin {:?})",
                    &stdin[..stdin.len().min(20)]
                );
            }
        }
    }
}

#[test]
fn document_file() {
    check_input(Some(("doc.json", DOC)), "");
}

#[test]
fn documents_file() {
    check_input(Some(("multi.json", MULTI)), "");
}

#[test]
fn document_stdin() {
    check_input(None, DOC);
}

#[test]
fn documents_stdin() {
    check_input(None, MULTI);
}

/// Outputs over 1 MB, which qj writes out while it prints them (in whole
/// stdio buffers): stdout and stderr merged into one file (`>out 2>&1`) show
/// where each buffer went out, which must be where jq's did. In `big.json`
/// the document is followed by a text that doesn't parse, whose error comes
/// after the document's whole buffers and before the rest. (stderr's `qj:`
/// is `jq:` anywhere here: it needn't start a line.)
#[test]
fn big_outputs_go_out_like_jq() {
    if !jq_available() {
        return;
    }
    let qj = env!("CARGO_BIN_EXE_qj");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut doc = String::from("[");
    for i in 0..20_000 {
        if i > 0 {
            doc.push(',');
        }
        doc.push_str(&format!(
            "{{\"i\":{i},\"s\":\"text \\u00e9 \\\"{i}\\\"\",\"a\":[1,2.50,-0,{{}}],\"n\":null}}"
        ));
    }
    doc.push_str("]\n");
    std::fs::write(dir.path().join("big.json"), format!("{doc}{{\"x\": ]\n")).expect("write");
    std::fs::write(dir.path().join("big2.json"), format!("{doc}{doc}")).expect("write");
    let cases: &[&[&str]] = &[
        &["-c", "."],
        &["."],
        &["--tab", "."],
        &["-c", ".[]"],
        &["-c", "map({i, s})"],
        &["-r", ".[] | .s"],
        &["-c", ". + ."],
        &["-c", "[.[] | select(.i == 5)]"],
    ];
    let merged = |script: String| {
        let status = Command::new("sh")
            .args(["-c", &script])
            .current_dir(dir.path())
            .status()
            .expect("sh");
        assert!(status.success());
        std::fs::read(dir.path().join("out")).expect("read out")
    };
    // Records for the parallel engine's jobs, some of which make errors
    // between outputs (`.a[0]`: jobs with and without errors).
    let mut records = String::new();
    for i in 0..8_000 {
        if i % 2_500 == 1_000 {
            records.push_str(&format!("{{\"i\":\"{i}\",\"s\":5,\"a\":{{}},\"n\":[]}}\n"));
        } else {
            records.push_str(&format!(
                "{{\"i\":{i},\"s\":\"text \\u00e9 \\\"{i}\\\"\",\"a\":[1,2.50,-0,{{}}],\"n\":null}}\n"
            ));
        }
    }
    std::fs::write(dir.path().join("records.json"), records).expect("write");
    let cases: Vec<&[&str]> = cases
        .iter()
        .copied()
        .chain([&["-c", ".a[0]"][..], &["--unbuffered", "-c", ".a[0]"]])
        .collect();
    for args in cases {
        let quoted: Vec<String> = args.iter().map(|a| format!("'{a}'")).collect();
        for file in ["big.json", "big2.json", "records.json"] {
            let script = |tool: &str, threads: &str| {
                format!(
                    "'{tool}' {threads} {} {file} > out 2>&1; echo \"status $?\" >> out",
                    quoted.join(" ")
                )
            };
            let want = String::from_utf8_lossy(&merged(script("jq", ""))).into_owned();
            for threads in ["", "--threads 1"] {
                let got = merged(script(qj, threads));
                let got = String::from_utf8_lossy(&got).replace("qj: ", "jq: ");
                assert!(
                    got == want,
                    "qj {threads} {args:?} {file}: merged output differs from jq's"
                );
            }
        }
    }
}
