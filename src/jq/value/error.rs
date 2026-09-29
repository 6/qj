//! jq errors: `jv_invalid_with_msg`.
//!
//! In jq an error is an invalid `jv` carrying a message value (usually a
//! string, but `error({...})` can carry anything). The value layer returns
//! `Result<_, Error>` where jq returns `jv_invalid_with_msg`; jq's plain
//! `jv_invalid()` ("no value") is expressed with `Option` instead.

use std::fmt;

use super::Value;
use super::print::dump_string_trunc;

/// An error carrying a jq value as its message (`jv_invalid_with_msg`).
#[derive(Clone)]
pub struct Error(Value);

impl Error {
    /// `jv_invalid_with_msg(msg)`.
    pub fn new(msg: Value) -> Error {
        Error(msg)
    }

    /// `jv_invalid_with_msg(jv_string(msg))`.
    pub fn msg(msg: impl AsRef<str>) -> Error {
        Error(Value::from(msg.as_ref()))
    }

    /// The message value (`jv_invalid_get_msg`).
    pub fn value(&self) -> &Value {
        &self.0
    }

    /// Consumes the error, returning its message value.
    pub fn into_value(self) -> Value {
        self.0
    }

    /// The message if it is a string.
    pub fn as_str(&self) -> Option<&str> {
        self.0.as_str()
    }

    /// builtin.c's `type_error`: `"<kind> (<value, truncated to 11 bytes>) <msg>"`.
    pub fn type_error(bad: &Value, msg: &str) -> Error {
        Error::msg(format!(
            "{} ({}) {}",
            bad.kind_name(),
            dump_string_trunc(bad, 15),
            msg
        ))
    }

    /// builtin.c's `type_error2`:
    /// `"<kind> (<value>) and <kind> (<value>) <msg>"`.
    pub fn type_error2(bad1: &Value, bad2: &Value, msg: &str) -> Error {
        Error::msg(format!(
            "{} ({}) and {} ({}) {}",
            bad1.kind_name(),
            dump_string_trunc(bad1, 15),
            bad2.kind_name(),
            dump_string_trunc(bad2, 15),
            msg
        ))
    }
}

impl fmt::Display for Error {
    /// The message string itself, or the message value dumped as JSON.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_str() {
            Some(s) => f.write_str(s),
            None => write!(f, "{}", self.0),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error({:?})", self.0)
    }
}

impl std::error::Error for Error {}
