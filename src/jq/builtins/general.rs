//! The general C builtins of `builtin.c`: conversions, keys/paths/has/contains,
//! length/type/number predicates, sorting and grouping impls, error/env/halt,
//! input/debug/stderr, module and origin queries, input position, decnum flags.
//!
//! Port of builtin.c. Owned by Track B1 (docs/JQ_PORT_PLAN.md). Interpreter state is
//! reached only through [`Host`]; the process environment (`env`) is read directly, as
//! builtin.c reads `environ`.

use std::cmp::Ordering;

use super::{CResult, Host};
use crate::jq::value::{self, Error, Number, Object, Str, Value, parse_sized};

/// `tojson` (nargs 1): port of builtin.c `f_dump` (`jv_dump_string(input, 0)`).
pub fn f_dump(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::from(input.to_json()))
}

/// `fromjson` (nargs 1): port of builtin.c `f_json_parse` (`jv_parse_sized`).
pub fn f_json_parse(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::String(s) => parse_sized(s.as_bytes()),
        _ => Err(Error::type_error(&input, "only strings can be parsed")),
    }
}

/// `tonumber` (nargs 1): port of builtin.c `f_tonumber` (built with `USE_DECNUM`).
/// Numbers are returned as they are; strings become literals
/// (`jv_number_with_literal`, which reads the C string up to the first NUL), so
/// `"1e2" | tonumber` prints `1E+2` and `"nan" | tonumber` is NaN.
pub fn f_tonumber(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    if matches!(input, Value::Number(_)) {
        return Ok(input);
    }
    if let Value::String(s) = &input
        && let Some(n) = Number::from_c_literal(s.as_bytes())
    {
        return Ok(Value::Number(n));
    }
    Err(Error::type_error(&input, "cannot be parsed as a number"))
}

/// `toboolean` (nargs 1): port of builtin.c `f_toboolean` (`strcmp`, so the string is
/// read up to its first NUL).
pub fn f_toboolean(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::Bool(_) => return Ok(input),
        Value::String(s) => match s.as_c_str() {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            _ => {}
        },
        _ => {}
    }
    Err(Error::type_error(&input, "cannot be parsed as a boolean"))
}

/// Port of builtin.c `f_tostring`, shared with `format`: strings are returned as they
/// are, anything else is dumped compactly.
pub(crate) fn tostring(input: Value) -> Value {
    match input {
        Value::String(_) => input,
        _ => Value::from(input.to_json()),
    }
}

/// `tostring` (nargs 1): port of builtin.c `f_tostring`.
pub fn f_tostring(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Ok(tostring(input))
}

/// `keys` (nargs 1): port of builtin.c `f_keys` (`jv_keys`).
pub fn f_keys(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match input {
        Value::Object(_) | Value::Array(_) => input.keys(),
        _ => Err(Error::type_error(&input, "has no keys")),
    }
}

/// `keys_unsorted` (nargs 1): port of builtin.c `f_keys_unsorted` (`jv_keys_unsorted`).
pub fn f_keys_unsorted(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match input {
        Value::Object(_) | Value::Array(_) => input.keys_unsorted(),
        _ => Err(Error::type_error(&input, "has no keys")),
    }
}

/// `setpath` (nargs 3): port of builtin.c `f_setpath` (`jv_setpath(input, path, value)`).
pub fn f_setpath(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let path = std::mem::take(&mut args[0]);
    let value = std::mem::take(&mut args[1]);
    input.setpath(&path, value)
}

/// `getpath` (nargs 2): port of builtin.c `f_getpath`:
/// `_jq_path_append(jq, input, path, jv_getpath(input, path))`, so that `getpath` also
/// works inside path expressions (`path(getpath(["a","b"]))`).
pub fn f_getpath(host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let p = std::mem::take(&mut args[0]);
    let r = input.getpath(&p);
    host.path_append(input, p, r)
}

