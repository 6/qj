//! `f_negate` and the `BINOPS` (`_plus` … `_greatereq`), plus the `binop_*` functions the
//! compiler's constant folding calls (parser.y `constant_fold`).
//!
//! Port of builtin.c. Owned by Track B1 (docs/JQ_PORT_PLAN.md).
//!
//! Arithmetic on two numbers always yields a native double (`jv_number(a + b)`), even for
//! literal operands; only `null + x`/`x + null` hand an operand back untouched, which is
//! why `1.000 + null` still prints `1.000`. Containers are consumed and updated in place
//! when uniquely owned (`jv_string_concat`, `jv_array_concat`, `jv_object_merge`).

use super::{CResult, Host};
use crate::jq::value::{Array, Error, Value};
use std::cmp::Ordering;

/// `BINOP(name)`: `f_name(jq, input, a, b)` frees the input and returns `binop_name(a, b)`.
///
/// The input must be dropped *before* the binop runs: in `. + [$x]` the input and `a`
/// are the same array, and only once the input's reference is gone is `a` uniquely
/// owned, so that `reduce range(n) as $x ([]; . + [$x])` appends in place (linear)
/// instead of copying the array on every step (quadratic).
macro_rules! binop_cfunctions {
    ($($(#[$doc:meta])* $f:ident => $binop:ident;)*) => {$(
        $(#[$doc])*
        pub fn $f(_host: &mut dyn Host, input: Value, args: &mut [Value]) -> CResult {
            drop(input);
            let a = std::mem::take(&mut args[0]);
            let b = std::mem::take(&mut args[1]);
            $binop(a, b)
        }
    )*};
}

binop_cfunctions! {
    /// `_plus` (nargs 3): port of builtin.c `f_plus` (`BINOP(plus)`).
    f_plus => binop_plus;
    /// `_minus` (nargs 3): port of builtin.c `f_minus` (`BINOP(minus)`).
    f_minus => binop_minus;
    /// `_multiply` (nargs 3): port of builtin.c `f_multiply` (`BINOP(multiply)`).
    f_multiply => binop_multiply;
    /// `_divide` (nargs 3): port of builtin.c `f_divide` (`BINOP(divide)`).
    f_divide => binop_divide;
    /// `_mod` (nargs 3): port of builtin.c `f_mod` (`BINOP(mod)`).
    f_mod => binop_mod;
    /// `_equal` (nargs 3): port of builtin.c `f_equal` (`BINOP(equal)`).
    f_equal => binop_equal;
    /// `_notequal` (nargs 3): port of builtin.c `f_notequal` (`BINOP(notequal)`).
    f_notequal => binop_notequal;
    /// `_less` (nargs 3): port of builtin.c `f_less` (`BINOP(less)`).
    f_less => binop_less;
    /// `_lesseq` (nargs 3): port of builtin.c `f_lesseq` (`BINOP(lesseq)`).
    f_lesseq => binop_lesseq;
    /// `_greater` (nargs 3): port of builtin.c `f_greater` (`BINOP(greater)`).
    f_greater => binop_greater;
    /// `_greatereq` (nargs 3): port of builtin.c `f_greatereq` (`BINOP(greatereq)`).
    f_greatereq => binop_greatereq;
}

/// `_negate` (nargs 1): port of builtin.c `f_negate` (unary `-`). A literal stays a
/// literal (`jv_number_negate`), so `-1.50` prints `-1.50`.
pub fn f_negate(_host: &mut dyn Host, input: Value, _args: &mut [Value]) -> CResult {
    match input {
        Value::Number(n) => Ok(Value::Number(n.negate())),
        _ => Err(Error::type_error(&input, "cannot be negated")),
    }
}

/// Port of builtin.c `binop_plus` (`+`), also used by constant folding.
pub fn binop_plus(a: Value, b: Value) -> CResult {
    match (a, b) {
        (Value::Null, b) => Ok(b),
        (a, Value::Null) => Ok(a),
        (Value::Number(x), Value::Number(y)) => Ok(Value::number(x.value() + y.value())),
        (Value::String(mut x), Value::String(y)) => {
            // jv_string_concat
            x.concat(&y);
            Ok(Value::String(x))
        }
        (Value::Array(mut x), Value::Array(y)) => {
            // jv_array_concat: append b's elements one by one (moved out when b is
            // uniquely owned).
            x.extend(y);
            Ok(Value::Array(x))
        }
        (Value::Object(mut x), Value::Object(y)) => {
            // jv_object_merge
            x.merge(&y);
            Ok(Value::Object(x))
        }
        (a, b) => Err(Error::type_error2(&a, &b, "cannot be added")),
    }
}

/// Port of builtin.c `binop_minus` (`-`), also used by constant folding. Array
/// subtraction keeps the elements of `a` not `jv_equal` to any element of `b`.
pub fn binop_minus(a: Value, b: Value) -> CResult {
    match (&a, &b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::number(x.value() - y.value())),
        (Value::Array(x), Value::Array(y)) => {
            let mut out = Array::new();
            for elem in x.iter() {
                if !y.iter().any(|e| elem.equal(e)) {
                    out.push(elem.clone());
                }
            }
            Ok(Value::Array(out))
        }
        _ => Err(Error::type_error2(&a, &b, "cannot be subtracted")),
    }
}

/// Port of builtin.c `binop_multiply` (`*`), also used by constant folding.
///
/// `string * number` (either order) repeats the string: the count is truncated to an
/// `int`, a negative or NaN count gives `null`, and a zero count gives `""`.
pub fn binop_multiply(a: Value, b: Value) -> CResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::number(x.value() * y.value())),
        (Value::String(s), Value::Number(n)) | (Value::Number(n), Value::String(s)) => {
            let d = n.value();
            let count = if d < 0.0 || d.is_nan() {
                -1
            } else if d > i32::MAX as f64 {
                i32::MAX
            } else {
                d as i32
            };
            s.repeat(count)
        }
        (Value::Object(mut x), Value::Object(y)) => {
            // jv_object_merge_recursive
            x.merge_recursive(&y);
            Ok(Value::Object(x))
        }
        (a, b) => Err(Error::type_error2(&a, &b, "cannot be multiplied")),
    }
}

