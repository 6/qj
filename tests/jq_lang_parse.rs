//! jq 1.8.1 parser conformance for `qj::jq::lang` (Track P).
//!
//! * `parse_matches_recorded_jq`: fast. Replays `tests/jq_lang/cases.jsonl`, which
//!   records what jq 1.8.1 does with every program from the upstream `.test` files
//!   (including `%%FAIL` blocks), the module files, `builtin.jq`, the hand-written
//!   corpus (`tests/jq_lang/corpus.jsonl`), and a sample of generated syntax-error
//!   mutations. Every program must be classified exactly as jq classifies it (syntax
//!   error or not), and every parse-time error must be reported byte-for-byte as jq
//!   reports it.
//! * `parse_vs_live_jq` (ignored): the same checks against the jq binary itself, on
//!   the full mutation set. `QJ_BLESS=1` rewrites `cases.jsonl`.
//!
//!   ```text
//!   cargo test --release --test jq_lang_parse -- --ignored --nocapture
//!   QJ_BLESS=1 QJ_JQ_TESTS=/path/to/jq-1.8.1/tests cargo test --release --test jq_lang_parse parse_vs_live_jq -- --ignored --nocapture
//!   ```
//!
//!   `QJ_JQ_TESTS` adds a directory of upstream `.test` files (man.test, onig.test,
//!   ...) to the ones in `tests/jq_compat/`. `JQ` selects the jq binary.

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use qj::jq::lang::ast::{DictPairKind, Literal, Node, NodeKind};
use qj::jq::lang::lexer::tokenize;
use qj::jq::lang::locfile::{LocFile, compile_errors_summary};
use qj::jq::lang::{NoHooks, ParseHooks, parse};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn data_dir() -> PathBuf {
    Path::new(ROOT).join("tests/jq_lang")
}

#[derive(Clone, Debug)]
struct Case {
    origin: String,
    src: Vec<u8>,
    /// jq's exit code when compiling the program without running it
    /// (`jq -- PROGRAM </dev/null`): 0 or 3.
    exit: i32,
    /// jq's stderr; when `stderr_fnv` is set (huge outputs), only the first line of
    /// each message.
    stderr: String,
    stderr_fnv: Option<u64>,
}

/// Recorded stderr longer than this is stored as a hash plus message heads.
const MAX_RECORDED_STDERR: usize = 4096;

fn message_heads(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| l.starts_with("jq: "))
        .map(|l| format!("{l}\n"))
        .collect()
}

// ---------------------------------------------------------------------------
// Our side: parse and render errors the way jq prints them.
// ---------------------------------------------------------------------------

/// A stand-in for Track C's hooks: jq's `block_is_const` checks for the simple
/// constants the parser can judge without the value layer. Anything else marks the
/// case as uncertain (not compared byte-for-byte).
#[derive(Default)]
struct TestHooks {
    uncertain: bool,
}

enum Const {
    Unknown,
    Runtime,
    Value(&'static str, String),
}

fn json_string(s: &str) -> Option<String> {
    if s.chars()
        .all(|c| (' '..='~').contains(&c) && c != '"' && c != '\\')
    {
        Some(format!("\"{s}\""))
    } else {
        None
    }
}

fn plain_number(t: &str) -> bool {
    let (int, frac) = match t.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (t, None),
    };
    let int_ok = int == "0"
        || (!int.is_empty() && !int.starts_with('0')) && int.bytes().all(|b| b.is_ascii_digit());
    int_ok && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

fn comma_leaves<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    match &n.kind {
        NodeKind::Comma(a, b) => {
            comma_leaves(a, out);
            comma_leaves(b, out);
        }
        _ => out.push(n),
    }
}

