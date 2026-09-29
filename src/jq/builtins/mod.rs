//! jq's C-coded builtins: port of `builtin.c`.
//!
//! [`function_list`] is jq's `function_list[]` in jq's order (the `libm.h` entries first,
//! then the rest). The order is observable through `builtins`, so keep it identical. The
//! compiler binds these by name and arity like `gen_cbinding`; the VM calls them from
//! `CALL_BUILTIN`.
//!
//! Ownership (docs/JQ_PORT_PLAN.md): Track B1 owns `binops.rs`, `general.rs`, `strings.rs`
//! and `format.rs`; Track B2 owns `platform.rs`; the compiler/VM tracks own everything
//! else in this directory (builtin.jq, bytecoded builtins, binding). This file is the
//! shared interface: change it only additively.

pub mod bind;
pub mod binops;
pub mod format;
pub mod general;
pub mod platform;
pub mod strings;
pub mod testing;

use crate::jq::value::{Error, Value};

/// What a C-coded builtin produces. execute.c's `CALL_BUILTIN` pushes a valid result and
/// raises an invalid-with-message as an error. No builtin in jq 1.8.1 returns a bare
/// `jv_invalid()` (which would backtrack like `empty`), so there is no third case.
pub type CResult = Result<Value, Error>;

/// A C-coded builtin: `input` plus `nargs - 1` evaluated arguments, `args[0]` being the
/// first declared parameter.
///
/// Arguments are owned by the callee, as jq passes `jv` by value: move them out with
/// `std::mem::take(&mut args[i])` rather than cloning, so uniquely owned values stay
/// uniquely owned and can be updated in place (this is what keeps `. + [$x]` inside
/// `reduce` linear). Dropping an unused argument or input is jq's `jv_free`.
pub type CFn = fn(&mut dyn Host, Value, &mut [Value]) -> CResult;

/// One entry of jq's `function_list[]` (`CFUNC(fptr, name, nargs)`).
#[derive(Clone, Copy)]
pub struct CFunction {
    pub name: &'static str,
    /// Arity including the input, 1..=4, as in `CFUNC`.
    pub nargs: usize,
    pub f: CFn,
}

/// The interpreter state a builtin may use (`jq_state` in builtin.c). The VM implements
/// it; builtins see nothing else of the VM.
pub trait Host {
    /// `f_input`: the next value from the input callback. `None` when there is no callback
    /// or no more input (jq then raises the error `"break"`); `Some(Err(..))` for an input
    /// error such as a parse error.
    fn next_input(&mut self) -> Option<CResult>;
    /// `f_debug`: hand `v` to the debug callback (the CLI prints `["DEBUG:",v]`).
    fn debug(&mut self, v: &Value);
    /// `f_stderr`: hand `v` to the stderr callback (the CLI prints it compactly, no newline).
    fn stderr(&mut self, v: &Value);
    /// `jq_halt(jq, exit_code, error_message)`; `None` stands for `jv_invalid()`.
    fn halt(&mut self, exit_code: Option<Value>, error_message: Option<Value>);
    /// `jq_get_lib_dirs` (`get_search_list`).
    fn lib_dirs(&self) -> Value;
    /// `jq_get_prog_origin`.
    fn prog_origin(&self) -> Value;
    /// `jq_get_jq_origin`.
    fn jq_origin(&self) -> Value;
    /// linker.c `load_module_meta` (`modulemeta`); `name` is a string.
    fn module_meta(&mut self, name: &Value) -> CResult;
    /// `jq_util_input_get_current_filename`; `None` is jq's invalid, which `input_filename`
    /// turns into `null`.
    fn current_filename(&self) -> Option<Value>;
    /// `jq_util_input_get_current_line`.
    fn current_line(&self) -> Value;
    /// execute.c `_jq_path_append(jq, v, p, value_at_path)`, for `f_getpath`: inside a
    /// path expression (`path(...)`, `paths`, `|=`, ...), when `v` is the value at the
    /// path being tracked, extend that path by `p` (an array of keys) and make
    /// `value_at_path` the tracked value. Returns `value_at_path`, which is what
    /// `getpath` returns. f_getpath is
    /// `host.path_append(input, path, input.getpath(&path))` (passing both by value).
    ///
    /// The default is the behavior outside path expressions: return `value_at_path`.
    fn path_append(&mut self, v: Value, p: Value, value_at_path: CResult) -> CResult {
        let _ = (v, p);
        value_at_path
    }
}

