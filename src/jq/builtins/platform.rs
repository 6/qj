//! Platform-backed builtins: port of `builtin.c`'s `f_match` (`_match_impl`), its date
//! and time builtins, and its `libm.h` functions, adapting Track X's value-free
//! primitives in [`crate::jq::platform`] to jq values.
//!
//! Owned by Track B2 (docs/JQ_PORT_PLAN.md).
//!
//! Each builtin does what its `builtin.c` counterpart does around the platform call:
//! the type checks, in jq's order and with jq's messages (the primitives raise the rest),
//! then the conversion of the result to a jq value:
//!
//! - `_match_impl` returns a boolean in test mode, else an array of match objects whose
//!   keys are in the order `f_match` inserts them ([`regex::Match::KEYS`],
//!   [`regex::Capture::keys`]), since jq prints objects in insertion order.
//! - `gmtime`, `localtime` and `strptime` return jq's broken-down time: an array of 8
//!   numbers (`tm2jv`), to which `strptime` appends the unparsed rest of its input when
//!   it stopped at whitespace. `strftime`, `strflocaltime` and `mktime` read the first 8
//!   elements of an array (`jv2tm`) and ignore the rest.
//! - The libm builtins each get their own [`CFn`], one per `libm.h` table entry.
//!
//! # jq's crashes
//!
//! jq 1.8.1 fails a C `assert()` in two places reachable from here:
//! `1e30 | strflocaltime("%c")` (`localtime` fails and `jv2tm` calls `jv_array_get` on
//! the error) and, on macOS, `"100" | strptime("%j")` (Apple's `strptime` sets a day of
//! the year that disagrees with jq's own). jq prints the assertion to stderr and dies of
//! `SIGABRT` (exit status 134 in a shell); `try` can't catch it. The primitives report
//! these as [`platform::Error::Abort`], and the builtins reproduce the crash with
//! [`platform::Error::abort_process`], which writes the same line to stderr and calls
//! `abort()`: it doesn't return, and nothing unwinds.
//!
//! What happens to results jq printed before the crash but hadn't flushed yet depends on
//! the C library: Apple's `abort()` flushes stdio, so they still reach stdout
//! (`jq -n '1, (1e30 | strflocaltime("%c"))' > f` leaves `1` in `f`), while glibc's
//! doesn't (since 2.27), so they're lost. `abort()` never flushes Rust-side buffers, so
//! matching that is up to whatever buffers qj's stdout (the CLI).

use super::{CFn, CFunction, CResult, Host};
use crate::jq::platform;
use crate::jq::platform::math::{self, LibmEntry, LibmFn, LibmOutput};
use crate::jq::platform::regex::{self, Capture, Match, MatchResult};
use crate::jq::platform::time::{self, BrokenDownTime, TimeInput};
use crate::jq::value::{Array, Error, Object, Str, Value};

/// A platform error as the jq error it stands for; for [`platform::Error::Abort`], jq's
/// crash (see the module docs).
fn raise(e: platform::Error) -> Error {
    match e {
        platform::Error::Msg(msg) => Error::new(Value::from(msg)),
        platform::Error::Abort(_) => e.abort_process(),
    }
}

// ---------------------------------------------------------------------------------
// libm.h
// ---------------------------------------------------------------------------------

/// The `libm.h` entries of jq's `function_list`, in `libm.h` order, built from
/// [`math::table`]. A jq arity of 0, 2 or 3 is a `CFUNC` nargs of 1, 3 or 4 (the input
/// counts), for the `_NO` variants too.
pub fn libm_functions() -> Vec<CFunction> {
    let table = math::table();
    assert!(
        table.len() <= LIBM_WRAPPERS.len(),
        "libm.h has {} entries but there are only {} wrappers",
        table.len(),
        LIBM_WRAPPERS.len()
    );
    table
        .iter()
        .zip(LIBM_WRAPPERS.iter().copied())
        .map(|(e, f)| CFunction {
            name: e.name,
            nargs: e.arity + 1,
            f,
        })
        .collect()
}

/// The builtin for `libm.h` entry `I`. A [`CFn`] is a plain function pointer, which
/// can't carry the entry, so every table index gets its own instance of this function.
fn f_libm<const I: usize>(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    call_libm(&math::table()[I], input, args)
}

macro_rules! libm_wrappers {
    ($($i:literal)*) => {
        [$(f_libm::<$i> as CFn),*]
    };
}

/// `f_libm::<I>` for every possible table index (`libm.h` has 61 entries).
static LIBM_WRAPPERS: [CFn; 64] = libm_wrappers!(
    0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
    32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60
    61 62 63
);

/// `jv_number_value` of an argument that must be a number (`LIBM_*`'s check).
fn number_required(v: &Value) -> Result<f64, Error> {
    v.as_f64()
        .ok_or_else(|| Error::type_error(v, "number required"))
}

