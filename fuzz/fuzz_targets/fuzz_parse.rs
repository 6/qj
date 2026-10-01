//! The FFI boundary: simdjson's DOM parser through `TapeParser`, on arbitrary
//! bytes, with every kernel this CPU runs. One parser per kernel parses the
//! whole input and then each half, so its buffers are reused at other sizes.
//! After every successful parse the whole tape is walked and every string
//! read, so a tape, string-buffer or structural-index pointer that's off
//! crashes here; and every kernel must accept what the active one accepts,
//! with the same tape, strings and structural indexes (their errors may
//! differ: each reports the first it finds).
//!
//! `cargo +nightly fuzz run fuzz_parse -s none -- -max_total_time=120`
//! (`SIMDJSON_FORCE_IMPLEMENTATION` chooses the active kernel; the target
//! refuses to start on one this CPU can't run).

#![no_main]

use libfuzzer_sys::fuzz_target;
use qj::simdjson::{
    Tape, TapeParser, active_implementation, pad_buffer, supported_implementations,
};

const PAYLOAD_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;

/// Reads every word of the tape, following simdjson's tape format, and
/// returns its strings.
fn walk(tape: &Tape<'_>, len: usize) -> Vec<Vec<u8>> {
    let mut strings = Vec::new();
    let words = tape.words;
    assert_eq!(
        words[0] >> 56,
        u64::from(b'r'),
        "the tape starts with its root"
    );
    assert_eq!(
        (words[0] & PAYLOAD_MASK) as usize,
        words.len(),
        "the root's payload is the tape's length"
    );
    assert_eq!(
        words[words.len() - 1],
        u64::from(b'r') << 56,
        "the tape ends with a root word pointing back at the start"
    );
    for &p in tape.structurals {
        assert!(
            (p as usize) <= len,
            "structural index {p} beyond the text ({len})"
        );
    }
    let mut i = 1;
    while i < words.len() - 1 {
        let word = words[i];
        match (word >> 56) as u8 {
            b'"' => {
                // SAFETY: the payload of a string word is its offset in this
                // tape's string buffer.
                let s = unsafe { tape.string((word & PAYLOAD_MASK) as usize) };
                assert!(std::str::from_utf8(s).is_ok(), "simdjson validates UTF-8");
                strings.push(s.to_vec());
                i += 1;
            }
            // 64-bit integers and doubles take the next word too.
            b'l' | b'u' | b'd' => i += 2,
            b'{' | b'[' | b'}' | b']' | b't' | b'f' | b'n' => i += 1,
            tag => panic!("unexpected tape word {tag:#x} at {i}"),
        }
    }
    assert_eq!(
        i,
        words.len() - 1,
        "the last value ends at the closing root"
    );
    strings
}

/// What a parse hands to src/io: the tape, the structural indexes and the
/// strings.
type Parsed = (Vec<u64>, Vec<u32>, Vec<Vec<u8>>);

fuzz_target!(init: qj::io::fuzzing::init_simdjson_kernel(), |data: &[u8]| {
    // The active kernel (what qj parses with), then every other one this
    // CPU runs.
    let active = active_implementation();
    let mut parsers = vec![(
        active.clone(),
        TapeParser::new().expect("simdjson parser allocation"),
    )];
    for name in supported_implementations() {
        if name != active {
            let parser =
                TapeParser::with_implementation(&name).expect("simdjson parser allocation");
            parsers.push((name, parser));
        }
    }
    let mid = data.len() / 2;
    for text in [data, &data[..mid], &data[mid..]] {
        // TapeParser's contract: no UTF-8 BOM at the start (simdjson would
        // skip it, jq doesn't mid-stream).
        if text.starts_with(b"\xEF\xBB\xBF") {
            continue;
        }
        let buf = pad_buffer(text);
        let mut first: Option<Result<Parsed, i32>> = None;
        for (name, parser) in &mut parsers {
            let got = parser.parse(&buf, text.len()).map(|tape| {
                let strings = walk(&tape, text.len());
                (tape.words.to_vec(), tape.structurals.to_vec(), strings)
            });
            let Some(want) = &first else {
                first = Some(got);
                continue;
            };
            let alike = match (want, &got) {
                (Err(_), Err(_)) => true,
                (a, b) => a == b,
            };
            assert!(
                alike,
                "{active} and {name} disagree on {text:?}:\n  {active}: {want:?}\n  {name}: {got:?}"
            );
        }
    }
});
