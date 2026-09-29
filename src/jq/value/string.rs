//! jq strings: port of the string parts of `jv.c`.
//!
//! A jq string is always valid UTF-8 (invalid input bytes are replaced with
//! U+FFFD using jq's decoder, see [`super::unicode`]) and may contain NUL.
//! Strings are reference counted; appending to a uniquely owned string grows
//! it in place (`jvp_string_append`), which keeps repeated concatenation in
//! `reduce`/`add` linear.
//!
//! Like jq's `jvp_string`, a string is one allocation: a header (reference
//! count, length, capacity) followed by the bytes. So a string
//! costs one `malloc` (an `Rc<String>` would cost two), and [`Str`] is a
//! thin pointer.

use std::alloc::{self, Layout};
use std::borrow::Borrow;
use std::cell::Cell;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::ptr::NonNull;

use super::unicode;
use super::{Array, Value};

/// The header in front of a string's bytes.
#[repr(C)]
struct Header {
    strong: Cell<usize>,
    /// [`hash_key`] of the contents once computed (0 until then; the hash
    /// is never 0).
    hash: Cell<u64>,
    len: usize,
    cap: usize,
}

const HEADER: usize = std::mem::size_of::<Header>();

/// A jq string value (`JV_KIND_STRING`).
pub struct Str(NonNull<Header>);

#[inline]
fn layout(cap: usize) -> Layout {
    match HEADER.checked_add(cap) {
        Some(size) if size <= isize::MAX as usize => {
            // SAFETY: the size doesn't overflow isize and the alignment is a
            // power of two.
            unsafe { Layout::from_size_align_unchecked(size, std::mem::align_of::<Header>()) }
        }
        _ => panic!("string too long"),
    }
}

impl Str {
    /// A uniquely owned string of length 0 with room for `cap` bytes.
    fn alloc(cap: usize) -> Str {
        let layout = layout(cap);
        // SAFETY: the layout has a non-zero size (the header).
        let p = unsafe { alloc::alloc(layout) }.cast::<Header>();
        let Some(p) = NonNull::new(p) else {
            alloc::handle_alloc_error(layout)
        };
        // SAFETY: `p` is valid for writes of the header.
        unsafe {
            p.as_ptr().write(Header {
                strong: Cell::new(1),
                hash: Cell::new(0),
                len: 0,
                cap,
            });
        }
        Str(p)
    }

    #[inline]
    fn header(&self) -> &Header {
        // SAFETY: the pointer is live for as long as `self` holds a reference.
        unsafe { self.0.as_ref() }
    }

    #[inline]
    fn data(&self) -> *mut u8 {
        // SAFETY: the bytes follow the header in the same allocation.
        unsafe { self.0.as_ptr().cast::<u8>().add(HEADER) }
    }

