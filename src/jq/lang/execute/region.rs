//! Regions: runs of simple instructions compiled to register code.
//!
//! Most of a jq program's instructions only shuffle the data stack around a few
//! computations: `. % 3 == 0` is `PUSHK_UNDER 0; SUBEXP_BEGIN; PUSHK_UNDER 3; DUP;
//! CALL_BUILTIN _mod; SUBEXP_END; CALL_BUILTIN _equal`, seven dispatches and a dozen
//! block pushes and pops for two operations. A *region* is such a run, compiled when the
//! program is loaded into a short list of [`Op`]s on a small register file: the stack
//! shuffles become register renaming, and only the value operations remain (here two
//! [`Op::Binop`]s). The interpreter loop runs a region as one instruction (see
//! `Program::fast_code`), and a function whose whole body is a region is run without a
//! frame when it is called (`CALL_JQ` of a closure without parameters, and natives'
//! closures): its region's *direct* form.
//!
//! # Why a region is exact
//!
//! A region contains no instruction that creates a fork point, calls a jq function,
//! or depends on anything but its operands and the frames' variables: see
//! [`Compiler::instr`] for the list. Between its first and last instruction the stack
//! then behaves as a plain LIFO stack, so the blocks jq allocates for intermediate
//! values are freed in the order they were allocated and nothing but those
//! instructions sees them. What remains observable is kept exactly:
//!
//! * **Value operations in the same order, on the same operands, with the same
//!   ownership.** Each instruction is translated into its own sequence of copies
//!   (`jv_copy`), moves and frees on registers, so every builtin call, index and
//!   insert sees its operands with the same reference counts as in jq: whether an
//!   array is written in place (and so its storage, which jq exposes) is decided
//!   identically. Only two things are left out, both invisible: pure renaming (the
//!   stack order), and a copy made by `DUP` right before a binop that frees it
//!   unused (`f_plus` frees its input before `binop_plus`).
//! * **The blocks of values that were on the stack before the region.** They are
//!   popped ([`Op::Pop`], [`Op::PopN`] for `stack_popn`) exactly where the
//!   instruction popping them runs, so a block a fork point keeps is copied (or, for
//!   `LOADVN` and `DUPN`, nulled) at the same moment relative to every error.
//! * **Errors** are raised by the same functions with the same operands; the region
//!   then drops its registers and backtracks, as the stack would be unwound.
//! * **Path expressions.** `INDEX` at the region's own nesting level tracks the path
//!   exactly as the instruction does ([`Jq::region_index`]); `getpath`, the only
//!   builtin that reads the path state, isn't allowed in regions. The region adds its
//!   net `SUBEXP_BEGIN`/`SUBEXP_END` count to `subexp_nest` when it exits.
//! * **Labels**: `GENLABEL` advances the same counter.
//!
//! Frames are not observable except through `--debug-trace` (which runs the original
//! code), so a direct call's missing frame is invisible: a region body has no fork
//! point, so jq's frame would be freed by the body's `RET`, before anything else runs.
//!
//! Control flow inside a region is forward jumps only (jq's are), for `if`, `and`,
//! `or` and `//=`-free alternatives: at every jump and join the registers are put in a
//! canonical order (stack slot `i` in register `i`), so all paths agree.

use std::mem::{ManuallyDrop, take};

use super::program::Program;
use super::stack::StackPtr;
use super::{Jq, label_object};
use crate::jq::builtins::CResult;
use crate::jq::builtins::binops::{
    binop_divide, binop_equal, binop_greater, binop_greatereq, binop_less, binop_lesseq,
    binop_minus, binop_mod, binop_multiply, binop_notequal, binop_plus,
};
use crate::jq::lang::bytecode::{ARG_NEWCLOSURE, Opcode, bytecode_operation_length};
use crate::jq::value::{Value, dump_string_trunc};

/// A register: an index into a region's register file.
pub(super) type Reg = u8;

/// The most registers a region uses (a power of two: indexes are masked with it).
pub(super) const MAX_REGS: usize = 16;

/// The register file of regions that use at most this many (a power of two).
const SMALL_REGS: usize = 4;

/// [`Op::Binop`]'s `input` when the copy of an operand it would free was never made.
const NO_REG: Reg = u8::MAX;

/// The opcode of region `i` in `Program::fast_code` is `REGION_BASE + i`.
pub(super) const REGION_BASE: u16 = 0x8000;

/// The most instructions a region compilation looks at (keeps loading linear).
const MAX_SCAN: usize = 512;

/// The fewest instructions worth a region in the interpreter loop.
const MIN_LOOP_INSTRS: u32 = 3;

/// The binops (`BINOP` in builtin.c): `f_<op>(input, a, b)` frees `input`, then
/// returns `binop_<op>(a, b)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BinKind {
    Plus,
    Minus,
    Multiply,
    Divide,
    Mod,
    Equal,
    NotEqual,
    Less,
    LessEq,
    Greater,
    GreaterEq,
}

impl BinKind {
    fn of(name: &str) -> Option<BinKind> {
        use BinKind::*;
        Some(match name {
            "_plus" => Plus,
            "_minus" => Minus,
            "_multiply" => Multiply,
            "_divide" => Divide,
            "_mod" => Mod,
            "_equal" => Equal,
            "_notequal" => NotEqual,
            "_less" => Less,
            "_lesseq" => LessEq,
            "_greater" => Greater,
            "_greatereq" => GreaterEq,
            _ => return None,
        })
    }
}

/// builtin.c's `dtoi` (as `binops.rs` has it).
#[inline]
fn dtoi(n: f64) -> i64 {
    if n < i64::MIN as f64 {
        i64::MIN
    } else if -n < i64::MIN as f64 {
        i64::MAX
    } else {
        n as i64
    }
}

