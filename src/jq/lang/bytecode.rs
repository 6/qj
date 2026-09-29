//! Compiled jq programs: port of jq 1.8.1's `opcode_list.h`, `bytecode.h` and
//! `bytecode.c` (the disassembler), plus the tail-call pass of `execute.c`
//! (`optimize`), which `jq_compile_args` applies before anything runs.
//!
//! This is the contract between the compiler ([`super::compile`]) and the VM:
//! [`Bytecode::code`] uses jq's exact encoding, so `execute.c` ports directly.
//!
//! * Each instruction is an opcode ([`Opcode`] as `u16`, numbered in
//!   `opcode_list.h` order) followed by its immediates; [`OpcodeDescription::length`]
//!   is the total in 16-bit units. `CALL_JQ`/`TAIL_CALL_JQ` are longer:
//!   `CALL_JQ nclosures (level, idx) (level, idx)*nclosures`, the first pair being the
//!   callee, see [`bytecode_operation_length`].
//! * `CALL_JQ` closure indices: `idx | ARG_NEWCLOSURE` is subfunction `idx` of the
//!   bytecode `level` frames up the lexical chain; plain `idx` is that frame's closure
//!   parameter `idx`.
//! * Variables (`LOADV`, `STOREV`, ...): `level, index` into the local frame `level`
//!   frames up. Constants (`LOADK`, ...): an index into [`Bytecode::constants`].
//!   Branches: an offset relative to the pc after the immediate (forward only).
//!   `CALL_BUILTIN nargs cfunc`: `nargs` includes the input; `cfunc` indexes the
//!   program-wide [`SymbolTable::cfunctions`].
//! * Zero-length opcodes (`CLOSURE_CREATE`, `CLOSURE_PARAM`, ...) exist only in the
//!   compiler's IR and never appear in `code`.
//!
//! [`dump_disassembly`] prints exactly what `jq --debug-dump-disasm` prints (before its
//! trailing blank line), and [`dump_operation`] is the per-instruction line used by
//! `--debug-trace`.

use std::cell::OnceCell;
use std::fmt::Write as _;
use std::rc::{Rc, Weak};

use crate::jq::builtins::CFunction;
use crate::jq::value::{DumpOptions, Value, dump_string};

/// jq's opcodes, in `opcode_list.h` order (the discriminant is the encoded value).
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum Opcode {
    LOADK = 0,
    DUP,
    DUPN,
    DUP2,
    PUSHK_UNDER,
    POP,
    LOADV,
    LOADVN,
    STOREV,
    STORE_GLOBAL,
    INDEX,
    INDEX_OPT,
    EACH,
    EACH_OPT,
    FORK,
    TRY_BEGIN,
    TRY_END,
    JUMP,
    JUMP_F,
    BACKTRACK,
    APPEND,
    INSERT,
    RANGE,
    SUBEXP_BEGIN,
    SUBEXP_END,
    PATH_BEGIN,
    PATH_END,
    CALL_BUILTIN,
    CALL_JQ,
    RET,
    TAIL_CALL_JQ,
    CLOSURE_PARAM,
    CLOSURE_REF,
    CLOSURE_CREATE,
    CLOSURE_CREATE_C,
    TOP,
    CLOSURE_PARAM_REGULAR,
    DEPS,
    MODULEMETA,
    GENLABEL,
    DESTRUCTURE_ALT,
    STOREVN,
    ERRORK,
}

/// `NUM_OPCODES`.
pub const NUM_OPCODES: usize = 43;

/// Every opcode, indexed by its encoding.
pub const ALL_OPCODES: [Opcode; NUM_OPCODES] = {
    use Opcode::*;
    [
        LOADK,
        DUP,
        DUPN,
        DUP2,
        PUSHK_UNDER,
        POP,
        LOADV,
        LOADVN,
        STOREV,
        STORE_GLOBAL,
        INDEX,
        INDEX_OPT,
        EACH,
        EACH_OPT,
        FORK,
        TRY_BEGIN,
        TRY_END,
        JUMP,
        JUMP_F,
        BACKTRACK,
        APPEND,
        INSERT,
        RANGE,
        SUBEXP_BEGIN,
        SUBEXP_END,
        PATH_BEGIN,
        PATH_END,
        CALL_BUILTIN,
        CALL_JQ,
        RET,
        TAIL_CALL_JQ,
        CLOSURE_PARAM,
        CLOSURE_REF,
        CLOSURE_CREATE,
        CLOSURE_CREATE_C,
        TOP,
        CLOSURE_PARAM_REGULAR,
        DEPS,
        MODULEMETA,
        GENLABEL,
        DESTRUCTURE_ALT,
        STOREVN,
        ERRORK,
    ]
};

