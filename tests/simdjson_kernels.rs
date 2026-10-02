//! Every simdjson kernel this CPU can run must parse exactly alike.
//!
//! simdjson picks a kernel at run time (on x86-64 icelake, haswell, westmere
//! or fallback; on ARM arm64), and qj's fast path rests on whichever one the
//! machine has. Their stage 1 (structural indexing, UTF-8 validation, string
//! and escape masks across 64-byte blocks) is separate code per kernel, so
//! this parses one corpus with each, through `TapeParser::with_implementation`,
//! and requires the same tape, strings and structural indexes, or an error
//! from each. Not the same error: each kernel reports the first it finds,
//! and they look in different orders (arm64 reports `UTF8_ERROR` where
//! fallback finds a `TAPE_ERROR` first, and the other way around), so the
//! reader may only use the code to choose its path (`is_extent_error`),
//! never for a result; and the reader's differential check runs on each
//! kernel (`the_reader_agrees_on_every_kernel`).
//!
//! CI's x86-64 runners check their kernels here on every change; on arm64
//! only `arm64` runs unless the `simdjson-fallback` feature compiles the
//! fallback kernel in:
//! `cargo test --features simdjson-fallback --test simdjson_kernels`.

use qj::simdjson::{TapeParser, active_implementation, padding, supported_implementations};
use std::io::Write;

const PAYLOAD: u64 = 0x00FF_FFFF_FFFF_FFFF;

/// Everything a parse hands to src/io.
#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    words: Vec<u64>,
    structurals: Vec<u32>,
    strings: Vec<Vec<u8>>,
}

fn snapshot(parser: &mut TapeParser, buf: &[u8], len: usize) -> Result<Parsed, i32> {
    let tape = parser.parse(buf, len)?;
    let mut strings = Vec::new();
    let mut i = 1;
    while i + 1 < tape.words.len() {
        let w = tape.words[i];
        match (w >> 56) as u8 {
            b'"' => {
                // SAFETY: the payload of a string word is its offset in the
                // tape's string buffer.
                strings.push(unsafe { tape.string((w & PAYLOAD) as usize) }.to_vec());
                i += 1;
            }
            b'l' | b'u' | b'd' => i += 2,
            _ => i += 1,
        }
    }
    Ok(Parsed {
        words: tape.words.to_vec(),
        structurals: tape.structurals.to_vec(),
        strings,
    })
}

struct Kernels {
    names: Vec<String>,
    parsers: Vec<TapeParser>,
    /// Reused for the padded copies.
    buf: Vec<u8>,
    texts: usize,
    accepted: usize,
}

impl Kernels {
    fn new() -> Kernels {
        let names = supported_implementations();
        assert!(!names.is_empty(), "no simdjson kernel runs here");
        let parsers = names
            .iter()
            .map(|n| TapeParser::with_implementation(n).unwrap())
            .collect();
        // Straight to the stderr handle, which libtest doesn't capture (as
        // jq_diff's scoreboard), so every run's log says which kernels it
        // checked: CI's runners differ in CPU, and icelake needs AVX-512.
        let _ = writeln!(
            std::io::stderr(),
            "simdjson kernels compared: {} (active: {})",
            names.join(", "),
            active_implementation()
        );
        Kernels {
            names,
            parsers,
            buf: Vec::new(),
            texts: 0,
            accepted: 0,
        }
    }

