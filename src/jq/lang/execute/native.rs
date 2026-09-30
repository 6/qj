//! Native implementations of jq-defined builtins: the VM side.
//!
//! Some `builtin.jq` definitions (`walk`, `paths`, `to_entries`, ...) are marked when they
//! are bound (`builtins::bind`), and the mark survives into their compiled function
//! ([`Bytecode::native`](crate::jq::lang::bytecode::Bytecode::native)). When `CALL_JQ`
//! calls a marked function and [`Jq::natives_ok`] holds, the VM runs the native
//! implementation (`builtins::native`) instead of the bytecode body. Otherwise the body
//! runs, so the bytecode stays the reference and every native must be observably
//! identical to it (outputs and their order, laziness, errors, labels, and even the
//! storage and identity of the values it returns, which jq exposes through array
//! views and path expressions).
//!
//! # Running closure arguments: sub-runs
//!
//! A native evaluates a closure argument with a *sub-run* on the same stack:
//! [`Jq::sub_start`] saves a base fork point (resuming at
//! [`Program::subrun_base_pc`]), pushes a frame for the closure that returns to
//! [`Program::subrun_ret_pc`], and runs the interpreter loop re-entrantly until the
//! closure returns a value (the run is then suspended, its fork points left on the
//! stack) or backtracking reaches the base (the closure is exhausted, or raised an
//! error). [`Jq::sub_next`] backtracks into a suspended run for its next output, and
//! [`Jq::sub_abandon`] ends one early the way jq's `break` does: an error unwinds its
//! fork points (running their backtracking handlers) down to the base. Only `?//`
//! (`DESTRUCTURE_ALT`) could catch that error and make the closure produce more values
//! (a jq quirk: `[1] | first(. as [$x] ?// $x | $x)` outputs `1` and `[1]`), so natives
//! that abandon or suspend closures are disabled in programs that use `?//`.
//!
//! # Producing several outputs: suspended generators
//!
//! A native that yields a value and has more to produce returns
//! [`Outcome::Yield`]: the VM pushes its state ([`Suspended`]) on the data stack with a
//! fork point above it (resuming at [`Program::native_resume_pc`]), like `RANGE`
//! keeps its bound. When backtracking reaches it, [`Resume::resume`] produces the next
//! outcome; when an error propagates through it, [`Resume::unwind`] unwinds the
//! sub-runs it keeps suspended, as jq's backtracking through the definition's fork
//! points would.
//!
//! A native can also end with a tail call ([`Outcome::Call`]): the closure then runs on
//! the VM exactly where the definition would call it, so its outputs, errors and
//! backtracking need no emulation (`walk(f)` ends with `| f`).

use std::sync::OnceLock;

use super::Jq;
pub(crate) use super::Raised;
use super::program::Program;
pub(crate) use super::stack::Closure;
use super::stack::StackPtr;
use crate::jq::builtins::native::{self, NativeId};
use crate::jq::lang::bytecode::{ARG_NEWCLOSURE, Opcode};
use crate::jq::value::{Error, Value};

/// How deeply natives may nest (each level is a Rust call of the interpreter loop);
/// deeper calls run the bytecode, which uses no native stack.
const MAX_NATIVE_DEPTH: u32 = 64;

/// The most closure arguments a native builtin takes.
pub(crate) const MAX_NATIVE_ARGS: usize = 4;

/// Whether natives are disabled for the whole process: by `QJ_NO_NATIVE=1` (for A/B
/// checks against the bytecode definitions), and in compat mode (`QJ_JQ_COMPAT`), which
/// reproduces the crashes of jq's recursive frees at jq's nesting depths, so values
/// must be freed where jq's definitions free them.
pub(super) fn disabled_by_env() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| {
        std::env::var_os("QJ_NO_NATIVE").is_some_and(|v| v == "1") || crate::compat::exactly_jq()
    })
}

