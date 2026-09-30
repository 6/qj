//! Output representation, stderr normalization and case classification.
//!
//! The default scoreboard's only normalization is on stderr, for the program
//! name: a `qj:` prefix at the start of a line is rewritten to `jq:`, and the
//! exact line `Use qj --help for help with command-line options,` (the usage
//! hint after option errors) to `Use jq --help ...` (on both sides, so user
//! text that happens to look like these compares equal too). stdout and exit
//! codes are compared exactly. The compat scoreboard (`QJ_JQ_COMPAT=1`, where
//! qj's name is jq's) normalizes nothing at all.

use crate::exec::Status;
use crate::hash;

/// Captured bytes. Outputs too large to keep in the cache are stored as a
/// length + hash (+ a prefix for display); equality is still exact up to a
/// 128-bit hash collision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Blob {
    Bytes(#[serde(with = "bytes_repr")] Vec<u8>),
    Digest {
        len: usize,
        fnv: String,
        #[serde(with = "bytes_repr")]
        head: Vec<u8>,
    },
}

/// Largest output kept verbatim in the cache.
pub const MAX_VERBATIM: usize = 1 << 20;

impl Blob {
    pub fn new(bytes: Vec<u8>, keep_verbatim: bool) -> Blob {
        if keep_verbatim || bytes.len() <= MAX_VERBATIM {
            Blob::Bytes(bytes)
        } else {
            Blob::Digest {
                len: bytes.len(),
                fnv: format!("{:032x}", hash::digest(&bytes)),
                head: bytes[..4096].to_vec(),
            }
        }
    }

    fn len_and_digest(&self) -> (usize, String) {
        match self {
            Blob::Bytes(b) => (b.len(), format!("{:032x}", hash::digest(b))),
            Blob::Digest { len, fnv, .. } => (*len, fnv.clone()),
        }
    }

    pub fn same(&self, other: &Blob) -> bool {
        match (self, other) {
            (Blob::Bytes(a), Blob::Bytes(b)) => a == b,
            _ => self.len_and_digest() == other.len_and_digest(),
        }
    }

    /// Bytes for display (a prefix for digests).
    pub fn shown(&self) -> (&[u8], Option<usize>) {
        match self {
            Blob::Bytes(b) => (b, None),
            Blob::Digest { len, head, .. } => (head, Some(*len)),
        }
    }
}

