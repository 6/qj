//! String builtins of `builtin.c`: startswith/endswith, split/1, explode/implode,
//! `_strindices`, trim/ltrim/rtrim.
//!
//! Port of builtin.c. Owned by Track B1 (docs/JQ_PORT_PLAN.md).

use super::{CResult, Host};
use crate::jq::value::unicode::codepoint_is_whitespace;
use crate::jq::value::{Error, Str, Value};

/// `startswith` (nargs 2): port of builtin.c `f_startswith` (a byte-prefix test).
pub fn f_startswith(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let b = std::mem::take(&mut args[0]);
    match (&input, &b) {
        (Value::String(a), Value::String(b)) => {
            Ok(Value::Bool(a.as_bytes().starts_with(b.as_bytes())))
        }
        _ => Err(Error::msg("startswith() requires string inputs")),
    }
}

/// `endswith` (nargs 2): port of builtin.c `f_endswith` (a byte-suffix test).
pub fn f_endswith(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let b = std::mem::take(&mut args[0]);
    match (&input, &b) {
        (Value::String(a), Value::String(b)) => {
            Ok(Value::Bool(a.as_bytes().ends_with(b.as_bytes())))
        }
        _ => Err(Error::msg("endswith() requires string inputs")),
    }
}

/// `split` (nargs 2): port of builtin.c `f_string_split` (`jv_string_split`; the regex
/// `split/2` is defined in builtin.jq).
pub fn f_string_split(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let sep = std::mem::take(&mut args[0]);
    match (&input, &sep) {
        (Value::String(s), Value::String(sep)) => Ok(Value::Array(s.split(sep))),
        _ => Err(Error::msg("split input and separator must be strings")),
    }
}

/// `explode` (nargs 1): port of builtin.c `f_string_explode` (`jv_string_explode`).
pub fn f_string_explode(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::String(s) => Ok(Value::Array(s.explode())),
        _ => Err(Error::msg("explode input must be a string")),
    }
}

/// `implode` (nargs 1): port of builtin.c `f_string_implode`. Codepoints are truncated
/// to integers; those outside Unicode or in the surrogate range become U+FFFD.
pub fn f_string_implode(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Str::implode(&input).map(Value::String)
}

/// `_strindices` (nargs 2): port of builtin.c `f_string_indexes` (`jv_string_indexes`):
/// the codepoint offsets of every, possibly overlapping, occurrence of the argument.
///
/// Deviation: jq 1.8.1 only ever calls this with two strings (`indices` checks the
/// types first); called directly with anything else it fails an assertion and aborts
/// (`jq -n '1 | _strindices("a")'` exits 134). The port raises an error instead.
pub fn f_string_indexes(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let k = std::mem::take(&mut args[0]);
    match (&input, &k) {
        (Value::String(j), Value::String(k)) => Ok(Value::Array(j.indexes(k))),
        _ => Err(Error::type_error2(
            &input,
            &k,
            "cannot be searched: _strindices requires string inputs",
        )),
    }
}

/// Port of builtin.c `string_trim`: strips leading and/or trailing codepoints with the
/// Unicode White_Space property (`jvp_codepoint_is_whitespace`). An untouched string
/// is returned as it is.
fn string_trim(a: Value, left: bool, right: bool) -> CResult {
    let Value::String(s) = &a else {
        return Err(Error::msg("trim input must be a string"));
    };
    let is_ws = |c: char| codepoint_is_whitespace(c as i32);
    let text = s.as_str();
    let mut trim_start = 0;
    if left {
        trim_start = text.len() - text.trim_start_matches(is_ws).len();
    }
    let mut trim_end = text.len();
    // make sure not empty string or start trim has trimmed everything
    if right && trim_end > trim_start {
        trim_end = trim_start + text[trim_start..].trim_end_matches(is_ws).len();
    }
    // no new string needed if there is nothing to trim
    if trim_start == 0 && trim_end == text.len() {
        return Ok(a);
    }
    Ok(Value::from(&text[trim_start..trim_end]))
}

/// `trim` (nargs 1): port of builtin.c `f_string_trim`.
pub fn f_string_trim(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    string_trim(input, true, true)
}

/// `ltrim` (nargs 1): port of builtin.c `f_string_ltrim`.
pub fn f_string_ltrim(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    string_trim(input, true, false)
}

/// `rtrim` (nargs 1): port of builtin.c `f_string_rtrim`.
pub fn f_string_rtrim(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    string_trim(input, false, true)
}
