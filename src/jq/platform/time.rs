//! Port of jq 1.8.1's date/time C builtins (`builtin.c`): `strptime/1`, `strftime/1`,
//! `strflocaltime/1`, `mktime`, `gmtime`, `localtime` and `now`, plus the `tm2jv` /
//! `jv2tm` conversions. `todate`, `fromdate`, `dateadd` and friends are jq code in
//! `builtin.jq` on top of these.
//!
//! Like jq, these call the C library (`strptime`, `strftime`, `timegm`, `mktime`,
//! `gmtime_r`, `localtime_r`, `gettimeofday`), so results depend on the platform the
//! same way jq's do: macOS and glibc differ (for example in `strptime`'s handling of
//! `%s`, `%z`, `%Z` and `%j`), and both honor `TZ`. The macOS behavior was checked
//! against the jq 1.8.1 binary; the glibc paths follow the C source.
//!
//! # Process-wide state
//!
//! - **Locale.** jq's `main()` calls `setlocale(LC_ALL, "")`, so on macOS `strftime`
//!   and `strptime` use the locale from `LC_ALL`/`LC_TIME`/`LANG` (`%A` is "Donnerstag"
//!   under `de_DE.UTF-8`; `%x` is `01/01/1970` under `en_US.UTF-8` but `01/01/70` in the
//!   C locale). This module gets the same effect without touching the global locale: it
//!   creates that locale once with `newlocale(LC_ALL_MASK, "", 0)` and installs it on
//!   the calling thread with `uselocale` around each libc call. jq's Linux release
//!   binary links glibc statically, and there `setlocale` changes every category but
//!   `LC_TIME`: its messages are translated and its bytes classified as the
//!   environment says, but its dates are always the C locale's, so on Linux the locale
//!   here is the environment's with `LC_TIME` from C ([`date_locale`]).
//! - **TZ.** On macOS `f_strftime` sets `TZ=UTC` around its `strftime` call (Apple's
//!   `%z` ignores `tm_gmtoff`), so `%Z %z %s` print `UTC +0000 <epoch>` for UTC times.
//!   This is reproduced by setting the environment variable; every function here holds
//!   one process-wide lock while calling into libc time code, so this is safe against
//!   the other time functions and against Rust's `std::env`, but not against foreign
//!   code reading the environment concurrently.
//!
//! # Mapping to jq values
//!
//! The builtin layer converts its input with [`TimeInput`] and format/string arguments
//! to `Option<&str>` (`None` when the argument is not a string), and the functions
//! raise jq's exact errors in `builtin.c`'s order. Broken-down times are
//! [`BrokenDownTime`] arrays of 8 numbers; `strptime` can add a 9th element (see
//! [`Parsed`]).

use super::{Error, utf8};
use libc::{c_char, c_int};
use std::ffi::{CStr, CString};
use std::ptr;
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::{Mutex, MutexGuard};

/// The C library's time functions, as jq calls them on each platform.
#[cfg(unix)]
mod sys {
    use libc::{c_char, tm};
    pub use libc::{gmtime_r, localtime_r, mktime, strftime, timegm};

    // POSIX's `strptime`, which the libc crate binds on Linux and macOS but
    // not on FreeBSD.
    unsafe extern "C" {
        pub fn strptime(s: *const c_char, format: *const c_char, tm: *mut tm) -> *mut c_char;
    }
}

/// On Windows, jq's release binary (MinGW, on UCRT) has no `timegm`, so
/// `my_mktime` is `_mkgmtime`; no `gmtime_r`/`localtime_r`, so `f_gmtime` and
/// `f_localtime` call `gmtime`/`localtime`; and no `strptime`, so it builds
/// its own (`strptime.c`, compiled by build.rs). `time_t` is 64-bit.
#[cfg(windows)]
mod sys {
    use libc::{c_char, size_t, time_t, tm};

