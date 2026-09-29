//! jq values: port of jq 1.8.1's `jv.c`, `jv_aux.c`, `jv_print.c`,
//! `jv_parse.c`, `jv_unicode.c`, `jv_file.c` and the output format of
//! `jv_dtoa.c`'s `jvp_dtoa_fmt`.
//!
//! # Overview
//!
//! [`Value`] is jq's `jv` (minus `JV_KIND_INVALID`: fallible operations return
//! `Result<_, Error>` where jq returns `jv_invalid_with_msg`, and `Option`
//! where it returns a bare `jv_invalid()`). Values are cheap to clone: every
//! payload is reference counted with `Rc` (values never cross threads; each
//! worker compiles its own program). Mutating operations take `self`/`&mut
//! self` and update the payload in place when it is uniquely owned, like jq's
//! refcount-1 fast paths, which keeps `reduce`/`add`/`setpath` loops linear.
//! Dropping deeply nested values is iterative (jq accepts 10000 levels).
//!
//! | jq | here |
//! |---|---|
//! | `jv` / `jv_kind` / `jv_kind_name` | [`Value`], [`Kind`], [`Value::kind_name`] |
//! | numbers (`jv_number`, `jv_number_with_literal`, `jv_number_value`, `jv_number_negate`, `jv_number_abs`, `jv_is_integer`, `jvp_number_is_nan`, `jvp_number_cmp`) | [`Number`] |
//! | strings (`jv_string_*`) | [`Str`] |
//! | arrays (`jv_array_*`, slices share storage) | [`Array`] |
//! | objects (`jv_object_*`, insertion ordered) | [`Object`] |
//! | `jv_invalid_with_msg`; builtin.c's `type_error`/`type_error2` | [`Error`], [`Error::type_error`], [`Error::type_error2`] |
//! | `jv_equal`, `jv_identical`, `jv_contains`, `jv_cmp` | [`Value::equal`], [`Value::identical`], [`Value::contains`], [`Value::compare`] |
//! | `jv_get`, `jv_set`, `jv_has` | [`Value::get`], [`Value::set`], [`Value::has`] |
//! | `jv_getpath`, `jv_setpath`, `jv_delpaths` | [`Value::getpath`], [`Value::setpath`], [`Value::delpaths`] |
//! | `jv_keys`, `jv_keys_unsorted` | [`Value::keys`], [`Value::keys_unsorted`] |
//! | `jv_sort`, `jv_group`, `jv_unique` | [`sort`], [`group`], [`unique`] |
//! | `jv_dump*`, `jv_dump_string`, `jv_dump_string_trunc`, `jq_set_colors` | [`print`](mod@print): [`DumpOptions`], [`dump`], [`dump_string`], [`dump_string_trunc`], [`Colors`] |
//! | `jv_parser_new`/`_set_buf`/`_remaining`/`_next`, `jv_parse_sized`, `jv_parse` | [`parse`]: [`Parser`], [`ParseFlags`], [`parse_sized`], [`parse::parse`], [`parse::parse_all`] |
//! | `jv_load_file` | [`load_file`] |
//! | `jvp_utf8_*`, `jvp_codepoint_is_whitespace` | [`unicode`] |
//! | `jvp_dtoa_fmt` | [`dtoa::dtoa_fmt`] |
//!
//! # Using it from the evaluator, builtins and CLI
//!
//! * Construct: `Value::Null`, `Value::Bool(b)`, [`Value::number`] (a native
//!   double, what arithmetic produces), [`Number::from_literal`] (what the
//!   JSON parser, program constants and `tonumber` produce; use
//!   [`Number::from_c_literal`] where jq passes a C string), `Value::from(&str)`,
//!   [`Value::string_from_bytes`] (repairs invalid UTF-8),
//!   [`Array::from_vec`] / `collect()`, `Object::new` + [`Object::insert`].
//! * Program number constants: the lexer passes the token to
//!   `jv_parse_sized`; [`parse_sized`] (or [`Number::from_literal`], which
//!   gives the same literal for lexer tokens) keeps `1.000`/`1E+2` exact.
//! * Errors that print a value use [`dump_string_trunc`] with jq's buffer
//!   size (15 or 30), e.g. `Cannot iterate over number (123)`.
//! * `tojson`/`tostring`/`@json` are [`Value::to_json`]; output uses
//!   [`dump`] with the CLI's [`DumpOptions`].
//! * Path tracking (`path(...)`, `jq->value_at_path`) must use
//!   [`Value::identical`]: literals and containers compare by allocation,
//!   so `1 | path(1)` is an error while `1 as $x | $x | path($x)` is `[]`.
//! * The CLI must feed stdin to the [`Parser`] the way jq does: `fgets`
//!   into a 4096-byte buffer, and a chunk without a newline is cut at its
//!   first NUL (`strlen`). jq's parser drops the rest of the current chunk
//!   after an error, so chunking is observable with `--seq`/`--stream-errors`.
//!   With `-R`, each chunk is turned into a string on its own
//!   ([`Str::push_bytes`]), so a character split by the 4095-byte boundary
//!   of a long line becomes two U+FFFD in jq too.
//!
//! # Quirks that are reproduced on purpose
//!
//! * Number literals keep their decimal text: `1.000` prints as `1.000`,
//!   `1e2` as `1E+2`, and two literals compare exactly
//!   (`100000000000000000001 > 100000000000000000000`). Arithmetic results are
//!   plain doubles printed with the shortest round-trip digits. A literal's
//!   double is jq's: rounded to 17 significant digits first, then to double.
//!   Negating a literal zero gives `0` (decNumber computes `0 - x`).
//! * Array slices are views: `[1,2,3,4] | .[0:2] == .[2:4]` is `true` because
//!   `jv_equal` short-circuits on shared storage with equal length, and
//!   `[range(4)+1] | .[0:2] | .[3] = 9` is `[1,2,3,9]` (the uniquely owned
//!   view is written in place and a stale element reappears).
//! * NaN sorts below every number; `nan < nan` is true but `nan == nan` is
//!   false. When `jv_cmp` is inconsistent (NaN keys, or literals beyond
//!   double precision mixed with doubles) sorting uses the platform `qsort`,
//!   as jq does.
//! * Strings with invalid UTF-8 are repaired with jq's own U+FFFD rules
//!   (e.g. a truncated sequence swallows the rest of the buffer).
//! * Object keys keep their first position when set again; a deleted and
//!   re-added key moves to the end.
//!
//! # Deviations
//!
//! * `delpaths` with a NaN path element terminates (jq 1.8.1 loops forever),
//!   and a malformed slice key in `delpaths` on an array just errors (jq
//!   double-frees the key and corrupts its heap).
//! * No `Object too big` error (jq's limit is 2^29 slots).