/// Port of the `LIBM_DD`, `LIBM_DDD`, `LIBM_DDDD` and `LIBM_DA` builtins and their
/// `_NO` variants.
///
/// A function missing at build time reports that without looking at its arguments.
/// Otherwise the numbers are checked in order: the input for `name/0` (`DD`, `DA`),
/// else each argument, first to last, ignoring the input. The result is a (non-literal)
/// number, or for `DA` the array `[result, out-parameter]`.
fn call_libm(entry: &LibmEntry, input: Value, args: &[Value]) -> CResult {
    let Some(func) = entry.func else {
        return Err(raise(entry.not_found()));
    };
    let output = match func {
        LibmFn::DD(_) | LibmFn::DA(_) => entry.apply(number_required(&input)?, &[]),
        LibmFn::DDD(_) | LibmFn::DDDD(_) => {
            let mut numbers = [0.0; 3];
            for (n, arg) in numbers.iter_mut().zip(&args[..entry.arity]) {
                *n = number_required(arg)?;
            }
            // The input is ignored (jq frees it unread).
            entry.apply(0.0, &numbers[..entry.arity])
        }
    }
    .map_err(raise)?;
    Ok(match output {
        LibmOutput::Number(x) => Value::number(x),
        // JV_ARRAY(jv_number(d), jv_number(value))
        LibmOutput::Pair([d, value]) => Value::from(vec![Value::number(d), Value::number(value)]),
    })
}

// ---------------------------------------------------------------------------------
// _match_impl
// ---------------------------------------------------------------------------------

/// `_match_impl` (nargs 4): port of builtin.c `f_match`,
/// `input | _match_impl(regex; modifiers; testmode)`.
///
/// Checks, in order: the input is a string (`<kind> (<dump>) cannot be matched, as it
/// is not a string`), the regex is a string (`<kind> (<dump>) is not a string`), the
/// modifiers are a string or null (`<kind> (<dump>) is not a string`). The modifier
/// letters, compilation and the search are [`regex::match_impl`]'s, errors included.
///
/// `testmode` selects test mode only if it is `true` (`jv_equal(testmode, jv_true())`);
/// anything else, `1` and `"true"` included, means match mode.
pub fn f_match(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let [regex, modifiers, testmode] = args else {
        unreachable!("_match_impl takes 3 arguments, got {}", args.len());
    };
    let test = matches!(testmode, Value::Bool(true));
    let Value::String(input) = &input else {
        return Err(Error::type_error(
            &input,
            "cannot be matched, as it is not a string",
        ));
    };
    let Value::String(regex) = regex else {
        return Err(Error::type_error(regex, "is not a string"));
    };
    let modifiers = match modifiers {
        Value::String(s) => Some(s.as_str()),
        Value::Null => None,
        other => return Err(Error::type_error(other, "is not a string")),
    };
    Ok(
        match regex::match_impl(input.as_str(), regex.as_str(), modifiers, test).map_err(raise)? {
            MatchResult::Test(matched) => Value::Bool(matched),
            MatchResult::Matches(matches) => {
                let mut result = Array::new();
                for m in matches {
                    result.push(match_value(m));
                }
                Value::Array(result)
            }
        },
    )
}

thread_local! {
    /// The keys of match and capture objects. jq allocates each one anew for every
    /// object; sharing them isn't observable.
    static MATCH_KEYS: [Str; 5] = ["offset", "length", "string", "captures", "name"].map(Str::from);
}

/// `jv_string(key)` for a key of a match or capture object.
fn match_key(key: &'static str) -> Str {
    MATCH_KEYS
        .with(|keys| keys.iter().find(|k| k.as_str() == key).cloned())
        .unwrap_or_else(|| Str::from(key))
}

/// A match object, keys in [`Match::KEYS`] order: `offset` and `length` in codepoints,
/// the matched `string`, and the `captures`.
fn match_value(m: Match) -> Value {
    let Match {
        offset,
        length,
        string,
        captures,
    } = m;
    let (mut string, mut captures) = (Some(string), Some(captures));
    let mut obj = Object::with_capacity(Match::KEYS.len());
    for key in Match::KEYS {
        let value = match key {
            "offset" => Value::number(offset as f64),
            "length" => Value::number(length as f64),
            "string" => Value::from(string.take().unwrap_or_default()),
            "captures" => {
                let mut values = Array::new();
                for c in captures.take().unwrap_or_default() {
                    values.push(capture_value(c));
                }
                Value::Array(values)
            }
            _ => unreachable!("unknown match key {key}"),
        };
        obj.insert(match_key(key), value);
    }
    Value::Object(obj)
}

