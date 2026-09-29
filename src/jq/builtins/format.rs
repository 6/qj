//! `f_format`: `format/1` (`@text`, `@json`, `@html`, `@uri`, `@urid`, `@csv`, `@tsv`, `@sh`, `@base64`, `@base64d`, `@base32`, `@base32d`).
//!
//! Owned by Track B1 (docs/JQ_PORT_PLAN.md). Scaffold: every function is a
//! stub returning a "not ported" error until the track fills it in.
use super::{CResult, Host, not_ported};
use crate::jq::value::Value;

/// `format` (nargs 2): port of builtin.c `f_format`.
pub fn f_format(_host: &mut dyn Host, _input: Value, _args: &mut [Value]) -> CResult {
    Err(not_ported("format"))
}
