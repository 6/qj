//! `f_negate` and the `BINOPS` (`_plus` … `_greatereq`), plus the `binop_*` functions the compiler's constant folding calls (parser.y `constant_fold`).
//!
//! Owned by Track B1 (docs/JQ_PORT_PLAN.md). Scaffold: every function is a
//! stub returning a "not ported" error until the track fills it in.
use super::{CResult, Host, not_ported};
use crate::jq::value::Value;

/// `_negate` (nargs 1): port of builtin.c `f_negate`.
pub fn f_negate(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_negate"))
}

/// `_plus` (nargs 3): port of builtin.c `f_plus`.
pub fn f_plus(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_plus"))
}

/// `_minus` (nargs 3): port of builtin.c `f_minus`.
pub fn f_minus(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_minus"))
}

/// `_multiply` (nargs 3): port of builtin.c `f_multiply`.
pub fn f_multiply(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_multiply"))
}

/// `_divide` (nargs 3): port of builtin.c `f_divide`.
pub fn f_divide(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_divide"))
}

/// `_mod` (nargs 3): port of builtin.c `f_mod`.
pub fn f_mod(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_mod"))
}

/// `_equal` (nargs 3): port of builtin.c `f_equal`.
pub fn f_equal(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_equal"))
}

/// `_notequal` (nargs 3): port of builtin.c `f_notequal`.
pub fn f_notequal(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_notequal"))
}

/// `_less` (nargs 3): port of builtin.c `f_less`.
pub fn f_less(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_less"))
}

/// `_lesseq` (nargs 3): port of builtin.c `f_lesseq`.
pub fn f_lesseq(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_lesseq"))
}

/// `_greater` (nargs 3): port of builtin.c `f_greater`.
pub fn f_greater(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_greater"))
}

/// `_greatereq` (nargs 3): port of builtin.c `f_greatereq`.
pub fn f_greatereq(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("_greatereq"))
}

/// builtin.c `binop_plus`, also used by constant folding.
pub fn binop_plus(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_plus"))
}

/// builtin.c `binop_minus`, also used by constant folding.
pub fn binop_minus(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_minus"))
}

/// builtin.c `binop_multiply`, also used by constant folding.
pub fn binop_multiply(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_multiply"))
}

/// builtin.c `binop_divide`, also used by constant folding.
pub fn binop_divide(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_divide"))
}

/// builtin.c `binop_mod`, also used by constant folding.
pub fn binop_mod(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_mod"))
}

/// builtin.c `binop_equal`, also used by constant folding.
pub fn binop_equal(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_equal"))
}

/// builtin.c `binop_notequal`, also used by constant folding.
pub fn binop_notequal(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_notequal"))
}

/// builtin.c `binop_less`, also used by constant folding.
pub fn binop_less(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_less"))
}

/// builtin.c `binop_lesseq`, also used by constant folding.
pub fn binop_lesseq(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_lesseq"))
}

/// builtin.c `binop_greater`, also used by constant folding.
pub fn binop_greater(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_greater"))
}

/// builtin.c `binop_greatereq`, also used by constant folding.
pub fn binop_greatereq(_a: Value, _b: Value) -> CResult {
    Err(not_ported("_greatereq"))
}
