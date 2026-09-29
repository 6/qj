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
    // `[., []]`: the value being updated and the paths to delete.
    let mut root = input.clone();
    let mut dels: Vec<Value> = Vec::new();
    let mut r = vm.sub_start_args(path_fn, &[paths], input)?;
    while let Some((s, p)) = r {
        if let Err(e) = modify_one(vm, &mut root, &mut dels, p, update) {
            vm.sub_abandon(s);
            return Err(e);
        }
        r = vm.sub_next(s)?;
    }
    if dels.is_empty() {
        // `delpaths([])` returns its input.
        return Ok(root);
    }
    Ok(root.delpaths(&Value::from(dels))?)
}

/// The reduce body for path `p`.
fn modify_one(
    vm: &mut Jq,
    root: &mut Value,
    dels: &mut Vec<Value>,
    p: Value,
    update: Closure,
) -> Result<(), Stop> {
    // `label $out`
    vm.gen_labels(1);
    let v = root.getpath(&p)?;
    match vm.sub_first(update, v)? {
        // setpath([0] + $p; $v)
        Some(u) => *root = std::mem::take(root).setpath(&p, u)?,
        // setpath([1, (.[1] | length)]; $p)
        None => dels.push(p),
    }
    Ok(())
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
