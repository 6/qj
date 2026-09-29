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

use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, Stop};
use crate::jq::value::{Array, Object, Value};

pub(super) fn walk(vm: &mut Jq, input: Value, f: Closure, c: ConstView<'_>) -> Outcome {
    match transform(vm, input, f, c.get(0)) {
        Ok(t) => Outcome::Call(f, t),
        Err(s) => s.into(),
    }
}

/// A container whose children are being walked.
enum Frame {
    /// `map(w)`: child `i` of `src` is being walked; `out` collects.
    Arr { src: Array, i: usize, out: Array },
    /// `map_values(w)`: key `i` of `src` is being walked; `obj` gets the updates and
    /// `dels` the keys whose `w` was empty.
    Obj {
        src: Object,
        i: usize,
        obj: Object,
        dels: Vec<Value>,
    },
}

/// `t` of `root`: `root` with `w` applied to its children.
fn transform(vm: &mut Jq, root: Value, f: Closure, map_empty: &Value) -> Result<Value, Stop> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut cur = root;
    loop {
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
                    vm.gen_labels(1);
                    let first = o.get_index(0).expect("non-empty").1.clone();
                    stack.push(Frame::Obj {
                        src: o.clone(),
                        i: 0,
                        obj: o,
                        dels: Vec::new(),
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
            let next = match top {
                Frame::Arr { src, i, out } => {
                    vm.sub_each(f, t, |_, v| {
                        out.push(v);
                        Ok(())
                    })?;
                    *i += 1;
                    src.get(*i).cloned()
                }
                Frame::Obj { src, i, obj, dels } => {
                    let (k, _) = src.get_index(*i).expect("walked key");
                    match vm.sub_first(f, t)? {
                        // setpath([0] + $p; $v)
                        Some(u) => obj.insert(k.clone(), u),
                        // setpath([1, (.[1] | length)]; $p)
                        None => dels.push(Value::from(vec![Value::String(k.clone())])),
                    }
                    *i += 1;
                    let next = src.get_index(*i).map(|(_, v)| v.clone());
                    if next.is_some() {
                        vm.gen_labels(1);
                    }
                    next
                }
            };
            if let Some(child) = next {
                cur = child;
                break;
            }
            t = match stack.pop().expect("top frame") {
                Frame::Arr { out, .. } => Value::Array(out),
                Frame::Obj { obj, dels, .. } => finish(obj, dels)?,
            };
        }
    }
}

/// `$dot[0] | delpaths($dot[1])`.
fn finish(obj: Object, dels: Vec<Value>) -> Result<Value, Stop> {
    if dels.is_empty() {
        return Ok(Value::Object(obj));
    }
    Ok(Value::Object(obj).delpaths(&Value::from(dels))?)
}
