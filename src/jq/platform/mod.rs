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

/// `LC_ALL_MASK` for `newlocale`, which the libc crate doesn't define for musl
/// (musl's `<locale.h>` has `0x7fffffff`).
#[cfg(target_env = "musl")]
pub const LC_ALL_MASK: libc::c_int = 0x7fff_ffff;
#[cfg(all(unix, not(target_env = "musl")))]
pub const LC_ALL_MASK: libc::c_int = libc::LC_ALL_MASK;

/// A POSIX locale object. Windows has none: there the process's locale is the
/// environment's from the start, as jq's is (`crate::os::init`), and this is
/// always null.
#[cfg(unix)]
pub type LocaleT = libc::locale_t;
#[cfg(windows)]
pub type LocaleT = *mut libc::c_void;

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
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        eprintln!("{}", glibc_assert_text(self.message()));
        #[cfg(all(unix, not(all(target_os = "linux", target_env = "gnu"))))]
        eprintln!("{}", self.message());
        #[cfg(windows)]
        crate::os::write_stderr(format!("{}\n", self.message()).as_bytes());
        // The BSDs' abort() flushes stdio (Apple's libc is FreeBSD's); glibc's,
        // musl's and the C runtime's don't.
        #[cfg(any(target_vendor = "apple", target_os = "freebsd", target_os = "netbsd"))]
        {
            use std::io::Write;
            if let Some(flush) = BEFORE_ABORT.get() {
                flush();
            }
            let _ = std::io::stdout().flush();
        }
        crate::compat::small_core_dump();
        std::process::abort()
    }
}

/// One of jq's `assert()`s failing, as the C library's `assert` words it
/// (without the newline). `file` is the path in jq's source tree
/// (`src/jv.c`), which jq's build passes to the compiler.
/// - glibc: `jq: src/jv.c:1312: jv_string_indexes: Assertion `EXPR' failed.`,
///   translated when it aborts ([`glibc_assert_text`]).
/// - macOS: `Assertion failed: (EXPR), function F, file jv.c, line N.`; jq's
///   release build names the file without its directory.
/// - FreeBSD: the same with `src/jv.c` (Apple's libc is FreeBSD's).
/// - NetBSD: `assertion "EXPR" failed: file "src/jv.c", line N, function "F"`.
/// - musl: `Assertion failed: EXPR (src/jv.c: F: N)`.
/// - Windows (UCRT): `Assertion failed: EXPR, file src/jv.c, line N`.
pub fn assert_text(expr: &str, file: &str, function: &str, line: u32) -> String {
    if cfg!(target_vendor = "apple") {
        let base = file.rsplit('/').next().unwrap_or(file);
        format!("Assertion failed: ({expr}), function {function}, file {base}, line {line}.")
    } else if cfg!(target_os = "freebsd") {
        format!("Assertion failed: ({expr}), function {function}, file {file}, line {line}.")
    } else if cfg!(target_os = "netbsd") {
        format!(
            "assertion \"{expr}\" failed: file \"{file}\", line {line}, function \"{function}\""
        )
    } else if cfg!(target_env = "musl") {
        format!("Assertion failed: {expr} ({file}: {function}: {line})")
    } else if cfg!(windows) {
        format!("Assertion failed: {expr}, file {file}, line {line}")
    } else {
        format!("jq: {file}:{line}: {function}: Assertion `{expr}' failed.")
    }
}

/// glibc's `__assert_fail` line for one of jq's `assert()`s: `msg` is the C
/// locale's (`jq: <file>:<line>: <function>: Assertion `<expr>' failed.`),
/// and glibc translates that format for `LC_MESSAGES`, in the locale jq's
/// `setlocale(LC_ALL, "")` chose. So with German messages installed and
/// `LC_ALL=de_DE.UTF-8`, jq says `... jv_string_indexes: Zusicherung
/// »JVP_HAS_KIND(j, JV_KIND_STRING)« nicht erfüllt.`. A message of another
/// shape, or a format this can't fill in, comes back unchanged.
///
/// The name at the start is glibc's `__progname`, the last component of
/// `argv[0]` — the only place jq's output carries `argv[0]`. By default it is
/// `jq`, as for jq run by that name; with `QJ_JQ_COMPAT=1` it is this
/// process's own `argv[0]`, as for jq run by whatever name qj was.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn glibc_assert_text(msg: &str) -> String {
    unsafe extern "C" {
        fn dcgettext(
            domain: *const libc::c_char,
            msgid: *const libc::c_char,
            category: libc::c_int,
        ) -> *mut libc::c_char;
    }
    const MSGID: &std::ffi::CStr = c"%s%s%s:%u: %s%sAssertion `%s' failed.\n%n";
    let progname = assert_progname();
    let Some(args) = assert_parts(msg, &progname) else {
        return msg.to_owned();
    };
    // SAFETY: dcgettext returns `msgid` itself or a NUL-terminated translation
    // that stays valid (the catalog stays loaded); both are copied at once.
    let format = time::with_env_locale(|| unsafe {
        std::ffi::CStr::from_ptr(dcgettext(
            c"libc".as_ptr(),
            MSGID.as_ptr(),
            libc::LC_MESSAGES,
        ))
        .to_bytes()
        .to_vec()
    });
    match fill_assert_format(&format, &args) {
        Some(mut out) => {
            if out.last() == Some(&b'\n') {
                out.pop();
            }
            String::from_utf8_lossy(&out).into_owned()
        }
        None => msg.to_owned(),
    }
}

