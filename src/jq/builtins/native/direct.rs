//! `select(f)`, `map(f)` and `repeat(f)`, when `f` is a direct region.
//!
//! ```jq
//! def select(f): if f then . else empty end;
//! def map(f): [.[] | f];
//! def repeat(exp): def _repeat: exp, _repeat; _repeat;
//! ```
//!
//! These run their closure once per value, which the definitions do with a frame, a
//! call and fork points per value. When the closure's body is a region
//! (`lang/execute/region.rs`; `Jq::direct`), it has at most one output, no fork points,
//! and does nothing but its value operations, so evaluating it directly, on the same
//! values in the same order, is all the definition does; otherwise the definition runs.
//!
//! What the definitions hold while the closure runs, the natives hold too: `select`'s
//! and `map`'s input (its `DUP`s, and `map`'s `FORK`, keep a copy until the end), and
//! `repeat`'s (its `,`'s fork point). Nothing is abandoned (a direct closure has no fork
//! points), so they also run in programs using `?//`.

use super::cannot_iterate;
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, Resume};
use crate::jq::value::Value;

/// `select(f)`: the input if `f`'s output is truthy; nothing if it's falsy or `f` has
/// none; `f`'s error.
pub(super) fn select(vm: &mut Jq, input: Value, f: Closure) -> Outcome {
    let Some(d) = vm.direct(f) else {
        return Outcome::Fallback(input);
    };
    match vm.eval_direct(d, f, input.clone()) {
        Ok(Some(c)) if c.is_truthy() => Outcome::Value(input),
        Ok(_) => Outcome::Empty,
        Err(e) => e.into(),
    }
}

/// `map(f)`: `f`'s output on each value of `.[]`, in order, appended to map's `[]`
/// (`c.get(0)`) as `APPEND` does. `.[]` copies each element (and the definition's
/// `FORK` holds the input throughout, so no element is ever uniquely owned).
pub(super) fn map(vm: &mut Jq, input: Value, f: Closure, c: ConstView<'_>) -> Outcome {
    let Some(d) = vm.direct(f) else {
        return Outcome::Fallback(input);
    };
    let Value::Array(mut out) = c.get(0).clone() else {
        unreachable!("map's []");
    };
    let mut each = |x: &Value| -> Result<(), Outcome> {
        match vm.eval_direct(d, f, x.clone()) {
            Ok(Some(v)) => out.push(v),
            Ok(None) => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    };
    let r = match &input {
        Value::Array(a) => a.iter().try_for_each(&mut each),
        Value::Object(o) => o.values().try_for_each(&mut each),
        _ => return cannot_iterate(&input).into(),
    };
    match r {
        Ok(()) => Outcome::Value(Value::Array(out)),
        Err(e) => e,
    }
}

/// `repeat(exp)` when `exp` always has an output (or raises): `exp` on the input,
/// forever. (One that can have none would make jq loop without output; the definition
/// runs then.)
pub(super) fn repeat(vm: &mut Jq, input: Value, exp: Closure) -> Outcome {
    match vm.direct(exp) {
        Some(d) if vm.direct_always_outputs(d) => Box::new(Repeat { input, exp, d }).next(vm),
        _ => Outcome::Fallback(input),
    }
}

struct Repeat {
    /// Held by `_repeat`'s `,`.
    input: Value,
    exp: Closure,
    d: u32,
}

impl Repeat {
    fn next(self: Box<Self>, vm: &mut Jq) -> Outcome {
        match vm.eval_direct(self.d, self.exp, self.input.clone()) {
            Ok(Some(v)) => Outcome::Yield(v, self),
            Ok(None) => unreachable!("a region that always has an output"),
            Err(e) => e.into(),
        }
    }
}

impl Resume for Repeat {
    fn resume(self: Box<Self>, vm: &mut Jq) -> Outcome {
        self.next(vm)
    }
}
