//! Native implementations of `builtin.jq` definitions.
//!
//! jq defines many builtins in its own language (`walk`, `paths`, `to_entries`, ...).
//! qj runs `builtin.jq` verbatim, so these run on the faithful VM by default; the
//! functions here produce the same results without the interpreter overhead. The VM
//! side (sub-runs, suspended generators, when a native may run) is
//! `lang/execute/native.rs`; its docs explain the mechanism.
//!
//! # The rule: mirror the definition's value operations
//!
//! A native must be observably identical to its definition, and jq observes more than
//! values: array storage (views share it, `jv_equal` compares shared views without
//! looking at elements, and a write into a unique view can bring back elements past
//! its end), identity (`path(...)` accepts a value only if it is `jv_identical` to the
//! tracked one, so a result that is a constant of the definition's bytecode can be
//! told apart from a fresh copy), label numbers (every `label` allocates one, and
//! `break` values show them) and the order of every closure call. So each native:
//!
//! * performs the definition's value operations in the same order with the same
//!   ownership: arrays are built by appending to the same constant `[]` the definition
//!   collects into (so they get jq's allocation sizes), values are cloned where jq
//!   copies them and moved where it moves them, and results that are constants in jq
//!   are the same constants here (see [`consts`]);
//! * raises the same errors at the same points (by calling the same C builtins and
//!   value operations);
//! * calls its closures in the same order, the same number of times, on the same
//!   inputs, and stops calling them where jq would (laziness);
//! * advances the label counter as the definition's `label`s would.
//!
//! The VM falls back to the bytecode where natives can't be exact (see
//! `Jq::natives_ok`): inside path expressions, with `--debug-trace`, and, for natives
//! that abandon or suspend closure arguments, in programs using `?//`.
//!
//! Every native is checked against its definition by `tests.rs` (in-process, on
//! generated programs and inputs), by the `fast_builtins.test` jq_diff corpus, and by
//! the differential fuzzer.

mod control;
mod entries;
mod modify;
mod paths;
mod strings;
mod values;
mod walk;

#[cfg(test)]
mod cases;
#[cfg(test)]
mod tests;

use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Call, ConstRef, Outcome, Pool, Pools, Resume, Stop};
use crate::jq::value::{Error, Value, dump_string_trunc};

/// The `builtin.jq` definitions with a native implementation, plus helper marks for
/// definitions whose constants other natives return.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum NativeId {
    /// `map/1` (not native; walk and with_entries return its `[]`).
    Map,
    /// The bytecoded `path/1` (not native; natives call it).
    Path,
    Modify,
    Assign,
    Join,
    ToEntries,
    FromEntries,
    WithEntries,
    Walk,
    Paths0,
    Paths1,
    Tostream,
    AsciiDowncase,
    AsciiUpcase,
    Recurse0,
    Values,
    Nulls,
    Booleans,
    Numbers,
    Strings,
    Arrays,
    Objects,
    Iterables,
    Scalars,
    Add0,
    Add1,
    Flatten,
    First1,
    Limit,
    IsEmpty,
    Any2,
    All2,
    Any1,
    All1,
    Any0,
    All0,
    In1,
    In2,
}

/// Every id, by `NativeId as usize`.
const ALL: [NativeId; NativeId::COUNT] = [
    NativeId::Map,
    NativeId::Path,
    NativeId::Modify,
    NativeId::Assign,
    NativeId::Join,
    NativeId::ToEntries,
    NativeId::FromEntries,
    NativeId::WithEntries,
    NativeId::Walk,
    NativeId::Paths0,
    NativeId::Paths1,
    NativeId::Tostream,
    NativeId::AsciiDowncase,
    NativeId::AsciiUpcase,
    NativeId::Recurse0,
    NativeId::Values,
    NativeId::Nulls,
    NativeId::Booleans,
    NativeId::Numbers,
    NativeId::Strings,
    NativeId::Arrays,
    NativeId::Objects,
    NativeId::Iterables,
    NativeId::Scalars,
    NativeId::Add0,
    NativeId::Add1,
    NativeId::Flatten,
    NativeId::First1,
    NativeId::Limit,
    NativeId::IsEmpty,
    NativeId::Any2,
    NativeId::All2,
    NativeId::Any1,
    NativeId::All1,
    NativeId::Any0,
    NativeId::All0,
    NativeId::In1,
    NativeId::In2,
];

impl NativeId {
    pub const COUNT: usize = 38;