    unsafe extern "C" {
        #[link_name = "_mkgmtime64"]
        pub fn timegm(tm: *mut tm) -> time_t;
        #[link_name = "_mktime64"]
        pub fn mktime(tm: *mut tm) -> time_t;
        #[link_name = "_gmtime64"]
        fn gmtime(t: *const time_t) -> *mut tm;
        #[link_name = "_localtime64"]
        fn localtime(t: *const time_t) -> *mut tm;
        pub fn strftime(
            s: *mut c_char,
            max: size_t,
            format: *const c_char,
            tm: *const tm,
        ) -> size_t;
        #[link_name = "jq_strptime"]
        pub fn strptime(s: *const c_char, format: *const c_char, tm: *mut tm) -> *mut c_char;
    }

    /// `gmtime`, copied out of the C runtime's per-thread result.
    pub unsafe fn gmtime_r(t: *const time_t, out: *mut tm) -> *mut tm {
        // SAFETY: the caller's pointers are valid; the result is this
        // thread's, valid until its next call.
        unsafe { copy_out(gmtime(t), out) }
    }

    /// `localtime`, as [`gmtime_r`].
    pub unsafe fn localtime_r(t: *const time_t, out: *mut tm) -> *mut tm {
        // SAFETY: as in `gmtime_r`.
        unsafe { copy_out(localtime(t), out) }
    }

    unsafe fn copy_out(r: *mut tm, out: *mut tm) -> *mut tm {
        if r.is_null() {
            return r;
        }
        // SAFETY: both point to valid `tm`s.
        unsafe { *out = *r };
        out
    }
}

/// jq's message for a non-number input to `gmtime`/`localtime`, which differs
/// between its `gmtime_r` and `gmtime` builds (Windows').
#[cfg(unix)]
const GMTIME_TYPE: &str = "gmtime() requires numeric inputs";
#[cfg(windows)]
const GMTIME_TYPE: &str = "gmtime requires numeric inputs";
#[cfg(unix)]
const LOCALTIME_TYPE: &str = "localtime() requires numeric inputs";
#[cfg(windows)]
const LOCALTIME_TYPE: &str = "localtime requires numeric inputs";

/// jq's broken-down time (`tm2jv`): `[year, month (0-11), day of month, hours, minutes,
/// seconds, day of week (0 = Sunday), day of year (0-365)]`. Every element is an
/// integer except the seconds that `gmtime`/`localtime` return, which keep the input's
/// fractional part (NaN seconds, printed by jq as `null`, for a NaN input).
pub type BrokenDownTime = [f64; 8];

/// A jq value as the time builtins see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimeInput<'a> {
    /// A number (seconds since the Unix epoch).
    Number(f64),
    /// An array, one entry per element: `Some(n)` for a number, `None` for anything
    /// else. Only the first 8 elements are read (`jv2tm`), so the builtin layer may pass
    /// just those. A shorter array leaves the remaining fields 0.
    Array(&'a [Option<f64>]),
    /// Any other value.
    Other,
}

/// Result of [`strptime`].
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// The broken-down time.
    pub tm: BrokenDownTime,
    /// If `strptime` stopped at trailing whitespace, jq appends the unparsed rest of
    /// the input as a 9th array element: `"2015-03-05T23:51:47Z \n" | strptime(...)`
    /// is `[2015,2,5,23,51,47,4,63," \n"]`.
    pub rest: Option<String>,
}

const ERR_CONVERT: &str = "error converting number of seconds since epoch to datetime";

/// Text of the macOS `assert()` that `f_strflocaltime` trips when `localtime` fails
/// (`jv_array_get` on the invalid result).
#[cfg(target_vendor = "apple")]
const ABORT_ARRAY_GET: &str = "Assertion failed: (JVP_HAS_KIND(j, JV_KIND_ARRAY)), function jv_array_get, file jv.c, line 1006.";
#[cfg(all(unix, not(target_vendor = "apple")))]
const ABORT_ARRAY_GET: &str =
    "jq: src/jv.c:1006: jv_array_get: Assertion `JVP_HAS_KIND(j, JV_KIND_ARRAY)' failed.";