impl Opcode {
    /// Decode an opcode word; `None` for jq's `#INVALID`.
    #[inline]
    pub fn from_u16(v: u16) -> Option<Opcode> {
        ALL_OPCODES.get(v as usize).copied()
    }

    /// `opcode_describe(op)`.
    #[inline]
    pub fn describe(self) -> &'static OpcodeDescription {
        &OPCODE_DESCRIPTIONS[self as usize]
    }

    /// The opcode's name as jq prints it.
    #[inline]
    pub fn name(self) -> &'static str {
        self.describe().name
    }
}

// bytecode.h flags.
pub const OP_HAS_CONSTANT: u32 = 2;
pub const OP_HAS_VARIABLE: u32 = 4;
pub const OP_HAS_BRANCH: u32 = 8;
pub const OP_HAS_CFUNC: u32 = 32;
pub const OP_HAS_UFUNC: u32 = 64;
pub const OP_IS_CALL_PSEUDO: u32 = 128;
pub const OP_HAS_BINDING: u32 = 1024;
/// Not part of any op: a pseudo-op flag for special handling of `break`.
pub const OP_BIND_WILDCARD: u32 = 2048;

/// `MAX_CFUNCTION_ARGS`: C builtins take at most 4 arguments, input included.
pub const MAX_CFUNCTION_ARGS: usize = 4;

/// `ARG_NEWCLOSURE`: marks a `CALL_JQ` closure index as a subfunction (not a param).
pub const ARG_NEWCLOSURE: u16 = 0x1000;

/// `struct opcode_description`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpcodeDescription {
    /// `None` only for jq's `#INVALID` description.
    pub op: Option<Opcode>,
    pub name: &'static str,
    pub flags: u32,
    /// Length in 16-bit units, including the opcode itself.
    pub length: usize,
    pub stack_in: i32,
    pub stack_out: i32,
}

// bytecode.c's immediate kinds: (flags, length).
const NONE: (u32, usize) = (0, 1);
const CONSTANT: (u32, usize) = (OP_HAS_CONSTANT, 2);
const VARIABLE: (u32, usize) = (OP_HAS_VARIABLE | OP_HAS_BINDING, 3);
const GLOBAL: (u32, usize) = (
    OP_HAS_CONSTANT | OP_HAS_VARIABLE | OP_HAS_BINDING | OP_IS_CALL_PSEUDO,
    4,
);
const BRANCH: (u32, usize) = (OP_HAS_BRANCH, 2);
const CFUNC: (u32, usize) = (OP_HAS_CFUNC | OP_HAS_BINDING, 3);
const UFUNC: (u32, usize) = (OP_HAS_UFUNC | OP_HAS_BINDING | OP_IS_CALL_PSEUDO, 4);
const DEFINITION: (u32, usize) = (OP_IS_CALL_PSEUDO | OP_HAS_BINDING, 0);
const CLOSURE_REF_IMM: (u32, usize) = (OP_IS_CALL_PSEUDO | OP_HAS_BINDING, 2);

const fn op(
    op: Opcode,
    name: &'static str,
    imm: (u32, usize),
    stack_in: i32,
    stack_out: i32,
) -> OpcodeDescription {
    OpcodeDescription {
        op: Some(op),
        name,
        flags: imm.0,
        length: imm.1,
        stack_in,
        stack_out,
    }
}

