//! Port of jq's `jvp_dtoa_fmt` (David Gay's `g_fmt`, from `jv_dtoa.c`).
//!
//! The shortest round-trip digits come from `ryu` (they are the same digits
//! `dtoa` mode 0 produces); only jq's layout rules are ported:
//!
//! * `decpt <= -4 || decpt > ndigits + 15` -> `d.ddde±XX` (at least two
//!   exponent digits): `1e-05`, `1.5e+17`, `1e+300`;
//! * `decpt <= 0` -> `0.000ddd`: `0.0001`;
//! * otherwise plain digits, padded with zeros: `15000000000000000`.
//!
//! where the value is `0.d1d2... * 10^decpt`.

/// Shortest round-trip decimal digits of a finite double: returns the number
/// of digits written to `digits` and `decpt`. Zero gives `"0"`, `decpt = 1`.
fn shortest_digits(x: f64, digits: &mut [u8; 24]) -> (usize, i32) {
    let mut buf = ryu::Buffer::new();
    let s = buf.format_finite(x).as_bytes();
    let mut n = 0usize;
    let mut int_len: Option<i32> = None;
    let mut exp: i32 = 0;
    let mut i = 0;
    if s.first() == Some(&b'-') {
        i = 1;
    }
    while i < s.len() {
        let c = s[i];
        match c {
            b'0'..=b'9' => {
                digits[n] = c;
                n += 1;
            }
            b'.' => int_len = Some(n as i32),
            b'e' | b'E' => {
                let rest = std::str::from_utf8(&s[i + 1..]).expect("ascii");
                exp = rest.parse().expect("ryu exponent");
                break;
            }
            _ => unreachable!("unexpected ryu output"),
        }
        i += 1;
    }
    let mut decpt = int_len.unwrap_or(n as i32) + exp;
    // Strip leading zeros (each one moves the decimal point).
    let lead = digits[..n].iter().take_while(|&&c| c == b'0').count();
    if lead == n {
        digits[0] = b'0';
        return (1, 1);
    }
    digits.copy_within(lead..n, 0);
    n -= lead;
    decpt -= lead as i32;
    // Strip trailing zeros.
    while n > 1 && digits[n - 1] == b'0' {
        n -= 1;
    }
    (n, decpt)
}

/// Appends `jvp_dtoa_fmt(x)` to `out`. `x` must be finite (jq clamps
/// infinities to `±DBL_MAX` and prints NaN as `null` before calling this).
pub fn write_dtoa_fmt(x: f64, out: &mut Vec<u8>) {
    debug_assert!(x.is_finite());
    // Fast path: integers below 1e16 print as themselves (their shortest
    // digits are exact and never reach the exponential form).
    if x.fract() == 0.0 && x.abs() < 1e16 {
        if x == 0.0 && x.is_sign_negative() {
            out.extend_from_slice(b"-0");
        } else {
            out.extend_from_slice(itoa::Buffer::new().format(x as i64).as_bytes());
        }
        return;
    }
    let mut digits = [0u8; 24];
    let (n, decpt) = shortest_digits(x, &mut digits);
    let digits = &digits[..n];
    if x.is_sign_negative() {
        out.push(b'-');
    }
    let nd = n as i32;
    if decpt <= -4 || decpt > nd + 15 {
        out.push(digits[0]);
        if n > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        let e = decpt - 1;
        if e < 0 {
            out.push(b'-');
        } else {
            out.push(b'+');
        }
        let e = e.unsigned_abs();
        if e < 10 {
            out.push(b'0');
        }
        out.extend_from_slice(itoa::Buffer::new().format(e).as_bytes());
    } else if decpt <= 0 {
        out.extend_from_slice(b"0.");
        for _ in decpt..0 {
            out.push(b'0');
        }
        out.extend_from_slice(digits);
    } else {
        let dp = decpt as usize;
        if dp >= n {
            out.extend_from_slice(digits);
            for _ in n..dp {
                out.push(b'0');
            }
        } else {
            out.extend_from_slice(&digits[..dp]);
            out.push(b'.');
            out.extend_from_slice(&digits[dp..]);
        }
    }
}

/// `jvp_dtoa_fmt(x)` as a `String`.
pub fn dtoa_fmt(x: f64) -> String {
    let mut v = Vec::with_capacity(24);
    write_dtoa_fmt(x, &mut v);
    String::from_utf8(v).expect("ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rules() {
        // Expectations from jq 1.8.1 (`jq -n '[...] | map(. * 1)'`).
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1.5e17, "1.5e+17"),
            (15000000000000000.0, "15000000000000000"),
            (1e16, "1e+16"),
            (1e300, "1e+300"),
            (0.30000000000000004, "0.30000000000000004"),
            (f64::MAX, "1.7976931348623157e+308"),
            (-f64::MAX, "-1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (123.456, "123.456"),
            (1e15, "1000000000000000"),
            (1.2345e-4, "0.00012345"),
            (1.2345e-5, "1.2345e-05"),
            (9007199254740993.0, "9007199254740992"),
            (1e22, "1e+22"),
            (1.7976931348623157e300, "1.7976931348623156e+300"),
        ];
        for &(x, want) in cases {
            assert_eq!(dtoa_fmt(x), want, "{x:e}");
        }
    }
}