fn const_eval(n: &Node) -> Const {
    match &n.kind {
        NodeKind::Literal(Literal::Null) => Const::Value("null", "null".into()),
        NodeKind::Literal(Literal::True) => Const::Value("boolean", "true".into()),
        NodeKind::Literal(Literal::False) => Const::Value("boolean", "false".into()),
        NodeKind::Literal(Literal::Number(t)) if plain_number(t) => {
            Const::Value("number", t.clone())
        }
        NodeKind::Literal(Literal::Number(_)) => Const::Unknown,
        NodeKind::Literal(Literal::String(s)) => match json_string(s) {
            Some(j) => Const::Value("string", j),
            None => Const::Unknown,
        },
        NodeKind::Str(lit) => match lit.constant_value() {
            Some(s) => match json_string(&s) {
                Some(j) => Const::Value("string", j),
                None => Const::Unknown,
            },
            None => Const::Runtime,
        },
        NodeKind::Array(None) => Const::Value("array", "[]".into()),
        NodeKind::Array(Some(q)) => {
            let mut leaves = Vec::new();
            comma_leaves(q, &mut leaves);
            let mut items = Vec::new();
            for leaf in leaves {
                match const_eval(leaf) {
                    Const::Value(_, d) => items.push(d),
                    Const::Unknown => return Const::Unknown,
                    Const::Runtime => return Const::Runtime,
                }
            }
            Const::Value("array", format!("[{}]", items.join(",")))
        }
        NodeKind::Object(pairs) => {
            let mut items = Vec::new();
            let mut keys = HashSet::new();
            for p in pairs {
                let (key, value) = match &p.kind {
                    DictPairKind::Named { key, value } => (key.clone(), value),
                    DictPairKind::Str { key, value } => match key.constant_value() {
                        Some(k) => (k, value),
                        None => return Const::Unknown,
                    },
                    _ => return Const::Unknown,
                };
                let (Some(k), Const::Value(_, v)) = (json_string(&key), const_eval(value)) else {
                    return Const::Unknown;
                };
                if !keys.insert(key) {
                    return Const::Unknown;
                }
                items.push(format!("{k}:{v}"));
            }
            Const::Value("object", format!("{{{}}}", items.join(",")))
        }
        NodeKind::Binary { op, lhs, rhs } => {
            use qj::jq::lang::ast::BinOp::*;
            if !matches!(
                op,
                Add | Sub | Mul | Div | Mod | Eq | Ne | Lt | Gt | Le | Ge
            ) {
                return Const::Runtime;
            }
            match (const_eval(lhs), const_eval(rhs)) {
                (Const::Value(..), Const::Value(..)) => Const::Unknown, // needs folding
                (Const::Unknown, _) | (_, Const::Unknown) => Const::Unknown,
                _ => Const::Runtime,
            }
        }
        NodeKind::LocObject | NodeKind::FuncDef { .. } => Const::Unknown,
        _ => Const::Runtime,
    }
}

fn trunc_dump(d: &str) -> String {
    // jv_dump_string_trunc(x, buf, 15)
    if d.len() > 14 {
        format!("{}...", &d[..11])
    } else {
        d.to_string()
    }
}

impl ParseHooks for TestHooks {
    fn check_object_key(&mut self, key: &Node) -> Option<String> {
        match const_eval(key) {
            Const::Unknown => {
                self.uncertain = true;
                None
            }
            Const::Runtime => None,
            Const::Value("string", _) => None,
            Const::Value(kind, dump) => Some(format!(
                "Cannot use {kind} ({}) as object key",
                trunc_dump(&dump)
            )),
        }
    }

    fn check_metadata(&mut self, meta: &Node) -> Option<String> {
        match const_eval(meta) {
            Const::Unknown => {
                self.uncertain = true;
                None
            }
            Const::Runtime => Some("Module metadata must be constant".into()),
            Const::Value("object", _) => None,
            Const::Value(..) => Some("Module metadata must be an object".into()),
        }
    }
}

struct Ours {
    /// Rejected as a syntax error (parse with no semantic hooks fails).
    syntax_reject: bool,
    /// jq-style stderr of the parse with [`TestHooks`] ("" if it succeeded).
    stderr: String,
    uncertain: bool,
}

fn ours(src: &[u8]) -> Ours {
    let syntax_reject = parse(src, &mut NoHooks).is_err();
    let mut hooks = TestHooks::default();
    let stderr = match parse(src, &mut hooks) {
        Ok(_) => String::new(),
        Err(errors) => {
            let locfile = LocFile::new("<top-level>", src);
            let mut s = String::new();
            for e in &errors {
                s.push_str(&e.render(&locfile));
                s.push('\n');
            }
            s.push_str(&compile_errors_summary(errors.len()));
            s.push('\n');
            s
        }
    };
    Ours {
        syntax_reject,
        stderr,
        uncertain: hooks.uncertain,
    }
}

// ---------------------------------------------------------------------------
// jq's side: classify its stderr.
// ---------------------------------------------------------------------------