/// `binop_<kind>(a, b)` of two numbers, where it is a number or a boolean: what
/// `binops.rs` computes, without taking the operands. `None` for the rest (errors, and
/// orderings with NaN, which `jv_cmp` sorts apart).
#[inline(always)]
fn binop_numbers(kind: BinKind, a: &Value, b: &Value) -> Option<Value> {
    use BinKind::*;
    use std::cmp::Ordering;
    let (Value::Number(x), Value::Number(y)) = (a, b) else {
        return None;
    };
    Some(match kind {
        Plus => Value::number(x.value() + y.value()),
        Minus => Value::number(x.value() - y.value()),
        Multiply => Value::number(x.value() * y.value()),
        Divide if y.value() != 0.0 => Value::number(x.value() / y.value()),
        Divide => return None,
        Mod => {
            let (na, nb) = (x.value(), y.value());
            if na.is_nan() || nb.is_nan() {
                Value::number(f64::NAN)
            } else {
                let bi = dtoi(nb);
                if bi == 0 {
                    return None;
                }
                // Check if the divisor is -1 to avoid overflow when the dividend is
                // INTMAX_MIN.
                let r = if bi == -1 { 0 } else { dtoi(na) % bi };
                Value::number(r as f64)
            }
        }
        Equal => Value::Bool(x.equal(y)),
        NotEqual => Value::Bool(!x.equal(y)),
        Less | LessEq | Greater | GreaterEq => {
            if x.is_nan() || y.is_nan() {
                return None;
            }
            let o = x.compare(y);
            Value::Bool(match kind {
                Less => o == Ordering::Less,
                LessEq => o != Ordering::Greater,
                Greater => o == Ordering::Greater,
                _ => o != Ordering::Less,
            })
        }
    })
}

/// Frees `v` (jq's `jv_free`), without calling the drop glue for values that own
/// nothing (null, booleans, native numbers).
#[inline(always)]
fn discard(v: Value) {
    match &v {
        Value::Null | Value::Bool(_) => std::mem::forget(v),
        Value::Number(n) if !n.is_literal() => std::mem::forget(v),
        _ => drop(v),
    }
}

/// `binop_<kind>(a, b)`, with the arithmetic and comparisons of two numbers inline.
#[inline]
fn binop(kind: BinKind, a: Value, b: Value) -> CResult {
    use BinKind::*;
    if let Some(v) = binop_numbers(kind, &a, &b) {
        discard(a);
        discard(b);
        return Ok(v);
    }
    match kind {
        Plus => binop_plus(a, b),
        Minus => binop_minus(a, b),
        Multiply => binop_multiply(a, b),
        Divide => binop_divide(a, b),
        Mod => binop_mod(a, b),
        Equal => binop_equal(a, b),
        NotEqual => binop_notequal(a, b),
        Less => binop_less(a, b),
        LessEq => binop_lesseq(a, b),
        Greater => binop_greater(a, b),
        GreaterEq => binop_greatereq(a, b),
    }
}

/// One operation of a region. Registers named `dst` are overwritten (freeing what
/// they held, as jq's replacing pushes do); consumed operands are moved out.
#[derive(Clone, Copy, Debug)]
pub(super) enum Op {
    /// `dst = stack_pop()`: the top block of the data stack, moved if it is freed,
    /// copied if a fork point keeps it.
    Pop { dst: Reg },
    /// `dst = stack_popn()`: like [`Op::Pop`], but a kept block gets `null`.
    PopN { dst: Reg },
    /// `dst = jv_copy(src)`.
    Clone { dst: Reg, src: Reg },
    /// `dst = src` (a move; `src` is left `null`).
    Move { dst: Reg, src: Reg },
    /// Exchanges two registers.
    Swap { a: Reg, b: Reg },
    /// `jv_free(r)`.
    Drop { r: Reg },
    /// `dst = jv_copy(constant)`.
    Const { dst: Reg, k: *const Value },
    /// `LOADV`: `dst = jv_copy(var)`.
    LoadV { dst: Reg, level: u16, idx: u16 },
    /// `LOADVN`: `dst = var; var = null`.
    LoadVN { dst: Reg, level: u16, idx: u16 },
    /// `STOREV`: `var = src`.
    StoreV { src: Reg, level: u16, idx: u16 },
    /// `STORE_GLOBAL`: `var = jv_copy(constant)`.
    StoreK {
        k: *const Value,
        level: u16,
        idx: u16,
    },
    /// `APPEND`: `var = jv_array_append(var, src)`.
    Append { src: Reg, level: u16, idx: u16 },
    /// `CALL_BUILTIN`: `dst = cfunction(input, args[..nargs - 1])`.
    Call {
        dst: Reg,
        cf: u16,
        nargs: u8,
        input: Reg,
        args: [Reg; 3],
    },
    /// A binop `CALL_BUILTIN`: frees `input` (unless [`NO_REG`]), then
    /// `dst = binop(a, b)`.
    Binop {
        dst: Reg,
        kind: BinKind,
        input: Reg,
        a: Reg,
        b: Reg,
    },
    /// [`Op::Binop`] with a constant operand: `a` is `k` if `kfirst`, else `b` is.
    BinopK {
        dst: Reg,
        kind: BinKind,
        input: Reg,
        r: Reg,
        k: *const Value,
        kfirst: bool,
    },
    /// `INDEX`/`INDEX_OPT`: `dst = t[k]`; `level` is the region's `SUBEXP` nesting
    /// there, relative to its start (path tracking applies at nesting 0).
    Index {
        dst: Reg,
        t: Reg,
        k: Reg,
        opt: bool,
        level: i32,
    },
    /// [`Op::Index`] with a constant key.
    IndexK {
        dst: Reg,
        t: Reg,
        k: *const Value,
        opt: bool,
        level: i32,
    },
    /// `INSERT`: `obj[k] = v` (in place in `obj`).
    Insert { obj: Reg, k: Reg, v: Reg },
    /// `GENLABEL`: `dst = {"__jq": next_label++}`.
    GenLabel { dst: Reg },
    /// `JUMP_F`: continue at op `target` if `r` is false or null (`r` stays).
    JumpF { r: Reg, target: u32 },
    /// `JUMP`: continue at op `target`.
    Jump { target: u32 },
    /// `BACKTRACK` (or a call of a function that only backtracks, like `empty`).
    Backtrack,
    /// `ERRORK`: raise the constant.
    ErrorK { k: *const Value },
    /// The end: push [`Region::exit`].
    Exit,
}