/// UCRT's (`_wassert`).
#[cfg(windows)]
const ABORT_ARRAY_GET: &str =
    "Assertion failed: JVP_HAS_KIND(j, JV_KIND_ARRAY), file src/jv.c, line 1006";

/// Text of the macOS `assert()` in `set_tm_yday` (only reachable on macOS).
#[cfg(unix)]
const ABORT_SET_TM_YDAY: &str = "Assertion failed: (yday == tm->tm_yday || tm->tm_yday == 367), function set_tm_yday, file builtin.c, line 1549.";

// ---------------------------------------------------------------------------------
// Global state: one lock for libc time calls, and the environment's locale.
// ---------------------------------------------------------------------------------

static TIME_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    TIME_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
struct Locale(super::LocaleT);

// SAFETY: a locale_t is an immutable, heap-allocated description once created; POSIX
// allows using it from any thread.
#[cfg(unix)]
unsafe impl Send for Locale {}
#[cfg(unix)]
unsafe impl Sync for Locale {}

#[cfg(unix)]
static ENV_LOCALE: OnceLock<Locale> = OnceLock::new();

/// The locale `setlocale(LC_ALL, "")` would select, or the C locale if the
/// environment names one that doesn't exist (setlocale then fails and leaves "C").
/// None on Windows (see [`super::LocaleT`]).
#[cfg(windows)]
fn env_locale() -> super::LocaleT {
    ptr::null_mut()
}

/// The locale `setlocale(LC_ALL, "")` would select, or the C locale if the
/// environment names one that doesn't exist (setlocale then fails and leaves "C").
#[cfg(unix)]
fn env_locale() -> super::LocaleT {
    ENV_LOCALE
        .get_or_init(|| {
            // SAFETY: newlocale with a NULL base allocates a new locale object.
            unsafe {
                let loc = libc::newlocale(super::LC_ALL_MASK, c"".as_ptr(), ptr::null_mut());
                if !loc.is_null() {
                    return Locale(loc);
                }
                Locale(libc::newlocale(
                    super::LC_ALL_MASK,
                    c"C".as_ptr(),
                    ptr::null_mut(),
                ))
            }
        })
        .0
}

/// Run `f` with the calling thread's locale as jq's after its
/// `setlocale(LC_ALL, "")` (see [`env_locale`]).
#[cfg(unix)]
pub(super) fn with_env_locale<R>(f: impl FnOnce() -> R) -> R {
    with_locale(env_locale(), f)
}

/// The locale jq's date builtins run in: the environment's ([`env_locale`]),
/// except on Linux, where jq's release binary (glibc linked statically) keeps
/// `LC_TIME` in the C locale after `setlocale(LC_ALL, "")`, whatever the
/// environment says: `LC_ALL=de_DE.UTF-8 jq -n '0 | strftime("%A")'` is
/// "Thursday" there, and `strptime("%A")` doesn't read "Donnerstag". Its
/// `LC_CTYPE` does follow the environment, which `strptime` uses to skip
/// spaces and jq to check what follows the date.
fn date_locale() -> super::LocaleT {
    #[cfg(target_os = "linux")]
    {
        static DATE_LOCALE: OnceLock<Locale> = OnceLock::new();
        DATE_LOCALE
            .get_or_init(|| {
                // SAFETY: duplocale copies the environment's locale object, and
                // newlocale takes the copy over (returning it modified, or NULL
                // with the copy still ours to free).
                unsafe {
                    let base = libc::duplocale(env_locale());
                    if base.is_null() {
                        return Locale(env_locale());
                    }
                    let loc = libc::newlocale(libc::LC_TIME_MASK, c"C".as_ptr(), base);
                    if loc.is_null() {
                        libc::freelocale(base);
                        return Locale(env_locale());
                    }
                    Locale(loc)
                }
            })
            .0
    }
    #[cfg(not(target_os = "linux"))]
    {
        env_locale()
    }
}

