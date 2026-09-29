//! The general C builtins of `builtin.c`: conversions, keys/paths/has/contains, length/type/number predicates, sorting and grouping impls, error/env/halt, input/debug/stderr, module and origin queries, input position, decnum flags.
//!
//! Owned by Track B1 (docs/JQ_PORT_PLAN.md). Scaffold: every function is a
//! stub returning a "not ported" error until the track fills it in.
use super::{CResult, Host, not_ported};
use crate::jq::value::Value;

/// `tojson` (nargs 1): port of builtin.c `f_dump`.
pub fn f_dump(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("tojson"))
}

/// `fromjson` (nargs 1): port of builtin.c `f_json_parse`.
pub fn f_json_parse(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("fromjson"))
}

/// `tonumber` (nargs 1): port of builtin.c `f_tonumber`.
pub fn f_tonumber(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("tonumber"))
}

/// `toboolean` (nargs 1): port of builtin.c `f_toboolean`.
pub fn f_toboolean(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("toboolean"))
}

/// `tostring` (nargs 1): port of builtin.c `f_tostring`.
pub fn f_tostring(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("tostring"))
}

/// `keys` (nargs 1): port of builtin.c `f_keys`.
pub fn f_keys(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("keys"))
}

/// `keys_unsorted` (nargs 1): port of builtin.c `f_keys_unsorted`.
pub fn f_keys_unsorted(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("keys_unsorted"))
}

/// `setpath` (nargs 3): port of builtin.c `f_setpath`.
pub fn f_setpath(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("setpath"))
}

/// `getpath` (nargs 2): port of builtin.c `f_getpath`.
pub fn f_getpath(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("getpath"))
}

/// `delpaths` (nargs 2): port of builtin.c `f_delpaths`.
pub fn f_delpaths(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("delpaths"))
}

/// `has` (nargs 2): port of builtin.c `f_has`.
pub fn f_has(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("has"))
}

/// `contains` (nargs 2): port of builtin.c `f_contains`.
pub fn f_contains(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("contains"))
}

/// `length` (nargs 1): port of builtin.c `f_length`.
pub fn f_length(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("length"))
}

/// `utf8bytelength` (nargs 1): port of builtin.c `f_utf8bytelength`.
pub fn f_utf8bytelength(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("utf8bytelength"))
}

/// `type` (nargs 1): port of builtin.c `f_type`.
pub fn f_type(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("type"))
}

/// `isinfinite` (nargs 1): port of builtin.c `f_isinfinite`.
pub fn f_isinfinite(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("isinfinite"))
}

/// `isnan` (nargs 1): port of builtin.c `f_isnan`.
pub fn f_isnan(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("isnan"))
}

/// `isnormal` (nargs 1): port of builtin.c `f_isnormal`.
pub fn f_isnormal(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("isnormal"))
}

/// `infinite` (nargs 1): port of builtin.c `f_infinite`.
pub fn f_infinite(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("infinite"))
}

/// `nan` (nargs 1): port of builtin.c `f_nan`.
pub fn f_nan(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("nan"))
}

/// `sort` (nargs 1): port of builtin.c `f_sort`.
pub fn f_sort(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("sort"))
}

/// `_sort_by_impl` (nargs 2): port of builtin.c `f_sort_by_impl`.
pub fn f_sort_by_impl(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_sort_by_impl"))
}

/// `_group_by_impl` (nargs 2): port of builtin.c `f_group_by_impl`.
pub fn f_group_by_impl(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_group_by_impl"))
}

/// `unique` (nargs 1): port of builtin.c `f_unique`.
pub fn f_unique(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("unique"))
}

/// `_unique_by_impl` (nargs 2): port of builtin.c `f_unique_by_impl`.
pub fn f_unique_by_impl(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_unique_by_impl"))
}

/// `bsearch` (nargs 2): port of builtin.c `f_bsearch`.
pub fn f_bsearch(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("bsearch"))
}

/// `min` (nargs 1): port of builtin.c `f_min`.
pub fn f_min(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("min"))
}

/// `max` (nargs 1): port of builtin.c `f_max`.
pub fn f_max(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("max"))
}

/// `_min_by_impl` (nargs 2): port of builtin.c `f_min_by_impl`.
pub fn f_min_by_impl(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_min_by_impl"))
}

/// `_max_by_impl` (nargs 2): port of builtin.c `f_max_by_impl`.
pub fn f_max_by_impl(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_max_by_impl"))
}

/// `error` (nargs 1): port of builtin.c `f_error`.
pub fn f_error(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("error"))
}

/// `env` (nargs 1): port of builtin.c `f_env`.
pub fn f_env(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("env"))
}

/// `halt` (nargs 1): port of builtin.c `f_halt`.
pub fn f_halt(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("halt"))
}

/// `halt_error` (nargs 2): port of builtin.c `f_halt_error`.
pub fn f_halt_error(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("halt_error"))
}

/// `get_search_list` (nargs 1): port of builtin.c `f_get_search_list`.
pub fn f_get_search_list(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("get_search_list"))
}

/// `get_prog_origin` (nargs 1): port of builtin.c `f_get_prog_origin`.
pub fn f_get_prog_origin(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("get_prog_origin"))
}

/// `get_jq_origin` (nargs 1): port of builtin.c `f_get_jq_origin`.
pub fn f_get_jq_origin(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("get_jq_origin"))
}

/// `modulemeta` (nargs 1): port of builtin.c `f_modulemeta`.
pub fn f_modulemeta(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("modulemeta"))
}

/// `input` (nargs 1): port of builtin.c `f_input`.
pub fn f_input(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("input"))
}

/// `debug` (nargs 1): port of builtin.c `f_debug`.
pub fn f_debug(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("debug"))
}

/// `stderr` (nargs 1): port of builtin.c `f_stderr`.
pub fn f_stderr(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("stderr"))
}

/// `input_filename` (nargs 1): port of builtin.c `f_current_filename`.
pub fn f_current_filename(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("input_filename"))
}

/// `input_line_number` (nargs 1): port of builtin.c `f_current_line`.
pub fn f_current_line(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("input_line_number"))
}

/// `have_decnum` (nargs 1): port of builtin.c `f_have_decnum`.
pub fn f_have_decnum(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("have_decnum"))
}
