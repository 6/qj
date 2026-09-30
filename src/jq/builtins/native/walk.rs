//! `walk(f)`.
//!
//! ```jq
//! def walk(f):
//!   def w:
//!     if type == "object"
//!     then map_values(w)
//!     elif type == "array" then map(w)
//!     else .
//!     end
//!     | f;
//!   w;
//! ```
//!
//! `w` on a value is `t | f`, where `t` applies `w` to the children: an array's
//! children through `map(w)` (every output of `w`, collected into map's `[]`), an
//! object's through `map_values(w)`, which is `_modify(.[]; w)`: for each key in turn
//! it allocates a label, takes the first output of `w` on the value (and `break`s out
//! of the rest), and deletes the key if there is none (with `delpaths`, at the end).
//! The native computes the top-level `t` depth first with an explicit stack, running
//! `f` on each inner node as the definition would, and then tail-calls `f` on it.
//! Taking only the first output of `f` needs `break`, so it's off with `?//`.
//!
//! The usual `f`, `if type == "T" then A else . end`, isn't run on nodes of other
//! types: there its one output is the node, and running it does nothing else.

use super::Update;
use super::modify::{other_finish, other_step};
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, Stop};
use crate::jq::value::{Array, Object, Value};

pub(super) fn walk(vm: &mut Jq, input: Value, f: Closure, c: ConstView<'_>) -> Outcome {
    let f = Update::new(vm, f);
    match transform(vm, input, &f, c.get(0)) {
        Ok(t) if f.skips(&t) => Outcome::Value(t),
        Ok(t) => Outcome::Call(f.f, t),
        Err(s) => s.into(),
    }
}

/// A container whose children are being walked.
enum Frame {
    /// `map(w)`: child `i` of `src` is being walked; `out` collects.
    Arr { src: Array, i: usize, out: Array },
    /// `map_values(w)`: key `i` of `src` is being walked, in the body of label number
    /// `label`; `updates` has the new values by position (`setpath([0] + $p; $v)`,
    /// applied together at the end: no key moves, and nothing sees the object in
    /// between) and `dels` the keys whose `w` was empty.
    Obj {
        src: Object,
        i: usize,
        updates: Vec<Option<Value>>,
        dels: Vec<Value>,
        label: u32,
    },
}

/// `t` of `root`: `root` with `w` applied to its children.
fn transform(vm: &mut Jq, root: Value, f: &Update, map_empty: &Value) -> Result<Value, Stop> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut cur = root;
    'descend: loop {
        // Descend to the first node whose `t` is itself.
        let mut t = loop {
            match cur {
                Value::Array(a) if !a.is_empty() => {
                    let first = a.as_slice()[0].clone();
                    let Value::Array(out) = map_empty.clone() else {
                        unreachable!()
                    };
                    stack.push(Frame::Arr { src: a, i: 0, out });
                    cur = first;
                }
                // `map(w)` on `[]` is map's `[]`.
                Value::Array(_) => break map_empty.clone(),
                Value::Object(o) if !o.is_empty() => {
                    // `_modify`'s `label $out`, for the first key.
                    let label = vm.gen_labels(1);
                    let first = o.get_index(0).expect("non-empty").1.clone();
                    let n = o.len();
                    stack.push(Frame::Obj {
                        src: o,
                        i: 0,
                        updates: Vec::with_capacity(n),
                        dels: Vec::new(),
                        label,
                    });
                    cur = first;
                }
                // `map_values(w)` on `{}` returns it (`delpaths([])`); scalars are `.`.
                v => break v,
            }
        };
        // Ascend: `t` belongs to the current child of the top frame.
        loop {
            let Some(top) = stack.last_mut() else {
                return Ok(t);
            };
            t = match feed(vm, top, t, f) {
                Ok(Some(child)) => {
                    cur = child;
                    continue 'descend;
                }
                Ok(None) => match stack.pop().expect("top frame") {
                    Frame::Arr { out, .. } => Value::Array(out),
                    Frame::Obj {
                        src, updates, dels, ..
                    } => match finish(src, updates, dels) {
                        Ok(v) => v,
                        Err(e) => unwind(vm, &mut stack, e, f, map_empty)?,
                    },
                },
                Err(e) => unwind(vm, &mut stack, e, f, map_empty)?,
            };
        }
    }
}