#[cfg(test)]
thread_local! {
    /// Native calls on this thread (tests check that natives really run).
    pub(crate) static CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What a native produces (initially, or when resumed).
pub(crate) enum Outcome {
    /// One output, and nothing more: return it to the caller.
    Value(Value),
    /// No (more) outputs: backtrack.
    Empty,
    /// Raise an error: backtrack with it.
    Raise(Raised),
    /// Return an output and keep the generator for more (see [`Resume`]).
    Yield(Value, Box<dyn Resume>),
    /// Tail-call a closure on a value: its outputs are the native's remaining outputs.
    Call(Closure, Value),
    /// The program halted (in a closure the native ran).
    Halted,
    /// Run the bytecode definition instead, on this input. Only before the native has
    /// done anything observable.
    Fallback(Value),
}

impl From<Stop> for Outcome {
    fn from(s: Stop) -> Outcome {
        match s {
            Stop::Raise(r) => Outcome::Raise(r),
            Stop::Halted => Outcome::Halted,
        }
    }
}

impl From<Result<Value, Stop>> for Outcome {
    fn from(r: Result<Value, Stop>) -> Outcome {
        match r {
            Ok(v) => Outcome::Value(v),
            Err(s) => s.into(),
        }
    }
}

/// Why a native stops early: an error it raises or that a closure raised, or a halt.
pub(crate) enum Stop {
    Raise(Raised),
    Halted,
}

impl From<Error> for Stop {
    fn from(e: Error) -> Stop {
        Stop::Raise(Raised::new(e.into_value()))
    }
}

/// A native generator waiting for backtracking (see the module docs).
pub(crate) trait Resume {
    /// Backtracking (without an error) reached the generator: its next outcome.
    fn resume(self: Box<Self>, vm: &mut Jq) -> Outcome;
    /// An error is propagating through the generator: unwind the sub-runs it keeps
    /// suspended ([`Jq::sub_unwind`]), leaving the error set.
    fn unwind(self: Box<Self>, vm: &mut Jq) {
        let _ = vm;
    }
}

/// A suspended native generator on the stack, and where its outputs go.
pub(crate) struct Suspended {
    pub retaddr: u32,
    pub generator: Box<dyn Resume>,
}

/// A suspended sub-run (a closure that returned a value and may have more). It must be
/// resumed ([`Jq::sub_next`]) or ended ([`Jq::sub_abandon`], [`Jq::sub_unwind`]) before
/// the native that started it returns, unless the native yields with it in its state.
#[must_use]
pub(crate) struct Sub {
    base: StackPtr,
}

/// What the interpreter loop stopped on.
pub(super) enum Exit {
    /// The top-level program produced a value.
    Yield(Value),
    /// No fork points are left (the program is done, or failed with `jq->error`).
    Done,
    /// A sub-run's closure returned a value.
    SubRet(Value),
    /// Backtracking reached the sub-run's base fork point.
    Base,
    /// The program halted.
    Halted,
}

/// What the VM does after a native call or resume.
pub(super) enum Applied {
    /// Continue at this pc.
    Continue(usize),
    /// Backtrack (with `jq->error` as it is).
    Backtrack,
    /// The program halted: the loop returns.
    Halted,
    /// Call the bytecode definition on this input.
    Fallback(Value),
}

/// Constants a native returns where jq's definition returns a constant of its
/// bytecode (identity is observable: two copies of the same constant are
/// `jv_identical`, so `path(...)` accepts one for the other), resolved per program.
///
/// They are kept as references into the constant pools, not copies: `--debug-trace`
/// prints constants' reference counts.
#[derive(Default)]
pub(crate) struct Consts {
    /// By function id: the constants its native needs, when it is marked.
    by_func: Vec<Vec<ConstRef>>,
}

/// A constant of a native: in a function's constant pool, or owned (for values that
/// are only compared, like the keys `from_entries` looks up).
pub(crate) enum ConstRef {
    Pool {
        func: u32,
        idx: u32,
    },
    Own(Value),
    /// A function (a builtin the native calls, closed over the native's own
    /// environment: builtins are all defined in the same scope).
    Func(u32),
}

impl Consts {
    /// Resolves the constants of every marked function, unmarking those whose
    /// constants can't be found (their bytecode then runs).
    pub(super) fn resolve(prog: &mut Program) {
        let mut by_func: Vec<Vec<ConstRef>> = (0..prog.funcs.len()).map(|_| Vec::new()).collect();
        // The function of each marked definition, for natives that use another one's
        // constants.
        let mut of_native: Vec<Option<u32>> = vec![None; NativeId::COUNT];
        for (i, f) in prog.funcs.iter().enumerate() {
            if let Some(id) = f.mark {
                of_native[id as usize].get_or_insert(i as u32);
            }
        }
        let mut native = vec![None; prog.funcs.len()];
        for (i, f) in prog.funcs.iter().enumerate() {
            let Some(id) = f.mark.filter(|id| id.dispatches()) else {
                continue;
            };
            let pools = Pools {
                prog,
                func: i as u32,
                of_native: &of_native,
            };
            if let Some(c) = native::consts(id, &pools) {
                by_func[i] = c;
                native[i] = Some(id);
            }
        }
        for (f, n) in prog.funcs.iter_mut().zip(native) {
            f.native = n;
        }
        prog.native_consts = Consts { by_func };
    }
}

/// The constant pools a native's constants come from.
pub(crate) struct Pools<'a> {
    prog: &'a Program,
    func: u32,
    of_native: &'a [Option<u32>],
}

