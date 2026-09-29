//! Checks the *content* of `qj::jq::lang`'s AST against jq 1.8.1 itself (Track P).
//!
//! Each program is parsed, printed back as a fully parenthesized jq program from the
//! AST, and both texts are run by the jq binary on the test's input. Any structural
//! mistake in the AST (precedence, `?` placement, keys, patterns, string parts)
//! changes the output. `unparse_is_stable` runs in the fast suite without jq.
//!
//! ```text
//! JQ=/path/to/jq-1.8.1 QJ_JQ_TESTS=/path/to/jq-1.8.1/tests \
//!   cargo test --release --test jq_lang_roundtrip -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use qj::jq::lang::ast::*;
use qj::jq::lang::parse_program;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

// ---------------------------------------------------------------------------
// Unparser: AST → fully parenthesized jq source with the same meaning.
// ---------------------------------------------------------------------------

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn string_lit(out: &mut String, lit: &StringLit) {
    if let Some(f) = &lit.format {
        let _ = write!(out, "@{f} ");
    }
    out.push('"');
    for p in &lit.parts {
        match p {
            StrPart::Text(t) => {
                let mut s = String::new();
                json_string(&mut s, t);
                out.push_str(&s[1..s.len() - 1]);
            }
            StrPart::Interp(n) => {
                out.push_str("\\(");
                node(out, n);
                out.push(')');
            }
        }
    }
    out.push('"');
}

/// A node as a `Term`: wrapped in parentheses, except `.` (so that `.["a"]`, the
/// unparse of `.a`, unparses the same way after reparsing).
fn term(out: &mut String, n: &Node) {
    if let NodeKind::Identity = n.kind {
        out.push('.');
        return;
    }
    out.push('(');
    node(out, n);
    out.push(')');
}

fn pattern(out: &mut String, p: &Pattern) {
    match &p.kind {
        PatternKind::Var(name) => {
            let _ = write!(out, "${name}");
        }
        PatternKind::Array(elems) => {
            out.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                pattern(out, e);
            }
            out.push(']');
        }
        PatternKind::Object(entries) => {
            out.push('{');
            for (i, e) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match &e.kind {
                    ObjPatKind::Var(name) => {
                        let _ = write!(out, "${name}");
                    }
                    ObjPatKind::VarPattern(name, p) => {
                        let _ = write!(out, "${name}: ");
                        pattern(out, p);
                    }
                    ObjPatKind::Named(name, p) => {
                        json_string(out, name);
                        out.push_str(": ");
                        pattern(out, p);
                    }
                    ObjPatKind::Str(key, p) => {
                        string_lit(out, key);
                        out.push_str(": ");
                        pattern(out, p);
                    }
                    ObjPatKind::Computed {
                        key, pattern: p, ..
                    } => {
                        term(out, key);
                        out.push_str(": ");
                        pattern(out, p);
                    }
                    ObjPatKind::Error(_) => panic!("error node in a valid program"),
                }
            }
            out.push('}');
        }
    }
}

fn patterns(out: &mut String, pats: &[Pattern]) {
    for (i, p) in pats.iter().enumerate() {
        if i > 0 {
            out.push_str(" ?// ");
        }
        pattern(out, p);
    }
}

fn func_def(out: &mut String, d: &FuncDef) {
    let _ = write!(out, "def {}", d.name);
    if !d.params.is_empty() {
        out.push('(');
        for (i, p) in d.params.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            if p.kind == ParamKind::Value {
                out.push('$');
            }
            out.push_str(&p.name);
        }
        out.push(')');
    }
    out.push_str(": ");
    node(out, &d.body);
    out.push_str("; ");
}