/// Port of builtin.c `binop_divide` (`/`), also used by constant folding. Strings are
/// split (`jv_string_split`); a zero divisor is an error, a NaN divisor is not.
pub fn binop_divide(a: Value, b: Value) -> CResult {
    match (&a, &b) {
        (Value::Number(x), Value::Number(y)) => {
            if y.value() == 0.0 {
                return Err(Error::type_error2(
                    &a,
                    &b,
                    "cannot be divided because the divisor is zero",
                ));
            }
            Ok(Value::number(x.value() / y.value()))
        }
        (Value::String(x), Value::String(y)) => Ok(Value::Array(x.split(y))),
        _ => Err(Error::type_error2(&a, &b, "cannot be divided")),
    }
}

/// builtin.c's `dtoi`: a double to `intmax_t`, saturating (a NaN never gets here). The
/// in-range cast truncates toward zero; `2^63` itself saturates as on arm64.
fn dtoi(n: f64) -> i64 {
    if n < i64::MIN as f64 {
        i64::MIN
    } else if -n < i64::MIN as f64 {
        i64::MAX
    } else {
        n as i64
    }
}

/// Port of builtin.c `binop_mod` (`%`), also used by constant folding. Both operands are
/// truncated to integers; NaN on either side gives NaN, and a divisor that truncates to
/// zero is an error.
pub fn binop_mod(a: Value, b: Value) -> CResult {
    match (&a, &b) {
        (Value::Number(x), Value::Number(y)) => {
            let na = x.value();
            let nb = y.value();
            if na.is_nan() || nb.is_nan() {
                return Ok(Value::number(f64::NAN));
            }
            let bi = dtoi(nb);
            if bi == 0 {
                return Err(Error::type_error2(
                    &a,
                    &b,
                    "cannot be divided (remainder) because the divisor is zero",
                ));
            }
            // Check if the divisor is -1 to avoid overflow when the dividend is INTMAX_MIN.
            let r = if bi == -1 { 0 } else { dtoi(na) % bi };
            Ok(Value::number(r as f64))
        }
        _ => Err(Error::type_error2(&a, &b, "cannot be divided (remainder)")),
    }
}

/// Port of builtin.c `binop_equal` (`==`, `jv_equal`), also used by constant folding.
pub fn binop_equal(a: Value, b: Value) -> CResult {
    Ok(Value::Bool(a.equal(&b)))
}

