//! The interpreter loop: port of execute.c's `jq_next` and the stack, frame, fork-point
//! and path helpers it uses.

use super::native::{Applied, Exit};
use super::program::{Program, pseudo};
use super::region::REGION_BASE;
use super::stack::{Closure, ForkPoint, Frame, NO_RETADDR, StackPtr};
use super::{Jq, Raised, label_object};
use crate::jq::builtins::CResult;
use crate::jq::lang::bytecode::{ARG_NEWCLOSURE, NUM_OPCODES, Opcode};
use crate::jq::value::{Error, Value, dump_string_trunc};

/// Opcode numbers as `u16` match patterns; `BT_*` is `ON_BACKTRACK(op)`.
mod op {
    use super::{NUM_OPCODES, Opcode};

    const BT: u16 = NUM_OPCODES as u16;

    pub const LOADK: u16 = Opcode::LOADK as u16;
    pub const DUP: u16 = Opcode::DUP as u16;
    pub const DUPN: u16 = Opcode::DUPN as u16;
    pub const DUP2: u16 = Opcode::DUP2 as u16;
    pub const PUSHK_UNDER: u16 = Opcode::PUSHK_UNDER as u16;
    pub const POP: u16 = Opcode::POP as u16;
    pub const LOADV: u16 = Opcode::LOADV as u16;
    pub const LOADVN: u16 = Opcode::LOADVN as u16;
    pub const STOREV: u16 = Opcode::STOREV as u16;
    pub const STORE_GLOBAL: u16 = Opcode::STORE_GLOBAL as u16;
    pub const INDEX: u16 = Opcode::INDEX as u16;
    pub const INDEX_OPT: u16 = Opcode::INDEX_OPT as u16;
    pub const EACH: u16 = Opcode::EACH as u16;
    pub const EACH_OPT: u16 = Opcode::EACH_OPT as u16;
    pub const FORK: u16 = Opcode::FORK as u16;
    pub const TRY_BEGIN: u16 = Opcode::TRY_BEGIN as u16;
    pub const TRY_END: u16 = Opcode::TRY_END as u16;
    pub const JUMP: u16 = Opcode::JUMP as u16;
    pub const JUMP_F: u16 = Opcode::JUMP_F as u16;
    pub const BACKTRACK: u16 = Opcode::BACKTRACK as u16;
    pub const APPEND: u16 = Opcode::APPEND as u16;
    pub const INSERT: u16 = Opcode::INSERT as u16;
    pub const RANGE: u16 = Opcode::RANGE as u16;
    pub const SUBEXP_BEGIN: u16 = Opcode::SUBEXP_BEGIN as u16;
    pub const SUBEXP_END: u16 = Opcode::SUBEXP_END as u16;
    pub const PATH_BEGIN: u16 = Opcode::PATH_BEGIN as u16;
    pub const PATH_END: u16 = Opcode::PATH_END as u16;
    pub const CALL_BUILTIN: u16 = Opcode::CALL_BUILTIN as u16;
    pub const CALL_JQ: u16 = Opcode::CALL_JQ as u16;
    pub const RET: u16 = Opcode::RET as u16;
    pub const TAIL_CALL_JQ: u16 = Opcode::TAIL_CALL_JQ as u16;
    pub const TOP: u16 = Opcode::TOP as u16;
    pub const GENLABEL: u16 = Opcode::GENLABEL as u16;
    pub const DESTRUCTURE_ALT: u16 = Opcode::DESTRUCTURE_ALT as u16;
    pub const STOREVN: u16 = Opcode::STOREVN as u16;
    pub const ERRORK: u16 = Opcode::ERRORK as u16;

    pub const BT_RANGE: u16 = BT + RANGE;
    pub const BT_STOREVN: u16 = BT + STOREVN;
    pub const BT_PATH_BEGIN: u16 = BT + PATH_BEGIN;
    pub const BT_PATH_END: u16 = BT + PATH_END;
    pub const BT_EACH: u16 = BT + EACH;
    pub const BT_EACH_OPT: u16 = BT + EACH_OPT;
    pub const BT_TRY_BEGIN: u16 = BT + TRY_BEGIN;
    pub const BT_TRY_END: u16 = BT + TRY_END;
    pub const BT_DESTRUCTURE_ALT: u16 = BT + DESTRUCTURE_ALT;
    pub const BT_FORK: u16 = BT + FORK;
    pub const BT_RET: u16 = BT + RET;

    // Pseudo-instructions of the native builtins (see `program.rs`).
    pub const SUBRUN_RET: u16 = super::pseudo::SUBRUN_RET;
    pub const BT_SUBRUN_BASE: u16 = BT + super::pseudo::SUBRUN_BASE;
    pub const BT_NATIVE_RESUME: u16 = BT + super::pseudo::NATIVE_RESUME;