/// One function's constant pool.
pub(crate) struct Pool<'a> {
    func: u32,
    values: &'a [Value],
}

impl<'a> Pools<'a> {
    /// The native function's own constants.
    pub fn own(&self) -> Pool<'a> {
        Pool {
            func: self.func,
            values: self.prog.constants(self.func),
        }
    }

    /// The function of another marked definition, if the program has it.
    pub fn func(&self, id: NativeId) -> Option<ConstRef> {
        Some(ConstRef::Func(self.of_native[id as usize]?))
    }

    /// The constants of another marked definition's function, if the program has it.
    pub fn native(&self, id: NativeId) -> Option<Pool<'a>> {
        let func = self.of_native[id as usize]?;
        Some(Pool {
            func,
            values: self.prog.constants(func),
        })
    }
}

impl Pool<'_> {
    /// The `n`th constant satisfying `pred`.
    pub fn find(&self, pred: impl Fn(&Value) -> bool, n: usize) -> Option<ConstRef> {
        let idx = self
            .values
            .iter()
            .enumerate()
            .filter(|(_, v)| pred(v))
            .nth(n)?
            .0;
        Some(ConstRef::Pool {
            func: self.func,
            idx: idx as u32,
        })
    }
}

/// A native's constants during a call (see [`Consts`]).
#[derive(Clone, Copy)]
pub(crate) struct ConstView<'a> {
    prog: &'a Program,
    refs: &'a [ConstRef],
}

impl<'a> ConstView<'a> {
    /// Constant `i`.
    pub fn get(&self, i: usize) -> &'a Value {
        match &self.refs[i] {
            ConstRef::Pool { func, idx } => &self.prog.constants(*func)[*idx as usize],
            ConstRef::Own(v) => v,
            ConstRef::Func(_) => unreachable!("a function, not a constant"),
        }
    }

    /// Function `i`, as a closure over `env` (the native's own environment).
    pub fn func(&self, i: usize, env: &Closure) -> Closure {
        match &self.refs[i] {
            ConstRef::Func(func) => Closure {
                func: *func,
                env: env.env,
            },
            _ => unreachable!("a constant, not a function"),
        }
    }

    /// The constants from `i` on.
    pub fn from(&self, i: usize) -> ConstView<'a> {
        ConstView {
            prog: self.prog,
            refs: &self.refs[i..],
        }
    }
}