/// `opcode_descriptions[]` (opcode_list.h).
#[rustfmt::skip]
static OPCODE_DESCRIPTIONS: [OpcodeDescription; NUM_OPCODES] = {
    use Opcode::*;
    [
        op(LOADK, "LOADK", CONSTANT, 1, 1),
        op(DUP, "DUP", NONE, 1, 2),
        op(DUPN, "DUPN", NONE, 1, 2),
        op(DUP2, "DUP2", NONE, 2, 3),
        op(PUSHK_UNDER, "PUSHK_UNDER", CONSTANT, 1, 2),
        op(POP, "POP", NONE, 1, 0),
        op(LOADV, "LOADV", VARIABLE, 1, 1),
        op(LOADVN, "LOADVN", VARIABLE, 1, 1),
        op(STOREV, "STOREV", VARIABLE, 1, 0),
        op(STORE_GLOBAL, "STORE_GLOBAL", GLOBAL, 0, 0),
        op(INDEX, "INDEX", NONE, 2, 1),
        op(INDEX_OPT, "INDEX_OPT", NONE, 2, 1),
        op(EACH, "EACH", NONE, 1, 1),
        op(EACH_OPT, "EACH_OPT", NONE, 1, 1),
        op(FORK, "FORK", BRANCH, 0, 0),
        op(TRY_BEGIN, "TRY_BEGIN", BRANCH, 0, 0),
        op(TRY_END, "TRY_END", NONE, 0, 0),
        op(JUMP, "JUMP", BRANCH, 0, 0),
        op(JUMP_F, "JUMP_F", BRANCH, 1, 0),
        op(BACKTRACK, "BACKTRACK", NONE, 0, 0),
        op(APPEND, "APPEND", VARIABLE, 1, 0),
        op(INSERT, "INSERT", NONE, 4, 2),
        op(RANGE, "RANGE", VARIABLE, 1, 1),
        op(SUBEXP_BEGIN, "SUBEXP_BEGIN", NONE, 1, 2),
        op(SUBEXP_END, "SUBEXP_END", NONE, 2, 2),
        op(PATH_BEGIN, "PATH_BEGIN", NONE, 1, 2),
        op(PATH_END, "PATH_END", NONE, 2, 1),
        op(CALL_BUILTIN, "CALL_BUILTIN", CFUNC, -1, 1),
        op(CALL_JQ, "CALL_JQ", UFUNC, 1, 1),
        op(RET, "RET", NONE, 1, 1),
        op(TAIL_CALL_JQ, "TAIL_CALL_JQ", UFUNC, 1, 1),
        op(CLOSURE_PARAM, "CLOSURE_PARAM", DEFINITION, 0, 0),
        op(CLOSURE_REF, "CLOSURE_REF", CLOSURE_REF_IMM, 0, 0),
        op(CLOSURE_CREATE, "CLOSURE_CREATE", DEFINITION, 0, 0),
        op(CLOSURE_CREATE_C, "CLOSURE_CREATE_C", DEFINITION, 0, 0),
        op(TOP, "TOP", NONE, 0, 0),
        op(CLOSURE_PARAM_REGULAR, "CLOSURE_PARAM_REGULAR", DEFINITION, 0, 0),
        op(DEPS, "DEPS", CONSTANT, 0, 0),
        op(MODULEMETA, "MODULEMETA", CONSTANT, 0, 0),
        op(GENLABEL, "GENLABEL", NONE, 0, 1),
        op(DESTRUCTURE_ALT, "DESTRUCTURE_ALT", BRANCH, 0, 0),
        op(STOREVN, "STOREVN", VARIABLE, 1, 0),
        op(ERRORK, "ERRORK", CONSTANT, 1, 0),
    ]
};

/// bytecode.c `invalid_opcode_description`.
pub static INVALID_OPCODE_DESCRIPTION: OpcodeDescription = OpcodeDescription {
    op: None,
    name: "#INVALID",
    flags: 0,
    length: 0,
    stack_in: 0,
    stack_out: 0,
};

/// `opcode_describe` for a raw opcode word (`#INVALID` when out of range).
#[inline]
pub fn opcode_describe(op: u16) -> &'static OpcodeDescription {
    OPCODE_DESCRIPTIONS
        .get(op as usize)
        .unwrap_or(&INVALID_OPCODE_DESCRIPTION)
}

/// `bytecode_operation_length(codeptr)`: the length of the instruction at the start of
/// `code`, in 16-bit units.
#[inline]
pub fn bytecode_operation_length(code: &[u16]) -> usize {
    let mut length = opcode_describe(code[0]).length;
    if code[0] == Opcode::CALL_JQ as u16 || code[0] == Opcode::TAIL_CALL_JQ as u16 {
        length += code[1] as usize * 2;
    }
    length
}

/// `struct symbol_table`: the C builtins the program uses, shared by every function
/// of one compiled program. `CALL_BUILTIN`'s second immediate indexes `cfunctions`,
/// in the order the compiler met them (only referenced builtins are included).
#[derive(Clone, Default)]
pub struct SymbolTable {
    pub cfunctions: Vec<CFunction>,
}

impl SymbolTable {
    /// `cfunc_names[i]`.
    pub fn cfunc_name(&self, i: usize) -> &'static str {
        self.cfunctions[i].name
    }
}

impl std::fmt::Debug for SymbolTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.cfunctions.iter().map(|c| (c.name, c.nargs)))
            .finish()
    }
}