fn node(out: &mut String, n: &Node) {
    match &n.kind {
        NodeKind::FuncDef { def, rest } => {
            func_def(out, def);
            node(out, rest);
        }
        NodeKind::As {
            source,
            patterns: pats,
            body,
        } => {
            term(out, source);
            out.push_str(" as ");
            patterns(out, pats);
            out.push_str(" | ");
            term(out, body);
        }
        NodeKind::Label { name, body } => {
            let _ = write!(out, "label ${name} | ");
            term(out, body);
        }
        NodeKind::Pipe(a, b) => {
            term(out, a);
            out.push_str(" | ");
            term(out, b);
        }
        NodeKind::Comma(a, b) => {
            term(out, a);
            out.push_str(", ");
            term(out, b);
        }
        NodeKind::Binary { op, lhs, rhs } => {
            term(out, lhs);
            let _ = write!(out, " {} ", op.as_str());
            term(out, rhs);
        }
        NodeKind::Identity => out.push('.'),
        NodeKind::Recurse => out.push_str(".."),
        NodeKind::Break(name) => {
            let _ = write!(out, "break ${name}");
        }
        NodeKind::Index {
            target,
            key,
            optional,
        } => {
            match target {
                Some(t) => term(out, t),
                None => out.push('.'),
            }
            out.push('[');
            node(out, key);
            out.push(']');
            if *optional {
                out.push('?');
            }
        }
        NodeKind::Each { target, optional } => {
            term(out, target);
            out.push_str(if *optional { "[]?" } else { "[]" });
        }
        NodeKind::Slice {
            target,
            from,
            to,
            optional,
        } => {
            term(out, target);
            out.push('[');
            if let Some(f) = from {
                node(out, f);
            }
            out.push(':');
            if let Some(t) = to {
                node(out, t);
            }
            out.push(']');
            if *optional {
                out.push('?');
            }
        }
        NodeKind::Optional(t) => {
            term(out, t);
            out.push('?');
        }
        NodeKind::Literal(lit) => match lit {
            Literal::Null => out.push_str("null"),
            Literal::True => out.push_str("true"),
            Literal::False => out.push_str("false"),
            Literal::Number(t) => out.push_str(t),
            Literal::String(s) => json_string(out, s),
        },
        NodeKind::Str(lit) => string_lit(out, lit),
        NodeKind::Format(name) => {
            let _ = write!(out, "@{name}");
        }
        NodeKind::Neg(t) => {
            out.push('-');
            term(out, t);
        }
        NodeKind::Array(q) => {
            out.push('[');
            if let Some(q) = q {
                node(out, q);
            }
            out.push(']');
        }
        NodeKind::Object(pairs) => {
            out.push('{');
            for (i, p) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match &p.kind {
                    DictPairKind::Named { key, value } => {
                        json_string(out, key);
                        out.push_str(": ");
                        term(out, value);
                    }
                    DictPairKind::Str { key, value } => {
                        string_lit(out, key);
                        out.push_str(": ");
                        term(out, value);
                    }
                    DictPairKind::StrShorthand(key) => string_lit(out, key),
                    DictPairKind::VarKey { name, value } => {
                        let _ = write!(out, "${name}: ");
                        term(out, value);
                    }
                    DictPairKind::Var(name) => {
                        let _ = write!(out, "${name}");
                    }
                    DictPairKind::NameShorthand(name) => out.push_str(name),
                    DictPairKind::LocObject => out.push_str("$__loc__"),
                    DictPairKind::Computed { key, value, .. } => {
                        term(out, key);
                        out.push_str(": ");
                        term(out, value);
                    }
                    DictPairKind::Error(_) => panic!("error node in a valid program"),
                }
            }
            out.push('}');
        }
        NodeKind::Reduce {
            source,
            patterns: pats,
            init,
            update,
        } => {
            out.push_str("reduce ");
            term(out, source);
            out.push_str(" as ");
            patterns(out, pats);
            out.push_str(" (");
            node(out, init);
            out.push_str("; ");
            node(out, update);
            out.push(')');
        }
        NodeKind::Foreach {
            source,
            patterns: pats,
            init,
            update,
            extract,
        } => {
            out.push_str("foreach ");
            term(out, source);
            out.push_str(" as ");
            patterns(out, pats);
            out.push_str(" (");
            node(out, init);
            out.push_str("; ");
            node(out, update);
            if let Some(e) = extract {
                out.push_str("; ");
                node(out, e);
            }
            out.push(')');
        }
        NodeKind::If { cond, then_, else_ } => {
            out.push_str("if ");
            node(out, cond);
            out.push_str(" then ");
            node(out, then_);
            if let Some(e) = else_ {
                out.push_str(" else ");
                node(out, e);
            }
            out.push_str(" end");
        }
        NodeKind::Try { body, handler } => {
            out.push_str("try ");
            term(out, body);
            if let Some(h) = handler {
                out.push_str(" catch ");
                term(out, h);
            }
        }
        NodeKind::VarTake(name) => {
            let _ = write!(out, "$$$${name}");
        }
        NodeKind::Var(name) => {
            let _ = write!(out, "${name}");
        }
        NodeKind::LocObject => out.push_str("$__loc__"),
        NodeKind::Call { name, args, .. } => {
            out.push_str(name);
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str("; ");
                    }
                    node(out, a);
                }
                out.push(')');
            }
        }
        NodeKind::Error => panic!("error node in a valid program"),
    }
}

