//! The executable form of a compiled program: jq's `struct bytecode` tree flattened into
//! one code array and a function table, so the interpreter can refer to functions and
//! return addresses with plain integers (jq uses `struct bytecode*` and `uint16_t*`).
//!
//! The code is copied verbatim; branch offsets are relative and closure references are
//! `(level, index)` pairs resolved through frames at run time, so nothing needs
//! rewriting. A global pc is `Func::base` plus jq's pc within that function.
//!
//! After the functions come three pseudo-instructions that no function jumps to. They
//! are where native builtins (`native.rs`) point frames and fork points: a sub-run's
//! frame returns to [`Program::subrun_ret_pc`], its base fork point resumes at
//! [`Program::subrun_base_pc`], and a suspended native generator's fork point at
//! [`Program::native_resume_pc`].

use std::rc::Rc;

use super::native::Consts;
use crate::jq::builtins::CFunction;
use crate::jq::builtins::native::NativeId;
use crate::jq::lang::bytecode::{Bytecode, Opcode, bytecode_operation_length};
use crate::jq::value::Value;

/// Pseudo-opcodes (beyond jq's opcodes and their `ON_BACKTRACK` variants).
pub(super) mod pseudo {
    /// Executed after a sub-run's closure returns a value.
    pub const SUBRUN_RET: u16 = 120;
    /// Only reached by backtracking: a sub-run's base fork point.
    pub const SUBRUN_BASE: u16 = 121;
    /// Only reached by backtracking: a suspended native generator.
    pub const NATIVE_RESUME: u16 = 122;
}

/// One function (a `struct bytecode`).
pub(super) struct Func {
    /// Where this function's code starts in [`Program::code`].
    pub base: u32,
    /// Number of closure parameters (`nclosures`).
    pub nclosures: u32,
    /// Number of local variable slots (`nlocals`).
    pub nlocals: u32,
    /// Function ids of `subfunctions`.
    pub subfunctions: Vec<u32>,
    /// The builtin.jq definition this function is, if it is marked
    /// ([`Bytecode::native`]).
    pub mark: Option<NativeId>,
    /// `mark`, when its native implementation runs in this program.
    pub native: Option<NativeId>,
    /// The original bytecode: its constant pool (`LOADK` copies from it, so constants
    /// have jq's refcounts), and `dump_operation` for `--debug-trace`.
    pub bc: Rc<Bytecode>,
}

/// A compiled program ready to run.
pub(super) struct Program {
    /// Every function's code, concatenated, then the pseudo-instructions.
    pub code: Vec<u16>,
    /// Function 0 is the top level.
    pub funcs: Vec<Func>,
    /// `globals->cfunctions`, indexed by `CALL_BUILTIN`'s second immediate.
    pub cfunctions: Vec<CFunction>,
    /// Where a sub-run's frame returns to ([`pseudo::SUBRUN_RET`]).
    pub subrun_ret_pc: u32,
    /// A sub-run's base fork point ([`pseudo::SUBRUN_BASE`]).
    pub subrun_base_pc: u32,
    /// A suspended native generator's fork point ([`pseudo::NATIVE_RESUME`]).
    pub native_resume_pc: u32,
    /// Whether any function uses `?//` (`DESTRUCTURE_ALT`), which catches the errors
    /// jq uses to abandon generators (`break`): natives that abandon or suspend their
    /// closure arguments then run jq's definitions instead.
    pub has_destructure_alt: bool,
    /// The constants natives return, as jq's definitions would (see
    /// [`Consts`]).
    pub native_consts: Consts,
    /// Every function's constants, in function order, when there are at most 65536:
    /// `code`'s constant immediates (`LOADK`, `PUSHK_UNDER`, `ERRORK`, `STORE_GLOBAL`)
    /// are then rewritten to index this table, so an instruction needn't look up its
    /// frame's function (the trace prints from the original `Bytecode::code`). Pointers
    /// into the constant pools, not copies: `--debug-trace` prints refcounts, and the
    /// pools live and stay unchanged as long as the program (`funcs[i].bc`).
    consts: Option<Vec<*const Value>>,
    /// Keeps the tree alive: `dump_operation` follows `parent` (weak) pointers.
    pub _root: Rc<Bytecode>,
}

