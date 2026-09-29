//! Random input generator for the differential tests: mostly valid JSON
//! texts with jq's extensions, adversarial bytes and chunk-boundary cases
//! mixed in, split across several in-memory inputs.

use super::Rng;

const WS: [&str; 8] = [" ", "\n", "\t", "\r\n", "  ", "\n\n", " \n ", ""];

pub(crate) fn number(r: &mut Rng) -> String {
    match r.below(24) {
        0 => "0".into(),
        1 => "-0".into(),
        2 => format!("{}", r.below(1000)),
        3 => format!("-{}", r.below(100000)),
        4 => format!("{}.{}", r.below(100), r.below(1000)),
        5 => format!("{}.{}0", r.below(100), r.below(10)),
        6 => format!("{}e{}", r.below(10), r.below(30)),
        7 => format!("{}E-{}", r.below(10), r.below(30)),
        8 => format!("{}.{}e+{}", r.below(10), r.below(100), r.below(400)),
        9 => "100000000000000000001".into(),
        10 => "18446744073709551616".into(),
        11 => "123456789012345678901234567890.5".into(),
        12 => "1e400".into(),
        13 => "-1e-400".into(),
        14 => "01".into(),
        15 => ".5".into(),
        16 => "+1".into(),
        17 => "1.".into(),
        18 => ["nan", "NaN", "-nan", "Infinity", "-Infinity", "inf"][r.below(6)].into(),
        19 => "9007199254740993".into(),
        20 => format!("{}", r.next() as i64),
        21 => "1.000".into(),
        22 => "0.0000001".into(),
        _ => format!("{}", r.next() % 1_000_000_000_000),
    }
}

fn string_body(r: &mut Rng, out: &mut Vec<u8>) {
    let n = r.below(12);
    for _ in 0..n {
        match r.below(40) {
            0 => out.extend_from_slice(b"\\n"),
            1 => out.extend_from_slice(b"\\\""),
            2 => out.extend_from_slice(b"\\\\"),
            3 => out.extend_from_slice(b"\\/"),
            4 => out.extend_from_slice(b"\\u00e9"),
            5 => out.extend_from_slice(b"\\ud83d\\ude00"),
            6 => out.extend_from_slice(b"\\u0000"),
            7 => out.extend_from_slice("é".as_bytes()),
            8 => out.extend_from_slice("😀".as_bytes()),
            9 if r.chance(1, 4) => out.extend_from_slice(b"\\ud800"), // lone high
            10 if r.chance(1, 4) => out.extend_from_slice(b"\\udc00"), // lone low
            11 if r.chance(1, 6) => out.push(0xff),                   // invalid UTF-8
            12 if r.chance(1, 6) => out.extend_from_slice(b"\xe2\x82"), // truncated seq
            13 if r.chance(1, 8) => out.push(b'\t'),                  // raw control
            14 if r.chance(1, 10) => out.extend_from_slice(b"\\x"),   // bad escape
            15 if r.chance(1, 10) => out.push(b'\n'),                 // raw newline
            16 => out.extend_from_slice(b"\\t\\r\\b\\f"),
            17 => out.extend_from_slice(b" spaced out "),
            18 if r.chance(1, 12) => out.push(0),
            19 if r.chance(1, 20) => {
                // long enough to cross fgets chunks
                let len = 3000 + r.below(6000);
                out.extend(std::iter::repeat_n(b'x', len));
            }
            20 => out.extend_from_slice(b"\xc3\xa9\xe2\x82\xac"),
            _ => {
                let len = 1 + r.below(10);
                for _ in 0..len {
                    out.push(b"abcdefghijklmnopqrstuvwxyz0123456789_ -{}[]:,"[r.below(45)]);
                }
            }
        }
    }
}

fn ws(r: &mut Rng, out: &mut Vec<u8>, pretty: bool) {
    if pretty {
        out.extend_from_slice(WS[r.below(WS.len())].as_bytes());
    } else if r.chance(1, 8) {
        out.push(b' ');
    }
}