fn split_messages(stderr: &str) -> Vec<String> {
    let mut msgs: Vec<String> = Vec::new();
    for line in stderr.split_inclusive('\n') {
        if line.starts_with("jq: ") || msgs.is_empty() {
            msgs.push(line.to_string());
        } else {
            msgs.last_mut().unwrap().push_str(line);
        }
    }
    msgs
}

/// Errors jq's parser reports that the front-end must reproduce by itself.
fn is_syntax_level(msg: &str) -> bool {
    let Some(t) = msg.strip_prefix("jq: error: ") else {
        return false;
    };
    t.starts_with("syntax error")
        || (t.contains(" (while parsing '")
            && (t.starts_with("Invalid") || t.starts_with("Expected")))
        || t.starts_with("break requires a label to break to")
        || t.starts_with("try .[\"field\"] instead of .field")
        || t.starts_with("Possibly unterminated '")
        || t.starts_with("May need parentheses around object key expression")
        || t.starts_with("Import path must be constant")
        || t.starts_with("memory exhausted")
}

/// Parse-time errors that depend on constant folding (reported through hooks).
fn is_parse_semantic(msg: &str) -> bool {
    let Some(t) = msg.strip_prefix("jq: error: ") else {
        return false;
    };
    (t.starts_with("Cannot use ") && t.contains(" as object key at "))
        || t.starts_with("Module metadata must be ")
}

#[derive(Default)]
struct Stats {
    total: usize,
    classified_ok: usize,
    misclassified: Vec<String>,
    /// cases where jq's parse failed (syntax or parse-time semantic errors)
    parse_failures: usize,
    exact: usize,
    uncertain: usize,
    inexact: Vec<String>,
    /// %%FAIL cases whose jq error is produced by the parser
    fail_cases: usize,
    fail_exact: usize,
}

fn show(src: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(src))
}

fn check_case(case: &Case, stats: &mut Stats) {
    stats.total += 1;
    let msgs = split_messages(&case.stderr);
    let jq_syntax = case.exit == 3 && msgs.iter().any(|m| is_syntax_level(m));
    let jq_parse_failed = case.exit == 3
        && msgs
            .iter()
            .any(|m| is_syntax_level(m) || is_parse_semantic(m));
    let o = ours(&case.src);
    if o.syntax_reject == jq_syntax {
        stats.classified_ok += 1;
    } else {
        stats.misclassified.push(format!(
            "{} {}: jq {}, qj {}\n--- jq:\n{}--- qj:\n{}",
            case.origin,
            show(&case.src),
            if jq_syntax { "rejects" } else { "accepts" },
            if o.syntax_reject {
                "rejects"
            } else {
                "accepts"
            },
            case.stderr,
            o.stderr
        ));
    }
    let is_fail_case = case.origin.ends_with("%%FAIL");
    if jq_parse_failed {
        stats.parse_failures += 1;
        if is_fail_case {
            stats.fail_cases += 1;
        }
        let same = match case.stderr_fnv {
            Some(h) => fnv1a(o.stderr.as_bytes()) == h,
            None => o.stderr == case.stderr,
        };
        if o.uncertain {
            stats.uncertain += 1;
        } else if same {
            stats.exact += 1;
            if is_fail_case {
                stats.fail_exact += 1;
            }
        } else {
            stats.inexact.push(format!(
                "{} {}\n--- jq:\n{}--- qj:\n{}",
                case.origin,
                show(&case.src),
                case.stderr,
                o.stderr
            ));
        }
    } else if !o.stderr.is_empty() && !o.uncertain {
        // jq's parser accepted it, so ours (with the stand-in hooks) must too.
        stats.inexact.push(format!(
            "{} {}: jq's parser accepts\n--- jq:\n{}--- qj:\n{}",
            case.origin,
            show(&case.src),
            case.stderr,
            o.stderr
        ));
    }
}