    /// The native for `builtin.jq`'s definition `name/arity`, if any.
    pub fn lookup(name: &str, arity: i32) -> Option<NativeId> {
        use NativeId::*;
        Some(match (name, arity) {
            ("map", 1) => Map,
            ("_modify", 2) => Modify,
            ("_assign", 2) => Assign,
            ("join", 1) => Join,
            ("to_entries", 0) => ToEntries,
            ("from_entries", 0) => FromEntries,
            ("with_entries", 1) => WithEntries,
            ("walk", 1) => Walk,
            ("paths", 0) => Paths0,
            ("paths", 1) => Paths1,
            ("tostream", 0) => Tostream,
            ("ascii_downcase", 0) => AsciiDowncase,
            ("ascii_upcase", 0) => AsciiUpcase,
            ("recurse", 0) => Recurse0,
            ("values", 0) => Values,
            ("nulls", 0) => Nulls,
            ("booleans", 0) => Booleans,
            ("numbers", 0) => Numbers,
            ("strings", 0) => Strings,
            ("arrays", 0) => Arrays,
            ("objects", 0) => Objects,
            ("iterables", 0) => Iterables,
            ("scalars", 0) => Scalars,
            ("add", 0) => Add0,
            ("add", 1) => Add1,
            ("_flatten", 1) => Flatten,
            ("first", 1) => First1,
            ("limit", 2) => Limit,
            ("isempty", 1) => IsEmpty,
            ("any", 2) => Any2,
            ("all", 2) => All2,
            ("any", 1) => Any1,
            ("all", 1) => All1,
            ("any", 0) => Any0,
            ("all", 0) => All0,
            ("IN", 1) => In1,
            ("IN", 2) => In2,
            _ => return None,
        })
    }

    /// The mark stored in a function's bytecode (`id + 1`).
    pub fn mark(self) -> u16 {
        self as u16 + 1
    }

    /// The id of a bytecode mark (0 is none).
    pub fn from_mark(mark: u16) -> Option<NativeId> {
        ALL.get((mark as usize).checked_sub(1)?).copied()
    }

    /// Whether calls run a native implementation (else the mark only locates
    /// constants).
    pub fn dispatches(self) -> bool {
        !matches!(self, NativeId::Map | NativeId::Path)
    }

    /// Whether the native abandons a closure argument before it is exhausted, or keeps
    /// one suspended while it yields: jq does these with `break` and backtracking,
    /// which `?//` can catch (so such natives don't run in programs using `?//`).
    pub fn abandons_closures(self) -> bool {
        use NativeId::*;
        match self {
            Walk | Paths1 | Modify | Assign | Add1 | First1 | Limit | IsEmpty | Any2 | All2
            | Any1 | All1 | In1 | In2 => true,
            Map | Path | Join | ToEntries | FromEntries | WithEntries | Paths0 | Tostream
            | AsciiDowncase | AsciiUpcase | Recurse0 | Values | Nulls | Booleans | Numbers
            | Strings | Arrays | Objects | Iterables | Scalars | Add0 | Flatten | Any0 | All0 => {
                false
            }
        }
    }
}

/// The constants native `id` needs from the program (see the module docs), or `None`
/// if they can't be found, in which case the definition's bytecode runs.
pub(crate) fn consts(id: NativeId, pools: &Pools<'_>) -> Option<Vec<ConstRef>> {
    use NativeId::*;
    match id {
        Map => Some(vec![empty_array(&pools.own(), 0)?]),
        Path => Some(Vec::new()),
        Modify | Assign => Some(vec![pools.func(Path)?]),
        Join => Some(vec![
            string(&pools.own(), 0, "")?,
            string(&pools.own(), 1, "")?,
            string(&pools.own(), 2, "")?,
        ]),
        ToEntries => entries::to_entries_consts(&pools.own()),
        FromEntries => entries::from_entries_consts(&pools.own()),
        WithEntries => {
            let mut c = entries::to_entries_consts(&pools.native(ToEntries)?)?;
            c.push(empty_array(&pools.native(Map)?, 0)?);
            c.extend(entries::from_entries_consts(&pools.native(FromEntries)?)?);
            Some(c)
        }
        Walk => Some(vec![empty_array(&pools.native(Map)?, 0)?]),
        Tostream => Some(vec![
            empty_array(&pools.own(), 0)?,
            empty_array(&pools.own(), 1)?,
        ]),
        Flatten => Some(vec![
            empty_array(&pools.own(), 0)?,
            number(&pools.own(), 0.0)?,
            ConstRef::Own(Value::number(1.0)),
        ]),
        Limit => Some(vec![
            number(&pools.own(), 0.0)?,
            number(&pools.own(), 1.0)?,
            string(&pools.own(), 0, "limit doesn't support negative count")?,
        ]),
        Paths0 | Paths1 | AsciiDowncase | AsciiUpcase | Recurse0 | Values | Nulls | Booleans
        | Numbers | Strings | Arrays | Objects | Iterables | Scalars | Add0 | Add1 | First1
        | IsEmpty | Any2 | All2 | Any1 | All1 | Any0 | All0 | In1 | In2 => Some(Vec::new()),
    }
}

/// The `n`th `[]` of a pool (the definition's collects, in order).
fn empty_array(pool: &Pool<'_>, n: usize) -> Option<ConstRef> {
    pool.find(|v| matches!(v, Value::Array(a) if a.is_empty()), n)
}

/// The first number constant of a pool equal to `x` (a literal, as jq's lexer makes).
fn number(pool: &Pool<'_>, x: f64) -> Option<ConstRef> {
    pool.find(|v| matches!(v, Value::Number(n) if n.value() == x), 0)
}