fn unparse(p: &Program) -> String {
    let mut out = String::new();
    if let Some(m) = &p.module {
        out.push_str("module ");
        node(&mut out, &m.meta);
        out.push_str("; ");
    }
    for imp in &p.imports {
        match &imp.kind {
            ImportKind::Code(name) => {
                out.push_str("import ");
                json_string(&mut out, &imp.path);
                let _ = write!(out, " as {name}");
            }
            ImportKind::Data(name) => {
                out.push_str("import ");
                json_string(&mut out, &imp.path);
                let _ = write!(out, " as ${name}");
            }
            ImportKind::Include => {
                out.push_str("include ");
                json_string(&mut out, &imp.path);
            }
        }
        if let Some(meta) = &imp.meta {
            out.push(' ');
            node(&mut out, meta);
        }
        out.push_str("; ");
    }
    match &p.body {
        ProgramBody::Main(n) => node(&mut out, n),
        ProgramBody::Library(defs) => {
            for d in defs {
                func_def(&mut out, d);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Test inputs
// ---------------------------------------------------------------------------

/// `(origin, program, input)` for every non-`%%FAIL` test of a jq `.test` file.
fn test_file_cases(path: &Path) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read(path).unwrap();
    let lines: Vec<&[u8]> = text.split(|&b| b == b'\n').collect();
    let skipline = |l: &[u8]| {
        let t: Vec<u8> = l
            .iter()
            .copied()
            .skip_while(|&b| b == b' ' || b == b'\t')
            .collect();
        t.is_empty() || t[0] == b'#'
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if skipline(lines[i]) {
            i += 1;
            continue;
        }
        let fail = lines[i].starts_with(b"%%FAIL");
        if fail {
            i += 1;
        }
        if i >= lines.len() {
            break;
        }
        let prog = lines[i].to_vec();
        let input = lines.get(i + 1).map(|l| l.to_vec()).unwrap_or_default();
        if !fail {
            out.push((format!("{name}:{}", i + 1), prog, input));
        }
        while i < lines.len() && !skipline(lines[i]) {
            i += 1;
        }
    }
    out
}

fn upstream_cases() -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut dirs = vec![Path::new(ROOT).join("tests/jq_compat")];
    if let Ok(extra) = std::env::var("QJ_JQ_TESTS") {
        dirs.push(PathBuf::from(extra));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            if f.extension().is_some_and(|e| e == "test") {
                for c in test_file_cases(&f) {
                    if seen.insert((c.1.clone(), c.2.clone())) {
                        out.push(c);
                    }
                }
            }
        }
    }
    out
}

fn corpus_programs() -> Vec<(String, Vec<u8>)> {
    let text = std::fs::read_to_string(Path::new(ROOT).join("tests/jq_lang/corpus.jsonl")).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with('"'))
        .map(|(i, l)| {
            let s: String = serde_json::from_str(l).unwrap();
            (format!("corpus.jsonl:{}", i + 1), s.into_bytes())
        })
        .collect()
}

#[test]
fn unparse_is_stable() {
    // Unparsing is idempotent after one round: parse(unparse(ast)) unparses to the
    // same text. This catches unparser bugs without jq.
    let mut programs: Vec<Vec<u8>> = upstream_cases().into_iter().map(|c| c.1).collect();
    programs.extend(corpus_programs().into_iter().map(|c| c.1));
    programs.push(std::fs::read(Path::new(ROOT).join("tests/jq_lang/builtin.jq")).unwrap());
    let mut checked = 0;
    for src in programs {
        let Ok(ast) = parse_program(&src) else {
            continue;
        };
        let once = unparse(&ast);
        let again = parse_program(&once)
            .unwrap_or_else(|e| panic!("unparsed {once:?} doesn't parse: {e:?}"));
        assert_eq!(unparse(&again), once, "{}", String::from_utf8_lossy(&src));
        checked += 1;
    }
    assert!(checked > 500, "{checked}");
}