/// Run `f` with `loc` as the calling thread's locale.
///
/// NetBSD's C library has no `uselocale`, so there `f` runs in the process's
/// locale, which qj leaves as C. Windows has no locale objects either, and
/// there the process's locale is the environment's (see [`super::LocaleT`]).
#[cfg(any(target_os = "netbsd", windows))]
fn with_locale<R>(_loc: super::LocaleT, f: impl FnOnce() -> R) -> R {
    f()
}

/// Run `f` with `loc` as the calling thread's locale.
#[cfg(all(unix, not(target_os = "netbsd")))]
fn with_locale<R>(loc: super::LocaleT, f: impl FnOnce() -> R) -> R {
    if loc.is_null() {
        return f();
    }
    // SAFETY: `loc` is a valid locale object; uselocale only changes this thread's
    // current locale, which is restored before returning.
    let old = unsafe { libc::uselocale(loc) };
    let r = f();
    unsafe { libc::uselocale(old) };
    r
}

/// Run `f` with `TZ=UTC`, restoring `TZ` afterwards, like `f_strftime` on macOS.
/// Must be called with `TIME_LOCK` held.
#[cfg(target_vendor = "apple")]
fn with_tz_utc<R>(f: impl FnOnce() -> R) -> R {
    let saved = std::env::var_os("TZ");
    // SAFETY: the caller holds TIME_LOCK, so no libc time function in this process
    // is reading TZ; std's own environment lock covers Rust readers.
    unsafe { std::env::set_var("TZ", "UTC") };
    let r = f();
    match saved {
        Some(tz) => unsafe { std::env::set_var("TZ", tz) },
        None => unsafe { std::env::remove_var("TZ") },
    }
    r
}

// ---------------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------------

/// `time_t secs = fsecs;` in `f_gmtime`/`f_localtime` (see [`super::c_double_to_i64`]).
///
/// The libc crate marks `time_t` deprecated on musl, where it plans to make it
/// 64-bit on 32-bit targets too, as musl 1.2 did.
#[allow(deprecated)]
fn double_to_time_t(d: f64) -> libc::time_t {
    super::c_double_to_i64(d) as libc::time_t
}

/// Port of `tm2jv`.
fn tm2jv(tm: &libc::tm) -> BrokenDownTime {
    [
        tm.tm_year.wrapping_add(1900) as f64,
        tm.tm_mon as f64,
        tm.tm_mday as f64,
        tm.tm_hour as f64,
        tm.tm_min as f64,
        tm.tm_sec as f64,
        tm.tm_wday as f64,
        tm.tm_yday as f64,
    ]
}

fn zeroed_tm() -> libc::tm {
    // SAFETY: struct tm is plain data (ints, a long, a char pointer); all-zero is how
    // jq initializes it (memset) and a valid value.
    unsafe { std::mem::zeroed() }
}

/// Port of `jv2tm`: `None` if an element among the first 8 is not a number or is NaN.
/// Each value is clamped to the `int` range and truncated; then the struct is
/// normalized with `timegm` (UTC) or `mktime` with `tm_isdst = -1` (local time),
/// which recomputes the day of week and day of year from the date.
fn jv2tm(fields: &[Option<f64>], localtime: bool) -> Option<libc::tm> {
    let mut tm = zeroed_tm();
    for (i, field) in fields.iter().take(8).enumerate() {
        let mut d = match field {
            Some(d) if !d.is_nan() => *d,
            _ => return None,
        };
        if i == 0 {
            d -= 1900.0;
        }
        let v = if d < c_int::MIN as f64 {
            c_int::MIN
        } else if d > c_int::MAX as f64 {
            c_int::MAX
        } else {
            d as c_int
        };
        match i {
            0 => tm.tm_year = v,
            1 => tm.tm_mon = v,
            2 => tm.tm_mday = v,
            3 => tm.tm_hour = v,
            4 => tm.tm_min = v,
            5 => tm.tm_sec = v,
            6 => tm.tm_wday = v,
            _ => tm.tm_yday = v,
        }
    }
    // SAFETY: valid struct tm; both functions normalize it in place.
    unsafe {
        if localtime {
            tm.tm_isdst = -1;
            sys::mktime(&mut tm);
        } else {
            // Without `timegm` (Windows), jq leaves a UTC time as given.
            #[cfg(unix)]
            sys::timegm(&mut tm);
        }
    }
    Some(tm)
}

