//! simdjson → [`Value`]: builds jq values straight from simdjson's DOM tape.
//!
//! [`SimdParser::parse`] accepts exactly one strict JSON (RFC 8259) document.
//! For such a document jq 1.8.1's parser (`jv_parse.c`, ported as
//! [`crate::jq::value::Parser`]) produces a fully determined value, and this
//! module produces the same one:
//!
//! * objects keep jq's duplicate-key rule (the last value wins, at the first
//!   key's position) because both insert with [`Object::insert`];
//! * number literals are read from the source text at their stage-1
//!   structural index and go through [`Number::from_literal`], the function
//!   jq's parser port uses, so the literal is preserved exactly (`1.50`,
//!   `1E+2`, `100000000000000000001`);
//! * strings come from simdjson's unescaped string buffer. simdjson accepts
//!   only valid UTF-8, and rejects lone surrogate escapes and unescaped
//!   control characters, so for any string it accepts, jq's decoder
//!   (including its U+FFFD repair) is the identity.
//!
//! Anything simdjson rejects is reported as [`Rejected`]: invalid UTF-8,
//! lone surrogates, `nan`/`Infinity`, leading zeros, trailing commas,
//! numbers outside double range, nesting deeper than 1024, and every
//! malformed text. The caller then uses jq's parser, which is the authority
//! for values and error messages. This module never produces an error of its
//! own.

use crate::jq::value::{Array, Number, Object, Str, Value};
use crate::simdjson::{Tape, TapeParser, padding};

/// Why [`SimdParser::parse`] declined a text: simdjson's error code, or
/// [`Rejected::UNSUPPORTED`] when simdjson accepted it but the value can't
/// be built here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejected(pub i32);

impl Rejected {
    /// Accepted by simdjson, but not convertible here (defensive; not
    /// expected to happen).
    pub const UNSUPPORTED: Rejected = Rejected(-2);
}

/// Documents larger than this are parsed with a fresh simdjson parser, so a
/// single huge document doesn't pin its buffers (about 14 bytes per input
/// byte) for the rest of the run.
const KEEP_CAPACITY: usize = 64 << 20;

/// A reusable simdjson parser that builds jq values (one per thread).
pub struct SimdParser {
    parser: TapeParser,
    /// Padded copy of the text when the caller's buffer lacks padding.
    scratch: Vec<u8>,
    /// Open containers while building (empty between calls).
    stack: Vec<Frame>,
    keys: KeyCache,
}

/// Recently seen object keys, shared between the objects and documents a
/// parser builds (NDJSON repeats the same keys in every record). Sharing a
/// key's allocation isn't observable in jq: keys are never mutated in
/// place (copy on write), and identity only matters for values at paths
/// (`path_intact` in jq's execute.c), which keys never are.
struct KeyCache {
    slots: Box<[Option<(u64, Str)>]>,
}

const KEY_SLOTS: usize = 1024;
const MAX_CACHED_KEY: usize = 48;

impl KeyCache {
    fn new() -> KeyCache {
        KeyCache {
            slots: vec![None; KEY_SLOTS].into_boxed_slice(),
        }
    }

    #[inline]
    fn get(&mut self, key: &str) -> Str {
        if key.len() > MAX_CACHED_KEY {
            return Str::from(key);
        }
        use std::hash::BuildHasher;
        let h = foldhash::fast::FixedState::with_seed(0x51_7c_c1_b7).hash_one(key.as_bytes());
        let slot = &mut self.slots[h as usize & (KEY_SLOTS - 1)];
        if let Some((sh, s)) = slot
            && *sh == h
            && s.as_bytes() == key.as_bytes()
        {
            return s.clone();
        }
        let s = Str::from(key);
        *slot = Some((h, s.clone()));
        s
    }
}

impl Default for SimdParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SimdParser {
    pub fn new() -> SimdParser {
        SimdParser {
            parser: TapeParser::new().expect("simdjson parser allocation"),
            scratch: Vec::new(),
            stack: Vec::new(),
            keys: KeyCache::new(),
        }
    }

    /// Parses `buf[start..end]` as exactly one strict JSON document and
    /// builds its jq value.
    ///
    /// Bytes of `buf` after `end` are used as simdjson's read padding when
    /// there are enough of them (their contents don't matter); otherwise the
    /// text is copied. The text must not start with a UTF-8 BOM.
    pub fn parse(&mut self, buf: &[u8], start: usize, end: usize) -> Result<Value, Rejected> {
        let len = end - start;
        if len == 0 {
            return Err(Rejected(13)); // simdjson EMPTY
        }
        if len > KEEP_CAPACITY || self.parser.capacity() > KEEP_CAPACITY {
            // Release (or don't keep) huge buffers.
            self.parser = TapeParser::new().expect("simdjson parser allocation");
        }
        let pad = padding();
        let text: &[u8] = if buf.len() - end >= pad {
            &buf[start..]
        } else {
            self.scratch.clear();
            self.scratch.extend_from_slice(&buf[start..end]);
            self.scratch.resize(len + pad, 0);
            &self.scratch
        };
        let tape = self.parser.parse(text, len).map_err(Rejected)?;
        let result = build(&tape, &text[..len], &mut self.stack, &mut self.keys);
        self.stack.clear();
        result
    }
}

