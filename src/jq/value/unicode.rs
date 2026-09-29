//! Port of jq 1.8.1 `jv_unicode.c` (and `jv_utf8_tables.h`).
//!
//! jq's UTF-8 decoder differs from Rust's `String::from_utf8_lossy` in how many
//! bytes a single U+FFFD replaces: jq consumes a whole overlong or surrogate
//! sequence as one bad codepoint, and a truncated sequence at the end of the
//! buffer swallows every remaining byte (even ASCII ones). Everything that
//! turns bytes into a jq string must go through [`decode_lossy`] to match.

/// Marker in [`UTF8_CODING_LENGTH`] for continuation bytes (`0x80..=0xBF`).
pub const UTF8_CONTINUATION_BYTE: u8 = 255;

/// `utf8_coding_length`: sequence length announced by a lead byte, 0 for bytes
/// that can never start a sequence (`0xC0`, `0xC1`, `0xF5..=0xFF`).
pub static UTF8_CODING_LENGTH: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = match i {
            0x00..=0x7F => 1,
            0x80..=0xBF => UTF8_CONTINUATION_BYTE,
            0xC0 | 0xC1 => 0,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 0,
        };
        i += 1;
    }
    t
};

/// `utf8_coding_bits`: payload mask for a lead byte.
static UTF8_CODING_BITS: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = match i {
            0x00..=0x7F => 0x7F,
            0x80..=0xBF => 0x3F,
            0xC0 | 0xC1 => 0,
            0xC2..=0xDF => 0x1F,
            0xE0..=0xEF => 0x0F,
            0xF0..=0xF4 => 0x07,
            _ => 0,
        };
        i += 1;
    }
    t
};

/// `utf8_first_codepoint`: smallest codepoint that needs a sequence of that length.
const UTF8_FIRST_CODEPOINT: [i32; 5] = [0x00, 0x00, 0x80, 0x800, 0x10000];

/// Port of `jvp_utf8_next`. Decodes one codepoint from `input[pos..]`.
///
/// Returns `None` at the end of input, otherwise `(codepoint, next_pos)` where
/// `codepoint` is -1 for an invalid sequence (which jq replaces with a single
/// U+FFFD).
#[inline]
pub fn utf8_next(input: &[u8], pos: usize) -> Option<(i32, usize)> {
    let end = input.len();
    if pos >= end {
        return None;
    }
    let first = input[pos];
    if first & 0x80 == 0 {
        // Fast path for ASCII
        return Some((first as i32, pos + 1));
    }
    let mut codepoint: i32 = -1;
    let mut length = UTF8_CODING_LENGTH[first as usize] as usize;
    if length == 0 || length == UTF8_CONTINUATION_BYTE as usize {
        // Bad single byte - either an invalid byte or an out-of-place continuation byte
        length = 1;
    } else if pos + length > end {
        // String ends before UTF8 sequence ends
        length = end - pos;
    } else {
        codepoint = (first & UTF8_CODING_BITS[first as usize]) as i32;
        for i in 1..length {
            let ch = input[pos + i];
            if UTF8_CODING_LENGTH[ch as usize] != UTF8_CONTINUATION_BYTE {
                // Invalid UTF8 sequence - not followed by the right number of continuation bytes
                codepoint = -1;
                length = i;
                break;
            }
            codepoint = (codepoint << 6) | (ch & 0x3f) as i32;
        }
        if codepoint < UTF8_FIRST_CODEPOINT[length] {
            // Overlong UTF8 sequence
            codepoint = -1;
        }
        if (0xD800..=0xDFFF).contains(&codepoint) {
            // Surrogate codepoints can't be encoded in UTF8
            codepoint = -1;
        }
        if codepoint > 0x10FFFF {
            // Outside Unicode range
            codepoint = -1;
        }
    }
    debug_assert!(length > 0);
    Some((codepoint, pos + length))
}