/// Port of builtin.c `binop_notequal` (`!=`), also used by constant folding.
pub fn binop_notequal(a: Value, b: Value) -> CResult {
    Ok(Value::Bool(!a.equal(&b)))
}

/// builtin.c's `order_cmp`: `jv_cmp(a, b)` against the operator.
fn order_cmp(a: &Value, b: &Value, accept: fn(Ordering) -> bool) -> CResult {
    Ok(Value::Bool(accept(a.compare(b))))
}

/// Port of builtin.c `binop_less` (`<`), also used by constant folding.
pub fn binop_less(a: Value, b: Value) -> CResult {
    order_cmp(&a, &b, Ordering::is_lt)
}

/// Port of builtin.c `binop_lesseq` (`<=`), also used by constant folding.
pub fn binop_lesseq(a: Value, b: Value) -> CResult {
    order_cmp(&a, &b, Ordering::is_le)
}

/// Port of builtin.c `binop_greater` (`>`), also used by constant folding.
pub fn binop_greater(a: Value, b: Value) -> CResult {
    order_cmp(&a, &b, Ordering::is_gt)
}

/// Port of builtin.c `binop_greatereq` (`>=`), also used by constant folding.
pub fn binop_greatereq(a: Value, b: Value) -> CResult {
    order_cmp(&a, &b, Ordering::is_ge)
}

#[cfg(test)]
mod tests {
    //! Expectations from the jq 1.8.1 binary, with the operands as JSON input so that
    //! they are literals exactly as here:
    //! `jq -c 'try _plus(.[0]; .[1]) catch ["ERR", .]' <<< '[a, b]'`.
    use super::*;
    use crate::jq::builtins::testing::TestHost;
    use crate::jq::value::parse::parse;

    fn v(json: &str) -> Value {
        parse(json).unwrap_or_else(|e| panic!("bad JSON {json}: {e}"))
    }

    fn show(r: CResult) -> String {
        match r {
            Ok(v) => v.to_json(),
            Err(e) => format!("ERR {}", e.value().to_json()),
        }
    }

    fn op(f: fn(Value, Value) -> CResult, a: &str, b: &str) -> String {
        show(f(v(a), v(b)))
    }

    #[test]
    fn plus() {
        assert_eq!(op(binop_plus, "1", "2"), "3");
        assert_eq!(op(binop_plus, "null", "null"), "null");
        assert_eq!(op(binop_plus, "1.000", "null"), "1.000");
        assert_eq!(op(binop_plus, "null", "1.000"), "1.000");
        // Arithmetic gives a native double: `100000000000000000001 + 0`.
        assert_eq!(op(binop_plus, "100000000000000000001", "0"), "1e+20");
        assert_eq!(op(binop_plus, "1.000", "0"), "1");
        assert_eq!(op(binop_plus, "\"a\"", "\"b\""), "\"ab\"");
        assert_eq!(op(binop_plus, "[1]", "[2]"), "[1,2]");
        assert_eq!(
            op(binop_plus, "{\"a\":1,\"b\":2}", "{\"b\":3,\"c\":4}"),
            "{\"a\":1,\"b\":3,\"c\":4}"
        );
        assert_eq!(
            op(binop_plus, "1", "\"a\""),
            r#"ERR "number (1) and string (\"a\") cannot be added""#
        );
        assert_eq!(
            op(binop_plus, "{}", "[]"),
            r#"ERR "object ({}) and array ([]) cannot be added""#
        );
        assert_eq!(
            op(binop_plus, "true", "false"),
            r#"ERR "boolean (true) and boolean (false) cannot be added""#
        );
        // type_error2 truncates each value to 11 bytes + "...".
        assert_eq!(
            op(binop_plus, "\"abcdefghijklmnop\"", "{\"key\":[1,2,3]}"),
            r#"ERR "string (\"abcdefghij...) and object ({\"key\":[1,2...) cannot be added""#
        );
    }

