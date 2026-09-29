//! String builtins of `builtin.c`: startswith/endswith, split/1, explode/implode, `_strindices`, trim/ltrim/rtrim.
//!
//! Owned by Track B1 (docs/JQ_PORT_PLAN.md). Scaffold: every function is a
//! stub returning a "not ported" error until the track fills it in.
use super::{CResult, Host, not_ported};
use crate::jq::value::Value;

/// `startswith` (nargs 2): port of builtin.c `f_startswith`.
pub fn f_startswith(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("startswith"))
}

/// `endswith` (nargs 2): port of builtin.c `f_endswith`.
pub fn f_endswith(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("endswith"))
}

/// `split` (nargs 2): port of builtin.c `f_string_split`.
pub fn f_string_split(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("split"))
}

/// `explode` (nargs 1): port of builtin.c `f_string_explode`.
pub fn f_string_explode(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("explode"))
}

/// `implode` (nargs 1): port of builtin.c `f_string_implode`.
pub fn f_string_implode(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("implode"))
}

/// `_strindices` (nargs 2): port of builtin.c `f_string_indexes`.
pub fn f_string_indexes(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_strindices"))
}

/// `trim` (nargs 1): port of builtin.c `f_string_trim`.
pub fn f_string_trim(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("trim"))
}

/// `ltrim` (nargs 1): port of builtin.c `f_string_ltrim`.
pub fn f_string_ltrim(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("ltrim"))
}

/// `rtrim` (nargs 1): port of builtin.c `f_string_rtrim`.
pub fn f_string_rtrim(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("rtrim"))
}