/// `__progname` as glibc's `assert()` prints it: `jq`, or in compat mode
/// glibc's `program_invocation_short_name`, which it set from this process's
/// `argv[0]` (everything after its last `/`) exactly as it does for jq.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn assert_progname() -> String {
    if !crate::compat::exactly_jq() {
        return "jq".to_owned();
    }
    unsafe extern "C" {
        static program_invocation_short_name: *const libc::c_char;
    }
    // SAFETY: glibc sets the pointer before `main` to a NUL-terminated string
    // inside `argv[0]` (or to "" without one), which lives as long as the
    // process.
    unsafe {
        let p = program_invocation_short_name;
        if p.is_null() {
            return String::new();
        }
        String::from_utf8_lossy(std::ffi::CStr::from_ptr(p).to_bytes()).into_owned()
    }
}

/// `__assert_fail`'s arguments for its format, from the C locale's line:
/// `__progname`, `": "` (nothing when the name is empty), the file, the line,
/// the function, `": "`, the expression.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn assert_parts<'a>(msg: &'a str, progname: &'a str) -> Option<[&'a str; 7]> {
    let rest = msg.strip_prefix("jq: ")?;
    let (file, rest) = rest.split_once(':')?;
    let (line, rest) = rest.split_once(": ")?;
    let (function, rest) = rest.split_once(": Assertion `")?;
    let expr = rest.strip_suffix("' failed.")?;
    let digits = !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit());
    let sep = if progname.is_empty() { "" } else { ": " };
    digits.then_some([progname, sep, file, line, function, ": ", expr])
}

