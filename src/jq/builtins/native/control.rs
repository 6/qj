//! `first(g)`, `limit($n; g)`, `isempty(g)`, `any`/`all` and `IN`.
//!
//! ```jq
//! def first(g): label $out | g | ., break $out;
//! def limit($n; expr):
//!   if $n > 0 then label $out | foreach expr as $item ($n; . - 1; $item, if . <= 0 then break $out else empty end)
//!   elif $n == 0 then empty
//!   else error("limit doesn't support negative count") end;
//! def isempty(g): first((g|false), true);
//! def all(generator; condition): isempty(generator|condition and empty);
//! def any(generator; condition): isempty(generator|condition or empty)|not;
//! def all(condition): all(.[]; condition);   def any(condition): any(.[]; condition);
//! def all: all(.[]; .);                      def any: any(.[]; .);
//! def IN(s): any(s == .; .);
//! def IN(src; s): any(src == s; .);
//! ```
//!
//! Each allocates `first`'s (or `limit`'s) label, and returns its value with the
//! definition's fork points still on the stack: the label's `try` (holding the input),
//! the closures' fork points, and `., break $out` (holding the value). They are
//! released when the caller backtracks (then `break` abandons the closures), so the
//! generators keep the same things until they are resumed. `break` is an error that
//! `?//` could catch, so these are off with `?//`.

use super::cannot_iterate;
use crate::jq::builtins::binops::{binop_equal, binop_greater, binop_lesseq, binop_minus};
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, Raised, Resume, Stop, Sub};
use crate::jq::value::Value;

/// A finished `first`: what its fork points hold until the caller backtracks. The
/// suspended sub-runs are abandoned then, innermost first.
struct Held {
    subs: Vec<Sub>,
    _values: Vec<Value>,
}

impl Held {
    fn new(subs: Vec<Sub>, values: Vec<Value>) -> Box<Held> {
        Box::new(Held {
            subs,
            _values: values,
        })
    }
}

impl Resume for Held {
    fn resume(mut self: Box<Self>, vm: &mut Jq) -> Outcome {
        while let Some(s) = self.subs.pop() {
            vm.sub_abandon(s);
        }
        Outcome::Empty
    }

    fn unwind(mut self: Box<Self>, vm: &mut Jq) {
        while let Some(s) = self.subs.pop() {
            vm.sub_unwind(s);
        }
    }
}

/// An error a closure raised inside the body of `label` number `label`: nothing more
/// if it is the label's own break (the label's handler ends the body quietly, whoever
/// raised it: `error({"__jq": 0})` does too), else the error.
fn raised_in_label(e: Stop, label: u32) -> Outcome {
    if e.is_break_of(label) {
        Outcome::Empty
    } else {
        e.into()
    }
}

/// `first(g)`.
pub(super) fn first(vm: &mut Jq, input: Value, g: Closure) -> Outcome {
    let label = vm.gen_labels(1);
    match vm.sub_start(g, input.clone()) {
        Err(e) => raised_in_label(e, label),
        Ok(None) => Outcome::Empty,
        Ok(Some((s, v))) => Outcome::Yield(v.clone(), Held::new(vec![s], vec![input, v])),
    }
}

/// `isempty(g)`: `false` as soon as `g` has an output, `true` if it has none.
pub(super) fn isempty(vm: &mut Jq, input: Value, g: Closure) -> Outcome {
    let label = vm.gen_labels(1);
    match vm.sub_start(g, input.clone()) {
        Err(e) => raised_in_label(e, label),
        Ok(None) => Outcome::Yield(Value::Bool(true), Held::new(vec![], vec![input])),
        Ok(Some((s, _))) => Outcome::Yield(Value::Bool(false), Held::new(vec![s], vec![input])),
    }
}

/// `any(gen; cond)` (`want` true) or `all(gen; cond)` (`want` false): whether some
/// output of `cond` on an output of `gen` is truthy (`any`) or falsy (`all` then is
/// false). `generator` is `None` for `.[]` (the one-argument forms), `cond` `None` for `.`.
pub(super) fn any_all(
    vm: &mut Jq,
    input: Value,
    generator: Option<Closure>,
    cond: Option<Closure>,
    any: bool,
) -> Outcome {
    let label = vm.gen_labels(1);
    // The result when some `cond` output decides it, and when none does.
    let (found, none) = (Value::Bool(any), Value::Bool(!any));
    // Does `c` decide the result?
    let decides = |c: &Value| c.is_truthy() == any;
    // One element of `gen`: run `cond` on it until it decides. Returns the suspended
    // `cond` run if it decided.
    let element = |vm: &mut Jq, x: Value| -> Result<Option<Option<Sub>>, Stop> {
        let Some(cond) = cond else {
            return Ok(decides(&x).then_some(None));
        };
        let mut r = vm.sub_start(cond, x)?;
        while let Some((s, c)) = r {
            if decides(&c) {
                return Ok(Some(Some(s)));
            }
            r = vm.sub_next(s)?;
        }
        Ok(None)
    };
    match generator {
        None => {
            // `.[]`, which holds the input anyway (as `first`'s try does).
            let elems: Vec<Value> = match &input {
                Value::Array(a) => a.iter().cloned().collect(),
                Value::Object(o) => o.values().cloned().collect(),
                _ => return raised_in_label(cannot_iterate(&input), label),
            };
            for x in elems {
                match element(vm, x) {
                    Err(e) => return raised_in_label(e, label),
                    Ok(Some(s)) => {
                        return Outcome::Yield(
                            found,
                            Held::new(s.into_iter().collect(), vec![input]),
                        );
                    }
                    Ok(None) => {}
                }
            }
        }
        Some(generator) => {
            let mut r = match vm.sub_start(generator, input.clone()) {
                Ok(r) => r,
                Err(e) => return raised_in_label(e, label),
            };
            while let Some((g, x)) = r {
                match element(vm, x) {
                    Err(e) => {
                        vm.sub_abandon(g);
                        return raised_in_label(e, label);
                    }
                    Ok(Some(s)) => {
                        let mut subs = vec![g];
                        subs.extend(s);
                        return Outcome::Yield(found, Held::new(subs, vec![input]));
                    }
                    Ok(None) => {}
                }
                r = match vm.sub_next(g) {
                    Ok(r) => r,
                    Err(e) => return raised_in_label(e, label),
                };
            }
        }
    }
    Outcome::Yield(none, Held::new(vec![], vec![input]))
}

