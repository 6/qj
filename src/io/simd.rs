//! simdjson → [`Value`]: builds jq values straight from simdjson's DOM tape.
//!
//! [`SimdParser::parse`] accepts exactly one strict JSON (RFC 8259) document.
//! For such a document jq 1.8.1's parser (`jv_parse.c`, ported as
//! [`crate::jq::value::Parser`]) produces a fully determined value, and this
//! module produces the same one:
//!
//! * objects keep jq's duplicate-key rule (the last value wins, at the first
//!   key's position, which keeps the first key string): an object whose keys
//!   are distinct is built from its entries in order, and one that may have
//!   duplicates is built with [`Object::insert`] like jq's `jv_object_set`;
//! * arrays are built from their elements with [`Array::from_vec`], which
//!   gives them the allocation jq's parser gives them by appending to
//!   `jv_array()` (observable through slices: `[1,2,3,4,5] | .[0:2] | .[5] =
//!   9` is `[1,2,3,4,5,9]`);
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

use crate::jq::value::number::Serials;
use crate::jq::value::{Array, Number, Object, Str, Value, hash_key};
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

/// The scratch stacks keep at most this many elements' worth of capacity
/// between documents.
const KEEP_ELEMENTS: usize = 1 << 16;

/// A reusable simdjson parser that builds jq values (one per thread).
pub struct SimdParser {
    parser: TapeParser,
    /// Padded copy of the text when the caller's buffer lacks padding.
    scratch: Vec<u8>,
    build: Builder,
}

/// Recently seen object keys, shared between the objects and documents a
/// parser builds (NDJSON repeats the same keys in every record). Sharing a
/// key's allocation isn't observable in jq: keys are never mutated in
/// place (copy on write), and identity only matters for values at paths
/// (`path_intact` in jq's execute.c), which keys never are. Every key the
/// cache returns has its hash cached ([`Str::key_hash`]).
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
        let h = hash_key(key.as_bytes());
        if key.len() > MAX_CACHED_KEY {
            let s = Str::from(key);
            s.set_key_hash(h);
            return s;
        }
        let slot = &mut self.slots[h as usize & (KEY_SLOTS - 1)];
        if let Some((sh, s)) = slot
            && *sh == h
            && s.as_bytes() == key.as_bytes()
        {
            return s.clone();
        }
        let s = Str::from(key);
        s.set_key_hash(h);
        *slot = Some((h, s.clone()));
        s
    }
}

/// Size of [`Builder::seen`] (a power of two). Objects with more than half
/// as many keys are checked for duplicates the slow way.
const SEEN_SLOTS: usize = 4096;

/// The state of [`build`], kept between documents for its allocations.
struct Builder {
    /// Open containers, innermost last (empty between documents).
    frames: Vec<Frame>,
    /// Elements of the open arrays.
    values: Vec<Value>,
    /// Entries of the open objects.
    entries: Vec<(Str, Value)>,
    keys: KeyCache,
    /// An open-addressing set of the key hashes of the object being closed:
    /// `(stamp, low 32 bits of the hash)`, where only slots with the
    /// current stamp are in the set.
    seen: Box<[(u32, u32)]>,
    stamp: u32,
    /// For inline integer literals.
    serials: Serials,
}