/// Bytes are stored as a string when they are valid UTF-8, else as base64.
mod bytes_repr {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    enum Repr {
        #[serde(rename = "s")]
        Str(String),
        #[serde(rename = "b64")]
        B64(String),
    }

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        match std::str::from_utf8(v) {
            Ok(t) => Repr::Str(t.to_string()).serialize(s),
            Err(_) => Repr::B64(base64::engine::general_purpose::STANDARD.encode(v)).serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        match Repr::deserialize(d)? {
            Repr::Str(t) => Ok(t.into_bytes()),
            Repr::B64(b) => base64::engine::general_purpose::STANDARD
                .decode(b)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// The first line of jq's usage hint (main.c `die()`), as qj prints it.
const QJ_USAGE_HINT: &[u8] = b"Use qj --help for help with command-line options,";

/// Rewrite a `qj:` program-name prefix at the start of any line to `jq:`, and
/// the whole line `Use qj --help for help with command-line options,` (the
/// usage hint after option errors) to jq's `Use jq --help ...`.
pub fn normalize_stderr(stderr: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(stderr.len());
    let mut at_line_start = true;
    let mut i = 0;
    while i < stderr.len() {
        if at_line_start && stderr[i..].starts_with(b"qj:") {
            out.extend_from_slice(b"jq:");
            i += 3;
            at_line_start = false;
            continue;
        }
        if at_line_start
            && stderr[i..].starts_with(QJ_USAGE_HINT)
            && matches!(stderr.get(i + QJ_USAGE_HINT.len()), None | Some(b'\n'))
        {
            out.extend_from_slice(b"Use jq");
            i += b"Use qj".len();
            at_line_start = false;
            continue;
        }
        let b = stderr[i];
        out.push(b);
        at_line_start = b == b'\n';
        i += 1;
    }
    out
}

/// [`normalize_stderr`] for stdout and stderr merged into one stream (CLI
/// cases with `merge`): there, stdout's buffer is written out in blocks that
/// split lines, so stderr's messages can start mid-line, and the `qj: `
/// prefix is rewritten wherever it appears. User text reaches both tools'
/// streams identically, so rewriting it on both sides can't hide a difference.
pub fn normalize_merged(stream: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(stream.len());
    let mut rest = stream;
    while let Some(i) = memchr::memmem::find(rest, b"qj: ") {
        out.extend_from_slice(&rest[..i]);
        out.extend_from_slice(b"jq: ");
        rest = &rest[i + 4..];
    }
    out.extend_from_slice(rest);
    normalize_stderr(&out)
}

/// One tool's observable behavior for an invocation. `stderr` is normalized.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Observed {
    pub status: Status,
    pub stdout: Blob,
    pub stderr: Blob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// stdout or exit code differ.
    Fail,
    /// stdout and exit code match; normalized stderr differs.
    Stdout,
    /// stdout, exit code and normalized stderr all match.
    Pass,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Fail => "fail",
            Level::Stdout => "stdout",
            Level::Pass => "pass",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "fail" => Some(Level::Fail),
            "stdout" => Some(Level::Stdout),
            "pass" => Some(Level::Pass),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Level(Level),
    /// jq never finished, and neither did qj, in the same way: killed by the
    /// same cap (the timeout, the output cap or the memory cap), with the same
    /// bytes on stdout and stderr up to that point. As strict as a pass, and
    /// recorded as one; the scoreboard counts these in a column of their own.
    Unfinished,
}

impl Verdict {
    /// The level the ratchet records: matching a program jq never finishes
    /// is a pass.
    pub fn level(self) -> Level {
        match self {
            Verdict::Level(l) => l,
            Verdict::Unfinished => Level::Pass,
        }
    }
}

/// Whether the tool was killed because it would not finish: it ran past the
/// timeout, or past the output or memory cap.
fn hit_a_limit(status: &Status) -> bool {
    matches!(
        status,
        Status::Timeout | Status::OutputLimit | Status::MemoryLimit
    )
}

pub fn classify(jq: &Observed, qj: &Observed) -> Verdict {
    // Where jq never finishes (`[1] | delpaths([[nan]])` loops forever,
    // growing an array as it goes), what it did before it was killed is the
    // expectation: `QJ_JQ_COMPAT=1` reproduces the hang, so qj has to end the
    // same way — the same cap, and the same output up to it (exactly the cap's
    // worth of stdout, when that is the cap). An answer, a crash, another cap
    // or other output is a difference like any other.
    if hit_a_limit(&jq.status) {
        return if qj.status == jq.status && jq.stdout.same(&qj.stdout) && jq.stderr.same(&qj.stderr)
        {
            Verdict::Unfinished
        } else {
            Verdict::Level(Level::Fail)
        };
    }
    if jq.status != qj.status || !jq.stdout.same(&qj.stdout) {
        Verdict::Level(Level::Fail)
    } else if jq.stderr.same(&qj.stderr) {
        Verdict::Level(Level::Pass)
    } else {
        Verdict::Level(Level::Stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: Status, out: &str, err: &str) -> Observed {
        Observed {
            status,
            stdout: Blob::new(out.as_bytes().to_vec(), true),
            stderr: Blob::new(normalize_stderr(err.as_bytes()), true),
        }
    }

    #[test]
    fn normalizes_only_line_start_prefix() {
        assert_eq!(
            normalize_stderr(b"qj: error: x\nqj: 1 compile error\n"),
            b"jq: error: x\njq: 1 compile error\n"
        );
        assert_eq!(normalize_stderr(b"jq: error"), b"jq: error");
        // Mid-line mentions are not the program-name prefix.
        assert_eq!(
            normalize_stderr(b"Use qj --help\nsee qj: x"),
            b"Use qj --help\nsee qj: x"
        );
        assert_eq!(normalize_stderr(b" qj: indented"), b" qj: indented");
        assert_eq!(normalize_stderr(b"qjx"), b"qjx");
    }

    #[test]
    fn normalizes_exactly_the_usage_hint_line() {
        assert_eq!(
            normalize_stderr(
                b"qj: Unknown option --foo\nUse qj --help for help with command-line options,\n\
                  or see the jq manpage, or online docs  at https://jqlang.org\n"
            ),
            b"jq: Unknown option --foo\nUse jq --help for help with command-line options,\n\
              or see the jq manpage, or online docs  at https://jqlang.org\n"
        );
        assert_eq!(
            normalize_stderr(b"Use qj --help for help with command-line options,"),
            b"Use jq --help for help with command-line options,"
        );
        // Only that exact line, and only at the start of a line.
        for s in [
            &b"Use qj --help for help with command-line options, x\n"[..],
            b"Use qj --help\n",
            b"x Use qj --help for help with command-line options,\n",
            b"For listing the command options, use qj --help.\n",
        ] {
            assert_eq!(normalize_stderr(s), s);
        }
    }

    #[test]
    fn classification() {
        let jq = obs(Status::Exit(5), "1\n", "jq: error (at <stdin>:0): boom\n");
        let same = obs(Status::Exit(5), "1\n", "qj: error (at <stdin>:0): boom\n");
        assert_eq!(classify(&jq, &same), Verdict::Level(Level::Pass));
        let other_err = obs(Status::Exit(5), "1\n", "qj: error: boom\n");
        assert_eq!(classify(&jq, &other_err), Verdict::Level(Level::Stdout));
        let other_rc = obs(Status::Exit(1), "1\n", "jq: error (at <stdin>:0): boom\n");
        assert_eq!(classify(&jq, &other_rc), Verdict::Level(Level::Fail));
        // Whitespace, number spelling and key order are all significant.
        let ws = obs(Status::Exit(5), "1 \n", "jq: error (at <stdin>:0): boom\n");
        assert_eq!(classify(&jq, &ws), Verdict::Level(Level::Fail));
        let a = obs(Status::Exit(0), "{\"a\":1,\"b\":2}\n", "");
        let b = obs(Status::Exit(0), "{\"b\":2,\"a\":1}\n", "");
        assert_eq!(classify(&a, &b), Verdict::Level(Level::Fail));
        let n1 = obs(Status::Exit(0), "1.0\n", "");
        let n2 = obs(Status::Exit(0), "1\n", "");
        assert_eq!(classify(&n1, &n2), Verdict::Level(Level::Fail));
        // A qj crash or hang where jq finishes is a failure.
        let crash = obs(Status::Signal(11), "", "");
        assert_eq!(
            classify(&obs(Status::Exit(0), "", ""), &crash),
            Verdict::Level(Level::Fail)
        );
        let hang = obs(Status::Timeout, "", "");
        let hog = obs(Status::MemoryLimit, "", "");
        let flood = obs(Status::OutputLimit, "1\n1\n", "");
        assert_eq!(
            classify(&obs(Status::Exit(0), "", ""), &hog),
            Verdict::Level(Level::Fail)
        );
        // The core-dump flag is part of the status.
        let dumped = obs(Status::CoreDumped(11), "", "");
        assert_eq!(classify(&dumped, &crash), Verdict::Level(Level::Fail));
        assert_eq!(
            classify(&dumped, &dumped.clone()),
            Verdict::Level(Level::Pass)
        );
        // Where jq never finishes, qj must not finish either, and in the same
        // way: the same cap, and the same output up to it.
        for jq in [&hang, &hog, &flood] {
            assert_eq!(classify(jq, &jq.clone()), Verdict::Unfinished);
            assert_eq!(classify(jq, &crash), Verdict::Level(Level::Fail));
            assert_eq!(
                classify(jq, &obs(Status::Exit(0), "1\n", "")),
                Verdict::Level(Level::Fail)
            );
            for other in [&hang, &hog, &flood] {
                if other.status != jq.status {
                    assert_eq!(classify(jq, other), Verdict::Level(Level::Fail));
                }
            }
        }
        let flood_err = obs(Status::OutputLimit, "1\n1\n", "qj: x\n");
        assert_eq!(classify(&flood, &flood_err), Verdict::Level(Level::Fail));
        let flood_other = obs(Status::OutputLimit, "1\n2\n", "");
        assert_eq!(classify(&flood, &flood_other), Verdict::Level(Level::Fail));
        assert_eq!(Verdict::Unfinished.level(), Level::Pass);
    }

    #[test]
    fn digests_compare_exactly() {
        let big: Vec<u8> = (0..(MAX_VERBATIM + 10)).map(|i| (i % 251) as u8).collect();
        let d = Blob::new(big.clone(), false);
        assert!(matches!(d, Blob::Digest { .. }));
        assert!(d.same(&Blob::new(big.clone(), true)));
        let mut other = big;
        *other.last_mut().unwrap() ^= 1;
        assert!(!d.same(&Blob::new(other, true)));
    }

    #[test]
    fn blob_serde_roundtrip() {
        for bytes in [b"text".to_vec(), vec![0xff, 0xfe, b'"']] {
            let b = Blob::new(bytes.clone(), true);
            let json = serde_json::to_string(&b).unwrap();
            let back: Blob = serde_json::from_str(&json).unwrap();
            assert!(back.same(&Blob::new(bytes, true)));
        }
    }
}