fn report(stats: &Stats, max_show: usize) {
    for m in stats.misclassified.iter().take(max_show) {
        eprintln!("MISCLASSIFIED {m}");
    }
    for m in stats.inexact.iter().take(max_show) {
        eprintln!("INEXACT {m}");
    }
    let pct = |a: usize, b: usize| {
        if b == 0 {
            100.0
        } else {
            100.0 * a as f64 / b as f64
        }
    };
    eprintln!(
        "classification (syntax error vs not): {}/{} ({:.2}%)",
        stats.classified_ok,
        stats.total,
        pct(stats.classified_ok, stats.total)
    );
    eprintln!(
        "parse-time error output byte-exact: {}/{} ({:.2}%), {} not compared (need constant folding)",
        stats.exact,
        stats.parse_failures - stats.uncertain,
        pct(stats.exact, stats.parse_failures - stats.uncertain),
        stats.uncertain
    );
    eprintln!(
        "upstream %%FAIL cases with parse-time errors: {}/{} byte-exact",
        stats.fail_exact, stats.fail_cases
    );
}

// ---------------------------------------------------------------------------
// Recorded expectations
// ---------------------------------------------------------------------------

fn load_cases(path: &Path) -> Vec<Case> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let generated = generated_programs();
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("bad cases.jsonl line");
            let origin = v["origin"].as_str().unwrap().to_string();
            let src = if let Some(h) = v.get("src_hex") {
                from_hex(h.as_str().unwrap())
            } else if let Some(s) = v.get("src") {
                s.as_str().unwrap().as_bytes().to_vec()
            } else {
                // generated programs are stored by name
                generated
                    .iter()
                    .find(|(o, _)| *o == origin)
                    .unwrap_or_else(|| panic!("unknown generated program {origin}"))
                    .1
                    .clone()
            };
            let text_of = |k: &str| v.get(k).and_then(|s| s.as_str()).unwrap_or("").to_string();
            let stderr_fnv = v
                .get("stderr_fnv")
                .map(|h| u64::from_str_radix(h.as_str().unwrap(), 16).unwrap());
            Case {
                origin,
                src,
                exit: v["exit"].as_i64().unwrap() as i32,
                stderr: if stderr_fnv.is_some() {
                    text_of("stderr_heads")
                } else {
                    text_of("stderr")
                },
                stderr_fnv,
            }
        })
        .collect()
}

fn from_hex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn parse_matches_recorded_jq() {
    let cases = load_cases(&data_dir().join("cases.jsonl"));
    assert!(cases.len() > 1000, "expected the recorded corpus");
    let mut stats = Stats::default();
    for c in &cases {
        check_case(c, &mut stats);
    }
    report(&stats, 20);
    assert!(
        stats.misclassified.is_empty(),
        "{} programs classified differently from jq",
        stats.misclassified.len()
    );
    assert!(
        stats.inexact.is_empty(),
        "{} programs with error output differing from jq",
        stats.inexact.len()
    );
}

#[test]
fn builtin_jq_parses_as_library() {
    let src = std::fs::read(data_dir().join("builtin.jq")).unwrap();
    let program = parse(&src, &mut NoHooks).expect("builtin.jq must parse");
    assert!(program.module.is_none() && program.imports.is_empty());
    match &program.body {
        qj::jq::lang::ast::ProgramBody::Library(defs) => {
            assert!(defs.len() > 100, "{} defs", defs.len());
            assert_eq!(defs[0].name, "halt_error");
        }
        _ => panic!("builtin.jq has no main program"),
    }
}

// ---------------------------------------------------------------------------
// Live comparison against the jq binary
// ---------------------------------------------------------------------------

fn jq_binary() -> String {
    std::env::var("JQ").unwrap_or_else(|_| "jq".to_string())
}

/// Compile without running: with no `-n` and empty stdin, jq never executes the
/// program, so compile errors are all it can report.
fn run_jq(jq: &str, src: &[u8]) -> (i32, String) {
    let out = Command::new(jq)
        .arg("--")
        .arg(std::ffi::OsStr::from_bytes(src))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run jq");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Programs of a jq `.test` file (jq_test.c's format), with `%%FAIL` marked.
fn test_file_programs(path: &Path) -> Vec<(String, Vec<u8>)> {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut lines = text.split(|&b| b == b'\n').enumerate().peekable();
    let skipline = |l: &[u8]| {
        let t: Vec<u8> = l
            .iter()
            .copied()
            .skip_while(|&b| b == b' ' || b == b'\t')
            .collect();
        t.is_empty() || t[0] == b'#'
    };
    while let Some((i, line)) = lines.next() {
        if skipline(line) {
            continue;
        }
        let mut fail = false;
        let mut prog_line = (i, line);
        if line.starts_with(b"%%FAIL") {
            fail = true;
            match lines.next() {
                Some(l) => prog_line = l,
                None => break,
            }
        }
        out.push((
            format!(
                "{name}:{}{}",
                prog_line.0 + 1,
                if fail { " %%FAIL" } else { "" }
            ),
            prog_line.1.to_vec(),
        ));
        // skip the rest of the test (input and outputs / error lines)
        while let Some((_, l)) = lines.peek() {
            if skipline(l) {
                break;
            }
            lines.next();
        }
    }
    out
}

fn corpus_programs() -> Vec<(String, Vec<u8>)> {
    let text = std::fs::read_to_string(data_dir().join("corpus.jsonl")).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with('"'))
        .map(|(i, l)| {
            let s: String = serde_json::from_str(l).expect("bad corpus line");
            (format!("corpus.jsonl:{}", i + 1), s.into_bytes())
        })
        .collect()
}

