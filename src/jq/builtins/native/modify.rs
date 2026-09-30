//! `_modify(paths; update)` (`|=`, `+=` and the other update-assignments, `//=`,
//! `map_values`) and `_assign(paths; $value)` (`=`).
//!
//! ```jq
//! def _assign(paths; $value): reduce path(paths) as $p (.; setpath($p; $value));
//! def _modify(paths; update):
//!     reduce path(paths) as $p ([., []];
//!         . as $dot
//!       | null
//!       | label $out
//!       | ($dot[0] | getpath($p)) as $v
//!       | (
//!           (   $$$$v
//!             | update
//!             | (., break $out) as $v
//!             | $$$$dot
//!             | setpath([0] + $p; $v)
//!           ),
//!           (
//!               $$$$dot
//!             | setpath([1, (.[1] | length)]; $p)
//!           )
//!         )
//!     ) | . as $dot | $dot[0] | delpaths($dot[1]);
//! ```
//!
//! `path(paths)` runs on the VM: the native calls the program's `path/1` in a sub-run,
//! so path tracking (and its errors) are jq's. For each path, in turn: `_modify`
//! allocates a label, reads the value, updates it with the first output of `update`
//! (breaking out of the rest) or, if there is none, records the path for deletion at
//! the end; `_assign` sets the value. The paths never leave these definitions (they
//! are only read, by `getpath`/`setpath`/`delpaths`), so their storage doesn't matter.
//! Both abandon closures (`break`, and on errors), so they're off with `?//`.

use super::Update;
use crate::jq::builtins::binops::binop_plus;
use crate::jq::builtins::general::f_length;
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, Outcome, Resume, Stop, Sub};
use crate::jq::value::Value;

/// `_modify(paths; update)` on `input`. `path_fn` is `path/1`.
pub(super) fn modify(
    vm: &mut Jq,
    input: Value,
    paths: Closure,
    update: Closure,
    path_fn: Closure,
) -> Result<Value, Stop> {
    let update = Update::new(vm, update);
    if let Some(opt) = vm.each_closure(paths) {
        return modify_each(vm, input, opt, &update);
    }
    if let Some(k) = vm.key_closure(paths) {
        // `path(.[k])` is `[k]`, or INDEX's error; after the INDEX the path expression
        // holds nothing of the input.
        input.get(&k)?;
        let mut state = State::new(input);
        if matches!(k, Value::Object(_)) {
            // A slice.
            state.step(vm, Value::from(vec![k]), &update)?;
        } else {
            state.step_key(vm, k, &update, false)?;
        }
        return state.finish();
    }
    // `[., []]`: the value being updated and the paths to delete.
    let mut state = State::new(input.clone());
    let mut r = vm.sub_start_args(path_fn, &[paths], input)?;
    while let Some((s, p)) = r {
        if let Err(e) = state.step(vm, p, &update) {
            vm.sub_abandon(s);
            return Err(e);
        }
        r = vm.sub_next(s)?;
    }
    state.finish()
}

/// `_modify(.[]; update)` (`map_values`, `.[] |= f`), or `.[]?` if `opt`: the paths of
/// `path(.[])` are the input's keys in order, so no path expression needs to run.
/// `.[]` holds its container (the input, which the state starts out as) in its fork
/// point until it produces the last element of an array (an object's to the end), so
/// the input is held just as long: the first update then copies it, or updates it in
/// place, as in jq.
fn modify_each(vm: &mut Jq, input: Value, opt: bool, update: &Update) -> Result<Value, Stop> {
    let n = match &input {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ if opt => return Ok(input),
        _ => return Err(super::cannot_iterate(&input)),
    };
    let is_array = matches!(input, Value::Array(_));
    let mut held = Some(input.clone());
    let mut state = State::new(input);
    for i in 0..n {
        let container = held.as_ref().expect("the container");
        let k = match container {
            Value::Array(_) => Value::number(i as f64),
            Value::Object(o) => Value::String(o.get_index(i).expect("key").0.clone()),
            _ => unreachable!(),
        };
        if is_array && i + 1 == n {
            held = None;
        }
        state.step_key(vm, k, update, true)?;
    }
    drop(held);
    state.finish()
}

/// The reduce's state, `[., []]`: the value being updated and the paths to delete.
enum State {
    /// `[root, dels]`, the state's shape as long as every body has an output.
    Pair { root: Value, dels: Vec<Value> },
    /// Any value (see [`other_step`]).
    Other(Value),
}