/// `IN(s)` (`src` `None`) or `IN(src; s)`: whether an output of `src` (or the input)
/// equals an output of `s`. In `src == s`, `s` is evaluated first (the outer loop).
pub(super) fn is_in(vm: &mut Jq, input: Value, src: Option<Closure>, s: Closure) -> Outcome {
    let label = vm.gen_labels(1);
    let mut rs = match vm.sub_start(s, input.clone()) {
        Ok(r) => r,
        Err(e) => return raised_in_label(e, label),
    };
    while let Some((ss, b)) = rs {
        match src {
            None => {
                if binop_equal(b, input.clone()).is_ok_and(|v| v.is_truthy()) {
                    return Outcome::Yield(Value::Bool(true), Held::new(vec![ss], vec![input]));
                }
            }
            Some(src) => {
                let mut ra = match vm.sub_start(src, input.clone()) {
                    Ok(r) => r,
                    Err(e) => {
                        vm.sub_abandon(ss);
                        return raised_in_label(e, label);
                    }
                };
                while let Some((sa, a)) = ra {
                    if binop_equal(a, b.clone()).is_ok_and(|v| v.is_truthy()) {
                        return Outcome::Yield(
                            Value::Bool(true),
                            Held::new(vec![ss, sa], vec![input]),
                        );
                    }
                    ra = match vm.sub_next(sa) {
                        Ok(r) => r,
                        Err(e) => {
                            vm.sub_abandon(ss);
                            return raised_in_label(e, label);
                        }
                    };
                }
            }
        }
        rs = match vm.sub_next(ss) {
            Ok(r) => r,
            Err(e) => return raised_in_label(e, label),
        };
    }
    Outcome::Yield(Value::Bool(false), Held::new(vec![], vec![input]))
}

/// `limit($n; expr)` for a pure `$n`. `c`: the literal `0` (for `$n > 0`, `$n == 0` and
/// `. <= 0`, which compare literals exactly), the `1` of `. - 1`, and the error text.
pub(super) fn limit(
    vm: &mut Jq,
    input: Value,
    nf: Closure,
    expr: Closure,
    c: ConstView<'_>,
) -> Outcome {
    let Some(n) = vm.pure_arg(nf, &input) else {
        return Outcome::Fallback(input);
    };
    let (zero, one) = (c.get(0), c.get(1));
    let truthy = |r: Result<Value, crate::jq::value::Error>| r.is_ok_and(|v| v.is_truthy());
    if truthy(binop_greater(n.clone(), zero.clone())) {
        let label = vm.gen_labels(1);
        let first = match vm.sub_start(expr, input.clone()) {
            Ok(r) => r,
            Err(e) => return raised_in_label(e, label),
        };
        Box::new(Limit {
            _input: input,
            state: n,
            item: Value::Null,
            sub: None,
            zero: zero.clone(),
            one: one.clone(),
            label,
        })
        .next(vm, first)
    } else if truthy(binop_equal(n, zero.clone())) {
        Outcome::Empty
    } else {
        Outcome::Raise(Raised::new(c.get(2).clone()))
    }
}

struct Limit {
    /// Held by the label's `try`.
    _input: Value,
    /// The foreach's state.
    state: Value,
    /// `$item`, held until the next item replaces it.
    item: Value,
    sub: Option<Sub>,
    zero: Value,
    one: Value,
    /// The `label $out` expr runs in.
    label: u32,
}

impl Limit {
    /// The foreach body for `expr`'s next output (`r`): the update, then `$item`.
    fn next(mut self: Box<Self>, vm: &mut Jq, r: Option<(Sub, Value)>) -> Outcome {
        let Some((s, item)) = r else {
            return Outcome::Empty;
        };
        match binop_minus(std::mem::take(&mut self.state), self.one.clone()) {
            Ok(state) => {
                self.state = state;
                self.item = item.clone();
                self.sub = Some(s);
                Outcome::Yield(item, self)
            }
            Err(e) => {
                vm.sub_abandon(s);
                raised_in_label(Stop::from(e), self.label)
            }
        }
    }
}

impl Resume for Limit {
    fn resume(mut self: Box<Self>, vm: &mut Jq) -> Outcome {
        let s = self.sub.take().expect("suspended expr");
        // `if . <= 0 then break $out else empty end`
        let done = binop_lesseq(self.state.clone(), self.zero.clone()).is_ok_and(|v| v.is_truthy());
        if done {
            vm.sub_abandon(s);
            return Outcome::Empty;
        }
        match vm.sub_next(s) {
            Ok(r) => self.next(vm, r),
            Err(e) => raised_in_label(e, self.label),
        }
    }

    fn unwind(mut self: Box<Self>, vm: &mut Jq) {
        if let Some(s) = self.sub.take() {
            vm.sub_unwind(s);
        }
    }
}
