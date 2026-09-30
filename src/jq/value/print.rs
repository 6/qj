//! Port of jq's `jv_print.c`: JSON output with every dump flag (pretty with
//! `--indent n` / `--tab`, compact, sorted keys, ASCII-only, colors with
//! `JQ_COLORS`), plus `jv_dump_string` and `jv_dump_string_trunc`.
//!
//! Output is built in a byte buffer (flushed to the writer in large chunks),
//! numbers use `itoa`/`ryu`, and string escaping copies unescaped runs.

use std::io::{self, Write};

use super::{Number, Value};

/// Nesting depth after which jq prints `<skipped: too deep>` (`MAX_PRINT_DEPTH`).
pub const MAX_PRINT_DEPTH: usize = 256;

const ESC: &str = "\x1b";
const COLRESET: &[u8] = b"\x1b[0m";

/// jq's default palette (`DEFAULT_COLORS`): null, false, true, numbers,
/// strings, arrays, objects, object keys.
const DEFAULT_COLORS: [&str; 8] = [
    "\x1b[0;90m",
    "\x1b[0;39m",
    "\x1b[0;39m",
    "\x1b[0;39m",
    "\x1b[0;32m",
    "\x1b[1;39m",
    "\x1b[1;39m",
    "\x1b[1;34m",
];

/// The color palette (`colors[]` in jv_print.c): one escape sequence per
/// kind in `jv_kind` order (null, false, true, number, string, array,
/// object) plus the object-key color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    codes: [String; 8],
}

impl Default for Colors {
    fn default() -> Colors {
        Colors {
            codes: DEFAULT_COLORS.map(String::from),
        }
    }
}

impl Colors {
    /// Port of `jq_set_colors`: parses a `JQ_COLORS` value (colon-separated
    /// SGR parameters made of digits and `;`, at most 8; missing entries keep
    /// their defaults, an empty string means all defaults). Returns `None`
    /// on an invalid character (jq then warns `Failed to set $JQ_COLORS`
    /// and keeps the defaults).
    pub fn parse(spec: &str) -> Option<Colors> {
        const COLORS_LEN: usize = 8;
        let s = spec.as_bytes();
        // Start of each color code, and one past the end of the last one.
        let mut codes: Vec<usize> = Vec::with_capacity(COLORS_LEN + 1);
        let mut pos = 0usize;
        let mut num_colors = 0usize;
        loop {
            codes.push(pos);
            while pos < s.len() && (s[pos].is_ascii_digit() || s[pos] == b';') {
                pos += 1;
            }
            if pos >= s.len() || num_colors + 1 >= COLORS_LEN {
                break;
            } else if s[pos] != b':' {
                return None; // invalid character
            }
            pos += 1;
            num_colors += 1;
        }
        let mut colors = Colors::default();
        if codes[num_colors] != pos {
            // count the last color and store its end (plus one byte for
            // consistency with starts); an empty last color is ignored
            num_colors += 1;
            codes.push(pos + 1);
        } else if num_colors == 0 {
            return Some(colors);
        }
        for ci in 0..num_colors {
            let start = codes[ci];
            let end = codes[ci + 1] - 1;
            let code = &spec[start..end];
            colors.codes[ci] = format!("{ESC}[{code}m");
        }
        Some(colors)
    }

    /// The escape sequence used for values of this kind.
    fn for_value(&self, v: &Value) -> &[u8] {
        let i = v.kind() as usize - 1;
        self.codes[i].as_bytes()
    }

    fn field(&self) -> &[u8] {
        self.codes[7].as_bytes()
    }
}

/// Indentation style for pretty output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Indent {
    /// Compact output (no `JV_PRINT_PRETTY`).
    #[default]
    Compact,
    /// Pretty output with this many spaces per level (`--indent n`, 0..=7;
    /// jq's default is 2).
    Spaces(u8),
    /// Pretty output indented with tabs (`--tab`, `--indent -1`).
    Tab,
}

/// jq's dump flags (`JV_PRINT_*`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DumpOptions {
    /// `JV_PRINT_PRETTY` + indent width, or compact.
    pub indent: Indent,
    /// `JV_PRINT_SORTED` (`-S`): object keys sorted at every level.
    pub sort_keys: bool,
    /// `JV_PRINT_ASCII` (`-a`): non-ASCII as `\uXXXX` escapes.
    pub ascii: bool,
    /// `JV_PRINT_COLOR` (`-C`) with this palette.
    pub colors: Option<Colors>,
}