/// The `n`th `{}` of a pool.
fn empty_object(pool: &Pool<'_>, n: usize) -> Option<ConstRef> {
    pool.find(|v| matches!(v, Value::Object(o) if o.is_empty()), n)
}

/// The `n`th string constant `s` of a pool.
fn string(pool: &Pool<'_>, n: usize, s: &str) -> Option<ConstRef> {
    pool.find(|v| v.as_str() == Some(s), n)
}

/// `.[]`'s error on a value that isn't iterable (`EACH`).
fn cannot_iterate(v: &Value) -> Stop {
    Error::msg(format!(
        "Cannot iterate over {} ({})",
        v.kind_name(),
        dump_string_trunc(v, 15)
    ))
    .into()
}

/// A container being iterated by `.[]?`: child `i` of `n` is being visited.
struct Level {
    node: Value,
    i: usize,
    n: usize,
}

/// How many children `.[]?` gives `v`.
fn children(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => 0,
    }
}

/// `.[]?`'s `i`th output on `v`: the path component (`EACH`'s key) and the child.
fn child(v: &Value, i: usize) -> (Value, Value) {
    match v {
        Value::Array(a) => (Value::number(i as f64), a.as_slice()[i].clone()),
        Value::Object(o) => {
            let (k, c) = o.get_index(i).expect("child");
            (Value::String(k.clone()), c.clone())
        }
        _ => unreachable!("child of a scalar"),
    }
}

/// A generator that has nothing more to produce, but holds values until backtracking
/// reaches it: where jq's definition returns its value with fork points still on the
/// stack, the values those keep alive stay shared until the caller backtracks (a
/// uniquely owned array would be written in place, which jq can observe).
struct Hold(#[allow(dead_code)] Value);

impl Resume for Hold {
    fn resume(self: Box<Self>, _vm: &mut Jq) -> Outcome {
        Outcome::Empty
    }
}

/// Runs native `id`.
pub(crate) fn call(id: NativeId, vm: &mut Jq, c: Call<'_>) -> Outcome {
    use NativeId::*;
    match id {
        Map | Path => Outcome::Fallback(c.input),
        Modify => {
            let path_fn = c.consts.func(0, &c.callee);
            modify::modify(vm, c.input, c.args[0], c.args[1], path_fn).into()
        }
        Assign => {
            let path_fn = c.consts.func(0, &c.callee);
            modify::assign(vm, c.input, c.args[0], c.args[1], path_fn)
        }
        Join => strings::join(vm, c.input, c.args[0], c.consts),
        ToEntries => entries::to_entries(c.input, c.consts).into(),
        FromEntries => entries::from_entries(vm, c.input, c.consts).into(),
        WithEntries => entries::with_entries(vm, c.input, c.args[0], c.consts).into(),
        Walk => walk::walk(vm, c.input, c.args[0], c.consts),
        Paths0 => paths::paths0(c.input),
        Paths1 => {
            let f = c.args[0];
            let test = match vm.tail_callee_mark(f).and_then(values::type_filter) {
                Some(keep) => Some(paths::NodeTest::Filter(keep)),
                None => vm.type_test_closure(f).map(paths::NodeTest::TypeIs),
            };
            paths::paths1(vm, c.input, f, test)
        }
        Tostream => paths::tostream(c.input, c.consts),
        AsciiDowncase => strings::ascii_case(c.input, b'A', b'Z').into(),
        AsciiUpcase => strings::ascii_case(c.input, b'a', b'z').into(),
        Recurse0 => values::recurse(c.input),
        Values => values::select_if(c.input, values::is_value),
        Nulls => values::select_if(c.input, values::is_null),
        Booleans => values::select_if(c.input, values::is_boolean),
        Numbers => values::select_if(c.input, values::is_number),
        Strings => values::select_if(c.input, values::is_string),
        Arrays => values::select_if(c.input, values::is_array),
        Objects => values::select_if(c.input, values::is_object),
        Iterables => values::select_if(c.input, values::is_iterable),
        Scalars => values::select_if(c.input, values::is_scalar),
        Add0 => values::add0(c.input).into(),
        Add1 => values::add1(vm, c.input, c.args[0]).into(),
        Flatten => values::flatten(vm, c.input, c.args[0], c.consts),
        First1 => control::first(vm, c.input, c.args[0]),
        Limit => control::limit(vm, c.input, c.args[0], c.args[1], c.consts),
        IsEmpty => control::isempty(vm, c.input, c.args[0]),
        Any2 => control::any_all(vm, c.input, Some(c.args[0]), Some(c.args[1]), true),
        All2 => control::any_all(vm, c.input, Some(c.args[0]), Some(c.args[1]), false),
        Any1 => control::any_all(vm, c.input, None, Some(c.args[0]), true),
        All1 => control::any_all(vm, c.input, None, Some(c.args[0]), false),
        Any0 => control::any_all(vm, c.input, None, None, true),
        All0 => control::any_all(vm, c.input, None, None, false),
        In1 => control::is_in(vm, c.input, None, c.args[0]),
        In2 => control::is_in(vm, c.input, Some(c.args[0]), c.args[1]),
    }
}