/// `delpaths` (nargs 2): port of builtin.c `f_delpaths` (`jv_delpaths`).
pub fn f_delpaths(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let paths = std::mem::take(&mut args[0]);
    input.delpaths(&paths)
}

/// `has` (nargs 2): port of builtin.c `f_has` (`jv_has`).
pub fn f_has(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let key = std::mem::take(&mut args[0]);
    input.has(&key).map(Value::Bool)
}

/// `contains` (nargs 2): port of builtin.c `f_contains`. The kinds must match exactly,
/// so `true | contains(false)` is an error rather than `false`.
pub fn f_contains(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let b = std::mem::take(&mut args[0]);
    if input.kind() == b.kind() {
        Ok(Value::Bool(input.contains(&b)))
    } else {
        Err(Error::type_error2(
            &input,
            &b,
            "cannot have their containment checked",
        ))
    }
}

/// `length` (nargs 1): port of builtin.c `f_length`. A number's length is its absolute
/// value (`jv_number_abs`, which keeps a literal a literal: `-1.50 | length` is `1.50`).
pub fn f_length(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::Array(a) => Ok(Value::from(a.len())),
        Value::Object(o) => Ok(Value::from(o.len())),
        Value::String(s) => Ok(Value::from(s.codepoint_len())),
        Value::Number(n) => Ok(Value::Number(n.abs())),
        Value::Null => Ok(Value::number(0.0)),
        Value::Bool(_) => Err(Error::type_error(&input, "has no length")),
    }
}

/// `utf8bytelength` (nargs 1): port of builtin.c `f_utf8bytelength`.
pub fn f_utf8bytelength(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::String(s) => Ok(Value::from(s.len())),
        _ => Err(Error::type_error(
            &input,
            "only strings have UTF-8 byte length",
        )),
    }
}

/// `type` (nargs 1): port of builtin.c `f_type`. Like jq, each call returns a new string
/// (`jv_string(jv_kind_name(...))`): sharing one per kind would show in
/// `--debug-trace`, which prints every value's refcount.
pub fn f_type(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    let name = match input {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    Ok(Value::from(name))
}

/// `jv_number_value` of a number, `None` for anything else (the number predicates
/// answer `false` for non-numbers).
fn number_value(v: &Value) -> Option<f64> {
    v.as_number().map(Number::value)
}

/// `isinfinite` (nargs 1): port of builtin.c `f_isinfinite`.
pub fn f_isinfinite(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::Bool(
        number_value(&input).is_some_and(f64::is_infinite),
    ))
}

/// `isnan` (nargs 1): port of builtin.c `f_isnan`.
pub fn f_isnan(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::Bool(number_value(&input).is_some_and(f64::is_nan)))
}

/// `isnormal` (nargs 1): port of builtin.c `f_isnormal` (C `isnormal`: not zero,
/// subnormal, infinite or NaN).
pub fn f_isnormal(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::Bool(
        number_value(&input).is_some_and(f64::is_normal),
    ))
}

/// `infinite` (nargs 1): port of builtin.c `f_infinite`.
pub fn f_infinite(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::number(f64::INFINITY))
}

/// `nan` (nargs 1): port of builtin.c `f_nan`.
pub fn f_nan(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::number(f64::NAN))
}

/// `sort` (nargs 1): port of builtin.c `f_sort` (`jv_sort(input, input)`).
pub fn f_sort(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::Array(a) => Ok(Value::Array(value::sort(a, a))),
        _ => Err(Error::type_error(
            &input,
            "cannot be sorted, as it is not an array",
        )),
    }
}

/// The check shared by `_sort_by_impl`, `_group_by_impl` and `_unique_by_impl`: both
/// arrays, of the same length.
fn by_impl(
    input: Value,
    keys: Value,
    f: fn(&value::Array, &value::Array) -> value::Array,
) -> CResult {
    match (&input, &keys) {
        (Value::Array(a), Value::Array(k)) if a.len() == k.len() => Ok(Value::Array(f(a, k))),
        _ => Err(Error::type_error2(
            &input,
            &keys,
            "cannot be sorted, as they are not both arrays",
        )),
    }
}