/// A capture object, keys in [`Capture::keys`] order: `offset` (-1 if the group didn't
/// participate), `length`, `string` (null if it didn't participate) and `name` (null for
/// an unnamed group).
fn capture_value(c: Capture) -> Value {
    let keys = c.keys();
    let Capture {
        offset,
        length,
        mut string,
        mut name,
        ..
    } = c;
    let mut obj = Object::with_capacity(keys.len());
    for key in keys {
        let value = match key {
            "offset" => Value::number(offset as f64),
            "length" => Value::number(length as f64),
            "string" => string.take().map_or(Value::Null, Value::from),
            "name" => name.take().map_or(Value::Null, Value::from),
            _ => unreachable!("unknown capture key {key}"),
        };
        obj.insert(match_key(key), value);
    }
    Value::Object(obj)
}

// ---------------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------------

/// A value as the time primitives see it. For an array, `fields` receives what `jv2tm`
/// reads: its first 8 elements, `Some(jv_number_value(n))` for a number and `None` for
/// anything else.
fn time_input<'a>(v: &Value, fields: &'a mut [Option<f64>; 8]) -> TimeInput<'a> {
    match v {
        Value::Number(n) => TimeInput::Number(n.value()),
        Value::Array(a) => {
            let n = a.len().min(fields.len());
            for (field, element) in fields.iter_mut().zip(a.iter()) {
                *field = element.as_f64();
            }
            TimeInput::Array(&fields[..n])
        }
        _ => TimeInput::Other,
    }
}

/// Port of `tm2jv`: `[year, month, mday, hours, minutes, seconds, wday, yday]`.
fn broken_down_time(tm: &BrokenDownTime) -> Array {
    let mut a = Array::new();
    for &x in tm {
        a.push(Value::number(x));
    }
    a
}

/// `strptime` (nargs 2): port of builtin.c `f_strptime`, `input | strptime(format)`.
///
/// Both must be strings (`strptime/1 requires string inputs and arguments`); the rest,
/// including the `date "..." does not match format "..."` error, is
/// [`time::strptime`]'s. If parsing stopped at whitespace, the unparsed rest of the
/// input is appended as a 9th element.
pub fn f_strptime(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let [format] = args else {
        unreachable!("strptime takes 1 argument, got {}", args.len());
    };
    let parsed = time::strptime(input.as_str(), format.as_str()).map_err(raise)?;
    let mut tm = broken_down_time(&parsed.tm);
    if let Some(rest) = parsed.rest {
        tm.push(Value::from(rest));
    }
    Ok(Value::Array(tm))
}

/// `strftime` (nargs 2): port of builtin.c `f_strftime`, `input | strftime(format)`.
///
/// The input is a number (seconds since the epoch, converted with `gmtime`) or a
/// broken-down time; see [`time::strftime`] for the errors and their order.
pub fn f_strftime(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let [format] = args else {
        unreachable!("strftime takes 1 argument, got {}", args.len());
    };
    let mut fields = [None; 8];
    let s = time::strftime(time_input(&input, &mut fields), format.as_str()).map_err(raise)?;
    Ok(Value::from(s))
}

/// `strflocaltime` (nargs 2): port of builtin.c `f_strflocaltime`.
///
/// Like [`f_strftime`] in the local time zone (see [`time::strflocaltime`]). A number
/// whose `localtime` conversion fails aborts the process when the format is a string,
/// as jq 1.8.1 does (see the module docs).
pub fn f_strflocaltime(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let [format] = args else {
        unreachable!("strflocaltime takes 1 argument, got {}", args.len());
    };
    let mut fields = [None; 8];
    let s = time::strflocaltime(time_input(&input, &mut fields), format.as_str()).map_err(raise)?;
    Ok(Value::from(s))
}

/// `mktime` (nargs 1): port of builtin.c `f_mktime`: a UTC broken-down time to seconds
/// since the epoch (see [`time::mktime`]).
pub fn f_mktime(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    let mut fields = [None; 8];
    let t = time::mktime(time_input(&input, &mut fields)).map_err(raise)?;
    Ok(Value::number(t))
}

/// `gmtime` (nargs 1): port of builtin.c `f_gmtime` (see [`time::gmtime`]).
pub fn f_gmtime(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    let mut fields = [None; 8];
    let tm = time::gmtime(time_input(&input, &mut fields)).map_err(raise)?;
    Ok(Value::Array(broken_down_time(&tm)))
}

/// `localtime` (nargs 1): port of builtin.c `f_localtime` (see [`time::localtime`]).
pub fn f_localtime(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    let mut fields = [None; 8];
    let tm = time::localtime(time_input(&input, &mut fields)).map_err(raise)?;
    Ok(Value::Array(broken_down_time(&tm)))
}

/// `now` (nargs 1): port of builtin.c `f_now`, the current time in seconds since the
/// epoch with microsecond resolution. The input is ignored.
pub fn f_now(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::number(time::now()))
}

#[cfg(test)]
mod tests;