/// Whether `op` reads or writes register `r`.
fn op_uses(op: &Op, r: Reg) -> bool {
    match *op {
        Op::Pop { dst }
        | Op::PopN { dst }
        | Op::Const { dst, .. }
        | Op::LoadV { dst, .. }
        | Op::LoadVN { dst, .. }
        | Op::GenLabel { dst } => dst == r,
        Op::Clone { dst, src } | Op::Move { dst, src } => dst == r || src == r,
        Op::Swap { a, b } => a == r || b == r,
        Op::Drop { r: x } | Op::JumpF { r: x, .. } => x == r,
        Op::StoreV { src, .. } | Op::Append { src, .. } => src == r,
        Op::StoreK { .. } | Op::Jump { .. } | Op::Backtrack | Op::ErrorK { .. } | Op::Exit => false,
        Op::Call {
            dst,
            input,
            args,
            nargs,
            ..
        } => dst == r || input == r || args[..nargs as usize - 1].contains(&r),
        Op::Binop {
            dst, input, a, b, ..
        } => dst == r || input == r || a == r || b == r,
        Op::BinopK {
            dst, input, r: x, ..
        } => dst == r || input == r || x == r,
        Op::Index { dst, t, k, .. } => dst == r || t == r || k == r,
        Op::IndexK { dst, t, .. } => dst == r || t == r,
        Op::Insert { obj, k, v } => obj == r || k == r || v == r,
    }
}

/// A compiled region (see the module docs).
pub(super) struct Region {
    pub ops: Vec<Op>,
    /// The registers holding the data stack at [`Op::Exit`], bottom to top.
    pub exit: Vec<Reg>,
    /// The global pc after the region.
    pub end: u32,
    /// The net change of `subexp_nest`.
    pub nest: i32,
    /// How many registers the ops use (registers `0..nregs`).
    pub nregs: u8,
    /// Whether the ops use variables of the current frame (level 0).
    pub vars0: bool,
    /// Whether the region can backtrack without an error (`BACKTRACK`, `empty`, an
    /// `INDEX_OPT` that fails).
    pub may_backtrack: bool,
}

/// How a region is entered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// From the interpreter loop, on the current frame's data stack.
    Loop,
    /// As a whole function body, without a frame: the input is given, the result
    /// returned, and variables are only those of enclosing functions.
    Direct,
}

/// Where a region's variables are.
#[derive(Clone, Copy)]
enum Vars {
    /// Level 0 is this frame (the interpreter loop's current frame).
    Frame(StackPtr),
    /// Level 1 is this frame (a direct region's closure environment).
    Env(StackPtr),
}

/// A region's register file. The compiler only writes a register that is empty (every
/// value is either on its stack or consumed), so writing needs no free, and after a
/// region exits every register is empty again: only backtracking frees them.
struct Regs<const N: usize>(ManuallyDrop<[Value; N]>);

impl<const N: usize> Regs<N> {
    #[inline(always)]
    fn new() -> Regs<N> {
        Regs(ManuallyDrop::new([const { Value::Null }; N]))
    }

    #[inline(always)]
    fn get(&self, r: Reg) -> &Value {
        &self.0[r as usize & (N - 1)]
    }

    #[inline(always)]
    fn get_mut(&mut self, r: Reg) -> &mut Value {
        &mut self.0[r as usize & (N - 1)]
    }

    /// Writes an empty register.
    #[inline(always)]
    fn put(&mut self, r: Reg, v: Value) {
        let slot = self.get_mut(r);
        debug_assert!(slot.is_null(), "register {r} written while full");
        // (No free of what it held: it was null.)
        std::mem::forget(std::mem::replace(slot, v));
    }

    #[inline(always)]
    fn take(&mut self, r: Reg) -> Value {
        take(self.get_mut(r))
    }

    #[inline(always)]
    fn swap(&mut self, a: Reg, b: Reg) {
        self.0.swap(a as usize & (N - 1), b as usize & (N - 1));
    }

    /// After backtracking: frees what the registers hold, as unwinding jq's stack
    /// would.
    #[cold]
    fn clear(&mut self, r: &Region) {
        for slot in &mut self.0[..r.nregs as usize] {
            drop(take(slot));
        }
    }

    /// After the exit, when every register is empty.
    #[inline(always)]
    fn done(&self, r: &Region) {
        debug_assert!(
            self.0[..r.nregs as usize].iter().all(Value::is_null),
            "a register is full after the exit"
        );
    }
}

/// A forward jump waiting for its target: the op to patch and the state there.
struct Pending {
    target: usize,
    depth: usize,
    level: i32,
    op: usize,
}

/// The state at the last point where the region could end.
struct Clean {
    pc: usize,
    nops: usize,
    stack: Vec<Reg>,
    level: i32,
    live: bool,
    ninstr: u32,
}

/// Compiles one region (see [`compile`]).
struct Compiler<'a> {
    prog: &'a Program,
    func: u32,
    mode: Mode,
    ops: Vec<Op>,
    /// The data stack above the values the region found (registers, bottom to top).
    stack: Vec<Reg>,
    /// Free registers (bit `r`).
    free: u32,
    /// Registers ever allocated (bit `r`).
    used: u32,
    /// Whether an op reads or writes a variable of the current frame.
    vars0: bool,
    level: i32,
    /// Whether the current position is reachable (not after a jump or backtrack).
    live: bool,
    /// Values popped from the stack the region found (for a direct region, only its
    /// input, in register 0).
    entry: u32,
    pending: Vec<Pending>,
    /// Where the current straight-line run of ops starts (a join or jump target).
    block_start: usize,
}

impl<'a> Compiler<'a> {
    fn alloc(&mut self) -> Option<Reg> {
        // Prefer the register of the new stack slot, so fewer moves canonicalize.
        let want = self.stack.len();
        let r = if want < MAX_REGS && self.free & (1 << want) != 0 {
            want as u32
        } else {
            if self.free == 0 {
                return None;
            }
            self.free.trailing_zeros()
        };
        self.free &= !(1 << r);
        self.used |= 1 << r;
        Some(r as Reg)
    }

    fn release(&mut self, r: Reg) {
        debug_assert!(self.free & (1 << r) == 0);
        self.free |= 1 << r;
    }

    /// The index of the op that put a constant in `r`, if it is in the current
    /// straight-line run of ops and no op since has read or written `r`: its only
    /// reader is then the op about to consume it, which can read the constant itself
    /// (constants are immutable, so when the copy is made doesn't matter).
    fn const_def(&self, r: Reg) -> Option<usize> {
        for i in (self.block_start..self.ops.len()).rev() {
            match self.ops[i] {
                Op::Const { dst, .. } if dst == r => return Some(i),
                ref op if op_uses(op, r) => return None,
                _ => {}
            }
        }
        None
    }

    /// Takes the constant op at `i` (see [`Compiler::const_def`]) out of the ops.
    fn take_const(&mut self, i: usize) -> *const Value {
        match self.ops.remove(i) {
            Op::Const { k, .. } => k,
            _ => unreachable!("a constant op"),
        }
    }