    /// A string holding `s`.
    #[inline]
    fn copy_of(s: &[u8]) -> Str {
        let r = Str::alloc(s.len());
        // SAFETY: the allocation has room for `s.len()` bytes after the header,
        // and nothing else refers to it yet.
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), r.data(), s.len());
            (*r.0.as_ptr()).len = s.len();
        }
        r
    }

    /// An empty string.
    pub fn new() -> Str {
        Str::alloc(0)
    }

    /// An empty string with room for `cap` bytes.
    pub fn with_capacity(cap: usize) -> Str {
        Str::alloc(cap)
    }

    /// `jv_string_sized`: builds a string from bytes, replacing invalid UTF-8
    /// with U+FFFD exactly as jq does.
    pub fn from_bytes(bytes: &[u8]) -> Str {
        match std::str::from_utf8(bytes) {
            Ok(s) => Str::from(s),
            Err(_) => Str::from(unicode::decode_lossy(bytes)),
        }
    }

    /// The string contents.
    #[inline]
    pub fn as_str(&self) -> &str {
        // SAFETY: the bytes are valid UTF-8 (every constructor and mutator
        // only stores valid UTF-8).
        unsafe { std::str::from_utf8_unchecked(self.as_bytes()) }
    }

    /// The string contents as bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: the first `len` bytes after the header are initialized.
        unsafe { std::slice::from_raw_parts(self.data(), self.header().len) }
    }

    /// `jv_string_length_bytes`.
    #[inline]
    pub fn len(&self) -> usize {
        self.header().len
    }

    /// Whether the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `jv_string_length_codepoints`.
    pub fn codepoint_len(&self) -> usize {
        self.as_str().chars().count()
    }

    /// The contents as C sees them through `jv_string_value`: up to the
    /// first NUL. Use this wherever jq formats a string with `%s` (error
    /// messages) or passes it to a C string function.
    pub fn as_c_str(&self) -> &str {
        let s = self.as_str();
        match memchr::memchr(0, s.as_bytes()) {
            Some(i) => &s[..i],
            None => s,
        }
    }

    /// Whether this is the only reference to the underlying buffer.
    #[inline]
    pub fn is_unique(&self) -> bool {
        self.header().strong.get() == 1
    }

    /// The number of references to the buffer (`jv_get_refcnt`).
    #[inline]
    pub fn refcount(&self) -> usize {
        self.header().strong.get()
    }

    /// Pointer identity (as used by `jv_equal`'s fast path and `jv_identical`).
    #[inline]
    pub fn ptr_eq(&self, other: &Str) -> bool {
        self.0 == other.0
    }

    /// Makes `self` uniquely owned with room for `extra` more bytes: in place
    /// when it's unique and fits, otherwise into a new buffer (jq's
    /// `jvp_string_append` sizing: twice the new length, at least 32).
    fn reserve_unique(&mut self, extra: usize) {
        let len = self.len();
        let need = len.checked_add(extra).expect("string too long");
        if self.is_unique() {
            if need <= self.header().cap {
                return;
            }
            let cap = need.saturating_mul(2).max(32);
            let old = layout(self.header().cap);
            // SAFETY: the block was allocated with `old`, the new size is
            // valid (`layout` checks it), and it's uniquely owned, so moving
            // it invalidates no other reference.
            let p = unsafe { alloc::realloc(self.0.as_ptr().cast(), old, layout(cap).size()) };
            let Some(p) = NonNull::new(p.cast::<Header>()) else {
                alloc::handle_alloc_error(layout(cap))
            };
            self.0 = p;
            // SAFETY: unique, so nothing else observes the header.
            unsafe { (*self.0.as_ptr()).cap = cap };
        } else {
            let cap = if extra == 0 {
                len
            } else {
                need.saturating_mul(2).max(32)
            };
            let mut copy = Str::alloc(cap);
            // SAFETY: `copy` has room for `len` bytes and is unique.
            unsafe {
                std::ptr::copy_nonoverlapping(self.data(), copy.data(), len);
                (*copy.0.as_ptr()).len = len;
            }
            std::mem::swap(self, &mut copy);
        }
    }

    /// Appends bytes that keep the contents valid UTF-8.
    #[inline]
    fn push_utf8(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            // jq unshares even for an empty append (identity is observable).
            self.reserve_unique(0);
            return;
        }
        self.reserve_unique(bytes.len());
        let len = self.len();
        // SAFETY: `reserve_unique` made the buffer unique with room for
        // `bytes.len()` more bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.data().add(len), bytes.len());
            let h = &mut *self.0.as_ptr();
            h.len = len + bytes.len();
            h.hash.set(0);
        }
    }

    /// `jv_string_append_str` / `jvp_string_append` for valid text.
    pub fn push_str(&mut self, s: &str) {
        self.push_utf8(s.as_bytes());
    }

    /// `jv_string_append_buf`: appends bytes, replacing invalid UTF-8.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        match std::str::from_utf8(bytes) {
            Ok(s) => self.push_str(s),
            Err(_) => {
                let mut fixed = String::with_capacity(bytes.len() + 8);
                unicode::push_lossy(&mut fixed, bytes);
                self.push_str(&fixed);
            }
        }
    }

    /// `jv_string_append_codepoint`. Non-scalar values (surrogates, values
    /// above U+10FFFF) become U+FFFD, which is what a jq string ends up
    /// holding for them.
    pub fn push_codepoint(&mut self, c: u32) {
        let mut buf = [0u8; 4];
        let c = char::from_u32(c).unwrap_or('\u{FFFD}');
        self.push_str(c.encode_utf8(&mut buf));
    }

    /// `jv_string_concat` (in place when `self` is uniquely owned).
    pub fn concat(&mut self, other: &Str) {
        // (`other` can't share `self`'s buffer while `self` is unique, so an
        // in-place append never moves bytes `other` points into.)
        self.push_str(other.as_str());
    }

    /// Consumes the string, returning the contents.
    pub fn into_string(self) -> String {
        self.as_str().to_owned()
    }

    /// [`hash_key`] of the contents, cached in the string (objects index
    /// their keys by it).
    #[inline]
    pub(crate) fn key_hash(&self) -> u64 {
        let h = self.header().hash.get();
        if h != 0 {
            return h;
        }
        let h = hash_key(self.as_bytes());
        self.header().hash.set(h);
        h
    }

    /// Records `h`, which must be [`hash_key`] of the contents, as the
    /// cached hash (for callers that hashed the bytes already).
    #[inline]
    pub(crate) fn set_key_hash(&self, h: u64) {
        debug_assert_eq!(h, hash_key(self.as_bytes()));
        self.header().hash.set(h);
    }
}

