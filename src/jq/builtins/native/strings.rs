//! String builtins defined in `builtin.jq`.
//!
//! ```jq
//! def join($x): reduce .[] as $i (null;
//!             (if .==null then "" else .+$x end) +
//!             ($i | if type=="boolean" or type=="number" then tostring else .//"" end)
//!         ) // "";
//! def ascii_downcase:
//!   explode | map( if 65 <= . and . <= 90 then . + 32  else . end) | implode;
//! def ascii_upcase:
//!   explode | map( if 97 <= . and . <= 122 then . - 32  else . end) | implode;
//! ```

use super::{Hold, cannot_iterate};
use crate::jq::builtins::binops::binop_plus;
use crate::jq::builtins::general::tostring;
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, Stop};
use crate::jq::value::{Error, Str, Value};

/// `join($x)`, when `$x` is pure (else the definition runs). `c`: join's `""`s in pool
/// order: `.//""`'s, `if .==null then ""`'s, and the final `// ""`'s.
///
/// The accumulator is updated in place, where jq's copies it (its subexpressions hold
/// references), so this is linear where jq is quadratic; strings have no views, so
/// only the time differs.
///
/// jq returns a joined string from `reduce ... // ""` with the `//`'s fork point still
/// on the stack, holding join's input until the caller backtracks (so a slice of a
/// uniquely owned array stays shared meanwhile): the native holds it too. (The `""`
/// of an empty input comes from the `//`'s second branch, with nothing left held.)
pub(super) fn join(vm: &mut Jq, input: Value, x: Closure, c: ConstView<'_>) -> Outcome {
    let Some(x) = vm.pure_arg(x, &input) else {
        return Outcome::Fallback(input);
    };
    match join_with(&input, x, c) {
        Ok(Some(v)) => Outcome::Yield(v, Box::new(Hold(input))),
        Ok(None) => Outcome::Value(c.get(2).clone()),
        Err(e) => e.into(),
    }
}

/// The reduce's result, if it isn't `null` (the input had elements).
fn join_with(input: &Value, x: Value, c: ConstView<'_>) -> Result<Option<Value>, Stop> {
    let mut acc = Value::Null;
    let mut step = |i: &Value| -> Result<(), Stop> {
        // `A + B`: B (the element's text) is evaluated first, then A.
        let b = match i {
            Value::Bool(_) | Value::Number(_) => tostring(i.clone()),
            Value::Null => c.get(0).clone(),
            _ => i.clone(),
        };
        let a = if acc.is_null() {
            c.get(1).clone()
        } else {
            binop_plus(std::mem::take(&mut acc), x.clone())?
        };
        acc = binop_plus(a, b)?;
        Ok(())
    };
    match input {
        Value::Array(a) => {
            for i in a.iter() {
                step(i)?;
            }
        }
        Value::Object(o) => {
            for i in o.values() {
                step(i)?;
            }
        }
        _ => return Err(cannot_iterate(input)),
    }
    Ok(acc.is_truthy().then_some(acc))
}

/// `ascii_downcase` (`lo..=hi` is `A..=Z`) or `ascii_upcase` (`a..=z`): `explode`'s
/// error for non-strings, else a new string (`implode` always makes one) with those
/// ASCII letters' case flipped. UTF-8 continuation and lead bytes are never ASCII, so
/// mapping bytes is mapping codepoints.
pub(super) fn ascii_case(input: Value, lo: u8, hi: u8) -> Result<Value, Stop> {
    let Value::String(s) = &input else {
        return Err(Error::msg("explode input must be a string").into());
    };
    let mut bytes = s.as_bytes().to_vec();
    for b in &mut bytes {
        if (lo..=hi).contains(b) {
            *b ^= 0x20;
        }
    }
    let text = String::from_utf8(bytes).expect("ASCII case mapping keeps UTF-8 valid");
    Ok(Value::String(Str::from(text)))
}
