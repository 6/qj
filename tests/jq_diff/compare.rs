//! Output representation, stderr normalization and case classification.
//!
//! The only normalization is on stderr: a `qj:` program-name prefix at the
//! start of a line is rewritten to `jq:` (on both sides, so user text that
//! happens to start a line with `qj:` compares equal too). stdout and exit
//! codes are compared exactly.

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

/// Rewrite a `qj:` program-name prefix at the start of any line to `jq:`.
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
        let b = stderr[i];
        out.push(b);
        at_line_start = b == b'\n';
        i += 1;
    }
    out
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Level(Level),
    /// The case can't be judged: jq itself timed out or hit the output or
    /// memory cap.
    Skipped(&'static str),
}

pub fn classify(jq: &Observed, qj: &Observed) -> Verdict {
    match jq.status {
        Status::Timeout => return Verdict::Skipped("jq timeout"),
        Status::OutputLimit => return Verdict::Skipped("jq output limit"),
        Status::MemoryLimit => return Verdict::Skipped("jq memory limit"),
        _ => {}
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
        // A qj crash or hang is a failure, a jq timeout is a skip.
        let crash = obs(Status::Signal(11), "", "");
        assert_eq!(
            classify(&obs(Status::Exit(0), "", ""), &crash),
            Verdict::Level(Level::Fail)
        );
        let hang = obs(Status::Timeout, "", "");
        assert_eq!(
            classify(&hang, &obs(Status::Exit(0), "", "")),
            Verdict::Skipped("jq timeout")
        );
        let hog = obs(Status::MemoryLimit, "", "");
        assert_eq!(
            classify(&obs(Status::Exit(0), "", ""), &hog),
            Verdict::Level(Level::Fail)
        );
        assert_eq!(
            classify(&hog, &obs(Status::Exit(0), "", "")),
            Verdict::Skipped("jq memory limit")
        );
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