/// jq's `function_list[]`, in jq's order: `libm.h` first, then the rest.
pub fn function_list() -> Vec<CFunction> {
    let mut list = platform::libm_functions();
    list.extend_from_slice(FUNCTION_LIST_TAIL);
    list
}

/// `function_list[]` after the `libm.h` entries.
#[rustfmt::skip]
static FUNCTION_LIST_TAIL: &[CFunction] = &[
    CFunction { name: "_negate", nargs: 1, f: binops::f_negate },
    CFunction { name: "_plus", nargs: 3, f: binops::f_plus },
    CFunction { name: "_minus", nargs: 3, f: binops::f_minus },
    CFunction { name: "_multiply", nargs: 3, f: binops::f_multiply },
    CFunction { name: "_divide", nargs: 3, f: binops::f_divide },
    CFunction { name: "_mod", nargs: 3, f: binops::f_mod },
    CFunction { name: "_equal", nargs: 3, f: binops::f_equal },
    CFunction { name: "_notequal", nargs: 3, f: binops::f_notequal },
    CFunction { name: "_less", nargs: 3, f: binops::f_less },
    CFunction { name: "_lesseq", nargs: 3, f: binops::f_lesseq },
    CFunction { name: "_greater", nargs: 3, f: binops::f_greater },
    CFunction { name: "_greatereq", nargs: 3, f: binops::f_greatereq },
    CFunction { name: "tojson", nargs: 1, f: general::f_dump },
    CFunction { name: "fromjson", nargs: 1, f: general::f_json_parse },
    CFunction { name: "tonumber", nargs: 1, f: general::f_tonumber },
    CFunction { name: "toboolean", nargs: 1, f: general::f_toboolean },
    CFunction { name: "tostring", nargs: 1, f: general::f_tostring },
    CFunction { name: "keys", nargs: 1, f: general::f_keys },
    CFunction { name: "keys_unsorted", nargs: 1, f: general::f_keys_unsorted },
    CFunction { name: "startswith", nargs: 2, f: strings::f_startswith },
    CFunction { name: "endswith", nargs: 2, f: strings::f_endswith },
    CFunction { name: "split", nargs: 2, f: strings::f_string_split },
    CFunction { name: "explode", nargs: 1, f: strings::f_string_explode },
    CFunction { name: "implode", nargs: 1, f: strings::f_string_implode },
    CFunction { name: "_strindices", nargs: 2, f: strings::f_string_indexes },
    CFunction { name: "trim", nargs: 1, f: strings::f_string_trim },
    CFunction { name: "ltrim", nargs: 1, f: strings::f_string_ltrim },
    CFunction { name: "rtrim", nargs: 1, f: strings::f_string_rtrim },
    CFunction { name: "setpath", nargs: 3, f: general::f_setpath },
    CFunction { name: "getpath", nargs: 2, f: general::f_getpath },
    CFunction { name: "delpaths", nargs: 2, f: general::f_delpaths },
    CFunction { name: "has", nargs: 2, f: general::f_has },
    CFunction { name: "contains", nargs: 2, f: general::f_contains },
    CFunction { name: "length", nargs: 1, f: general::f_length },
    CFunction { name: "utf8bytelength", nargs: 1, f: general::f_utf8bytelength },
    CFunction { name: "type", nargs: 1, f: general::f_type },
    CFunction { name: "isinfinite", nargs: 1, f: general::f_isinfinite },
    CFunction { name: "isnan", nargs: 1, f: general::f_isnan },
    CFunction { name: "isnormal", nargs: 1, f: general::f_isnormal },
    CFunction { name: "infinite", nargs: 1, f: general::f_infinite },
    CFunction { name: "nan", nargs: 1, f: general::f_nan },
    CFunction { name: "sort", nargs: 1, f: general::f_sort },
    CFunction { name: "_sort_by_impl", nargs: 2, f: general::f_sort_by_impl },
    CFunction { name: "_group_by_impl", nargs: 2, f: general::f_group_by_impl },
    CFunction { name: "unique", nargs: 1, f: general::f_unique },
    CFunction { name: "_unique_by_impl", nargs: 2, f: general::f_unique_by_impl },
    CFunction { name: "bsearch", nargs: 2, f: general::f_bsearch },
    CFunction { name: "min", nargs: 1, f: general::f_min },
    CFunction { name: "max", nargs: 1, f: general::f_max },
    CFunction { name: "_min_by_impl", nargs: 2, f: general::f_min_by_impl },
    CFunction { name: "_max_by_impl", nargs: 2, f: general::f_max_by_impl },
    CFunction { name: "error", nargs: 1, f: general::f_error },
    CFunction { name: "format", nargs: 2, f: format::f_format },
    CFunction { name: "env", nargs: 1, f: general::f_env },
    CFunction { name: "halt", nargs: 1, f: general::f_halt },
    CFunction { name: "halt_error", nargs: 2, f: general::f_halt_error },
    CFunction { name: "get_search_list", nargs: 1, f: general::f_get_search_list },
    CFunction { name: "get_prog_origin", nargs: 1, f: general::f_get_prog_origin },
    CFunction { name: "get_jq_origin", nargs: 1, f: general::f_get_jq_origin },
    CFunction { name: "_match_impl", nargs: 4, f: platform::f_match },
    CFunction { name: "modulemeta", nargs: 1, f: general::f_modulemeta },
    CFunction { name: "input", nargs: 1, f: general::f_input },
    CFunction { name: "debug", nargs: 1, f: general::f_debug },
    CFunction { name: "stderr", nargs: 1, f: general::f_stderr },
    CFunction { name: "strptime", nargs: 2, f: platform::f_strptime },
    CFunction { name: "strftime", nargs: 2, f: platform::f_strftime },
    CFunction { name: "strflocaltime", nargs: 2, f: platform::f_strflocaltime },
    CFunction { name: "mktime", nargs: 1, f: platform::f_mktime },
    CFunction { name: "gmtime", nargs: 1, f: platform::f_gmtime },
    CFunction { name: "localtime", nargs: 1, f: platform::f_localtime },
    CFunction { name: "now", nargs: 1, f: platform::f_now },
    CFunction { name: "input_filename", nargs: 1, f: general::f_current_filename },
    CFunction { name: "input_line_number", nargs: 1, f: general::f_current_line },
    CFunction { name: "have_decnum", nargs: 1, f: general::f_have_decnum },
    CFunction { name: "have_literal_numbers", nargs: 1, f: general::f_have_decnum },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_list_matches_jq_order_and_arity() {
        let list = function_list();
        // The libm.h entries followed by jq 1.8.1's 75 other entries (64 CFUNCs + 11 BINOPS).
        assert_eq!(
            list.len(),
            crate::jq::platform::math::table().len() + FUNCTION_LIST_TAIL.len()
        );
        assert_eq!(FUNCTION_LIST_TAIL.len(), 75);
        assert_eq!(FUNCTION_LIST_TAIL[0].name, "_negate");
        assert_eq!(
            FUNCTION_LIST_TAIL.last().unwrap().name,
            "have_literal_numbers"
        );
        assert!(list.iter().all(|c| (1..=4).contains(&c.nargs)));
    }
}