fn module_files(dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            module_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "jq") {
            let rel = p.strip_prefix(ROOT).unwrap_or(&p).display().to_string();
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

/// Small deterministic PRNG (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const REPLACEMENTS: &[&str] = &[
    ")", "]", "}", "(", "[", "{", ",", "|", ";", ":", ".", "..", "?", "?//", "//", "=", "|=", "+=",
    "==", "and", "as", "$x", "\"", "1", "x", "if", "then", "else", "elif", "end", "try", "catch",
    "def", "reduce", "foreach", "label", "break", "import", "include", "module", "$__loc__",
    "@base64", "\\(", "#", "-", "`", "\"\\x\"", "\"\\(",
];

/// Syntax-error mutations of a program: truncations, deletions, duplications and
/// replacements at token boundaries, chosen deterministically.
fn mutations(src: &[u8], per_program: usize) -> Vec<Vec<u8>> {
    let toks: Vec<_> = tokenize(src)
        .into_iter()
        .filter(|(t, _)| t.sym != 0)
        .map(|(_, loc)| (loc.start as usize, loc.end as usize))
        .collect();
    if toks.is_empty() {
        return Vec::new();
    }
    let mut rng = Rng(fnv1a(src) | 1);
    let mut out = Vec::new();
    for _ in 0..per_program {
        let (s, e) = toks[rng.below(toks.len())];
        let e = e.min(src.len());
        let m: Vec<u8> = match rng.below(6) {
            0 => src[..s].to_vec(),
            1 => src[..e].to_vec(),
            2 => [&src[..s], &src[e..]].concat(),
            3 => [&src[..e], b" ", &src[s..e], &src[e..]].concat(),
            4 => {
                let r = REPLACEMENTS[rng.below(REPLACEMENTS.len())].as_bytes();
                [&src[..s], r, &src[e..]].concat()
            }
            _ => {
                // cut inside a token (strings, numbers, identifiers)
                let cut = s + rng.below(e - s + 1);
                src[..cut].to_vec()
            }
        };
        if !m.contains(&0) {
            out.push(m);
        }
    }
    out
}

/// Lexemes for random token soups: every token kind, lexer-state openers and closers,
/// odd escapes and comments, and invalid bytes.
const SOUP: &[&[u8]] = &[
    b"as",
    b"def",
    b"module",
    b"import",
    b"include",
    b"if",
    b"then",
    b"else",
    b"elif",
    b"reduce",
    b"foreach",
    b"end",
    b"and",
    b"or",
    b"try",
    b"catch",
    b"label",
    b"break",
    b"f",
    b"a::b",
    b"true",
    b"null",
    b"not",
    b"$__loc__",
    b"$x",
    b"$$$$x",
    b"$",
    b"@base64",
    b"@",
    b"@1",
    b".a",
    b".if",
    b"..",
    b".",
    b".[",
    b".5",
    b"1",
    b"1.5",
    b"1e3",
    b"1.",
    b"007",
    b"1e",
    b"|",
    b",",
    b"=",
    b"==",
    b"!=",
    b"<",
    b"<=",
    b">",
    b">=",
    b"+",
    b"-",
    b"*",
    b"/",
    b"%",
    b"+=",
    b"-=",
    b"*=",
    b"/=",
    b"%=",
    b"//=",
    b"|=",
    b"//",
    b"?//",
    b"?",
    b":",
    b";",
    b"(",
    b")",
    b"[",
    b"]",
    b"{",
    b"}",
    b"\"a\"",
    b"\"\"",
    b"\"\\(",
    b"\"",
    b"\"\\x\"",
    b"\"\\u12\"",
    b"\"\\ud83d\"",
    b"\"\\n",
    b"\\(",
    b"\\",
    b"# c\n",
    b"# c \\\n",
    b"#\r\n",
    b"#",
    b" ",
    b"\n",
    b"\t",
    b"\r",
    b"`",
    b"!",
    b"\x80",
    b"\xc3",
    b"\xc3\xa9",
    b"\x0b",
];