/// A native call: the input, closure arguments and constants, and the closure being
/// called (its definition's).
pub(crate) struct Call<'a> {
    pub input: Value,
    pub args: &'a [Closure],
    pub consts: ConstView<'a>,
    pub callee: Closure,
}

impl Raised {
    /// A new error with message `msg` (`jv_invalid_with_msg`).
    pub(crate) fn new(msg: Value) -> Raised {
        Raised { msg, wraps: 0 }
    }
}

impl Jq {
    /// Enables or disables natives for this interpreter (they are on by default, unless
    /// `QJ_NO_NATIVE=1`).
    pub fn set_natives(&mut self, on: bool) {
        self.natives = on;
    }

    /// Whether the native implementation of `id` may run now: natives are enabled, no
    /// trace is printed, no path is being tracked, the nesting limit isn't reached, and
    /// the program has no `?//` if the native abandons or suspends closures.
    #[inline]
    pub(super) fn natives_ok(&self, prog: &Program, id: NativeId) -> bool {
        self.natives
            && self.debug_trace == 0
            && (self.subexp_nest > 0 || !matches!(self.path, Value::Array(_)))
            && self.native_depth < MAX_NATIVE_DEPTH
            && !(prog.has_destructure_alt && id.abandons_closures())
    }

    /// Runs native `id` for a call of function `func` (the caller has popped the input
    /// and, for a tail call, the calling frame). `retaddr` is where outputs go.
    pub(super) fn call_native(
        &mut self,
        prog: &Program,
        id: NativeId,
        callee: Closure,
        input: Value,
        args: &[Closure],
        retaddr: u32,
    ) -> Applied {
        #[cfg(debug_assertions)]
        let fork_top = self.fork_top;
        #[cfg(test)]
        CALLS.with(|c| c.set(c.get() + 1));
        self.native_depth += 1;
        let out = native::call(
            id,
            self,
            Call {
                input,
                args,
                consts: ConstView {
                    prog,
                    refs: &prog.native_consts.by_func[callee.func as usize],
                },
                callee,
            },
        );
        self.native_depth -= 1;
        #[cfg(debug_assertions)]
        if !matches!(out, Outcome::Yield(..) | Outcome::Halted) {
            debug_assert_eq!(self.fork_top, fork_top, "{id:?} left fork points");
        }
        self.apply(prog, out, retaddr)
    }

    /// `ON_BACKTRACK` of a native generator's fork point: its state is on top of the
    /// restored data stack.
    pub(super) fn resume_native(&mut self, prog: &Program, raising: bool) -> Applied {
        let (s, top) = self.stk.pop_native(self.stk_top);
        self.stk_top = top;
        let Suspended { retaddr, generator } = *s;
        if raising {
            generator.unwind(self);
            debug_assert!(self.error.is_some());
            return Applied::Backtrack;
        }
        self.native_depth += 1;
        let out = generator.resume(self);
        self.native_depth -= 1;
        if let Outcome::Fallback(_) = out {
            unreachable!("a resumed native can't fall back");
        }
        self.apply(prog, out, retaddr)
    }

    fn apply(&mut self, prog: &Program, out: Outcome, retaddr: u32) -> Applied {
        match out {
            Outcome::Value(v) => {
                self.push(v);
                Applied::Continue(retaddr as usize)
            }
            Outcome::Empty => {
                debug_assert!(self.error.is_none());
                Applied::Backtrack
            }
            Outcome::Raise(r) => {
                self.error = Some(r);
                Applied::Backtrack
            }
            Outcome::Yield(v, generator) => {
                let spos = (self.stk_top, self.curr_frame);
                let s = Box::new(Suspended { retaddr, generator });
                self.stk_top = self.stk.push_native(self.stk_top, s);
                self.stack_save(prog.native_resume_pc as usize, spos);
                self.push(v);
                Applied::Continue(retaddr as usize)
            }
            Outcome::Call(cl, v) => {
                let retdata = self.stk_top;
                self.frame_push(prog, &[], cl, 0, 0);
                let f = self.stk.frame_mut(self.curr_frame);
                f.retdata = retdata;
                f.retaddr = retaddr;
                self.push(v);
                Applied::Continue(prog.funcs[cl.func as usize].base as usize)
            }
            Outcome::Halted => Applied::Halted,
            Outcome::Fallback(v) => Applied::Fallback(v),
        }
    }