    /// Parses `text` with every kernel, its padding first zeroed and then
    /// filled with `fill`, and panics where any of them disagrees with the
    /// first (best) kernel, or with itself on other padding (whose contents
    /// must not matter, down to the error code).
    fn check_with(&mut self, text: &[u8], fill: u8) {
        // TapeParser's contract: no UTF-8 BOM at the start.
        if text.starts_with(b"\xEF\xBB\xBF") {
            return;
        }
        self.texts += 1;
        let mut zeroed: Vec<Result<Parsed, i32>> = Vec::with_capacity(self.parsers.len());
        for pad in [0, fill] {
            self.buf.clear();
            self.buf.extend_from_slice(text);
            self.buf.resize(text.len() + padding(), pad);
            for (k, parser) in self.parsers.iter_mut().enumerate() {
                let got = snapshot(parser, &self.buf, text.len());
                let (other, want) = if pad == 0 && k == zeroed.len() {
                    if k == 0 {
                        self.accepted += usize::from(got.is_ok());
                        zeroed.push(got);
                        continue;
                    }
                    // Kernels may report different errors for a text (each
                    // stops at the first it finds, in its own order).
                    (0, &zeroed[0])
                } else {
                    (k, &zeroed[k])
                };
                let alike = match (want, &got) {
                    (Err(a), Err(b)) => other != k || a == b,
                    (a, b) => a == b,
                };
                assert!(
                    alike,
                    "simdjson kernels disagree on {:?}:\n  {} (zeroed padding): {}\n  {} (padding {pad:#04x}): {}",
                    String::from_utf8_lossy(text),
                    self.names[other],
                    brief(want),
                    self.names[k],
                    brief(&got),
                );
                if pad == 0 {
                    zeroed.push(got);
                }
            }
        }
    }

    fn check(&mut self, text: &[u8]) {
        // Padding that looks like the inside of a string, an escape, or
        // invalid UTF-8.
        let fill = [b'"', b'\\', 0xFF, b'{', b' '][self.texts % 5];
        self.check_with(text, fill);
    }
}

fn brief(r: &Result<Parsed, i32>) -> String {
    match r {
        Err(code) => format!("error {code}"),
        Ok(p) => format!(
            "tape {:x?}, structurals {:?}, strings {:?}",
            p.words, p.structurals, p.strings
        ),
    }
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a [u8]]) -> &'a [u8] {
        xs[self.below(xs.len())]
    }
}

/// Pieces of string content: plain, escapes, multibyte UTF-8, and what
/// each kernel's UTF-8 validation and escape tracking must reject alike.
const STRING_PIECES: &[&[u8]] = &[
    b"a",
    b"xyz",
    b" ",
    b"\\\"",
    b"\\\\",
    b"\\/",
    b"\\n",
    b"\\t",
    b"\\u0000",
    b"\\u00e9",
    b"\\uFFFF",
    b"\\ud83d\\ude00",
    "é".as_bytes(),
    "€".as_bytes(),
    "😀".as_bytes(),
    "\u{10FFFF}".as_bytes(),
    b"{[,:]}",
    b"'",
    // Invalid in strings, or as UTF-8.
    b"\\ud800",
    b"\\udc00x",
    b"\\x",
    b"\\u12",
    b"\t",
    b"\n",
    b"\x00",
    b"\x1f",
    b"\x7f",
    b"\x80",
    b"\xC0\xAF",
    b"\xC3",
    b"\xE2\x82",
    b"\xED\xA0\x80",
    b"\xF4\x90\x80\x80",
    b"\xF8\x88\x80\x80\x80",
    b"\xFF",
];

const NUMBERS: &[&[u8]] = &[
    b"0",
    b"-0",
    b"1",
    b"-1",
    b"0.5",
    b"-0.0",
    b"1e2",
    b"1E+2",
    b"1e-2",
    b"123456789",
    b"9007199254740992",
    b"9007199254740993",
    b"9223372036854775807",
    b"9223372036854775808",
    b"-9223372036854775808",
    b"-9223372036854775809",
    b"18446744073709551615",
    b"18446744073709551616",
    b"1.7976931348623157e308",
    b"1.7976931348623159e308",
    b"2.2250738585072014e-308",
    b"4.9e-324",
    b"2e-324",
    b"1e309",
    b"1e-400",
    b"0.1000000000000000055511151231257827",
    b"123456789012345678901234567890",
    b"3.141592653589793238462643383279",
    b"1e0000000000000000000001",
    // Invalid.
    b"01",
    b"-",
    b"1.",
    b".5",
    b"+1",
    b"1e",
    b"1e+",
    b"0x10",
    b"NaN",
    b"-Infinity",
    b"1.5e3.2",
];