/// Port of `set_tm_wday`: Gauss's algorithm, including its documented wrong answers
/// for January and February of years divisible by 100 (e.g. 1900-01-01 gives Sunday).
fn set_tm_wday(tm: &mut libc::tm) {
    let full_year = 1900i32.wrapping_add(tm.tm_year);
    let century = full_year / 100;
    let mut year = full_year % 100;
    if tm.tm_mon < 2 {
        year -= 1;
    }
    let mut mon = tm.tm_mon.wrapping_sub(1);
    if mon < 1 {
        mon = mon.wrapping_add(12);
    }
    let wday = (tm
        .tm_mday
        .wrapping_add((2.6 * mon as f64 - 0.2).floor() as c_int)
        .wrapping_add(year)
        .wrapping_add((year as f64 / 4.0).floor() as c_int)
        .wrapping_add((century as f64 / 4.0).floor() as c_int)
        .wrapping_sub(century.wrapping_mul(2)))
        % 7;
    tm.tm_wday = if wday < 0 { wday + 7 } else { wday };
}

/// Port of `set_tm_yday`. `Err` if jq's `assert(yday == tm->tm_yday || tm->tm_yday ==
/// 367)` fails, which aborts jq.
#[cfg(unix)]
fn set_tm_yday(tm: &mut libc::tm) -> Result<(), Error> {
    const D: [c_int; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut mon = tm.tm_mon;
    let year = 1900i32.wrapping_add(tm.tm_year);
    let leap_day =
        (tm.tm_mon > 1 && ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0)) as c_int;
    // "Bound check index into d[]"
    if mon < 0 {
        mon = mon.wrapping_neg();
    }
    if mon > 11 {
        mon %= 12;
    }
    let yday = D[mon.clamp(0, 11) as usize]
        .wrapping_add(leap_day)
        .wrapping_add(tm.tm_mday)
        .wrapping_sub(1);
    if !(yday == tm.tm_yday || tm.tm_yday == 367) {
        return Err(Error::Abort(ABORT_SET_TM_YDAY.to_owned()));
    }
    tm.tm_yday = yday;
    Ok(())
}

/// A jq string as C sees it (`jv_string_value`): cut at the first NUL.
fn c_string(s: &str) -> CString {
    CString::new(utf8::until_nul(s)).expect("no interior NUL after truncation")
}

// ---------------------------------------------------------------------------------
// The builtins
// ---------------------------------------------------------------------------------

fn gmtime_unlocked(fsecs: f64) -> Result<BrokenDownTime, Error> {
    let secs = double_to_time_t(fsecs);
    let mut tm = zeroed_tm();
    // SAFETY: valid pointers to locals.
    if unsafe { sys::gmtime_r(&secs, &mut tm) }.is_null() {
        return Err(Error::msg(ERR_CONVERT));
    }
    let mut r = tm2jv(&tm);
    r[5] += fsecs - fsecs.floor();
    Ok(r)
}

fn localtime_unlocked(fsecs: f64) -> Result<BrokenDownTime, Error> {
    let secs = double_to_time_t(fsecs);
    let mut tm = zeroed_tm();
    // SAFETY: valid pointers to locals.
    if unsafe { sys::localtime_r(&secs, &mut tm) }.is_null() {
        return Err(Error::msg(ERR_CONVERT));
    }
    let mut r = tm2jv(&tm);
    r[5] += fsecs - fsecs.floor();
    Ok(r)
}

