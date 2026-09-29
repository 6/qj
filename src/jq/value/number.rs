//! jq numbers: port of the number parts of `jv.c` (built with `USE_DECNUM`).
//!
//! A jq number is either a plain double ("native", produced by arithmetic) or a
//! *literal*: a decimal number parsed from JSON text, a program constant, or
//! `tonumber`. Literals keep their decimal value exactly (jq uses decNumber) and
//! print in decNumber's canonical to-scientific-string form (`1e2` -> `1E+2`,
//! `1.50` -> `1.50`, `0.0000001` -> `1E-7`). Two literals compare exactly as
//! decimals; any other comparison uses doubles.
//!
//! Only the decNumber behaviours jq relies on are implemented: parsing
//! (`decNumberFromString` in jq's context), to-string, compare, negate/abs
//! (`decNumberMinus`/`decNumberAbs`) and conversion to double
//! (`jvp_literal_number_to_double`).

use std::cell::Cell;
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use super::dtoa;

/// decNumber context used by jq (`DEC_INIT_BASE` with extended digits):
/// `emax = 999999999`, `emin = -999999999`,
/// `digits = min(DEC_MAX_DIGITS, INT32_MAX - (DECDPUN-1) - (emax - emin - 1))`.
const CTX_EMAX: i64 = 999_999_999;
const CTX_EMIN: i64 = -999_999_999;
const CTX_DIGITS: i64 = 147_483_648;
/// Smallest exponent of a subnormal in jq's context: `emin - (digits - 1)`.
const CTX_ETINY: i64 = CTX_EMIN - (CTX_DIGITS - 1);
/// `DECNUMMAXE * 2`: exponent used by decNumberFromString when the exponent
/// has too many digits to represent.
const EXP_TOO_BIG: i64 = 1_999_999_998;

/// A jq number: a double, optionally carrying an exact decimal literal.
///
/// Cheap to clone (a literal is reference counted, like jq's
/// `jvp_literal_number`).
#[derive(Clone)]
pub struct Number(Repr);

#[derive(Clone)]
enum Repr {
    Native(f64),
    Literal(Rc<Literal>),
}

/// Decimal coefficient: significant digits with leading zeros removed (a zero
/// is the single digit 0). Trailing zeros are significant for printing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Coeff {
    /// Up to 19 digits.
    Small(u64),
    /// ASCII digits, more than 19 of them, no leading zero.
    Big(Box<[u8]>),
}

/// A decimal literal (port of `jvp_literal_number` + its `decNumber`).
pub(crate) struct Literal {
    neg: bool,
    /// decNumber infinity (from `Infinity`/`inf` or exponent overflow).
    inf: bool,
    coeff: Coeff,
    /// Exponent: value is `coeff * 10^exp`.
    exp: i64,
    /// Cached `jvp_literal_number_to_double` (NaN = not computed yet; a
    /// literal is never NaN).
    double: Cell<f64>,
}

/// Parsed decimal (the result of `decNumberFromString`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Decimal {
    neg: bool,
    inf: bool,
    coeff: Coeff,
    exp: i64,
}

enum ParsedNumber {
    Decimal(Decimal),
    NaN,
}

impl Coeff {
    fn from_digits(d: &[u8]) -> Coeff {
        debug_assert!(!d.is_empty());
        if d.len() <= 19 {
            let mut v: u64 = 0;
            for &c in d {
                v = v * 10 + (c - b'0') as u64;
            }
            Coeff::Small(v)
        } else {
            Coeff::Big(d.into())
        }
    }

    fn is_zero(&self) -> bool {
        matches!(self, Coeff::Small(0))
    }

    /// Writes the digits into `buf` and returns them.
    fn digits<'a>(&'a self, buf: &'a mut itoa::Buffer) -> &'a [u8] {
        match self {
            Coeff::Small(v) => buf.format(*v).as_bytes(),
            Coeff::Big(b) => b,
        }
    }
}

