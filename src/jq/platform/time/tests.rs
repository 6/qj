//! Tests for the date/time port. Expectations come from the jq 1.8.1 binary on macOS
//! (`jq -nc '<input> | <builtin>'` with the stated `TZ`/`LC_ALL`). Tests of behavior
//! that is macOS libc specific are `cfg(target_os = "macos")`; the rest hold on glibc
//! too.

use super::*;
use std::sync::Mutex;

// Not bound by the `libc` crate.
unsafe extern "C" {
    fn tzset();
}

/// Serializes these tests: some change `TZ`, which the others read through libc.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` with `TZ` set (`TIME_LOCK` is held while the variable changes).
///
/// `tzset()` makes the change visible to glibc's `localtime_r`, which (unlike macOS's)
/// doesn't re-read `TZ` once initialized. jq never changes `TZ` mid-process there, so
/// only the tests need this.
fn with_tz<R>(tz: &str, f: impl FnOnce() -> R) -> R {
    let saved = {
        let _g = lock();
        let saved = std::env::var_os("TZ");
        // SAFETY: TIME_LOCK is held and the tests that read TZ are serialized.
        unsafe {
            std::env::set_var("TZ", tz);
            tzset();
        }
        saved
    };
    let r = f();
    let _g = lock();
    // SAFETY: as above.
    unsafe {
        match saved {
            Some(v) => std::env::set_var("TZ", v),
            None => std::env::remove_var("TZ"),
        }
        tzset();
    }
    r
}

/// Whether the tz database has `zone` (a TZ test is meaningless without it).
fn zone_available(zone: &str) -> bool {
    ["/usr/share/zoneinfo", "/var/db/timezone/zoneinfo"]
        .iter()
        .any(|dir| std::path::Path::new(dir).join(zone).exists())
}

struct TestLocale(libc::locale_t);

impl Drop for TestLocale {
    fn drop(&mut self) {
        // SAFETY: created by newlocale below, never null.
        unsafe { libc::freelocale(self.0) };
    }
}

/// A locale by name, if it is installed.
fn locale(name: &str) -> Option<TestLocale> {
    let name = CString::new(name).unwrap();
    // SAFETY: plain allocation.
    let loc = unsafe {
        libc::newlocale(
            crate::jq::platform::LC_ALL_MASK,
            name.as_ptr(),
            ptr::null_mut(),
        )
    };
    // Lazily: a `TestLocale(null)` built and dropped here would call
    // `freelocale(NULL)`, which glibc doesn't allow (it segfaults), and most
    // Linux systems lack the locales these tests ask for.
    (!loc.is_null()).then(|| TestLocale(loc))
}

fn c_locale() -> TestLocale {
    locale("C").expect("the C locale always exists")
}

fn arr(xs: &[f64]) -> Vec<Option<f64>> {
    xs.iter().copied().map(Some).collect()
}

fn msg(s: &str) -> Error {
    Error::Msg(s.to_owned())
}

/// Compare broken-down times, treating NaN seconds as equal.
fn assert_bt(got: BrokenDownTime, want: [f64; 8]) {
    let same = got
        .iter()
        .zip(want.iter())
        .all(|(g, w)| g == w || (g.is_nan() && w.is_nan()));
    assert!(same, "got {got:?}, want {want:?}");
}

fn strftime_c(input: TimeInput, fmt: &str) -> Result<String, Error> {
    strftime_in(input, Some(fmt), c_locale().0)
}

fn strflocaltime_c(input: TimeInput, fmt: &str) -> Result<String, Error> {
    strflocaltime_in(input, Some(fmt), c_locale().0)
}

fn strptime_c(input: &str, fmt: &str) -> Result<Parsed, Error> {
    strptime_in(Some(input), Some(fmt), c_locale().0)
}

fn parsed(tm: [f64; 8]) -> Parsed {
    Parsed { tm, rest: None }
}

#[test]
fn gmtime_basics() {
    let _s = serial();
    let g = |x| gmtime(TimeInput::Number(x));
    assert_bt(
        g(1425599507.0).unwrap(),
        [2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0],
    );
    // Fractional seconds are kept.
    assert_bt(
        g(1425599507.5).unwrap(),
        [2015.0, 2.0, 5.0, 23.0, 51.0, 47.5, 4.0, 63.0],
    );
    assert_bt(
        g(-1.5).unwrap(),
        [1969.0, 11.0, 31.0, 23.0, 59.0, 59.5, 3.0, 364.0],
    );
    assert_bt(
        g(-0.25).unwrap(),
        [1970.0, 0.0, 1.0, 0.0, 0.0, 0.75, 4.0, 0.0],
    );
    let convert = msg("error converting number of seconds since epoch to datetime");
    assert_eq!(g(1e30), Err(convert.clone()));
    assert_eq!(g(-1e30), Err(convert.clone()));
    assert_eq!(g(f64::INFINITY), Err(convert.clone()));
    assert_eq!(g(1e18), Err(convert));
    assert_eq!(
        gmtime(TimeInput::Other),
        Err(msg("gmtime() requires numeric inputs"))
    );
    assert_eq!(
        gmtime(TimeInput::Array(&arr(&[1.0]))),
        Err(msg("gmtime() requires numeric inputs"))
    );
}