/// `bc->debuginfo`: `{"name": ..., "params": [...], "locals": [...]}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugInfo {
    /// The function's name (`@lambda` for closures built from arguments); `None`
    /// (jq's `null`) for the top level.
    pub name: Option<String>,
    /// Closure parameter names, one per closure (`nclosures` of them). Empty at the
    /// top level.
    pub params: Vec<String>,
    /// Local variable names, by frame index (without `$`).
    pub locals: Vec<String>,
}

/// `struct bytecode`: one compiled function (the top-level program or a subfunction).
pub struct Bytecode {
    /// The instructions; `codelen` is `code.len()`.
    pub code: Vec<u16>,
    /// Number of local variable slots the frame needs (jq's `maxvar + 2`).
    pub nlocals: usize,
    /// Number of closure parameters.
    pub nclosures: usize,
    /// The constant pool (`LOADK`, `PUSHK_UNDER`, `ERRORK`, `STORE_GLOBAL`).
    pub constants: Vec<Value>,
    /// The program-wide C builtin table.
    pub globals: Rc<SymbolTable>,
    /// Functions defined in this one (`CALL_JQ ... idx|ARG_NEWCLOSURE`).
    pub subfunctions: Vec<Rc<Bytecode>>,
    /// The lexically enclosing function ([`Bytecode::parent`]), set by
    /// [`link_parents`]; unset for the top level.
    parent: OnceCell<Weak<Bytecode>>,
    pub debuginfo: DebugInfo,
    /// Not jq's: for a builtin.jq definition that has a native implementation, its
    /// [`NativeId`](crate::jq::builtins::native::NativeId) + 1, else 0. The VM may run
    /// the native instead of `code` (see `execute/native.rs`); nothing else changes.
    pub native: u16,
}

impl std::fmt::Debug for Bytecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&dump_disassembly(0, self))
    }
}

/// Dropping is iterative: functions nest as deeply as jq's parser allows (thousands
/// of levels), which a recursive drop could overflow on a small thread stack.
impl Drop for Bytecode {
    fn drop(&mut self) {
        let mut stack = std::mem::take(&mut self.subfunctions);
        while let Some(child) = stack.pop() {
            if let Ok(mut bc) = Rc::try_unwrap(child) {
                stack.append(&mut bc.subfunctions);
            }
        }
    }
}

impl Bytecode {
    /// `codelen`.
    #[inline]
    pub fn codelen(&self) -> usize {
        self.code.len()
    }

    /// A function whose parent is set later by [`link_parents`].
    pub fn new(
        code: Vec<u16>,
        nlocals: usize,
        nclosures: usize,
        constants: Vec<Value>,
        globals: Rc<SymbolTable>,
        subfunctions: Vec<Rc<Bytecode>>,
        debuginfo: DebugInfo,
    ) -> Bytecode {
        Bytecode {
            code,
            nlocals,
            nclosures,
            constants,
            globals,
            subfunctions,
            parent: OnceCell::new(),
            debuginfo,
            native: 0,
        }
    }

    /// `bc->parent`: the lexically enclosing function, `None` for the top level (or
    /// once the tree is gone).
    pub fn parent(&self) -> Option<Rc<Bytecode>> {
        self.parent.get()?.upgrade()
    }

    /// The function's name for the disassembly (`null` at the top level).
    fn name_str(&self) -> &str {
        self.debuginfo.name.as_deref().unwrap_or("null")
    }
}

/// Points each function's [`Bytecode::parent`] at the function it is defined in, for
/// the whole tree under `root` (iteratively: functions nest thousands deep).
pub fn link_parents(root: &Rc<Bytecode>) {
    let mut stack = vec![root.clone()];
    while let Some(bc) = stack.pop() {
        for sub in &bc.subfunctions {
            let _ = sub.parent.set(Rc::downgrade(&bc));
            stack.push(sub.clone());
        }
    }
}

/// bytecode.c `getlevel`: the bytecode `level` steps up the lexical chain.
fn getlevel(bc: &Bytecode, level: u16) -> Option<Rc<Bytecode>> {
    let mut cur = bc.parent()?;
    for _ in 1..level {
        let next = cur.parent()?;
        cur = next;
    }
    Some(cur)
}

/// Runs `f` on the bytecode `level` steps up the lexical chain (`bc` itself for 0).
fn with_level<R>(bc: &Bytecode, level: u16, f: impl FnOnce(&Bytecode) -> R) -> Option<R> {
    if level == 0 {
        Some(f(bc))
    } else {
        getlevel(bc, level).map(|b| f(&b))
    }
}