    #[test]
    fn minus() {
        assert_eq!(op(binop_minus, "5", "3"), "2");
        assert_eq!(op(binop_minus, "[1,2,1,3]", "[1]"), "[2,3]");
        assert_eq!(
            op(binop_minus, "[1,[2],{\"a\":1}]", "[[2],{\"a\":1}]"),
            "[1]"
        );
        assert_eq!(op(binop_minus, "[1.0,1]", "[1]"), "[]");
        assert_eq!(
            op(binop_minus, "1", "\"a\""),
            r#"ERR "number (1) and string (\"a\") cannot be subtracted""#
        );
        assert_eq!(
            op(binop_minus, "null", "1"),
            r#"ERR "null (null) and number (1) cannot be subtracted""#
        );
        assert_eq!(
            op(binop_minus, "\"abc\"", "\"b\""),
            r#"ERR "string (\"abc\") and string (\"b\") cannot be subtracted""#
        );
    }

    #[test]
    fn multiply() {
        assert_eq!(op(binop_multiply, "2", "3.5"), "7");
        assert_eq!(op(binop_multiply, "\"abc\"", "0"), "\"\"");
        assert_eq!(op(binop_multiply, "\"abc\"", "0.5"), "\"\"");
        assert_eq!(op(binop_multiply, "\"abc\"", "1.5"), "\"abc\"");
        assert_eq!(op(binop_multiply, "\"abc\"", "-1"), "null");
        assert_eq!(op(binop_multiply, "\"abc\"", "nan"), "null");
        assert_eq!(op(binop_multiply, "2", "\"abc\""), "\"abcabc\"");
        assert_eq!(op(binop_multiply, "\"\"", "1e300"), "\"\"");
        assert_eq!(
            op(binop_multiply, "\"ab\"", "1e300"),
            r#"ERR "Repeat string result too long""#
        );
        assert_eq!(
            op(
                binop_multiply,
                "{\"a\":{\"b\":1},\"c\":1}",
                "{\"a\":{\"c\":2},\"c\":{\"d\":3}}"
            ),
            "{\"a\":{\"b\":1,\"c\":2},\"c\":{\"d\":3}}"
        );
        assert_eq!(
            op(binop_multiply, "[1]", "2"),
            r#"ERR "array ([1]) and number (2) cannot be multiplied""#
        );
        assert_eq!(
            op(binop_multiply, "\"a\"", "\"b\""),
            r#"ERR "string (\"a\") and string (\"b\") cannot be multiplied""#
        );
    }

    #[test]
    fn divide() {
        assert_eq!(op(binop_divide, "1", "4"), "0.25");
        assert_eq!(
            op(binop_divide, "1", "0"),
            r#"ERR "number (1) and number (0) cannot be divided because the divisor is zero""#
        );
        assert_eq!(
            op(binop_divide, "0", "-0.0"),
            r#"ERR "number (0) and number (-0.0) cannot be divided because the divisor is zero""#
        );
        assert_eq!(op(binop_divide, "1", "nan"), "null");
        assert_eq!(op(binop_divide, "\"a,b\"", "\",\""), "[\"a\",\"b\"]");
        assert_eq!(op(binop_divide, "\"\"", "\",\""), "[]");
        assert_eq!(op(binop_divide, "\"ab\"", "\"\""), "[\"a\",\"b\"]");
        assert_eq!(
            op(binop_divide, "[]", "1"),
            r#"ERR "array ([]) and number (1) cannot be divided""#
        );
    }

    #[test]
    fn modulo() {
        assert_eq!(op(binop_mod, "-5", "3"), "-2");
        assert_eq!(op(binop_mod, "5", "-3"), "2");
        assert_eq!(op(binop_mod, "5.9", "3.9"), "2");
        assert_eq!(op(binop_mod, "nan", "3"), "null");
        assert_eq!(op(binop_mod, "3", "nan"), "null");
        assert_eq!(op(binop_mod, "5", "1e1000"), "5");
        assert_eq!(op(binop_mod, "-1e1000", "3"), "-2");
        assert_eq!(op(binop_mod, "1e19", "7"), "0");
        assert_eq!(op(binop_mod, "-9223372036854775808", "-1"), "0");
        assert_eq!(
            op(binop_mod, "5", "0"),
            r#"ERR "number (5) and number (0) cannot be divided (remainder) because the divisor is zero""#
        );
        assert_eq!(
            op(binop_mod, "5", "0.5"),
            r#"ERR "number (5) and number (0.5) cannot be divided (remainder) because the divisor is zero""#
        );
        assert_eq!(
            op(binop_mod, "\"a\"", "1"),
            r#"ERR "string (\"a\") and number (1) cannot be divided (remainder)""#
        );
    }

