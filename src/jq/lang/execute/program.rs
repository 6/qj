//! The executable form of a compiled program: jq's `struct bytecode` tree flattened into
//! one code array and a function table, so the interpreter can refer to functions and
//! return addresses with plain integers (jq uses `struct bytecode*` and `uint16_t*`).
//!
//! The code is copied verbatim; branch offsets are relative and closure references are
//! `(level, index)` pairs resolved through frames at run time, so nothing needs
//! rewriting. A global pc is `Func::base` plus jq's pc within that function.

use std::rc::Rc;

use crate::jq::builtins::CFunction;
use crate::jq::lang::bytecode::Bytecode;

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
    /// The original bytecode: its constant pool (`LOADK` copies from it, so constants
    /// have jq's refcounts), and `dump_operation` for `--debug-trace`.
    pub bc: Rc<Bytecode>,
}

/// A compiled program ready to run.
pub(super) struct Program {
    /// Every function's code, concatenated.
    pub code: Vec<u16>,
    /// Function 0 is the top level.
    pub funcs: Vec<Func>,
    /// `globals->cfunctions`, indexed by `CALL_BUILTIN`'s second immediate.
    pub cfunctions: Vec<CFunction>,
    /// Keeps the tree alive: `dump_operation` follows `parent` (weak) pointers.
    pub _root: Rc<Bytecode>,
}

impl Program {
    /// Flattens the tree rooted at `root` (the top-level program).
    pub fn new(root: Rc<Bytecode>) -> Program {
        let mut prog = Program {
            code: Vec::new(),
            funcs: Vec::new(),
            cfunctions: root.globals.cfunctions.clone(),
            _root: root.clone(),
        };
        prog.add(&root);
        prog
    }

    fn add(&mut self, bc: &Rc<Bytecode>) -> u32 {
        let id = self.funcs.len() as u32;
        self.funcs.push(Func {
            base: self.code.len() as u32,
            nclosures: bc.nclosures as u32,
            nlocals: bc.nlocals as u32,
            subfunctions: Vec::with_capacity(bc.subfunctions.len()),
            bc: bc.clone(),
        });
        self.code.extend_from_slice(&bc.code);
        for sub in &bc.subfunctions {
            let sub_id = self.add(sub);
            self.funcs[id as usize].subfunctions.push(sub_id);
        }
        id
    }
}