fn indent_str(out: &mut String, indent: usize) {
    out.extend(std::iter::repeat_n(' ', indent));
}

/// `dump_code`.
fn dump_code(out: &mut String, indent: usize, bc: &Bytecode) {
    let mut pc = 0;
    while pc < bc.code.len() {
        indent_str(out, indent);
        write_operation(out, bc, pc);
        out.push('\n');
        let len = bytecode_operation_length(&bc.code[pc..]);
        if len == 0 {
            break; // #INVALID: jq would loop forever
        }
        pc += len;
    }
}

/// `dump_disassembly(indent, bc)`: what `jq --debug-dump-disasm` prints (jq's `main`
/// then prints one more `\n`).
pub fn dump_disassembly(indent: usize, bc: &Bytecode) -> String {
    let mut out = String::new();
    write_disassembly(&mut out, indent, bc);
    out
}

/// bytecode.c's recursive `dump_disassembly`, with an explicit stack.
fn write_disassembly(out: &mut String, indent: usize, bc: &Bytecode) {
    write_function(out, indent, bc);
    // (function, its indent, next subfunction to print)
    let mut stack: Vec<(&Bytecode, usize, usize)> = vec![(bc, indent, 0)];
    while let Some(top) = stack.last_mut() {
        let (b, ind, i) = *top;
        let Some(subfn) = b.subfunctions.get(i) else {
            stack.pop();
            continue;
        };
        top.2 += 1;
        indent_str(out, ind);
        let _ = writeln!(out, "{}:{}:", subfn.name_str(), i);
        write_function(out, ind + 2, subfn);
        stack.push((subfn, ind + 2, 0));
    }
}

/// A function's parameters and code (without its subfunctions).
fn write_function(out: &mut String, indent: usize, bc: &Bytecode) {
    if bc.nclosures > 0 {
        indent_str(out, indent);
        out.push_str("[params: ");
        for i in 0..bc.nclosures {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(bc.debuginfo.params.get(i).map_or("", |s| s.as_str()));
        }
        out.push_str("]\n");
    }
    dump_code(out, indent, bc);
}

/// `dump_operation(bc, codeptr)`: one instruction (at `pc`), without a newline, e.g.
/// `0003 CALL_JQ f:0 @lambda:1^1`.
pub fn dump_operation(bc: &Bytecode, pc: usize) -> String {
    let mut out = String::new();
    write_operation(&mut out, bc, pc);
    out
}

fn write_operation(out: &mut String, bc: &Bytecode, pc: usize) {
    let code = &bc.code;
    let _ = write!(out, "{pc:04} ");
    let mut pc = pc;
    let op = opcode_describe(code[pc]);
    pc += 1;
    out.push_str(op.name);
    if op.length <= 1 {
        return;
    }
    let imm = code[pc];
    pc += 1;
    match op.op {
        Some(Opcode::CALL_JQ | Opcode::TAIL_CALL_JQ) => {
            for _ in 0..imm as usize + 1 {
                let level = code[pc];
                let mut idx = code[pc + 1];
                pc += 2;
                let name = if idx & ARG_NEWCLOSURE != 0 {
                    idx &= !ARG_NEWCLOSURE;
                    with_level(bc, level, |b| {
                        b.subfunctions
                            .get(idx as usize)
                            .map(|s| s.name_str().to_string())
                    })
                } else {
                    with_level(bc, level, |b| b.debuginfo.params.get(idx as usize).cloned())
                };
                let name = name.flatten().unwrap_or_default();
                let _ = write!(out, " {name}:{idx}");
                if level != 0 {
                    let _ = write!(out, "^{level}");
                }
            }
        }
        Some(Opcode::CALL_BUILTIN) => {
            let func = code[pc] as usize;
            let name = bc.globals.cfunctions.get(func).map_or("", |c| c.name);
            let _ = write!(out, " {name}");
        }
        _ if op.flags & OP_HAS_BRANCH != 0 => {
            let _ = write!(out, " {:04}", pc + imm as usize);
        }
        _ if op.flags & OP_HAS_CONSTANT != 0 => {
            out.push(' ');
            match bc.constants.get(imm as usize) {
                Some(v) => out.push_str(&dump_string(v, &DumpOptions::default())),
                None => out.push_str("<invalid>"),
            }
        }
        _ if op.flags & OP_HAS_VARIABLE != 0 => {
            let v = code[pc];
            let name = with_level(bc, imm, |b| b.debuginfo.locals.get(v as usize).cloned())
                .flatten()
                .unwrap_or_default();
            let _ = write!(out, " ${name}:{v}");
            if imm != 0 {
                let _ = write!(out, "^{imm}");
            }
        }
        _ => {
            let _ = write!(out, " {imm}");
        }
    }
}