    // ---- sub-runs -------------------------------------------------------------------

    /// Starts running closure `f` on `input`. Returns its first output and the suspended
    /// run, `None` if it produced nothing, or why it stopped.
    pub(crate) fn sub_start(
        &mut self,
        f: Closure,
        input: Value,
    ) -> Result<Option<(Sub, Value)>, Stop> {
        self.sub_start_args(f, &[], input)
    }

    /// [`Jq::sub_start`] for a function with closure parameters, given `args`.
    pub(crate) fn sub_start_args(
        &mut self,
        f: Closure,
        args: &[Closure],
        input: Value,
    ) -> Result<Option<(Sub, Value)>, Stop> {
        let prog = self.prog.clone();
        let pos = (self.stk_top, self.curr_frame);
        self.stack_save(prog.subrun_base_pc as usize, pos);
        let base = self.fork_top;
        let retdata = self.stk_top;
        self.frame_push_args(&prog, f, args);
        let fr = self.stk.frame_mut(self.curr_frame);
        fr.retdata = retdata;
        fr.retaddr = prog.subrun_ret_pc;
        self.push(input);
        let pc = prog.funcs[f.func as usize].base as usize;
        self.sub_continue(&prog, pc, false, base)
    }

    /// The next output of a suspended sub-run.
    pub(crate) fn sub_next(&mut self, s: Sub) -> Result<Option<(Sub, Value)>, Stop> {
        debug_assert!(self.error.is_none());
        if self.sub_idle(&s) {
            // Only the base is left: restoring it is all backtracking would do.
            self.pop_base(&s);
            return Ok(None);
        }
        let prog = self.prog.clone();
        let pc = self.stack_restore().expect("sub-run base fork point");
        self.sub_continue(&prog, pc, true, s.base)
    }

    /// Restores (pops) the base fork point of an idle sub-run, as `BT_SUBRUN_BASE` would.
    #[inline]
    fn pop_base(&mut self, s: &Sub) {
        debug_assert!(self.sub_idle(s));
        let pc = self.stack_restore().expect("sub-run base fork point");
        debug_assert_eq!(pc, self.prog.subrun_base_pc as usize);
        debug_assert_eq!(self.last_fork, s.base);
    }

    fn sub_continue(
        &mut self,
        prog: &Program,
        pc: usize,
        backtracking: bool,
        base: StackPtr,
    ) -> Result<Option<(Sub, Value)>, Stop> {
        match self.run::<false>(prog, pc, backtracking, base) {
            Exit::SubRet(v) => Ok(Some((Sub { base }, v))),
            Exit::Base => match self.error.take() {
                Some(e) => Err(Stop::Raise(e)),
                None => Ok(None),
            },
            Exit::Halted => Err(Stop::Halted),
            Exit::Yield(_) | Exit::Done => unreachable!("sub-run left its base"),
        }
    }

    /// Whether a suspended sub-run has no fork points left above its base: it can't
    /// produce more, and (in jq) nothing of it holds values alive any more. Finish it
    /// with [`Jq::sub_finish`].
    pub(crate) fn sub_idle(&self, s: &Sub) -> bool {
        self.fork_top == s.base
    }

