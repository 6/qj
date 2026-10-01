//! Port of jq 1.8.1's `libm.h`: the math builtins jq takes straight from the C math
//! library, as a table the builtin layer can register.
//!
//! Every entry calls the platform's C function through FFI (not a Rust approximation),
//! so results are bit-identical to jq built for the same libm. `libm.h` has four
//! shapes (the `LIBM_*` macros in `builtin.c`), all registered as C functions:
//!
//! | shape | jq builtin | result |
//! |---|---|---|
//! | `LIBM_DD` | `name/0` | `f(.)` |
//! | `LIBM_DDD` | `name/2` | `f($a; $b)`; the input is ignored |
//! | `LIBM_DDDD` | `name/3` | `f($a; $b; $c)` (only `fma`) |
//! | `LIBM_DA` | `name/0` | `[f(.), out]`: `frexp` gives `[mantissa, exponent]`, `modf` `[fraction, integer part]`, `lgamma_r` `[lgamma, sign]` |
//!
//! # Mapping to jq builtins
//!
//! For each [`LibmEntry`] the builtin layer should:
//!
//! 1. If [`LibmEntry::func`] is `None` (jq was built without the function; the `_NO`
//!    macros), raise [`LibmEntry::not_found`], `Error: <name>/<arity> not found at build
//!    time`, without looking at the arguments.
//! 2. Otherwise check the numbers in order (the input for `name/0`; otherwise each
//!    argument, first to last, ignoring the input) and raise
//!    `<kind> (<dump>) number required` for the first non-number (`type_error`).
//! 3. Call [`LibmEntry::apply`] with the numbers (`jv_number_value`, so literals become
//!    doubles first) and build the number or two-element array.
//!
//! # Platforms
//!
//! On macOS and glibc all 61 functions exist, so `jq -n 'builtins'` lists all of them.
//! `builtin.c` renames four on Apple: `gamma` is `tgamma` there (on glibc it's
//! `lgamma`), `exp10` is `__exp10`, `drem` is `remainder`, and `significand` is jq's own
//! `2*frexp(x)`. `jn`/`yn` have no `_NO` variant, so on a platform without them the
//! builtins don't exist at all. For other targets this module assumes only C99 and
//! reports the rest as not found; that keeps them building but was not checked against
//! a jq build there.

use super::{Error, c_double_to_i32, c_double_to_i64};
use std::ffi::{c_int, c_long};
use std::sync::OnceLock;

/// The C functions, by `libm.h` shape.
#[derive(Clone, Copy, Debug)]
pub enum LibmFn {
    /// `LIBM_DD`: number to number.
    DD(fn(f64) -> f64),
    /// `LIBM_DDD`: two numbers to a number.
    DDD(fn(f64, f64) -> f64),
    /// `LIBM_DDDD`: three numbers to a number.
    DDDD(fn(f64, f64, f64) -> f64),
    /// `LIBM_DA`: number to `[result, out-parameter]`.
    DA(fn(f64) -> [f64; 2]),
}

/// One libm builtin.
#[derive(Clone, Copy, Debug)]
pub struct LibmEntry {
    /// The builtin's name.
    pub name: &'static str,
    /// Its jq arity (`name/arity`): 0 for `DD`/`DA`, 2 for `DDD`, 3 for `DDDD`.
    pub arity: usize,
    /// The function, or `None` if jq defines the builtin only to report it missing.
    pub func: Option<LibmFn>,
}

/// Result of [`LibmEntry::apply`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LibmOutput {
    /// A number (`DD`, `DDD`, `DDDD`).
    Number(f64),
    /// A two-element array (`DA`).
    Pair([f64; 2]),
}

impl LibmEntry {
    /// jq's error for a function missing at build time.
    pub fn not_found(&self) -> Error {
        Error::Msg(format!(
            "Error: {}/{} not found at build time",
            self.name, self.arity
        ))
    }