pub mod array;
mod aux;
mod deep;
pub mod dtoa;
mod error;
pub mod file;
pub mod number;
pub mod object;
pub mod parse;
pub mod print;
mod qsort;
pub mod string;
pub mod unicode;

#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::fmt;

pub use array::Array;
pub use aux::{group, sort, unique};
pub use error::Error;
pub use file::load_file;
pub use number::Number;
pub use object::Object;
pub use parse::{ParseFlags, Parser, parse_sized};
pub use print::{
    Colors, DumpOptions, Indent, dump, dump_refcounted, dump_string, dump_string_trunc,
};
pub use string::Str;

/// `jv_kind` without `JV_KIND_INVALID`, in jq's order (which is also the
/// cross-type sort order: null < false < true < numbers < strings < arrays <
/// objects).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Kind {
    Null = 1,
    False = 2,
    True = 3,
    Number = 4,
    String = 5,
    Array = 6,
    Object = 7,
}

impl Kind {
    /// `jv_kind_name`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Null => "null",
            Kind::False | Kind::True => "boolean",
            Kind::Number => "number",
            Kind::String => "string",
            Kind::Array => "array",
            Kind::Object => "object",
        }
    }
}

/// A jq value (`jv`).
#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(Str),
    Array(Array),
    Object(Object),
}

impl Value {
    /// `jv_get_kind`.
    #[inline]
    pub fn kind(&self) -> Kind {
        match self {
            Value::Null => Kind::Null,
            Value::Bool(false) => Kind::False,
            Value::Bool(true) => Kind::True,
            Value::Number(_) => Kind::Number,
            Value::String(_) => Kind::String,
            Value::Array(_) => Kind::Array,
            Value::Object(_) => Kind::Object,
        }
    }