/// Port of `jvp_utf8_is_valid`.
pub fn utf8_is_valid(input: &[u8]) -> bool {
    // jq's decoder rejects exactly what the standard library rejects
    // (overlongs, surrogates, > U+10FFFF, truncation), so this is equivalent.
    std::str::from_utf8(input).is_ok()
}

/// Port of `jvp_utf8_decode_length`: length of the sequence starting with a
/// (valid) lead byte.
#[inline]
pub fn utf8_decode_length(startchar: u8) -> usize {
    if startchar & 0x80 == 0 {
        1
    } else if startchar & 0xE0 == 0xC0 {
        2
    } else if startchar & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

/// Port of `jvp_utf8_encode_length`.
#[inline]
pub fn utf8_encode_length(codepoint: u32) -> usize {
    if codepoint <= 0x7F {
        1
    } else if codepoint <= 0x7FF {
        2
    } else if codepoint <= 0xFFFF {
        3
    } else {
        4
    }
}

/// Port of `jvp_utf8_encode`. Unlike `char::encode_utf8` this also encodes
/// surrogate codepoints (as the invalid 3-byte sequence jq produces for a
/// lone `\uDC00` escape); callers then run the bytes through [`decode_lossy`].
#[inline]
pub fn utf8_encode(codepoint: u32, out: &mut Vec<u8>) {
    debug_assert!(codepoint <= 0x10FFFF);
    if codepoint <= 0x7F {
        out.push(codepoint as u8);
    } else if codepoint <= 0x7FF {
        out.push(0xC0 + ((codepoint & 0x7C0) >> 6) as u8);
        out.push(0x80 + (codepoint & 0x03F) as u8);
    } else if codepoint <= 0xFFFF {
        out.push(0xE0 + ((codepoint & 0xF000) >> 12) as u8);
        out.push(0x80 + ((codepoint & 0x0FC0) >> 6) as u8);
        out.push(0x80 + (codepoint & 0x003F) as u8);
    } else {
        out.push(0xF0 + ((codepoint & 0x1C0000) >> 18) as u8);
        out.push(0x80 + ((codepoint & 0x03F000) >> 12) as u8);
        out.push(0x80 + ((codepoint & 0x000FC0) >> 6) as u8);
        out.push(0x80 + (codepoint & 0x00003F) as u8);
    }
}

/// Port of `jvp_utf8_backtrack`: returns the index of the first byte of the
/// codepoint containing `input[start]`, assuming `start` is the last byte of
/// the string and `min` its first. Returns `None` on an invalid byte or when no
/// lead byte is found. `missing_bytes` receives the number of bytes the last
/// codepoint still lacks.
pub fn utf8_backtrack(
    input: &[u8],
    start: usize,
    min: usize,
    missing_bytes: Option<&mut usize>,
) -> Option<usize> {
    debug_assert!(min <= start);
    if min == start {
        return Some(min);
    }
    let mut start = start as isize;
    let mut length: usize = 0;
    let mut seen: usize = 1;
    while start >= min as isize {
        length = UTF8_CODING_LENGTH[input[start as usize] as usize] as usize;
        if length != UTF8_CONTINUATION_BYTE as usize {
            break;
        }
        start -= 1;
        seen += 1;
    }
    if length == 0 || length == UTF8_CONTINUATION_BYTE as usize || length < seen {
        return None;
    }
    if let Some(m) = missing_bytes {
        *m = length - seen;
    }
    Some(start as usize)
}

/// Port of `jvp_string_copy_replace_bad` (and so of `jv_string_sized`):
/// converts bytes to a string, replacing each bad sequence (as delimited by
/// [`utf8_next`]) with one U+FFFD.
pub fn decode_lossy(input: &[u8]) -> String {
    match std::str::from_utf8(input) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            let mut out = String::with_capacity(input.len() + 8);
            push_lossy(&mut out, input);
            out
        }
    }
}