    /// Call the function. `input` is used by `name/0` builtins, `args` (which must have
    /// `arity` elements) by the others.
    pub fn apply(&self, input: f64, args: &[f64]) -> Result<LibmOutput, Error> {
        match self.func {
            None => Err(self.not_found()),
            Some(LibmFn::DD(f)) => Ok(LibmOutput::Number(f(input))),
            Some(LibmFn::DA(f)) => Ok(LibmOutput::Pair(f(input))),
            Some(LibmFn::DDD(f)) => Ok(LibmOutput::Number(f(args[0], args[1]))),
            Some(LibmFn::DDDD(f)) => Ok(LibmOutput::Number(f(args[0], args[1], args[2]))),
        }
    }
}

/// The libm builtins, in `libm.h` order.
pub fn table() -> &'static [LibmEntry] {
    static TABLE: OnceLock<Vec<LibmEntry>> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

/// The entry for `name/arity`, if jq defines it.
pub fn lookup(name: &str, arity: usize) -> Option<&'static LibmEntry> {
    table().iter().find(|e| e.name == name && e.arity == arity)
}

/// The C math functions. Calling them is always sound (they take plain doubles and
/// integers, and the out-pointer ones get valid pointers); they're declared as ordinary
/// unsafe externs.
mod c {
    use std::ffi::{c_int, c_long};

    // C99.
    unsafe extern "C" {
        pub fn acos(x: f64) -> f64;
        pub fn acosh(x: f64) -> f64;
        pub fn asin(x: f64) -> f64;
        pub fn asinh(x: f64) -> f64;
        pub fn atan(x: f64) -> f64;
        pub fn atan2(y: f64, x: f64) -> f64;
        pub fn atanh(x: f64) -> f64;
        pub fn cos(x: f64) -> f64;
        pub fn cosh(x: f64) -> f64;
        pub fn exp(x: f64) -> f64;
        pub fn exp2(x: f64) -> f64;
        pub fn hypot(x: f64, y: f64) -> f64;
        pub fn log(x: f64) -> f64;
        pub fn log10(x: f64) -> f64;
        pub fn log2(x: f64) -> f64;
        pub fn pow(x: f64, y: f64) -> f64;
        pub fn remainder(x: f64, y: f64) -> f64;
        pub fn sin(x: f64) -> f64;
        pub fn sinh(x: f64) -> f64;
        pub fn tan(x: f64) -> f64;
        pub fn tanh(x: f64) -> f64;
        pub fn tgamma(x: f64) -> f64;
        pub fn erf(x: f64) -> f64;
        pub fn erfc(x: f64) -> f64;
        pub fn expm1(x: f64) -> f64;
        pub fn lgamma(x: f64) -> f64;
        pub fn log1p(x: f64) -> f64;
        pub fn logb(x: f64) -> f64;
        pub fn nearbyint(x: f64) -> f64;
        pub fn nextafter(x: f64, y: f64) -> f64;
        pub fn scalbln(x: f64, n: c_long) -> f64;
        pub fn ldexp(x: f64, n: c_int) -> f64;
        pub fn modf(x: f64, iptr: *mut f64) -> f64;
        pub fn frexp(x: f64, exp: *mut c_int) -> f64;
    }