/// Port of decNumber's `decNumberFromString` for jq's context. Returns `None`
/// on a syntax error (`DEC_Conversion_syntax`).
fn parse_decimal(s: &[u8]) -> Option<ParsedNumber> {
    let mut i = 0;
    let mut neg = false;
    if let Some(&c) = s.first() {
        if c == b'-' {
            neg = true;
            i = 1;
        } else if c == b'+' {
            i = 1;
        }
    }
    // Scan the coefficient: digits with at most one '.'.
    let coeff_start = i;
    let mut dot: Option<usize> = None;
    let mut ndig = 0usize;
    let mut last_digit: Option<usize> = None;
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_digit() {
            ndig += 1;
            last_digit = Some(i);
        } else if c == b'.' && dot.is_none() {
            dot = Some(i);
        } else {
            break;
        }
        i += 1;
    }
    if ndig == 0 {
        // No digits: only Infinity / NaN are possible, and not after a '.'.
        if dot.is_some() || i >= s.len() {
            return None;
        }
        let rest = &s[i..];
        if rest.eq_ignore_ascii_case(b"infinity") || rest.eq_ignore_ascii_case(b"inf") {
            return Some(ParsedNumber::Decimal(Decimal {
                neg,
                inf: true,
                coeff: Coeff::Small(0),
                exp: 0,
            }));
        }
        // A NaN expected (optionally signalling), with an optional payload.
        let mut r = rest;
        if matches!(r.first(), Some(b's' | b'S')) {
            r = &r[1..];
        }
        if r.len() < 3 || !r[..3].eq_ignore_ascii_case(b"nan") {
            return None;
        }
        let payload = &r[3..];
        let payload = &payload[payload.iter().take_while(|&&c| c == b'0').count()..];
        if payload.is_empty() {
            return Some(ParsedNumber::NaN);
        }
        if !payload.iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // A NaN with a payload: syntactically valid for decNumber, but
        // jvp_literal_number_new rejects it.
        return None;
    }
    let mut exponent: i64 = 0;
    if i < s.len() {
        // Had some digits; an exponent is the only valid continuation.
        let c = s[i];
        if c != b'e' && c != b'E' {
            return None;
        }
        i += 1;
        let mut nege = false;
        if i < s.len() && s[i] == b'-' {
            nege = true;
            i += 1;
        } else if i < s.len() && s[i] == b'+' {
            i += 1;
        }
        if i >= s.len() {
            return None;
        }
        // Strip insignificant leading zeros (keeping a final one).
        while s[i] == b'0' && i + 1 < s.len() {
            i += 1;
        }
        let firstexp = i;
        let mut e: i64 = 0;
        while i < s.len() && s[i].is_ascii_digit() {
            // Saturate; the length check below decides overflow like decNumber.
            e = (e * 10 + (s[i] - b'0') as i64).min(i64::MAX / 100);
            i += 1;
        }
        if i < s.len() {
            return None;
        }
        let explen = i - firstexp;
        if explen >= 10 && (explen > 10 || s[firstexp] > b'1') {
            e = EXP_TOO_BIG;
        }
        exponent = if nege { -e } else { e };
    }
    // Collect significant digits (skip leading zeros, keep a final 0).
    let last = last_digit.expect("ndig > 0");
    let mut digits: Vec<u8> = Vec::with_capacity(ndig);
    let mut leading = true;
    for (idx, &c) in s[coeff_start..=last].iter().enumerate() {
        if c == b'.' {
            continue;
        }
        if leading && c == b'0' && coeff_start + idx != last {
            continue;
        }
        leading = false;
        digits.push(c);
    }
    if let Some(d) = dot
        && d < last
    {
        exponent -= (last - d) as i64;
    }
    let mut dec = Decimal {
        neg,
        inf: false,
        coeff: Coeff::from_digits(&digits),
        exp: exponent,
    };
    finalize(&mut dec, digits);
    Some(ParsedNumber::Decimal(dec))
}

