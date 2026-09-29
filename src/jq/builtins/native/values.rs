//! `recurse` (`..`), the type filters, `add` and `_flatten`.
//!
//! ```jq
//! def recurse(f): def r: ., (f | r); r;
//! def recurse: recurse(.[]?);
//! def values: select(. != null);        # and nulls, booleans, numbers, strings,
//! def scalars: select(type|. != "array" and . != "object");   # arrays, objects, iterables
//! def add(f): reduce f as $x (null; . + $x);
//! def add: add(.[]);
//! def _flatten($x): reduce .[] as $i ([]; if $i | type == "array" and $x != 0 then . + ($i | _flatten($x-1)) else . + [$i] end);
//! ```

use super::{Level, cannot_iterate, child, children};
use crate::jq::builtins::binops::{binop_minus, binop_plus};
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, Resume, Stop};
use crate::jq::value::Value;

// ---- recurse ----------------------------------------------------------------------

/// `recurse` (and `..`) outside path expressions: the nodes in pre-order. jq's `,` fork
/// point in `r` holds each node until its second branch, and `.[]?`'s fork point holds
/// a container until its last child (an array; an object until the end), so the
/// generator does the same.
pub(super) fn recurse(input: Value) -> Outcome {
    let v = input.clone();
    Outcome::Yield(
        v,
        Box::new(Recurse {
            stack: Vec::new(),
            node: input,
        }),
    )
}

struct Recurse {
    stack: Vec<Level>,
    /// The node yielded last.
    node: Value,
}

impl Resume for Recurse {
    fn resume(mut self: Box<Self>, _vm: &mut Jq) -> Outcome {
        let node = std::mem::take(&mut self.node);
        let n = children(&node);
        let next = if n > 0 {
            let (_, c) = child(&node, 0);
            let node = if n == 1 && is_array(&node) {
                Value::Null
            } else {
                node
            };
            self.stack.push(Level { node, i: 0, n });
            Some(c)
        } else {
            drop(node);
            loop {
                let Some(top) = self.stack.last_mut() else {
                    break None;
                };
                if top.i + 1 < top.n {
                    top.i += 1;
                    let (_, c) = child(&top.node, top.i);
                    if top.i + 1 == top.n && is_array(&top.node) {
                        top.node = Value::Null;
                    }
                    break Some(c);
                }
                self.stack.pop();
            }
        };
        match next {
            Some(c) => {
                self.node = c.clone();
                Outcome::Yield(c, self)
            }
            None => Outcome::Empty,
        }
    }
}

// ---- type filters -----------------------------------------------------------------

/// A type filter: the input if `keep` accepts it (the condition can't fail, and select
/// leaves no fork points behind).
pub(super) fn select_if(input: Value, keep: fn(&Value) -> bool) -> Outcome {
    if keep(&input) {
        Outcome::Value(input)
    } else {
        Outcome::Empty
    }
}

pub(super) fn is_value(v: &Value) -> bool {
    !v.is_null()
}
pub(super) fn is_null(v: &Value) -> bool {
    v.is_null()
}
pub(super) fn is_boolean(v: &Value) -> bool {
    matches!(v, Value::Bool(_))
}
pub(super) fn is_number(v: &Value) -> bool {
    matches!(v, Value::Number(_))
}
pub(super) fn is_string(v: &Value) -> bool {
    matches!(v, Value::String(_))
}
pub(super) fn is_array(v: &Value) -> bool {
    matches!(v, Value::Array(_))
}
pub(super) fn is_object(v: &Value) -> bool {
    matches!(v, Value::Object(_))
}
pub(super) fn is_iterable(v: &Value) -> bool {
    matches!(v, Value::Array(_) | Value::Object(_))
}
pub(super) fn is_scalar(v: &Value) -> bool {
    !is_iterable(v)
}

// ---- add ----------------------------------------------------------------------------