impl DumpOptions {
    /// Compact output (flags 0, as used by `tojson`).
    pub fn compact() -> DumpOptions {
        DumpOptions::default()
    }

    /// jq's default output: pretty with 2 spaces.
    pub fn pretty() -> DumpOptions {
        DumpOptions {
            indent: Indent::Spaces(2),
            ..DumpOptions::default()
        }
    }

    /// `JV_PRINT_INDENT_FLAGS(n)`: tab for `n < 0 || n > 7`, else `n` spaces.
    pub fn with_indent(n: i32) -> DumpOptions {
        DumpOptions {
            indent: if !(0..=7).contains(&n) {
                Indent::Tab
            } else {
                Indent::Spaces(n as u8)
            },
            ..DumpOptions::default()
        }
    }
}

/// Where the printer writes: a byte buffer that may be drained to a writer
/// between elements.
trait Sink {
    fn buf(&mut self) -> &mut Vec<u8>;
    /// Called between elements; may flush or abort the dump.
    fn checkpoint(&mut self) -> io::Result<()>;
}

impl Sink for Vec<u8> {
    #[inline]
    fn buf(&mut self) -> &mut Vec<u8> {
        self
    }
    #[inline]
    fn checkpoint(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const FLUSH_AT: usize = 1 << 16;

struct WriterSink<'w, W: Write> {
    buf: Vec<u8>,
    w: &'w mut W,
}

impl<W: Write> Sink for WriterSink<'_, W> {
    #[inline]
    fn buf(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }
    #[inline]
    fn checkpoint(&mut self) -> io::Result<()> {
        if self.buf.len() >= FLUSH_AT {
            self.w.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

/// Where a dump goes when its owner may write it out as it's produced (like
/// stdio, which jq's `jv_dumpf` writes a token at a time): the printers
/// append to [`DumpSink::buf`] and call [`DumpSink::checkpoint`] between
/// elements.
pub trait DumpSink {
    fn buf(&mut self) -> &mut Vec<u8>;
    /// Between elements: the owner may write out (a prefix of) the buffer.
    fn checkpoint(&mut self);
}

impl DumpSink for Vec<u8> {
    #[inline]
    fn buf(&mut self) -> &mut Vec<u8> {
        self
    }
    #[inline]
    fn checkpoint(&mut self) {}
}

/// A [`DumpSink`] as the printer's sink.
struct Streaming<'s, S: DumpSink>(&'s mut S);

impl<S: DumpSink> Sink for Streaming<'_, S> {
    #[inline]
    fn buf(&mut self) -> &mut Vec<u8> {
        self.0.buf()
    }
    #[inline]
    fn checkpoint(&mut self) -> io::Result<()> {
        self.0.checkpoint();
        Ok(())
    }
}

/// Stops the dump once more than `limit` bytes were produced.
struct TruncSink {
    buf: Vec<u8>,
    limit: usize,
}

impl Sink for TruncSink {
    #[inline]
    fn buf(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }
    #[inline]
    fn checkpoint(&mut self) -> io::Result<()> {
        if self.buf.len() > self.limit {
            Err(io::Error::other("truncated"))
        } else {
            Ok(())
        }
    }
}

/// Bytes that need escaping in a JSON string: control characters, `"`,
/// `\` and DEL.
static NEEDS_ESCAPE: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = true;
        i += 1;
    }
    t[b'"' as usize] = true;
    t[b'\\' as usize] = true;
    t[0x7F] = true;
    t
};

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_u_escape(out: &mut Vec<u8>, c: u32) {
    out.extend_from_slice(b"\\u");
    out.push(HEX[((c >> 12) & 0xF) as usize]);
    out.push(HEX[((c >> 8) & 0xF) as usize]);
    out.push(HEX[((c >> 4) & 0xF) as usize]);
    out.push(HEX[(c & 0xF) as usize]);
}

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// A mask whose lowest set bit is the high bit of the first byte of `w`
/// (little-endian) that [`NEEDS_ESCAPE`] flags, and that is zero when none
/// does. Higher bits may be spurious (borrows), so only the lowest counts.
#[inline]
fn escape_mask(w: u64) -> u64 {
    #[inline]
    fn zero_bytes(v: u64) -> u64 {
        v.wrapping_sub(LO) & !v & HI
    }
    // Bytes below 0x20 (exact for the lowest one; bytes >= 0x80 never set
    // their bit because of `!w`).
    let control = w.wrapping_sub(0x20 * LO) & !w & HI;
    control
        | zero_bytes(w ^ (b'"' as u64 * LO))
        | zero_bytes(w ^ (b'\\' as u64 * LO))
        | zero_bytes(w ^ (0x7F * LO))
}