    /// Frees the value in `r` (which stays allocated, now empty). A copy made just
    /// before is simply not made (`DUP; POP`, `SUBEXP_BEGIN; LOADV`): its reference
    /// count went up and down with nothing in between.
    fn drop_reg(&mut self, r: Reg) {
        if self.ops.len() > self.block_start
            && let Some(Op::Clone { dst, .. }) = self.ops.last()
            && *dst == r
        {
            self.ops.pop();
        } else {
            self.ops.push(Op::Drop { r });
        }
    }

    fn push(&mut self, r: Reg) {
        self.stack.push(r);
    }

    /// Pops a register, or the stack the region found (`n`: `stack_popn`).
    fn pop(&mut self, n: bool) -> Option<Reg> {
        if let Some(r) = self.stack.pop() {
            return Some(r);
        }
        let dst = match self.mode {
            Mode::Loop => {
                let dst = self.alloc()?;
                self.ops
                    .push(if n { Op::PopN { dst } } else { Op::Pop { dst } });
                dst
            }
            // The input, which jq pushed right above the new frame: it is moved out by
            // its first pop, either way. There is nothing below it. Its register was
            // reserved (it is set before the ops run).
            Mode::Direct if self.entry == 0 => 0,
            Mode::Direct => return None,
        };
        self.entry += 1;
        Some(dst)
    }

    /// Whether the state can be canonicalized: a direct region's input register is
    /// reserved until the input is popped.
    fn can_branch(&self) -> Option<()> {
        (self.mode == Mode::Loop || self.entry > 0).then_some(())
    }

    /// A variable reference, if the mode allows it.
    fn var(&mut self, level: u16) -> Option<()> {
        self.vars0 |= level == 0;
        (self.mode == Mode::Loop || level > 0).then_some(())
    }

    fn constant(&self, idx: u16) -> *const Value {
        self.prog.constant(|| self.func, idx) as *const Value
    }

    /// Puts stack slot `i` in register `i`, for a jump or join.
    fn canonicalize(&mut self) -> Option<()> {
        if self.stack.len() > MAX_REGS {
            return None;
        }
        for i in 0..self.stack.len() {
            let r = self.stack[i];
            let want = i as Reg;
            if r == want {
                continue;
            }
            if let Some(j) = self.stack.iter().position(|&x| x == want) {
                self.ops.push(Op::Swap { a: want, b: r });
                self.stack[i] = want;
                self.stack[j] = r;
            } else {
                debug_assert!(self.free & (1 << want) != 0);
                self.ops.push(Op::Move { dst: want, src: r });
                self.free &= !(1 << want);
                self.release(r);
                self.stack[i] = want;
            }
        }
        Some(())
    }

    /// The canonical state with `depth` stack slots.
    fn set_canonical(&mut self, depth: usize, level: i32) {
        self.stack = (0..depth as Reg).collect();
        self.free = ((1u64 << MAX_REGS) - 1) as u32 & !((1u64 << depth) - 1) as u32;
        self.level = level;
    }

    /// Joins the jumps to `pc` with the fall-through path. `None` if they disagree.
    fn join(&mut self, pc: usize) -> Option<()> {
        if !self.pending.iter().any(|p| p.target == pc) {
            return Some(());
        }
        self.can_branch()?;
        let here: Vec<Pending> = self.pending.extract_if(.., |p| p.target == pc).collect();
        if self.live {
            self.canonicalize()?;
        } else {
            self.set_canonical(here[0].depth, here[0].level);
            self.live = true;
        }
        let at = self.ops.len() as u32;
        for p in here {
            if p.depth != self.stack.len() || p.level != self.level {
                return None;
            }
            match &mut self.ops[p.op] {
                Op::JumpF { target, .. } | Op::Jump { target } => *target = at,
                _ => unreachable!("pending jump"),
            }
        }
        self.block_start = self.ops.len();
        Some(())
    }

    /// Records a jump to `target` from the op just pushed.
    fn jump_to(&mut self, target: usize) {
        self.pending.push(Pending {
            target,
            depth: self.stack.len(),
            level: self.level,
            op: self.ops.len() - 1,
        });
    }

    /// The function a `CALL_JQ`'s callee `(level, idx)` names statically, if it is a
    /// subfunction (not a closure parameter).
    fn static_callee(&self, level: u16, idx: u16) -> Option<u32> {
        if idx & ARG_NEWCLOSURE == 0 {
            return None;
        }
        let mut f = self.func;
        for _ in 0..level {
            f = self.prog.funcs[f as usize].parent;
            if f == u32::MAX {
                return None;
            }
        }
        self.prog.funcs[f as usize]
            .subfunctions
            .get((idx & !ARG_NEWCLOSURE) as usize)
            .copied()
    }