/// `add`: `reduce .[] as $x (null; . + $x)`.
///
/// `.[]` (`EACH`) lets go of an array when it produces its last element (not of an
/// object), so by the last `+` the accumulator (which starts out as the first element
/// itself) may be uniquely owned and extended in place: the array is dropped there too.
pub(super) fn add0(input: Value) -> Result<Value, Stop> {
    let n = match &input {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => return Err(cannot_iterate(&input)),
    };
    let mut input = Some(input);
    let mut acc = Value::Null;
    for i in 0..n {
        let container = input.as_ref().expect("container");
        let x = elem(container, i).expect("element").clone();
        if i + 1 == n && matches!(container, Value::Array(_)) {
            input = None;
        }
        acc = binop_plus(std::mem::take(&mut acc), x)?;
    }
    Ok(acc)
}

/// `add(f)`: `reduce f as $x (null; . + $x)`. A failing `+` propagates back through
/// `f`'s fork points (`?//` could catch it), so this one is off with `?//`.
pub(super) fn add1(vm: &mut Jq, input: Value, f: Closure) -> Result<Value, Stop> {
    let mut acc = Value::Null;
    vm.sub_each(f, input, |_, x| {
        acc = binop_plus(std::mem::take(&mut acc), x)?;
        Ok(())
    })?;
    Ok(acc)
}

// ---- _flatten -------------------------------------------------------------------------

/// `_flatten($x)` for a pure `$x` (flatten and flatten/1 call it; `flatten`'s `-1` is
/// `1 | -.`, which `pure_arg` evaluates). `c`: the reduce's `[]`, then the literal `0`
/// of `$x != 0` (a literal compares exactly: `1E-400 != 0`), then the `1` of `$x-1`.
pub(super) fn flatten(vm: &mut Jq, input: Value, xf: Closure, c: ConstView<'_>) -> Outcome {
    match vm.pure_arg(xf, &input) {
        Some(x) => flatten_with(input, x, c).into(),
        None => Outcome::Fallback(input),
    }
}

/// One `_flatten` call being reduced: `elems` of its input, with the next index.
struct Flat {
    elems: Value,
    i: usize,
    acc: Value,
    x: Value,
}

fn elem(v: &Value, i: usize) -> Option<&Value> {
    match v {
        Value::Array(a) => a.get(i),
        Value::Object(o) => o.get_index(i).map(|(_, v)| v),
        _ => None,
    }
}

fn flatten_with(input: Value, x: Value, c: ConstView<'_>) -> Result<Value, Stop> {
    if !matches!(input, Value::Array(_) | Value::Object(_)) {
        return Err(cannot_iterate(&input));
    }
    let (empty, zero, one) = (c.get(0), c.get(1), c.get(2));
    let mut stack = vec![Flat {
        elems: input,
        i: 0,
        acc: empty.clone(),
        x,
    }];
    loop {
        let top = stack.last_mut().expect("a _flatten call");
        let Some(i) = elem(&top.elems, top.i).cloned() else {
            // The reduce is done: `. + ($i | _flatten($x-1))` in the caller.
            let done = stack.pop().expect("a _flatten call").acc;
            match stack.last_mut() {
                None => return Ok(done),
                Some(parent) => {
                    parent.acc = binop_plus(std::mem::take(&mut parent.acc), done)?;
                    continue;
                }
            }
        };
        top.i += 1;
        // `$i | type == "array" and $x != 0`
        if matches!(i, Value::Array(_)) && !top.x.equal(zero) {
            // `_flatten($x-1)` on `$i`: its `$x` first, then its reduce.
            let x = binop_minus(top.x.clone(), one.clone())?;
            stack.push(Flat {
                elems: i,
                i: 0,
                acc: empty.clone(),
                x,
            });
        } else {
            // `. + [$i]`
            top.acc = binop_plus(std::mem::take(&mut top.acc), Value::from(vec![i]))?;
        }
    }
}
