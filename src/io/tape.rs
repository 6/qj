//! Printing and navigating a parsed document on simdjson's tape, without
//! building jq values.
//!
//! [`Doc`] is a read-only view of one document that [`super::simd`] parsed:
//! simdjson's tape, its string buffer, the stage-1 structural indexes and
//! the source text. [`Doc::print`] writes a value exactly as
//! [`crate::jq::value::print::dump_to_vec`] writes the
//! [`crate::jq::value::Value`] that [`super::simd`]'s builder makes of it:
//! jq's duplicate-key rule (the last value at the first key's position),
//! number literals in decNumber's canonical form, strings re-escaped by jq's
//! rules, jq's pretty-printing, sorted keys, ASCII output, and its
//! `<skipped: too deep>` past 256 levels. The tests (`src/io/tests/tape.rs`)
//! check exactly that, against the builder and the value printer, for every
//! layout.
//!
//! # The structural cursor
//!
//! Number literals are printed from their source text, found at their stage-1
//! structural index (and so is the sign of a zero, which the tape loses). The
//! tape has no such index, so a [`Node`] carries it: every tape item has one
//! structural (a container two: its brackets), and valid JSON puts a `,`
//! between elements and a `:` after each key. Walking the tape in order keeps
//! the cursor exact; [`Doc::skip`] counts a skipped container's structurals
//! from its own and its nested containers' element counts. (simdjson
//! saturates those counts at 2^24 - 1; such huge containers are counted by
//! walking them.)

use crate::jq::value::print::{
    DumpSink, MAX_PRINT_DEPTH, write_json_string, write_json_string_raw,
};
use crate::jq::value::{DumpOptions, Indent, Number, hash_key};
use crate::simdjson::Tape;

const PAYLOAD_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;
/// simdjson's saturated container count: the real count is at least this.
const COUNT_SATURATED: usize = 0xFF_FFFF;

/// Where jq stops printing nested values (`<skipped: too deep>`): values
/// deeper than this.
pub const PRINT_DEPTH: usize = MAX_PRINT_DEPTH;

/// One parsed document.
pub struct Doc<'a> {
    tape: &'a Tape<'a>,
    words: &'a [u64],
    structurals: &'a [u32],
    src: &'a [u8],
    /// `src` and the bytes after it in the parser's buffer (its padding).
    padded: &'a [u8],
}

/// A value in a [`Doc`]: its tape index and structural index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    i: usize,
    si: usize,
}

/// The structural index of a node that has none (an object key of
/// [`Doc::for_each_key`]; stepping past it keeps it).
const NO_CURSOR: usize = usize::MAX;

/// What a value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Null,
    False,
    True,
    Number,
    String,
    Array,
    Object,
}

/// How [`Doc::print`] lays values out: jq's dump flags, without colors.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pretty: bool,
    tab: bool,
    spaces: usize,
    sort_keys: bool,
    ascii: bool,
}

impl Layout {
    /// The layout of `opts`, or `None` with colors (not handled here).
    pub fn new(opts: &DumpOptions) -> Option<Layout> {
        if opts.colors.is_some() {
            return None;
        }
        let (pretty, tab, spaces) = match opts.indent {
            Indent::Compact => (false, false, 0),
            Indent::Spaces(n) => (true, false, n as usize),
            Indent::Tab => (true, true, 0),
        };
        Some(Layout {
            pretty,
            tab,
            spaces,
            sort_keys: opts.sort_keys,
            ascii: opts.ascii,
        })
    }

    /// Whether non-ASCII characters are escaped (`-a`).
    pub fn ascii(&self) -> bool {
        self.ascii
    }

    /// Whether object keys are sorted (`-S`).
    pub fn sort_keys(&self) -> bool {
        self.sort_keys
    }

    #[inline]
    fn indent(&self, n: usize, out: &mut Vec<u8>) {
        if self.tab {
            out.resize(out.len() + n, b'\t');
        } else {
            out.resize(out.len() + n * self.spaces, b' ');
        }
    }

    /// Before element `index` of a non-empty container at `depth`.
    #[inline]
    pub fn before_element(&self, index: usize, depth: usize, out: &mut Vec<u8>) {
        if index != 0 {
            out.push(b',');
        }
        if self.pretty {
            out.push(b'\n');
            self.indent(depth + 1, out);
        }
    }

    /// An object key and what follows it, before its value.
    #[inline]
    pub fn key(&self, key: &str, out: &mut Vec<u8>) {
        write_json_string(key, self.ascii, out);
        out.push(b':');
        if self.pretty {
            out.push(b' ');
        }
    }