/// The hash of an object key's bytes (see [`Str::key_hash`]); never 0.
/// The seed is random per process.
#[inline]
pub(crate) fn hash_key(bytes: &[u8]) -> u64 {
    use std::hash::BuildHasher;
    use std::sync::OnceLock;
    static SEED: OnceLock<u64> = OnceLock::new();
    let seed =
        *SEED.get_or_init(|| foldhash::fast::RandomState::default().hash_one(0x51_7c_c1_b7u64));
    let h = foldhash::fast::FixedState::with_seed(seed).hash_one(bytes);
    if h == 0 { 1 } else { h }
}

impl Clone for Str {
    #[inline]
    fn clone(&self) -> Str {
        let h = self.header();
        let n = h.strong.get();
        if n == usize::MAX {
            std::process::abort();
        }
        h.strong.set(n + 1);
        Str(self.0)
    }
}

impl Drop for Str {
    #[inline]
    fn drop(&mut self) {
        let h = self.header();
        let n = h.strong.get();
        if n == 1 {
            let cap = h.cap;
            // SAFETY: the last reference: the block was allocated with
            // `layout(cap)` and nothing refers to it anymore.
            unsafe { alloc::dealloc(self.0.as_ptr().cast(), layout(cap)) };
        } else {
            h.strong.set(n - 1);
        }
    }
}

impl Default for Str {
    fn default() -> Str {
        Str::new()
    }
}

impl Str {
    /// `jv_string_slice`: codepoint-indexed slice. `start`/`end` are clamped
    /// like `jvp_clamp_slice_params` (against the byte length, as jq does).
    pub fn slice(&self, start: i64, end: i64) -> Str {
        let s = self.as_str();
        let len = s.len() as i64;
        let (start, end) = clamp_slice_params(len, start, end);
        // Byte offset corresponding to `start` codepoints.
        let mut chars = s.char_indices();
        let mut i = 0;
        let mut p = 0usize;
        while i < start {
            match chars.next() {
                Some((_, c)) => p += c.len_utf8(),
                None => return Str::new(),
            }
            i += 1;
        }
        let mut e = p;
        while i < end {
            match chars.next() {
                Some((_, c)) => e += c.len_utf8(),
                None => {
                    e = s.len();
                    break;
                }
            }
            i += 1;
        }
        // jq always allocates a new string here (identity is observable).
        Str::from(&s[p..e])
    }