pub(crate) fn value(r: &mut Rng, out: &mut Vec<u8>, depth: usize, pretty: bool) {
    let k = if depth > 5 { r.below(6) } else { r.below(10) };
    match k {
        0 => out.extend_from_slice(number(r).as_bytes()),
        1 => {
            out.push(b'"');
            string_body(r, out);
            out.push(b'"');
        }
        2 => out.extend_from_slice([&b"true"[..], b"false", b"null"][r.below(3)]),
        3 | 4 | 5 => out.extend_from_slice(number(r).as_bytes()),
        6 | 7 => {
            out.push(b'[');
            let n = r.below(5);
            for i in 0..n {
                if i > 0 {
                    ws(r, out, pretty);
                    out.push(b',');
                }
                ws(r, out, pretty);
                value(r, out, depth + 1, pretty);
            }
            ws(r, out, pretty);
            if r.chance(1, 40) {
                out.push(b','); // trailing comma
            }
            out.push(b']');
        }
        _ => {
            out.push(b'{');
            let n = r.below(5);
            for i in 0..n {
                if i > 0 {
                    ws(r, out, pretty);
                    out.push(b',');
                }
                ws(r, out, pretty);
                out.push(b'"');
                if r.chance(1, 3) {
                    out.push(b"abc"[r.below(3)]); // duplicate keys
                } else {
                    string_body(r, out);
                }
                out.push(b'"');
                ws(r, out, pretty);
                out.push(b':');
                ws(r, out, pretty);
                value(r, out, depth + 1, pretty);
            }
            ws(r, out, pretty);
            out.push(b'}');
        }
    }
}

/// One input stream: texts separated by whitespace (or nothing), with
/// occasional garbage.
pub(crate) fn stream(r: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    match r.below(30) {
        0 => out.extend_from_slice(b"\xEF\xBB\xBF"),
        1 => out.extend_from_slice(b"\xEF\xBB"),
        2 => out.extend_from_slice(b"\xEF\xBB\x41"),
        _ => {}
    }
    let texts = 1 + r.below(12);
    for _ in 0..texts {
        let pretty = r.chance(1, 4);
        match r.below(60) {
            0 => out.push(b']'),
            1 => out.push(b'}'),
            2 => out.push(b','),
            3 => out.push(b':'),
            4 => out.push(0),
            5 => out.push(0x1e), // RS
            6 => out.extend_from_slice(b"truex"),
            7 => out.extend_from_slice(b"'a'"),
            8 => {
                // truncated text
                let mut t = Vec::new();
                value(r, &mut t, 0, pretty);
                let cut = r.below(t.len() + 1);
                out.extend_from_slice(&t[..cut]);
            }
            9 => {
                // deep nesting (beyond simdjson's 1024, sometimes beyond jq's)
                let d = [1030, 5000, 10001][r.below(3)];
                out.extend(std::iter::repeat_n(b'[', d));
                out.extend(std::iter::repeat_n(b']', d));
            }
            10 => {
                // padding to put the next text near a 4095-byte boundary
                let len = 4000 + r.below(200);
                out.extend(std::iter::repeat_n(b' ', len));
            }
            11 => out.extend_from_slice(b"\x00\x00garbage\x00"),
            _ => value(r, &mut out, 0, pretty),
        }
        // separator
        match r.below(12) {
            0 => {}
            1 => out.push(b' '),
            2 | 3 | 4 | 5 | 6 => out.push(b'\n'),
            7 => out.extend_from_slice(b"\r\n"),
            8 => out.push(b'\t'),
            9 => out.extend_from_slice(b"\n\n"),
            10 => out.extend_from_slice(b" \n"),
            _ => out.push(b' '),
        }
    }
    if r.chance(1, 3) && out.last() == Some(&b'\n') {
        out.pop();
    }
    out
}

/// Splits `data` into up to four named inputs at random points, sometimes
/// adding inputs that fail to open or read.
pub(crate) fn inputs(
    r: &mut Rng,
    data: &[u8],
) -> (Vec<String>, Vec<(std::ffi::OsString, super::MemFile)>) {
    let parts = 1 + r.below(4);
    let mut cuts: Vec<usize> = (1..parts).map(|_| r.below(data.len() + 1)).collect();
    cuts.sort_unstable();
    let mut names = Vec::new();
    let mut files = Vec::new();
    let mut prev = 0;
    for (i, &c) in cuts.iter().chain(std::iter::once(&data.len())).enumerate() {
        let name = if parts == 1 && r.chance(1, 2) {
            "-".to_string()
        } else {
            format!("f{i}")
        };
        match r.below(20) {
            0 => {
                names.push(format!("missing{i}"));
            }
            1 => {
                let n = format!("dir{i}");
                files.push((
                    n.clone().into(),
                    super::MemFile::ReadError(Vec::new(), libc::EISDIR),
                ));
                names.push(n);
            }
            2 => {
                let n = format!("empty{i}");
                files.push((n.clone().into(), super::MemFile::Data(Vec::new())));
                names.push(n);
            }
            _ => {}
        }
        files.push((
            name.clone().into(),
            super::MemFile::Data(data[prev..c].to_vec()),
        ));
        names.push(name);
        prev = c;
    }
    (names, files)
}