fn string(r: &mut Rng, out: &mut Vec<u8>, max_pieces: usize) {
    out.push(b'"');
    for _ in 0..r.below(max_pieces + 1) {
        let p = r.pick(STRING_PIECES);
        // Mostly valid content.
        if STRING_PIECES.iter().position(|q| *q == p).unwrap() < 18 || r.below(8) == 0 {
            out.extend_from_slice(p);
        }
    }
    out.push(b'"');
}

fn value(r: &mut Rng, out: &mut Vec<u8>, depth: usize) {
    const WS: &[&[u8]] = &[b"", b"", b" ", b"\n", b"\t", b"\r\n", b"   "];
    let leaf = depth > 6 || r.below(3) == 0;
    let ws = |r: &mut Rng, out: &mut Vec<u8>| out.extend_from_slice(r.pick(WS));
    match if leaf { r.below(4) } else { 4 + r.below(2) } {
        0 => out.extend_from_slice(r.pick(NUMBERS)),
        1 => string(r, out, 12),
        2 => out.extend_from_slice(r.pick(&[b"true", b"false", b"null", b"nul", b"tru"])),
        3 => string(r, out, 80),
        4 => {
            out.push(b'[');
            for i in 0..r.below(6) {
                if i > 0 {
                    out.push(b',');
                }
                ws(r, out);
                value(r, out, depth + 1);
                ws(r, out);
            }
            out.push(b']');
        }
        _ => {
            out.push(b'{');
            for i in 0..r.below(6) {
                if i > 0 {
                    out.push(b',');
                }
                ws(r, out);
                string(r, out, 6);
                ws(r, out);
                out.push(b':');
                ws(r, out);
                value(r, out, depth + 1);
            }
            out.push(b'}');
        }
    }
}

/// `text`, then versions of it with one byte flipped, removed or
/// inserted, and every prefix ending in its last 70 bytes.
fn mutations(r: &mut Rng, text: &[u8], k: &mut Kernels) {
    k.check(text);
    if text.is_empty() {
        return;
    }
    const BYTES: &[u8] = b"\"\\{}[],: \n\x00\x80\xFFa0eu";
    for _ in 0..4 {
        let i = r.below(text.len());
        let b = BYTES[r.below(BYTES.len())];
        let mut t = text.to_vec();
        match r.below(3) {
            0 => t[i] = b,
            1 => {
                t.remove(i);
            }
            _ => t.insert(i, b),
        }
        k.check(&t);
    }
    for end in text.len().saturating_sub(70)..text.len() {
        k.check(&text[..end]);
    }
}