/// Appends `input` to `out` with jq's U+FFFD replacement (`jv_string_append_buf`).
pub fn push_lossy(out: &mut String, input: &[u8]) {
    if let Ok(s) = std::str::from_utf8(input) {
        out.push_str(s);
        return;
    }
    let mut pos = 0;
    // Copy maximal valid runs at once; decode the bad spots with jq's rules.
    while pos < input.len() {
        match std::str::from_utf8(&input[pos..]) {
            Ok(s) => {
                out.push_str(s);
                return;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // SAFETY-free: from_utf8 guarantees the prefix is valid.
                out.push_str(std::str::from_utf8(&input[pos..pos + valid]).unwrap());
                pos += valid;
                // jq decides how many bytes the bad sequence spans.
                let (c, next) = utf8_next(input, pos).expect("not at end");
                debug_assert_eq!(c, -1);
                out.push('\u{FFFD}');
                pos = next;
            }
        }
    }
}

/// Port of `jvp_codepoint_is_whitespace` (characters with the Unicode
/// White_Space property).
pub fn codepoint_is_whitespace(c: i32) -> bool {
    (0x0009..=0x000D).contains(&c)
        || c == 0x0020
        || c == 0x0085
        || c == 0x00A0
        || c == 0x1680
        || (0x2000..=0x200A).contains(&c)
        || c == 0x2028
        || c == 0x2029
        || c == 0x202F
        || c == 0x205F
        || c == 0x3000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lossy(b: &[u8]) -> String {
        decode_lossy(b)
    }

    #[test]
    fn replacement_granularity_matches_jq() {
        // Truncated sequence at the end swallows the following ASCII byte:
        // `printf '"\xe2a"' | jq .` => "�"
        assert_eq!(lossy(b"\xe2a"), "\u{FFFD}");
        // but not when the sequence fits: `printf '"\xe2ab"' | jq .` => "�ab"
        assert_eq!(lossy(b"\xe2ab"), "\u{FFFD}ab");
        // Overlong encodings are one replacement for the whole sequence...
        assert_eq!(lossy(b"\xe0\x80\x80"), "\u{FFFD}");
        assert_eq!(lossy(b"\xf0\x80\x80\x80"), "\u{FFFD}");
        // ...but C0/C1 can never start a sequence: one replacement per byte.
        assert_eq!(lossy(b"\xc0\x80"), "\u{FFFD}\u{FFFD}");
        // A sequence cut short by a non-continuation byte keeps that byte.
        assert_eq!(lossy(b"\xf0\x9f\x98A"), "\u{FFFD}A");
        // Encoded surrogates are one replacement.
        assert_eq!(lossy(b"\xed\xa0\x80"), "\u{FFFD}");
        // Lone continuation bytes are one replacement each.
        assert_eq!(lossy(b"\x80\x80"), "\u{FFFD}\u{FFFD}");
        // Beyond U+10FFFF.
        assert_eq!(lossy(b"\xf4\x90\x80\x80"), "\u{FFFD}");
        assert_eq!(lossy(b"\xf5\x80"), "\u{FFFD}\u{FFFD}");
        assert_eq!(lossy(b"a\xffb"), "a\u{FFFD}b");
        assert_eq!(lossy("h\u{e9}llo".as_bytes()), "h\u{e9}llo");
    }

    #[test]
    fn backtrack() {
        let s = "ab\u{20AC}c".as_bytes(); // a b E2 82 AC c
        assert_eq!(utf8_backtrack(s, 4, 0, None), Some(2));
        assert_eq!(utf8_backtrack(s, 3, 0, None), Some(2));
        assert_eq!(utf8_backtrack(s, 2, 0, None), Some(2));
        assert_eq!(utf8_backtrack(s, 1, 0, None), Some(1));
        let mut missing = 9;
        assert_eq!(utf8_backtrack(s, 3, 0, Some(&mut missing)), Some(2));
        assert_eq!(missing, 1);
    }
}