    /// `jv_kind_name(jv_get_kind(v))`: `"null"`, `"boolean"`, `"number"`,
    /// `"string"`, `"array"` or `"object"`.
    #[inline]
    pub fn kind_name(&self) -> &'static str {
        self.kind().name()
    }

    /// `jv_number(x)`: a native (non-literal) number.
    #[inline]
    pub fn number(x: f64) -> Value {
        Value::Number(Number::from_f64(x))
    }

    /// `jv_string_sized`: a string from bytes, replacing invalid UTF-8.
    pub fn string_from_bytes(bytes: &[u8]) -> Value {
        Value::String(Str::from_bytes(bytes))
    }

    /// An empty array (`jv_array()`).
    pub fn empty_array() -> Value {
        Value::Array(Array::new())
    }

    /// An empty object (`jv_object()`).
    pub fn empty_object() -> Value {
        Value::Object(Object::new())
    }

    /// Whether this is `null`.
    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// jq truthiness: everything except `null` and `false` is true.
    #[inline]
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Null | Value::Bool(false))
    }

    /// The string contents, if this is a string.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The number, if this is a number.
    #[inline]
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// The number as a double (`jv_number_value`), if this is a number.
    #[inline]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(n.value()),
            _ => None,
        }
    }

    /// The array, if this is an array.
    #[inline]
    pub fn as_array(&self) -> Option<&Array> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The object, if this is an object.
    #[inline]
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// `jv_equal`: structural equality, with jq's quirks: numbers compare
    /// with [`Number::equal`] (NaN is unequal to everything), objects ignore
    /// key order, and arrays sharing storage with the same length are equal
    /// without looking at the elements (see [`Array::same_storage`]).
    /// Safe on arbitrarily deep values.
    pub fn equal(&self, other: &Value) -> bool {
        deep::equal(self, other)
    }

    /// `jv_identical`: same payload allocation (and, for arrays, the same
    /// view); non-allocated values compare by value, doubles bitwise.
    pub fn identical(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a.identical(b),
            (Value::String(a), Value::String(b)) => a.ptr_eq(b),
            (Value::Array(a), Value::Array(b)) => a.identical(b),
            (Value::Object(a), Value::Object(b)) => a.ptr_eq(b),
            _ => false,
        }
    }

    /// `jv_contains`: objects contain objects whose values they contain,
    /// arrays contain arrays whose every element is contained by some
    /// element, strings contain substrings; anything else must be equal.
    /// Values of different kinds (including `true` vs `false`) never
    /// contain each other. Safe on arbitrarily deep values.
    pub fn contains(&self, other: &Value) -> bool {
        deep::contains(self, other)
    }

    /// `jv_get_refcnt`: the number of references to the payload of an
    /// allocated value (strings, arrays, objects, number literals); 1 for
    /// null, booleans and native numbers.
    pub fn refcount(&self) -> usize {
        match self {
            Value::Null | Value::Bool(_) => 1,
            Value::Number(n) => n.refcount(),
            Value::String(s) => s.refcount(),
            Value::Array(a) => a.refcount(),
            Value::Object(o) => o.refcount(),
        }
    }

    /// `jv_dump_string(v, 0)`: compact JSON text (as `tojson` produces).
    pub fn to_json(&self) -> String {
        dump_string(self, &DumpOptions::default())
    }
}

/// Equality is jq's `jv_equal` (so NaN != NaN, and the array-view quirk
/// applies). There is deliberately no `Eq`/`Ord`: use [`Value::compare`].
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.equal(other)
    }
}

impl fmt::Display for Value {
    /// Compact JSON (`jv_dump_string(v, 0)`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Value {
        Value::number(x)
    }
}

impl From<i32> for Value {
    fn from(x: i32) -> Value {
        Value::number(x as f64)
    }
}

impl From<i64> for Value {
    fn from(x: i64) -> Value {
        Value::number(x as f64)
    }
}

impl From<usize> for Value {
    fn from(x: usize) -> Value {
        Value::number(x as f64)
    }
}

impl From<Number> for Value {
    fn from(n: Number) -> Value {
        Value::Number(n)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::String(Str::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::String(Str::from(s))
    }
}

impl From<Str> for Value {
    fn from(s: Str) -> Value {
        Value::String(s)
    }
}

impl From<Array> for Value {
    fn from(a: Array) -> Value {
        Value::Array(a)
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Value {
        Value::Array(Array::from_vec(v))
    }
}

impl From<Object> for Value {
    fn from(o: Object) -> Value {
        Value::Object(o)
    }
}

impl FromIterator<Value> for Value {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Value {
        Value::Array(iter.into_iter().collect())
    }
}

/// `jv_cmp` as an `Ordering` (see [`Value::compare`]).
pub fn compare(a: &Value, b: &Value) -> Ordering {
    a.compare(b)
}