#[test]
fn every_kernel_parses_alike() {
    let mut k = Kernels::new();
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);

    // Fixed cases.
    for text in [
        &b""[..],
        b" ",
        b"\n\t\r ",
        b"0",
        b"\"\"",
        b"[]",
        b"{}",
        b"[1,2]",
        b"{\"a\":1}",
        b"[1] [2]",
        b"[1,]",
        b"{\"a\" 1}",
        b"{\"a\":}",
        b"{1:2}",
        b"]",
        b"[}",
        b"\"\\",
        b"\"\\\"",
        b"\"",
        b"tru",
        b"truex",
        b"nulll",
        b"true false",
        b"[true,false,null]",
        b"[\"a\"\"b\"]",
        b"\x00",
        b"[1]\x00",
        b"\xEF\xBB\xBF[]",
    ] {
        mutations(&mut r, text, &mut k);
    }
    for n in NUMBERS {
        k.check(n);
        k.check(&[b"[", *n, b"]"].concat());
        k.check(&[b"{\"n\":", *n, b"}"].concat());
    }
    for p in STRING_PIECES {
        k.check(&[b"\"", *p, b"\""].concat());
        k.check(&[b"[\"", *p, b"\",1]"].concat());
    }

    // Nesting around simdjson's limit (1024).
    for depth in [1, 2, 63, 64, 65, 1023, 1024, 1025, 1100] {
        let arrays = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        k.check(arrays.as_bytes());
        k.check(&arrays.as_bytes()[..arrays.len() - 1]);
        let objects = format!("{}0{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        k.check(objects.as_bytes());
        let mixed = format!(
            "{}1{}",
            "[{\"k\":".repeat(depth / 2),
            "}]".repeat(depth / 2)
        );
        k.check(mixed.as_bytes());
    }

    // Texts whose length, or whose significant bytes, sit at every offset
    // around simdjson's 64-byte blocks (and its 128-byte stage 1 steps).
    let specials: &[&[u8]] = &[
        b"\"",
        b"\\\"",
        b"\\\\",
        b"\\\\\\\"",
        b"\\u00e9",
        b"\\ud83d\\ude00",
        "é".as_bytes(),
        "😀".as_bytes(),
        b"\xF0\x9F\x98",
        b"\xED\xA0\x80",
        b"\xC0\x80",
        b"\x80",
        b"\xFF",
        b"\x01",
        b"\n",
    ];
    for s in specials {
        for at in 0..200 {
            // A string with the special piece at byte `at`.
            let mut t = b"\"".to_vec();
            t.extend(std::iter::repeat_n(b'x', at));
            t.extend_from_slice(s);
            t.extend_from_slice(b"yy\"");
            k.check(&t);
            // In an array, after `at` bytes of whitespace, and unclosed.
            let mut t = vec![b' '; at];
            t.extend_from_slice(b"[\"");
            t.extend_from_slice(s);
            t.extend_from_slice(b"\",1]");
            k.check(&t);
            k.check(&t[..t.len() - 2]);
        }
    }
    // Runs of backslashes ending at every offset: whether the quote after
    // them is escaped carries from one block to the next.
    for run in 1..70 {
        for at in [0, 1, 30, 62, 63, 64, 65, 126, 127, 128] {
            let mut t = b"[\"".to_vec();
            t.extend(std::iter::repeat_n(b'a', at));
            t.extend(std::iter::repeat_n(b'\\', run));
            t.extend_from_slice(b"\",\"z\"]");
            k.check(&t);
        }
    }
    // Lengths 0..300 of the same shapes.
    for len in 0..300 {
        let mut t = b"\"".to_vec();
        t.extend(std::iter::repeat_n(b'q', len));
        t.push(b'"');
        k.check(&t);
        k.check(&t[..t.len() - 1]);
        let mut t = b"[".to_vec();
        t.extend(std::iter::repeat_n(b' ', len));
        t.extend_from_slice(b"1]");
        k.check(&t);
        let digits: Vec<u8> = (0..len).map(|i| b'1' + (i % 9) as u8).collect();
        k.check(&digits);
        let mut t = b"0.".to_vec();
        t.extend_from_slice(&digits);
        t.extend_from_slice(b"e-5");
        k.check(&t);
    }

    // Long strings, of every kind of content.
    for p in STRING_PIECES {
        for n in [100, 1000, 70_000] {
            let mut t = b"\"".to_vec();
            while t.len() < n {
                t.extend_from_slice(p);
            }
            t.push(b'"');
            k.check(&t);
        }
    }

    // Generated documents, and mutations of them.
    for i in 0..3000 {
        let mut t = Vec::new();
        value(&mut r, &mut t, if i % 4 == 0 { 0 } else { 3 });
        if i % 2 == 0 {
            mutations(&mut r, &t, &mut k);
        } else {
            k.check(&t);
        }
    }

    // The jq_diff corpus and jq's own test suites: every line, and each
    // data file whole (programs, inputs and expected outputs: most aren't
    // JSON, so they test errors too).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat");
    let mut files = Vec::new();
    for dir in [root.clone(), root.join("corpus"), root.join("corpus/data")] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "test" | "json" | "ndjson") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(files.len() > 20, "{files:?}");
    for path in &files {
        let data = std::fs::read(path).unwrap();
        if path.extension().is_some_and(|e| e != "test") {
            k.check(&data);
        }
        for line in data.split(|&b| b == b'\n') {
            k.check(line);
        }
    }

    eprintln!(
        "{} texts, {} accepted, alike on {}",
        k.texts,
        k.accepted,
        k.names.join(", ")
    );
    assert!(
        k.accepted > 10_000,
        "{} of {} accepted",
        k.accepted,
        k.texts
    );
}