/// decNumber's `decFinalize` for a freshly parsed number in jq's context:
/// overflow to infinity, subnormal rounding and clamping of zeros.
fn finalize(dec: &mut Decimal, mut digits: Vec<u8>) {
    let nd = digits.len() as i64;
    if dec.coeff.is_zero() {
        // decSetSubnormal clamps a zero's exponent to Etiny; decSetOverflow
        // clamps it to Emax (zero does not overflow).
        dec.exp = dec.exp.clamp(CTX_ETINY, CTX_EMAX);
        return;
    }
    let tinyexp = CTX_EMIN - nd + 1;
    if dec.exp < tinyexp {
        // Subnormal: round (half-up, jq's context rounding) to exponent Etiny.
        let adjust = CTX_ETINY - dec.exp;
        if adjust <= 0 {
            return;
        }
        // Remove `adjust` digits from the right with ROUND_HALF_UP.
        if adjust > nd {
            // All digits go; the value rounds to 0 (the dropped part is < 0.1).
            dec.coeff = Coeff::Small(0);
            dec.exp = CTX_ETINY;
            return;
        }
        let keep = (nd - adjust) as usize;
        let round_up = digits[keep] >= b'5';
        digits.truncate(keep);
        if digits.is_empty() {
            digits.push(b'0');
        }
        if round_up {
            increment_digits(&mut digits);
        }
        // Strip leading zeros (a rounding up of "0" gives "1").
        let nz = digits.iter().take_while(|&&c| c == b'0').count();
        if nz == digits.len() {
            digits = vec![b'0'];
        } else {
            digits.drain(..nz);
        }
        dec.coeff = Coeff::from_digits(&digits);
        dec.exp = CTX_ETINY;
        return;
    }
    if dec.exp > CTX_EMAX - nd + 1 {
        // Overflow: ROUND_HALF_UP gives infinity.
        dec.inf = true;
        dec.coeff = Coeff::Small(0);
        dec.exp = 0;
    }
}

/// Adds one to a decimal digit string, growing it on carry.
fn increment_digits(d: &mut Vec<u8>) {
    for c in d.iter_mut().rev() {
        if *c == b'9' {
            *c = b'0';
        } else {
            *c += 1;
            return;
        }
    }
    d.insert(0, b'1');
}

impl Decimal {
    fn into_literal(self) -> Literal {
        Literal {
            neg: self.neg,
            inf: self.inf,
            coeff: self.coeff,
            exp: self.exp,
            double: Cell::new(f64::NAN),
        }
    }
}

impl Literal {
    /// Port of `decNumberToString` (to-scientific-string). Writes nothing
    /// special for infinities; callers print those as doubles.
    fn write_canonical(&self, out: &mut Vec<u8>) {
        debug_assert!(!self.inf);
        let mut buf = itoa::Buffer::new();
        let digits = self.coeff.digits(&mut buf);
        let nd = digits.len() as i64;
        if self.neg {
            out.push(b'-');
        }
        let exp = self.exp;
        if exp == 0 {
            out.extend_from_slice(digits);
            return;
        }
        let mut pre = nd + exp; // digits before '.'
        let mut e = 0i64;
        if exp > 0 || pre < -5 {
            // exponential form
            e = exp + nd - 1;
            pre = 1;
        }
        if pre > 0 {
            let pre_u = pre as usize;
            out.extend_from_slice(&digits[..pre_u.min(digits.len())]);
            if (pre_u) < digits.len() {
                out.push(b'.');
                out.extend_from_slice(&digits[pre_u..]);
            }
        } else {
            out.extend_from_slice(b"0.");
            for _ in pre..0 {
                out.push(b'0');
            }
            out.extend_from_slice(digits);
        }
        if e != 0 {
            out.push(b'E');
            if e < 0 {
                out.push(b'-');
            } else {
                out.push(b'+');
            }
            out.extend_from_slice(itoa::Buffer::new().format(e.unsigned_abs()).as_bytes());
        }
    }

    fn canonical_string(&self) -> Option<String> {
        if self.inf {
            return None;
        }
        let mut v = Vec::with_capacity(24);
        self.write_canonical(&mut v);
        Some(String::from_utf8(v).expect("ASCII"))
    }