    // The C99 functions Rust's `compiler_builtins` also defines. It does
    // so weakly, but on Linux the linker meets `compiler_builtins` before
    // libm, so a plain `cbrt` would bind to Rust's port of musl/CORE-MATH
    // instead of glibc's, and those differ from glibc (and so from jq) in
    // `cbrt`'s last bit and in which zero `fmax`/`fmin` return for `0` and
    // `-0`. glibc exports each of them a second time under its `_Float64`
    // name (glibc 2.27 and later), which nothing else defines.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe extern "C" {
        #[link_name = "cbrtf64"]
        pub fn cbrt(x: f64) -> f64;
        #[link_name = "ceilf64"]
        pub fn ceil(x: f64) -> f64;
        #[link_name = "copysignf64"]
        pub fn copysign(x: f64, y: f64) -> f64;
        #[link_name = "fabsf64"]
        pub fn fabs(x: f64) -> f64;
        #[link_name = "fdimf64"]
        pub fn fdim(x: f64, y: f64) -> f64;
        #[link_name = "floorf64"]
        pub fn floor(x: f64) -> f64;
        #[link_name = "fmaf64"]
        pub fn fma(x: f64, y: f64, z: f64) -> f64;
        #[link_name = "fmaxf64"]
        pub fn fmax(x: f64, y: f64) -> f64;
        #[link_name = "fminf64"]
        pub fn fmin(x: f64, y: f64) -> f64;
        #[link_name = "fmodf64"]
        pub fn fmod(x: f64, y: f64) -> f64;
        #[link_name = "rintf64"]
        pub fn rint(x: f64) -> f64;
        #[link_name = "roundf64"]
        pub fn round(x: f64) -> f64;
        #[link_name = "sqrtf64"]
        pub fn sqrt(x: f64) -> f64;
        #[link_name = "truncf64"]
        pub fn trunc(x: f64) -> f64;
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    unsafe extern "C" {
        pub fn cbrt(x: f64) -> f64;
        pub fn ceil(x: f64) -> f64;
        pub fn copysign(x: f64, y: f64) -> f64;
        pub fn fabs(x: f64) -> f64;
        pub fn fdim(x: f64, y: f64) -> f64;
        pub fn floor(x: f64) -> f64;
        pub fn fma(x: f64, y: f64, z: f64) -> f64;
        pub fn fmax(x: f64, y: f64) -> f64;
        pub fn fmin(x: f64, y: f64) -> f64;
        pub fn fmod(x: f64, y: f64) -> f64;
        pub fn rint(x: f64) -> f64;
        pub fn round(x: f64) -> f64;
        pub fn sqrt(x: f64) -> f64;
        pub fn trunc(x: f64) -> f64;
    }

    // XSI and BSD extras: Bessel functions.
    #[cfg(all(any(target_vendor = "apple", libm_bessel), not(windows)))]
    unsafe extern "C" {
        pub fn j0(x: f64) -> f64;
        pub fn j1(x: f64) -> f64;
        pub fn y0(x: f64) -> f64;
        pub fn y1(x: f64) -> f64;
        pub fn jn(n: c_int, x: f64) -> f64;
        pub fn yn(n: c_int, x: f64) -> f64;
    }
    // Windows' C runtime has them with a leading underscore.
    #[cfg(windows)]
    unsafe extern "C" {
        #[link_name = "_j0"]
        pub fn j0(x: f64) -> f64;
        #[link_name = "_j1"]
        pub fn j1(x: f64) -> f64;
        #[link_name = "_y0"]
        pub fn y0(x: f64) -> f64;
        #[link_name = "_y1"]
        pub fn y1(x: f64) -> f64;
        #[link_name = "_jn"]
        pub fn jn(n: c_int, x: f64) -> f64;
        #[link_name = "_yn"]
        pub fn yn(n: c_int, x: f64) -> f64;
    }

    // XSI and BSD extras: the rest.
    #[cfg(any(target_vendor = "apple", libm_xsi))]
    unsafe extern "C" {
        pub fn scalb(x: f64, n: f64) -> f64;
        pub fn lgamma_r(x: f64, sign: *mut c_int) -> f64;
    }

    // builtin.c: `#define exp10 __exp10` on Apple.
    #[cfg(target_vendor = "apple")]
    unsafe extern "C" {
        #[link_name = "__exp10"]
        pub fn exp10(x: f64) -> f64;
    }

    #[cfg(libm_exp10)]
    unsafe extern "C" {
        pub fn exp10(x: f64) -> f64;
    }

    #[cfg(libm_gamma)]
    unsafe extern "C" {
        pub fn gamma(x: f64) -> f64;
    }

    #[cfg(libm_xsi)]
    unsafe extern "C" {
        pub fn drem(x: f64, y: f64) -> f64;
        pub fn significand(x: f64) -> f64;
    }

    // `double nexttoward(double, long double)`: `long double` is `double` on Apple
    // silicon, so the C signature can be declared exactly there.
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    unsafe extern "C" {
        pub fn nexttoward(x: f64, y: f64) -> f64;
    }
}

// Functions that aren't a plain `f(double...)` call.

fn frexp_da(x: f64) -> [f64; 2] {
    let mut e: c_int = 0;
    // SAFETY: valid out-pointer.
    let d = unsafe { c::frexp(x, &mut e) };
    [d, e as f64]
}