    /// `jv_string_indexes`: codepoint offsets of every (possibly
    /// overlapping) occurrence of `needle`. An empty needle gives `[]`.
    pub fn indexes(&self, needle: &Str) -> Array {
        let mut out = Array::new();
        let hay = self.as_bytes();
        let nb = needle.as_bytes();
        if nb.is_empty() {
            return out;
        }
        let mut n: usize = 0; // codepoints before `lp`
        let mut lp = 0usize;
        let mut p = 0usize;
        let finder = memchr::memmem::Finder::new(nb);
        while let Some(off) = finder.find(&hay[p..]) {
            let at = p + off;
            while lp < at {
                lp += unicode::utf8_decode_length(hay[lp]);
                n += 1;
            }
            out.push(Value::from(n as f64));
            p = at + 1;
        }
        out
    }

    /// `jv_string_split`: splits on a byte substring; an empty separator
    /// splits into codepoints. A string ending with the separator yields a
    /// trailing `""`; the empty string yields `[]`.
    pub fn split(&self, sep: &Str) -> Array {
        let mut out = Array::new();
        let s = self.as_str();
        if sep.is_empty() {
            for c in s.chars() {
                let mut buf = [0u8; 4];
                out.push(Value::String(Str::from(&*c.encode_utf8(&mut buf))));
            }
            return out;
        }
        let bytes = s.as_bytes();
        let sepb = sep.as_bytes();
        let finder = memchr::memmem::Finder::new(sepb);
        let mut p = 0usize;
        while p < bytes.len() {
            let found = finder.find(&bytes[p..]).map(|o| p + o);
            let e = found.unwrap_or(bytes.len());
            // Separator matches are at char boundaries (valid UTF-8 needle).
            out.push(Value::String(Str::from(&s[p..e])));
            if found.is_some() && e + sepb.len() == bytes.len() {
                out.push(Value::String(Str::new()));
            }
            p = e + sepb.len();
        }
        out
    }

    /// `jv_string_explode`: the codepoints as (native) numbers.
    pub fn explode(&self) -> Array {
        let mut out = Array::with_capacity(self.len());
        for c in self.as_str().chars() {
            out.push(Value::from(c as u32 as f64));
        }
        out
    }

    /// `f_string_implode` (builtin.c; `jv_string_implode` asserts instead):
    /// codepoints to a string, with jq's validation errors. Codepoints
    /// outside the Unicode range or in the surrogate range become U+FFFD;
    /// fractional values are truncated.
    pub fn implode(input: &Value) -> Result<Str, super::Error> {
        let arr = match input {
            Value::Array(a) => a,
            _ => {
                return Err(super::Error::msg("implode input must be an array"));
            }
        };
        let mut s = String::with_capacity(arr.len());
        for n in arr.iter() {
            let num = match n {
                Value::Number(num) if !num.is_nan() => num,
                _ => {
                    return Err(super::Error::type_error(
                        n,
                        "can't be imploded, unicode codepoint needs to be numeric",
                    ));
                }
            };
            let nv = double_to_int(num.value());
            // outside codepoint range or in utf16 surrogate pair range
            let c = if !(0..=0x10FFFF).contains(&nv) || (0xD800..=0xDFFF).contains(&nv) {
                0xFFFD
            } else {
                nv as u32
            };
            s.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
        }
        Ok(Str::from(s))
    }

    /// `jv_string_repeat` (with jq's own argument conventions: `n < 0`
    /// gives `null`, a result of `INT_MAX` bytes or more is an error).
    pub fn repeat(&self, n: i32) -> Result<Value, super::Error> {
        if n < 0 {
            return Ok(Value::Null);
        }
        let len = self.len() as i64;
        let res_len = len * n as i64;
        if res_len >= i32::MAX as i64 {
            return Err(super::Error::msg("Repeat string result too long"));
        }
        if res_len == 0 {
            return Ok(Value::String(Str::new()));
        }
        Ok(Value::String(Str::from(self.as_str().repeat(n as usize))))
    }
}