fn run_jq(jq: &str, program: &[u8], input: &[u8]) -> (i32, String, String) {
    use std::io::Write;
    let mut child = Command::new(jq)
        .arg("-c")
        .arg("-L")
        .arg(Path::new(ROOT).join("tests/jq_compat/modules"))
        .arg("--")
        .arg(std::ffi::OsStr::from_bytes(program))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run jq");
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// builtin.jq's AST, checked through jq: every upstream test program runs with all of
/// builtin.jq's definitions prepended, once verbatim and once unparsed from our AST.
/// User definitions shadow the builtins, so a wrong tree for any builtin changes the
/// behavior of the tests that use it.
#[test]
#[ignore]
fn builtin_jq_roundtrip_vs_live_jq() {
    let jq = std::env::var("JQ").unwrap_or_else(|_| "jq".into());
    let builtin = std::fs::read(Path::new(ROOT).join("tests/jq_lang/builtin.jq")).unwrap();
    let ast = parse_program(&builtin).unwrap();
    let unparsed = unparse(&ast);
    let mut jobs = Vec::new();
    for (origin, src, input) in upstream_cases() {
        let Ok(p) = parse_program(&src) else {
            continue;
        };
        // directives must come first, and $__loc__ would see different line numbers
        if p.module.is_some() || !p.imports.is_empty() || src.windows(8).any(|w| w == b"$__loc__") {
            continue;
        }
        let mut original = builtin.clone();
        original.push(b'\n');
        original.extend_from_slice(&src);
        let mut ours = unparsed.clone().into_bytes();
        ours.push(b'\n');
        ours.extend_from_slice(&src);
        jobs.push((origin, original, ours, input));
    }
    let threads = 6;
    let chunk = jobs.len().div_ceil(threads);
    let failures: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|jobs| {
                let jq = jq.clone();
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    for (origin, original, ours, input) in jobs {
                        let a = run_jq(&jq, original, input);
                        let b = run_jq(&jq, ours, input);
                        let same = a.0 == b.0 && a.1 == b.1 && (a.0 == 3 || a.2 == b.2);
                        if !same {
                            failures.push(format!(
                                "{origin}: jq(builtin.jq): {a:?}\n  jq(unparsed): {b:?}"
                            ));
                        }
                    }
                    failures
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    for f in failures.iter().take(30) {
        eprintln!("MISMATCH {f}");
    }
    eprintln!(
        "builtin.jq round trip: {}/{} test programs behave identically under jq",
        jobs.len() - failures.len(),
        jobs.len()
    );
    assert!(failures.is_empty());
}

#[test]
#[ignore]
fn ast_roundtrip_vs_live_jq() {
    let jq = std::env::var("JQ").unwrap_or_else(|_| "jq".into());
    let mut cases = upstream_cases();
    cases.extend(
        corpus_programs()
            .into_iter()
            .map(|(o, p)| (o, p, b"null".to_vec())),
    );
    // Besides each test's own input, generic inputs that make more of a program's
    // structure observable (optional indexing, iteration, alternatives, errors).
    let extra: [&[u8]; 3] = [
        br#"{"a":{"b":[1,{"c":2}]},"d":"x","e":null}"#,
        br#"[3,[4],{"e":false},"s"]"#,
        b"null",
    ];
    let mut jobs: Vec<(String, Vec<u8>, String, Vec<u8>)> = Vec::new();
    for (origin, src, input) in &cases {
        let Ok(ast) = parse_program(src) else {
            continue;
        };
        let text = unparse(&ast);
        jobs.push((origin.clone(), src.clone(), text.clone(), input.clone()));
        for e in extra {
            if e != input.as_slice() {
                jobs.push((origin.clone(), src.clone(), text.clone(), e.to_vec()));
            }
        }
    }
    let threads = 6;
    let chunk = jobs.len().div_ceil(threads);
    let failures: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|jobs| {
                let jq = jq.clone();
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    for (origin, src, text, input) in jobs {
                        let a = run_jq(&jq, src, input);
                        let b = run_jq(&jq, text.as_bytes(), input);
                        // Compile errors mention columns, which differ; compare the
                        // rest verbatim.
                        let same = a.0 == b.0 && a.1 == b.1 && (a.0 == 3 || a.2 == b.2);
                        if !same {
                            failures.push(format!(
                                "{origin}: {}\n  input: {}\n  unparsed: {text}\n  jq(original): {a:?}\n  jq(unparsed): {b:?}",
                                String::from_utf8_lossy(src),
                                String::from_utf8_lossy(input)
                            ));
                        }
                    }
                    failures
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let compared = jobs.len();
    for f in failures.iter().take(30) {
        eprintln!("MISMATCH {f}");
    }
    eprintln!(
        "AST round trip: {}/{} (program, input) runs behave identically under jq",
        compared - failures.len(),
        compared
    );
    assert!(failures.is_empty());
}