fn modf_da(x: f64) -> [f64; 2] {
    let mut i = 0.0;
    // SAFETY: valid out-pointer.
    let d = unsafe { c::modf(x, &mut i) };
    [d, i]
}

#[cfg(any(target_vendor = "apple", libm_xsi))]
fn lgamma_r_da(x: f64) -> [f64; 2] {
    let mut sign: c_int = 0;
    // SAFETY: valid out-pointer.
    let d = unsafe { c::lgamma_r(x, &mut sign) };
    [d, sign as f64]
}

/// `LIBM_DDD(jn)` passes the double `$a` to `jn`'s `int` parameter.
#[cfg(any(target_vendor = "apple", libm_bessel))]
fn jn_ddd(a: f64, b: f64) -> f64 {
    unsafe { c::jn(c_double_to_i32(a), b) }
}

#[cfg(any(target_vendor = "apple", libm_bessel))]
fn yn_ddd(a: f64, b: f64) -> f64 {
    unsafe { c::yn(c_double_to_i32(a), b) }
}

/// `ldexp(double, int)`: `$b` goes through C's implicit conversion to `int`.
fn ldexp_ddd(a: f64, b: f64) -> f64 {
    unsafe { c::ldexp(a, c_double_to_i32(b)) }
}

/// `scalbln(double, long)`.
fn scalbln_ddd(a: f64, b: f64) -> f64 {
    unsafe { c::scalbln(a, c_double_to_i64(b) as c_long) }
}

/// `nexttoward(double, long double)` with a `double` direction. Where `long double` is
/// wider than `double` this is exactly `nextafter`: the direction and the `x == y`
/// result only depend on `y`'s value, which the widening preserves.
fn nexttoward_ddd(a: f64, b: f64) -> f64 {
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    {
        unsafe { c::nexttoward(a, b) }
    }
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64")))]
    {
        unsafe { c::nextafter(a, b) }
    }
}

/// `gamma`: `tgamma` on Apple (builtin.c's `#define gamma tgamma`), and the C
/// library's `gamma` (the log of the gamma function) elsewhere.
#[cfg(target_vendor = "apple")]
fn gamma_dd(x: f64) -> f64 {
    unsafe { c::tgamma(x) }
}
#[cfg(libm_gamma)]
fn gamma_dd(x: f64) -> f64 {
    unsafe { c::gamma(x) }
}

/// `exp10`.
#[cfg(any(target_vendor = "apple", libm_exp10))]
fn exp10_dd(x: f64) -> f64 {
    unsafe { c::exp10(x) }
}

/// `drem`: `remainder` on Apple (`#define drem remainder`).
#[cfg(target_vendor = "apple")]
fn drem_ddd(a: f64, b: f64) -> f64 {
    unsafe { c::remainder(a, b) }
}
#[cfg(libm_xsi)]
fn drem_ddd(a: f64, b: f64) -> f64 {
    unsafe { c::drem(a, b) }
}

/// `significand`: on Apple, jq's own `__jq_significand`, `2*frexp(x, &z)`.
#[cfg(target_vendor = "apple")]
fn significand_dd(x: f64) -> f64 {
    2.0 * frexp_da(x)[0]
}
#[cfg(libm_xsi)]
fn significand_dd(x: f64) -> f64 {
    unsafe { c::significand(x) }
}

/// `Some(f)` where the C library has the function (`$pred`), and otherwise `None`
/// (jq's `_NO` stub, which says "not found at build time"); `f` is only compiled
/// where the function exists.
macro_rules! when {
    ($pred:meta, $e:expr) => {{
        #[cfg($pred)]
        let f = Some($e);
        #[cfg(not($pred))]
        let f = None;
        f
    }};
}

fn entry(name: &'static str, arity: usize, func: Option<LibmFn>) -> LibmEntry {
    LibmEntry { name, arity, func }
}

/// A plain C99 `double f(double)`.
macro_rules! dd {
    ($name:ident) => {
        entry(
            stringify!($name),
            0,
            Some(LibmFn::DD(|x| unsafe { c::$name(x) })),
        )
    };
}

