//! The FFI boundary: simdjson's DOM parser through `TapeParser`, on arbitrary
//! bytes. One parser parses the whole input and then each half, so its
//! buffers are reused at other sizes. After every successful parse the whole
//! tape is walked and every string read, so a tape, string-buffer or
//! structural-index pointer that's off crashes here.
//!
//! `cargo +nightly fuzz run fuzz_parse -s none -- -max_total_time=120`

#![no_main]

use libfuzzer_sys::fuzz_target;
use qj::simdjson::{Tape, TapeParser, pad_buffer};

const PAYLOAD_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;

/// Reads every word of the tape, following simdjson's tape format.
fn walk(tape: &Tape<'_>, len: usize) {
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
}

fuzz_target!(|data: &[u8]| {
    let mut parser = TapeParser::new().expect("simdjson parser allocation");
    let mid = data.len() / 2;
    for text in [data, &data[..mid], &data[mid..]] {
        // TapeParser's contract: no UTF-8 BOM at the start (simdjson would
        // skip it, jq doesn't mid-stream).
        if text.starts_with(b"\xEF\xBB\xBF") {
            continue;
        }
        let buf = pad_buffer(text);
        if let Ok(tape) = parser.parse(&buf, text.len()) {
            walk(&tape, text.len());
        }
    }
});
