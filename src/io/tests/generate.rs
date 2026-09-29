//! Random input generator for the differential tests: JSON texts with
//! jq's extensions, adversarial bytes and chunk-boundary cases mixed in at
//! a per-case rate, split across several in-memory inputs.

use super::Rng;

const WS: [&str; 8] = [" ", "\n", "\t", "\r\n", "  ", "\n\n", " \n ", ""];

/// A generator with a "weirdness" rate: the chance, per 1000, that any
/// given element is something other than plain valid JSON.
pub(crate) struct Gen<'a> {
    pub(crate) r: &'a mut Rng,
    pub(crate) weird: usize,
}

impl Gen<'_> {
    fn odd(&mut self) -> bool {
        self.r.below(1000) < self.weird
    }

    pub(crate) fn number(&mut self) -> String {
        if self.odd() {
            return match self.r.below(12) {
                0 => "100000000000000000001".into(),
                1 => "18446744073709551616".into(),
                2 => "123456789012345678901234567890.5".into(),
                3 => "1e400".into(),
                4 => "-1e-400".into(),
                5 => "01".into(),
                6 => ".5".into(),
                7 => "+1".into(),
                8 => "1.".into(),
                9 => ["nan", "NaN", "-nan", "Infinity", "-Infinity", "inf"][self.r.below(6)].into(),
                10 => "1e".into(),
                _ => "-".into(),
            };
        }
        let r = &mut *self.r;
        match r.below(14) {
            0 => "0".into(),
            1 => "-0".into(),
            2 => format!("{}", r.below(1000)),
            3 => format!("-{}", r.below(100000)),
            4 => format!("{}.{}", r.below(100), r.below(1000)),
            5 => format!("{}.{}0", r.below(100), r.below(10)),
            6 => format!("{}e{}", r.below(10), r.below(30)),
            7 => format!("{}E-{}", r.below(10), r.below(30)),
            8 => format!("{}.{}e+{}", r.below(10), r.below(100), r.below(300)),
            9 => "9007199254740993".into(),
            10 => format!("{}", r.next() as i64),
            11 => "1.000".into(),
            12 => "0.0000001".into(),
            _ => format!("{}", r.next() % 1_000_000_000_000),
        }
    }

    fn string_body(&mut self, out: &mut Vec<u8>) {
        let n = self.r.below(12);
        for _ in 0..n {
            if self.odd() {
                match self.r.below(9) {
                    0 => out.extend_from_slice(b"\\ud800"), // lone high surrogate
                    1 => out.extend_from_slice(b"\\udc00"), // lone low surrogate
                    2 => out.push(0xff),                    // invalid UTF-8
                    3 => out.extend_from_slice(b"\xe2\x82"), // truncated sequence
                    4 => out.push(b'\t'),                   // raw control character
                    5 => out.extend_from_slice(b"\\x"),     // bad escape
                    6 => out.push(b'\n'),                   // raw newline
                    7 => out.push(0),
                    _ => out.extend_from_slice(b"\\ud800\\u0041"), // bad pair
                }
                continue;
            }
            let r = &mut *self.r;
            match r.below(24) {
                0 => out.extend_from_slice(b"\\n"),
                1 => out.extend_from_slice(b"\\\""),
                2 => out.extend_from_slice(b"\\\\"),
                3 => out.extend_from_slice(b"\\/"),
                4 => out.extend_from_slice(b"\\u00e9"),
                5 => out.extend_from_slice(b"\\ud83d\\ude00"),
                6 => out.extend_from_slice(b"\\u0000"),
                7 => out.extend_from_slice("é".as_bytes()),
                8 => out.extend_from_slice("😀".as_bytes()),
                9 => out.extend_from_slice(b"\\t\\r\\b\\f"),
                10 => out.extend_from_slice(b" spaced out "),
                11 if r.chance(1, 20) => {
                    // long enough to cross fgets chunks
                    let len = 3000 + r.below(6000);
                    out.extend(std::iter::repeat_n(b'x', len));
                }
                12 => out.extend_from_slice(b"\xc3\xa9\xe2\x82\xac"),
                13 => out.extend_from_slice(b"\\u20AC\\uFFFF"),
                _ => {
                    let len = 1 + r.below(10);
                    for _ in 0..len {
                        out.push(b"abcdefghijklmnopqrstuvwxyz0123456789_ -{}[]:,"[r.below(45)]);
                    }
                }
            }
        }
    }

    fn ws(&mut self, out: &mut Vec<u8>, pretty: bool) {
        if pretty {
            out.extend_from_slice(WS[self.r.below(WS.len())].as_bytes());
        } else if self.r.chance(1, 8) {
            out.push(b' ');
        }
    }

    pub(crate) fn value(&mut self, out: &mut Vec<u8>, depth: usize, pretty: bool) {
        let k = if depth > 5 {
            self.r.below(6)
        } else {
            self.r.below(10)
        };
        match k {
            0 | 3 | 4 | 5 => out.extend_from_slice(self.number().as_bytes()),
            1 => {
                out.push(b'"');
                self.string_body(out);
                out.push(b'"');
            }
            2 => out.extend_from_slice([&b"true"[..], b"false", b"null"][self.r.below(3)]),
            6 | 7 => {
                out.push(b'[');
                let n = self.r.below(5);
                for i in 0..n {
                    if i > 0 {
                        self.ws(out, pretty);
                        out.push(b',');
                    }
                    self.ws(out, pretty);
                    self.value(out, depth + 1, pretty);
                }
                self.ws(out, pretty);
                if self.odd() {
                    out.push(b','); // trailing comma
                }
                out.push(b']');
            }
            _ => {
                out.push(b'{');
                let n = self.r.below(5);
                for i in 0..n {
                    if i > 0 {
                        self.ws(out, pretty);
                        out.push(b',');
                    }
                    self.ws(out, pretty);
                    out.push(b'"');
                    if self.r.chance(1, 3) {
                        out.push(b"abc"[self.r.below(3)]); // duplicate keys
                    } else {
                        self.string_body(out);
                    }
                    out.push(b'"');
                    self.ws(out, pretty);
                    out.push(b':');
                    self.ws(out, pretty);
                    self.value(out, depth + 1, pretty);
                }
                self.ws(out, pretty);
                out.push(b'}');
            }
        }
    }

    /// Something between texts that isn't a plain text.
    fn oddity(&mut self, out: &mut Vec<u8>, pretty: bool) {
        match self.r.below(12) {
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
                self.value(&mut t, 0, pretty);
                let cut = self.r.below(t.len() + 1);
                out.extend_from_slice(&t[..cut]);
            }
            9 => {
                // deep nesting (beyond simdjson's 1024, sometimes beyond jq's)
                let d = [1030, 5000, 10001][self.r.below(3)];
                out.extend(std::iter::repeat_n(b'[', d));
                out.extend(std::iter::repeat_n(b']', d));
            }
            10 => out.extend_from_slice(b"\x00\x00garbage\x00"),
            _ => out.extend_from_slice(b"\xEF\xBB\xBF"),
        }
    }
}