    /// Before the closing bracket of a non-empty container at `depth`.
    #[inline]
    pub fn before_close(&self, depth: usize, out: &mut Vec<u8>) {
        if self.pretty {
            out.push(b'\n');
            self.indent(depth, out);
        }
    }
}

#[inline]
fn tag(w: u64) -> u8 {
    (w >> 56) as u8
}

#[inline]
fn is_number_byte(c: u8) -> bool {
    matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
}

/// Scratch space for printing objects with duplicate keys or sorted keys.
#[derive(Default)]
pub struct Scratch {
    /// `(stamp, low 32 bits of a key hash)` open-addressing slots.
    seen: Vec<(u32, u32)>,
    stamp: u32,
    entries: Vec<Vec<(Node, Node)>>,
}

const SEEN_SLOTS: usize = 4096;

/// Objects with at most this many keys are checked for duplicates pairwise.
const FEW_KEYS: usize = 16;

/// A cheap summary of a key: equal keys have equal fingerprints. (Length
/// and the first and last 8 bytes, read with overlapping loads.)
#[inline]
pub(crate) fn fingerprint(b: &[u8]) -> u64 {
    let n = b.len();
    let (a, z) = if n >= 8 {
        (
            u64::from_le_bytes(b[..8].try_into().expect("8 bytes")),
            u64::from_le_bytes(b[n - 8..].try_into().expect("8 bytes")),
        )
    } else if n >= 4 {
        (
            u64::from(u32::from_le_bytes(b[..4].try_into().expect("4 bytes"))),
            u64::from(u32::from_le_bytes(b[n - 4..].try_into().expect("4 bytes"))),
        )
    } else if n > 0 {
        (
            u64::from(b[0]) | u64::from(b[n / 2]) << 8 | u64::from(b[n - 1]) << 16,
            0,
        )
    } else {
        (0, 0)
    };
    a ^ z.rotate_left(29) ^ (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// `a == b`, with overlapping loads rather than a call to `memcmp` for up
/// to 16 bytes.
#[inline]
pub(crate) fn short_eq(a: &[u8], b: &[u8]) -> bool {
    let n = a.len();
    if n != b.len() {
        return false;
    }
    let word = |s: &[u8], i: usize| u64::from_le_bytes(s[i..i + 8].try_into().expect("8 bytes"));
    let half = |s: &[u8], i: usize| u32::from_le_bytes(s[i..i + 4].try_into().expect("4 bytes"));
    match n {
        0 => true,
        1..=3 => a[0] == b[0] && a[n / 2] == b[n / 2] && a[n - 1] == b[n - 1],
        4..=7 => half(a, 0) == half(b, 0) && half(a, n - 4) == half(b, n - 4),
        8..=16 => word(a, 0) == word(b, 0) && word(a, n - 8) == word(b, n - 8),
        _ => a == b,
    }
}

impl Scratch {
    /// A list for one object's entries (returned with [`Scratch::put`]).
    fn take(&mut self) -> Vec<(Node, Node)> {
        let mut v = self.entries.pop().unwrap_or_default();
        v.clear();
        v
    }

    fn put(&mut self, v: Vec<(Node, Node)>) {
        self.entries.push(v);
    }
}

impl<'a> Doc<'a> {
    /// The document whose tape is `tape`: the text `padded[..len]`, which
    /// the rest of `padded` follows (the parser's padding).
    pub fn new(tape: &'a Tape<'a>, padded: &'a [u8], len: usize) -> Doc<'a> {
        Doc {
            tape,
            words: tape.words,
            structurals: tape.structurals,
            src: &padded[..len],
            padded,
        }
    }

    /// The document's root value.
    #[inline]
    pub fn root(&self) -> Node {
        Node { i: 1, si: 0 }
    }

    #[inline]
    fn word(&self, n: Node) -> u64 {
        self.words[n.i]
    }

    /// The kind of `n`.
    #[inline]
    pub fn kind(&self, n: Node) -> NodeKind {
        match tag(self.word(n)) {
            b'n' => NodeKind::Null,
            b'f' => NodeKind::False,
            b't' => NodeKind::True,
            b'l' | b'u' | b'd' => NodeKind::Number,
            b'"' => NodeKind::String,
            b'[' => NodeKind::Array,
            b'{' => NodeKind::Object,
            t => unreachable!("not a value on the tape: {t}"),
        }
    }

    /// The element count of an array, or the key count of an object *on the
    /// tape* (duplicate keys included).
    #[inline]
    pub fn count(&self, n: Node) -> usize {
        self.count_at(n.i)
    }

    /// [`Doc::count`] of the container whose open word is at `i`.
    #[inline]
    fn count_at(&self, i: usize) -> usize {
        let w = self.words[i];
        let c = ((w >> 32) & 0xFF_FFFF) as usize;
        if c < COUNT_SATURATED {
            return c;
        }
        // Saturated: count by walking the elements.
        let end = (w & 0xFFFF_FFFF) as usize - 1; // the closing word
        let mut j = i + 1;
        let mut c = 0;
        let object = tag(w) == b'{';
        while j < end {
            if object {
                j += 1; // the key
            }
            j = self.jump_at(j);
            c += 1;
        }
        c
    }

    /// The contents of a string node (or of an object key).
    #[inline]
    pub fn str(&self, n: Node) -> &'a str {
        debug_assert_eq!(tag(self.word(n)), b'"');
        // SAFETY: the payload of a string word is its offset in the tape's
        // string buffer.
        let bytes = unsafe { self.tape.string((self.word(n) & PAYLOAD_MASK) as usize) };
        // SAFETY: simdjson validated the text as UTF-8, and its unescaped
        // strings are valid UTF-8 (see `super::simd`).
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    /// The source text of a number node.
    #[inline]
    fn number_text(&self, n: Node) -> &'a [u8] {
        let p = self.structurals[n.si] as usize;
        let rest = &self.src[p..];
        let len = rest
            .iter()
            .position(|&c| !is_number_byte(c))
            .unwrap_or(rest.len());
        &rest[..len]
    }

    /// The number [`super::simd`]'s builder makes of a number node (without
    /// an inline literal's identity, which nothing here shows).
    pub fn number(&self, n: Node) -> Number {
        match Number::from_literal(self.number_text(n)) {
            Some(x) => x,
            None => unreachable!("simdjson accepted a number decNumber doesn't"),
        }
    }

    /// Writes a number node as jq prints its literal: an integer item's
    /// digits (its canonical decNumber form, `-0` included), or the
    /// canonical form of the text.
    #[inline]
    fn write_number(&self, n: Node, out: &mut Vec<u8>) {
        let w = self.word(n);
        match tag(w) {
            b'l' => {
                let v = self.words[n.i + 1] as i64;
                if v == 0 && self.src[self.structurals[n.si] as usize] == b'-' {
                    out.extend_from_slice(b"-0");
                } else {
                    out.extend_from_slice(itoa::Buffer::new().format(v).as_bytes());
                }
            }
            b'u' => {
                let v = self.words[n.i + 1];
                out.extend_from_slice(itoa::Buffer::new().format(v).as_bytes());
            }
            _ => self.number(n).write_json(out),
        }
    }

    /// The tape index just after the value whose first word is at `i`
    /// (without the structural cursor, so in constant time).
    #[inline]
    fn jump_at(&self, i: usize) -> usize {
        let w = self.words[i];
        match tag(w) {
            b'[' | b'{' => (w & 0xFFFF_FFFF) as usize,
            b'l' | b'u' | b'd' => i + 2,
            _ => i + 1,
        }
    }

    /// The node just after `n`: where its next sibling starts (before the
    /// separator), or its parent's closing bracket.
    #[inline]
    pub fn skip(&self, n: Node) -> Node {
        let w = self.word(n);
        match tag(w) {
            b'[' | b'{' => self.skip_container(n, w),
            b'l' | b'u' | b'd' => Node {
                i: n.i + 2,
                si: n.si + 1,
            },
            // (Saturating: the keys of `for_each_key` have no cursor, and
            // what comes after them has none either.)
            _ => Node {
                i: n.i + 1,
                si: n.si.saturating_add(1),
            },
        }
    }

    fn skip_container(&self, n: Node, w: u64) -> Node {
        let end = (w & 0xFFFF_FFFF) as usize;
        let mut si = n.si;
        let mut j = n.i;
        while j < end {
            let w = self.words[j];
            match tag(w) {
                b'[' => {
                    // `[` and the commas between elements.
                    let c = self.count_at(j);
                    si += 1 + c.saturating_sub(1);
                    j += 1;
                }
                b'{' => {
                    // `{`, a `:` per key, and the commas between keys.
                    let c = self.count_at(j);
                    si += 1 + c + c.saturating_sub(1);
                    j += 1;
                }
                // Two words: the value's is raw data.
                b'l' | b'u' | b'd' => {
                    si += 1;
                    j += 2;
                }
                // Strings (keys too), literals, closing brackets.
                _ => {
                    si += 1;
                    j += 1;
                }
            }
        }
        Node { i: end, si }
    }

    /// Calls `f` on each element of an array, in order.
    #[inline]
    pub fn for_each_element<E>(
        &self,
        array: Node,
        mut f: impl FnMut(Node) -> Result<(), E>,
    ) -> Result<(), E> {
        debug_assert_eq!(tag(self.word(array)), b'[');
        let n = self.count(array);
        let mut e = Node {
            i: array.i + 1,
            si: array.si + 1,
        };
        for k in 0..n {
            if k != 0 {
                e.si += 1; // ','
            }
            f(e)?;
            e = self.skip(e);
        }
        Ok(())
    }

    /// Calls `f` on each key/value pair of an object *on the tape*, in
    /// order (duplicate keys included).
    #[inline]
    pub fn for_each_entry<E>(
        &self,
        object: Node,
        mut f: impl FnMut(Node, Node) -> Result<(), E>,
    ) -> Result<(), E> {
        debug_assert_eq!(tag(self.word(object)), b'{');
        let n = self.count(object);
        let mut k = Node {
            i: object.i + 1,
            si: object.si + 1,
        };
        for idx in 0..n {
            if idx != 0 {
                k.si += 1; // ','
            }
            let v = Node {
                i: k.i + 1,
                si: k.si + 2, // the key and ':'
            };
            f(k, v)?;
            k = self.skip(v);
        }
        Ok(())
    }

    /// Calls `f` on each key of an object *on the tape*, in order. The keys'
    /// nodes have no structural cursor (strings don't need one), which makes
    /// this cheaper than [`Doc::for_each_entry`].
    #[inline]
    pub fn for_each_key<E>(
        &self,
        object: Node,
        mut f: impl FnMut(Node) -> Result<(), E>,
    ) -> Result<(), E> {
        debug_assert_eq!(tag(self.word(object)), b'{');
        let n = self.count(object);
        let mut i = object.i + 1;
        for _ in 0..n {
            f(Node { i, si: NO_CURSOR })?;
            i = self.jump_at(i + 1);
        }
        Ok(())
    }

    /// The value jq's object has for `key` (the last one of a duplicated
    /// key), if the key is there.
    pub fn get(&self, object: Node, key: &str) -> Option<Node> {
        debug_assert_eq!(tag(self.word(object)), b'{');
        let n = self.count(object);
        let mut k = Node {
            i: object.i + 1,
            si: object.si + 1,
        };
        for idx in 0..n {
            if idx != 0 {
                k.si += 1; // ','
            }
            let v = Node {
                i: k.i + 1,
                si: k.si + 2,
            };
            if self.str(k) == key {
                // Only another occurrence could change the answer: look at
                // the remaining keys without tracking the cursor.
                let mut i = self.jump_at(v.i);
                for _ in idx + 1..n {
                    if self.str(Node { i, si: NO_CURSOR }) == key {
                        return self.get_last(object, key);
                    }
                    i = self.jump_at(i + 1);
                }
                return Some(v);
            }
            k = self.skip(v);
        }
        None
    }

    /// Whether an object has `key` (once or more).
    pub fn has_key(&self, object: Node, key: &str) -> bool {
        debug_assert_eq!(tag(self.word(object)), b'{');
        self.for_each_key(
            object,
            |k| if self.str(k) == key { Err(()) } else { Ok(()) },
        )
        .is_err()
    }

    /// [`Doc::get`] for a key that occurs more than once.
    fn get_last(&self, object: Node, key: &str) -> Option<Node> {
        let mut found = None;
        let _ = self.for_each_entry(object, |k, v| {
            if self.str(k) == key {
                found = Some(v);
            }
            Ok::<(), ()>(())
        });
        found
    }

    /// The entries of an object as jq's object holds them: in order of
    /// first appearance, each with its key's last value. `None` when the
    /// keys are distinct (the tape's order is jq's order).
    pub fn dedup_entries(&self, object: Node, scratch: &mut Scratch) -> Option<Vec<(Node, Node)>> {
        if !self.may_have_duplicate_keys(object, scratch) {
            return None;
        }
        let mut entries = scratch.take();
        self.entries(object, &mut entries);
        let n = entries.len();
        self.dedup(&mut entries);
        if entries.len() == n {
            // Only hash collisions: distinct after all.
            scratch.put(entries);
            return None;
        }
        Some(entries)
    }

    /// Whether an object's keys may repeat: `false` means they're distinct;
    /// `true` may be a hash collision.
    pub fn may_have_duplicate_keys(&self, object: Node, scratch: &mut Scratch) -> bool {
        let n = self.count(object);
        if n <= 1 {
            return false;
        }
        if n <= FEW_KEYS {
            // Few keys: compare them pairwise, by fingerprint first.
            let mut prints = [0u64; FEW_KEYS];
            let mut keys = [0usize; FEW_KEYS];
            let mut dup = false;
            let mut idx = 0;
            let _ = self.for_each_key(object, |k| {
                let s = self.str(k);
                let f = fingerprint(s.as_bytes());
                for j in 0..idx {
                    if prints[j] == f && self.str(Node { i: keys[j], si: 0 }) == s {
                        dup = true;
                        return Err(()); // stop
                    }
                }
                prints[idx] = f;
                keys[idx] = k.i;
                idx += 1;
                Ok(())
            });
            dup
        } else if n <= SEEN_SLOTS / 2 {
            // Hash the keys into a stamped open-addressing set.
            if scratch.seen.is_empty() {
                scratch.seen = vec![(0, 0); SEEN_SLOTS];
            }
            scratch.stamp = scratch.stamp.wrapping_add(1);
            if scratch.stamp == 0 {
                scratch.seen.fill((0, 0));
                scratch.stamp = 1;
            }
            let stamp = scratch.stamp;
            let seen = &mut scratch.seen;
            let mut maybe_dup = false;
            let _ = self.for_each_key(object, |k| {
                let h = hash_key(self.str(k).as_bytes());
                let tag = h as u32;
                let mut slot = (h >> 32) as usize & (SEEN_SLOTS - 1);
                loop {
                    let e = &mut seen[slot];
                    if e.0 != stamp {
                        *e = (stamp, tag);
                        return Ok(());
                    }
                    if e.1 == tag {
                        maybe_dup = true;
                        return Err(()); // stop
                    }
                    slot = (slot + 1) & (SEEN_SLOTS - 1);
                }
            });
            maybe_dup
        } else {
            true
        }
    }

    /// Prints `n` (at nesting depth `depth`) as jq prints the value
    /// [`super::simd`] builds of it.
    ///
    /// Returns the node after `n` (what [`Doc::skip`] gives), so that
    /// printing walks the tape once.
    pub fn print<S: DumpSink>(
        &self,
        n: Node,
        depth: usize,
        layout: &Layout,
        scratch: &mut Scratch,
        sink: &mut S,
    ) -> Node {
        if depth > PRINT_DEPTH {
            sink.buf().extend_from_slice(b"<skipped: too deep>");
            return self.skip(n);
        }
        if let Some(next) = self.print_scalar(n, layout, sink.buf()) {
            return next;
        }
        // Elements that are scalars are printed here, not by a call (unless
        // they're too deep).
        let inline = depth < PRINT_DEPTH;
        let w = self.word(n);
        match tag(w) {
            b'[' => {
                let count = self.count(n);
                if count == 0 {
                    sink.buf().extend_from_slice(b"[]");
                    return self.after_close(n);
                }
                sink.buf().push(b'[');
                let mut e = Node {
                    i: n.i + 1,
                    si: n.si + 1,
                };
                for idx in 0..count {
                    if idx != 0 {
                        e.si += 1; // ','
                    }
                    let out = sink.buf();
                    layout.before_element(idx, depth, out);
                    e = match inline.then(|| self.print_scalar(e, layout, out)).flatten() {
                        Some(next) => next,
                        None => self.print(e, depth + 1, layout, scratch, sink),
                    };
                    sink.checkpoint();
                }
                let out = sink.buf();
                layout.before_close(depth, out);
                out.push(b']');
                // `e` is at the closing bracket.
                Node {
                    i: e.i + 1,
                    si: e.si + 1,
                }
            }
            b'{' => {
                let count = self.count(n);
                if count == 0 {
                    sink.buf().extend_from_slice(b"{}");
                    return self.after_close(n);
                }
                sink.buf().push(b'{');
                let close;
                if !layout.sort_keys && !self.may_have_duplicate_keys(n, scratch) {
                    let mut k = Node {
                        i: n.i + 1,
                        si: n.si + 1,
                    };
                    for idx in 0..count {
                        if idx != 0 {
                            k.si += 1; // ','
                        }
                        let out = sink.buf();
                        layout.before_element(idx, depth, out);
                        self.write_key(k, layout, out);
                        let v = Node {
                            i: k.i + 1,
                            si: k.si + 2, // the key and ':'
                        };
                        k = match inline.then(|| self.print_scalar(v, layout, out)).flatten() {
                            Some(next) => next,
                            None => self.print(v, depth + 1, layout, scratch, sink),
                        };
                        sink.checkpoint();
                    }
                    close = k;
                } else {
                    let mut entries = scratch.take();
                    close = self.entries(n, &mut entries);
                    self.dedup(&mut entries);
                    if layout.sort_keys {
                        entries.sort_by(|a, b| self.str(a.0).cmp(self.str(b.0)));
                    }
                    for (idx, &(k, v)) in entries.iter().enumerate() {
                        let out = sink.buf();
                        layout.before_element(idx, depth, out);
                        self.write_key(k, layout, out);
                        self.print(v, depth + 1, layout, scratch, sink);
                        sink.checkpoint();
                    }
                    scratch.put(entries);
                }
                let out = sink.buf();
                layout.before_close(depth, out);
                out.push(b'}');
                Node {
                    i: close.i + 1,
                    si: close.si + 1,
                }
            }
            t => unreachable!("not a value on the tape: {t}"),
        }
    }

    /// Writes a string node (a value or a key) as jq prints it: from its
    /// text in the source when that is the same (see
    /// [`write_json_string_raw`]) and the node has a structural cursor (the
    /// keys of [`Doc::for_each_key`] have none).
    #[inline]
    fn write_string(&self, n: Node, ascii: bool, out: &mut Vec<u8>) {
        let s = self.str(n);
        if n.si == NO_CURSOR {
            return write_json_string(s, ascii, out);
        }
        let q = self.structurals[n.si] as usize;
        debug_assert_eq!(self.src[q], b'"', "the cursor of a string is at its quote");
        let raw = self.padded.get(q + 1..).unwrap_or_default();
        write_json_string_raw(raw, s, ascii, out);
    }

    /// An object key (with its structural cursor) and what follows it,
    /// before its value.
    #[inline]
    fn write_key(&self, k: Node, layout: &Layout, out: &mut Vec<u8>) {
        self.write_string(k, layout.ascii, out);
        out.push(b':');
        if layout.pretty {
            out.push(b' ');
        }
    }

    /// Prints `n` if it is a scalar, returning the node after it.
    #[inline(always)]
    fn print_scalar(&self, n: Node, layout: &Layout, out: &mut Vec<u8>) -> Option<Node> {
        let next = |words| Node {
            i: n.i + words,
            si: n.si.saturating_add(1),
        };
        match tag(self.word(n)) {
            b'"' => {
                self.write_string(n, layout.ascii, out);
                Some(next(1))
            }
            b'l' | b'u' | b'd' => {
                self.write_number(n, out);
                Some(next(2))
            }
            b'n' => {
                out.extend_from_slice(b"null");
                Some(next(1))
            }
            b'f' => {
                out.extend_from_slice(b"false");
                Some(next(1))
            }
            b't' => {
                out.extend_from_slice(b"true");
                Some(next(1))
            }
            _ => None,
        }
    }

    /// The node after an empty container (its two brackets).
    #[inline]
    fn after_close(&self, n: Node) -> Node {
        Node {
            i: n.i + 2,
            si: n.si + 2,
        }
    }

    /// Appends the entries of an object *on the tape* to `into`, and
    /// returns the node of its closing bracket.
    fn entries(&self, object: Node, into: &mut Vec<(Node, Node)>) -> Node {
        let n = self.count(object);
        let mut k = Node {
            i: object.i + 1,
            si: object.si + 1,
        };
        for idx in 0..n {
            if idx != 0 {
                k.si += 1; // ','
            }
            let v = Node {
                i: k.i + 1,
                si: k.si + 2,
            };
            into.push((k, v));
            k = self.skip(v);
        }
        k
    }

    /// jq's rule on an object's entries: the first position of each key,
    /// with its last value.
    fn dedup(&self, entries: &mut Vec<(Node, Node)>) {
        let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut w = 0;
        for r in 0..entries.len() {
            let (k, v) = entries[r];
            match index.get(self.str(k)) {
                Some(&at) => entries[at].1 = v,
                None => {
                    index.insert(self.str(k), w);
                    entries[w] = (k, v);
                    w += 1;
                }
            }
        }
        entries.truncate(w);
    }
}