/// `nan | gmtime` is `[1970,0,1,0,0,null,4,0]`: `(time_t)NaN` is 0 on aarch64.
#[cfg(target_arch = "aarch64")]
#[test]
fn gmtime_nan() {
    let _s = serial();
    assert_bt(
        gmtime(TimeInput::Number(f64::NAN)).unwrap(),
        [1970.0, 0.0, 1.0, 0.0, 0.0, f64::NAN, 4.0, 0.0],
    );
}

#[cfg(target_os = "macos")]
#[test]
fn gmtime_extremes_macos() {
    let _s = serial();
    let g = |x| gmtime(TimeInput::Number(x));
    assert_bt(
        g(67767976233316800.0).unwrap(),
        [2147483647.0, 11.0, 29.0, 12.0, 0.0, 0.0, 0.0, 362.0],
    );
    assert!(g(67768036191676799.0).is_err());
    assert_bt(
        g(-67768040609740800.0).unwrap(),
        [-2147481748.0, 0.0, 1.0, 0.0, 0.0, 0.0, 4.0, 0.0],
    );
    assert!(g(-67768100567971200.0).is_err());
}

#[test]
fn mktime_basics() {
    let _s = serial();
    let m = |xs: &[f64]| mktime(TimeInput::Array(&arr(xs)));
    assert_eq!(
        m(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0]),
        Ok(1425599507.0)
    );
    assert_eq!(m(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0]), Ok(1425599507.0));
    assert_eq!(m(&[2015.0, 2.0, 5.0]), Ok(1425513600.0));
    // Missing fields are 0: day 0 of January is December 31st.
    assert_eq!(m(&[2015.0]), Ok(1419984000.0));
    // Fields are truncated, and out-of-range fields normalized.
    assert_eq!(m(&[2015.9, 2.9, 5.9, 23.9, 51.9, 47.9]), Ok(1425599507.0));
    assert_eq!(m(&[2015.0, 14.0, 5.0, 23.0, 51.0, 47.0]), Ok(1457221907.0));
    assert_eq!(m(&[2015.0, -1.0, 5.0, 23.0, 51.0, 47.0]), Ok(1417823507.0));
    // Values are clamped to the int range first.
    assert_eq!(m(&[2015.0, 2.0, 5.0, 23.0, 51.0, 1e10]), Ok(3573083107.0));
    assert_eq!(m(&[2015.0, 2.0, 5.0, 23.0, 51.0, -1e10]), Ok(-721884188.0));
    assert_eq!(m(&[1970.0, 0.0, 1.0, 0.0, 0.0, 0.0]), Ok(0.0));
    // timegm's -1 and jq's -2 sentinel are real times, reported as errors.
    assert_eq!(
        m(&[1969.0, 11.0, 31.0, 23.0, 59.0, 59.0]),
        Err(msg("invalid gmtime representation"))
    );
    assert_eq!(
        m(&[1969.0, 11.0, 31.0, 23.0, 59.0, 58.0]),
        Err(msg("mktime not supported on this platform"))
    );
    // Only the first 8 elements are read.
    let mut ninth = arr(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 1.0, 2.0]);
    ninth.push(None);
    assert_eq!(mktime(TimeInput::Array(&ninth)), Ok(1425599507.0));
}