/// A plain C99 `double f(double, double)`.
macro_rules! ddd {
    ($name:ident) => {
        entry(
            stringify!($name),
            2,
            Some(LibmFn::DDD(|a, b| unsafe { c::$name(a, b) })),
        )
    };
}

fn build_table() -> Vec<LibmEntry> {
    let mut t = vec![
        dd!(acos),
        dd!(acosh),
        dd!(asin),
        dd!(asinh),
        dd!(atan),
        ddd!(atan2),
        dd!(atanh),
        dd!(cbrt),
        dd!(cos),
        dd!(cosh),
        dd!(exp),
        dd!(exp2),
        dd!(floor),
        ddd!(hypot),
        entry(
            "j0",
            0,
            when!(
                any(target_vendor = "apple", libm_bessel),
                LibmFn::DD(|x| unsafe { c::j0(x) })
            ),
        ),
        entry(
            "j1",
            0,
            when!(
                any(target_vendor = "apple", libm_bessel),
                LibmFn::DD(|x| unsafe { c::j1(x) })
            ),
        ),
        dd!(log),
        dd!(log10),
        dd!(log2),
        ddd!(pow),
        ddd!(remainder),
        dd!(sin),
        dd!(sinh),
        dd!(sqrt),
        dd!(tan),
        dd!(tanh),
        dd!(tgamma),
        entry(
            "y0",
            0,
            when!(
                any(target_vendor = "apple", libm_bessel),
                LibmFn::DD(|x| unsafe { c::y0(x) })
            ),
        ),
        entry(
            "y1",
            0,
            when!(
                any(target_vendor = "apple", libm_bessel),
                LibmFn::DD(|x| unsafe { c::y1(x) })
            ),
        ),
    ];
    // jn and yn have no `_NO` stub: without them the builtins don't exist.
    #[cfg(any(target_vendor = "apple", libm_bessel))]
    t.extend([
        entry("jn", 2, Some(LibmFn::DDD(jn_ddd))),
        entry("yn", 2, Some(LibmFn::DDD(yn_ddd))),
    ]);
    t.extend([
        dd!(ceil),
        ddd!(copysign),
        entry(
            "drem",
            2,
            when!(
                any(target_vendor = "apple", libm_xsi),
                LibmFn::DDD(drem_ddd)
            ),
        ),
        dd!(erf),
        dd!(erfc),
        entry(
            "exp10",
            0,
            when!(
                any(target_vendor = "apple", libm_exp10),
                LibmFn::DD(exp10_dd)
            ),
        ),
        dd!(expm1),
        dd!(fabs),
        ddd!(fdim),
        entry(
            "fma",
            3,
            Some(LibmFn::DDDD(|a, b, x| unsafe { c::fma(a, b, x) })),
        ),
        ddd!(fmax),
        ddd!(fmin),
        ddd!(fmod),
        entry(
            "gamma",
            0,
            when!(
                any(target_vendor = "apple", libm_gamma),
                LibmFn::DD(gamma_dd)
            ),
        ),
        dd!(lgamma),
        dd!(log1p),
        dd!(logb),
        dd!(nearbyint),
        ddd!(nextafter),
        entry("nexttoward", 2, Some(LibmFn::DDD(nexttoward_ddd))),
        dd!(rint),
        dd!(round),
        entry(
            "scalb",
            2,
            when!(
                any(target_vendor = "apple", libm_xsi),
                LibmFn::DDD(|a, b| unsafe { c::scalb(a, b) })
            ),
        ),
        entry("scalbln", 2, Some(LibmFn::DDD(scalbln_ddd))),
        entry(
            "significand",
            0,
            when!(
                any(target_vendor = "apple", libm_xsi),
                LibmFn::DD(significand_dd)
            ),
        ),
        dd!(trunc),
        entry("ldexp", 2, Some(LibmFn::DDD(ldexp_ddd))),
        entry("modf", 0, Some(LibmFn::DA(modf_da))),
        entry("frexp", 0, Some(LibmFn::DA(frexp_da))),
        entry(
            "lgamma_r",
            0,
            when!(
                any(target_vendor = "apple", libm_xsi),
                LibmFn::DA(lgamma_r_da)
            ),
        ),
    ]);
    t
}

#[cfg(test)]
mod tests;