    /// Ends a sub-run that is [`Jq::sub_idle`] (pops its base fork point).
    pub(crate) fn sub_finish(&mut self, s: Sub) {
        self.pop_base(&s);
    }

    /// Ends a suspended sub-run the way jq's `break` does: an error unwinds its fork
    /// points (their handlers restore paths and propagate it) down to the base.
    pub(crate) fn sub_abandon(&mut self, s: Sub) {
        debug_assert!(self.error.is_none());
        if self.sub_idle(&s) {
            self.pop_base(&s);
            return;
        }
        self.error = Some(Raised::new(Value::Null));
        self.sub_unwind(s);
        self.error = None;
    }

    /// Unwinds a suspended sub-run with the error being raised (`jq->error` is set), as
    /// when that error propagates through the fork points of jq's definition.
    pub(crate) fn sub_unwind(&mut self, s: Sub) {
        debug_assert!(self.error.is_some());
        if self.sub_idle(&s) {
            self.pop_base(&s);
            return;
        }
        let prog = self.prog.clone();
        let pc = self.stack_restore().expect("sub-run base fork point");
        match self.run::<false>(&prog, pc, true, s.base) {
            Exit::Base => {}
            Exit::Halted => {}
            _ => unreachable!("an unwinding sub-run produced a value (?// catches breaks)"),
        }
    }

    /// Runs `f` on `input` to completion, calling `each` on every output in order. When
    /// `each` fails, the run is abandoned and its error returned.
    pub(crate) fn sub_each(
        &mut self,
        f: Closure,
        input: Value,
        mut each: impl FnMut(&mut Jq, Value) -> Result<(), Stop>,
    ) -> Result<(), Stop> {
        let mut r = self.sub_start(f, input)?;
        while let Some((s, v)) = r {
            if let Err(e) = each(self, v) {
                self.sub_abandon(s);
                return Err(e);
            }
            r = self.sub_next(s)?;
        }
        Ok(())
    }