/// `t`, the walked value of the top frame's current child, goes to its container; then
/// the next child to walk, or `None` when the container is done.
fn feed(vm: &mut Jq, top: &mut Frame, t: Value, f: &Update) -> Result<Option<Value>, Stop> {
    match top {
        Frame::Arr { src, i, out } => {
            if f.skips(&t) {
                out.push(t);
            } else {
                vm.sub_each(f.f, t, |_, v| {
                    out.push(v);
                    Ok(())
                })?;
            }
            *i += 1;
            Ok(src.get(*i).cloned())
        }
        Frame::Obj {
            src,
            i,
            updates,
            dels,
            label,
        } => {
            let u = f.first(vm, t)?;
            if u.is_none() {
                // setpath([1, (.[1] | length)]; $p)
                let (k, _) = src.get_index(*i).expect("walked key");
                dels.push(Value::from(vec![Value::String(k.clone())]));
            }
            updates.push(u);
            *i += 1;
            let next = src.get_index(*i).map(|(_, v)| v.clone());
            if next.is_some() {
                *label = vm.gen_labels(1);
            }
            Ok(next)
        }
    }
}

/// An error raised while the top frame's current child was walked (or its container
/// finished): it unwinds the containers being walked, up to the first object whose
/// current key's label it is the break of (`error({"__jq": n})`): the label swallows it
/// there, and that object's `_modify` goes on without the body's output (see
/// `modify::other_step`). Returns that object's walked value, or the error if it gets
/// out of the walk.
fn unwind(
    vm: &mut Jq,
    stack: &mut Vec<Frame>,
    e: Stop,
    f: &Update,
    map_empty: &Value,
) -> Result<Value, Stop> {
    let mut e = e;
    loop {
        let Some(top) = stack.pop() else {
            return Err(e);
        };
        if let Frame::Obj { src, i, label, .. } = top
            && e.is_break_of(label)
        {
            match other_rest(vm, &src, i + 1, f, map_empty) {
                Ok(t) => return Ok(t),
                // Raised in the parent's current child.
                Err(e2) => e = e2,
            }
        }
    }
}

/// The rest of `map_values(w)` on `src` from key `from` on, after the reduce's state
/// became `null` (the body for key `from - 1` had no output), then its result.
fn other_rest(
    vm: &mut Jq,
    src: &Object,
    from: usize,
    f: &Update,
    map_empty: &Value,
) -> Result<Value, Stop> {
    let mut dot = Value::Null;
    for j in from..src.len() {
        // `label $out`
        let label = vm.gen_labels(1);
        let k = Value::String(src.get_index(j).expect("key").0.clone());
        dot = other_step(vm, dot, Value::from(vec![k]), label, |vm, v| {
            // `w`: `t | f`, the first output.
            let t = transform(vm, v, f, map_empty)?;
            f.first(vm, t)
        })?;
    }
    other_finish(dot)
}
/// The updates, then `$dot[0] | delpaths($dot[1])`.
fn finish(mut obj: Object, updates: Vec<Option<Value>>, dels: Vec<Value>) -> Result<Value, Stop> {
    if updates.iter().any(Option::is_some) {
        // The first setpath copies the (shared) object; the rest write in place.
        for (v, u) in obj.values_mut().zip(updates) {
            if let Some(u) = u {
                *v = u;
            }
        }
    }
    if dels.is_empty() {
        return Ok(Value::Object(obj));
    }
    Ok(Value::Object(obj).delpaths(&Value::from(dels))?)
}