/// A (translated) `__assert_fail` format filled in with `args`: `%s` and `%u`
/// take the arguments in order (`%N$s` the Nth), `%n` prints nothing and `%%`
/// is a percent sign. `None` for anything else.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn fill_assert_format(format: &[u8], args: &[&str]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut next = 0;
    let mut i = 0;
    while i < format.len() {
        if format[i] != b'%' {
            out.push(format[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let digits = format[j..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let mut arg = None;
        if digits > 0 && format.get(j + digits) == Some(&b'$') {
            let n: usize = std::str::from_utf8(&format[j..j + digits])
                .ok()?
                .parse()
                .ok()?;
            arg = Some(n.checked_sub(1)?);
            j += digits + 1;
        }
        match format.get(j)? {
            b's' | b'u' => {
                let k = arg.unwrap_or(next);
                out.extend_from_slice(args.get(k)?.as_bytes());
                next = k + 1;
            }
            b'n' => {}
            b'%' => out.push(b'%'),
            _ => return None,
        }
        i = j + 1;
    }
    Some(out)
}

/// What [`Error::abort_process`] calls before aborting on macOS and the BSDs,
/// where jq's `abort()` flushes stdio: the CLI registers a function that flushes its
/// own stdout buffer ([`set_before_abort`]).
static BEFORE_ABORT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Registers the function [`Error::abort_process`] calls to flush buffered
/// standard output before aborting, on macOS and the BSDs only (glibc's and
/// musl's `abort()` don't flush, so there the buffered output is lost, as in
/// jq). Only the
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

/// `strerror(errnum)`: the C library's message for an `errno`, which jq prints
/// verbatim (Rust's `io::Error` adds " (os error N)"), in the environment's
/// locale as after jq's `setlocale(LC_ALL, "")`: glibc translates it for
/// `LC_MESSAGES` when the language's messages are installed (`jq . missing`
/// under `LC_ALL=de_DE.UTF-8` says "Datei oder Verzeichnis nicht gefunden");
/// Apple's libc doesn't translate these.
#[cfg(windows)]
pub fn strerror(errnum: i32) -> Vec<u8> {
    // SAFETY: UCRT's `strerror` returns this thread's NUL-terminated buffer,
    // copied at once (its text for an unknown number is "Unknown error").
    unsafe { std::ffi::CStr::from_ptr(libc::strerror(errnum)) }
        .to_bytes()
        .to_vec()
}

/// `strerror(errnum)`, as above.
#[cfg(unix)]
pub fn strerror(errnum: i32) -> Vec<u8> {
    time::with_env_locale(|| {
        let mut buf = [0 as libc::c_char; 512];
        // SAFETY: `buf` is writable for its length; strerror_r (the XSI
        // version, which the libc crate binds on glibc too) NUL-terminates it,
        // with the C library's own text for an unknown number too.
        let rc = unsafe { libc::strerror_r(errnum, buf.as_mut_ptr(), buf.len()) };
        if rc != 0 && buf[0] == 0 {
            return format!("Unknown error: {errnum}").into_bytes();
        }
        // SAFETY: NUL-terminated by strerror_r.
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_bytes()
            .to_vec()
    })
}

/// C's implicit `double` to `long`/`time_t` conversion, as compilers emit it. It's
/// undefined behavior in C for NaN and out-of-range values; what the hardware
/// instruction does then is what jq does: aarch64's `fcvtzs` saturates and maps NaN to
/// 0 (Rust's `as`), x86-64's `cvttsd2si` returns `i64::MIN` ("integer indefinite").
pub(crate) fn c_double_to_i64(d: f64) -> i64 {
    // [-2^63, 2^63): `i64::MIN as f64` is exactly -2^63.
    #[cfg(target_arch = "x86_64")]
    if !((i64::MIN as f64)..-(i64::MIN as f64)).contains(&d) {
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

    /// jq's assert lines on glibc, and the format as glibc's German messages
    /// translate it (`__assert_fail`, `LC_ALL=de_DE.UTF-8`).
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn glibc_assert_lines_fill_translated_formats() {
        let line = "jq: src/jv.c:1312: jv_string_indexes: Assertion `JVP_HAS_KIND(j, JV_KIND_STRING)' failed.";
        let args = assert_parts(line, "jq").expect("an assert line");
        assert_eq!(
            args,
            [
                "jq",
                ": ",
                "src/jv.c",
                "1312",
                "jv_string_indexes",
                ": ",
                "JVP_HAS_KIND(j, JV_KIND_STRING)"
            ]
        );
        let english = fill_assert_format(b"%s%s%s:%u: %s%sAssertion `%s' failed.\n%n", &args);
        assert_eq!(english, Some(format!("{line}\n").into_bytes()));
        let german = "%s%s%s:%u: %s%sZusicherung \u{bb}%s\u{ab} nicht erf\u{fc}llt.\n%n";
        assert_eq!(
            fill_assert_format(german.as_bytes(), &args).map(String::from_utf8),
            Some(Ok("jq: src/jv.c:1312: jv_string_indexes: Zusicherung \u{bb}JVP_HAS_KIND(j, JV_KIND_STRING)\u{ab} nicht erf\u{fc}llt.\n".to_owned()))
        );
        let positional = fill_assert_format(b"%7$s (%3$s:%4$u)%%", &args);
        assert_eq!(
            positional,
            Some(b"JVP_HAS_KIND(j, JV_KIND_STRING) (src/jv.c:1312)%".to_vec())
        );
        assert_eq!(fill_assert_format(b"%d", &args), None);
        assert_eq!(
            assert_parts(
                "Assertion failed: (x), function f, file jv.c, line 1.",
                "jq"
            ),
            None
        );
        // Without translations (the C locale), the line comes back as it is.
        assert_eq!(glibc_assert_text(line), line);
        // `__progname` is argv[0]'s last component, and an empty one drops
        // its ": " too, as glibc's format does.
        let named = assert_parts(line, "myjq").expect("an assert line");
        assert_eq!(
            fill_assert_format(b"%s%s%s:%u: %s%sAssertion `%s' failed.\n%n", &named),
            Some(format!("my{line}\n").into_bytes())
        );
        let unnamed = assert_parts(line, "").expect("an assert line");
        assert_eq!(
            fill_assert_format(b"%s%s%s:%u: %s%sAssertion `%s' failed.\n%n", &unnamed),
            Some(format!("{}\n", &line["jq: ".len()..]).into_bytes())
        );
    }

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