#[test]
fn mktime_errors() {
    let _s = serial();
    let parsed = msg("mktime requires parsed datetime inputs");
    let mut bad = arr(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0]);
    bad.push(None);
    assert_eq!(mktime(TimeInput::Array(&bad)), Err(parsed.clone()));
    assert_eq!(mktime(TimeInput::Array(&[None])), Err(parsed.clone()));
    assert_eq!(
        mktime(TimeInput::Array(&arr(&[
            2015.0,
            2.0,
            5.0,
            23.0,
            51.0,
            f64::NAN
        ]))),
        Err(parsed)
    );
    for input in [TimeInput::Number(1.0), TimeInput::Other] {
        assert_eq!(mktime(input), Err(msg("mktime requires array inputs")));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mktime_macos_range() {
    let _s = serial();
    let m = |xs: &[f64]| mktime(TimeInput::Array(&arr(xs)));
    let invalid = Err(msg("invalid gmtime representation"));
    // macOS timegm rejects years before 1900.
    assert_eq!(m(&[]), invalid);
    assert_eq!(m(&[1899.0, 11.0, 31.0]), invalid);
    assert_eq!(m(&[1800.0, 0.0, 1.0]), invalid);
    assert_eq!(m(&[1900.0, 0.0, 1.0]), Ok(-2208988800.0));
    assert_eq!(
        m(&[1901.0, 11.0, 13.0, 20.0, 45.0, 52.0]),
        Ok(-2147483648.0)
    );
    assert_eq!(m(&[10000.0, 0.0, 1.0]), Ok(253402300800.0));
    assert_eq!(m(&[1e30, 2.0, 5.0]), Ok(67768036165584000.0));
    assert_eq!(m(&[f64::INFINITY]), Ok(67768036160054400.0));
    assert_eq!(m(&[-1e30, 2.0, 5.0]), invalid);
}

#[test]
fn strftime_formats_c_locale() {
    let _s = serial();
    let t = TimeInput::Number(1425599507.0);
    assert_eq!(
        strftime_c(t, "%Y-%m-%dT%H:%M:%SZ").unwrap(),
        "2015-03-05T23:51:47Z"
    );
    let extended = strftime_c(
        TimeInput::Number(1425599507.9),
        "%A, %B %d, %Y %j %U %W %u %w %e %C %y %G %g %V %k %l %I %p %M %S",
    );
    if cfg!(target_env = "musl") {
        // musl has no %k or %l, so its strftime fails, for jq built on it too.
        assert_eq!(extended, Err(msg("strftime/1: unknown system failure")));
    } else {
        assert_eq!(
            extended.unwrap(),
            "Thursday, March 05, 2015 064 09 09 4 4  5 20 15 2015 15 10 23 11 11 PM 51 47"
        );
    }
    assert_eq!(
        strftime_c(
            t,
            "%c | %x | %X | %D | %F | %T | %R | %r | %h | %n | %t | %%"
        )
        .unwrap(),
        "Thu Mar  5 23:51:47 2015 | 03/05/15 | 23:51:47 | 03/05/15 | 2015-03-05 | 23:51:47 | 23:51 | 11:51:47 PM | Mar | \n | \t | %"
    );
    // The format is a C string.
    assert_eq!(
        strftime_c(TimeInput::Number(0.0), "abc\0def").unwrap(),
        "abc"
    );
    assert_eq!(strftime_c(TimeInput::Number(0.0), "").unwrap(), "");
}

#[test]
fn strftime_normalizes_with_timegm() {
    let _s = serial();
    // The given day of week and day of year are ignored.
    assert_eq!(
        strftime_c(
            TimeInput::Array(&arr(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 0.0, 0.0])),
            "%A %j"
        )
        .unwrap(),
        "Thursday 064"
    );
    assert_eq!(
        strftime_c(
            TimeInput::Array(&arr(&[2015.0, 14.0, 40.0, 25.0, 61.0, 61.0])),
            "%c"
        )
        .unwrap(),
        "Sun Apr 10 02:02:01 2016"
    );
}