/// Port of `f_gmtime`: seconds since the epoch to a UTC broken-down time.
///
/// Errors: `gmtime() requires numeric inputs`; `error converting number of seconds
/// since epoch to datetime` when `gmtime_r` fails (the year overflows an `int`, or the
/// input is infinite). NaN converts like 0 on aarch64 and keeps NaN seconds.
pub fn gmtime(input: TimeInput) -> Result<BrokenDownTime, Error> {
    let TimeInput::Number(fsecs) = input else {
        return Err(Error::msg(GMTIME_TYPE));
    };
    let _guard = lock();
    gmtime_unlocked(fsecs)
}

/// Port of `f_localtime`: like [`gmtime`] in the local time zone (`TZ`).
///
/// Errors: `localtime() requires numeric inputs`, and the conversion error.
pub fn localtime(input: TimeInput) -> Result<BrokenDownTime, Error> {
    let TimeInput::Number(fsecs) = input else {
        return Err(Error::msg(LOCALTIME_TYPE));
    };
    let _guard = lock();
    localtime_unlocked(fsecs)
}

/// Port of `f_mktime`: a UTC broken-down time to seconds since the epoch, with
/// `timegm`. Out-of-range fields are normalized (`[2015,14,5]` is 2016-03-05) and
/// fractional fields truncated.
///
/// Errors: `mktime requires array inputs`; `mktime requires parsed datetime inputs`
/// (non-number or NaN element); `invalid gmtime representation` when `timegm` returns
/// -1, which includes the valid time 1969-12-31T23:59:59 and, on macOS, anything before
/// 1900; `mktime not supported on this platform` when it returns -2 (jq's sentinel,
/// hit by 1969-12-31T23:59:58).
pub fn mktime(input: TimeInput) -> Result<f64, Error> {
    let TimeInput::Array(fields) = input else {
        return Err(Error::msg("mktime requires array inputs"));
    };
    let _guard = lock();
    let mut tm =
        jv2tm(fields, false).ok_or_else(|| Error::msg("mktime requires parsed datetime inputs"))?;
    // my_mktime: timegm(), or on Windows _mkgmtime().
    // SAFETY: valid struct tm.
    let t = unsafe { sys::timegm(&mut tm) };
    if t == -1 {
        return Err(Error::msg("invalid gmtime representation"));
    }
    if t == -2 {
        return Err(Error::msg("mktime not supported on this platform"));
    }
    Ok(t as f64)
}

/// `strftime(buf, strlen(fmt) + 100, fmt, tm)` in `loc`, with jq's failure check.
fn format_tm(tm: &libc::tm, fmt: &str, loc: super::LocaleT, name: &str) -> Result<String, Error> {
    let cfmt = c_string(fmt);
    let fmt_not_empty = !cfmt.as_bytes().is_empty();
    let max_size = cfmt.as_bytes().len() + 100;
    let mut buf = vec![0u8; max_size];
    // SAFETY: `buf` has `max_size` bytes; `cfmt` is NUL-terminated; `tm` is valid.
    let n = with_locale(loc, || unsafe {
        sys::strftime(buf.as_mut_ptr() as *mut c_char, max_size, cfmt.as_ptr(), tm)
    });
    // "POSIX doesn't provide errno values for strftime() failures; weird"
    if (n == 0 && fmt_not_empty) || n > max_size {
        return Err(Error::Msg(format!("{name}/1: unknown system failure")));
    }
    Ok(utf8::string_sized(&buf[..n]))
}

fn strftime_in(
    input: TimeInput,
    format: Option<&str>,
    loc: super::LocaleT,
) -> Result<String, Error> {
    let _guard = lock();
    let from_number;
    let fields = match input {
        TimeInput::Number(secs) => {
            from_number = gmtime_unlocked(secs)?.map(Some);
            &from_number[..]
        }
        TimeInput::Array(fields) => fields,
        TimeInput::Other => return Err(Error::msg("strftime/1 requires parsed datetime inputs")),
    };
    let Some(fmt) = format else {
        return Err(Error::msg("strftime/1 requires a string format"));
    };
    let tm = jv2tm(fields, false)
        .ok_or_else(|| Error::msg("strftime/1 requires parsed datetime inputs"))?;
    #[cfg(target_vendor = "apple")]
    {
        // "Apple Libc (as of version 1669.40.2) contains a bug which causes it to
        // ignore the `tm.tm_gmtoff` in favor of the global timezone. To print the
        // proper timezone offset we temporarily switch the TZ to UTC."
        with_tz_utc(|| format_tm(&tm, fmt, loc, "strftime"))
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        format_tm(&tm, fmt, loc, "strftime")
    }
}

