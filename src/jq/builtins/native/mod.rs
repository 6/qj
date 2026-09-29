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

mod entries;
mod paths;
mod strings;
mod walk;

#[cfg(test)]
mod cases;
#[cfg(test)]
mod tests;

use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Call, ConstRef, Outcome, Pool, Pools};
use crate::jq::value::Value;

/// The `builtin.jq` definitions with a native implementation, plus helper marks for
/// definitions whose constants other natives return.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum NativeId {
    /// `map/1` (not native; walk and with_entries return its `[]`).
    Map,
    ToEntries,
    FromEntries,
    WithEntries,
    Walk,
    Paths0,
    Paths1,
    Tostream,
    AsciiDowncase,
    AsciiUpcase,
}

/// Every id, by `NativeId as usize`.
const ALL: [NativeId; NativeId::COUNT] = [
    NativeId::Map,
    NativeId::ToEntries,
    NativeId::FromEntries,
    NativeId::WithEntries,
    NativeId::Walk,
    NativeId::Paths0,
    NativeId::Paths1,
    NativeId::Tostream,
    NativeId::AsciiDowncase,
    NativeId::AsciiUpcase,
];

impl NativeId {
    pub const COUNT: usize = 10;

    /// The native for `builtin.jq`'s definition `name/arity`, if any.
    pub fn lookup(name: &str, arity: i32) -> Option<NativeId> {
        use NativeId::*;
        Some(match (name, arity) {
            ("map", 1) => Map,
            ("to_entries", 0) => ToEntries,
            ("from_entries", 0) => FromEntries,
            ("with_entries", 1) => WithEntries,
            ("walk", 1) => Walk,
            ("paths", 0) => Paths0,
            ("paths", 1) => Paths1,
            ("tostream", 0) => Tostream,
            ("ascii_downcase", 0) => AsciiDowncase,
            ("ascii_upcase", 0) => AsciiUpcase,
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
        !matches!(self, NativeId::Map)
    }

    /// Whether the native abandons a closure argument before it is exhausted, or keeps
    /// one suspended while it yields: jq does these with `break` and backtracking,
    /// which `?//` can catch (so such natives don't run in programs using `?//`).
    pub fn abandons_closures(self) -> bool {
        use NativeId::*;
        match self {
            Walk | Paths1 => true,
            Map | ToEntries | FromEntries | WithEntries | Paths0 | Tostream | AsciiDowncase
            | AsciiUpcase => false,
        }
    }
}

/// The constants native `id` needs from the program (see the module docs), or `None`
/// if they can't be found, in which case the definition's bytecode runs.
pub(crate) fn consts(id: NativeId, pools: &Pools<'_>) -> Option<Vec<ConstRef>> {
    use NativeId::*;
    match id {
        Map => Some(vec![empty_array(&pools.own(), 0)?]),
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
        Paths0 | Paths1 | AsciiDowncase | AsciiUpcase => Some(Vec::new()),
    }
}

/// The `n`th `[]` of a pool (the definition's collects, in order).
fn empty_array(pool: &Pool<'_>, n: usize) -> Option<ConstRef> {
    pool.find(|v| matches!(v, Value::Array(a) if a.is_empty()), n)
}

/// The `n`th `{}` of a pool.
fn empty_object(pool: &Pool<'_>, n: usize) -> Option<ConstRef> {
    pool.find(|v| matches!(v, Value::Object(o) if o.is_empty()), n)
}

/// The string constant `s` of a pool.
fn string(pool: &Pool<'_>, s: &str) -> Option<ConstRef> {
    pool.find(|v| v.as_str() == Some(s), 0)
}

/// Runs native `id`.
pub(crate) fn call(id: NativeId, vm: &mut Jq, c: Call<'_>) -> Outcome {
    use NativeId::*;
    match id {
        Map => Outcome::Fallback(c.input),
        ToEntries => entries::to_entries(c.input, c.consts).into(),
        FromEntries => entries::from_entries(vm, c.input, c.consts).into(),
        WithEntries => entries::with_entries(vm, c.input, c.args[0], c.consts).into(),
        Walk => walk::walk(vm, c.input, c.args[0], c.consts),
        Paths0 => paths::paths0(c.input),
        Paths1 => paths::paths1(vm, c.input, c.args[0]),
        Tostream => paths::tostream(c.input, c.consts),
        AsciiDowncase => strings::ascii_case(c.input, b'A', b'Z').into(),
        AsciiUpcase => strings::ascii_case(c.input, b'a', b'z').into(),
    }
}