    /// `ON_BACKTRACK(op)`.
    #[inline]
    pub const fn on_backtrack(op: u16) -> u16 {
        op + BT
    }
}

impl Jq {
    // ---- data stack (execute.c stack_push/stack_pop/stack_popn) -----------------

    #[inline]
    pub(super) fn push(&mut self, v: Value) {
        self.stk_top = self.stk.push_value(self.stk_top, v);
    }

    #[inline]
    pub(super) fn pop(&mut self) -> Value {
        let (v, top) = self.stk.pop_value(self.stk_top);
        self.stk_top = top;
        v
    }

    #[inline]
    pub(super) fn popn(&mut self) -> Value {
        let (v, top) = self.stk.popn_value(self.stk_top);
        self.stk_top = top;
        v
    }

    // ---- frames ---------------------------------------------------------------

    /// `frame_get_level`: follow `env` links `level` times from the current frame.
    #[inline]
    fn frame_get_level(&self, level: u16) -> StackPtr {
        let mut fr = self.curr_frame;
        for _ in 0..level {
            fr = self.stk.frame(fr).env;
        }
        fr
    }

    /// `frame_local_var`: the index of local `var` of the frame `level` levels up.
    #[inline]
    fn local_var(&self, prog: &Program, var: u16, level: u16) -> usize {
        let fr = self.stk.frame(self.frame_get_level(level));
        debug_assert!((var as u32) < prog.funcs[fr.func as usize].nlocals);
        let _ = prog;
        fr.locals as usize + var as usize
    }

    /// `make_closure`: the closure described by the `(level, idx)` pair at `code[pc..]`.
    #[inline]
    fn make_closure(&self, prog: &Program, code: &[u16], pc: usize) -> Closure {
        let level = code[pc];
        let idx = code[pc + 1];
        let fridx = self.frame_get_level(level);
        let fr = self.stk.frame(fridx);
        if idx & ARG_NEWCLOSURE != 0 {
            // A new closure closing the frame identified by level, and with the bytecode
            // body of the idx'th subfunction of that frame.
            let subfn_idx = (idx & !ARG_NEWCLOSURE) as usize;
            Closure {
                func: prog.funcs[fr.func as usize].subfunctions[subfn_idx],
                env: fridx,
            }
        } else {
            // A reference to a closure from the frame identified by level; copy it as-is.
            debug_assert!((idx as u32) < prog.funcs[fr.func as usize].nclosures);
            self.stk.closures[fr.closures as usize + idx as usize]
        }
    }

    /// `frame_push` for the top-level program (no arguments).
    pub(super) fn frame_push_top(&mut self, top: Closure) {
        let prog = self.prog.clone();
        self.frame_push(&prog, &[], top, 0, 0);
        let f = self.stk.frame_mut(self.curr_frame);
        f.retdata = 0;
        f.retaddr = NO_RETADDR;
    }

    /// `frame_push`: a frame for `callee`, whose closure arguments are the `nargs` pairs
    /// at `code[argdef..]` (resolved against the caller's frame).
    pub(super) fn frame_push(
        &mut self,
        prog: &Program,
        code: &[u16],
        callee: Closure,
        argdef: usize,
        nargs: usize,
    ) {
        let func = &prog.funcs[callee.func as usize];
        debug_assert_eq!(nargs, func.nclosures as usize);
        let frame = Frame {
            func: callee.func,
            env: callee.env,
            retdata: 0,
            retaddr: NO_RETADDR,
            closures: self.stk.closures.len() as u32,
            locals: self.stk.locals.len() as u32,
        };
        let new_frame_idx = self.stk.push_frame(self.curr_frame, frame);
        for i in 0..nargs {
            let cl = self.make_closure(prog, code, argdef + i * 2);
            self.stk.closures.push(cl);
        }
        // jq initializes locals to jv_invalid(); they are always stored before use.
        for _ in 0..func.nlocals {
            self.stk.locals.push(Value::Null);
        }
        self.curr_frame = new_frame_idx;
    }

    /// `frame_push` with the closure arguments given (for native sub-runs).
    pub(super) fn frame_push_args(&mut self, prog: &Program, callee: Closure, args: &[Closure]) {
        let func = &prog.funcs[callee.func as usize];
        debug_assert_eq!(args.len(), func.nclosures as usize);
        let frame = Frame {
            func: callee.func,
            env: callee.env,
            retdata: 0,
            retaddr: NO_RETADDR,
            closures: self.stk.closures.len() as u32,
            locals: self.stk.locals.len() as u32,
        };
        let new_frame_idx = self.stk.push_frame(self.curr_frame, frame);
        self.stk.closures.extend_from_slice(args);
        let n = self.stk.locals.len() + func.nlocals as usize;
        self.stk.locals.resize(n, Value::Null);
        self.curr_frame = new_frame_idx;
    }