/// C's `(int)` conversion of a double as it behaves on jq's supported
/// platforms for the ranges that matter: truncation toward zero, saturating
/// out-of-range values (arm64 semantics; on x86 out-of-range values become
/// `INT_MIN`, which every caller treats the same way).
pub(crate) fn double_to_int(d: f64) -> i64 {
    if d.is_nan() { 0 } else { (d as i32) as i64 }
}

/// Port of `jvp_clamp_slice_params`.
pub(crate) fn clamp_slice_params(len: i64, start: i64, end: i64) -> (i64, i64) {
    let mut start = start;
    let mut end = end;
    if start < 0 {
        start += len;
    }
    if end < 0 {
        end += len;
    }
    if start < 0 {
        start = 0;
    }
    if start > len {
        start = len;
    }
    if end > len {
        end = len;
    }
    if end < start {
        end = start;
    }
    (start, end)
}

impl Deref for Str {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for Str {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for Str {
    #[inline]
    fn eq(&self, other: &Str) -> bool {
        self.ptr_eq(other) || self.as_bytes() == other.as_bytes()
    }
}

impl Eq for Str {}

impl PartialEq<str> for Str {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Str {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// jq's `string_cmp`: bytewise `memcmp`, then length — the same as `str`'s
/// ordering.
impl Ord for Str {
    fn cmp(&self, other: &Str) -> Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl PartialOrd for Str {
    fn partial_cmp(&self, other: &Str) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Str {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Must agree with `str`'s Hash (objects are looked up by `&str`).
        self.as_str().hash(state)
    }
}

impl From<&str> for Str {
    #[inline]
    fn from(s: &str) -> Str {
        Str::copy_of(s.as_bytes())
    }
}

impl From<String> for Str {
    fn from(s: String) -> Str {
        Str::copy_of(s.as_bytes())
    }
}

impl From<&String> for Str {
    fn from(s: &String) -> Str {
        Str::copy_of(s.as_bytes())
    }
}

impl fmt::Debug for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Str {
        Str::from(x)
    }

    fn strs(a: &Array) -> Vec<String> {
        a.iter()
            .map(|v| match v {
                Value::String(s) => s.as_str().to_owned(),
                _ => panic!("not a string"),
            })
            .collect()
    }

    fn nums(a: &Array) -> Vec<f64> {
        a.iter()
            .map(|v| match v {
                Value::Number(n) => n.value(),
                _ => panic!("not a number"),
            })
            .collect()
    }

    #[test]
    fn split() {
        // `jq -nc '"a,b," | split(",")'` etc.
        assert_eq!(strs(&s("a,b,").split(&s(","))), ["a", "b", ""]);
        assert_eq!(strs(&s("").split(&s(","))), Vec::<String>::new());
        assert_eq!(strs(&s(",").split(&s(","))), ["", ""]);
        assert_eq!(strs(&s("a,,b").split(&s(","))), ["a", "", "b"]);
        assert_eq!(strs(&s("abc").split(&s(""))), ["a", "b", "c"]);
        assert_eq!(strs(&s("a\u{e9}b").split(&s(""))), ["a", "\u{e9}", "b"]);
        assert_eq!(strs(&s("abc").split(&s("abc"))), ["", ""]);
        assert_eq!(strs(&s("xaax").split(&s("aa"))), ["x", "x"]);
    }

    #[test]
    fn indexes() {
        // `jq -nc '"a,b, cd, efg" | indices(", ")'` => [3,7]
        assert_eq!(nums(&s("a,b, cd, efg").indexes(&s(", "))), [3.0, 7.0]);
        // overlapping: `"aaaa" | indices("aa")` => [0,1,2]
        assert_eq!(nums(&s("aaaa").indexes(&s("aa"))), [0.0, 1.0, 2.0]);
        // codepoint offsets: `"éaéa" | indices("a")` => [1,3]
        assert_eq!(nums(&s("\u{e9}a\u{e9}a").indexes(&s("a"))), [1.0, 3.0]);
        assert_eq!(nums(&s("abc").indexes(&s(""))), Vec::<f64>::new());
    }