/// Set in the child processes of [`the_reader_agrees_on_every_kernel`].
const CHILD: &str = "QJ_KERNEL_TEST_CHILD";

/// The input layer on every kernel: since the kernels report different
/// errors for the same text, and the reader picks its path by the error
/// (`is_extent_error`), its fast path must give jq's results whichever
/// kernel parses (`qj::io::fuzzing::check_reader_equivalence`: values,
/// errors, file names and line numbers, whole, streamed and through the
/// parallel engine, against jq's input loop). The kernel is process-wide
/// (`SIMDJSON_FORCE_IMPLEMENTATION`), so this test runs itself once per
/// kernel in a child process.
#[test]
fn the_reader_agrees_on_every_kernel() {
    if std::env::var_os(CHILD).is_some() {
        let forced = std::env::var("SIMDJSON_FORCE_IMPLEMENTATION").unwrap();
        assert_eq!(qj::simdjson::checked_active_implementation(), Ok(forced));
        reader_corpus();
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = supported_implementations()
        .into_iter()
        .map(|kernel| {
            let child = std::process::Command::new(&exe)
                .args([
                    "the_reader_agrees_on_every_kernel",
                    "--exact",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("SIMDJSON_FORCE_IMPLEMENTATION", &kernel)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            (kernel, child)
        })
        .collect();
    for (kernel, child) in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "the reader on simdjson's {kernel} kernel: {}\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
    }
}

/// Inputs for `check_reader_equivalence`: three bytes choosing the reader's
/// options and how the rest splits into files, then texts one after another
/// (NDJSON or not, valid or not).
fn reader_corpus() {
    use qj::io::fuzzing::check_reader_equivalence;
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    const SEPS: &[&[u8]] = &[b"\n", b"\n", b" ", b"", b"\r\n", b"\n\n", b"\t"];
    const DAMAGE: &[u8] = b"\"\\{}[],: \x00\x80\xFF";
    let mut inputs = 0;
    for i in 0..1500 {
        let mut data = vec![(i % 256) as u8, r.next() as u8, r.next() as u8];
        for _ in 0..1 + r.below(6) {
            let mut t = Vec::new();
            let depth = if r.below(3) == 0 { 0 } else { 3 };
            value(&mut r, &mut t, depth);
            if r.below(4) == 0 {
                let at = r.below(t.len());
                t[at] = DAMAGE[r.below(DAMAGE.len())];
            }
            data.extend_from_slice(&t);
            data.extend_from_slice(r.pick(SEPS));
        }
        check_reader_equivalence(&data);
        inputs += 1;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/corpus/data");
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read(entry.unwrap().path()).unwrap();
        if text.len() > 4096 {
            continue;
        }
        for cfg in 0..24u8 {
            let mut data = vec![cfg, 85, 170];
            data.extend_from_slice(&text);
            check_reader_equivalence(&data);
            inputs += 1;
        }
    }
    eprintln!(
        "the reader agrees with jq's input loop on {inputs} inputs, on simdjson's {}",
        active_implementation()
    );
}