    /// `frame_pop`.
    #[inline]
    fn frame_pop(&mut self) {
        debug_assert!(self.curr_frame != 0);
        self.curr_frame = self.stk.pop_frame(self.curr_frame);
    }

    // ---- fork points ------------------------------------------------------------

    /// `stack_save(jq, retaddr, sp)`: records a fork point resuming at `retaddr` with the
    /// current data stack and frame, then moves to the stack position `sp`.
    pub(super) fn stack_save(&mut self, retaddr: usize, sp: (StackPtr, StackPtr)) {
        let path_len = match &self.path {
            Value::Array(a) => a.len() as u32,
            _ => 0,
        };
        let saved_path = !self.value_at_path.is_null();
        if saved_path {
            self.stk.saved_paths.push(self.value_at_path.clone());
        }
        let fork = ForkPoint {
            saved_data_stack: self.stk_top,
            saved_curr_frame: self.curr_frame,
            path_len,
            subexp_nest: self.subexp_nest,
            return_address: retaddr as u32,
            saved_path,
        };
        self.fork_top = self.stk.push_fork(self.fork_top, fork);
        self.stk_top = sp.0;
        self.curr_frame = sp.1;
    }

    /// `stack_restore`: frees everything allocated after the last fork point, restores
    /// its state and returns its pc; `None` when there are no fork points left.
    pub(super) fn stack_restore(&mut self) -> Option<usize> {
        while !self.stk.pop_will_free(self.fork_top) {
            if self.stk.pop_will_free(self.stk_top) {
                // A value, or a suspended native generator abandoned with its fork point.
                self.stk_top = self.stk.drop_data(self.stk_top);
            } else if self.stk.pop_will_free(self.curr_frame) {
                self.frame_pop();
            } else {
                unreachable!("stack_restore: block owned by neither data stack nor frames");
            }
        }

        if self.fork_top == 0 {
            return None;
        }

        self.last_fork = self.fork_top;
        let (fork, next) = self.stk.pop_fork(self.fork_top);
        self.stk_top = fork.saved_data_stack;
        self.curr_frame = fork.saved_curr_frame;
        if let Value::Array(_) = &self.path {
            let Value::Array(a) = std::mem::take(&mut self.path) else {
                unreachable!()
            };
            self.path = Value::Array(a.into_slice(0, fork.path_len as i64));
        }
        if fork.saved_path {
            self.value_at_path = self.stk.saved_paths.pop().expect("saved value_at_path");
        } else if !self.value_at_path.is_null() {
            // (Outside path expressions it is null already: no free to call.)
            self.value_at_path = Value::Null;
        }
        self.subexp_nest = fork.subexp_nest;
        self.fork_top = next;
        Some(fork.return_address as usize)
    }

    // ---- paths ------------------------------------------------------------------

    /// `path_intact`: outside subexpressions of a path expression, a value may only be
    /// used as a path step if it is (identically) the value at the tracked path.
    #[inline]
    fn path_intact(&self, curr: &Value) -> bool {
        if self.subexp_nest == 0 && matches!(self.path, Value::Array(_)) {
            curr.identical(&self.value_at_path)
        } else {
            true
        }
    }

    /// `path_append`.
    #[inline]
    fn path_append(&mut self, component: Value, value_at_path: Value) {
        if self.subexp_nest == 0
            && let Value::Array(p) = &mut self.path
        {
            p.push(component);
            self.value_at_path = value_at_path;
        }
    }

    /// `_jq_path_append` (for `f_getpath`).
    pub(super) fn jq_path_append(&mut self, v: Value, p: Value, value_at_path: CResult) -> CResult {
        let value_at_path = match value_at_path {
            Ok(x) if self.subexp_nest == 0 && matches!(self.path, Value::Array(_)) => x,
            other => return other,
        };
        if !v.identical(&self.value_at_path) {
            return Ok(value_at_path);
        }
        if let Value::Array(path) = &mut self.path {
            match p {
                Value::Array(pa) => path.extend_from_array(&pa),
                other => path.push(other),
            }
        }
        self.value_at_path = value_at_path.clone();
        Ok(value_at_path)
    }

    // ---- errors -----------------------------------------------------------------

    /// `set_error(jq, jv_invalid_with_msg(msg))`.
    #[inline]
    pub(super) fn set_error(&mut self, msg: Value) {
        self.error = Some(Raised { msg, wraps: 0 });
    }

    // ---- the interpreter ------------------------------------------------------------