/// The index of the first byte at or after `i` that may need escaping
/// (see [`NEEDS_ESCAPE`]; with `ascii_only` also non-ASCII bytes), or where
/// fewer than 8 bytes are left (the caller looks at those one by one).
#[inline]
fn skip_plain(bytes: &[u8], mut i: usize, ascii_only: bool) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        while i + 16 <= bytes.len() {
            // SAFETY: NEON is part of the aarch64 baseline, and the load
            // reads the 16 bytes at `i`, which are in bounds.
            let mask = unsafe {
                let v = vld1q_u8(bytes.as_ptr().add(i));
                let control = vcltq_u8(v, vdupq_n_u8(0x20));
                let quote = vceqq_u8(v, vdupq_n_u8(b'"'));
                let backslash = vceqq_u8(v, vdupq_n_u8(b'\\'));
                let del = vceqq_u8(v, vdupq_n_u8(0x7F));
                let mut m = vorrq_u8(vorrq_u8(control, quote), vorrq_u8(backslash, del));
                if ascii_only {
                    m = vorrq_u8(m, vcgeq_u8(v, vdupq_n_u8(0x80)));
                }
                // Four bits per byte, in order.
                let nibbles = vshrn_n_u16(vreinterpretq_u16_u8(m), 4);
                vget_lane_u64(vreinterpret_u64_u8(nibbles), 0)
            };
            if mask != 0 {
                return i + (mask.trailing_zeros() / 4) as usize;
            }
            i += 16;
        }
    }
    let high = if ascii_only { HI } else { 0 };
    while let Some(chunk) = bytes.get(i..i + 8) {
        let w = u64::from_le_bytes(chunk.try_into().expect("8 bytes"));
        let m = escape_mask(w) | (w & high);
        if m != 0 {
            return i + (m.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    i
}

/// Whether no byte of `b` needs escaping (see [`NEEDS_ESCAPE`]; with
/// `ascii_only`, no byte is non-ASCII either). Short strings are checked with
/// two overlapping loads rather than byte by byte.
#[inline]
fn is_plain(b: &[u8], ascii_only: bool) -> bool {
    let n = b.len();
    let high = if ascii_only { HI } else { 0 };
    // (A mask is non-zero exactly when some byte is flagged: borrows only
    // start at flagged bytes.)
    let flagged = |w: u64| escape_mask(w) | (w & high) != 0;
    let word = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"));
    if n >= 16 {
        #[cfg(target_arch = "aarch64")]
        {
            use std::arch::aarch64::*;
            // SAFETY: NEON is part of the aarch64 baseline, and every load
            // reads 16 bytes at an offset at most `n - 16`.
            unsafe {
                let chunk = |i: usize| {
                    let v = vld1q_u8(b.as_ptr().add(i));
                    let control = vcltq_u8(v, vdupq_n_u8(0x20));
                    let quote = vceqq_u8(v, vdupq_n_u8(b'"'));
                    let backslash = vceqq_u8(v, vdupq_n_u8(b'\\'));
                    let del = vceqq_u8(v, vdupq_n_u8(0x7F));
                    let mut m = vorrq_u8(vorrq_u8(control, quote), vorrq_u8(backslash, del));
                    if ascii_only {
                        m = vorrq_u8(m, vcgeq_u8(v, vdupq_n_u8(0x80)));
                    }
                    vmaxvq_u8(m) != 0
                };
                let mut i = 0;
                while i + 16 < n {
                    if chunk(i) {
                        return false;
                    }
                    i += 16;
                }
                return !chunk(n - 16);
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let mut i = 0;
            while i + 8 < n {
                if flagged(word(i)) {
                    return false;
                }
                i += 8;
            }
            return !flagged(word(n - 8));
        }
    }
    if n >= 8 {
        return !flagged(word(0)) && !flagged(word(n - 8));
    }
    if n >= 4 {
        let half = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"));
        return !flagged(u64::from(half(0)) | u64::from(half(n - 4)) << 32);
    }
    b.iter()
        .all(|&c| !NEEDS_ESCAPE[c as usize] && (c < 0x80 || !ascii_only))
}

/// Copies `src` to `dst`: short slices with two overlapping loads and
/// stores rather than a call to `memcpy`.
///
/// # Safety
///
/// `dst` must be valid for writes of `src.len()` bytes that don't overlap
/// `src`.
#[inline]
unsafe fn copy_to(src: &[u8], dst: *mut u8) {
    let n = src.len();
    let s = src.as_ptr();
    // SAFETY: every read is within `src`, every write within `dst[..n]`.
    unsafe {
        if n > 32 {
            std::ptr::copy_nonoverlapping(s, dst, n);
        } else if n >= 16 {
            let a = s.cast::<[u8; 16]>().read_unaligned();
            let z = s.add(n - 16).cast::<[u8; 16]>().read_unaligned();
            dst.cast::<[u8; 16]>().write_unaligned(a);
            dst.add(n - 16).cast::<[u8; 16]>().write_unaligned(z);
        } else if n >= 8 {
            let a = s.cast::<u64>().read_unaligned();
            let z = s.add(n - 8).cast::<u64>().read_unaligned();
            dst.cast::<u64>().write_unaligned(a);
            dst.add(n - 8).cast::<u64>().write_unaligned(z);
        } else if n >= 4 {
            let a = s.cast::<u32>().read_unaligned();
            let z = s.add(n - 4).cast::<u32>().read_unaligned();
            dst.cast::<u32>().write_unaligned(a);
            dst.add(n - 4).cast::<u32>().write_unaligned(z);
        } else if n > 0 {
            *dst = *s;
            *dst.add(n / 2) = *s.add(n / 2);
            *dst.add(n - 1) = *s.add(n - 1);
        }
    }
}

/// How many bytes of the source [`write_json_string_raw`] needs for a
/// string of `len` bytes: whole 16-byte blocks, at least one.
#[inline]
pub fn raw_read_len(len: usize) -> usize {
    len.next_multiple_of(16).max(16)
}

/// [`write_json_string`] of `s` (a string from simdjson's tape), copying its
/// text in the source, `raw` (from just after its opening quote, with at
/// least [`raw_read_len`] bytes), when nothing needs escaping: then the
/// source spells it without escapes (the first backslash of an escape
/// would be among its first `s.len()` bytes), so its first `s.len()` bytes
/// are `s`. It checks and copies whole 16-byte blocks, writing past the end
/// into the spare capacity of `out`, so that a short string takes one load
/// and one store rather than branches on its length.
#[inline]
pub fn write_json_string_raw(raw: &[u8], s: &str, ascii_only: bool, out: &mut Vec<u8>) {
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        const INDEX: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let n = s.len();
        if raw.len() < raw_read_len(n) {
            return write_json_string(s, ascii_only, out);
        }
        let start = out.len();
        out.reserve(raw_read_len(n) + 2);
        // SAFETY: NEON is part of the aarch64 baseline; each load reads a
        // block of `raw` (`i + 16 <= raw_read_len(n) <= raw.len()`); the
        // stores write into the capacity reserved after `start`, and the
        // length only grows to cover the string and its quotes.
        unsafe {
            let src = raw.as_ptr();
            let dst = out.as_mut_ptr().add(start);
            *dst = b'"';
            let index = vld1q_u8(INDEX.as_ptr());
            let mut i = 0;
            loop {
                let v = vld1q_u8(src.add(i));
                let control = vcltq_u8(v, vdupq_n_u8(0x20));
                let quote = vceqq_u8(v, vdupq_n_u8(b'"'));
                let backslash = vceqq_u8(v, vdupq_n_u8(b'\\'));
                let del = vceqq_u8(v, vdupq_n_u8(0x7F));
                let mut m = vorrq_u8(vorrq_u8(control, quote), vorrq_u8(backslash, del));
                if ascii_only {
                    m = vorrq_u8(m, vcgeq_u8(v, vdupq_n_u8(0x80)));
                }
                // (Only the string's own bytes count.)
                let inside = vcltq_u8(index, vdupq_n_u8((n - i).min(16) as u8));
                if vmaxvq_u8(vandq_u8(m, inside)) != 0 {
                    // Something to escape (nothing was written yet).
                    return write_json_string(s, ascii_only, out);
                }
                vst1q_u8(dst.add(1 + i), v);
                i += 16;
                if i >= n {
                    break;
                }
            }
            *dst.add(n + 1) = b'"';
            out.set_len(start + n + 2);
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = raw;
        write_json_string(s, ascii_only, out)
    }
}

/// Port of `jvp_dump_string`: a quoted, escaped JSON string.
pub fn write_json_string(s: &str, ascii_only: bool, out: &mut Vec<u8>) {
    let bytes = s.as_bytes();
    out.reserve(bytes.len() + 2);
    if is_plain(bytes, ascii_only) {
        let len = out.len();
        // SAFETY: the `bytes.len() + 2` bytes after `len` are reserved, and
        // all of them are written before the length covers them.
        unsafe {
            let dst = out.as_mut_ptr().add(len);
            *dst = b'"';
            copy_to(bytes, dst.add(1));
            *dst.add(bytes.len() + 1) = b'"';
            out.set_len(len + bytes.len() + 2);
        }
        return;
    }
    out.push(b'"');
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        // Skip the bytes that need no escaping, then look at the first one
        // that may.
        i = skip_plain(bytes, i, ascii_only);
        if i >= bytes.len() {
            break;
        }
        let b = bytes[i];
        if b < 0x80 {
            if !NEEDS_ESCAPE[b as usize] {
                i += 1;
                continue;
            }
            out.extend_from_slice(&bytes[start..i]);
            match b {
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                0x08 => out.extend_from_slice(b"\\b"),
                b'\t' => out.extend_from_slice(b"\\t"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\n' => out.extend_from_slice(b"\\n"),
                0x0C => out.extend_from_slice(b"\\f"),
                _ => push_u_escape(out, b as u32),
            }
            i += 1;
            start = i;
        } else if ascii_only {
            out.extend_from_slice(&bytes[start..i]);
            let c = s[i..].chars().next().expect("char boundary");
            let c32 = c as u32;
            if c32 <= 0xFFFF {
                push_u_escape(out, c32);
            } else {
                let c = c32 - 0x10000;
                push_u_escape(out, 0xD800 | ((c & 0xFFC00) >> 10));
                push_u_escape(out, 0xDC00 | (c & 0x003FF));
            }
            i += c.len_utf8();
            start = i;
        } else {
            i += 1;
        }
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

struct Printer<'o> {
    opts: &'o DumpOptions,
    /// Spaces per level (pretty) — ignored with tabs.
    spaces: usize,
    pretty: bool,
    tab: bool,
    /// `JV_PRINT_REFCOUNT`
    refcounts: bool,
    /// `QJ_JQ_COMPAT=1`: how many `jv_dump_term` frames jq's stack has room
    /// for here; [`u64::MAX`] otherwise, so the test costs one comparison.
    frames: u64,
}

impl Printer<'_> {
    /// `put_refcnt`: ` (<refcount>)`. jq prints `jv_get_refcnt(x) - 1`
    /// because `jv_dump_term` owns an extra copy of `x`; this printer
    /// borrows, so the plain count is the same number.
    #[inline]
    fn put_refcnt(&self, x: &Value, out: &mut Vec<u8>) {
        if self.refcounts {
            out.extend_from_slice(b" (");
            out.extend_from_slice(itoa::Buffer::new().format(x.refcount()).as_bytes());
            out.push(b')');
        }
    }

    fn indent(&self, n: usize, out: &mut Vec<u8>) {
        if self.tab {
            out.resize(out.len() + n, b'\t');
        } else {
            out.resize(out.len() + n * self.spaces, b' ');
        }
    }

    /// Port of `jv_dump_term`.
    fn term<S: Sink>(&self, x: &Value, indent: usize, sink: &mut S) -> io::Result<()> {
        // QJ_JQ_COMPAT=1: jq recurses here once a level, and on a small stack
        // it runs out before `MAX_PRINT_DEPTH` can stop it.
        if indent as u64 >= self.frames {
            crate::compat::die_of_stack_overflow();
        }
        let color = self.opts.colors.as_ref().map(|c| c.for_value(x));
        if let Some(c) = color {
            sink.buf().extend_from_slice(c);
        }
        if indent > MAX_PRINT_DEPTH {
            sink.buf().extend_from_slice(b"<skipped: too deep>");
        } else {
            match x {
                Value::Null => sink.buf().extend_from_slice(b"null"),
                Value::Bool(false) => sink.buf().extend_from_slice(b"false"),
                Value::Bool(true) => sink.buf().extend_from_slice(b"true"),
                Value::Number(n) => self.number(n, indent, sink)?,
                Value::String(s) => {
                    write_json_string(s.as_str(), self.opts.ascii, sink.buf());
                    self.put_refcnt(x, sink.buf());
                }
                Value::Array(a) => {
                    if a.is_empty() {
                        sink.buf().extend_from_slice(b"[]");
                    } else {
                        sink.buf().push(b'[');
                        for (i, elem) in a.iter().enumerate() {
                            let out = sink.buf();
                            if i != 0 {
                                if let Some(c) = color {
                                    out.extend_from_slice(c);
                                }
                                out.push(b',');
                            }
                            if color.is_some() {
                                out.extend_from_slice(COLRESET);
                            }
                            if self.pretty {
                                out.push(b'\n');
                                self.indent(indent + 1, out);
                            }
                            self.term(elem, indent + 1, sink)?;
                            sink.checkpoint()?;
                        }
                        let out = sink.buf();
                        if self.pretty {
                            out.push(b'\n');
                            self.indent(indent, out);
                        }
                        if let Some(c) = color {
                            out.extend_from_slice(c);
                        }
                        out.push(b']');
                        self.put_refcnt(x, out);
                    }
                }
                Value::Object(o) => {
                    if o.is_empty() {
                        sink.buf().extend_from_slice(b"{}");
                    } else {
                        sink.buf().push(b'{');
                        if self.opts.sort_keys {
                            let mut keys: Vec<_> = o.iter().collect();
                            keys.sort_by(|a, b| a.0.cmp(b.0));
                            for (i, (k, v)) in keys.into_iter().enumerate() {
                                self.field(i == 0, k.as_str(), v, color, indent, sink)?;
                            }
                        } else {
                            for (i, (k, v)) in o.iter().enumerate() {
                                self.field(i == 0, k.as_str(), v, color, indent, sink)?;
                            }
                        }
                        let out = sink.buf();
                        if self.pretty {
                            out.push(b'\n');
                            self.indent(indent, out);
                        }
                        if let Some(c) = color {
                            out.extend_from_slice(c);
                        }
                        out.push(b'}');
                        self.put_refcnt(x, out);
                    }
                }
            }
        }
        if color.is_some() {
            sink.buf().extend_from_slice(COLRESET);
        }
        Ok(())
    }

    fn number<S: Sink>(&self, n: &Number, _indent: usize, sink: &mut S) -> io::Result<()> {
        // NaN is dumped as a null term (with the null color when colored,
        // since jv_dump_term recurses on jv_null()).
        if n.is_nan() {
            if let Some(colors) = &self.opts.colors {
                let out = sink.buf();
                out.extend_from_slice(colors.for_value(&Value::Null));
                out.extend_from_slice(b"null");
                out.extend_from_slice(COLRESET);
            } else {
                sink.buf().extend_from_slice(b"null");
            }
            return Ok(());
        }
        n.write_json(sink.buf());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn field<S: Sink>(
        &self,
        first: bool,
        key: &str,
        value: &Value,
        color: Option<&[u8]>,
        indent: usize,
        sink: &mut S,
    ) -> io::Result<()> {
        let out = sink.buf();
        if !first {
            if let Some(c) = color {
                out.extend_from_slice(c);
            }
            out.push(b',');
        }
        if color.is_some() {
            out.extend_from_slice(COLRESET);
        }
        if self.pretty {
            out.push(b'\n');
            self.indent(indent + 1, out);
        }
        if let Some(colors) = &self.opts.colors {
            out.extend_from_slice(colors.field());
        }
        write_json_string(key, self.opts.ascii, out);
        if color.is_some() {
            out.extend_from_slice(COLRESET);
        }
        if let Some(c) = color {
            out.extend_from_slice(c);
        }
        out.push(b':');
        if color.is_some() {
            out.extend_from_slice(COLRESET);
        }
        if self.pretty {
            out.push(b' ');
        }
        self.term(value, indent + 1, sink)?;
        sink.checkpoint()
    }
}

/// A printer for `opts`, whose depth compat mode checks against jq's stack as
/// [`Site::Print`](crate::compat::Site::Print) (`main.c`'s output path) or
/// [`Site::Dump`](crate::compat::Site::Dump) (everything that dumps while the
/// program runs, from deeper in jq's stack).
fn printer(opts: &DumpOptions, site: crate::compat::Site) -> Printer<'_> {
    let (pretty, tab, spaces) = match opts.indent {
        Indent::Compact => (false, false, 0),
        Indent::Spaces(n) => (true, false, n as usize),
        Indent::Tab => (true, true, 0),
    };
    Printer {
        opts,
        spaces,
        pretty,
        tab,
        refcounts: false,
        frames: crate::compat::frames_available(site),
    }
}

/// Appends the dump of `v` to `out` (`jv_dump_term` into a buffer).
pub fn dump_to_vec(v: &Value, opts: &DumpOptions, out: &mut Vec<u8>) {
    printer(opts, crate::compat::Site::Dump)
        .term(v, 0, out)
        .expect("writing to a Vec cannot fail");
}

/// [`dump_to_vec`] into a [`DumpSink`], which may write the dump out as it
/// grows.
pub fn dump_to_sink<S: DumpSink>(v: &Value, opts: &DumpOptions, sink: &mut S) {
    printer(opts, crate::compat::Site::Print)
        .term(v, 0, &mut Streaming(sink))
        .expect("a DumpSink cannot fail");
}

/// `jv_dumpf`: writes the dump of `v` to `w` (no trailing newline).
pub fn dump<W: Write>(v: &Value, opts: &DumpOptions, w: &mut W) -> io::Result<()> {
    let mut sink = WriterSink {
        buf: Vec::with_capacity(4096),
        w,
    };
    printer(opts, crate::compat::Site::Dump).term(v, 0, &mut sink)?;
    sink.w.write_all(&sink.buf)
}

/// `jv_dump(v, flags | JV_PRINT_REFCOUNT)`, as `--debug-trace` prints stack
/// values: like [`dump`], with ` (<n>)` after every string and every
/// non-empty array or object, `n` being its [`Value::refcount`]. That is the
/// number jq prints when the caller holds the references jq's VM would.
pub fn dump_refcounted<W: Write>(v: &Value, opts: &DumpOptions, w: &mut W) -> io::Result<()> {
    let mut sink = WriterSink {
        buf: Vec::with_capacity(256),
        w,
    };
    let mut p = printer(opts, crate::compat::Site::Dump);
    p.refcounts = true;
    p.term(v, 0, &mut sink)?;
    sink.w.write_all(&sink.buf)
}

/// `jv_dump_string`: the dump of `v` as a string.
pub fn dump_string(v: &Value, opts: &DumpOptions) -> String {
    let mut out = Vec::new();
    dump_to_vec(v, opts, &mut out);
    // The printer only emits valid UTF-8 (strings are valid, escapes ASCII).
    String::from_utf8(out).expect("valid UTF-8")
}

/// Port of `jv_dump_string_trunc(x, outbuf, bufsize)`: the compact dump,
/// cut to at most `bufsize - 1` bytes with a trailing `...` when it does
/// not fit (without splitting a UTF-8 sequence). jq uses `bufsize` 15 or 30
/// in error messages.
pub fn dump_string_trunc(v: &Value, bufsize: usize) -> String {
    let mut sink = TruncSink {
        buf: Vec::new(),
        limit: bufsize,
    };
    // An early stop only happens once the output is known to be too long.
    let _ = printer(&DumpOptions::default(), crate::compat::Site::Dump).term(v, 0, &mut sink);
    let mut out = sink.buf;
    // strlen(): the dump never contains NUL (it is escaped).
    let len = out.len();
    if bufsize == 0 {
        return String::new();
    }
    if len > bufsize - 1 && bufsize >= 4 {
        out.truncate(bufsize);
        // Indicate truncation with '...' without breaking UTF-8.
        let mut cut = bufsize - 4;
        if let Some(s) = super::unicode::utf8_backtrack(&out, bufsize - 4, 0, None) {
            cut = s;
        }
        out.truncate(cut);
        out.extend_from_slice(b"...");
    } else {
        out.truncate(bufsize - 1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `jvp_dump_string` one character at a time.
    fn reference(s: &str, ascii_only: bool) -> Vec<u8> {
        let mut out = vec![b'"'];
        for c in s.chars() {
            let c32 = c as u32;
            match c {
                '"' => out.extend_from_slice(b"\\\""),
                '\\' => out.extend_from_slice(b"\\\\"),
                '\u{8}' => out.extend_from_slice(b"\\b"),
                '\t' => out.extend_from_slice(b"\\t"),
                '\r' => out.extend_from_slice(b"\\r"),
                '\n' => out.extend_from_slice(b"\\n"),
                '\u{c}' => out.extend_from_slice(b"\\f"),
                _ if c32 < 0x20 || c32 == 0x7F => push_u_escape(&mut out, c32),
                _ if c32 >= 0x80 && ascii_only => {
                    if c32 <= 0xFFFF {
                        push_u_escape(&mut out, c32);
                    } else {
                        let c = c32 - 0x10000;
                        push_u_escape(&mut out, 0xD800 | ((c & 0xFFC00) >> 10));
                        push_u_escape(&mut out, 0xDC00 | (c & 0x003FF));
                    }
                }
                _ => {
                    let mut b = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                }
            }
        }
        out.push(b'"');
        out
    }

    #[test]
    fn string_escaping_matches_the_character_loop() {
        // Characters around every boundary the word-at-a-time scan tests.
        let alphabet: Vec<char> = (0u32..0x82)
            .chain([
                0x1F, 0x20, 0x21, 0x22, 0x5C, 0x7E, 0x7F, 0xFF, 0x100, 0x7FF, 0x800,
            ])
            .chain([0xFFFD, 0xFFFF, 0x10000, 0x1F600, 0x10FFFF])
            .filter_map(char::from_u32)
            .collect();
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let len = (next() % 120) as usize;
            // Mostly plain text, so long clean runs occur.
            let s: String = (0..len)
                .map(|_| {
                    let r = next();
                    if r % 4 == 0 {
                        alphabet[(r >> 8) as usize % alphabet.len()]
                    } else {
                        (b'a' + (r >> 8) as u8 % 26) as char
                    }
                })
                .collect();
            for ascii in [false, true] {
                let mut out = Vec::new();
                write_json_string(&s, ascii, &mut out);
                assert_eq!(out, reference(&s, ascii), "{s:?} ascii={ascii}");
            }
            // As a source text would spell it (escaped at random, even
            // where no escape is needed), followed by more text: printed
            // from the source, it must come out the same.
            let mut src = String::new();
            for c in s.chars() {
                let r = next();
                match c {
                    '"' | '\\' => src.extend(['\\', c]),
                    c if (c as u32) < 0x20 => src.push_str(&format!("\\u{:04x}", c as u32)),
                    '/' if r % 2 == 0 => src.push_str("\\/"),
                    c if r % 16 == 0 && (c as u32) < 0x10000 => {
                        src.push_str(&format!("\\u{:04X}", c as u32))
                    }
                    c => src.push(c),
                }
            }
            src.push_str(["\"", "\",\"x\"", "\"\"\"", "\\\"\"]"][(next() % 4) as usize]);
            for ascii in [false, true] {
                let mut out = b"[".to_vec();
                write_json_string_raw(src.as_bytes(), &s, ascii, &mut out);
                assert_eq!(out[1..], reference(&s, ascii), "{src:?} ascii={ascii}");
                // Also with the source cut short (not enough to read ahead).
                let mut out = Vec::new();
                let cut = &src.as_bytes()[..src.len().min(raw_read_len(s.len()) - 1)];
                write_json_string_raw(cut, &s, ascii, &mut out);
                assert_eq!(out, reference(&s, ascii), "{src:?} ascii={ascii} (cut)");
            }
        }
    }
}