    /// Port of `jvp_literal_number_to_double`: reduce to 17 significant digits
    /// (decimal64 context with `digits = 17`, ROUND_HALF_EVEN), then strtod.
    fn to_double(&self) -> f64 {
        let cached = self.double.get();
        if !cached.is_nan() {
            return cached;
        }
        let d = self.compute_double();
        self.double.set(d);
        d
    }

    fn compute_double(&self) -> f64 {
        let sign = if self.neg { -1.0 } else { 1.0 };
        if self.inf {
            return sign * f64::INFINITY;
        }
        // Clinger's fast path: exact coefficient and exact power of ten.
        if let Coeff::Small(c) = self.coeff
            && c < (1u64 << 53)
            && (-22..=22).contains(&self.exp)
        {
            let v = c as f64;
            let r = if self.exp >= 0 {
                v * POW10[self.exp as usize]
            } else {
                v / POW10[(-self.exp) as usize]
            };
            return sign * r;
        }
        let mut buf = itoa::Buffer::new();
        let digits = self.coeff.digits(&mut buf);
        let mut digits: Vec<u8> = digits.to_vec();
        let mut exp = self.exp;
        if digits.len() > 17 {
            // decNumberReduce in a 17-digit ROUND_HALF_EVEN context.
            let drop = digits.len() - 17;
            let first_dropped = digits[17];
            let rest_nonzero = digits[18..].iter().any(|&c| c != b'0');
            let last_kept_odd = (digits[16] - b'0') % 2 == 1;
            digits.truncate(17);
            exp += drop as i64;
            let round_up =
                first_dropped > b'5' || (first_dropped == b'5' && (rest_nonzero || last_kept_odd));
            if round_up {
                increment_digits(&mut digits);
            }
        }
        // Values out of double range need no special casing: the decimal64
        // context turns them into 0 or Infinity, and strtod does the same.
        let adjusted = exp + digits.len() as i64 - 1;
        if adjusted > 400 {
            return sign * f64::INFINITY;
        }
        if adjusted < -400 {
            return sign * 0.0;
        }
        let mut s = String::with_capacity(digits.len() + 8);
        s.push_str(std::str::from_utf8(&digits).expect("digits"));
        s.push('e');
        s.push_str(&exp.to_string());
        let v: f64 = s.parse().expect("valid float syntax");
        sign * v
    }

    fn is_zero(&self) -> bool {
        !self.inf && self.coeff.is_zero()
    }
}

static POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// Exact decimal comparison (`decNumberCompare`) of two literals.
fn literal_cmp(a: &Literal, b: &Literal) -> Ordering {
    // Infinities.
    if a.inf || b.inf {
        let rank = |l: &Literal| -> i8 { if l.inf { if l.neg { -2 } else { 2 } } else { 0 } };
        let (ra, rb) = (rank(a), rank(b));
        if ra != 0 && rb != 0 {
            return ra.cmp(&rb);
        }
        if ra != 0 {
            return if ra < 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        return if rb < 0 {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    let az = a.is_zero();
    let bz = b.is_zero();
    if az && bz {
        return Ordering::Equal;
    }
    // Sign of each (zero counts as 0).
    let sa: i8 = if az {
        0
    } else if a.neg {
        -1
    } else {
        1
    };
    let sb: i8 = if bz {
        0
    } else if b.neg {
        -1
    } else {
        1
    };
    if sa != sb {
        return sa.cmp(&sb);
    }
    // Same non-zero sign: compare magnitudes.
    let mag = magnitude_cmp(a, b);
    if sa < 0 { mag.reverse() } else { mag }
}

fn magnitude_cmp(a: &Literal, b: &Literal) -> Ordering {
    let mut ba = itoa::Buffer::new();
    let mut bb = itoa::Buffer::new();
    let da = a.coeff.digits(&mut ba);
    let db = b.coeff.digits(&mut bb);
    let adj_a = a.exp + da.len() as i64 - 1;
    let adj_b = b.exp + db.len() as i64 - 1;
    if adj_a != adj_b {
        return adj_a.cmp(&adj_b);
    }
    let n = da.len().max(db.len());
    for i in 0..n {
        let ca = da.get(i).copied().unwrap_or(b'0');
        let cb = db.get(i).copied().unwrap_or(b'0');
        if ca != cb {
            return ca.cmp(&cb);
        }
    }
    Ordering::Equal
}

impl Number {
    /// `jv_number`: a native double.
    #[inline]
    pub fn from_f64(x: f64) -> Number {
        Number(Repr::Native(x))
    }

    /// `jv_number_with_literal`: parses a decimal literal with decNumber's
    /// syntax (`1`, `-1.50`, `.5`, `5.`, `+1`, `1e5`, `Infinity`, `inf`,
    /// `NaN`, ...). Returns `None` where jq returns an invalid value. A NaN
    /// literal yields a native NaN, as in jq.
    ///
    /// The whole slice is parsed; callers mirroring C string semantics
    /// (the JSON parser, `tonumber`) must cut at the first NUL themselves
    /// (see [`Number::from_c_literal`]).
    pub fn from_literal(s: &[u8]) -> Option<Number> {
        match parse_decimal(s)? {
            ParsedNumber::NaN => Some(Number::from_f64(f64::NAN)),
            ParsedNumber::Decimal(d) => Some(Number(Repr::Literal(Rc::new(d.into_literal())))),
        }
    }

    /// Like [`Number::from_literal`], but stops at the first NUL byte like
    /// jq's C-string based callers (`tonumber`, the JSON parser).
    pub fn from_c_literal(s: &[u8]) -> Option<Number> {
        let end = memchr::memchr(0, s).unwrap_or(s.len());
        Number::from_literal(&s[..end])
    }

    /// `jv_number_value`: the number as a double (for literals, the cached
    /// result of jq's decimal-to-double conversion).
    #[inline]
    pub fn value(&self) -> f64 {
        match &self.0 {
            Repr::Native(x) => *x,
            Repr::Literal(l) => l.to_double(),
        }
    }

    /// `jv_number_has_literal`.
    #[inline]
    pub fn is_literal(&self) -> bool {
        matches!(self.0, Repr::Literal(_))
    }

    /// `jv_number_get_literal`: the canonical decimal text of a literal, or
    /// `None` for native numbers and for infinite literals (which jq prints
    /// as doubles).
    pub fn literal(&self) -> Option<String> {
        match &self.0 {
            Repr::Native(_) => None,
            Repr::Literal(l) => l.canonical_string(),
        }
    }

    /// `jvp_number_is_nan`. Literals are never NaN.
    #[inline]
    pub fn is_nan(&self) -> bool {
        match &self.0 {
            Repr::Native(x) => x.is_nan(),
            Repr::Literal(_) => false,
        }
    }

    /// `jv_is_integer`: `|modf(x).frac| < DBL_EPSILON`.
    pub fn is_integer(&self) -> bool {
        let x = self.value();
        let frac = x - x.trunc();
        // modf(inf) has a zero fractional part.
        let frac = if x.is_infinite() { 0.0 } else { frac };
        frac.abs() < f64::EPSILON
    }

    /// `jv_number_negate`: literals stay literals (`decNumberMinus`, which
    /// turns any zero into +0); natives negate the double.
    pub fn negate(&self) -> Number {
        match &self.0 {
            Repr::Native(x) => Number::from_f64(-x),
            Repr::Literal(l) => {
                let neg = if l.is_zero() { false } else { !l.neg };
                Number(Repr::Literal(Rc::new(Literal {
                    neg,
                    inf: l.inf,
                    coeff: l.coeff.clone(),
                    exp: l.exp,
                    double: Cell::new(f64::NAN),
                })))
            }
        }
    }

    /// `jv_number_abs`: literals stay literals (`decNumberAbs`).
    pub fn abs(&self) -> Number {
        match &self.0 {
            Repr::Native(x) => Number::from_f64(x.abs()),
            Repr::Literal(l) => Number(Repr::Literal(Rc::new(Literal {
                neg: false,
                inf: l.inf,
                coeff: l.coeff.clone(),
                exp: l.exp,
                double: Cell::new(f64::NAN),
            }))),
        }
    }

    /// `jvp_number_cmp`: two literals compare exactly as decimals, anything
    /// else compares as doubles (`<` gives Less, `==` Equal, otherwise
    /// Greater; so NaN compares Greater than everything here — jq's `jv_cmp`
    /// handles NaN before getting here).
    pub fn compare(&self, other: &Number) -> Ordering {
        if let (Repr::Literal(a), Repr::Literal(b)) = (&self.0, &other.0) {
            return literal_cmp(a, b);
        }
        let da = self.value();
        let db = other.value();
        if da < db {
            Ordering::Less
        } else if da == db {
            Ordering::Equal
        } else {
            Ordering::Greater
        }
    }

    /// `jvp_number_equal`.
    #[inline]
    pub fn equal(&self, other: &Number) -> bool {
        self.compare(other) == Ordering::Equal
    }

    /// Identity as in `jv_identical`: literals by allocation, natives by bit
    /// pattern.
    pub fn identical(&self, other: &Number) -> bool {
        match (&self.0, &other.0) {
            (Repr::Native(a), Repr::Native(b)) => a.to_bits() == b.to_bits(),
            (Repr::Literal(a), Repr::Literal(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Writes the number as jq prints it (`jv_dump_term`): the canonical
    /// literal if there is one, `null` for NaN, otherwise `jvp_dtoa_fmt` of
    /// the double with infinities clamped to `±DBL_MAX`.
    pub fn write_json(&self, out: &mut Vec<u8>) {
        match &self.0 {
            Repr::Literal(l) if !l.inf => l.write_canonical(out),
            _ => {
                let d = self.value();
                if d.is_nan() {
                    out.extend_from_slice(b"null");
                } else {
                    let d = d.clamp(-f64::MAX, f64::MAX);
                    dtoa::write_dtoa_fmt(d, out);
                }
            }
        }
    }

    /// The number printed as JSON, as a `String`.
    pub fn to_json_string(&self) -> String {
        let mut v = Vec::with_capacity(24);
        self.write_json(&mut v);
        String::from_utf8(v).expect("ASCII")
    }
}

impl From<f64> for Number {
    fn from(x: f64) -> Number {
        Number::from_f64(x)
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Native(x) => write!(f, "Number({x:?})"),
            Repr::Literal(_) => write!(f, "Literal({})", self.to_json_string()),
        }
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Number {
        Number::from_literal(s.as_bytes()).unwrap_or_else(|| panic!("invalid literal {s}"))
    }

    fn canon(s: &str) -> String {
        lit(s).to_json_string()
    }

    #[test]
    fn canonical_literals() {
        // Expectations from `jq -c . <<< '[...]'` (jq 1.8.1).
        let cases = [
            ("1e2", "1E+2"),
            ("1E-2", "0.01"),
            ("3.0e0", "3.0"),
            ("0e10", "0E+10"),
            ("1.5e-7", "1.5E-7"),
            ("-0", "-0"),
            ("100000000000000000000", "100000000000000000000"),
            ("1e1000", "1E+1000"),
            ("-1e1000", "-1E+1000"),
            ("0.00", "0.00"),
            ("1.0e1", "10"),
            ("007", "7"),
            ("1.50", "1.50"),
            ("100e-2", "1.00"),
            ("0e-7", "0E-7"),
            ("0e-6", "0.000000"),
            ("1e-7", "1E-7"),
            ("12.34e5", "1.234E+6"),
            (".5", "0.5"),
            ("5.", "5"),
            ("+1", "1"),
            ("-.5", "-0.5"),
            ("1e999999999", "1E+999999999"),
            ("1e-999999999", "1E-999999999"),
            ("1e-1000000000", "1E-1000000000"),
            ("1e-1147483647", "0E-1147483646"),
            ("0.1e-5", "0.000001"),
            ("0.0000001", "1E-7"),
            ("123.456e-10", "1.23456E-8"),
            ("-0e0", "-0"),
            ("0e-0", "0"),
            ("5e-1", "0.5"),
            ("-0.0e5", "-0E+4"),
            ("1e05", "1E+5"),
            ("-01", "-1"),
            ("00", "0"),
            ("12345678901234567890123", "12345678901234567890123"),
            (
                "1.2345678901234567890123e30",
                "1.2345678901234567890123E+30",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(canon(input), want, "literal {input}");
        }
    }

    #[test]
    fn overflowing_literals_print_as_doubles() {
        // `echo '[1e1000000000, 10e999999999]' | jq -c .`
        assert_eq!(canon("1e1000000000"), "1.7976931348623157e+308");
        assert_eq!(canon("10e999999999"), "1.7976931348623157e+308");
        assert_eq!(canon("infinity"), "1.7976931348623157e+308");
        assert_eq!(canon("-Inf"), "-1.7976931348623157e+308");
    }

    #[test]
    fn literal_syntax() {
        for bad in [
            "",
            "-",
            "--1",
            "1e",
            "1e+",
            "nan1",
            "infinit",
            "Infinityx",
            "True",
            "x",
            "1.2.3",
            "1ee5",
            "0x10",
            "1_000",
            ".",
            "-.",
            "e5",
            "NaN123",
            " 1",
            "1 ",
        ] {
            assert!(
                Number::from_literal(bad.as_bytes()).is_none(),
                "{bad:?} should be invalid"
            );
        }
        for nan in ["nan", "NaN", "-nan", "NaN0", "NaN00", "sNaN", "nAn"] {
            let n = Number::from_literal(nan.as_bytes()).unwrap();
            assert!(n.is_nan() && !n.is_literal(), "{nan}");
        }
        assert_eq!(
            Number::from_c_literal(b"12\x003").unwrap().to_json_string(),
            "12"
        );
    }

    #[test]
    fn literal_to_double() {
        assert_eq!(lit("1e2").value(), 100.0);
        assert_eq!(lit("0.1").value(), 0.1);
        assert_eq!(lit("-0").value().to_bits(), (-0.0f64).to_bits());
        assert_eq!(lit("1e1000").value(), f64::INFINITY);
        assert_eq!(lit("1e-1000").value(), 0.0);
        // jq rounds to 17 significant digits before strtod (double rounding):
        // `jq -n '1.0000000000000001110223024625156541 + 0'` => 1
        assert_eq!(lit("1.0000000000000001110223024625156541").value(), 1.0);
        assert_eq!(lit("100000000000000000001").value(), 1e20);
    }

    #[test]
    fn literal_compare() {
        use Ordering::*;
        let c = |a: &str, b: &str| lit(a).compare(&lit(b));
        assert_eq!(c("1", "1.0"), Equal);
        assert_eq!(c("-0", "0"), Equal);
        assert_eq!(c("0e5", "0.000"), Equal);
        assert_eq!(c("100000000000000000001", "100000000000000000000"), Greater);
        assert_eq!(c("-100000000000000000001", "-100000000000000000000"), Less);
        assert_eq!(c("1e1000", "1e999"), Greater);
        assert_eq!(c("1e1000000000", "1e999999999"), Greater); // inf vs finite
        assert_eq!(c("-1", "1"), Less);
        assert_eq!(c("0.5", "5e-1"), Equal);
        assert_eq!(c("12", "9"), Greater);
        // literal vs native compares doubles
        assert_eq!(
            lit("100000000000000000001").compare(&Number::from_f64(1e20)),
            Equal
        );
    }

    #[test]
    fn negate_and_abs() {
        assert_eq!(lit("0").negate().to_json_string(), "0");
        assert_eq!(lit("-0").negate().to_json_string(), "0");
        assert_eq!(lit("1.50").negate().to_json_string(), "-1.50");
        assert_eq!(lit("-1e2").abs().to_json_string(), "1E+2");
        assert_eq!(lit("-0").abs().to_json_string(), "0");
        assert_eq!(Number::from_f64(0.0).negate().to_json_string(), "-0");
    }
}