    #[test]
    fn slice() {
        assert_eq!(s("abcdef").slice(1, 3).as_str(), "bc");
        assert_eq!(s("a\u{e9}\u{20ac}d").slice(1, 3).as_str(), "\u{e9}\u{20ac}");
        assert_eq!(s("abc").slice(-2, 100).as_str(), "bc");
        // start beyond the codepoints (but within the byte length)
        assert_eq!(s("\u{20ac}\u{20ac}").slice(4, 6).as_str(), "");
    }

    #[test]
    fn repeat() {
        assert!(matches!(s("ab").repeat(-1), Ok(Value::Null)));
        assert_eq!(s("ab").repeat(0).unwrap().as_str().unwrap(), "");
        assert_eq!(s("ab").repeat(3).unwrap().as_str().unwrap(), "ababab");
        assert_eq!(
            s("ab").repeat(i32::MAX / 2 + 1).unwrap_err().to_string(),
            "Repeat string result too long"
        );
    }

    #[test]
    fn implode_explode() {
        let e = s("a\u{e9}\u{1F600}").explode();
        assert_eq!(nums(&e), [97.0, 233.0, 128512.0]);
        let back = Str::implode(&Value::Array(e)).unwrap();
        assert_eq!(back.as_str(), "a\u{e9}\u{1F600}");
        let arr: Value = vec![
            Value::from(55296.0),
            Value::from(-1.0),
            Value::from(1114112.0),
            Value::from(65.9),
        ]
        .into();
        assert_eq!(
            Str::implode(&arr).unwrap().as_str(),
            "\u{FFFD}\u{FFFD}\u{FFFD}A"
        );
        // `jq -n '["a"] | implode'`
        let bad: Value = vec![Value::from("a")].into();
        assert_eq!(
            Str::implode(&bad).unwrap_err().to_string(),
            "string (\"a\") can't be imploded, unicode codepoint needs to be numeric"
        );
        assert_eq!(
            Str::implode(&Value::from(1.0)).unwrap_err().to_string(),
            "implode input must be an array"
        );
    }

    #[test]
    fn shared_strings_are_copied_on_write() {
        let a = s("ab");
        assert_eq!(a.refcount(), 1);
        let b = a.clone();
        assert_eq!(a.refcount(), 2);
        assert!(a.ptr_eq(&b));
        // Appending, even nothing, to a shared string makes a new one (jq's
        // jvp_string_append allocates unless the string is unshared).
        let mut c = a.clone();
        c.push_str("");
        assert!(!c.ptr_eq(&a));
        assert_eq!(c.as_str(), "ab");
        assert_eq!(a.refcount(), 2);
        let mut d = b;
        d.push_str("cd");
        assert_eq!((a.as_str(), d.as_str()), ("ab", "abcd"));
        assert_eq!(a.refcount(), 1);
        // A unique string is appended to in place, even with nothing.
        let mut e = s("x");
        let p = e.as_ptr();
        e.push_str("");
        assert_eq!(e.as_ptr(), p);
        // Growth keeps the contents; NUL and multi-byte text survive.
        let mut f = Str::new();
        for i in 0..1000 {
            f.push_str(if i % 2 == 0 { "\u{e9}" } else { "\0" });
        }
        assert_eq!(f.len(), 1500);
        assert_eq!(f.codepoint_len(), 1000);
        f.push_codepoint(0xD800);
        assert!(f.as_str().ends_with('\u{FFFD}'));
        f.push_bytes(b"\xffz");
        assert!(f.as_str().ends_with("\u{FFFD}z"));
    }

    #[test]
    fn in_place_append() {
        let mut a = s("x");
        let p = a.as_ptr();
        a.reserve_unique(64);
        let p2 = a.as_ptr();
        a.push_str("yz");
        assert_eq!(a.as_ptr(), p2);
        assert_ne!(p, std::ptr::null());
        let b = a.clone();
        a.push_str("!");
        assert_eq!(b.as_str(), "xyz");
        assert_eq!(a.as_str(), "xyz!");
    }
}