    /// The first output of `f` on `input` (the rest of the run is abandoned).
    pub(crate) fn sub_first(&mut self, f: Closure, input: Value) -> Result<Option<Value>, Stop> {
        match self.sub_start(f, input)? {
            Some((s, v)) => {
                self.sub_abandon(s);
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// The value of closure `f` on `input` when running it can't do anything else:
    /// `f` is `.`, a constant (`LOADK k`) or a variable of an enclosing function
    /// (`LOADV`). What jq's `$param` bindings evaluate without side effects.
    pub(crate) fn pure_arg(&self, f: Closure, input: &Value) -> Option<Value> {
        let prog = &*self.prog;
        let func = &prog.funcs[f.func as usize];
        let code = &func.bc.code;
        const RET: u16 = Opcode::RET as u16;
        match code.as_slice() {
            [RET] => Some(input.clone()),
            [op, k, RET] if *op == Opcode::LOADK as u16 => {
                func.bc.constants.get(*k as usize).cloned()
            }
            // `-k` (jq doesn't fold unary minus): `LOADK k; CALL_BUILTIN _negate`.
            [op, k, call, 1, cf, RET]
                if *op == Opcode::LOADK as u16
                    && *call == Opcode::CALL_BUILTIN as u16
                    && prog
                        .cfunctions
                        .get(*cf as usize)
                        .is_some_and(|c| c.name == "_negate") =>
            {
                match func.bc.constants.get(*k as usize)? {
                    Value::Number(n) => Some(Value::Number(n.negate())),
                    _ => None,
                }
            }
            [op, level, var, RET] if *op == Opcode::LOADV as u16 && *level > 0 => {
                // The variable's frame: `level` env links up from the closure's frame,
                // whose env is `f.env`.
                let mut fr = f.env;
                for _ in 1..*level {
                    fr = self.stk.frame(fr).env;
                }
                let fr = self.stk.frame(fr);
                self.stk
                    .locals
                    .get(fr.locals as usize + *var as usize)
                    .cloned()
            }
            _ => None,
        }
    }

    /// The key `k` if closure `f` is `.[k]` for a constant `k` (`.a`, `."a"`, `.[0]`:
    /// `PUSHK_UNDER k; INDEX`).
    pub(crate) fn key_closure(&self, f: Closure) -> Option<Value> {
        let func = &self.prog.funcs[f.func as usize];
        match func.bc.code.as_slice() {
            [push, k, index, ret]
                if *push == Opcode::PUSHK_UNDER as u16
                    && *index == Opcode::INDEX as u16
                    && *ret == Opcode::RET as u16 =>
            {
                func.bc.constants.get(*k as usize).cloned()
            }
            _ => None,
        }
    }

    /// The marked function a call of closure `f` runs, if `f` only calls it without
    /// arguments (`TAIL_CALL_JQ g; RET`: how `paths(scalars)` passes `scalars`),
    /// perhaps through more such closures (`def f(g): def h: paths(g); ...` passes a
    /// closure that calls `g`). Each callee is resolved as `make_closure` would in its
    /// caller's frame.
    pub(crate) fn tail_callee_mark(&self, f: Closure) -> Option<NativeId> {
        let prog = &*self.prog;
        let mut f = f;
        // Bounded: a function can call itself.
        for _ in 0..8 {
            let func = &prog.funcs[f.func as usize];
            if func.mark.is_some() {
                return func.mark;
            }
            let &[op, 0, level, idx, ret] = func.bc.code.as_slice() else {
                return None;
            };
            // Level 0 is `f`'s own frame, which doesn't exist before the call.
            if op != Opcode::TAIL_CALL_JQ as u16 || ret != Opcode::RET as u16 || level == 0 {
                return None;
            }
            // `frame_get_level(level)` from `f`'s frame, whose env is `f.env`.
            let mut env = f.env;
            for _ in 1..level {
                env = self.stk.frame(env).env;
            }
            let fr = self.stk.frame(env);
            f = if idx & ARG_NEWCLOSURE != 0 {
                let sub = prog.funcs[fr.func as usize]
                    .subfunctions
                    .get((idx & !ARG_NEWCLOSURE) as usize)?;
                Closure { func: *sub, env }
            } else {
                *self.stk.closures.get(fr.closures as usize + idx as usize)?
            };
        }
        None
    }

    /// Whether closure `f` is `.[]` (`Some(false)`) or `.[]?` (`Some(true)`).
    pub(crate) fn each_closure(&self, f: Closure) -> Option<bool> {
        let code = &self.prog.funcs[f.func as usize].bc.code;
        match code.as_slice() {
            [op, ret] if *ret == Opcode::RET as u16 => {
                if *op == Opcode::EACH as u16 {
                    Some(false)
                } else if *op == Opcode::EACH_OPT as u16 {
                    Some(true)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// `GENLABEL`'s counter, advanced by `n` labels (the natives' definitions allocate
    /// labels, whose numbers `break` values expose).
    pub(crate) fn gen_labels(&mut self, n: u32) {
        self.next_label = self.next_label.wrapping_add(n);
    }

    /// Swaps in the path-expression registers (`jq->path`, `jq->value_at_path`,
    /// `jq->subexp_nest`), returning the previous ones: for natives that run closures
    /// inside a path expression of their definition (`paths(f)`).
    pub(crate) fn swap_path_state(&mut self, state: PathState) -> PathState {
        PathState {
            path: std::mem::replace(&mut self.path, state.path),
            value_at_path: std::mem::replace(&mut self.value_at_path, state.value_at_path),
            subexp_nest: std::mem::replace(&mut self.subexp_nest, state.subexp_nest),
        }
    }
}

/// The path-expression registers (see [`Jq::swap_path_state`]).
pub(crate) struct PathState {
    pub path: Value,
    pub value_at_path: Value,
    pub subexp_nest: i32,
}