/// An open container while walking the tape.
enum Frame {
    Array(Array),
    Object {
        obj: Object,
        /// Key waiting for its value.
        key: Option<Str>,
        /// Completed key/value pairs (duplicates included), for separator
        /// accounting.
        pairs: usize,
    },
}

const PAYLOAD_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;

#[inline]
fn is_number_byte(c: u8) -> bool {
    matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
}

/// Walks the tape and builds the value. `src` is the parsed text; the
/// structural cursor `si` tracks the source position of each tape item
/// (skipping the `:`/`,` separators that valid JSON puts in fixed places),
/// which is where number literals are read from.
fn build(
    tape: &Tape<'_>,
    src: &[u8],
    stack: &mut Vec<Frame>,
    keys: &mut KeyCache,
) -> Result<Value, Rejected> {
    let words = tape.words;
    let structurals = tape.structurals;
    let mut i = 1; // words[0] is the root
    let mut si = 0usize;
    loop {
        let word = words[i];
        let tag = (word >> 56) as u8;
        // Separator before this item (none before a closing bracket).
        if tag != b']' && tag != b'}' {
            match stack.last() {
                Some(Frame::Array(a)) if !a.is_empty() => si += 1, // ','
                Some(Frame::Object { key: Some(_), .. }) => si += 1, // ':'
                Some(Frame::Object {
                    key: None, pairs, ..
                }) if *pairs > 0 => si += 1, // ','
                _ => {}
            }
        }
        debug_assert!(
            structural_matches(tag, src, structurals, si),
            "structural cursor out of sync at tape word {i}"
        );
        let value = match tag {
            b'{' => {
                let count = ((word >> 32) & 0xFF_FFFF) as usize;
                stack.push(Frame::Object {
                    obj: Object::with_capacity(count),
                    key: None,
                    pairs: 0,
                });
                i += 1;
                si += 1;
                continue;
            }
            b'[' => {
                let count = ((word >> 32) & 0xFF_FFFF) as usize;
                stack.push(Frame::Array(Array::with_capacity(count)));
                i += 1;
                si += 1;
                continue;
            }
            b'}' | b']' => {
                i += 1;
                si += 1;
                match stack.pop() {
                    Some(Frame::Array(a)) => Value::Array(a),
                    Some(Frame::Object { obj, .. }) => Value::Object(obj),
                    None => return Err(Rejected::UNSUPPORTED),
                }
            }
            b'"' => {
                // SAFETY: the payload of a string word is its offset in the
                // string buffer of this tape.
                let bytes = unsafe { tape.string((word & PAYLOAD_MASK) as usize) };
                debug_assert!(std::str::from_utf8(bytes).is_ok());
                // SAFETY: simdjson validated the whole text as UTF-8, and a
                // decoded string is made of complete source sequences between
                // ASCII delimiters (quotes, escapes) and simdjson's UTF-8
                // encodings of escaped code points, which are never lone
                // surrogates (it rejects those) nor above U+10FFFF.
                let s = unsafe { std::str::from_utf8_unchecked(bytes) };
                i += 1;
                si += 1;
                if let Some(Frame::Object {
                    key: key @ None, ..
                }) = stack.last_mut()
                {
                    *key = Some(keys.get(s));
                    continue;
                }
                Value::String(Str::from(s))
            }
            b'l' | b'u' | b'd' => {
                let p = *structurals.get(si).ok_or(Rejected::UNSUPPORTED)? as usize;
                let rest = src.get(p..).ok_or(Rejected::UNSUPPORTED)?;
                if !matches!(rest.first(), Some(b'-' | b'0'..=b'9')) {
                    return Err(Rejected::UNSUPPORTED);
                }
                let n = rest
                    .iter()
                    .position(|&c| !is_number_byte(c))
                    .unwrap_or(rest.len());
                let number = Number::from_literal(&rest[..n]).ok_or(Rejected::UNSUPPORTED)?;
                i += 2;
                si += 1;
                Value::Number(number)
            }
            b't' => {
                i += 1;
                si += 1;
                Value::Bool(true)
            }
            b'f' => {
                i += 1;
                si += 1;
                Value::Bool(false)
            }
            b'n' => {
                i += 1;
                si += 1;
                Value::Null
            }
            _ => return Err(Rejected::UNSUPPORTED),
        };
        match stack.last_mut() {
            None => return Ok(value),
            Some(Frame::Array(a)) => a.push(value),
            Some(Frame::Object { obj, key, pairs }) => {
                let k = key.take().ok_or(Rejected::UNSUPPORTED)?;
                obj.insert(k, value);
                *pairs += 1;
            }
        }
    }
}

/// Debug check: the structural at `si` is the character the tape item
/// `tag` starts with.
fn structural_matches(tag: u8, src: &[u8], structurals: &[u32], si: usize) -> bool {
    let Some(&p) = structurals.get(si) else {
        return false;
    };
    let Some(&c) = src.get(p as usize) else {
        return false;
    };
    match tag {
        b'{' | b'}' | b'[' | b']' | b'"' => c == tag,
        b'l' | b'u' | b'd' => c == b'-' || c.is_ascii_digit(),
        b't' | b'f' | b'n' => c == tag,
        _ => true,
    }
}
