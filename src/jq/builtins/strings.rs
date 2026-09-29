//! String builtins of `builtin.c`: startswith/endswith, split/1, explode/implode,
//! `_strindices`, trim/ltrim/rtrim.
//!
//! Port of builtin.c. Owned by Track B1 (docs/JQ_PORT_PLAN.md).

use super::{CResult, Host};
use crate::jq::platform::Error::Abort;
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

/// The assertions `jv_string_indexes` fails on a non-string input (`j`) or needle
/// (`k`), as the platform's `assert()` prints them.
#[cfg(target_vendor = "apple")]
const ASSERT_INDEXES_J: &str = "Assertion failed: (JVP_HAS_KIND(j, JV_KIND_STRING)), function jv_string_indexes, file jv.c, line 1312.";
#[cfg(target_vendor = "apple")]
const ASSERT_INDEXES_K: &str = "Assertion failed: (JVP_HAS_KIND(k, JV_KIND_STRING)), function jv_string_indexes, file jv.c, line 1313.";
#[cfg(not(target_vendor = "apple"))]
const ASSERT_INDEXES_J: &str =
    "jq: src/jv.c:1312: jv_string_indexes: Assertion `JVP_HAS_KIND(j, JV_KIND_STRING)' failed.";
#[cfg(not(target_vendor = "apple"))]
const ASSERT_INDEXES_K: &str =
    "jq: src/jv.c:1313: jv_string_indexes: Assertion `JVP_HAS_KIND(k, JV_KIND_STRING)' failed.";

/// `_strindices` (nargs 2): port of builtin.c `f_string_indexes` (`jv_string_indexes`):
/// the codepoint offsets of every, possibly overlapping, occurrence of the argument.
///
/// jq 1.8.1 doesn't check the types here (`indices` does, before calling it): called
/// directly with a non-string, it fails an assertion and dies of SIGABRT
/// (`jq -n '1 | _strindices("a")'` exits 134), which `try` can't catch. The port
/// reproduces the crash as the platform builtins do
/// ([`crate::jq::platform::Error::abort_process`]).
pub fn f_string_indexes(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
    let k = std::mem::take(&mut args[0]);
    match (&input, &k) {
        (Value::String(j), Value::String(k)) => Ok(Value::Array(j.indexes(k))),
        (Value::String(_), _) => Abort(ASSERT_INDEXES_K.to_owned()).abort_process(),
        _ => Abort(ASSERT_INDEXES_J.to_owned()).abort_process(),
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

#[cfg(test)]
mod tests {
    //! The fixture suite (`general/tests.rs`) covers these builtins; this checks the one
    //! case it can't: `_strindices` crashing like jq.
    use super::*;
    use crate::jq::builtins::testing::TestHost;

    /// Env var telling the child which call to make.
    const CHILD_ENV: &str = "QJ_TEST_B1_STRINDICES_CHILD";

    /// `jq -n '1 | _strindices("a")'` and `jq -n '"a" | _strindices(1)'` die of SIGABRT
    /// after printing the failed assertion (the input is checked first). Each call runs
    /// in a child process: this test binary, running `strindices_abort_child`.
    #[cfg(unix)]
    #[test]
    fn strindices_on_non_strings_aborts_like_jq() {
        use std::os::unix::process::ExitStatusExt;
        for (which, assertion) in [
            ("input", ASSERT_INDEXES_J),
            ("both", ASSERT_INDEXES_J),
            ("needle", ASSERT_INDEXES_K),
        ] {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "jq::builtins::strings::tests::strindices_abort_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, which)
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.signal(),
                Some(libc::SIGABRT),
                "{which}: expected SIGABRT, got {:?}\nstderr: {stderr}",
                out.status
            );
            assert!(
                stderr.lines().any(|l| l == assertion),
                "{which}: stderr lacks {assertion:?}:\n{stderr}"
            );
        }
    }

    #[test]
    #[ignore = "run by strindices_on_non_strings_aborts_like_jq"]
    fn strindices_abort_child() {
        let Ok(which) = std::env::var(CHILD_ENV) else {
            return;
        };
        let (input, needle) = match which.as_str() {
            "input" => (Value::from(1.0), Value::from("a")),
            "both" => (Value::Null, Value::from(1.0)),
            _ => (Value::from("a"), Value::from(1.0)),
        };
        let got = f_string_indexes(&mut TestHost::default(), input, &mut [needle]);
        panic!("{which}: returned instead of aborting: {got:?}");
    }
}
