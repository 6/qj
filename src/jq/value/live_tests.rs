//! Live differential tests against the `jq` binary on PATH (must be
//! jq-1.8.1). Ignored by default; run with
//!
//! ```text
//! cargo test --release --lib jq::value::live_tests -- --ignored --nocapture
//! ```
//!
//! Random inputs come from a seeded generator (`QJ_SEED` overrides the seed,
//! `QJ_CASES` the number of cases) so failures are reproducible.

use std::io::Write;
use std::process::{Command, Stdio};

use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn jq_available() -> bool {
    match Command::new("jq").arg("--version").output() {
        Ok(o) => {
            let v = String::from_utf8_lossy(&o.stdout);
            if v.trim() != "jq-1.8.1" {
                eprintln!("skipping: jq on PATH is {v:?}, need jq-1.8.1");
                return false;
            }
            true
        }
        Err(_) => {
            eprintln!("skipping: no jq on PATH");
            false
        }
    }
}

fn run_jq(args: &[&str], input: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut child = Command::new("jq")
        .args(args)
        .env_remove("JQ_COLORS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq");
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("jq output");
    writer.join().unwrap();
    (out.stdout, out.stderr)
}

fn ws(rng: &mut Rng, out: &mut Vec<u8>) {
    while rng.chance(15) {
        out.push(*rng.pick(b" \t\r\n"));
    }
}

fn gen_number(rng: &mut Rng, out: &mut Vec<u8>) {
    let digits = |rng: &mut Rng, n: u64, out: &mut Vec<u8>| {
        for _ in 0..n {
            out.push(b'0' + rng.below(10) as u8);
        }
    };
    if rng.chance(3) {
        let special: &[&[u8]] = &[
            b"nan",
            b"-nan",
            b"NaN",
            b"infinity",
            b"-Infinity",
            b"-0",
            b"-0.0",
        ];
        out.extend_from_slice(*rng.pick(special));
        return;
    }
    if rng.chance(40) {
        out.push(b'-');
    }
    let long = rng.chance(20);
    let int_len = 1 + rng.below(if long { 25 } else { 6 });
    if rng.chance(10) {
        out.push(b'0'); // leading zeros are accepted by jq
    }
    digits(rng, int_len, out);
    if rng.chance(40) {
        out.push(b'.');
        let long = rng.chance(20);
        let frac_len = rng.below(if long { 25 } else { 5 }) + 1;
        digits(rng, frac_len, out);
    }
    if rng.chance(30) {
        out.push(*rng.pick(b"eE"));
        if rng.chance(50) {
            out.push(*rng.pick(b"+-"));
        }
        let e = if rng.chance(10) {
            rng.below(2000)
        } else {
            rng.below(30)
        };
        out.extend_from_slice(e.to_string().as_bytes());
    }
}