/// Port of `f_strftime`: format a UTC time. A number input goes through [`gmtime`]
/// first (its errors propagate).
///
/// Errors, in order: `strftime/1 requires parsed datetime inputs` (input neither
/// number nor array); `strftime/1 requires a string format`; `strftime/1 requires
/// parsed datetime inputs` (bad array element); `strftime/1: unknown system failure`
/// when the output is empty for a non-empty format or longer than `strlen(format) +
/// 100` bytes (`"%c%c%c%c%c"` fails this way).
///
/// The format is cut at its first NUL, and non-UTF-8 output (from a non-UTF-8 locale)
/// gets U+FFFD replacements, as in jq.
pub fn strftime(input: TimeInput, format: Option<&str>) -> Result<String, Error> {
    strftime_in(input, format, date_locale())
}

fn strflocaltime_in(
    input: TimeInput,
    format: Option<&str>,
    loc: super::LocaleT,
) -> Result<String, Error> {
    let _guard = lock();
    let from_number;
    let fields = match input {
        TimeInput::Number(secs) => match localtime_unlocked(secs) {
            Ok(bt) => {
                from_number = bt.map(Some);
                &from_number[..]
            }
            // f_strflocaltime doesn't check localtime's result: with a string format it
            // passes the invalid value to jv2tm, whose jv_array_get asserts.
            Err(_) if format.is_some() => return Err(Error::Abort(ABORT_ARRAY_GET.to_owned())),
            Err(_) => return Err(Error::msg("strflocaltime/1 requires a string format")),
        },
        TimeInput::Array(fields) => fields,
        TimeInput::Other => {
            return Err(Error::msg(
                "strflocaltime/1 requires parsed datetime inputs",
            ));
        }
    };
    let Some(fmt) = format else {
        return Err(Error::msg("strflocaltime/1 requires a string format"));
    };
    let tm = jv2tm(fields, true)
        .ok_or_else(|| Error::msg("strflocaltime/1 requires parsed datetime inputs"))?;
    format_tm(&tm, fmt, loc, "strflocaltime")
}

/// Port of `f_strflocaltime`: format a local time. A number input goes through
/// [`localtime`]; an array is normalized with `mktime`, so `%Z`/`%z` reflect DST at
/// that date.
///
/// When `mktime` can't represent the date (before 1900 on macOS), `tm_zone` stays NULL
/// and `%Z` falls back to libc's global `tzname`, which every local-time conversion
/// updates. So the result depends on the process's earlier time calls, in jq too:
/// `TZ=Asia/Tokyo jq -nc '([] | strflocaltime("%Z")), (-3000000000 | localtime |
/// empty), ([] | strflocaltime("%Z"))'` prints "JST" then "LMT". This makes the same
/// libc calls as jq, so a sequential run matches; a different call order (parallel
/// evaluation) can differ.
///
/// Errors: as [`strftime`] with `strflocaltime/1` in the messages, except that a number
/// whose `localtime` conversion fails gives `strflocaltime/1 requires a string format`
/// if the format isn't a string and otherwise an [`Error::Abort`]: jq 1.8.1 crashes on
/// `1e30 | strflocaltime("%c")`.
pub fn strflocaltime(input: TimeInput, format: Option<&str>) -> Result<String, Error> {
    strflocaltime_in(input, format, date_locale())
}