    /// Translates the instruction at `pc`. `None` if it can't be in a region.
    fn instr(&mut self, pc: usize) -> Option<()> {
        use Opcode::*;
        let code = &self.prog.code;
        let op = Opcode::from_u16(code[pc])?;
        let imm = |i: usize| code[pc + i];
        match op {
            TOP => {}
            LOADK => {
                let r = self.pop(false)?;
                self.drop_reg(r);
                let k = self.constant(imm(1));
                self.ops.push(Op::Const { dst: r, k });
                self.push(r);
            }
            DUP | SUBEXP_BEGIN => {
                let r = self.pop(false)?;
                let c = self.alloc()?;
                self.ops.push(Op::Clone { dst: c, src: r });
                self.push(r);
                self.push(c);
                if op == SUBEXP_BEGIN {
                    self.level += 1;
                }
            }
            DUPN => {
                let r = self.pop(true)?;
                let c = self.alloc()?;
                self.ops.push(Op::Clone { dst: c, src: r });
                self.push(c);
                self.push(r);
            }
            DUP2 => {
                let keep = self.pop(false)?;
                let v = self.pop(false)?;
                let c = self.alloc()?;
                self.ops.push(Op::Clone { dst: c, src: v });
                self.push(c);
                self.push(keep);
                self.push(v);
            }
            PUSHK_UNDER => {
                let r = self.pop(false)?;
                let c = self.alloc()?;
                let k = self.constant(imm(1));
                self.ops.push(Op::Const { dst: c, k });
                self.push(c);
                self.push(r);
            }
            POP => {
                let r = self.pop(false)?;
                self.drop_reg(r);
                self.release(r);
            }
            SUBEXP_END => {
                let a = self.pop(false)?;
                let b = self.pop(false)?;
                self.push(a);
                self.push(b);
                self.level -= 1;
            }
            LOADV | LOADVN => {
                let (level, idx) = (imm(1), imm(2));
                self.var(level)?;
                let r = self.pop(op == LOADVN)?;
                // (jq copies or moves the variable before freeing the value it
                // replaces; nothing can tell.)
                self.drop_reg(r);
                self.ops.push(if op == LOADV {
                    Op::LoadV { dst: r, level, idx }
                } else {
                    Op::LoadVN { dst: r, level, idx }
                });
                self.push(r);
            }
            STOREV | APPEND => {
                let (level, idx) = (imm(1), imm(2));
                self.var(level)?;
                let r = self.pop(false)?;
                self.ops.push(if op == STOREV {
                    Op::StoreV { src: r, level, idx }
                } else {
                    Op::Append { src: r, level, idx }
                });
                self.release(r);
            }
            STORE_GLOBAL => {
                let (level, idx) = (imm(2), imm(3));
                self.var(level)?;
                let k = self.constant(imm(1));
                self.ops.push(Op::StoreK { k, level, idx });
            }
            INDEX | INDEX_OPT => {
                let t = self.pop(false)?;
                let k = self.pop(false)?;
                let (opt, level) = (op == INDEX_OPT, self.level);
                let op = match self.const_def(k) {
                    // `.a`, `.[0]`: `PUSHK_UNDER k; INDEX`.
                    Some(i) => Op::IndexK {
                        dst: t,
                        t,
                        k: self.take_const(i),
                        opt,
                        level,
                    },
                    None => Op::Index {
                        dst: t,
                        t,
                        k,
                        opt,
                        level,
                    },
                };
                self.ops.push(op);
                self.release(k);
                self.push(t);
            }
            INSERT => {
                let stktop = self.pop(false)?;
                let v = self.pop(false)?;
                let k = self.pop(false)?;
                let obj = self.pop(false)?;
                self.ops.push(Op::Insert { obj, k, v });
                self.release(k);
                self.release(v);
                self.push(obj);
                self.push(stktop);
            }
            GENLABEL => {
                let r = self.alloc()?;
                self.ops.push(Op::GenLabel { dst: r });
                self.push(r);
            }
            CALL_BUILTIN => {
                let nargs = imm(1) as usize;
                let cf = imm(2);
                let f = self.prog.cfunctions.get(cf as usize)?;
                debug_assert_eq!(f.nargs, nargs);
                // halt/halt_error stop the program; getpath reads the path state.
                if matches!(f.name, "halt" | "halt_error" | "getpath") || !(1..=4).contains(&nargs)
                {
                    return None;
                }
                let input = self.pop(false)?;
                let mut args = [NO_REG; 3];
                for a in args.iter_mut().take(nargs - 1) {
                    *a = self.pop(false)?;
                }
                match BinKind::of(f.name).filter(|_| nargs == 3) {
                    Some(kind) => {
                        let (a, b) = (args[0], args[1]);
                        // `DUP` right before: its copy is freed first, unused.
                        let mut input = input;
                        if self.ops.len() > self.block_start
                            && let Some(Op::Clone { dst, src }) = self.ops.last()
                            && *dst == input
                            && (*src == a || *src == b)
                        {
                            self.ops.pop();
                            self.release(input);
                            input = NO_REG;
                        }
                        // A constant operand is read in place (`. + 1`: `PUSHK_UNDER 1;
                        // DUP; CALL_BUILTIN _plus`).
                        let (op, dst, other) = if let Some(i) = self.const_def(b) {
                            let k = self.take_const(i);
                            let op = Op::BinopK {
                                dst: a,
                                kind,
                                input,
                                r: a,
                                k,
                                kfirst: false,
                            };
                            (op, a, b)
                        } else if let Some(i) = self.const_def(a) {
                            let k = self.take_const(i);
                            let op = Op::BinopK {
                                dst: b,
                                kind,
                                input,
                                r: b,
                                k,
                                kfirst: true,
                            };
                            (op, b, a)
                        } else {
                            let op = Op::Binop {
                                dst: a,
                                kind,
                                input,
                                a,
                                b,
                            };
                            (op, a, b)
                        };
                        self.ops.push(op);
                        if input != NO_REG {
                            self.release(input);
                        }
                        self.release(other);
                        self.push(dst);
                    }
                    None => {
                        self.ops.push(Op::Call {
                            dst: input,
                            cf,
                            nargs: nargs as u8,
                            input,
                            args,
                        });
                        for &a in &args[..nargs - 1] {
                            self.release(a);
                        }
                        self.push(input);
                    }
                }
            }
            JUMP_F => {
                let target = pc + 2 + imm(1) as usize;
                let r = self.pop(false)?;
                self.can_branch()?;
                self.push(r);
                self.canonicalize()?;
                let r = *self.stack.last().expect("tested value");
                self.ops.push(Op::JumpF { r, target: 0 });
                self.jump_to(target);
                self.block_start = self.ops.len();
            }
            JUMP => {
                let target = pc + 2 + imm(1) as usize;
                self.can_branch()?;
                self.canonicalize()?;
                self.ops.push(Op::Jump { target: 0 });
                self.jump_to(target);
                self.live = false;
            }
            BACKTRACK => {
                self.ops.push(Op::Backtrack);
                self.live = false;
            }
            ERRORK => {
                let k = self.constant(imm(1));
                self.ops.push(Op::ErrorK { k });
                self.live = false;
            }
            CALL_JQ | TAIL_CALL_JQ => {
                // Only calls of a function that just backtracks (`empty`): the call
                // pops its input, then the callee's BACKTRACK unwinds everything
                // (a tail call's frame included) as this one does.
                let nclosures = imm(1);
                let callee = self.static_callee(imm(2), imm(3))?;
                let f = &self.prog.funcs[callee as usize];
                if nclosures != 0 || self.prog.code[f.base as usize] != BACKTRACK as u16 {
                    return None;
                }
                let r = self.pop(false)?;
                self.ops.push(Op::Drop { r });
                self.release(r);
                self.ops.push(Op::Backtrack);
                self.live = false;
            }
            _ => return None,
        }
        Some(())
    }

    /// The state at `pc` as a place the region may end.
    fn clean(&self, pc: usize, ninstr: u32) -> Clean {
        Clean {
            pc,
            nops: self.ops.len(),
            stack: self.stack.clone(),
            level: self.level,
            live: self.live,
            ninstr,
        }
    }
}