/// One input stream: texts separated by whitespace (or nothing), with
/// oddities at the case's rate.
pub(crate) fn stream(r: &mut Rng) -> Vec<u8> {
    let weird = [0, 0, 5, 20, 60, 200][r.below(6)];
    let mut g = Gen { r, weird };
    let mut out = Vec::new();
    match g.r.below(30) {
        0 => out.extend_from_slice(b"\xEF\xBB\xBF"),
        1 if weird > 0 => out.extend_from_slice(b"\xEF\xBB"),
        2 if weird > 0 => out.extend_from_slice(b"\xEF\xBB\x41"),
        _ => {}
    }
    let texts = 1 + g.r.below(16);
    for _ in 0..texts {
        let pretty = g.r.chance(1, 4);
        if g.odd() {
            g.oddity(&mut out, pretty);
        } else if g.r.chance(1, 40) {
            // padding to put the next text near a 4095-byte boundary
            let len = 4000 + g.r.below(200);
            out.extend(std::iter::repeat_n(b' ', len));
        } else {
            g.value(&mut out, 0, pretty);
        }
        // separator
        match g.r.below(12) {
            0 => {}
            1 => out.push(b' '),
            2..=6 => out.push(b'\n'),
            7 => out.extend_from_slice(b"\r\n"),
            8 => out.push(b'\t'),
            9 => out.extend_from_slice(b"\n\n"),
            10 => out.extend_from_slice(b" \n"),
            _ => out.push(b' '),
        }
    }
    if g.r.chance(1, 3) && out.last() == Some(&b'\n') {
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