    #[test]
    fn comparisons() {
        assert_eq!(op(binop_equal, "1", "1.0"), "true");
        assert_eq!(op(binop_equal, "nan", "nan"), "false");
        assert_eq!(
            op(binop_equal, "{\"a\":1,\"b\":2}", "{\"b\":2,\"a\":1}"),
            "true"
        );
        assert_eq!(op(binop_notequal, "1", "\"1\""), "true");
        assert_eq!(op(binop_less, "nan", "nan"), "true");
        assert_eq!(op(binop_less, "null", "false"), "true");
        assert_eq!(op(binop_less, "[1,2]", "[1,2,0]"), "true");
        assert_eq!(op(binop_lesseq, "{\"a\":1}", "{\"a\":1}"), "true");
        assert_eq!(op(binop_greater, "\"b\"", "\"ab\""), "true");
        assert_eq!(op(binop_greatereq, "nan", "nan"), "false");
        assert_eq!(
            op(
                binop_greater,
                "100000000000000000001",
                "100000000000000000000"
            ),
            "true"
        );
    }

    #[test]
    fn cfunctions() {
        let mut host = TestHost::default();
        let mut args = [v("[1]"), v("[2]")];
        assert_eq!(show(f_plus(&mut host, v("null"), &mut args)), "[1,2]");
        // Arguments are moved out, as jq passes them by value.
        assert!(args[0].is_null() && args[1].is_null());
        assert_eq!(show(f_negate(&mut host, v("1.50"), &mut [])), "-1.50");
        assert_eq!(show(f_negate(&mut host, v("-0"), &mut [])), "0");
        assert_eq!(
            show(f_negate(&mut host, v("null"), &mut [])),
            r#"ERR "null (null) cannot be negated""#
        );
        assert_eq!(
            show(f_negate(&mut host, v("\"abc\""), &mut [])),
            r#"ERR "string (\"abc\") cannot be negated""#
        );
        let mut args = [v("3"), v("2")];
        assert_eq!(show(f_mod(&mut host, v("null"), &mut args)), "1");
    }

    /// `. + [$x]` inside `reduce`: the VM passes the input and `a` as two references to
    /// the same value. The input is dropped first (jq's `jv_free(input)`), so `a` is
    /// unique and grows in place. Each container gets spare capacity so that growing
    /// in place keeps its buffer, while a copy would allocate a new one.
    #[test]
    fn plus_updates_the_left_operand_in_place() {
        let mut host = TestHost::default();

        let mut arr = Array::with_capacity(16);
        for i in 0..3 {
            arr.push(Value::from(i));
        }
        let a = Value::Array(arr);
        let before = a.as_array().unwrap().as_slice().as_ptr();
        let mut args = [a.clone(), v("[3]")];
        let r = f_plus(&mut host, a, &mut args).unwrap();
        assert_eq!(r.to_json(), "[0,1,2,3]");
        assert_eq!(r.as_array().unwrap().as_slice().as_ptr(), before);

        let mut s = crate::jq::value::Str::with_capacity(64);
        s.push_str("ab");
        let a = Value::String(s);
        let before = a.as_str().unwrap().as_ptr();
        let mut args = [a.clone(), v("\"cd\"")];
        let r = f_plus(&mut host, a, &mut args).unwrap();
        assert_eq!(r.to_json(), "\"abcd\"");
        assert_eq!(r.as_str().unwrap().as_ptr(), before);

        let a = v("{\"a\":1}");
        let before: *const Value = a.as_object().unwrap().get_index(0).unwrap().1;
        let mut args = [a.clone(), v("{\"a\":2}")];
        let r = f_plus(&mut host, a, &mut args).unwrap();
        assert_eq!(r.to_json(), "{\"a\":2}");
        let after: *const Value = r.as_object().unwrap().get_index(0).unwrap().1;
        assert_eq!(after, before);

        // A shared operand is copied, not modified: `[1] as $x | $x + [2], $x`.
        let x = v("[1]");
        let mut args = [x.clone(), v("[2]")];
        let r = f_plus(&mut host, Value::Null, &mut args).unwrap();
        assert_eq!(r.to_json(), "[1,2]");
        assert_eq!(x.to_json(), "[1]");
    }
}