    /// `jq_next`.
    pub(super) fn jq_next(&mut self) -> Option<Result<Value, Error>> {
        let prog = self.prog.clone();
        let pc = self.stack_restore().expect("jq_next: nothing to resume");
        let backtracking = !self.initial_execution;
        self.initial_execution = false;
        debug_assert!(self.error.is_none());
        let exit = if self.debug_trace != 0 {
            self.run::<true>(&prog, pc, backtracking, 0)
        } else {
            self.run::<false>(&prog, pc, backtracking, 0)
        };
        match exit {
            Exit::Yield(v) => Some(Ok(v)),
            Exit::Done => self.error.take().map(|e| Err(Error::new(e.msg))),
            Exit::Halted => None,
            Exit::SubRet(_) | Exit::Base => unreachable!("sub-run exit at the top level"),
        }
    }

    /// The interpreter loop of `jq_next`, from `pc` (backtracking into it if
    /// `backtracking`). Natives run it re-entrantly for their sub-runs (`native.rs`),
    /// whose base fork point is `base` (0 at the top level). `TRACE` is whether
    /// `--debug-trace` is on (`debug_trace != 0`); natives never run then.
    ///
    /// jq checks `jq_halted` before every instruction; only `CALL_BUILTIN` (`halt`,
    /// `halt_error`) and natives (whose closures may halt) can halt the program, so the
    /// check is after those.
    pub(super) fn run<const TRACE: bool>(
        &mut self,
        prog: &Program,
        mut pc: usize,
        mut backtracking: bool,
        base: StackPtr,
    ) -> Exit {
        // The trace prints the original instructions; regions only in `fast_code`.
        let opt = !TRACE && self.opt;
        let code: &[u16] = if opt { &prog.fast_code } else { &prog.code };

        // `goto do_backtrack`.
        macro_rules! backtrack {
            () => {{
                match self.stack_restore() {
                    Some(p) => {
                        pc = p;
                        backtracking = true;
                        continue;
                    }
                    None => return Exit::Done,
                }
            }};
        }

        // `jq_halted` at the top of the loop.
        macro_rules! halted {
            () => {{
                if TRACE {
                    self.trace_write(b"\t<halted>\n");
                }
                return Exit::Halted;
            }};
        }
        if self.halted {
            halted!();
        }

        loop {
            let mut opcode = code[pc];
            let mut raising = false;

            if TRACE {
                self.trace_instruction(prog, pc, backtracking);
            }

            if backtracking {
                opcode = op::on_backtrack(opcode);
                backtracking = false;
                raising = self.error.is_some();
            }
            pc += 1;

            match opcode {
                op::TOP => {}

                op::ERRORK => {
                    let v = self.constant(prog, code[pc]);
                    self.set_error(v);
                    backtrack!();
                }

                op::LOADK => {
                    let v = self.constant(prog, code[pc]);
                    pc += 1;
                    self.stk_top = self.stk.replace_value(self.stk_top, v);
                }

                op::GENLABEL => {
                    let label = label_object(self.next_label);
                    self.next_label = self.next_label.wrapping_add(1);
                    self.push(label);
                }

                op::DUP => {
                    self.stk_top = self.stk.dup_value(self.stk_top);
                }

                op::DUPN => {
                    let v = self.popn();
                    self.push(v.clone());
                    self.push(v);
                }

                op::DUP2 => {
                    let keep = self.pop();
                    let v = self.pop();
                    self.push(v.clone());
                    self.push(keep);
                    self.push(v);
                }

                op::SUBEXP_BEGIN => {
                    self.stk_top = self.stk.dup_value(self.stk_top);
                    self.subexp_nest += 1;
                }

                op::SUBEXP_END => {
                    debug_assert!(self.subexp_nest > 0);
                    self.subexp_nest -= 1;
                    self.stk_top = self.stk.swap_values(self.stk_top);
                }

                op::PUSHK_UNDER => {
                    let v = self.constant(prog, code[pc]);
                    pc += 1;
                    self.stk_top = self.stk.push_under(self.stk_top, v);
                }

                op::POP => {
                    self.stk_top = self.stk.drop_value(self.stk_top);
                }

                op::APPEND => {
                    let v = self.pop();
                    let level = code[pc];
                    let vidx = code[pc + 1];
                    pc += 2;
                    let var = self.local_var(prog, vidx, level);
                    match &mut self.stk.locals[var] {
                        Value::Array(a) => a.push(v),
                        _ => unreachable!("APPEND to a non-array"),
                    }
                }

                op::INSERT => {
                    let stktop = self.pop();
                    let v = self.pop();
                    let k = self.pop();
                    let objv = self.pop();
                    match (objv, k) {
                        (Value::Object(mut o), Value::String(k)) => {
                            o.insert(k, v);
                            self.push(Value::Object(o));
                            self.push(stktop);
                        }
                        (objv, k) => {
                            debug_assert!(matches!(objv, Value::Object(_)));
                            let msg = format!(
                                "Cannot use {} ({}) as object key",
                                k.kind_name(),
                                dump_string_trunc(&k, 15)
                            );
                            self.set_error(Value::from(msg));
                            backtrack!();
                        }
                    }
                }

                op::RANGE | op::BT_RANGE => {
                    let level = code[pc];
                    let v = code[pc + 1];
                    pc += 2;
                    let var = self.local_var(prog, v, level);
                    // Another value: jq pops the bound and pushes it back, which puts
                    // it in the same block when it is the last one. Leave it there.
                    if !raising
                        && self.stk.pop_will_free(self.stk_top)
                        && let (Value::Number(c), Value::Number(m)) =
                            (&self.stk.locals[var], self.stk.value(self.stk_top))
                        && c.value() < m.value()
                    {
                        let cur = c.value();
                        let curr =
                            std::mem::replace(&mut self.stk.locals[var], Value::number(cur + 1.0));
                        let spos = (self.stk.next(self.stk_top), self.curr_frame);
                        self.stack_save(pc - 3, spos);
                        self.push(curr);
                        continue;
                    }
                    let max = self.pop();
                    if raising {
                        drop(max);
                        backtrack!();
                    }
                    let (cur, maxv) = match (&self.stk.locals[var], &max) {
                        (Value::Number(c), Value::Number(m)) => (c.value(), m.value()),
                        _ => {
                            self.set_error(Value::from("Range bounds must be numeric"));
                            drop(max);
                            backtrack!();
                        }
                    };
                    if cur >= maxv {
                        // finished iterating
                        drop(max);
                        backtrack!();
                    }
                    let curr =
                        std::mem::replace(&mut self.stk.locals[var], Value::number(cur + 1.0));
                    let spos = (self.stk_top, self.curr_frame);
                    self.push(max);
                    self.stack_save(pc - 3, spos);
                    self.push(curr);
                }

                op::LOADV => {
                    let level = code[pc];
                    let v = code[pc + 1];
                    pc += 2;
                    let var = self.local_var(prog, v, level);
                    if TRACE {
                        self.trace_var_refcount(v, var);
                    }
                    let val = self.stk.locals[var].clone();
                    self.stk_top = self.stk.replace_value(self.stk_top, val);
                }

                // Does a load but replaces the variable with null.
                op::LOADVN => {
                    let level = code[pc];
                    let v = code[pc + 1];
                    pc += 2;
                    let var = self.local_var(prog, v, level);
                    if TRACE {
                        self.trace_var_refcount(v, var);
                    }
                    let val = std::mem::take(&mut self.stk.locals[var]);
                    self.stk_top = self.stk.replace_value_n(self.stk_top, val);
                }

                op::STOREVN | op::STOREV => {
                    if opcode == op::STOREVN {
                        let pos = (self.stk_top, self.curr_frame);
                        self.stack_save(pc - 1, pos);
                    }
                    let level = code[pc];
                    let v = code[pc + 1];
                    pc += 2;
                    let var = self.local_var(prog, v, level);
                    let val = self.pop();
                    if TRACE {
                        self.trace_store(v, &val);
                    }
                    self.stk.locals[var] = val;
                }

                op::BT_STOREVN => {
                    let level = code[pc];
                    let v = code[pc + 1];
                    let var = self.local_var(prog, v, level);
                    self.stk.locals[var] = Value::Null;
                    backtrack!();
                }

                op::STORE_GLOBAL => {
                    // Get the constant
                    let val = self.constant(prog, code[pc]);
                    // Store the var
                    let level = code[pc + 1];
                    let v = code[pc + 2];
                    pc += 3;
                    let var = self.local_var(prog, v, level);
                    if TRACE {
                        self.trace_store(v, &val);
                    }
                    self.stk.locals[var] = val;
                }

                op::PATH_BEGIN => {
                    let v = self.pop();
                    // jq pushes jq->path itself (it is replaced just below); the fork
                    // point still records its length.
                    self.push(self.path.clone());
                    let pos = (self.stk_top, self.curr_frame);
                    self.stack_save(pc - 1, pos);
                    self.push(Value::number(self.subexp_nest as f64));
                    let old_value_at_path = std::mem::take(&mut self.value_at_path);
                    self.push(old_value_at_path);
                    self.push(v.clone());
                    self.path = Value::empty_array();
                    self.value_at_path = v; // next INDEX operation must index into v
                    self.subexp_nest = 0;
                }

                op::PATH_END => {
                    let v = self.pop();
                    // detect invalid path expression like path(.a | reverse)
                    if !self.path_intact(&v) {
                        let msg = format!(
                            "Invalid path expression with result {}",
                            dump_string_trunc(&v, 30)
                        );
                        self.set_error(Value::from(msg));
                        backtrack!();
                    }
                    drop(v); // discard value, only keep path

                    let old_value_at_path = self.pop();
                    let old_subexp_nest = self.pop().as_f64().unwrap_or(0.0) as i32;

                    let path = std::mem::take(&mut self.path);
                    self.path = self.pop();

                    let spos = (self.stk_top, self.curr_frame);
                    self.push(path.clone());
                    self.stack_save(pc - 1, spos);

                    self.push(path);
                    self.subexp_nest = old_subexp_nest;
                    self.value_at_path = old_value_at_path;
                }

                op::BT_PATH_BEGIN | op::BT_PATH_END => {
                    self.path = self.pop();
                    backtrack!();
                }

                op::INDEX | op::INDEX_OPT => {
                    let t = self.pop();
                    let k = self.pop();
                    // detect invalid path expression like path(reverse | .a)
                    if !self.path_intact(&t) {
                        let msg = format!(
                            "Invalid path expression near attempt to access element {} of {}",
                            dump_string_trunc(&k, 15),
                            dump_string_trunc(&t, 30)
                        );
                        self.set_error(Value::from(msg));
                        backtrack!();
                    }
                    // jv_get(t, jv_copy(k)): t is consumed.
                    let r = t.get(&k);
                    drop(t);
                    match r {
                        Ok(v) => {
                            self.path_append(k, v.clone());
                            self.push(v);
                        }
                        Err(e) => {
                            drop(k);
                            if opcode == op::INDEX {
                                self.set_error(e.into_value());
                            }
                            backtrack!();
                        }
                    }
                }

                op::JUMP => {
                    let offset = code[pc] as usize;
                    pc += 1;
                    pc += offset;
                }

                op::JUMP_F => {
                    let offset = code[pc] as usize;
                    pc += 1;
                    let t = self.pop();
                    if !t.is_truthy() {
                        pc += offset;
                    }
                    self.push(t); // FIXME do this better
                }

                op::EACH | op::EACH_OPT | op::BT_EACH | op::BT_EACH_OPT => {
                    let first = opcode == op::EACH || opcode == op::EACH_OPT;
                    let (container, idx) = if first {
                        let container = self.pop();
                        // detect invalid path expression like path(reverse | .[])
                        if !self.path_intact(&container) {
                            let msg = format!(
                                "Invalid path expression near attempt to iterate through {}",
                                dump_string_trunc(&container, 30)
                            );
                            self.set_error(Value::from(msg));
                            backtrack!();
                        }
                        // (jq pushes container and -1 and falls through, popping them
                        // right back.)
                        (container, -1i64)
                    } else {
                        let idx = self.pop().as_f64().unwrap_or(0.0) as i32 as i64;
                        let container = self.pop();
                        (container, idx)
                    };

                    let mut is_last = false;
                    let (keep_going, idx, kv) = match &container {
                        Value::Array(a) => {
                            let idx = if first { 0 } else { idx + 1 };
                            let len = a.len() as i64;
                            is_last = idx == len - 1;
                            if idx < len {
                                let value = a.get(idx as usize).cloned().unwrap_or_default();
                                (true, idx, Some((Value::number(idx as f64), value)))
                            } else {
                                (false, idx, None)
                            }
                        }
                        Value::Object(o) => {
                            let idx = if first { 0 } else { idx + 1 };
                            match o.get_index(idx as usize) {
                                Some((k, v)) => {
                                    (true, idx, Some((Value::String(k.clone()), v.clone())))
                                }
                                None => (false, idx, None),
                            }
                        }
                        _ => {
                            debug_assert!(first);
                            if opcode == op::EACH {
                                let msg = format!(
                                    "Cannot iterate over {} ({})",
                                    container.kind_name(),
                                    dump_string_trunc(&container, 15)
                                );
                                self.set_error(Value::from(msg));
                            }
                            (false, idx, None)
                        }
                    };

                    if !keep_going || raising {
                        drop(kv);
                        drop(container);
                        backtrack!();
                    }
                    let (key, value) = kv.expect("keep_going");
                    if is_last {
                        // we don't need to make a backtrack point
                        drop(container);
                        self.path_append(key, value.clone());
                        self.push(value);
                    } else {
                        let spos = (self.stk_top, self.curr_frame);
                        self.push(container);
                        self.push(Value::number(idx as f64));
                        self.stack_save(pc - 1, spos);
                        self.path_append(key, value.clone());
                        self.push(value);
                    }
                }

                op::BACKTRACK => {
                    backtrack!();
                }

                op::TRY_BEGIN => {
                    let pos = (self.stk_top, self.curr_frame);
                    self.stack_save(pc - 1, pos);
                    pc += 1; // skip handler offset this time
                }

                op::TRY_END => {
                    let pos = (self.stk_top, self.curr_frame);
                    self.stack_save(pc - 1, pos);
                }

                op::BT_TRY_BEGIN => {
                    if !raising {
                        // `try EXP ...` -- EXP backtracked (e.g., EXP was `empty`), so we
                        // backtrack more:
                        drop(self.pop());
                        backtrack!();
                    }
                    // Else `(try EXP ... ) | EXP2` raised an error.
                    //
                    // If the error was wrapped in another error, then that means EXP2
                    // raised the error. We unwrap it and re-raise it as it wasn't raised
                    // by EXP.
                    let err = self.error.as_mut().expect("raising");
                    if err.wraps > 0 {
                        err.wraps -= 1;
                        backtrack!();
                    }
                    // Else we caught an error containing a non-error value, so we jump to
                    // the handler.
                    let offset = code[pc] as usize;
                    pc += 1;
                    drop(self.pop()); // free the input
                    let err = self.error.take().expect("raising");
                    self.push(err.msg); // push the error's message
                    pc += offset;
                }

                op::BT_TRY_END => {
                    // Wrap the error so the matching TRY_BEGIN doesn't catch it
                    if raising && let Some(err) = &mut self.error {
                        err.wraps += 1;
                    }
                    backtrack!();
                }

                op::DESTRUCTURE_ALT | op::FORK => {
                    let pos = (self.stk_top, self.curr_frame);
                    self.stack_save(pc - 1, pos);
                    pc += 1; // skip offset this time
                }

                op::BT_DESTRUCTURE_ALT => {
                    if self.error.is_none() {
                        // `try EXP ...` backtracked here (no value, `empty`), so we
                        // backtrack more
                        drop(self.pop());
                        backtrack!();
                    }
                    // `try EXP ...` exception caught in EXP. DESTRUCTURE_ALT doesn't want
                    // the error message on the stack, as we would just want to throw it
                    // away anyway.
                    self.error = None;
                    let offset = code[pc] as usize;
                    pc += 1;
                    pc += offset;
                }

                op::BT_FORK => {
                    if raising {
                        backtrack!();
                    }
                    let offset = code[pc] as usize;
                    pc += 1;
                    pc += offset;
                }

                op::CALL_BUILTIN => {
                    let nargs = code[pc] as usize;
                    let function = prog.cfunctions[code[pc + 1] as usize];
                    pc += 2;
                    debug_assert_eq!(nargs, function.nargs);
                    let input = self.pop();
                    // The arguments, sized by arity (nothing extra to drop afterwards).
                    let top = match nargs {
                        1 => (function.f)(self, input, &mut []),
                        2 => {
                            let mut args = [self.pop()];
                            (function.f)(self, input, &mut args)
                        }
                        3 => {
                            let a = self.pop();
                            let b = self.pop();
                            let mut args = [a, b];
                            (function.f)(self, input, &mut args)
                        }
                        _ => {
                            let mut args = [Value::Null, Value::Null, Value::Null];
                            for a in args.iter_mut().take(nargs - 1) {
                                *a = self.pop();
                            }
                            (function.f)(self, input, &mut args[..nargs - 1])
                        }
                    };
                    match top {
                        Ok(v) => {
                            self.push(v);
                            if self.halted {
                                halted!();
                            }
                        }
                        Err(e) => {
                            self.set_error(e.into_value());
                            backtrack!();
                        }
                    }
                }

                op::TAIL_CALL_JQ | op::CALL_JQ => {
                    // Bytecode layout here:
                    //
                    //  CALL_JQ
                    //  <nclosures>                       (i.e., number of call arguments)
                    //  <callee closure>                  (what we're calling)
                    //  <nclosures' worth of closures>    (frame reference + code pointer)
                    //
                    //  <next instruction (to return to)>
                    //
                    // Each closure consists of two uint16_t values: a "level" identifying
                    // the frame to be closed over, and an index. See make_closure().
                    let mut input = self.pop();
                    let nclosures = code[pc] as usize;
                    pc += 1;
                    let mut retaddr = (pc + 2 + nclosures * 2) as u32;
                    let mut retdata = self.stk_top;
                    let cl = self.make_closure(prog, code, pc);
                    // A function whose body is a region runs without a frame: jq's
                    // frame would be freed by the body's RET before anything else ran.
                    if opt
                        && nclosures == 0
                        && let Some(d) = prog.funcs[cl.func as usize].direct
                    {
                        let r = &prog.regions[d as usize];
                        if opcode == op::CALL_JQ {
                            match self.run_direct(prog, r, cl.env, input) {
                                Some(v) => {
                                    self.push(v);
                                    pc = retaddr as usize;
                                    continue;
                                }
                                None => backtrack!(),
                            }
                        }
                        // A tail call pops the caller's frame first (freeing its
                        // locals, which is observable), and returns to its caller. The
                        // top level's return yields instead: leave that to the frame.
                        let f = *self.stk.frame(self.curr_frame);
                        if f.retaddr != NO_RETADDR {
                            self.frame_pop();
                            debug_assert_eq!(self.stk_top, f.retdata);
                            match self.run_direct(prog, r, cl.env, input) {
                                Some(v) => {
                                    self.push(v);
                                    pc = f.retaddr as usize;
                                    continue;
                                }
                                None => backtrack!(),
                            }
                        }
                    }
                    // A builtin.jq definition with a native implementation (tail calls
                    // only without arguments, which jq would resolve after the pop).
                    let native = match prog.funcs[cl.func as usize].native {
                        Some(id)
                            if (opcode == op::CALL_JQ || nclosures == 0)
                                && self.natives_ok(prog, id) =>
                        {
                            Some(id)
                        }
                        _ => None,
                    };
                    let mut args = [Closure { func: 0, env: 0 }; super::native::MAX_NATIVE_ARGS];
                    if native.is_some() {
                        for (i, a) in args.iter_mut().enumerate().take(nclosures) {
                            *a = self.make_closure(prog, code, pc + 2 + i * 2);
                        }
                    }
                    if opcode == op::TAIL_CALL_JQ {
                        let f = self.stk.frame(self.curr_frame);
                        retaddr = f.retaddr;
                        retdata = f.retdata;
                        self.frame_pop();
                    }
                    if let Some(id) = native {
                        debug_assert_eq!(self.stk_top, retdata);
                        match self.call_native(prog, id, cl, input, &args[..nclosures], retaddr) {
                            Applied::Continue(p) => {
                                pc = p;
                                continue;
                            }
                            Applied::Backtrack => backtrack!(),
                            Applied::Halted => halted!(),
                            Applied::Fallback(v) => input = v,
                        }
                    }
                    self.frame_push(prog, code, cl, pc + 2, nclosures);
                    let f = self.stk.frame_mut(self.curr_frame);
                    f.retdata = retdata;
                    f.retaddr = retaddr;
                    pc = prog.funcs[cl.func as usize].base as usize;
                    self.push(input);
                }

                op::RET => {
                    let f = *self.stk.frame(self.curr_frame);
                    // When a fork point keeps the frame (a generator) and the value's
                    // block is the last one, jq frees that block, keeps the frame, and
                    // pushes the value into the same block with the same `next` (the
                    // frame's retdata): only the current frame changes.
                    if f.retaddr != NO_RETADDR
                        && self.stk.pop_will_free(self.stk_top)
                        && self.curr_frame + 1 != self.stk_top
                    {
                        debug_assert_eq!(self.stk.next(self.stk_top), f.retdata);
                        pc = f.retaddr as usize;
                        self.curr_frame = self.stk.next(self.curr_frame);
                        continue;
                    }
                    let value = self.pop();
                    debug_assert_eq!(self.stk_top, f.retdata);
                    if f.retaddr != NO_RETADDR {
                        // function return
                        pc = f.retaddr as usize;
                        self.frame_pop();
                    } else {
                        // top-level return, yielding value
                        let spos = (self.stk_top, self.curr_frame);
                        self.push(Value::Null);
                        self.stack_save(pc - 1, spos);
                        return Exit::Yield(value);
                    }
                    self.push(value);
                }

                op::BT_RET => {
                    // resumed after top-level return
                    backtrack!();
                }

                op::SUBRUN_RET => {
                    // A sub-run's closure returned: hand its value to the native.
                    return Exit::SubRet(self.pop());
                }

                op::BT_SUBRUN_BASE => {
                    if self.last_fork == base {
                        return Exit::Base;
                    }
                    // The base of a run whose native is gone: keep backtracking.
                    debug_assert!(false, "orphaned sub-run base");
                    backtrack!();
                }

                op::BT_NATIVE_RESUME => match self.resume_native(prog, raising) {
                    Applied::Continue(p) => pc = p,
                    Applied::Backtrack => backtrack!(),
                    Applied::Halted => halted!(),
                    Applied::Fallback(_) => unreachable!(),
                },

                // A region (`region.rs`): the instructions from pc - 1 to its end.
                op if op >= REGION_BASE && opt => {
                    let r = &prog.regions[(op - REGION_BASE) as usize];
                    if self.run_region(prog, r) {
                        pc = r.end as usize;
                    } else {
                        backtrack!();
                    }
                }

                _ => {
                    let name = Opcode::from_u16(opcode % NUM_OPCODES as u16)
                        .map_or("#INVALID", |o| o.name());
                    panic!("invalid instruction {name} ({opcode}) at pc {}", pc - 1);
                }
            }
        }
    }

    /// `jv_array_get(jv_copy(frame_current(jq)->bc->constants), idx)`.
    #[inline(always)]
    fn constant(&self, prog: &Program, idx: u16) -> Value {
        prog.constant(|| self.stk.frame(self.curr_frame).func, idx)
            .clone()
    }
}