/// An open container while walking the tape.
struct Frame {
    object: bool,
    /// Where this container's elements start in `values` or `entries`.
    start: usize,
    /// The key waiting for its value (objects).
    key: Option<Str>,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            frames: Vec::new(),
            values: Vec::new(),
            entries: Vec::new(),
            keys: KeyCache::new(),
            seen: vec![(0, 0); SEEN_SLOTS].into_boxed_slice(),
            stamp: 0,
            serials: Serials::default(),
        }
    }

    /// Clears the stacks after a document (also after a rejected one).
    fn reset(&mut self) {
        self.frames.clear();
        self.values.clear();
        self.entries.clear();
        if self.values.capacity() > KEEP_ELEMENTS {
            self.values = Vec::new();
        }
        if self.entries.capacity() > KEEP_ELEMENTS {
            self.entries = Vec::new();
        }
    }

    /// Whether the keys of `entries[start..]` are certainly distinct: their
    /// hashes are (a false "no" just takes the slow path).
    fn distinct_keys(&mut self, start: usize) -> bool {
        let n = self.entries.len() - start;
        if n <= 1 {
            return true;
        }
        if n > SEEN_SLOTS / 2 {
            return false;
        }
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen.fill((0, 0));
            self.stamp = 1;
        }
        let stamp = self.stamp;
        for (k, _) in &self.entries[start..] {
            let h = k.key_hash();
            let tag = h as u32;
            let mut slot = (h >> 32) as usize & (SEEN_SLOTS - 1);
            loop {
                let e = &mut self.seen[slot];
                if e.0 != stamp {
                    *e = (stamp, tag);
                    break;
                }
                if e.1 == tag {
                    return false;
                }
                slot = (slot + 1) & (SEEN_SLOTS - 1);
            }
        }
        true
    }

    /// The object of `entries[start..]`, as jq's parser builds it by setting
    /// each key in turn.
    fn close_object(&mut self, start: usize) -> Object {
        if self.distinct_keys(start) {
            return Object::from_unique_entries(self.entries.drain(start..).collect());
        }
        let mut obj = Object::with_capacity(self.entries.len() - start);
        for (k, v) in self.entries.drain(start..) {
            obj.insert(k, v);
        }
        obj
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
            build: Builder::new(),
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
        let result = build(&tape, &text[..len], &mut self.build);
        self.build.reset();
        if len > KEEP_CAPACITY {
            // Free the big buffers now, not at the next document.
            self.parser = TapeParser::new().expect("simdjson parser allocation");
        }
        if self.scratch.capacity() > KEEP_CAPACITY {
            self.scratch = Vec::new();
        }
        result
    }
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
fn build(tape: &Tape<'_>, src: &[u8], b: &mut Builder) -> Result<Value, Rejected> {
    let words = tape.words;
    let structurals = tape.structurals;
    let mut i = 1; // words[0] is the root
    let mut si = 0usize;
    loop {
        let word = words[i];
        let tag = (word >> 56) as u8;
        // Separator before this item (none before a closing bracket).
        if tag != b']'
            && tag != b'}'
            && let Some(f) = b.frames.last()
        {
            if f.object {
                if f.key.is_some() || b.entries.len() > f.start {
                    si += 1; // ':' before a value, ',' before a later key
                }
            } else if b.values.len() > f.start {
                si += 1; // ','
            }
        }
        debug_assert!(
            structural_matches(tag, src, structurals, si),
            "structural cursor out of sync at tape word {i}"
        );
        let value = match tag {
            b'{' => {
                b.frames.push(Frame {
                    object: true,
                    start: b.entries.len(),
                    key: None,
                });
                i += 1;
                si += 1;
                continue;
            }
            b'[' => {
                b.frames.push(Frame {
                    object: false,
                    start: b.values.len(),
                    key: None,
                });
                i += 1;
                si += 1;
                continue;
            }
            b'}' => {
                i += 1;
                si += 1;
                let f = b.frames.pop().ok_or(Rejected::UNSUPPORTED)?;
                if !f.object || f.key.is_some() {
                    return Err(Rejected::UNSUPPORTED);
                }
                Value::Object(b.close_object(f.start))
            }
            b']' => {
                i += 1;
                si += 1;
                let f = b.frames.pop().ok_or(Rejected::UNSUPPORTED)?;
                if f.object {
                    return Err(Rejected::UNSUPPORTED);
                }
                Value::Array(Array::from_vec(b.values.drain(f.start..).collect()))
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
                if let Some(f) = b.frames.last_mut()
                    && f.object
                    && f.key.is_none()
                {
                    f.key = Some(b.keys.get(s));
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
                // An `l` item is an integer without fraction or exponent, and
                // JSON allows no `+` and no leading zeros, so its text is
                // canonical (the integer's decimal form), except `-0`.
                let inline = match tag {
                    b'l' => {
                        let v = *words.get(i + 1).ok_or(Rejected::UNSUPPORTED)? as i64;
                        if v != 0 || rest[0] != b'-' {
                            Number::inline_int_literal(v, &mut b.serials)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let number = match inline {
                    Some(n) => n,
                    None => {
                        let n = rest
                            .iter()
                            .position(|&c| !is_number_byte(c))
                            .unwrap_or(rest.len());
                        Number::from_literal(&rest[..n]).ok_or(Rejected::UNSUPPORTED)?
                    }
                };
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
        match b.frames.last_mut() {
            None => return Ok(value),
            Some(f) if f.object => {
                let k = f.key.take().ok_or(Rejected::UNSUPPORTED)?;
                b.entries.push((k, value));
            }
            Some(_) => b.values.push(value),
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