/// Compiles the longest region of function `func` starting at global pc `start`.
/// Returns it and the number of instructions it covers.
pub(super) fn compile(
    prog: &Program,
    func: u32,
    start: usize,
    mode: Mode,
) -> Option<(Region, u32)> {
    let f = &prog.funcs[func as usize];
    let end = f.base as usize + f.bc.code.len();
    let mut c = Compiler {
        prog,
        func,
        mode,
        ops: Vec::new(),
        stack: Vec::new(),
        // A direct region's input goes in register 0.
        free: ((1u64 << MAX_REGS) - 1) as u32 & if mode == Mode::Direct { !1 } else { !0 },
        used: if mode == Mode::Direct { 1 } else { 0 },
        vars0: false,
        level: 0,
        live: true,
        entry: 0,
        pending: Vec::new(),
        block_start: 0,
    };
    let mut pc = start;
    let mut ninstr = 0u32;
    let mut best: Option<Clean> = None;
    while pc < end && (pc - start) < MAX_SCAN {
        if c.join(pc).is_none() {
            break;
        }
        if c.pending.is_empty() && ninstr > 0 {
            best = Some(c.clean(pc, ninstr));
        }
        if !c.live {
            if c.pending.is_empty() {
                break;
            }
            // Unreachable from the region (something outside may jump here).
            pc += bytecode_operation_length(&prog.code[pc..end]).max(1);
            continue;
        }
        if c.instr(pc).is_none() {
            break;
        }
        ninstr += 1;
        pc += bytecode_operation_length(&prog.code[pc..end]).max(1);
    }
    if pc >= end && c.join(pc).is_some() && c.pending.is_empty() && ninstr > 0 {
        best = Some(c.clean(pc, ninstr));
    }
    let best = best?;
    let mut ops = c.ops;
    ops.truncate(best.nops);
    let exit = if best.live { best.stack } else { Vec::new() };
    if best.live {
        ops.push(Op::Exit);
    }
    if mode == Mode::Direct {
        // The whole body, up to its RET, taking the input and leaving the one result.
        let ret = end - 1;
        if best.pc != ret
            || prog.code[ret] != Opcode::RET as u16
            || c.entry != 1
            || (best.live && (exit.len() != 1 || best.level != 0))
        {
            return None;
        }
    }
    let may_backtrack = ops.iter().any(|op| {
        matches!(
            op,
            Op::Backtrack | Op::Index { opt: true, .. } | Op::IndexK { opt: true, .. }
        )
    });
    Some((
        Region {
            ops,
            exit,
            end: best.pc as u32,
            nest: best.level,
            nregs: (32 - c.used.leading_zeros()) as u8,
            vars0: c.vars0,
            may_backtrack,
        },
        best.ninstr,
    ))
}

/// The positions of `code[start..end]` (one function) that the interpreter can reach
/// other than by falling through: branch targets (`JUMP`, `JUMP_F`, and the second
/// branch of `FORK`, `TRY_BEGIN` and `DESTRUCTURE_ALT`) and return addresses (after
/// `CALL_JQ`).
fn entry_points(code: &[u16], start: usize, end: usize) -> std::collections::HashSet<usize> {
    use Opcode::*;
    let mut out = std::collections::HashSet::new();
    let mut pc = start;
    while pc < end {
        let len = bytecode_operation_length(&code[pc..end]).max(1);
        match Opcode::from_u16(code[pc]) {
            Some(JUMP | JUMP_F | FORK | TRY_BEGIN | DESTRUCTURE_ALT) => {
                out.insert(pc + 2 + code[pc + 1] as usize);
            }
            Some(CALL_JQ) => {
                out.insert(pc + len);
            }
            _ => {}
        }
        pc += len;
    }
    out
}

/// Builds `prog.regions`, `prog.fast_code` and each function's direct region.
pub(super) fn optimize(prog: &mut Program) {
    let mut regions = Vec::new();
    let mut direct = vec![None; prog.funcs.len()];
    for func in 0..prog.funcs.len() as u32 {
        let f = &prog.funcs[func as usize];
        if f.native.is_none()
            && f.nclosures == 0
            && regions.len() < (u16::MAX - REGION_BASE) as usize
            && let Some((r, _)) = compile(prog, func, f.base as usize, Mode::Direct)
        {
            direct[func as usize] = Some(regions.len() as u32);
            regions.push(r);
        }
    }
    let mut fast = prog.code.clone();
    for func in 0..prog.funcs.len() {
        let f = &prog.funcs[func];
        let (start, end) = (f.base as usize, f.base as usize + f.bc.code.len());
        let entries = entry_points(&prog.code, start, end);
        // Positions inside a region run only when something jumps or returns there
        // (the region runs its own copy of them otherwise).
        let mut covered = start;
        let mut pc = start;
        while pc < end {
            let len = bytecode_operation_length(&prog.code[pc..end]).max(1);
            if regions.len() >= (u16::MAX - REGION_BASE) as usize {
                break;
            }
            if (pc >= covered || entries.contains(&pc))
                && let Some((r, n)) = compile(prog, func as u32, pc, Mode::Loop)
                && n >= MIN_LOOP_INSTRS
            {
                covered = covered.max(r.end as usize);
                fast[pc] = REGION_BASE + regions.len() as u16;
                regions.push(r);
            }
            pc += len;
        }
    }
    for (f, d) in prog.funcs.iter_mut().zip(direct) {
        f.direct = d;
    }
    prog.fast_code = fast;
    prog.regions = regions;
}

/// The regions of `prog`, as text (for looking at what the compiler made).
#[cfg(test)]
pub(super) fn dump(prog: &Program) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for (i, f) in prog.funcs.iter().enumerate() {
        if let Some(d) = f.direct {
            let _ = writeln!(out, "func {i} (base {}): direct region {d}", f.base);
        }
    }
    for (pc, &op) in prog.fast_code.iter().enumerate() {
        if op >= REGION_BASE {
            let r = &prog.regions[(op - REGION_BASE) as usize];
            let _ = writeln!(out, "pc {pc}..{}: region {}", r.end, op - REGION_BASE);
        }
    }
    for (i, r) in prog.regions.iter().enumerate() {
        let _ = writeln!(
            out,
            "region {i}: end {} nest {} exit {:?} nregs {}",
            r.end, r.nest, r.exit, r.nregs
        );
        for op in &r.ops {
            let _ = writeln!(out, "    {op:?}");
        }
    }
    out
}

