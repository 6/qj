//! The UTF-8 helpers from jq's `jv_unicode.c` that the platform primitives need.
//!
//! Strings coming back from C (Oniguruma match slices and error messages, strftime and
//! strptime output) can be invalid UTF-8. jq turns them into strings with
//! `jv_string_sized`, which replaces bad sequences with U+FFFD using its own decoder.
//! That differs from `String::from_utf8_lossy` for some inputs (jq replaces a whole
//! overlong or surrogate sequence with a single U+FFFD, for example), so it is ported
//! here rather than approximated.

/// Port of `jv_utf8_tables.h`'s `utf8_coding_length`: 1..=4 for a lead byte, 0 for an
/// invalid byte, and `CONT` for a continuation byte.
const CONT: u8 = 255;

fn coding_length(b: u8) -> u8 {
    match b {
        0x00..=0x7F => 1,
        0x80..=0xBF => CONT,
        0xC0..=0xC1 => 0,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        0xF5..=0xFF => 0,
    }
}

/// Port of `jv_utf8_tables.h`'s `utf8_coding_bits`.
fn coding_bits(b: u8) -> u32 {
    match b {
        0x00..=0x7F => 0x7F,
        0x80..=0xBF => 0x3F,
        0xC0..=0xC1 => 0x00,
        0xC2..=0xDF => 0x1F,
        0xE0..=0xEF => 0x0F,
        0xF0..=0xF4 => 0x07,
        0xF5..=0xFF => 0x00,
    }
}

/// Port of `jv_utf8_tables.h`'s `utf8_first_codepoint`.
const FIRST_CODEPOINT: [i32; 5] = [0, 0x0, 0x80, 0x800, 0x10000];

/// Port of `jv_unicode.c: jvp_utf8_next`. Decodes one sequence at the start of `bytes`
/// (non-empty) and returns `(codepoint or -1 if invalid, length consumed)`.
fn utf8_next(bytes: &[u8]) -> (i32, usize) {
    let first = bytes[0];
    let mut length = coding_length(first) as usize;
    let mut codepoint: i32 = -1;
    if first & 0x80 == 0 {
        codepoint = first as i32;
        length = 1;
    } else if length == 0 || length == CONT as usize {
        // Bad single byte: an invalid byte or an out-of-place continuation byte.
        length = 1;
    } else if length > bytes.len() {
        // String ends before the sequence does.
        length = bytes.len();
    } else {
        let mut cp = (first as u32 & coding_bits(first)) as i32;
        for (i, &ch) in bytes.iter().enumerate().take(length).skip(1) {
            if coding_length(ch) != CONT {
                // Not followed by enough continuation bytes.
                cp = -1;
                length = i;
                break;
            }
            cp = (cp << 6) | (ch & 0x3F) as i32;
        }
        codepoint = cp;
        if codepoint < FIRST_CODEPOINT[length] {
            codepoint = -1; // overlong
        }
        if (0xD800..=0xDFFF).contains(&codepoint) {
            codepoint = -1; // surrogate
        }
        if codepoint > 0x10FFFF {
            codepoint = -1; // outside Unicode
        }
    }
    (codepoint, length)
}

/// Port of `jv.c: jv_string_sized`: the bytes as a string if they are valid UTF-8,
/// otherwise with each bad sequence replaced by U+FFFD (`jvp_string_copy_replace_bad`).
pub(crate) fn string_sized(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    let mut i = 0;
    while i < bytes.len() {
        let (cp, len) = utf8_next(&bytes[i..]);
        match char::from_u32(cp as u32) {
            Some(c) if cp >= 0 => out.push(c),
            _ => out.push('\u{FFFD}'),
        }
        i += len;
    }
    out
}

/// Port of `jv_unicode.c: jvp_utf8_decode_length`, which `f_match` uses to count
/// codepoints. It assumes `b` starts a valid sequence.
pub(crate) fn decode_length(b: u8) -> usize {
    if b & 0x80 == 0 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_unchanged() {
        assert_eq!(string_sized(b"abc"), "abc");
        assert_eq!(string_sized("é日😀".as_bytes()), "é日😀");
        assert_eq!(string_sized(b"a\0b"), "a\0b");
    }

    #[test]
    fn replacement_follows_jq_not_std() {
        // Lone continuation bytes: one U+FFFD each (same as std).
        assert_eq!(string_sized(b"\xA9\xA9x"), "\u{FFFD}\u{FFFD}x");
        // Truncated sequence at the end: one U+FFFD for the whole prefix.
        assert_eq!(string_sized(b"x\xE6\x97"), "x\u{FFFD}");
        // Overlong encoding of NUL: jq consumes the whole sequence as one U+FFFD,
        // std produces two.
        assert_eq!(string_sized(b"\xC0\x80"), "\u{FFFD}\u{FFFD}");
        assert_eq!(string_sized(b"\xE0\x80\x80"), "\u{FFFD}");
        assert_eq!(
            String::from_utf8_lossy(b"\xE0\x80\x80"),
            "\u{FFFD}\u{FFFD}\u{FFFD}"
        );
        // Encoded surrogate: one U+FFFD in jq.
        assert_eq!(string_sized(b"\xED\xA0\x80z"), "\u{FFFD}z");
        // Above U+10FFFF: F4 90 80 80.
        assert_eq!(string_sized(b"\xF4\x90\x80\x80"), "\u{FFFD}");
        // Lead byte not followed by enough continuation bytes: the lead and the valid
        // continuations become one U+FFFD, the next byte starts fresh.
        assert_eq!(string_sized(b"\xE6\x97x"), "\u{FFFD}x");
        // Latin-1 "ao\xFBt" as produced by strftime in an ISO8859-1 locale.
        assert_eq!(string_sized(b"ao\xFBt"), "ao\u{FFFD}t");
    }

    #[test]
    fn decode_length_matches_jq() {
        assert_eq!(decode_length(b'a'), 1);
        assert_eq!(decode_length(0xC3), 2);
        assert_eq!(decode_length(0xE6), 3);
        assert_eq!(decode_length(0xF0), 4);
        // jq returns 4 for continuation bytes (it assumes a lead byte).
        assert_eq!(decode_length(0xA9), 4);
    }
}
