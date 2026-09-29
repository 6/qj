//! Platform-backed builtins: `_match_impl` (Oniguruma), the date/time functions (libc), and the `libm.h` table, adapting `crate::jq::platform` to `Value`.
//!
//! Owned by Track B2 (docs/JQ_PORT_PLAN.md). Scaffold: every function is a
//! stub returning a "not ported" error until the track fills it in.
use super::{CFunction, CResult, Host, not_ported};
use crate::jq::value::Value;

/// The `libm.h` entries of jq's `function_list`, in `libm.h` order, built from
/// [`crate::jq::platform::math::table`]. A jq arity of 0, 2 or 3 is a `CFUNC` nargs of
/// 1, 3 or 4 (the input counts).
pub fn libm_functions() -> Vec<CFunction> {
    crate::jq::platform::math::table()
        .iter()
        .map(|e| CFunction {
            name: e.name,
            nargs: e.arity + 1,
            f: f_libm_not_ported,
        })
        .collect()
}

/// Stub shared by all libm entries until B2 gives each its own wrapper.
fn f_libm_not_ported(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("libm function"))
}

/// `_match_impl` (nargs 4): port of builtin.c `f_match`.
pub fn f_match(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_match_impl"))
}

/// `strptime` (nargs 2): port of builtin.c `f_strptime`.
pub fn f_strptime(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("strptime"))
}

/// `strftime` (nargs 2): port of builtin.c `f_strftime`.
pub fn f_strftime(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("strftime"))
}

/// `strflocaltime` (nargs 2): port of builtin.c `f_strflocaltime`.
pub fn f_strflocaltime(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("strflocaltime"))
}

/// `mktime` (nargs 1): port of builtin.c `f_mktime`.
pub fn f_mktime(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("mktime"))
}

/// `gmtime` (nargs 1): port of builtin.c `f_gmtime`.
pub fn f_gmtime(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("gmtime"))
}

/// `localtime` (nargs 1): port of builtin.c `f_localtime`.
pub fn f_localtime(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("localtime"))
}

/// `now` (nargs 1): port of builtin.c `f_now`.
pub fn f_now(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("now"))
}