#[cfg(test)]
thread_local! {
    /// Regions run by the interpreter loop on this thread (tests check they run).
    pub(super) static REGION_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Direct regions run (frameless calls, natives' closures) on this thread.
    pub(super) static DIRECT_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether regions are disabled for the whole process: by `QJ_NO_VM_OPT=1` (for A/B
/// checks against the original code), and in compat mode (`QJ_JQ_COMPAT`), which
/// models jq's C stack and so runs jq's instructions as they are.
pub(super) fn disabled_by_env() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        std::env::var_os("QJ_NO_VM_OPT").is_some_and(|v| v == "1") || crate::compat::exactly_jq()
    })
}

impl Jq {
    /// Enables or disables the optimized code for this interpreter (on by default,
    /// unless `QJ_NO_VM_OPT=1`): regions, and frameless calls of region bodies.
    pub fn set_optimize(&mut self, on: bool) {
        self.opt = on;
    }

    /// Runs region `r` (compiled for the interpreter loop) on the current stack.
    /// `true`: continue at `r.end`; `false`: backtrack (with `self.error` set if an
    /// error was raised).
    #[inline(never)]
    pub(super) fn run_region(&mut self, prog: &Program, r: &Region) -> bool {
        #[cfg(test)]
        REGION_RUNS.with(|c| c.set(c.get() + 1));
        // (Most regions need few registers: a small file is quicker to set up.)
        if r.nregs as usize <= SMALL_REGS {
            self.run_region_with(prog, r, &mut Regs::<SMALL_REGS>::new())
        } else {
            self.run_region_with(prog, r, &mut Regs::<MAX_REGS>::new())
        }
    }

    #[inline(always)]
    fn run_region_with<const N: usize>(
        &mut self,
        prog: &Program,
        r: &Region,
        regs: &mut Regs<N>,
    ) -> bool {
        let vars = Vars::Frame(self.curr_frame);
        if !self.exec_ops(prog, r, regs, vars) {
            regs.clear(r);
            return false;
        }
        for &x in &r.exit {
            let v = regs.take(x);
            self.push(v);
        }
        self.subexp_nest += r.nest;
        regs.done(r);
        true
    }

    /// Runs direct region `r` of a closure over `env` on `input`: its output, or
    /// `None` if it backtracked (with `self.error` set if an error was raised).
    #[inline(never)]
    pub(super) fn run_direct(
        &mut self,
        prog: &Program,
        r: &Region,
        env: StackPtr,
        input: Value,
    ) -> Option<Value> {
        #[cfg(test)]
        DIRECT_RUNS.with(|c| c.set(c.get() + 1));
        if r.nregs as usize <= SMALL_REGS {
            self.run_direct_with(prog, r, env, input, &mut Regs::<SMALL_REGS>::new())
        } else {
            self.run_direct_with(prog, r, env, input, &mut Regs::<MAX_REGS>::new())
        }
    }

    #[inline(always)]
    fn run_direct_with<const N: usize>(
        &mut self,
        prog: &Program,
        r: &Region,
        env: StackPtr,
        input: Value,
        regs: &mut Regs<N>,
    ) -> Option<Value> {
        regs.put(0, input);
        if !self.exec_ops(prog, r, regs, Vars::Env(env)) {
            regs.clear(r);
            return None;
        }
        let v = regs.take(r.exit[0]);
        regs.done(r);
        Some(v)
    }
    /// The local slot of variable `idx` at `level`; `base0` is the current frame's
    /// first local (when `vars` is a frame).
    #[inline(always)]
    fn region_var(&self, vars: Vars, base0: usize, level: u16, idx: u16) -> usize {
        let (mut fr, hops) = match vars {
            Vars::Frame(_) if level == 0 => return base0 + idx as usize,
            Vars::Frame(f) => (f, level),
            Vars::Env(e) => (e, level - 1),
        };
        for _ in 0..hops {
            fr = self.stk.frame(fr).env;
        }
        self.stk.frame(fr).locals as usize + idx as usize
    }