fn strptime_in(
    input: Option<&str>,
    format: Option<&str>,
    loc: super::LocaleT,
) -> Result<Parsed, Error> {
    let (Some(input), Some(fmt)) = (input, format) else {
        return Err(Error::msg(
            "strptime/1 requires string inputs and arguments",
        ));
    };
    let _guard = lock();
    let cinput = c_string(input);
    let cfmt = c_string(fmt);
    let mut tm = zeroed_tm();
    tm.tm_wday = 8; // sentinel
    tm.tm_yday = 367; // sentinel
    let (end, bad_end) = with_locale(loc, || {
        // SAFETY: NUL-terminated strings and a valid struct tm. `end` points into
        // `cinput` (or is NULL).
        unsafe {
            let end = sys::strptime(cinput.as_ptr(), cfmt.as_ptr(), &mut tm);
            let bad_end = end.is_null() || (*end != 0 && libc::isspace(*end as u8 as c_int) == 0);
            (end, bad_end)
        }
    });
    if bad_end {
        return Err(Error::Msg(format!(
            "date \"{}\" does not match format \"{}\"",
            utf8::until_nul(input),
            utf8::until_nul(fmt)
        )));
    }
    #[cfg(target_vendor = "apple")]
    {
        // "Apple has made it worse [...] we always use our functions to set these on
        // OS X, and document that %u and %j are unsupported on OS X."
        set_tm_wday(&mut tm);
        set_tm_yday(&mut tm)?;
    }
    // jq's own strptime (Windows'): `set_tm_wday` only.
    #[cfg(windows)]
    set_tm_wday(&mut tm);
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        if tm.tm_wday == 8 && tm.tm_mday != 0 && (0..=11).contains(&tm.tm_mon) {
            set_tm_wday(&mut tm);
        }
        if tm.tm_yday == 367 && tm.tm_mday != 0 && (0..=11).contains(&tm.tm_mon) {
            set_tm_yday(&mut tm)?;
        }
    }
    // SAFETY: `end` is non-null (checked above) and points into `cinput`.
    let rest = unsafe { CStr::from_ptr(end) }.to_bytes();
    Ok(Parsed {
        tm: tm2jv(&tm),
        rest: (!rest.is_empty()).then(|| utf8::string_sized(rest)),
    })
}

/// Port of `f_strptime`: parse `input` with the C library's `strptime`, then (on macOS,
/// always; elsewhere only if `strptime` left them unset) compute the day of week and
/// day of year with jq's own `set_tm_wday`/`set_tm_yday`.
///
/// `None` for either argument means it isn't a string: `strptime/1 requires string
/// inputs and arguments`. If parsing fails, or stops anywhere but at whitespace or the
/// end: `date "<input>" does not match format "<format>"`. Both strings are cut at
/// their first NUL, as C sees them.
///
/// [`Error::Abort`] on macOS when `strptime` set a day of year that disagrees with
/// jq's computation (`"100" | strptime("%j")`): jq 1.8.1 fails an assertion there.
pub fn strptime(input: Option<&str>, format: Option<&str>) -> Result<Parsed, Error> {
    strptime_in(input, format, date_locale())
}

/// Port of `f_now`: `gettimeofday` as seconds with microsecond resolution
/// (MinGW's, on Windows, reads the system time as `SystemTime` does).
#[cfg(windows)]
pub fn now() -> f64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    since.as_secs() as f64 + since.subsec_micros() as f64 / 1000000.0
}

/// Port of `f_now`: `gettimeofday` as seconds with microsecond resolution.
#[cfg(unix)]
pub fn now() -> f64 {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    // SAFETY: valid pointer; a NULL timezone argument is allowed.
    if unsafe { libc::gettimeofday(&mut tv, ptr::null_mut()) } == -1 {
        // SAFETY: time(NULL) is always safe.
        return unsafe { libc::time(ptr::null_mut()) } as f64;
    }
    tv.tv_sec as f64 + tv.tv_usec as f64 / 1000000.0
}

#[cfg(test)]
mod tests;