/// `_sort_by_impl` (nargs 2): port of builtin.c `f_sort_by_impl` (`jv_sort`).
pub fn f_sort_by_impl(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    by_impl(input, std::mem::take(&mut args[0]), value::sort)
}

/// `_group_by_impl` (nargs 2): port of builtin.c `f_group_by_impl` (`jv_group`).
pub fn f_group_by_impl(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    by_impl(input, std::mem::take(&mut args[0]), value::group)
}

/// `unique` (nargs 1): port of builtin.c `f_unique` (`jv_unique(input, input)`).
pub fn f_unique(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match &input {
        Value::Array(a) => Ok(Value::Array(value::unique(a, a))),
        _ => Err(Error::type_error(
            &input,
            "cannot be sorted, as it is not an array",
        )),
    }
}

/// `_unique_by_impl` (nargs 2): port of builtin.c `f_unique_by_impl` (`jv_unique`).
pub fn f_unique_by_impl(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    by_impl(input, std::mem::take(&mut args[0]), value::unique)
}

/// `bsearch` (nargs 2): port of builtin.c `f_bsearch`. On a sorted array, the index of
/// the target if present, otherwise `-1 - ix` where `ix` is the insertion point.
pub fn f_bsearch(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let target = std::mem::take(&mut args[0]);
    let a = match &input {
        Value::Array(a) => a.as_slice(),
        _ => return Err(Error::type_error(&input, "cannot be searched from")),
    };
    let mut start = 0usize;
    let mut end = a.len();
    while start < end {
        let mid = start + (end - start) / 2;
        match target.compare(&a[mid]) {
            Ordering::Equal => return Ok(Value::from(mid)),
            Ordering::Less => end = mid,
            Ordering::Greater => start = mid + 1,
        }
    }
    Ok(Value::number(-1.0 - start as f64))
}

/// Port of builtin.c `minmax_by`: the element of `values` whose key is smallest (the
/// first such) or largest (the last such), by `jv_cmp`.
fn minmax_by(values: &Value, keys: &Value, is_min: bool) -> CResult {
    let Value::Array(vals) = values else {
        return Err(Error::type_error2(values, keys, "cannot be iterated over"));
    };
    let Value::Array(ks) = keys else {
        return Err(Error::type_error2(values, keys, "cannot be iterated over"));
    };
    if vals.len() != ks.len() {
        return Err(Error::type_error2(values, keys, "have wrong length"));
    }
    let ks = ks.as_slice();
    let Some(mut retkey) = ks.first() else {
        return Ok(Value::Null);
    };
    let mut ret = 0;
    for (i, item) in ks.iter().enumerate().skip(1) {
        let cmp = item.compare(retkey);
        if (cmp == Ordering::Less) == is_min {
            retkey = item;
            ret = i;
        }
    }
    Ok(vals.as_slice()[ret].clone())
}

/// `min` (nargs 1): port of builtin.c `f_min`.
pub fn f_min(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    minmax_by(&input, &input, true)
}

/// `max` (nargs 1): port of builtin.c `f_max`.
pub fn f_max(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    minmax_by(&input, &input, false)
}

/// `_min_by_impl` (nargs 2): port of builtin.c `f_min_by_impl`.
pub fn f_min_by_impl(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let keys = std::mem::take(&mut args[0]);
    minmax_by(&input, &keys, true)
}

/// `_max_by_impl` (nargs 2): port of builtin.c `f_max_by_impl`.
pub fn f_max_by_impl(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let keys = std::mem::take(&mut args[0]);
    minmax_by(&input, &keys, false)
}

/// `error` (nargs 1): port of builtin.c `f_error`: the input becomes the error's
/// message, whatever its kind.
pub fn f_error(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    Err(Error::new(input))
}