#[test]
fn strftime_errors() {
    let _s = serial();
    let fmt_err = msg("strftime/1 requires a string format");
    let parsed = msg("strftime/1 requires parsed datetime inputs");
    assert_eq!(strftime(TimeInput::Other, Some("%c")), Err(parsed.clone()));
    assert_eq!(strftime(TimeInput::Other, None), Err(parsed.clone()));
    assert_eq!(strftime(TimeInput::Number(1.0), None), Err(fmt_err.clone()));
    assert_eq!(strftime(TimeInput::Array(&[None]), None), Err(fmt_err));
    assert_eq!(
        strftime(TimeInput::Array(&[None]), Some("%c")),
        Err(parsed.clone())
    );
    // A number goes through gmtime first: its error wins, and NaN seconds fail jv2tm.
    let convert = msg("error converting number of seconds since epoch to datetime");
    assert_eq!(
        strftime(TimeInput::Number(1e30), None),
        Err(convert.clone())
    );
    assert_eq!(strftime(TimeInput::Number(1e30), Some("%c")), Err(convert));
    #[cfg(target_arch = "aarch64")]
    assert_eq!(
        strftime(TimeInput::Number(f64::NAN), Some("%c")),
        Err(parsed)
    );
    // Output longer than strlen(format) + 100 bytes.
    assert_eq!(
        strftime_c(TimeInput::Number(0.0), "%c%c%c%c%c"),
        Err(msg("strftime/1: unknown system failure"))
    );
    assert_eq!(
        strftime_c(TimeInput::Number(0.0), "%c%c%c%c").unwrap(),
        "Thu Jan  1 00:00:00 1970".repeat(4)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn strftime_macos_specifics() {
    let _s = serial();
    // TZ is switched to UTC around strftime, whatever the local zone is.
    for tz in ["Asia/Tokyo", "America/New_York", "UTC"] {
        with_tz(tz, || {
            assert_eq!(
                strftime_c(TimeInput::Number(1425599507.0), "%Z %z %s %+ %H").unwrap(),
                "UTC +0000 1425599507 Thu Mar  5 23:51:47 UTC 2015 23"
            );
        });
    }
    // Apple's handling of unknown conversions.
    assert_eq!(
        strftime_c(
            TimeInput::Number(0.0),
            "%é %q %Q %E %O %Ey %Od %5Y %-d %_d %0d %^a %#a"
        )
        .unwrap(),
        "é q Q   70 01 5Y 1  1 01 ^a #a"
    );
    // Before 1900 timegm fails and leaves the fields as given; Apple's %s gives -1.
    assert_eq!(
        strftime_c(TimeInput::Array(&arr(&[])), "%A %j %c").unwrap(),
        "Sunday 001 Sun Jan  0 00:00:00 1900"
    );
    assert_eq!(
        strftime_c(TimeInput::Array(&arr(&[1800.0, 0.0, 1.0])), "%s %c %j %A").unwrap(),
        "-1 Sun Jan  1 00:00:00 1800 001 Sunday"
    );
}

/// libc's own `strftime_l` of the epoch (UTC) in `loc`: what jq's `strftime`
/// prints, since jq calls libc's `strftime` in the locale `setlocale` chose.
#[cfg(target_os = "macos")]
fn libc_strftime_epoch(fmt: &str, loc: libc::locale_t) -> String {
    let t: libc::time_t = 0;
    // SAFETY: gmtime_r and strftime_l write only into the buffers given, and
    // `loc` is a valid locale object.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&t, &mut tm);
        let fmt = CString::new(fmt).unwrap();
        let mut buf = [0u8; 512];
        let n = libc::strftime_l(buf.as_mut_ptr().cast(), buf.len(), fmt.as_ptr(), &tm, loc);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// `strftime` formats in the locale it's given. The exact text is the
/// locale's, which changes between macOS releases (macOS 15's German `%c` is
/// "Do  1 Jan", macOS 26's "Do.  1 Jan."), so it's checked against libc's own
/// `strftime_l`, plus the parts every release agrees on.
#[cfg(target_os = "macos")]
#[test]
fn strftime_uses_the_locale() {
    let _s = serial();
    let zero = TimeInput::Number(0.0);
    let fmt = "%A %B %c %x %X %p %r";
    for (name, fmt, start) in [
        ("de_DE.UTF-8", fmt, "Donnerstag Januar "),
        ("ja_JP.UTF-8", fmt, "木曜日 1月 "),
        // en_US has a 4-digit %x year; the C locale doesn't.
        (
            "en_US.UTF-8",
            "%c | %x | %X | %r | %p",
            "Thu Jan  1 00:00:00 1970 | 01/01/1970 | ",
        ),
    ] {
        if let Some(loc) = locale(name) {
            let got = strftime_in(zero, Some(fmt), loc.0).unwrap();
            assert_eq!(got, libc_strftime_epoch(fmt, loc.0), "{name}");
            assert!(got.starts_with(start), "{name}: {got}");
        }
    }
    assert_eq!(
        strftime_c(zero, "%c | %x | %X | %r | %p").unwrap(),
        "Thu Jan  1 00:00:00 1970 | 01/01/70 | 00:00:00 | 12:00:00 AM | AM"
    );
    // Latin-1 output is not UTF-8: jq replaces the bad byte.
    if let Some(fr) = locale("fr_FR.ISO8859-1") {
        assert_eq!(
            strftime_in(TimeInput::Number(86400.0 * 31.0 * 7.0), Some("%A %B"), fr.0).unwrap(),
            "jeudi ao\u{FFFD}t"
        );
    }
}

#[test]
fn strflocaltime_in_zones() {
    let _s = serial();
    if !zone_available("Asia/Tokyo") || !zone_available("America/New_York") {
        eprintln!("skipping: tz database not installed");
        return;
    }
    with_tz("Asia/Tokyo", || {
        assert_eq!(
            strflocaltime_c(
                TimeInput::Array(&arr(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0])),
                "%c %Z %z %s"
            )
            .unwrap(),
            "Thu Mar  5 23:51:47 2015 JST +0900 1425567107"
        );
        assert_eq!(
            strflocaltime_c(TimeInput::Number(1425599507.0), "%c %Z %z %s").unwrap(),
            "Fri Mar  6 08:51:47 2015 JST +0900 1425599507"
        );
        assert_bt(
            localtime(TimeInput::Number(1425599507.25)).unwrap(),
            [2015.0, 2.0, 6.0, 8.0, 51.0, 47.25, 5.0, 64.0],
        );
        // mktime is always UTC.
        let local = localtime(TimeInput::Number(1425599507.0))
            .unwrap()
            .map(Some);
        assert_eq!(mktime(TimeInput::Array(&local)), Ok(1425631907.0));
    });
    with_tz("America/New_York", || {
        let f = |xs: &[f64]| strflocaltime_c(TimeInput::Array(&arr(xs)), "%c %Z %z %s").unwrap();
        assert_eq!(
            f(&[2015.0, 6.0, 5.0, 23.0, 51.0, 47.0]),
            "Sun Jul  5 23:51:47 2015 EDT -0400 1436154707"
        );
        assert_eq!(
            f(&[2015.0, 0.0, 5.0, 23.0, 51.0, 47.0]),
            "Mon Jan  5 23:51:47 2015 EST -0500 1420519907"
        );
        // 02:30 on the spring-forward day doesn't exist; how mktime resolves it is up
        // to the libc (macOS moves it to 03:30 EDT).
        #[cfg(target_os = "macos")]
        assert_eq!(
            f(&[2015.0, 2.0, 8.0, 2.0, 30.0, 0.0]),
            "Sun Mar  8 03:30:00 2015 EDT -0400 1425799800"
        );
        assert_bt(
            localtime(TimeInput::Number(1436140307.75)).unwrap(),
            [2015.0, 6.0, 5.0, 19.0, 51.0, 47.75, 0.0, 185.0],
        );
        assert_bt(
            localtime(TimeInput::Number(-1.5)).unwrap(),
            [1969.0, 11.0, 31.0, 18.0, 59.0, 59.5, 3.0, 364.0],
        );
    });
}

/// On macOS, strftime's TZ switch must not leak into later local-time calls.
#[cfg(target_os = "macos")]
#[test]
fn tz_is_restored_after_strftime() {
    let _s = serial();
    with_tz("Asia/Tokyo", || {
        assert_eq!(strftime_c(TimeInput::Number(0.0), "%H").unwrap(), "00");
        assert_eq!(localtime(TimeInput::Number(0.0)).unwrap()[3], 9.0);
        assert_eq!(
            strflocaltime_c(TimeInput::Number(0.0), "%H %Z").unwrap(),
            "09 JST"
        );
        assert_eq!(
            strftime_c(TimeInput::Number(0.0), "%H %Z").unwrap(),
            "00 UTC"
        );
        assert_eq!(localtime(TimeInput::Number(0.0)).unwrap()[3], 9.0);
        assert_eq!(std::env::var("TZ").as_deref(), Ok("Asia/Tokyo"));
    });
}

/// Runs `f` in a process of its own: this test binary again, running only the test
/// `name` of this module. For what lives in libc for the whole process (`tzname`),
/// which tests elsewhere in the crate change as they run alongside, and which
/// `serial()` doesn't keep them from.
#[cfg(target_os = "macos")]
fn in_own_process(name: &str, f: impl FnOnce()) {
    const CHILD: &str = "QJ_TIME_TEST_CHILD";
    if std::env::var_os(CHILD).is_some_and(|v| v == name) {
        f();
        return;
    }
    // libtest names a test by its path without the crate's name.
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", &format!("{module}::{name}"), "--test-threads=1"])
        .env(CHILD, name)
        .output()
        .expect("run the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{name} failed in its own process:\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// For a date mktime can't represent, `%Z` comes from libc's `tzname`, which the last
/// local-time conversion set (jq 1.8.1 prints "JST", "LMT", "JST" for this sequence).
#[cfg(target_os = "macos")]
#[test]
fn pre_1900_zone_name_follows_earlier_conversions() {
    in_own_process("pre_1900_zone_name_follows_earlier_conversions", || {
        let _s = serial();
        if !zone_available("Asia/Tokyo") {
            return;
        }
        with_tz("Asia/Tokyo", || {
            let zone = || strflocaltime_c(TimeInput::Array(&[]), "%Z").unwrap();
            localtime(TimeInput::Number(0.0)).unwrap();
            assert_eq!(zone(), "JST");
            localtime(TimeInput::Number(-3000000000.0)).unwrap();
            assert_eq!(zone(), "LMT");
            localtime(TimeInput::Number(0.0)).unwrap();
            assert_eq!(zone(), "JST");
        });
    });
}

#[cfg(target_os = "macos")]
#[test]
fn unknown_tz_is_utc_on_macos() {
    let _s = serial();
    with_tz("Invalid/Zone", || {
        assert_bt(
            localtime(TimeInput::Number(1425599507.0)).unwrap(),
            [2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0],
        );
        assert_eq!(
            strflocaltime_c(TimeInput::Number(1425599507.0), "%c %Z %z").unwrap(),
            "Thu Mar  5 23:51:47 2015 UTC +0000"
        );
    });
}

#[test]
fn strflocaltime_errors() {
    let _s = serial();
    let fmt_err = msg("strflocaltime/1 requires a string format");
    let parsed = msg("strflocaltime/1 requires parsed datetime inputs");
    assert_eq!(
        strflocaltime(TimeInput::Other, Some("%c")),
        Err(parsed.clone())
    );
    assert_eq!(strflocaltime(TimeInput::Other, None), Err(parsed.clone()));
    assert_eq!(
        strflocaltime(TimeInput::Number(1.0), None),
        Err(fmt_err.clone())
    );
    assert_eq!(
        strflocaltime(TimeInput::Array(&[None]), Some("%c")),
        Err(parsed.clone())
    );
    // localtime's failure isn't checked: without a string format jq reports the format...
    assert_eq!(strflocaltime(TimeInput::Number(1e30), None), Err(fmt_err));
    // ...and with one it crashes (jv_array_get's assert).
    assert!(matches!(
        strflocaltime(TimeInput::Number(1e30), Some("%c")),
        Err(Error::Abort(m)) if m.contains("JVP_HAS_KIND(j, JV_KIND_ARRAY)")
    ));
    assert!(matches!(
        strflocaltime(TimeInput::Number(f64::INFINITY), Some("%c")),
        Err(Error::Abort(_))
    ));
    #[cfg(target_arch = "aarch64")]
    assert_eq!(
        strflocaltime(TimeInput::Number(f64::NAN), Some("%c")),
        Err(parsed)
    );
    assert_eq!(
        strflocaltime_c(TimeInput::Number(0.0), "%c%c%c%c%c"),
        Err(msg("strflocaltime/1: unknown system failure"))
    );
    assert_eq!(strflocaltime_c(TimeInput::Number(0.0), "").unwrap(), "");
}

#[cfg(target_os = "macos")]
#[test]
fn strflocaltime_abort_text_macos() {
    let _s = serial();
    assert_eq!(
        strflocaltime(TimeInput::Number(1e30), Some("%c")),
        Err(Error::Abort(
            "Assertion failed: (JVP_HAS_KIND(j, JV_KIND_ARRAY)), function jv_array_get, file jv.c, line 1006."
                .into()
        ))
    );
}

#[test]
fn localtime_errors() {
    let _s = serial();
    assert_eq!(
        localtime(TimeInput::Other),
        Err(msg("localtime() requires numeric inputs"))
    );
    assert_eq!(
        localtime(TimeInput::Number(1e30)),
        Err(msg(
            "error converting number of seconds since epoch to datetime"
        ))
    );
}

#[test]
fn strptime_basics() {
    let _s = serial();
    let iso = "%Y-%m-%dT%H:%M:%SZ";
    assert_eq!(
        strptime_c("2015-03-05T23:51:47Z", iso),
        Ok(parsed([2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0]))
    );
    assert_eq!(
        strptime_c("2015-03-05T23:51:47Z", "%FT%TZ"),
        Ok(parsed([2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0]))
    );
    assert_eq!(
        strptime_c("Mar 5 2015", "%b %d %Y"),
        Ok(parsed([2015.0, 2.0, 5.0, 0.0, 0.0, 0.0, 4.0, 63.0]))
    );
    assert_eq!(
        strptime_c("March 5 2015 11:51:47 PM", "%B %d %Y %I:%M:%S %p"),
        Ok(parsed([2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0]))
    );
    assert_eq!(
        strptime_c("12/31/99", "%D"),
        Ok(parsed([1999.0, 11.0, 31.0, 0.0, 0.0, 0.0, 5.0, 364.0]))
    );
    // Stopping at whitespace is accepted; the rest becomes a 9th element.
    assert_eq!(
        strptime_c("2015-03-05T23:51:47Z \n x", iso),
        Ok(Parsed {
            tm: [2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0],
            rest: Some(" \n x".into()),
        })
    );
    assert_eq!(
        strptime_c("2015\t\n", "%Y").map(|p| p.rest),
        Ok(Some("\t\n".into()))
    );
}

#[test]
fn strptime_errors() {
    let _s = serial();
    let iso = "%Y-%m-%dT%H:%M:%SZ";
    assert_eq!(
        strptime_c("2015-03-05T23:51:47Zx", iso),
        Err(msg(
            "date \"2015-03-05T23:51:47Zx\" does not match format \"%Y-%m-%dT%H:%M:%SZ\""
        ))
    );
    assert_eq!(
        strptime_c("2015-13-01", "%Y-%m-%d"),
        Err(msg(
            "date \"2015-13-01\" does not match format \"%Y-%m-%d\""
        ))
    );
    // C sees the strings up to the first NUL, in the error message too.
    assert_eq!(
        strptime_c("abc\0", "abcd"),
        Err(msg("date \"abc\" does not match format \"abcd\""))
    );
    let types = msg("strptime/1 requires string inputs and arguments");
    assert_eq!(strptime(Some("x"), None), Err(types.clone()));
    assert_eq!(strptime(None, Some("%c")), Err(types.clone()));
    assert_eq!(strptime(None, None), Err(types));
}

/// macOS strptime leaves fields unset that glibc computes, and jq then always uses its
/// own day-of-week / day-of-year code on macOS.
#[cfg(target_os = "macos")]
#[test]
fn strptime_macos_specifics() {
    let _s = serial();
    assert_eq!(
        strptime_c("12", "%H"),
        Ok(parsed([1900.0, 0.0, 0.0, 12.0, 0.0, 0.0, 6.0, -1.0]))
    );
    // Gauss's formula is wrong for January 1900 (it was a Monday).
    assert_eq!(
        strptime_c("1900-01-01", "%Y-%m-%d"),
        Ok(parsed([1900.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]))
    );
    assert_eq!(
        strptime_c("2015 100", "%Y %j"),
        Ok(parsed([2015.0, 3.0, 10.0, 0.0, 0.0, 0.0, 5.0, 99.0]))
    );
    assert_eq!(
        strptime_c("abc\0def", "abc"),
        Ok(parsed([1900.0, 0.0, 0.0, 0.0, 0.0, 0.0, 6.0, -1.0]))
    );
    // Apple's %Y skips leading whitespace by not consuming anything.
    assert_eq!(
        strptime_c(" 2015", "%Y"),
        Ok(Parsed {
            tm: [1900.0, 0.0, 0.0, 0.0, 0.0, 0.0, 6.0, -1.0],
            rest: Some(" 2015".into()),
        })
    );
    // Trailing ASCII whitespace is accepted, but not U+00A0 (isspace(0xC2) is false).
    assert_eq!(
        strptime_c("2015 ", "%Y"),
        Ok(Parsed {
            tm: [2015.0, 0.0, 0.0, 0.0, 0.0, 0.0, 3.0, -1.0],
            rest: Some(" ".into()),
        })
    );
    assert_eq!(
        strptime_c("2015\u{c}\r", "%Y").map(|p| p.rest),
        Ok(Some("\u{c}\r".into()))
    );
    assert_eq!(
        strptime_c("2015\u{a0}", "%Y"),
        Err(msg("date \"2015\u{a0}\" does not match format \"%Y\""))
    );
    // jq aborts when Apple's tm_yday disagrees with its own.
    for (input, fmt) in [("100", "%j"), ("2015 3 5 100", "%Y %m %d %j")] {
        assert_eq!(strptime_c(input, fmt), Err(abort_set_tm_yday()), "{input}");
    }
    // %s is converted to local time by Apple's strptime.
    with_tz("UTC", || {
        assert_eq!(
            strptime_c("1425599507", "%s"),
            Ok(parsed([2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0]))
        );
    });
    if zone_available("America/New_York") {
        with_tz("America/New_York", || {
            assert_eq!(
                strptime_c("1425599507", "%s"),
                Ok(parsed([2015.0, 2.0, 5.0, 18.0, 51.0, 47.0, 4.0, 63.0]))
            );
            assert_eq!(
                strptime_c("2015-03-05 +0900", "%Y-%m-%d %z"),
                Ok(parsed([2015.0, 2.0, 4.0, 10.0, 0.0, 0.0, 3.0, 62.0]))
            );
        });
    }
}

#[cfg(target_os = "macos")]
#[test]
fn strptime_uses_the_locale() {
    let _s = serial();
    let Some(de) = locale("de_DE.UTF-8") else {
        return;
    };
    assert_eq!(
        strptime_in(Some("Donnerstag 1970"), Some("%A %Y"), de.0),
        Ok(parsed([1970.0, 0.0, 0.0, 0.0, 0.0, 0.0, 3.0, -1.0]))
    );
    assert_eq!(
        strptime_in(Some("Thursday 1970"), Some("%A %Y"), de.0),
        Err(msg(
            "date \"Thursday 1970\" does not match format \"%A %Y\""
        ))
    );
    assert_eq!(
        strptime_c("Thursday 1970", "%A %Y"),
        Ok(parsed([1970.0, 0.0, 0.0, 0.0, 0.0, 0.0, 3.0, -1.0]))
    );
}

#[test]
fn set_tm_wday_and_yday() {
    let tm = |y: c_int, m: c_int, d: c_int| {
        let mut t = zeroed_tm();
        t.tm_year = y - 1900;
        t.tm_mon = m;
        t.tm_mday = d;
        t.tm_wday = 8;
        t.tm_yday = 367;
        t
    };
    let wday_yday = |y, m, d| {
        let mut t = tm(y, m, d);
        set_tm_wday(&mut t);
        set_tm_yday(&mut t).unwrap();
        (t.tm_wday, t.tm_yday)
    };
    assert_eq!(wday_yday(2015, 2, 5), (4, 63));
    assert_eq!(wday_yday(2000, 0, 1), (6, 0));
    assert_eq!(wday_yday(2000, 2, 1), (3, 60));
    assert_eq!(wday_yday(2100, 0, 1), (4, 0)); // really a Friday
    assert_eq!(wday_yday(1900, 0, 1), (0, 0)); // really a Monday
    assert_eq!(wday_yday(1900, 1, 28), (2, 58));
    assert_eq!(wday_yday(1900, 2, 1), (4, 59));
    assert_eq!(wday_yday(-100, 0, 1), (0, 0));
    // The assertion only accepts the sentinel or the same value.
    let mut t = tm(2015, 0, 0);
    t.tm_yday = 99;
    assert_eq!(set_tm_yday(&mut t), Err(abort_set_tm_yday()));
}

/// The date cases of jq 1.8.1's `jq.test` and `man.test`, evaluated through the
/// primitives the way `builtin.jq` composes them (`fromdate` is
/// `strptime("%Y-%m-%dT%H:%M:%SZ") | mktime`, `todate` is `strftime` of that format).
#[test]
fn upstream_date_cases() {
    let _s = serial();
    let iso = "%Y-%m-%dT%H:%M:%SZ";
    let fields = |p: &Parsed| p.tm.map(Some);
    let broken = |xs: &[f64]| arr(xs);

    assert_eq!(
        strftime_c(
            TimeInput::Array(&broken(&[2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0])),
            iso
        )
        .as_deref(),
        Ok("2015-03-05T23:51:47Z")
    );
    assert_eq!(
        strftime_c(TimeInput::Number(1435677542.822351), "%A, %B %d, %Y").as_deref(),
        Ok("Tuesday, June 30, 2015")
    );
    assert_eq!(
        strftime_c(TimeInput::Array(&broken(&[2024.0, 2.0, 15.0])), iso).as_deref(),
        Ok("2024-03-15T00:00:00Z")
    );
    assert_eq!(
        mktime(TimeInput::Array(&broken(&[2024.0, 8.0, 21.0]))),
        Ok(1726876800.0)
    );
    assert_bt(
        gmtime(TimeInput::Number(1425599507.0)).unwrap(),
        [2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0],
    );

    // ["a",1,2,3,4,5,6,7]
    let bad: Vec<Option<f64>> = std::iter::once(None)
        .chain((1..8).map(|i| Some(i as f64)))
        .collect();
    assert_eq!(
        strftime(TimeInput::Array(&bad), Some(iso)),
        Err(msg("strftime/1 requires parsed datetime inputs"))
    );
    assert_eq!(
        strflocaltime(TimeInput::Array(&bad), Some(iso)),
        Err(msg("strflocaltime/1 requires parsed datetime inputs"))
    );
    assert_eq!(
        mktime(TimeInput::Array(&bad)),
        Err(msg("mktime requires parsed datetime inputs"))
    );
    // oss-fuzz #67403: `0 | strftime([])`, `0 | strflocaltime({})`.
    assert_eq!(
        strftime(TimeInput::Number(0.0), None),
        Err(msg("strftime/1 requires a string format"))
    );
    assert_eq!(
        strflocaltime(TimeInput::Number(0.0), None),
        Err(msg("strflocaltime/1 requires a string format"))
    );

    let p = strptime_c("2015-03-05T23:51:47Z", iso).unwrap();
    assert_eq!(p, parsed([2015.0, 2.0, 5.0, 23.0, 51.0, 47.0, 4.0, 63.0]));
    assert_eq!(mktime(TimeInput::Array(&fields(&p))), Ok(1425599507.0));
    let p = strptime_c("2025-06-07T08:09:10", "%FT%T").unwrap();
    assert_eq!(p, parsed([2025.0, 5.0, 7.0, 8.0, 9.0, 10.0, 6.0, 157.0]));
    assert_eq!(mktime(TimeInput::Array(&fields(&p))), Ok(1749283750.0));

    // "Check day-of-week and day of year computations (should trip an assert if this
    // fails)": every day from 1970-03-01 for 67 years through strftime and strptime.
    let start = strptime_c("1970-03-01T01:02:03Z", iso).unwrap();
    let start = mktime(TimeInput::Array(&fields(&start))).unwrap();
    let mut last = None;
    for day in 0..365 * 67 {
        let t = start + 86400.0 * day as f64;
        let s = strftime_c(TimeInput::Number(t), iso).unwrap();
        last = Some(strptime_c(&s, iso).unwrap_or_else(|e| panic!("{s}: {e}")));
    }
    assert_eq!(
        last,
        Some(parsed([2037.0, 1.0, 11.0, 1.0, 2.0, 3.0, 3.0, 41.0]))
    );

    // CVE-2025-49014 regression: `0 | strflocaltime("")`.
    assert_eq!(
        strflocaltime_c(TimeInput::Number(0.0), "").as_deref(),
        Ok("")
    );
}

/// `strftime`/`strptime` use the locale from the environment, as jq's
/// `setlocale(LC_ALL, "")` does on macOS; jq's Linux release binary keeps its dates
/// in the C locale (see `date_locale`). The locale is read once per process, so this
/// re-runs the test binary with `LC_ALL` set and checks the result in the child.
#[test]
fn locale_comes_from_the_environment() {
    if locale("de_DE.UTF-8").is_none() {
        eprintln!("skipping: de_DE.UTF-8 not installed");
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "jq::platform::time::tests::env_locale_child",
            "--ignored",
            "--nocapture",
        ])
        .env("LC_ALL", "de_DE.UTF-8")
        .env("QJ_TEST_ENV_LOCALE_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "run by locale_comes_from_the_environment"]
fn env_locale_child() {
    if std::env::var_os("QJ_TEST_ENV_LOCALE_CHILD").is_none() {
        return;
    }
    // On the BSDs qj sets the process's locale at start-up, as jq's main
    // does (`crate::os::init`); this child is a test binary, so it does that.
    #[cfg(any(target_os = "netbsd", target_os = "freebsd"))]
    crate::os::init();
    let (thursday, january) = if cfg!(target_os = "linux") {
        ("Thursday", "January")
    } else {
        ("Donnerstag", "Januar")
    };
    assert_eq!(
        strftime(TimeInput::Number(0.0), Some("%A %B")),
        Ok(format!("{thursday} {january}"))
    );
    assert_eq!(
        strptime(Some(&format!("{thursday} 1970")), Some("%A %Y")).map(|p| p.tm[0]),
        Ok(1970.0)
    );
    if cfg!(target_os = "linux") {
        assert!(strptime(Some("Donnerstag 1970"), Some("%A %Y")).is_err());
    }
}

#[test]
fn now_is_the_current_time() {
    let expected = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let got = now();
    assert!(
        (got - expected).abs() < 5.0,
        "now() = {got}, expected about {expected}"
    );
    assert!(
        got.fract() != 0.0 || now().fract() != 0.0,
        "now() has sub-second precision"
    );
}
