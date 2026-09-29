//! Value-free platform primitives used by jq builtins: Oniguruma regex (`_match_impl`),
//! libc time functions, and the libm function table (`libm.h`).
//!
//! These are ports of the C parts of jq 1.8.1's `builtin.c` that talk to the platform.
//! They take and return plain Rust data (strings, `f64`, small structs) so that the
//! builtin layer only has to convert between these and jq values:
//!
//! - [`regex`]: the engine behind `_match_impl/3` (`f_match`), on the same Oniguruma
//!   version jq 1.8.1 vendors.
//!
//! Behavior is platform dependent in the same way jq's is (macOS libc vs glibc); each
//! function documents the differences it knows about.

pub mod regex;
mod utf8;

use std::fmt;

/// Why a platform primitive failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// jq raises an ordinary error whose value is this exact message string
    /// (`jv_invalid_with_msg(jv_string(msg))`). `try` can catch it.
    Msg(String),
    /// jq 1.8.1 fails a C `assert()` at this point and aborts the whole process: `try`
    /// cannot catch it, the process dies with SIGABRT (a shell reports exit status 134),
    /// and stdio output that jq had buffered but not yet flushed is lost. The string is
    /// the line the platform's assert handler writes to stderr, without the newline.
    ///
    /// Whether the port reproduces the crash is the caller's decision; see
    /// [`Error::abort_process`].
    Abort(String),
}

impl Error {
    pub(crate) fn msg(msg: impl Into<String>) -> Self {
        Error::Msg(msg.into())
    }

    /// The error message (for [`Error::Abort`], the assertion text).
    pub fn message(&self) -> &str {
        match self {
            Error::Msg(m) | Error::Abort(m) => m,
        }
    }

    /// Reproduce jq's crash for an [`Error::Abort`]: write the assertion line to stderr
    /// and abort the process. For [`Error::Msg`] this does the same with the message.
    pub fn abort_process(&self) -> ! {
        eprintln!("{}", self.message());
        std::process::abort()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}