    /// The ops of a region. `false`: backtrack.
    #[inline(always)]
    fn exec_ops<const N: usize>(
        &mut self,
        prog: &Program,
        r: &Region,
        regs: &mut Regs<N>,
        vars: Vars,
    ) -> bool {
        let base0 = match vars {
            Vars::Frame(f) if r.vars0 => self.stk.frame(f).locals as usize,
            _ => 0,
        };
        let ops = &r.ops[..];
        let mut i = 0usize;
        loop {
            match ops[i] {
                Op::Pop { dst } => {
                    let v = self.pop();
                    regs.put(dst, v);
                }
                Op::PopN { dst } => {
                    let v = self.popn();
                    regs.put(dst, v);
                }
                Op::Clone { dst, src } => {
                    let v = regs.get(src).clone();
                    regs.put(dst, v);
                }
                Op::Move { dst, src } => {
                    let v = regs.take(src);
                    regs.put(dst, v);
                }
                Op::Swap { a, b } => regs.swap(a, b),
                Op::Drop { r } => discard(regs.take(r)),
                Op::Const { dst, k } => {
                    // SAFETY: `k` points into a constant pool of `prog` (see
                    // `Compiler::constant`), which lives unchanged as long as `prog`.
                    let v = unsafe { &*k }.clone();
                    regs.put(dst, v);
                }
                Op::LoadV { dst, level, idx } => {
                    let var = self.region_var(vars, base0, level, idx);
                    let v = self.stk.locals[var].clone();
                    regs.put(dst, v);
                }
                Op::LoadVN { dst, level, idx } => {
                    let var = self.region_var(vars, base0, level, idx);
                    let v = take(&mut self.stk.locals[var]);
                    regs.put(dst, v);
                }
                Op::StoreV { src, level, idx } => {
                    let v = regs.take(src);
                    let var = self.region_var(vars, base0, level, idx);
                    self.stk.locals[var] = v;
                }
                Op::StoreK { k, level, idx } => {
                    // SAFETY: as for `Op::Const`.
                    let v = unsafe { &*k }.clone();
                    let var = self.region_var(vars, base0, level, idx);
                    self.stk.locals[var] = v;
                }
                Op::Append { src, level, idx } => {
                    let v = regs.take(src);
                    let var = self.region_var(vars, base0, level, idx);
                    match &mut self.stk.locals[var] {
                        Value::Array(a) => a.push(v),
                        _ => unreachable!("APPEND to a non-array"),
                    }
                }
                Op::Call {
                    dst,
                    cf,
                    nargs,
                    input,
                    args,
                } => {
                    let f = prog.cfunctions[cf as usize];
                    let x = regs.take(input);
                    let res = match nargs {
                        1 => (f.f)(self, x, &mut []),
                        2 => {
                            let mut a = [regs.take(args[0])];
                            (f.f)(self, x, &mut a)
                        }
                        3 => {
                            let mut a = [regs.take(args[0]), regs.take(args[1])];
                            (f.f)(self, x, &mut a)
                        }
                        _ => {
                            let mut a =
                                [regs.take(args[0]), regs.take(args[1]), regs.take(args[2])];
                            (f.f)(self, x, &mut a)
                        }
                    };
                    match res {
                        Ok(v) => regs.put(dst, v),
                        Err(e) => {
                            self.set_error(e.into_value());
                            return false;
                        }
                    }
                }
                Op::Binop {
                    dst,
                    kind,
                    input,
                    a,
                    b,
                } => {
                    if input != NO_REG {
                        discard(regs.take(input));
                    }
                    let av = regs.take(a);
                    let bv = regs.take(b);
                    match binop(kind, av, bv) {
                        Ok(v) => regs.put(dst, v),
                        Err(e) => {
                            self.set_error(e.into_value());
                            return false;
                        }
                    }
                }
                Op::BinopK {
                    dst,
                    kind,
                    input,
                    r,
                    k,
                    kfirst,
                } => {
                    if input != NO_REG {
                        discard(regs.take(input));
                    }
                    // SAFETY: as for `Op::Const`.
                    let k = unsafe { &*k };
                    let rv = regs.take(r);
                    let fast = if kfirst {
                        binop_numbers(kind, k, &rv)
                    } else {
                        binop_numbers(kind, &rv, k)
                    };
                    let res = match fast {
                        Some(v) => {
                            discard(rv);
                            Ok(v)
                        }
                        None if kfirst => binop(kind, k.clone(), rv),
                        None => binop(kind, rv, k.clone()),
                    };
                    match res {
                        Ok(v) => regs.put(dst, v),
                        Err(e) => {
                            self.set_error(e.into_value());
                            return false;
                        }
                    }
                }
                Op::Index {
                    dst,
                    t,
                    k,
                    opt,
                    level,
                } => {
                    let tv = regs.take(t);
                    let kv = regs.take(k);
                    match self.region_index(tv, kv, opt, self.subexp_nest + level) {
                        Some(v) => regs.put(dst, v),
                        None => return false,
                    }
                }
                Op::IndexK {
                    dst,
                    t,
                    k,
                    opt,
                    level,
                } => {
                    let tv = regs.take(t);
                    // SAFETY: as for `Op::Const`.
                    let k = unsafe { &*k };
                    match self.region_index_k(tv, k, opt, self.subexp_nest + level) {
                        Some(v) => regs.put(dst, v),
                        None => return false,
                    }
                }
                Op::Insert { obj, k, v } => {
                    let kv = regs.take(k);
                    let vv = regs.take(v);
                    match (regs.get_mut(obj), kv) {
                        (Value::Object(o), Value::String(k)) => o.insert(k, vv),
                        (objv, k) => {
                            debug_assert!(matches!(objv, Value::Object(_)));
                            let msg = format!(
                                "Cannot use {} ({}) as object key",
                                k.kind_name(),
                                dump_string_trunc(&k, 15)
                            );
                            self.set_error(Value::from(msg));
                            return false;
                        }
                    }
                }
                Op::GenLabel { dst } => {
                    let label = label_object(self.next_label);
                    self.next_label = self.next_label.wrapping_add(1);
                    regs.put(dst, label);
                }
                Op::JumpF { r, target } => {
                    if !regs.get(r).is_truthy() {
                        i = target as usize;
                        continue;
                    }
                }
                Op::Jump { target } => {
                    i = target as usize;
                    continue;
                }
                Op::Backtrack => return false,
                Op::ErrorK { k } => {
                    // SAFETY: as for `Op::Const`.
                    let v = unsafe { &*k }.clone();
                    self.set_error(v);
                    return false;
                }
                Op::Exit => return true,
            }
            i += 1;
        }
    }
    /// [`Jq::region_index`] with a constant key: the copy that goes in the tracked path
    /// is made there (jq's copy was made by `PUSHK_UNDER`).
    fn region_index_k(&mut self, t: Value, k: &Value, opt: bool, nest: i32) -> Option<Value> {
        let tracking = nest == 0 && matches!(self.path, Value::Array(_));
        // path_intact: detect invalid path expression like path(reverse | .a)
        if tracking && !t.identical(&self.value_at_path) {
            let msg = format!(
                "Invalid path expression near attempt to access element {} of {}",
                dump_string_trunc(k, 15),
                dump_string_trunc(&t, 30)
            );
            self.set_error(Value::from(msg));
            return None;
        }
        // jv_get(t, jv_copy(k)): t is consumed.
        let r = t.get(k);
        drop(t);
        match r {
            Ok(v) => {
                // path_append
                if tracking && let Value::Array(p) = &mut self.path {
                    p.push(k.clone());
                    self.value_at_path = v.clone();
                }
                Some(v)
            }
            Err(e) => {
                if !opt {
                    self.set_error(e.into_value());
                }
                None
            }
        }
    }

    /// `INDEX`/`INDEX_OPT` with `subexp_nest` equal to `nest`: `t[k]`, or `None` to
    /// backtrack (with the error set, except for `INDEX_OPT`'s own errors).
    fn region_index(&mut self, t: Value, k: Value, opt: bool, nest: i32) -> Option<Value> {
        let tracking = nest == 0 && matches!(self.path, Value::Array(_));
        // path_intact: detect invalid path expression like path(reverse | .a)
        if tracking && !t.identical(&self.value_at_path) {
            let msg = format!(
                "Invalid path expression near attempt to access element {} of {}",
                dump_string_trunc(&k, 15),
                dump_string_trunc(&t, 30)
            );
            self.set_error(Value::from(msg));
            return None;
        }
        // jv_get(t, jv_copy(k)): t is consumed.
        let r = t.get(&k);
        drop(t);
        match r {
            Ok(v) => {
                // path_append
                if tracking && let Value::Array(p) = &mut self.path {
                    p.push(k);
                    self.value_at_path = v.clone();
                }
                Some(v)
            }
            Err(e) => {
                drop(k);
                if !opt {
                    self.set_error(e.into_value());
                }
                None
            }
        }
    }
}