/// Random token soups (deterministic): mostly syntax errors that drive bison's error
/// recovery through unusual states.
fn soup_programs(count: usize) -> Vec<(String, Vec<u8>)> {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    (0..count)
        .map(|i| {
            let len = 1 + rng.below(16);
            let mut p = Vec::new();
            for _ in 0..len {
                p.extend_from_slice(SOUP[rng.below(SOUP.len())]);
                match rng.below(4) {
                    0 | 1 => p.push(b' '),
                    2 => {}
                    _ => p.push(b'\n'),
                }
            }
            (format!("soup {i}"), p)
        })
        .collect()
}

/// Nesting depths around bison's 10000-state stack limit, and long chains.
fn generated_programs() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    // jq folds these while parsing, so they compile at any length
    let n = 100_000;
    out.push((
        format!("chain-plus[{n}]"),
        format!("1{}", "+1".repeat(n)).into_bytes(),
    ));
    out.push((
        format!("chain-comma[{n}]"),
        format!("[1{}]", ",1".repeat(n)).into_bytes(),
    ));
    for n in [3000usize, 3300, 3332, 3333, 3334, 3400] {
        let pairs: Vec<String> = (0..n).map(|i| format!("a{i}:1")).collect();
        out.push((
            format!("object-pairs[{n}]"),
            format!("{{{}}}", pairs.join(",")).into_bytes(),
        ));
    }
    for n in [9990usize, 9995, 9996] {
        out.push((format!("deep-neg[{n}]"), ("-".repeat(n) + "1").into_bytes()));
    }
    for n in [9995usize, 9996, 9997, 9998, 10001] {
        out.push((
            format!("deep[{n}]"),
            ("[".repeat(n) + &"]".repeat(n)).into_bytes(),
        ));
    }
    for n in [4997usize, 4998, 4999, 5000] {
        out.push((
            format!("deep-paren-plus[{n}]"),
            ("(1+".repeat(n) + "1" + &")".repeat(n)).into_bytes(),
        ));
    }
    for n in [3331usize, 3332, 3333, 3334] {
        out.push((
            format!("deep-interp[{n}]"),
            ("\"\\(".repeat(n) + &")\"".repeat(n)).into_bytes(),
        ));
    }
    for n in [2000usize, 4990, 4999, 5000] {
        out.push((format!("deep-neg[{n}]"), ("-".repeat(n) + "1").into_bytes()));
    }
    out
}