/// execute.c `ret_follows`: is the instruction at `pc` a `RET`, or a chain of `JUMP`s
/// ending in one?
fn ret_follows(code: &[u16], mut pc: usize) -> bool {
    loop {
        match code.get(pc) {
            Some(&op) if op == Opcode::RET as u16 => return true,
            Some(&op) if op == Opcode::JUMP as u16 => match code.get(pc + 1) {
                Some(&off) => pc = pc + 2 + off as usize,
                None => return false,
            },
            _ => return false,
        }
    }
}

/// execute.c `tail_call_analyze`: a `CALL_JQ` whose closures (callee included) all
/// live in enclosing frames (level > 0), followed by a `RET` (possibly through
/// `JUMP`s), becomes `TAIL_CALL_JQ`.
fn tail_call_analyze(code: &[u16], pc: usize) -> Opcode {
    debug_assert_eq!(code[pc], Opcode::CALL_JQ as u16);
    let mut p = pc + 1;
    let nclosures = code[p] as usize + 1;
    p += 1;
    for _ in 0..nclosures {
        if code[p] == 0 {
            return Opcode::CALL_JQ;
        }
        p += 2;
    }
    if ret_follows(code, p) {
        Opcode::TAIL_CALL_JQ
    } else {
        Opcode::CALL_JQ
    }
}

/// execute.c `optimize_code` on one function's code.
pub fn optimize_code(code: &mut [u16]) {
    let mut pc = 0;
    while pc < code.len() {
        if code[pc] == Opcode::CALL_JQ as u16 {
            code[pc] = tail_call_analyze(code, pc) as u16;
        }
        let len = bytecode_operation_length(&code[pc..]);
        if len == 0 {
            break;
        }
        pc += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_table_matches_order() {
        for (i, op) in ALL_OPCODES.iter().enumerate() {
            assert_eq!(*op as usize, i);
            assert_eq!(op.describe().op, Some(*op));
            assert_eq!(Opcode::from_u16(i as u16), Some(*op));
            assert_eq!(format!("{op:?}"), op.name());
        }
        assert_eq!(Opcode::from_u16(NUM_OPCODES as u16), None);
        assert_eq!(opcode_describe(999).name, "#INVALID");
        assert_eq!(Opcode::ERRORK as usize, NUM_OPCODES - 1);
    }

    #[test]
    fn lengths() {
        assert_eq!(Opcode::LOADK.describe().length, 2);
        assert_eq!(Opcode::LOADV.describe().length, 3);
        assert_eq!(Opcode::STORE_GLOBAL.describe().length, 4);
        assert_eq!(Opcode::CALL_JQ.describe().length, 4);
        assert_eq!(Opcode::CALL_BUILTIN.describe().length, 3);
        assert_eq!(Opcode::CLOSURE_CREATE.describe().length, 0);
        // CALL_JQ with two closure arguments: 4 + 2*2.
        assert_eq!(
            bytecode_operation_length(&[Opcode::CALL_JQ as u16, 2, 0, 0, 0, 0, 0, 0]),
            8
        );
    }

    #[test]
    fn tail_calls() {
        use Opcode::*;
        // CALL_JQ 0 closures, callee at level 1, then RET.
        let mut code = vec![CALL_JQ as u16, 0, 1, ARG_NEWCLOSURE, RET as u16];
        optimize_code(&mut code);
        assert_eq!(code[0], TAIL_CALL_JQ as u16);
        // Level 0 callee: not a tail call.
        let mut code = vec![CALL_JQ as u16, 0, 0, ARG_NEWCLOSURE, RET as u16];
        optimize_code(&mut code);
        assert_eq!(code[0], CALL_JQ as u16);
        // Through a JUMP to the RET.
        let mut code = vec![
            CALL_JQ as u16,
            0,
            1,
            0,
            JUMP as u16,
            1,
            POP as u16,
            RET as u16,
        ];
        optimize_code(&mut code);
        assert_eq!(code[0], TAIL_CALL_JQ as u16);
        // Followed by something else.
        let mut code = vec![CALL_JQ as u16, 0, 1, 0, POP as u16, RET as u16];
        optimize_code(&mut code);
        assert_eq!(code[0], CALL_JQ as u16);
    }
}