impl Program {
    /// Flattens the tree rooted at `root` (the top-level program), numbering functions
    /// in depth-first preorder. Iterative: jq's parser accepts function nesting several
    /// thousand levels deep.
    pub fn new(root: Rc<Bytecode>) -> Program {
        let mut prog = Program {
            code: Vec::new(),
            funcs: Vec::new(),
            cfunctions: root.globals.cfunctions.clone(),
            subrun_ret_pc: 0,
            subrun_base_pc: 0,
            native_resume_pc: 0,
            has_destructure_alt: false,
            native_consts: Consts::default(),
            consts: None,
            _root: root.clone(),
        };
        // A function and where its id goes: (parent id, index among its subfunctions).
        type Pending = (Rc<Bytecode>, Option<(u32, usize)>);
        let mut work: Vec<Pending> = vec![(root, None)];
        while let Some((bc, parent)) = work.pop() {
            let id = prog.funcs.len() as u32;
            if let Some((p, i)) = parent {
                prog.funcs[p as usize].subfunctions[i] = id;
            }
            prog.has_destructure_alt |= uses_destructure_alt(&bc.code);
            prog.funcs.push(Func {
                base: prog.code.len() as u32,
                nclosures: bc.nclosures as u32,
                nlocals: bc.nlocals as u32,
                subfunctions: vec![0; bc.subfunctions.len()],
                mark: NativeId::from_mark(bc.native),
                native: None,
                bc: bc.clone(),
            });
            prog.code.extend_from_slice(&bc.code);
            for (i, sub) in bc.subfunctions.iter().enumerate().rev() {
                work.push((sub.clone(), Some((id, i))));
            }
        }
        prog.subrun_ret_pc = prog.code.len() as u32;
        prog.code.push(pseudo::SUBRUN_RET);
        prog.subrun_base_pc = prog.code.len() as u32;
        prog.code.push(pseudo::SUBRUN_BASE);
        prog.native_resume_pc = prog.code.len() as u32;
        prog.code.push(pseudo::NATIVE_RESUME);
        Consts::resolve(&mut prog);
        prog.flatten_constants();
        prog
    }

    /// Builds [`Program::consts`] and rewrites the constant immediates to index it.
    fn flatten_constants(&mut self) {
        let total: usize = self.funcs.iter().map(|f| f.bc.constants.len()).sum();
        if total > u16::MAX as usize + 1 {
            return;
        }
        let mut consts = Vec::with_capacity(total);
        for f in &self.funcs {
            let first = consts.len();
            consts.extend(f.bc.constants.iter().map(|v| v as *const Value));
            let (start, end) = (f.base as usize, f.base as usize + f.bc.code.len());
            let mut pc = start;
            while pc < end {
                let op = self.code[pc];
                let is_const = [
                    Opcode::LOADK,
                    Opcode::PUSHK_UNDER,
                    Opcode::ERRORK,
                    Opcode::STORE_GLOBAL,
                ]
                .iter()
                .any(|o| *o as u16 == op);
                if is_const {
                    self.code[pc + 1] = (first + self.code[pc + 1] as usize) as u16;
                }
                let len = bytecode_operation_length(&self.code[pc..end]);
                if len == 0 {
                    break;
                }
                pc += len;
            }
        }
        self.consts = Some(consts);
    }

    /// The constant a (rewritten) constant immediate names, for an instruction of
    /// function `func` (`jv_array_get(frame_current(jq)->bc->constants, idx)`).
    #[inline(always)]
    pub fn constant(&self, func: impl FnOnce() -> u32, idx: u16) -> &Value {
        match &self.consts {
            // SAFETY: the pointers are into `self.funcs[..].bc.constants`, which are
            // never changed and live as long as `self` (see `consts`).
            Some(c) => unsafe { &*c[idx as usize] },
            None => &self.funcs[func() as usize].bc.constants[idx as usize],
        }
    }

    /// The constant pool of function `func` (what its `LOADK`s copy from).
    pub fn constants(&self, func: u32) -> &[Value] {
        &self.funcs[func as usize].bc.constants
    }
}

/// Whether `code` contains a `DESTRUCTURE_ALT`.
fn uses_destructure_alt(code: &[u16]) -> bool {
    let mut pc = 0;
    while pc < code.len() {
        if code[pc] == Opcode::DESTRUCTURE_ALT as u16 {
            return true;
        }
        let len = bytecode_operation_length(&code[pc..]);
        if len == 0 {
            break;
        }
        pc += len;
    }
    false
}