impl State {
    fn new(root: Value) -> State {
        State::Pair {
            root,
            dels: Vec::new(),
        }
    }

    /// The reduce body for path `p`:
    ///
    /// ```jq
    /// . as $dot | null | label $out | ($dot[0] | getpath($p)) as $v
    /// | (($$$$v | update | (., break $out) as $v | $$$$dot | setpath([0] + $p; $v)),
    ///    ($$$$dot | setpath([1, (.[1] | length)]; $p)))
    /// ```
    fn step(&mut self, vm: &mut Jq, p: Value, update: &Update) -> Result<(), Stop> {
        // `label $out`
        let label = vm.gen_labels(1);
        match self {
            State::Pair { root, dels } => {
                let v = root.getpath(&p)?;
                match update.first(vm, v) {
                    // setpath([0] + $p; $v)
                    Ok(Some(u)) => *root = std::mem::take(root).setpath(&p, u)?,
                    // setpath([1, (.[1] | length)]; $p)
                    Ok(None) => dels.push(p),
                    // The label swallows its own break: no output, so a `null` state.
                    Err(e) if e.is_break_of(label) => *self = State::Other(Value::Null),
                    Err(e) => return Err(e),
                }
            }
            State::Other(dot) => {
                let dot = std::mem::take(dot);
                *self = State::Other(other_step(vm, dot, p, label, |vm, v| update.first(vm, v))?);
            }
        }
        Ok(())
    }