#[test]
#[ignore]
fn parse_vs_live_jq() {
    let jq = jq_binary();
    let version = Command::new(&jq)
        .arg("--version")
        .output()
        .expect("jq not found");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "jq-1.8.1",
        "set JQ to a jq 1.8.1 binary"
    );

    // Sources: upstream test files, module files, builtin.jq, corpus.
    let mut dirs = vec![Path::new(ROOT).join("tests/jq_compat")];
    if let Ok(extra) = std::env::var("QJ_JQ_TESTS") {
        dirs.push(PathBuf::from(extra));
    }
    let mut base: Vec<(String, Vec<u8>)> = Vec::new();
    for dir in &dirs {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            if f.extension().is_some_and(|e| e == "test") {
                base.extend(test_file_programs(&f));
            }
        }
    }
    module_files(&Path::new(ROOT).join("tests/jq_compat/modules"), &mut base);
    base.push((
        "builtin.jq".into(),
        std::fs::read(data_dir().join("builtin.jq")).unwrap(),
    ));
    base.extend(corpus_programs());

    let mut seen = HashSet::new();
    let mut cases: Vec<(String, Vec<u8>, bool)> = Vec::new(); // (origin, src, recorded)
    for (origin, src) in &base {
        if seen.insert(src.clone()) {
            cases.push((origin.clone(), src.clone(), true));
        }
    }
    let per_program: usize = std::env::var("QJ_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    for (origin, src) in &base {
        for (i, m) in mutations(src, per_program).into_iter().enumerate() {
            if seen.insert(m.clone()) {
                // record a deterministic sample in cases.jsonl
                let recorded = fnv1a(&m).is_multiple_of(20);
                cases.push((format!("{origin} mutation {i}"), m, recorded));
            }
        }
    }
    for (origin, src) in generated_programs() {
        if seen.insert(src.clone()) {
            cases.push((origin, src, true));
        }
    }
    let soups: usize = std::env::var("QJ_SOUPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    for (origin, src) in soup_programs(soups) {
        if seen.insert(src.clone()) {
            let recorded = fnv1a(&src).is_multiple_of(40);
            cases.push((origin, src, recorded));
        }
    }
    eprintln!("running jq on {} programs", cases.len());

    // Run jq in parallel.
    let threads = 6;
    let results: Vec<(i32, String)> = {
        let chunks: Vec<_> = cases.chunks(cases.len().div_ceil(threads)).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunks
                .iter()
                .map(|chunk| {
                    let jq = jq.clone();
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|(_, src, _)| run_jq(&jq, src))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        })
    };

    let generated: HashSet<String> = generated_programs().into_iter().map(|(o, _)| o).collect();
    let mut stats = Stats::default();
    let mut recorded = String::new();
    for ((origin, src, rec), (exit, stderr)) in cases.iter().zip(results) {
        let case = Case {
            origin: origin.clone(),
            src: src.clone(),
            exit,
            stderr,
            stderr_fnv: None,
        };
        check_case(&case, &mut stats);
        if *rec {
            let mut obj = serde_json::Map::new();
            obj.insert("origin".into(), case.origin.clone().into());
            if !generated.contains(&case.origin) {
                match std::str::from_utf8(&case.src) {
                    Ok(s) => obj.insert("src".into(), s.into()),
                    Err(_) => obj.insert("src_hex".into(), to_hex(&case.src).into()),
                };
            }
            obj.insert("exit".into(), case.exit.into());
            if case.exit != 0 {
                if case.stderr.len() > MAX_RECORDED_STDERR {
                    let h = format!("{:016x}", fnv1a(case.stderr.as_bytes()));
                    obj.insert("stderr_fnv".into(), h.into());
                    obj.insert("stderr_heads".into(), message_heads(&case.stderr).into());
                } else {
                    obj.insert("stderr".into(), case.stderr.clone().into());
                }
            }
            recorded.push_str(&serde_json::Value::Object(obj).to_string());
            recorded.push('\n');
        }
    }
    report(&stats, 40);
    if std::env::var("QJ_BLESS").is_ok() {
        std::fs::write(data_dir().join("cases.jsonl"), recorded).unwrap();
        eprintln!("wrote tests/jq_lang/cases.jsonl");
    }
    assert!(stats.misclassified.is_empty());
    assert!(stats.inexact.is_empty());
}

/// builtin.jq is parsed at every startup; this prints the time per parse
/// (`cargo test --release --test jq_lang_parse builtin_parse_time -- --ignored --nocapture`).
#[test]
#[ignore]
fn builtin_parse_time() {
    let src = std::fs::read(data_dir().join("builtin.jq")).unwrap();
    let n = 2000;
    for _ in 0..50 {
        parse(&src, &mut NoHooks).unwrap();
    }
    let start = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(parse(std::hint::black_box(&src), &mut NoHooks).unwrap());
    }
    let per = start.elapsed() / n;
    eprintln!("builtin.jq ({} bytes): {per:?} per parse", src.len());
}

/// Development aid: `QJ_PARSE_PROBE='prog' cargo test --test jq_lang_parse probe -- --ignored --nocapture`
#[test]
#[ignore]
fn probe() {
    let Ok(src) = std::env::var("QJ_PARSE_PROBE") else {
        return;
    };
    let o = ours(src.as_bytes());
    let (exit, stderr) = run_jq(&jq_binary(), src.as_bytes());
    eprintln!("--- jq (exit {exit}):\n{stderr}--- qj:\n{}", o.stderr);
    if let Ok(p) = parse(src.as_bytes(), &mut NoHooks) {
        eprintln!("{}", p.to_sexpr());
    }
}
