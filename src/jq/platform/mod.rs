//! Value-free platform primitives used by jq builtins: Oniguruma regex (`_match_impl`),
//! libc time functions, and the libm function table (`libm.h`).
//!
//! These are ports of the C parts of jq 1.8.1's `builtin.c` that talk to the platform.
//! They take and return plain Rust data (strings, `f64`, small structs) so that the
//! builtin layer only has to convert between these and jq values:
//!
//! - [`regex`]: the engine behind `_match_impl/3` (`f_match`), on the same Oniguruma
//!   version jq 1.8.1 vendors.
//! - [`time`]: `strptime`, `strftime`, `strflocaltime`, `mktime`, `gmtime`, `localtime`
//!   and `now`, through libc like jq.
//! - [`math`]: the `libm.h` function table.
//!
//! Behavior is platform dependent in the same way jq's is (macOS libc vs glibc); each
//! function documents the differences it knows about.

pub mod math;
pub mod regex;
pub mod time;
mod utf8;

use std::fmt;

/// Why a platform primitive failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// jq raises an ordinary error whose value is this exact message string
    /// (`jv_invalid_with_msg(jv_string(msg))`). `try` can catch it.
    Msg(String),
    /// jq 1.8.1 fails a C `assert()` at this point and aborts the whole process: `try`
    /// cannot catch it, and the process dies with SIGABRT (a shell reports exit status
    /// 134). The string is the line the platform's assert handler writes to stderr,
    /// without the newline.
    ///
    /// What happens to output jq had buffered but not yet written depends on the libc:
    /// Apple's `abort()` flushes stdio, so on macOS every earlier result still appears
    /// (`jq -n 'range(5), (1e30|strflocaltime("%c"))' | cat` prints 0 to 4, then exits
    /// 134), while glibc's `abort()` (since 2.27) doesn't flush, so there the unflushed
    /// part of stdout is lost.
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
    ///
    /// On Apple targets this flushes `std::io::stdout()` first, as Apple's `abort()`
    /// flushes stdio; elsewhere it doesn't, like glibc's. Rust's abort never flushes
    /// anything, so a caller that buffers output itself (a `BufWriter`) must flush it
    /// before calling this to match jq on macOS, and must not to match jq on glibc.
    pub fn abort_process(&self) -> ! {
        eprintln!("{}", self.message());
        #[cfg(target_vendor = "apple")]
        {
            use std::io::Write;
            if let Some(flush) = BEFORE_ABORT.get() {
                flush();
            }
            let _ = std::io::stdout().flush();
        }
        std::process::abort()
    }
}

/// What [`Error::abort_process`] calls on Apple targets before aborting, where
/// jq's `abort()` flushes stdio: the CLI registers a function that flushes its
/// own stdout buffer ([`set_before_abort`]).
static BEFORE_ABORT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Registers the function [`Error::abort_process`] calls to flush buffered
/// standard output before aborting, on Apple targets only (glibc's `abort()`
/// doesn't flush, so there the buffered output is lost, as in jq). Only the
/// first registration counts.
pub fn set_before_abort(flush: fn()) {
    let _ = BEFORE_ABORT.set(flush);
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

/// C's implicit `double` to `long`/`time_t` conversion, as compilers emit it. It's
/// undefined behavior in C for NaN and out-of-range values; what the hardware
/// instruction does then is what jq does: aarch64's `fcvtzs` saturates and maps NaN to
/// 0 (Rust's `as`), x86-64's `cvttsd2si` returns `i64::MIN` ("integer indefinite").
pub(crate) fn c_double_to_i64(d: f64) -> i64 {
    #[cfg(target_arch = "x86_64")]
    if !(-9.223_372_036_854_775_808e18..9.223_372_036_854_775_808e18).contains(&d) {
        return i64::MIN;
    }
    d as i64
}

/// C's implicit `double` to `int` conversion (see [`c_double_to_i64`]); on x86-64 the
/// 32-bit `cvttsd2si` returns `i32::MIN` for NaN and out-of-range values.
pub(crate) fn c_double_to_i32(d: f64) -> i32 {
    #[cfg(target_arch = "x86_64")]
    if !(-2_147_483_648.0..2_147_483_648.0).contains(&d) {
        return i32::MIN;
    }
    d as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_conversions_truncate_in_range() {
        assert_eq!(c_double_to_i64(1.9), 1);
        assert_eq!(c_double_to_i64(-1.9), -1);
        assert_eq!(c_double_to_i32(2147483647.9), 2147483647);
        assert_eq!(c_double_to_i32(-2147483648.9), -2147483648);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn c_conversions_saturate_on_aarch64() {
        assert_eq!(c_double_to_i64(f64::NAN), 0);
        assert_eq!(c_double_to_i64(1e30), i64::MAX);
        assert_eq!(c_double_to_i64(-1e30), i64::MIN);
        assert_eq!(c_double_to_i32(f64::NAN), 0);
        assert_eq!(c_double_to_i32(1e10), i32::MAX);
        assert_eq!(c_double_to_i32(-1e10), i32::MIN);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn c_conversions_are_indefinite_on_x86_64() {
        assert_eq!(c_double_to_i64(f64::NAN), i64::MIN);
        assert_eq!(c_double_to_i64(1e30), i64::MIN);
        assert_eq!(c_double_to_i32(f64::NAN), i32::MIN);
        assert_eq!(c_double_to_i32(1e10), i32::MIN);
        assert_eq!(c_double_to_i32(2147483648.0), i32::MIN);
    }
}