fn gen_string(rng: &mut Rng, out: &mut Vec<u8>) {
    out.push(b'"');
    let n = rng.below(12);
    for _ in 0..n {
        match rng.below(10) {
            0 => {
                let esc: &[&[u8]] = &[
                    b"\\n", b"\\t", b"\\\"", b"\\\\", b"\\/", b"\\b", b"\\f", b"\\r",
                ];
                out.extend_from_slice(*rng.pick(esc));
            }
            1 => {
                // \uXXXX: anything but a lone high surrogate (a parse error)
                let mut c = rng.below(0x10000) as u32;
                if (0xD800..=0xDBFF).contains(&c) {
                    let lo = 0xDC00 + rng.below(0x400) as u32;
                    out.extend_from_slice(format!("\\u{c:04x}\\u{lo:04X}").as_bytes());
                } else {
                    if rng.chance(10) {
                        c = rng.below(0x20) as u32;
                    }
                    out.extend_from_slice(format!("\\u{c:04X}").as_bytes());
                }
            }
            2 => {
                let c = char::from_u32(0x80 + rng.below(0x3000) as u32).unwrap_or('\u{e9}');
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            3 => {
                let c = char::from_u32(0x10000 + rng.below(0x1000) as u32).unwrap_or('\u{1F600}');
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            4 => out.push(0x80 + rng.below(0x80) as u8), // invalid UTF-8
            5 => out.push(0x7F),
            _ => out.push(*rng.pick(b"abcxyzABC 0123{}[]:,")),
        }
    }
    out.push(b'"');
}

fn gen_value(rng: &mut Rng, depth: u32, out: &mut Vec<u8>) {
    let choice = rng.below(if depth >= 5 { 6 } else { 9 });
    match choice {
        0 => out.extend_from_slice(*rng.pick(&[&b"null"[..], b"true", b"false"])),
        1 | 2 => gen_number(rng, out),
        3..=5 => gen_string(rng, out),
        6 | 7 => {
            out.push(b'[');
            let n = rng.below(5);
            for i in 0..n {
                if i > 0 {
                    ws(rng, out);
                    out.push(b',');
                }
                ws(rng, out);
                gen_value(rng, depth + 1, out);
            }
            ws(rng, out);
            out.push(b']');
        }
        _ => {
            out.push(b'{');
            let n = rng.below(5);
            let keys: &[&[u8]] = &[
                b"\"a\"",
                b"\"b\"",
                b"\"\"",
                b"\"\\u00e9\"",
                b"\"A\"",
                b"\"aa\"",
            ];
            for i in 0..n {
                if i > 0 {
                    out.push(b',');
                }
                ws(rng, out);
                if rng.chance(70) {
                    out.extend_from_slice(*rng.pick(keys)); // duplicates on purpose
                } else {
                    gen_string(rng, out);
                }
                ws(rng, out);
                out.push(b':');
                ws(rng, out);
                gen_value(rng, depth + 1, out);
            }
            ws(rng, out);
            out.push(b'}');
        }
    }
}

/// Random documents printed with every output flag set, compared byte for
/// byte with jq.
#[test]
#[ignore]
fn live_random_documents() {
    if !jq_available() {
        return;
    }
    let seed = env_u64("QJ_SEED", 0x5eed);
    let cases = env_u64("QJ_CASES", 3000);
    let mut rng = Rng(seed | 1);
    let mut input = Vec::new();
    for _ in 0..cases {
        ws(&mut rng, &mut input);
        gen_value(&mut rng, 0, &mut input);
        input.push(b'\n');
    }
    let values: Vec<Value> = parse::parse_all(&input, ParseFlags::default())
        .into_iter()
        .map(|r| r.expect("generated documents are valid"))
        .collect();
    assert_eq!(values.len() as u64, cases);
    let flag_sets: &[&[&str]] = &[
        &["-c"],
        &[],
        &["-S", "-c"],
        &["-a", "-c"],
        &["--tab", "-S"],
        &["--indent", "3", "-a"],
        &["-C", "-c"],
        &["-C", "-S"],
    ];
    for flags in flag_sets {
        let mut args: Vec<&str> = flags.to_vec();
        args.push(".");
        let (want, err) = run_jq(&args, &input);
        assert!(
            err.is_empty(),
            "jq failed: {}",
            String::from_utf8_lossy(&err)
        );
        let mut opts = DumpOptions::pretty();
        let mut i = 0;
        while i < flags.len() {
            match flags[i] {
                "-c" => opts.indent = Indent::Compact,
                "--tab" => opts.indent = Indent::Tab,
                "--indent" => {
                    i += 1;
                    opts.indent = DumpOptions::with_indent(flags[i].parse().unwrap()).indent;
                }
                "-S" => opts.sort_keys = true,
                "-a" => opts.ascii = true,
                "-C" => opts.colors = Some(Colors::default()),
                _ => unreachable!(),
            }
            i += 1;
        }
        let mut got = Vec::new();
        for v in &values {
            dump(v, &opts, &mut got).unwrap();
            got.push(b'\n');
        }
        if got != want {
            // Report the first differing document.
            let gl: Vec<&[u8]> = got.split(|&b| b == b'\n').collect();
            let wl: Vec<&[u8]> = want.split(|&b| b == b'\n').collect();
            let k = gl
                .iter()
                .zip(wl.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            panic!(
                "flags {flags:?}: line {k} differs\n got {:?}\n jq  {:?}",
                String::from_utf8_lossy(gl.get(k).copied().unwrap_or_default()),
                String::from_utf8_lossy(wl.get(k).copied().unwrap_or_default())
            );
        }
    }
    eprintln!(
        "{cases} random documents x {} flag sets match jq",
        flag_sets.len()
    );
}

/// Random doubles (all bit patterns) and random decimal literals, printed
/// after `. * 1`, compared with jq.
#[test]
#[ignore]
fn live_number_sweep() {
    if !jq_available() {
        return;
    }
    let seed = env_u64("QJ_SEED", 0xd70a);
    let cases = env_u64("QJ_CASES", 200_000);
    let mut rng = Rng(seed | 1);
    let mut inputs: Vec<String> = Vec::with_capacity(cases as usize);
    for i in 0..cases {
        if i % 2 == 0 {
            let x = f64::from_bits(rng.next());
            if !x.is_finite() {
                continue;
            }
            // Rust's `{:e}` gives the shortest round-trip digits; jq reads
            // them back exactly (at most 17 significant digits).
            inputs.push(format!("{x:e}"));
        } else {
            let mut s = Vec::new();
            gen_number(&mut rng, &mut s);
            inputs.push(String::from_utf8(s).unwrap());
        }
    }
    let doc = inputs.join("\n");
    let (want, err) = run_jq(&["-c", "[., . * 1, -., length]"], doc.as_bytes());
    assert!(
        err.is_empty(),
        "jq failed: {}",
        String::from_utf8_lossy(&err)
    );
    let want = String::from_utf8(want).unwrap();
    let mut failures = 0;
    for (input, line) in inputs.iter().zip(want.lines()) {
        let v = parse_sized(input.as_bytes()).unwrap();
        let n = v.as_number().unwrap();
        let got = Value::from(vec![
            v.clone(),
            Value::number(n.value() * 1.0),
            Value::Number(n.negate()),
            Value::Number(n.abs()),
        ])
        .to_json();
        if got != line {
            failures += 1;
            if failures <= 20 {
                eprintln!("{input}: got {got}, jq {line}");
            }
        }
    }
    assert_eq!(failures, 0, "{failures} of {} numbers differ", inputs.len());
    eprintln!("{} numbers match jq", inputs.len());
}

fn gen_small_number(rng: &mut Rng) -> String {
    match rng.below(10) {
        0 => format!("-{}", rng.below(8)),
        1 => format!("{}.{}", rng.below(6), rng.below(10)),
        2 => format!("-{}.{}", rng.below(6), rng.below(10)),
        3 => (*rng.pick(&[
            "1e300", "-1e300", "1e1000", "-0", "0.5", "-0.5", "1.000", "2e0",
        ]))
        .to_owned(),
        _ => rng.below(7).to_string(),
    }
}

fn gen_key(rng: &mut Rng) -> String {
    match rng.below(12) {
        0..=3 => gen_small_number(rng),
        4..=6 => {
            (*rng.pick(&["\"a\"", "\"b\"", "\"c\"", "\"\"", "\"\u{e9}\"", "\"start\""])).to_owned()
        }
        7 | 8 => {
            let part = |rng: &mut Rng| {
                if rng.chance(25) {
                    "null".to_owned()
                } else {
                    gen_small_number(rng)
                }
            };
            let s = part(rng);
            let e = part(rng);
            match rng.below(10) {
                0 => format!("{{\"start\":{s}}}"),
                1 => format!("{{\"end\":{e}}}"),
                2 => format!("{{\"start\":\"x\",\"end\":{e}}}"),
                _ => format!("{{\"start\":{s},\"end\":{e}}}"),
            }
        }
        9 => (*rng.pick(&["null", "true", "false"])).to_owned(),
        10 => (*rng.pick(&["[]", "[1]", "[1,2]", "[\"a\"]"])).to_owned(),
        _ => "{}".to_owned(),
    }
}

fn gen_path(rng: &mut Rng) -> String {
    let n = rng.below(4);
    let keys: Vec<String> = (0..n).map(|_| gen_key(rng)).collect();
    format!("[{}]", keys.join(","))
}

fn gen_small_value(rng: &mut Rng) -> String {
    match rng.below(10) {
        0..=3 => {
            let n = rng.below(6);
            let items: Vec<String> = (0..n).map(|_| gen_small_scalar_or_nested(rng)).collect();
            format!("[{}]", items.join(","))
        }
        4..=6 => {
            let n = rng.below(5);
            let items: Vec<String> = (0..n)
                .map(|_| {
                    let k = *rng.pick(&["\"a\"", "\"b\"", "\"c\"", "\"\u{e9}\"", "\"A\""]);
                    format!("{k}:{}", gen_small_scalar_or_nested(rng))
                })
                .collect();
            format!("{{{}}}", items.join(","))
        }
        _ => {
            let mut out = Vec::new();
            gen_value(rng, 3, &mut out);
            String::from_utf8_lossy(&out).into_owned()
        }
    }
}

fn gen_small_scalar_or_nested(rng: &mut Rng) -> String {
    if rng.chance(25) {
        return gen_small_value(rng);
    }
    match rng.below(5) {
        0 => gen_small_number(rng),
        1 => (*rng.pick(&["\"x\"", "\"ab\"", "\"\"", "\"\u{e9}\""])).to_owned(),
        2 => (*rng.pick(&["null", "true", "false"])).to_owned(),
        _ => rng.below(9).to_string(),
    }
}

const LIVE_OPS: &[(&str, &str)] = &[
    ("get", "$a | .[$b]"),
    ("set", "$a | setpath([$b]; $c)"),
    ("has", "$a | has($b)"),
    ("getpath", "$a | getpath($b)"),
    ("setpath", "$a | setpath($b; $c)"),
    ("delpaths", "$a | delpaths($b)"),
    ("keys", "$a | keys"),
    ("keys_unsorted", "$a | keys_unsorted"),
    ("sort", "$a | sort"),
    ("unique", "$a | unique"),
    ("contains", "$a | contains($b)"),
    ("cmp", "[$a < $b, $a == $b, $a > $b, $a <= $b, $a >= $b]"),
    ("tojson", "$a | tojson"),
    ("length", "$a | length"),
    ("plus", "$a + $b"),
    ("multiply", "$a * $b"),
];

/// Random values, keys and paths through the jv_aux operations, compared
/// with jq (including error messages).
#[test]
#[ignore]
fn live_random_ops() {
    if !jq_available() {
        return;
    }
    let seed = env_u64("QJ_SEED", 0x0b5);
    let cases = env_u64("QJ_CASES", 20000);
    let mut rng = Rng(seed | 1);
    let mut rows: Vec<(usize, String, String, String)> = Vec::new();
    for _ in 0..cases {
        let op = rng.below(LIVE_OPS.len() as u64) as usize;
        let a = gen_small_value(&mut rng);
        let (b, c) = match LIVE_OPS[op].0 {
            "get" | "has" | "set" => (gen_key(&mut rng), gen_small_scalar_or_nested(&mut rng)),
            "getpath" | "setpath" => (gen_path(&mut rng), gen_small_scalar_or_nested(&mut rng)),
            "delpaths" => {
                let n = rng.below(4);
                let ps: Vec<String> = (0..n).map(|_| gen_path(&mut rng)).collect();
                (format!("[{}]", ps.join(",")), "null".to_owned())
            }
            _ => (gen_small_value(&mut rng), "null".to_owned()),
        };
        // jq 1.8.1 hangs on delpaths with NaN keys, and double-frees the key
        // when a slice key on an array is malformed (jv_dels frees the key
        // that parse_slice already consumed), corrupting its heap:
        // `jq -nc 'def f: [1,2] | try delpaths([[{}]]) catch .; [range(10) | f]'`
        // hangs. Keep both out.
        if LIVE_OPS[op].0 == "delpaths"
            && (b.contains("nan")
                || b.contains("NaN")
                || b.contains("{}")
                || b.contains("{\"end\"")
                || b.contains("\"start\":\"x\"")
                || b.matches("{\"start\":").count() != b.matches(",\"end\":").count())
        {
            continue;
        }
        // `string * n` repeats the string: keep n small.
        if LIVE_OPS[op].0 == "multiply" {
            let (av, bv) = (parse_sized(a.as_bytes()), parse_sized(b.as_bytes()));
            let big = |v: &Result<Value, Error>| {
                v.as_ref()
                    .ok()
                    .and_then(Value::as_f64)
                    .is_some_and(|x| x.abs() > 64.0 && x < i32::MAX as f64)
            };
            let is_str = |v: &Result<Value, Error>| matches!(v, Ok(Value::String(_)));
            if (is_str(&av) && big(&bv)) || (is_str(&bv) && big(&av)) {
                continue;
            }
        }
        rows.push((op, a, b, c));
    }
    let mut prog = String::from(". as [$op, $a, $b, $c] | try (");
    for (i, (name, expr)) in LIVE_OPS.iter().enumerate() {
        prog.push_str(if i == 0 { "if " } else { " elif " });
        prog.push_str(&format!("$op == \"{name}\" then {expr}"));
    }
    prog.push_str(" else error(\"bad op\") end | [0, .]) catch [1, .]");
    // One case per line (newlines only ever appear as whitespace between
    // tokens), which keeps failures easy to bisect.
    let input: String = rows
        .iter()
        .map(|(op, a, b, c)| {
            format!("[\"{}\",{a},{b},{c}]", LIVE_OPS[*op].0).replace('\n', " ") + "\n"
        })
        .collect();
    let (want, err) = run_jq(&["-c", &prog], input.as_bytes());
    assert!(
        err.is_empty(),
        "jq failed: {}",
        String::from_utf8_lossy(&err)
    );
    let want = String::from_utf8(want).unwrap();
    let n = want.lines().count();
    if let Ok(path) = std::env::var("QJ_DUMP") {
        std::fs::write(&path, &input).unwrap();
        std::fs::write(format!("{path}.prog"), &prog).unwrap();
    }
    if n != rows.len() {
        let bad = input.lines().nth(n).unwrap_or_default();
        panic!(
            "jq stopped after {n} of {} cases; next input: {bad}\nprogram: {prog}",
            rows.len()
        );
    }
    let mut failures = 0;
    for ((op, a, b, c), line) in rows.iter().zip(want.lines()) {
        let (av, bv, cv) = (
            parse_sized(a.as_bytes()).unwrap(),
            parse_sized(b.as_bytes()).unwrap(),
            parse_sized(c.as_bytes()).unwrap(),
        );
        // jq's $a/$b stay referenced by the input array meanwhile.
        let _keep = (av.clone(), bv.clone());
        let got = super::tests::tagged(super::tests::run_op(LIVE_OPS[*op].0, av, bv, cv));
        if got != line {
            failures += 1;
            if failures <= 25 {
                eprintln!(
                    "{} a={a} b={b} c={c}\n  got {got}\n  jq  {line}",
                    LIVE_OPS[*op].0
                );
            }
        }
    }
    assert_eq!(failures, 0, "{failures} of {} ops differ", rows.len());
    eprintln!("{} random ops match jq", rows.len());
}

/// A random ASCII-only, single-line document with random damage.
fn gen_mutated_doc(rng: &mut Rng) -> Vec<u8> {
    let mut doc = Vec::new();
    gen_value(rng, 1, &mut doc);
    for b in doc.iter_mut() {
        if *b >= 0x80 || *b == b'\n' || *b == b'\r' {
            *b = b'x';
        }
    }
    let n = rng.below(4);
    for _ in 0..n {
        if doc.is_empty() {
            break;
        }
        let at = rng.below(doc.len() as u64 + 1) as usize;
        match rng.below(6) {
            0 if at < doc.len() => {
                doc.remove(at);
            }
            1 | 2 => {
                let c = *rng.pick(b"[]{}:,\"\\ 0123456789.eE+-tfnulax'\t/");
                doc.insert(at, c);
            }
            3 if at < doc.len() => {
                doc[at] = *rng.pick(b"[]{}:,\" 1.eE-nx\\");
            }
            4 => doc.truncate(at),
            _ => {
                let end = (at + rng.below(6) as usize).min(doc.len());
                let piece = doc[at..end].to_vec();
                doc.splice(at..at, piece);
            }
        }
    }
    doc
}

/// Damaged documents through `fromjson` (jv_parse_sized) and through stdin
/// with `--seq` and `--stream-errors` (errors do not stop those modes), so
/// that error messages and positions are compared with jq in bulk.
#[test]
#[ignore]
fn live_parse_mutations() {
    if !jq_available() {
        return;
    }
    let seed = env_u64("QJ_SEED", 0x9a55);
    let cases = env_u64("QJ_CASES", 20000);
    let mut rng = Rng(seed | 1);
    let docs: Vec<Vec<u8>> = (0..cases).map(|_| gen_mutated_doc(&mut rng)).collect();

    // 1. fromjson, one process for all documents.
    let input: String = docs
        .iter()
        .map(|d| {
            let s = String::from_utf8(d.clone()).expect("ascii");
            Value::from(s).to_json() + "\n"
        })
        .collect();
    let (want, err) = run_jq(
        &["-c", "try (fromjson | [0, .]) catch [1, .]"],
        input.as_bytes(),
    );
    assert!(
        err.is_empty(),
        "jq failed: {}",
        String::from_utf8_lossy(&err)
    );
    let want = String::from_utf8(want).unwrap();
    let mut failures = 0;
    for (d, line) in docs.iter().zip(want.lines()) {
        let got = super::tests::tagged(parse_sized(d));
        if got != line {
            failures += 1;
            if failures <= 20 {
                eprintln!(
                    "fromjson {:?}\n  got {got}\n  jq  {line}",
                    String::from_utf8_lossy(d)
                );
            }
        }
    }
    assert_eq!(failures, 0, "{failures} fromjson results differ");

    // 2. stdin modes where parse errors are not fatal: one document per
    // line (jq reads line by line and drops the rest of a line after an
    // error in --stream-errors mode).
    for flags in [
        &["--seq"][..],
        &["--stream-errors"][..],
        &["--seq", "--stream"][..],
    ] {
        let mut input = Vec::new();
        for d in &docs[..docs.len().min(3000)] {
            if flags.contains(&"--seq") {
                input.push(0x1e);
            }
            input.extend_from_slice(d);
            input.push(b'\n');
        }
        let mut args: Vec<&str> = vec!["-c"];
        args.extend_from_slice(flags);
        args.push(".");
        let (want_out, want_err) = run_jq(&args, &input);
        let flags_s: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
        let chunks = super::tests::jq_stdin_chunks(&input);
        let (out, err) = super::tests::simulate_cli(&chunks, &flags_s);
        if out != want_out || err != want_err {
            let show = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            let (go, wo) = (show(&out), show(&want_out));
            let (ge, we) = (show(&err), show(&want_err));
            let k = go.lines().zip(wo.lines()).position(|(a, b)| a != b);
            let ke = ge.lines().zip(we.lines()).position(|(a, b)| a != b);
            panic!(
                "{flags:?}: stdout differs at line {k:?}: got {:?} jq {:?}\nstderr differs at line {ke:?}: got {:?} jq {:?}",
                k.and_then(|k| go.lines().nth(k)),
                k.and_then(|k| wo.lines().nth(k)),
                ke.and_then(|k| ge.lines().nth(k)),
                ke.and_then(|k| we.lines().nth(k)),
            );
        }
    }
    eprintln!("{} damaged documents match jq", docs.len());
}

/// Sorting with NaN keys, where jq's result depends on its qsort.
#[test]
#[ignore]
fn live_nan_sorts() {
    if !jq_available() {
        return;
    }
    let seed = env_u64("QJ_SEED", 0x7a7);
    let cases = env_u64("QJ_CASES", 3000);
    let mut rng = Rng(seed | 1);
    let mut docs: Vec<String> = Vec::new();
    for _ in 0..cases {
        let n = if rng.chance(80) {
            rng.below(60)
        } else {
            rng.below(400)
        };
        let nan_pct = rng.below(101);
        let items: Vec<String> = (0..n)
            .map(|i| {
                let k = if rng.chance(nan_pct) {
                    (*rng.pick(&["nan", "[nan]", "[nan,1]", "[nan,0]", "{\"a\":nan}"])).to_owned()
                } else {
                    match rng.below(4) {
                        0 => format!("[{}]", rng.below(3)),
                        1 => "null".to_owned(),
                        _ => rng.below(5).to_string(),
                    }
                };
                format!("{{\"k\":{k},\"i\":{i}}}")
            })
            .collect();
        docs.push(format!("[{}]", items.join(",")));
    }
    let input = docs.join("\n");
    let programs = [
        "sort_by(.k) | map(.i)",
        "group_by(.k) | map(map(.i))",
        "unique_by(.k) | map(.i)",
        "map([.k, .i]) | sort | map(.[1])",
    ];
    for prog in programs {
        let (want, err) = run_jq(&["-c", prog], input.as_bytes());
        assert!(
            err.is_empty(),
            "jq failed: {}",
            String::from_utf8_lossy(&err)
        );
        let want = String::from_utf8(want).unwrap();
        let mut failures = 0;
        for (doc, line) in docs.iter().zip(want.lines()) {
            let arr = parse_sized(doc.as_bytes()).unwrap();
            let arr = arr.as_array().unwrap();
            let get = |v: &Value, key: &str| v.as_object().unwrap().get(key).cloned().unwrap();
            let got = match prog {
                "map([.k, .i]) | sort | map(.[1])" => {
                    let pairs: Array = arr
                        .iter()
                        .map(|v| Value::from(vec![get(v, "k"), get(v, "i")]))
                        .collect();
                    let sorted = sort(&pairs, &pairs);
                    Value::from(
                        sorted
                            .iter()
                            .map(|p| p.as_array().unwrap().get(1).cloned().unwrap())
                            .collect::<Vec<_>>(),
                    )
                }
                _ => {
                    let keys: Array = arr.iter().map(|v| Value::from(vec![get(v, "k")])).collect();
                    let ids = |a: &Array| -> Value {
                        Value::from(a.iter().map(|v| get(v, "i")).collect::<Vec<_>>())
                    };
                    match prog {
                        "sort_by(.k) | map(.i)" => ids(&sort(arr, &keys)),
                        "unique_by(.k) | map(.i)" => ids(&unique(arr, &keys)),
                        _ => Value::from(
                            group(arr, &keys)
                                .iter()
                                .map(|g| ids(g.as_array().unwrap()))
                                .collect::<Vec<_>>(),
                        ),
                    }
                }
            }
            .to_json();
            if got != line {
                failures += 1;
                if failures <= 5 {
                    eprintln!("{prog} on {doc}\n  got {got}\n  jq  {line}");
                }
            }
        }
        assert_eq!(failures, 0, "{prog}: {failures} of {} differ", docs.len());
    }
    eprintln!(
        "{} NaN-keyed sorts x {} programs match jq",
        docs.len(),
        programs.len()
    );
}

/// Sorting keys that mix literals differing only beyond double precision
/// with native numbers (jv_cmp is not transitive then).
#[test]
#[ignore]
fn live_lossy_literal_sorts() {
    if !jq_available() {
        return;
    }
    let seed = env_u64("QJ_SEED", 0x1055);
    let cases = env_u64("QJ_CASES", 2000);
    let mut rng = Rng(seed | 1);
    let lits = [
        "100000000000000000001",
        "100000000000000000000",
        "99999999999999999999",
        "100000000000000000002",
        "1e20",
        "1.0000000000000000001e20",
        "5",
        "1e400",
        "1e401",
    ];
    let mut docs: Vec<String> = Vec::new();
    for _ in 0..cases {
        let n = rng.below(40);
        let items: Vec<String> = (0..n)
            .map(|i| {
                let k = *rng.pick(&lits);
                let native = rng.chance(40);
                format!("{{\"k\":{k},\"n\":{native},\"i\":{i}}}")
            })
            .collect();
        docs.push(format!("[{}]", items.join(",")));
    }
    let input = docs.join("\n");
    let prog = "sort_by(if .n then .k * 1 else .k end) | map(.i)";
    let (want, err) = run_jq(&["-c", prog], input.as_bytes());
    assert!(
        err.is_empty(),
        "jq failed: {}",
        String::from_utf8_lossy(&err)
    );
    let want = String::from_utf8(want).unwrap();
    let mut failures = 0;
    for (doc, line) in docs.iter().zip(want.lines()) {
        let arr = parse_sized(doc.as_bytes()).unwrap();
        let arr = arr.as_array().unwrap();
        let get = |v: &Value, key: &str| v.as_object().unwrap().get(key).cloned().unwrap();
        let keys: Array = arr
            .iter()
            .map(|v| {
                let k = get(v, "k");
                let k = if get(v, "n").is_truthy() {
                    Value::number(k.as_f64().unwrap() * 1.0)
                } else {
                    k
                };
                Value::from(vec![k])
            })
            .collect();
        let got = Value::from(
            sort(arr, &keys)
                .iter()
                .map(|v| get(v, "i"))
                .collect::<Vec<_>>(),
        )
        .to_json();
        if got != line {
            failures += 1;
            if failures <= 5 {
                eprintln!("{prog} on {doc}\n  got {got}\n  jq  {line}");
            }
        }
    }
    assert_eq!(failures, 0, "{failures} of {} differ", docs.len());
    eprintln!("{} mixed-precision sorts match jq", docs.len());
}
