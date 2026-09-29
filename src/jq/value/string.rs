//! jq strings: port of the string parts of `jv.c`.
//!
//! A jq string is always valid UTF-8 (invalid input bytes are replaced with
//! U+FFFD using jq's decoder, see [`super::unicode`]) and may contain NUL.
//! Strings are reference counted; appending to a uniquely owned string grows
//! it in place (`jvp_string_append`), which keeps repeated concatenation in
//! `reduce`/`add` linear.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

use super::unicode;
use super::{Array, Value};

/// A jq string value (`JV_KIND_STRING`).
#[derive(Clone, Default)]
pub struct Str(Rc<String>);

impl Str {
    /// An empty string.
    pub fn new() -> Str {
        Str::default()
    }

    /// `jv_string_sized`: builds a string from bytes, replacing invalid UTF-8
    /// with U+FFFD exactly as jq does.
    pub fn from_bytes(bytes: &[u8]) -> Str {
        Str(Rc::new(unicode::decode_lossy(bytes)))
    }

    /// The string contents.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The string contents as bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// `jv_string_length_bytes`.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `jv_string_length_codepoints`.
    pub fn codepoint_len(&self) -> usize {
        self.0.chars().count()
    }

    /// Mutable access, copying the contents first if they are shared
    /// (jq's refcount-1 in-place mutation).
    #[inline]
    pub fn make_mut(&mut self) -> &mut String {
        Rc::make_mut(&mut self.0)
    }

    /// Whether this is the only reference to the underlying buffer.
    pub fn is_unique(&self) -> bool {
        Rc::strong_count(&self.0) == 1 && Rc::weak_count(&self.0) == 0
    }

    /// Pointer identity (as used by `jv_equal`'s fast path and `jv_identical`).
    #[inline]
    pub fn ptr_eq(&self, other: &Str) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// `jv_string_append_str` / `jvp_string_append` for valid text.
    pub fn push_str(&mut self, s: &str) {
        self.make_mut().push_str(s);
    }

    /// `jv_string_append_buf`: appends bytes, replacing invalid UTF-8.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        unicode::push_lossy(self.make_mut(), bytes);
    }

    /// `jv_string_append_codepoint`. Non-scalar values (surrogates, values
    /// above U+10FFFF) become U+FFFD, which is what a jq string ends up
    /// holding for them.
    pub fn push_codepoint(&mut self, c: u32) {
        self.make_mut()
            .push(char::from_u32(c).unwrap_or('\u{FFFD}'));
    }

    /// `jv_string_concat` (in place when `self` is uniquely owned).
    pub fn concat(&mut self, other: &Str) {
        self.push_str(other.as_str());
    }

    /// Consumes the string, returning the owned contents (copying only if
    /// shared).
    pub fn into_string(self) -> String {
        Rc::try_unwrap(self.0).unwrap_or_else(|rc| (*rc).clone())
    }

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
        &self.0
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Str {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Str {
    fn eq(&self, other: &Str) -> bool {
        self.ptr_eq(other) || self.0.as_bytes() == other.0.as_bytes()
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
    fn from(s: &str) -> Str {
        Str(Rc::new(s.to_owned()))
    }
}

impl From<String> for Str {
    fn from(s: String) -> Str {
        Str(Rc::new(s))
    }
}

impl From<&String> for Str {
    fn from(s: &String) -> Str {
        Str(Rc::new(s.clone()))
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
    fn in_place_append() {
        let mut a = s("x");
        let p = a.as_ptr();
        a.make_mut().reserve(64);
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