/// `env` (nargs 1): port of builtin.c `f_env`: the process environment as an object,
/// in `environ` order.
///
/// Read through `std::env::vars_os` (which synchronizes with `std::env::set_var`)
/// rather than `environ` itself. The two agree except on malformed entries: std splits
/// `=x=1` after the name `=x` where jq's `strchr` gives the name `""` (repaired below),
/// and std skips entries without any `=`, which jq maps to `null`.
pub fn f_env(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    let mut env = Object::new();
    for (name, val) in std::env::vars_os() {
        let (name, val) = (name.as_encoded_bytes(), val.as_encoded_bytes());
        if name.first() == Some(&b'=') {
            // The raw entry was `name=val`; jq splits it at its first '=' (index 0).
            let mut rest = name[1..].to_vec();
            rest.push(b'=');
            rest.extend_from_slice(val);
            env.insert(Str::new(), Value::string_from_bytes(&rest));
        } else {
            env.insert(Str::from_bytes(name), Value::string_from_bytes(val));
        }
    }
    Ok(Value::Object(env))
}

/// `halt` (nargs 1): port of builtin.c `f_halt` (`jq_halt(jq, jv_invalid(), jv_invalid())`).
pub fn f_halt(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    host.halt(None, None);
    Ok(Value::Bool(true))
}

/// `halt_error` (nargs 2): port of builtin.c `f_halt_error`. A non-numeric exit code is
/// reported against the *input*: `"x" | halt_error("a")` fails with
/// `string ("x") halt_error/1: number required`.
pub fn f_halt_error(host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let a = std::mem::take(&mut args[0]);
    if !matches!(a, Value::Number(_)) {
        return Err(Error::type_error(&input, "halt_error/1: number required"));
    }
    host.halt(Some(a), Some(input));
    Ok(Value::Bool(true))
}

/// `get_search_list` (nargs 1): port of builtin.c `f_get_search_list`.
pub fn f_get_search_list(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(host.lib_dirs())
}

/// `get_prog_origin` (nargs 1): port of builtin.c `f_get_prog_origin`.
pub fn f_get_prog_origin(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(host.prog_origin())
}

/// `get_jq_origin` (nargs 1): port of builtin.c `f_get_jq_origin`.
pub fn f_get_jq_origin(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(host.jq_origin())
}

/// `modulemeta` (nargs 1): port of builtin.c `f_modulemeta` (`load_module_meta`).
pub fn f_modulemeta(host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match input {
        Value::String(_) => host.module_meta(&input),
        _ => Err(Error::msg("modulemeta input module name must be a string")),
    }
}

/// `input` (nargs 1): port of builtin.c `f_input`. No callback or no more input raises
/// the error `"break"` (which `inputs` catches).
pub fn f_input(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    match host.next_input() {
        Some(r) => r,
        None => Err(Error::msg("break")),
    }
}

/// `debug` (nargs 1): port of builtin.c `f_debug`: hands the input to the debug
/// callback and outputs it unchanged.
pub fn f_debug(host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    host.debug(&input);
    Ok(input)
}

/// `stderr` (nargs 1): port of builtin.c `f_stderr`: hands the input to the stderr
/// callback and outputs it unchanged.
pub fn f_stderr(host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    host.stderr(&input);
    Ok(input)
}

/// `input_filename` (nargs 1): port of builtin.c `f_current_filename` (`null` when there
/// is no current file).
pub fn f_current_filename(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(host.current_filename().unwrap_or(Value::Null))
}

/// `input_line_number` (nargs 1): port of builtin.c `f_current_line`.
pub fn f_current_line(host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(host.current_line())
}

/// `have_decnum` and `have_literal_numbers` (nargs 1): port of builtin.c
/// `f_have_decnum`. The port follows jq built with decNumber (`USE_DECNUM`).
pub fn f_have_decnum(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Ok(Value::Bool(true))
}

#[cfg(test)]
mod tests;