    /// [`State::step`] for the path `[k]` of a key `k` (a number or a string, not a
    /// slice), without making the path: `getpath([k])` is `get(k)`, and
    /// `setpath([k]; u)` is `setpath_rec`'s `get(k)`, `set(k; null)`, a free of what the
    /// get returned, then `set(k; u)`. The path is only made if it is to be deleted.
    ///
    /// `own`: `k` is one of the value's own keys (`.[]`'s), where that sequence leaves
    /// exactly what `set(k; u)` does: it can't fail, whether the container is copied
    /// (and into what storage) depends only on the container, and the old value at `k`
    /// is freed either way, before anything else runs.
    fn step_key(&mut self, vm: &mut Jq, k: Value, update: &Update, own: bool) -> Result<(), Stop> {
        let State::Pair { root, dels } = self else {
            return self.step(vm, Value::from(vec![k]), update);
        };
        // `label $out`
        let label = vm.gen_labels(1);
        let v = root.get(&k)?;
        match update.first(vm, v) {
            // setpath([0] + $p; $v)
            Ok(Some(u)) => {
                let r = std::mem::take(root);
                *root = if own {
                    r.set(&k, u)?
                } else {
                    let subroot = r.get(&k)?;
                    let parent = r.set(&k, Value::Null)?;
                    drop(subroot);
                    parent.set(&k, u)?
                };
            }
            // setpath([1, (.[1] | length)]; $p)
            Ok(None) => dels.push(Value::from(vec![k])),
            // The label swallows its own break: no output, so a `null` state.
            Err(e) if e.is_break_of(label) => *self = State::Other(Value::Null),
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// `. as $dot | $dot[0] | delpaths($dot[1])`.
    fn finish(self) -> Result<Value, Stop> {
        match self {
            State::Pair { root, dels } => {
                if dels.is_empty() {
                    // `delpaths([])` returns its input.
                    return Ok(root);
                }
                // `$dot` still holds the state, so delpaths works on a shared value (a
                // uniquely owned array would be changed in place).
                let held = root.clone();
                let r = root.delpaths(&Value::from(dels));
                drop(held);
                Ok(r?)
            }
            State::Other(dot) => other_finish(dot),
        }
    }
}

/// `_modify`'s reduce body on a state `dot` of any shape, for path `p` in the body of
/// label number `label`; `first` is the update's first output on a value. The state
/// has another shape than `[., []]` only after an update raised its own label's
/// break (`error({"__jq": n})`): the label swallows it, the body has no output, and
/// the reduce's state becomes `null` (jq then usually ends with `delpaths`' "Paths
/// must be specified as an array").
pub(super) fn other_step(
    vm: &mut Jq,
    dot: Value,
    p: Value,
    label: u32,
    first: impl FnOnce(&mut Jq, Value) -> Result<Option<Value>, Stop>,
) -> Result<Value, Stop> {
    // `($dot[0] | getpath($p)) as $v`
    let v = dot.get(&Value::from(0))?.getpath(&p)?;
    match first(vm, v) {
        // `(., break $out) as $v | $$$$dot | setpath([0] + $p; $v)`
        Ok(Some(u)) => {
            let path = binop_plus(Value::from(vec![Value::from(0)]), p)?;
            Ok(dot.setpath(&path, u)?)
        }
        // `$$$$dot | setpath([1, (.[1] | length)]; $p)`
        Ok(None) => {
            let n = f_length(vm, dot.get(&Value::from(1))?, &mut [])?;
            Ok(dot.setpath(&Value::from(vec![Value::from(1), n]), p)?)
        }
        Err(e) if e.is_break_of(label) => Ok(Value::Null),
        Err(e) => Err(e),
    }
}

/// `. as $dot | $dot[0] | delpaths($dot[1])` on a state of any shape.
pub(super) fn other_finish(dot: Value) -> Result<Value, Stop> {
    let root = dot.get(&Value::from(0))?;
    let dels = dot.get(&Value::from(1))?;
    // (`$dot` holds the state meanwhile.)
    let r = root.delpaths(&dels);
    drop(dot);
    Ok(r?)
}
/// `_assign(paths; $value)` on `input`: for each output of `value` (usually just one,
/// read without running anything when it's pure), the input with every path set.
pub(super) fn assign(
    vm: &mut Jq,
    input: Value,
    paths: Closure,
    value: Closure,
    path_fn: Closure,
) -> Outcome {
    if let Some(v) = vm.pure_arg(value, &input) {
        return assign_one(vm, input, paths, path_fn, v).into();
    }
    // `value as $value | ...`: every output of `value`, lazily.
    let first = match vm.sub_start(value, input.clone()) {
        Ok(r) => r,
        Err(e) => return e.into(),
    };
    Box::new(Assign {
        input,
        paths,
        path_fn,
        sub: None,
    })
    .next(vm, first)
}

fn assign_one(
    vm: &mut Jq,
    input: Value,
    paths: Closure,
    path_fn: Closure,
    v: Value,
) -> Result<Value, Stop> {
    if let Some(k) = vm.key_closure(paths) {
        // `path(.[k])` (see `modify`), then `setpath([k]; $value)`.
        input.get(&k)?;
        return Ok(input.setpath(&Value::from(vec![k]), v)?);
    }
    // The reduce's state starts as `.` itself.
    let mut state = input.clone();
    let mut r = vm.sub_start_args(path_fn, &[paths], input)?;
    while let Some((s, p)) = r {
        match std::mem::take(&mut state).setpath(&p, v.clone()) {
            Ok(x) => state = x,
            Err(e) => {
                vm.sub_abandon(s);
                return Err(e.into());
            }
        }
        r = vm.sub_next(s)?;
    }
    Ok(state)
}

/// `_assign` with a `value` that has (or may have) several outputs.
struct Assign {
    input: Value,
    paths: Closure,
    path_fn: Closure,
    /// `value`'s suspended run.
    sub: Option<Sub>,
}

impl Assign {
    fn next(mut self: Box<Self>, vm: &mut Jq, r: Option<(Sub, Value)>) -> Outcome {
        let Some((s, v)) = r else {
            return Outcome::Empty;
        };
        // jq keeps the input alive (below `value`'s fork points) only while `value` has
        // fork points left; after its last output the reduce owns it (and may update a
        // uniquely owned array in place).
        if vm.sub_idle(&s) {
            vm.sub_finish(s);
            return assign_one(vm, self.input, self.paths, self.path_fn, v).into();
        }
        match assign_one(vm, self.input.clone(), self.paths, self.path_fn, v) {
            Ok(x) => {
                self.sub = Some(s);
                Outcome::Yield(x, self)
            }
            Err(e) => {
                vm.sub_abandon(s);
                e.into()
            }
        }
    }
}

impl Resume for Assign {
    fn resume(mut self: Box<Self>, vm: &mut Jq) -> Outcome {
        let s = self.sub.take().expect("suspended value");
        match vm.sub_next(s) {
            Ok(r) => self.next(vm, r),
            Err(e) => e.into(),
        }
    }

    fn unwind(mut self: Box<Self>, vm: &mut Jq) {
        if let Some(s) = self.sub.take() {
            vm.sub_unwind(s);
        }
    }
}
