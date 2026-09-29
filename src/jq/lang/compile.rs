//! Port of jq 1.8.1's `compile.c`: the block/instruction IR that parser.y's actions
//! build ([`super::lower`]), lexical binding (`block_bind_*`), and `block_compile`,
//! which turns a bound program into [`Bytecode`].
//!
//! # The IR
//!
//! jq represents code as doubly linked lists of `struct inst`; a `block` is a
//! `(first, last)` pair, so joining is O(1), and instructions point at each other
//! (`bound_by`, branch targets). Here the instructions live in an arena owned by a
//! [`Compiler`] and refer to each other by [`InstId`]; a [`Block`] is the same
//! `(first, last)` pair. Freed instructions are simply abandoned in the arena, which
//! is dropped with the compiler.
//!
//! Every `gen_*` function of compile.c is a method with the same name and the same
//! instruction sequence, so disassembly matches `jq --debug-dump-disasm` exactly.
//!
//! # Depth
//!
//! jq recurses over nested closures when binding and compiling. A long chain like
//! `. + . + ... + .` nests one lambda per `+`, so the walks here that follow
//! `subfn`/`arglist` (binding, reference marking, argument expansion, compiling
//! subfunctions) use explicit stacks instead of recursion.

use std::rc::Rc;

use super::ast::Loc;
use super::bytecode::{
    self, ARG_NEWCLOSURE, Bytecode, DebugInfo, OP_BIND_WILDCARD, OP_HAS_BINDING, OP_HAS_BRANCH,
    OP_HAS_CONSTANT, OP_HAS_VARIABLE, OP_IS_CALL_PSEUDO, Opcode, SymbolTable,
};
use super::locfile::LocFile;
use crate::jq::builtins::CFunction;
use crate::jq::value::{Object, Str, Value};

use Opcode::*;

/// Index of an instruction in the [`Compiler`]'s arena (jq's `inst*`).
pub type InstId = u32;

/// The name `gen_lambda` gives closures.
const LAMBDA: &str = "@lambda";

/// Index of a [`LocFile`] registered with [`Compiler::add_locfile`].
pub type LocFileId = u32;

/// jq's `block`: a (possibly empty) list of instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Block {
    pub(crate) first: Option<InstId>,
    pub(crate) last: Option<InstId>,
}

impl Block {
    /// `gen_noop()`.
    pub const NOOP: Block = Block {
        first: None,
        last: None,
    };

    /// `block_is_noop`.
    #[inline]
    pub fn is_noop(&self) -> bool {
        self.first.is_none() && self.last.is_none()
    }
}

/// `struct inst`.
#[derive(Clone)]
pub(crate) struct Inst {
    next: Option<InstId>,
    prev: Option<InstId>,
    pub(crate) op: Opcode,
    /// `imm.intval`
    intval: u16,
    /// `imm.target`
    target: Option<InstId>,
    /// `imm.constant` (only meaningful for ops with `OP_HAS_CONSTANT`)
    pub(crate) constant: Value,
    /// `imm.cfunc`
    cfunc: Option<CFunction>,
    locfile: Option<LocFileId>,
    source: Loc,
    /// `NULL`: unbound free variable; itself: a binder; another inst: a use.
    bound_by: Option<InstId>,
    pub(crate) symbol: Option<Rc<str>>,
    any_unbound: i8,
    referenced: bool,
    pub(crate) nformals: i32,
    pub(crate) nactuals: i32,
    /// Body of a `CLOSURE_CREATE` (and the matcher of a `DESTRUCTURE_ALT`).
    pub(crate) subfn: Block,
    /// Formals of a `CLOSURE_CREATE`, arguments of a `CALL_JQ`.
    pub(crate) arglist: Block,
    /// Which function this instruction was compiled into (`compiled`), as a function
    /// id + 1; 0 before compilation.
    compiled: u32,
    /// Position just after this instruction (`bytecode_pos`).
    bytecode_pos: i32,
}

/// Owns the instruction arena, the source files instructions point into, and the
/// error messages reported so far (jq's `jq_report_error` calls, in order).
pub struct Compiler {
    insts: Vec<Inst>,
    locfiles: Vec<Rc<LocFile>>,
    /// `fname` of each locfile.
    locfile_names: Vec<Value>,
    /// Messages exactly as jq hands them to `jq_report_error` (the CLI prints each
    /// followed by a newline).
    pub(crate) messages: Vec<String>,
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

/// `opcode_describe(op)->flags`.
#[inline]
fn flags(op: Opcode) -> u32 {
    op.describe().flags
}

impl Compiler {
    pub fn new() -> Compiler {
        Compiler {
            insts: Vec::with_capacity(256),
            locfiles: Vec::new(),
            locfile_names: Vec::new(),
            messages: Vec::new(),
        }
    }

    /// Registers a source file (`locfile_init`) that instructions can point into.
    pub fn add_locfile(&mut self, lf: Rc<LocFile>) -> LocFileId {
        self.locfile_names.push(Value::from(lf.fname()));
        self.locfiles.push(lf);
        (self.locfiles.len() - 1) as LocFileId
    }

    /// The file's name as a jq string (`l->fname`, shared by `$__loc__` constants).
    pub fn locfile_name(&self, id: LocFileId) -> Value {
        self.locfile_names[id as usize].clone()
    }

    pub fn locfile(&self, id: LocFileId) -> &Rc<LocFile> {
        &self.locfiles[id as usize]
    }

    /// `jq_report_error`.
    pub(crate) fn report(&mut self, msg: String) {
        self.messages.push(msg);
    }

    /// `locfile_locate(l, loc, "%s", msg)`, `msg` including its `jq: error: ` prefix.
    fn locate(&mut self, lf: Option<LocFileId>, loc: Loc, msg: &str) {
        let text = match lf {
            Some(id) => self.locfiles[id as usize].locate(loc, msg),
            // jq would crash on the NULL locfile; there is no location to show.
            None => format!("jq: error: {msg}"),
        };
        self.report(text);
    }

    #[inline]
    pub(crate) fn inst(&self, i: InstId) -> &Inst {
        &self.insts[i as usize]
    }

    #[inline]
    fn inst_mut(&mut self, i: InstId) -> &mut Inst {
        &mut self.insts[i as usize]
    }

    /// Iterates over a block's top-level instructions.
    pub(crate) fn iter(&self, b: Block) -> BlockIter<'_> {
        BlockIter {
            c: self,
            cur: b.first,
        }
    }

    // ------------------------------------------------------------------
    // Instructions and blocks
    // ------------------------------------------------------------------

    /// `inst_new`.
    fn inst_new(&mut self, op: Opcode) -> InstId {
        let id = self.insts.len() as InstId;
        self.insts.push(Inst {
            next: None,
            prev: None,
            op,
            intval: 0,
            target: None,
            constant: Value::Null,
            cfunc: None,
            locfile: None,
            source: Loc::UNKNOWN,
            bound_by: None,
            symbol: None,
            any_unbound: 0,
            referenced: false,
            nformals: -1,
            nactuals: -1,
            subfn: Block::NOOP,
            arglist: Block::NOOP,
            compiled: 0,
            bytecode_pos: -1,
        });
        id
    }

    /// `inst_block`.
    #[inline]
    fn inst_block(i: InstId) -> Block {
        Block {
            first: Some(i),
            last: Some(i),
        }
    }

    /// `block_is_single`.
    #[inline]
    pub fn block_is_single(&self, b: Block) -> bool {
        b.first.is_some() && b.first == b.last
    }

    /// `block_take`: removes and returns the first instruction.
    fn block_take(&mut self, b: &mut Block) -> Option<InstId> {
        let i = b.first?;
        match self.inst(i).next {
            Some(next) => {
                self.inst_mut(next).prev = None;
                b.first = Some(next);
                self.inst_mut(i).next = None;
            }
            None => {
                b.first = None;
                b.last = None;
            }
        }
        Some(i)
    }

    /// `block_take_last`.
    fn block_take_last(&mut self, b: &mut Block) -> Option<InstId> {
        let i = b.last?;
        match self.inst(i).prev {
            Some(prev) => {
                let next = self.inst(i).next;
                self.inst_mut(prev).next = next;
                b.last = Some(prev);
                self.inst_mut(i).prev = None;
            }
            None => {
                b.first = None;
                b.last = None;
            }
        }
        Some(i)
    }

    /// `gen_location`: gives every top-level instruction without a location this one.
    pub fn gen_location(&mut self, loc: Loc, lf: LocFileId, b: Block) -> Block {
        let mut cur = b.first;
        while let Some(i) = cur {
            let inst = self.inst_mut(i);
            if inst.source == Loc::UNKNOWN {
                inst.source = loc;
                inst.locfile = Some(lf);
            }
            cur = inst.next;
        }
        b
    }

    /// `gen_op_simple`.
    pub fn gen_op_simple(&mut self, op: Opcode) -> Block {
        debug_assert_eq!(op.describe().length, 1);
        Self::inst_block(self.inst_new(op))
    }

    /// `gen_error`: `ERRORK` raising `constant`.
    pub fn gen_error(&mut self, constant: Value) -> Block {
        let i = self.inst_new(ERRORK);
        self.inst_mut(i).constant = constant;
        Self::inst_block(i)
    }

    /// `gen_const`.
    pub fn gen_const(&mut self, constant: Value) -> Block {
        let i = self.inst_new(LOADK);
        self.inst_mut(i).constant = constant;
        Self::inst_block(i)
    }

    /// `gen_const_global`: a data import (`import "f" as $name`).
    pub fn gen_const_global(&mut self, constant: Value, name: &str) -> Block {
        let i = self.inst_new(STORE_GLOBAL);
        let inst = self.inst_mut(i);
        inst.constant = constant;
        inst.symbol = Some(Rc::from(name));
        inst.any_unbound = 0;
        Self::inst_block(i)
    }

    /// `gen_op_pushk_under`.
    pub fn gen_op_pushk_under(&mut self, constant: Value) -> Block {
        let i = self.inst_new(PUSHK_UNDER);
        self.inst_mut(i).constant = constant;
        Self::inst_block(i)
    }

    /// `block_is_const`: a single `LOADK` or `PUSHK_UNDER`.
    pub fn block_is_const(&self, b: Block) -> bool {
        self.block_is_single(b) && matches!(self.inst(b.first.unwrap()).op, LOADK | PUSHK_UNDER)
    }

    /// `block_const` (for a [`Compiler::block_is_const`] block).
    pub fn block_const(&self, b: Block) -> Value {
        debug_assert!(self.block_is_const(b));
        self.inst(b.first.unwrap()).constant.clone()
    }

    /// `gen_op_target`: a branch to the last instruction of `target`.
    pub fn gen_op_target(&mut self, op: Opcode, target: Block) -> Block {
        debug_assert!(flags(op) & OP_HAS_BRANCH != 0);
        let t = target.last.expect("gen_op_target: empty target");
        let i = self.inst_new(op);
        self.inst_mut(i).target = Some(t);
        Self::inst_block(i)
    }

    /// `gen_op_targetlater`.
    fn gen_op_targetlater(&mut self, op: Opcode) -> Block {
        debug_assert!(flags(op) & OP_HAS_BRANCH != 0);
        Self::inst_block(self.inst_new(op))
    }

    /// `inst_set_target`.
    fn inst_set_target(&mut self, b: Block, target: Block) {
        debug_assert!(self.block_is_single(b));
        let t = target.last.expect("inst_set_target: empty target");
        self.inst_mut(b.first.unwrap()).target = Some(t);
    }

    /// `gen_op_unbound`: a reference to (or binder of) `name`, not yet bound.
    pub fn gen_op_unbound(&mut self, op: Opcode, name: &str) -> Block {
        self.gen_op_unbound_rc(op, Rc::from(name))
    }

    fn gen_op_unbound_rc(&mut self, op: Opcode, name: Rc<str>) -> Block {
        debug_assert!(flags(op) & OP_HAS_BINDING != 0);
        let i = self.inst_new(op);
        let inst = self.inst_mut(i);
        inst.symbol = Some(name);
        inst.any_unbound = 1;
        Self::inst_block(i)
    }

    /// `gen_op_var_fresh`: a variable binder.
    pub fn gen_op_var_fresh(&mut self, op: Opcode, name: &str) -> Block {
        debug_assert!(flags(op) & OP_HAS_VARIABLE != 0);
        let b = self.gen_op_unbound(op, name);
        let i = b.first.unwrap();
        self.inst_mut(i).bound_by = Some(i);
        b
    }

    /// `gen_op_bound`: a reference bound to `binder`.
    pub fn gen_op_bound(&mut self, op: Opcode, binder: Block) -> Block {
        debug_assert!(self.block_is_single(binder));
        let binder = binder.first.unwrap();
        let sym = self.inst(binder).symbol.clone().expect("binder symbol");
        let b = self.gen_op_unbound_rc(op, sym);
        let i = b.first.unwrap();
        let inst = self.inst_mut(i);
        inst.bound_by = Some(binder);
        inst.any_unbound = 0;
        b
    }

    /// `gen_dictpair`.
    pub fn gen_dictpair(&mut self, k: Block, v: Block) -> Block {
        let k = self.gen_subexp(k);
        let v = self.gen_subexp(v);
        let insert = self.gen_op_simple(INSERT);
        self.block3(k, v, insert)
    }

    /// `inst_join`.
    fn inst_join(&mut self, a: InstId, b: InstId) {
        debug_assert!(self.inst(a).next.is_none());
        debug_assert!(self.inst(b).prev.is_none());
        self.inst_mut(a).next = Some(b);
        self.inst_mut(b).prev = Some(a);
    }

    /// `block_append`.
    pub fn block_append(&mut self, b: &mut Block, b2: Block) {
        if let Some(first2) = b2.first {
            match b.last {
                Some(last) => self.inst_join(last, first2),
                None => b.first = Some(first2),
            }
            b.last = b2.last;
        }
    }

    /// `block_join`.
    pub fn block_join(&mut self, a: Block, b: Block) -> Block {
        let mut c = a;
        self.block_append(&mut c, b);
        c
    }

    /// `BLOCK(a, b, c)`.
    #[inline]
    pub fn block3(&mut self, a: Block, b: Block, c: Block) -> Block {
        let ab = self.block_join(a, b);
        self.block_join(ab, c)
    }

    /// `BLOCK(...)` for any number of blocks.
    pub fn blocks(&mut self, bs: &[Block]) -> Block {
        let mut acc = Block::NOOP;
        for &b in bs {
            self.block_append(&mut acc, b);
        }
        acc
    }

    /// `block_has_only_binders_and_imports`.
    pub fn block_has_only_binders_and_imports(&self, binders: Block, bindflags: u32) -> bool {
        let bindflags = bindflags | OP_HAS_BINDING;
        self.iter(binders).all(|i| {
            let op = self.inst(i).op;
            flags(op) & bindflags == bindflags || op == DEPS || op == MODULEMETA
        })
    }

    /// `block_has_only_binders`.
    pub fn block_has_only_binders(&self, binders: Block, bindflags: u32) -> bool {
        let bindflags = (bindflags | OP_HAS_BINDING) & !OP_BIND_WILDCARD;
        self.iter(binders).all(|i| {
            let op = self.inst(i).op;
            flags(op) & bindflags == bindflags || op == MODULEMETA
        })
    }

    /// `block_count_actuals`: a call site's actual parameters.
    fn block_count_actuals(&self, b: Block) -> i32 {
        let mut args = 0;
        for i in self.iter(b) {
            match self.inst(i).op {
                CLOSURE_CREATE | CLOSURE_PARAM | CLOSURE_CREATE_C => args += 1,
                op => panic!("block_count_actuals: unknown function type {op:?}"),
            }
        }
        args
    }

    // ------------------------------------------------------------------
    // Binding
    // ------------------------------------------------------------------

    /// `block_bind_subblock_inner` (iterative): binds every unbound instruction of
    /// `body` (recursing into closures and argument lists) that refers to `binder`,
    /// returning how many were bound.
    fn block_bind_subblock(
        &mut self,
        binder: InstId,
        body: Block,
        bindflags: u32,
        break_distance: i32,
    ) -> usize {
        debug_assert!(break_distance >= 0);
        let binder_op = self.inst(binder).op;
        debug_assert_eq!(
            flags(binder_op) & bindflags,
            bindflags & !OP_BIND_WILDCARD,
            "block_bind_subblock: binder {binder_op:?}"
        );
        debug_assert!(
            self.inst(binder).bound_by.is_none() || self.inst(binder).bound_by == Some(binder)
        );

        self.inst_mut(binder).bound_by = Some(binder);
        let bsym = self.inst(binder).symbol.clone().expect("binder symbol");
        let bnformals = self.inst(binder).nformals;
        let is_anonlabel_binder = bsym.starts_with("*anonlabel");
        let want = bindflags & !OP_BIND_WILDCARD;
        let wildcard = bindflags & OP_BIND_WILDCARD != 0;

        enum Task {
            Scan {
                cur: Option<InstId>,
                owner: Option<InstId>,
                bd: i32,
            },
            Finish {
                inst: InstId,
                owner: Option<InstId>,
            },
        }
        let mut nrefs = 0;
        let mut stack = vec![Task::Scan {
            cur: body.first,
            owner: None,
            bd: break_distance,
        }];
        while let Some(task) = stack.pop() {
            match task {
                Task::Finish { inst, owner } => {
                    if self.inst(inst).any_unbound != 0
                        && let Some(o) = owner
                    {
                        self.inst_mut(o).any_unbound = 1;
                    }
                }
                Task::Scan {
                    mut cur,
                    owner,
                    mut bd,
                } => {
                    while let Some(i) = cur {
                        let inst = &self.insts[i as usize];
                        cur = inst.next;
                        if inst.any_unbound == 0 {
                            continue;
                        }
                        let fl = flags(inst.op);
                        if fl & bindflags == want && inst.bound_by.is_none() && {
                            let sym = inst.symbol.as_deref().unwrap_or("");
                            sym == &*bsym
                                // break/break2/break3 (dead in jq 1.8.1, kept for fidelity)
                                || (wildcard && {
                                    let s = sym.as_bytes();
                                    s.len() == 2
                                        && s[0] == b'*'
                                        && bd <= 3
                                        && s[1] as i32 == b'1' as i32 + bd
                                })
                        } {
                            if inst.nactuals == -1 || inst.nactuals == bnformals {
                                self.insts[i as usize].bound_by = Some(binder);
                                nrefs += 1;
                            }
                        } else if fl & bindflags == want
                            && inst.bound_by.is_some()
                            && is_anonlabel_binder
                            && inst
                                .symbol
                                .as_deref()
                                .is_some_and(|s| s.starts_with("*anonlabel"))
                        {
                            bd += 1;
                        }
                        let inst = &mut self.insts[i as usize];
                        inst.any_unbound = (inst.symbol.is_some() && inst.bound_by.is_none()) as i8;
                        let (subfn, arglist) = (inst.subfn, inst.arglist);
                        if subfn.is_noop() && arglist.is_noop() {
                            if inst.any_unbound != 0
                                && let Some(o) = owner
                            {
                                self.inst_mut(o).any_unbound = 1;
                            }
                            continue;
                        }
                        // Recurse into the closure body, then the argument list, then
                        // propagate this instruction's flag, then continue the list.
                        stack.push(Task::Scan { cur, owner, bd });
                        stack.push(Task::Finish { inst: i, owner });
                        stack.push(Task::Scan {
                            cur: arglist.first,
                            owner: Some(i),
                            bd,
                        });
                        stack.push(Task::Scan {
                            cur: subfn.first,
                            owner: Some(i),
                            bd,
                        });
                        break;
                    }
                }
            }
        }
        nrefs
    }

    /// `block_bind_each`.
    fn block_bind_each(&mut self, binder: Block, body: Block, bindflags: u32) -> usize {
        debug_assert!(self.block_has_only_binders(binder, bindflags));
        let bindflags = bindflags | OP_HAS_BINDING;
        let mut nrefs = 0;
        let mut cur = binder.first;
        while let Some(i) = cur {
            nrefs += self.block_bind_subblock(i, body, bindflags, 0);
            cur = self.inst(i).next;
        }
        nrefs
    }

    /// `block_bind`.
    pub fn block_bind(&mut self, binder: Block, body: Block, bindflags: u32) -> Block {
        self.block_bind_each(binder, body, bindflags);
        self.block_join(binder, body)
    }

    /// `block_bind_library`: binds a library's definitions into `body`, as
    /// `libname::name` (or plain `name` without a libname). The definitions are not
    /// joined into the result.
    pub fn block_bind_library(
        &mut self,
        binder: Block,
        body: Block,
        bindflags: u32,
        libname: Option<&str>,
    ) -> Block {
        let bindflags = bindflags | OP_HAS_BINDING;
        let matchname = match libname {
            Some(l) if !l.is_empty() => format!("{l}::"),
            _ => String::new(),
        };
        debug_assert!(self.block_has_only_binders(binder, bindflags));
        let mut cur = binder.last;
        while let Some(i) = cur {
            let mut bindflags2 = bindflags;
            let cname = self.inst(i).symbol.clone().expect("binder symbol");
            let tname: Rc<str> = Rc::from(format!("{matchname}{cname}"));
            // Ew
            if flags(self.inst(i).op) & (OP_HAS_VARIABLE | OP_HAS_CONSTANT) != 0 {
                bindflags2 = OP_HAS_VARIABLE | OP_HAS_BINDING;
            }
            // This mutation is ugly, even if we undo it
            self.inst_mut(i).symbol = Some(tname);
            self.block_bind_subblock(i, body, bindflags2, 0);
            self.inst_mut(i).symbol = Some(cname);
            cur = self.inst(i).prev;
        }
        body
    }

    /// `block_bind_referenced`: binds a sequence of binders (which must not already be
    /// bound to each other) to `body`, throwing away unreferenced ones.
    pub fn block_bind_referenced(&mut self, binder: Block, body: Block, bindflags: u32) -> Block {
        debug_assert!(self.block_has_only_binders(binder, bindflags));
        let bindflags = bindflags | OP_HAS_BINDING;
        let mut binder = binder;
        let mut body = body;
        while let Some(curr) = self.block_take_last(&mut binder) {
            if self.block_bind_subblock(curr, body, bindflags, 0) != 0 {
                body = self.block_join(Self::inst_block(curr), body);
            }
        }
        body
    }

    /// `block_bind_self`: binds each definition to the ones after it.
    pub fn block_bind_self(&mut self, binder: Block, bindflags: u32) -> Block {
        debug_assert!(self.block_has_only_binders(binder, bindflags));
        let bindflags = bindflags | OP_HAS_BINDING;
        let mut binder = binder;
        let mut body = Block::NOOP;
        while let Some(curr) = self.block_take_last(&mut binder) {
            self.block_bind_subblock(curr, body, bindflags, 0);
            body = self.block_join(Self::inst_block(curr), body);
        }
        body
    }

    /// `block_mark_referenced` (iterative, same visiting order).
    fn block_mark_referenced(&mut self, body: Block) {
        // (cursor walking backwards, saw_top)
        let mut stack: Vec<(Option<InstId>, bool)> = vec![(body.last, false)];
        while let Some((mut cur, mut saw_top)) = stack.pop() {
            while let Some(i) = cur {
                cur = self.inst(i).prev;
                let inst = self.inst(i);
                if saw_top && inst.bound_by == Some(i) && !inst.referenced {
                    continue;
                }
                if inst.op == TOP {
                    saw_top = true;
                }
                if let Some(b) = inst.bound_by {
                    self.inst_mut(b).referenced = true;
                }
                let inst = self.inst(i);
                let (arglist, subfn) = (inst.arglist, inst.subfn);
                if arglist.is_noop() && subfn.is_noop() {
                    continue;
                }
                stack.push((cur, saw_top));
                stack.push((subfn.last, false));
                stack.push((arglist.last, false));
                break;
            }
        }
    }

    /// `block_drop_unreferenced`.
    pub fn block_drop_unreferenced(&mut self, body: Block) -> Block {
        self.block_mark_referenced(body);
        let mut body = body;
        let mut refd = Block::NOOP;
        while let Some(curr) = self.block_take(&mut body) {
            let inst = self.inst(curr);
            if inst.bound_by == Some(curr) && !inst.referenced {
                // inst_free
            } else {
                refd = self.block_join(refd, Self::inst_block(curr));
            }
        }
        refd
    }

    /// `block_take_imports`: removes the leading `MODULEMETA`/`DEPS` instructions,
    /// returning the `DEPS` constants in order.
    pub fn block_take_imports(&mut self, body: &mut Block) -> Vec<Value> {
        let mut imports = Vec::new();
        while let Some(first) = body.first {
            let op = self.inst(first).op;
            if op != MODULEMETA && op != DEPS {
                break;
            }
            let dep = self.block_take(body).unwrap();
            if op == DEPS {
                imports.push(self.inst(dep).constant.clone());
            }
        }
        imports
    }

    /// `block_list_funcs`: `name/arity` of the top-level definitions, deduplicated in
    /// first-seen order.
    pub fn block_list_funcs(&self, body: Block, omit_underscores: bool) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for i in self.iter(body) {
            let inst = self.inst(i);
            if matches!(inst.op, CLOSURE_CREATE | CLOSURE_CREATE_C)
                && let Some(sym) = &inst.symbol
                && (!omit_underscores || !sym.starts_with('_'))
            {
                let key = format!("{}/{}", sym, inst.nformals);
                if seen.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
        out
    }

    /// `gen_module`.
    pub fn gen_module(&mut self, metadata: Block) -> Block {
        debug_assert!(self.block_is_const(metadata));
        let mut c = self.block_const(metadata);
        if !matches!(c, Value::Object(_)) {
            let mut o = Object::new();
            o.insert(Str::from("metadata"), c);
            c = Value::Object(o);
        }
        let i = self.inst_new(MODULEMETA);
        self.inst_mut(i).constant = c;
        Self::inst_block(i)
    }

    /// `block_module_meta`.
    pub fn block_module_meta(&self, b: Block) -> Value {
        match b.first {
            Some(i) if self.inst(i).op == MODULEMETA => self.inst(i).constant.clone(),
            _ => Value::Null,
        }
    }

    /// `gen_import`.
    pub fn gen_import(&mut self, name: &str, as_: Option<&str>, is_data: bool) -> Block {
        let mut meta = Object::new();
        if let Some(a) = as_ {
            meta.insert(Str::from("as"), Value::from(a));
        }
        meta.insert(Str::from("is_data"), Value::Bool(is_data));
        meta.insert(Str::from("relpath"), Value::from(name));
        let i = self.inst_new(DEPS);
        self.inst_mut(i).constant = Value::Object(meta);
        Self::inst_block(i)
    }

    /// `gen_import_meta`: `jv_object_merge(metadata, import)`.
    pub fn gen_import_meta(&mut self, import: Block, metadata: Block) -> Block {
        debug_assert!(self.block_is_single(import));
        let i = import.first.unwrap();
        let Value::Object(mut meta) = self.block_const(metadata) else {
            panic!("gen_import_meta: metadata is not an object");
        };
        if let Value::Object(imp) = &self.inst(i).constant {
            let imp = imp.clone();
            meta.merge(&imp);
        }
        self.inst_mut(i).constant = Value::Object(meta);
        import
    }

    /// `gen_function`: `def name(formals): body;`.
    pub fn gen_function(&mut self, name: &str, formals: Block, body: Block) -> Block {
        let i = self.inst_new(CLOSURE_CREATE);
        let mut body = body;
        let mut nformals = 0;
        let mut cur = formals.last;
        while let Some(p) = cur {
            nformals += 1;
            self.inst_mut(p).nformals = 0;
            if self.inst(p).op == CLOSURE_PARAM_REGULAR {
                self.inst_mut(p).op = CLOSURE_PARAM;
                let sym = self.inst(p).symbol.clone().unwrap();
                let call = self.gen_call(&sym, Block::NOOP);
                body = self.gen_var_binding(call, &sym, body);
            }
            self.block_bind_subblock(p, body, OP_IS_CALL_PSEUDO | OP_HAS_BINDING, 0);
            cur = self.inst(p).prev;
        }
        let inst = self.inst_mut(i);
        inst.subfn = body;
        inst.symbol = Some(Rc::from(name));
        inst.any_unbound = -1;
        inst.nformals = nformals;
        inst.arglist = formals;
        let b = Self::inst_block(i);
        if name == LAMBDA {
            // Binding a lambda to itself binds nothing (no call can be named
            // `@lambda`); jq's walk would only refresh the `any_unbound` hints, which
            // may stay conservative. Skipping it keeps `. + . + ... + .` (one lambda
            // per `+`, each containing the rest) linear instead of quadratic.
            inst.bound_by = Some(i);
        } else {
            self.block_bind_subblock(i, b, OP_IS_CALL_PSEUDO | OP_HAS_BINDING, 0);
        }
        b
    }

    /// `gen_param_regular`: a `$name` parameter.
    pub fn gen_param_regular(&mut self, name: &str) -> Block {
        self.gen_op_unbound(CLOSURE_PARAM_REGULAR, name)
    }

    /// `gen_param`: a closure parameter.
    pub fn gen_param(&mut self, name: &str) -> Block {
        self.gen_op_unbound(CLOSURE_PARAM, name)
    }

    /// `gen_lambda`.
    pub fn gen_lambda(&mut self, body: Block) -> Block {
        self.gen_function(LAMBDA, Block::NOOP, body)
    }

    /// `gen_call`.
    pub fn gen_call(&mut self, name: &str, args: Block) -> Block {
        let b = self.gen_op_unbound(CALL_JQ, name);
        let i = b.first.unwrap();
        let nactuals = self.block_count_actuals(args);
        let inst = self.inst_mut(i);
        inst.arglist = args;
        inst.nactuals = nactuals;
        b
    }

    /// `gen_subexp`.
    pub fn gen_subexp(&mut self, a: Block) -> Block {
        if a.is_noop() {
            return self.gen_op_simple(DUP);
        }
        if self.block_is_single(a) && self.inst(a.first.unwrap()).op == LOADK {
            let c = self.block_const(a);
            return self.gen_op_pushk_under(c);
        }
        let begin = self.gen_op_simple(SUBEXP_BEGIN);
        let end = self.gen_op_simple(SUBEXP_END);
        self.block3(begin, a, end)
    }

    /// `gen_both`: `a, b`.
    pub fn gen_both(&mut self, a: Block, b: Block) -> Block {
        let jump = self.gen_op_targetlater(JUMP);
        let fork = self.gen_op_target(FORK, jump);
        let c = self.blocks(&[fork, a, jump, b]);
        self.inst_set_target(jump, c);
        c
    }

    /// Is `i` the start of `SUBEXP_BEGIN LOADK SUBEXP_END`? Returns the `LOADK`.
    fn subexp_const(&self, i: Option<InstId>) -> Option<InstId> {
        let i = i?;
        if self.inst(i).op != SUBEXP_BEGIN {
            return None;
        }
        let k = self.inst(i).next?;
        if self.inst(k).op != LOADK {
            return None;
        }
        let end = self.inst(k).next?;
        if self.inst(end).op != SUBEXP_END {
            return None;
        }
        Some(k)
    }

    /// `gen_const_object`: folds `{k: v, ...}` with constant string keys and constant
    /// values. Returns [`Block::NOOP`] (leaving `expr` intact) when it can't.
    pub fn gen_const_object(&mut self, expr: Block) -> Block {
        let mut o = Object::new();
        let mut i = expr.first;
        while let Some(cur) = i {
            let k;
            if self.inst(cur).op == PUSHK_UNDER {
                k = self.inst(cur).constant.clone();
                i = self.inst(cur).next;
            } else if let Some(kk) = self.subexp_const(Some(cur)) {
                k = self.inst(kk).constant.clone();
                i = self.inst(self.inst(kk).next.unwrap()).next;
            } else {
                return Block::NOOP;
            }
            let v;
            match i {
                Some(vi) if self.inst(vi).op == PUSHK_UNDER => {
                    v = self.inst(vi).constant.clone();
                    i = self.inst(vi).next;
                }
                _ => match self.subexp_const(i) {
                    Some(vk) => {
                        v = self.inst(vk).constant.clone();
                        i = self.inst(self.inst(vk).next.unwrap()).next;
                    }
                    None => return Block::NOOP,
                },
            }
            match i {
                Some(ins) if self.inst(ins).op == INSERT => {}
                _ => return Block::NOOP,
            }
            let Value::String(ks) = k else {
                return Block::NOOP;
            };
            o.insert(ks, v);
            i = self.inst(i.unwrap()).next;
        }
        self.gen_const(Value::Object(o))
    }

    /// `gen_const_array`: folds `[1, 2, ...]` of constants.
    fn gen_const_array(&mut self, expr: Block) -> Block {
        let mut all_const = true;
        let mut commas = 0usize;
        let mut normal = true;
        let mut a: Vec<Value> = Vec::new();
        let mut cur = expr.first;
        while let Some(i) = cur {
            let inst = self.inst(i);
            if inst.op == FORK {
                commas += 1;
                if inst.target.is_none_or(|t| self.inst(t).op != JUMP) || !a.is_empty() {
                    normal = false;
                    break;
                }
            } else if all_const && inst.op == LOADK {
                if inst.next.is_some_and(|n| self.inst(n).op != JUMP) {
                    normal = false;
                    break;
                }
                a.push(inst.constant.clone());
            } else if inst.op != JUMP || inst.target.is_none_or(|t| self.inst(t).op != LOADK) {
                all_const = false;
            }
            cur = inst.next;
        }
        if all_const
            && normal
            && expr.last.is_none_or(|l| self.inst(l).op == LOADK)
            && a.len() == commas + 1
        {
            return self.gen_const(Value::from(a));
        }
        Block::NOOP
    }

    /// `gen_collect`: `[expr]`.
    pub fn gen_collect(&mut self, expr: Block) -> Block {
        let const_array = self.gen_const_array(expr);
        if const_array.first.is_some() {
            return const_array;
        }
        let array_var = self.gen_op_var_fresh(STOREV, "collect");
        let dup = self.gen_op_simple(DUP);
        let empty = self.gen_const(Value::empty_array());
        let c = self.block3(dup, empty, array_var);
        let append = self.gen_op_bound(APPEND, array_var);
        let backtrack = self.gen_op_simple(BACKTRACK);
        let tail = self.block_join(append, backtrack);
        let fork = self.gen_op_target(FORK, tail);
        let load = self.gen_op_bound(LOADVN, array_var);
        self.blocks(&[c, fork, expr, tail, load])
    }

    /// `bind_matcher`.
    fn bind_matcher(&mut self, matcher: Block, body: Block) -> Block {
        let mut cur = matcher.first;
        while let Some(i) = cur {
            let inst = self.inst(i);
            if matches!(inst.op, STOREV | STOREVN) && inst.bound_by.is_none() {
                self.block_bind_subblock(i, body, OP_HAS_VARIABLE, 0);
            }
            cur = self.inst(i).next;
        }
        self.block_join(matcher, body)
    }

    /// `block_get_unbound_vars`: names of the unbound `STOREV`/`STOREVN`s, in
    /// first-seen order.
    fn block_get_unbound_vars(&self, b: Block, vars: &mut Vec<Rc<str>>) {
        let mut stack = vec![b.first];
        while let Some(mut cur) = stack.pop() {
            while let Some(i) = cur {
                let inst = self.inst(i);
                cur = inst.next;
                if inst.subfn.first.is_some() {
                    stack.push(cur);
                    cur = inst.subfn.first;
                    continue;
                }
                if matches!(inst.op, STOREV | STOREVN) && inst.bound_by.is_none() {
                    let sym = inst.symbol.clone().unwrap();
                    if !vars.contains(&sym) {
                        vars.push(sym);
                    }
                }
            }
        }
    }

    /// `bind_alternation_matchers`: `?//` destructuring.
    fn bind_alternation_matchers(&mut self, matchers: Block, body: Block) -> Block {
        let mut preamble = Block::NOOP;
        let mut altmatchers = Block::NOOP;
        let mut mb = Block::NOOP;
        let mut final_matcher = matchers;

        // Pass through the matchers to find all destructured names.
        while let Some(f) = final_matcher.first {
            if self.inst(f).op != DESTRUCTURE_ALT {
                break;
            }
            let i = self.block_take(&mut final_matcher).unwrap();
            self.block_append(&mut altmatchers, Self::inst_block(i));
        }

        // We don't have any alternations here, so we can use the simplest case.
        if altmatchers.first.is_none() {
            return self.bind_matcher(final_matcher, body);
        }

        // Collect var names
        let mut all_vars = Vec::new();
        self.block_get_unbound_vars(altmatchers, &mut all_vars);
        self.block_get_unbound_vars(final_matcher, &mut all_vars);

        // We need a preamble of STOREVs to which to bind the matchers and the body.
        for key in all_vars {
            let dup = self.gen_op_simple(DUP);
            let null = self.gen_const(Value::Null);
            let store = self.gen_op_unbound_rc(STOREV, key);
            preamble = self.blocks(&[preamble, dup, null, store]);
        }

        // Now we build each matcher in turn
        let mut cur = altmatchers.first;
        while let Some(i) = cur {
            let submatcher = self.inst(i).subfn;
            // If we're successful, jump to the end of the matchers
            let jump = self.gen_op_target(JUMP, final_matcher);
            let submatcher = self.block_join(submatcher, jump);
            // DESTRUCTURE_ALT to the end of this submatcher so we can skip to the next
            // one on error
            let alt = self.gen_op_target(DESTRUCTURE_ALT, submatcher);
            mb = self.blocks(&[mb, alt, submatcher]);
            // We're done with this inst and we don't want it anymore
            self.inst_mut(i).subfn = Block::NOOP;
            cur = self.inst(i).next;
        }

        let rest = self.block3(mb, final_matcher, body);
        self.bind_matcher(preamble, rest)
    }

    /// `gen_reduce`.
    pub fn gen_reduce(&mut self, source: Block, matcher: Block, init: Block, body: Block) -> Block {
        let res_var = self.gen_op_var_fresh(STOREV, "reduce");
        let loadvn = self.gen_op_bound(LOADVN, res_var);
        let storev = self.gen_op_bound(STOREV, res_var);
        let inner = self.block3(loadvn, body, storev);
        let matched = self.bind_alternation_matchers(matcher, inner);
        let dupn = self.gen_op_simple(DUPN);
        let backtrack = self.gen_op_simple(BACKTRACK);
        let lp = self.blocks(&[dupn, source, matched, backtrack]);
        let dup = self.gen_op_simple(DUP);
        let fork = self.gen_op_target(FORK, lp);
        let load = self.gen_op_bound(LOADVN, res_var);
        self.blocks(&[dup, init, res_var, fork, lp, load])
    }

    /// `gen_foreach`.
    pub fn gen_foreach(
        &mut self,
        source: Block,
        matcher: Block,
        init: Block,
        update: Block,
        extract: Block,
    ) -> Block {
        let state_var = self.gen_op_var_fresh(STOREV, "foreach");
        let loadvn = self.gen_op_bound(LOADVN, state_var);
        let dup_state = self.gen_op_simple(DUP);
        let storev = self.gen_op_bound(STOREV, state_var);
        let inner = self.blocks(&[loadvn, update, dup_state, storev, extract]);
        let matched = self.bind_alternation_matchers(matcher, inner);
        let dup1 = self.gen_op_simple(DUP);
        let dup2 = self.gen_op_simple(DUP);
        self.blocks(&[dup1, init, state_var, dup2, source, matched])
    }

    /// `gen_definedor`: `a // b`.
    pub fn gen_definedor(&mut self, a: Block, b: Block) -> Block {
        // var found := false
        let found_var = self.gen_op_var_fresh(STOREV, "found");
        let dup = self.gen_op_simple(DUP);
        let f = self.gen_const(Value::Bool(false));
        let init = self.block3(dup, f, found_var);

        // if found, backtrack. Otherwise execute b
        let backtrack = self.gen_op_simple(BACKTRACK);
        let dup = self.gen_op_simple(DUP);
        let loadv = self.gen_op_bound(LOADV, found_var);
        let jump_f = self.gen_op_target(JUMP_F, backtrack);
        let pop = self.gen_op_simple(POP);
        let tail = self.blocks(&[dup, loadv, jump_f, backtrack, pop, b]);

        // try again
        let if_notfound = self.gen_op_simple(BACKTRACK);

        // found := true, produce result
        let dup = self.gen_op_simple(DUP);
        let t = self.gen_const(Value::Bool(true));
        let storev = self.gen_op_bound(STOREV, found_var);
        let jump = self.gen_op_target(JUMP, tail);
        let if_found = self.blocks(&[dup, t, storev, jump]);

        let fork = self.gen_op_target(FORK, if_notfound);
        let jump_f = self.gen_op_target(JUMP_F, if_found);
        self.blocks(&[init, fork, a, jump_f, if_found, if_notfound, tail])
    }

    /// `block_has_main`.
    pub fn block_has_main(&self, top: Block) -> bool {
        self.iter(top).any(|i| self.inst(i).op == TOP)
    }

    /// `block_is_funcdef`.
    pub fn block_is_funcdef(&self, b: Block) -> bool {
        b.first.is_some_and(|i| self.inst(i).op == CLOSURE_CREATE)
    }

    /// `gen_condbranch`.
    pub fn gen_condbranch(&mut self, iftrue: Block, iffalse: Block) -> Block {
        let jump = self.gen_op_target(JUMP, iffalse);
        let iftrue = self.block_join(iftrue, jump);
        let jump_f = self.gen_op_target(JUMP_F, iftrue);
        self.block3(jump_f, iftrue, iffalse)
    }

    /// `gen_and`: `a and b`.
    pub fn gen_and(&mut self, a: Block, b: Block) -> Block {
        // a and b = if a then (if b then true else false) else false
        let dup = self.gen_op_simple(DUP);
        let pop1 = self.gen_op_simple(POP);
        let t = self.gen_const(Value::Bool(true));
        let f = self.gen_const(Value::Bool(false));
        let inner = self.gen_condbranch(t, f);
        let iftrue = self.block3(pop1, b, inner);
        let pop2 = self.gen_op_simple(POP);
        let f2 = self.gen_const(Value::Bool(false));
        let iffalse = self.block_join(pop2, f2);
        let cb = self.gen_condbranch(iftrue, iffalse);
        self.block3(dup, a, cb)
    }

    /// `gen_or`: `a or b`.
    pub fn gen_or(&mut self, a: Block, b: Block) -> Block {
        // a or b = if a then true else (if b then true else false)
        let dup = self.gen_op_simple(DUP);
        let pop1 = self.gen_op_simple(POP);
        let t = self.gen_const(Value::Bool(true));
        let iftrue = self.block_join(pop1, t);
        let pop2 = self.gen_op_simple(POP);
        let t2 = self.gen_const(Value::Bool(true));
        let f2 = self.gen_const(Value::Bool(false));
        let inner = self.gen_condbranch(t2, f2);
        let iffalse = self.block3(pop2, b, inner);
        let cb = self.gen_condbranch(iftrue, iffalse);
        self.block3(dup, a, cb)
    }

    /// `gen_destructure_alt`: one `?//` alternative.
    pub fn gen_destructure_alt(&mut self, matcher: Block) -> Block {
        let mut cur = matcher.first;
        while let Some(i) = cur {
            if self.inst(i).op == STOREV {
                self.inst_mut(i).op = STOREVN;
            }
            cur = self.inst(i).next;
        }
        let i = self.inst_new(DESTRUCTURE_ALT);
        self.inst_mut(i).subfn = matcher;
        Self::inst_block(i)
    }

    /// `gen_var_binding`: `var as $name | body`.
    pub fn gen_var_binding(&mut self, var: Block, name: &str, body: Block) -> Block {
        let store = self.gen_op_unbound(STOREV, name);
        self.gen_destructure(var, store, body)
    }

    /// `gen_array_matcher`.
    pub fn gen_array_matcher(&mut self, left: Block, curr: Block) -> Block {
        let index = if left.is_noop() {
            0
        } else {
            // `left` was returned by this function, so the third inst is the constant
            // containing the previously used index
            let first = left.first.unwrap();
            debug_assert_eq!(self.inst(first).op, DUP);
            let second = self.inst(first).next.unwrap();
            let k = if self.inst(second).op == PUSHK_UNDER {
                second
            } else {
                debug_assert_eq!(self.inst(second).op, SUBEXP_BEGIN);
                self.inst(second).next.unwrap()
            };
            1 + self.inst(k).constant.as_f64().unwrap_or(0.0) as i32
        };
        // `left` goes at the end so that the const index is in a predictable place
        let dup = self.gen_op_simple(DUP);
        let k = self.gen_const(Value::number(index as f64));
        let k = self.gen_subexp(k);
        let index = self.gen_op_simple(INDEX);
        self.blocks(&[dup, k, index, curr, left])
    }

    /// `gen_object_matcher`.
    pub fn gen_object_matcher(&mut self, name: Block, curr: Block) -> Block {
        let dup = self.gen_op_simple(DUP);
        let name = self.gen_subexp(name);
        let index = self.gen_op_simple(INDEX);
        self.blocks(&[dup, name, index, curr])
    }

    /// `gen_destructure`: `var as matchers | body`.
    pub fn gen_destructure(&mut self, var: Block, matchers: Block, body: Block) -> Block {
        let mut var = var;
        let mut body = body;
        // var bindings can be added after coding the program; leave the TOP first.
        let mut top = Block::NOOP;
        if body.first.is_some_and(|f| self.inst(f).op == TOP) {
            let t = self.block_take(&mut body).unwrap();
            top = Self::inst_block(t);
        }
        if matchers
            .first
            .is_some_and(|f| self.inst(f).op == DESTRUCTURE_ALT)
        {
            let dup = self.gen_op_simple(DUP);
            self.block_append(&mut var, dup);
        } else {
            let dup = self.gen_op_simple(DUP);
            top = self.block_join(top, dup);
        }
        let var = self.gen_subexp(var);
        let pop = self.gen_op_simple(POP);
        let bound = self.bind_alternation_matchers(matchers, body);
        self.blocks(&[top, var, pop, bound])
    }

    /// `gen_wildvar_binding`: like `gen_var_binding`, but binds `break`'s wildcard.
    fn gen_wildvar_binding(&mut self, var: Block, name: &str, body: Block) -> Block {
        let dup = self.gen_op_simple(DUP);
        let store = self.gen_op_unbound(STOREV, name);
        let bound = self.block_bind(store, body, OP_HAS_VARIABLE | OP_BIND_WILDCARD);
        self.block3(dup, var, bound)
    }

    /// `gen_cond`: `if cond then iftrue else iffalse end`.
    pub fn gen_cond(&mut self, cond: Block, iftrue: Block, iffalse: Block) -> Block {
        let dup = self.gen_op_simple(DUP);
        let cond = self.gen_subexp(cond);
        let pop = self.gen_op_simple(POP);
        let c = self.block_join(cond, pop);
        let pop1 = self.gen_op_simple(POP);
        let t = self.block_join(pop1, iftrue);
        let pop2 = self.gen_op_simple(POP);
        let f = self.block_join(pop2, iffalse);
        let cb = self.gen_condbranch(t, f);
        self.block3(dup, c, cb)
    }

    /// `gen_try`: `try exp catch handler`.
    pub fn gen_try(&mut self, exp: Block, handler: Block) -> Block {
        let handler = if handler.is_noop() {
            let dup = self.gen_op_simple(DUP);
            let pop = self.gen_op_simple(POP);
            self.block_join(dup, pop)
        } else {
            handler
        };
        let jump = self.gen_op_target(JUMP, handler);
        let try_begin = self.gen_op_target(TRY_BEGIN, jump);
        let try_end = self.gen_op_simple(TRY_END);
        self.blocks(&[try_begin, exp, try_end, jump, handler])
    }

    /// `gen_label`: `label $name | exp`, `label` being `*label-name`.
    pub fn gen_label(&mut self, label: &str, exp: Block) -> Block {
        let l1 = self.gen_lambda(Block::NOOP);
        let lv = self.gen_op_unbound(LOADV, label);
        let l2 = self.gen_lambda(lv);
        let args = self.block_join(l1, l2);
        let cond = self.gen_call("_equal", args);
        let backtrack = self.gen_op_simple(BACKTRACK);
        let error = self.gen_call("error", Block::NOOP);
        let handler = self.gen_cond(cond, backtrack, error);
        let pop = self.gen_op_simple(POP);
        // try exp catch if . == $label then empty else error end
        let tried = self.gen_try(exp, handler);
        let body = self.block_join(pop, tried);
        let genlabel = self.gen_op_simple(GENLABEL);
        self.gen_wildvar_binding(genlabel, label, body)
    }

    /// `gen_cbinding`: prepends a `CLOSURE_CREATE_C` binder for each C builtin (so the
    /// result lists them in reverse).
    pub fn gen_cbinding(&mut self, cfunctions: &[CFunction], code: Block) -> Block {
        let mut code = code;
        for cf in cfunctions {
            let b = self.gen_cfunction(*cf);
            code = self.block_join(b, code);
        }
        code
    }

    /// One `CLOSURE_CREATE_C` of [`Compiler::gen_cbinding`].
    pub fn gen_cfunction(&mut self, cf: CFunction) -> Block {
        let i = self.inst_new(CLOSURE_CREATE_C);
        let inst = self.inst_mut(i);
        inst.cfunc = Some(cf);
        inst.symbol = Some(Rc::from(cf.name));
        inst.nformals = cf.nargs as i32 - 1;
        inst.any_unbound = 0;
        Self::inst_block(i)
    }

    /// The `(name, arity)` of every call in `b` (recursively) that is still unbound.
    pub(crate) fn unbound_calls(&self, b: Block, out: &mut Vec<(Rc<str>, i32)>) {
        let mut stack = vec![b.first];
        while let Some(mut cur) = stack.pop() {
            while let Some(i) = cur {
                let inst = self.inst(i);
                cur = inst.next;
                if inst.any_unbound == 0 {
                    continue;
                }
                if inst.op == CALL_JQ && inst.bound_by.is_none() {
                    out.push((inst.symbol.clone().unwrap(), inst.nactuals));
                }
                if inst.arglist.first.is_some() {
                    stack.push(inst.arglist.first);
                }
                if inst.subfn.first.is_some() {
                    stack.push(inst.subfn.first);
                }
            }
        }
    }
}

/// Iterator over a block's top-level instructions.
pub(crate) struct BlockIter<'a> {
    c: &'a Compiler,
    cur: Option<InstId>,
}

impl Iterator for BlockIter<'_> {
    type Item = InstId;
    fn next(&mut self) -> Option<InstId> {
        let i = self.cur?;
        self.cur = self.c.inst(i).next;
        Some(i)
    }
}

// ----------------------------------------------------------------------
// Compiling to bytecode
// ----------------------------------------------------------------------

/// What `expand_call_arglist` substitutes for unbound `$ENV` and `$name`.
pub struct Globals<'a> {
    /// The named arguments (`args`): unbound `$name` becomes `LOADK args[name]`.
    pub args: &'a Object,
    /// `$ENV`; built from the process environment on first use when `None`
    /// (`make_env`).
    pub env: Option<Value>,
}

/// execute.c/compile.c `make_env`: the process environment as an object.
pub fn make_env() -> Value {
    use std::os::unix::ffi::OsStrExt;
    let mut r = Object::new();
    for (k, v) in std::env::vars_os() {
        r.insert(
            Str::from_bytes(k.as_bytes()),
            Value::string_from_bytes(v.as_bytes()),
        );
    }
    Value::Object(r)
}

/// A function being compiled (a `struct bytecode` under construction).
struct FnState {
    code: Vec<u16>,
    nlocals: usize,
    nclosures: usize,
    constants: Vec<Value>,
    debuginfo: DebugInfo,
    /// Subfunction ids, by subfunction index.
    subfunctions: Vec<usize>,
    /// Enclosing function id (`parent`).
    parent: Option<usize>,
}

/// A C-builtin call whose arguments are being inlined by `expand_call_arglist`.
struct PendingCCall {
    curr: InstId,
    prelude: Block,
    actual_args: u16,
}

struct ExpandFrame {
    input: Block,
    ret: Block,
    call: Option<PendingCCall>,
}

impl Compiler {
    /// `expand_call_arglist` (iterative): resolves `$ENV` and named arguments,
    /// reports unbound symbols, and expands calls into calling sequences (closures
    /// for jq functions, inlined `SUBEXP`s for C functions).
    fn expand_call_arglist(&mut self, b: Block, globals: &mut Globals<'_>) -> (Block, usize) {
        let mut errors = 0;
        // The recursion over C builtin arguments, as an explicit stack: each frame is
        // one `expand_call_arglist(&body)` call.
        let mut stack = vec![ExpandFrame {
            input: b,
            ret: Block::NOOP,
            call: None,
        }];
        loop {
            let top = stack.len() - 1;

            // Expanding the arguments of a C builtin call: take the next one.
            if let Some(call) = &stack[top].call {
                let curr = call.curr;
                let mut arglist = self.inst(curr).arglist;
                if let Some(arg) = self.block_take(&mut arglist) {
                    self.inst_mut(curr).arglist = arglist;
                    debug_assert_eq!(self.inst(arg).op, CLOSURE_CREATE); // FIXME
                    let body = self.inst(arg).subfn;
                    self.inst_mut(arg).subfn = Block::NOOP;
                    stack.push(ExpandFrame {
                        input: body,
                        ret: Block::NOOP,
                        call: None,
                    });
                    continue;
                }
                let call = stack[top].call.take().unwrap();
                let inst = self.inst_mut(curr);
                debug_assert_eq!(inst.op, CALL_JQ);
                inst.op = CALL_BUILTIN;
                // include the implicit input in arg count
                inst.intval = call.actual_args + 1;
                let ret = stack[top].ret;
                stack[top].ret = self.block3(ret, call.prelude, Self::inst_block(curr));
                continue;
            }

            let mut input = stack[top].input;
            let Some(curr) = self.block_take(&mut input) else {
                let body = stack.pop().unwrap().ret;
                let Some(parent) = stack.last_mut() else {
                    return (body, errors);
                };
                // An argument of a C builtin call is expanded; arguments should be
                // pushed in reverse order, so prepend it to the prelude.
                let call = parent.call.as_mut().unwrap();
                let prelude = call.prelude;
                let sub = self.gen_subexp(body);
                let joined = self.block_join(sub, prelude);
                let call = stack.last_mut().unwrap().call.as_mut().unwrap();
                call.prelude = joined;
                call.actual_args += 1;
                continue;
            };
            stack[top].input = input;

            let op = self.inst(curr).op;
            if flags(op) & OP_HAS_BINDING != 0 && self.inst(curr).bound_by.is_none() {
                let sym = self
                    .inst(curr)
                    .symbol
                    .clone()
                    .unwrap_or_else(|| Rc::from(""));
                if op == LOADV && &*sym == "ENV" {
                    let env = globals.env.get_or_insert_with(make_env).clone();
                    let inst = self.inst_mut(curr);
                    inst.op = LOADK;
                    inst.constant = env;
                } else if op == LOADV
                    && let Some(v) = globals.args.get(&sym)
                {
                    let v = v.clone();
                    let inst = self.inst_mut(curr);
                    inst.op = LOADK;
                    inst.constant = v;
                } else {
                    let s = sym.as_bytes();
                    let inst = self.inst(curr);
                    let (lf, source) = (inst.locfile, inst.source);
                    let msg = if s.len() == 2 && s[0] == b'*' && (b'1'..=b'3').contains(&s[1]) {
                        "jq: error: break used outside labeled control structure".to_string()
                    } else if op == LOADV {
                        format!("jq: error: ${sym} is not defined")
                    } else {
                        format!("jq: error: {}/{} is not defined", sym, inst.nactuals)
                    };
                    self.locate(lf, source, &msg);
                    errors += 1;
                    // don't process this instruction if it's not well-defined
                    let ret = stack[top].ret;
                    stack[top].ret = self.block_join(ret, Self::inst_block(curr));
                    continue;
                }
            }

            let mut prelude = Block::NOOP;
            if self.inst(curr).op == CALL_JQ {
                let callee = self.inst(curr).bound_by.unwrap();
                match self.inst(callee).op {
                    CLOSURE_CREATE | CLOSURE_PARAM => {
                        let mut callargs = Block::NOOP;
                        let mut actual_args: u16 = 0;
                        let mut arglist = self.inst(curr).arglist;
                        while let Some(i) = self.block_take(&mut arglist) {
                            let b = Self::inst_block(i);
                            match self.inst(i).op {
                                CLOSURE_REF => self.block_append(&mut callargs, b),
                                CLOSURE_CREATE => {
                                    self.block_append(&mut prelude, b);
                                    let r = self.gen_op_bound(CLOSURE_REF, b);
                                    self.block_append(&mut callargs, r);
                                }
                                other => {
                                    panic!("expand_call_arglist: unknown parameter type {other:?}")
                                }
                            }
                            actual_args += 1;
                        }
                        let inst = self.inst_mut(curr);
                        inst.intval = actual_args;
                        inst.arglist = callargs;
                    }
                    CLOSURE_CREATE_C => {
                        stack[top].call = Some(PendingCCall {
                            curr,
                            prelude: Block::NOOP,
                            actual_args: 0,
                        });
                        continue;
                    }
                    other => panic!("expand_call_arglist: unknown function type {other:?}"),
                }
            }
            let ret = stack[top].ret;
            stack[top].ret = self.block3(ret, prelude, Self::inst_block(curr));
        }
    }

    /// `nesting_level`: how many functions up the lexical chain `target` was compiled.
    fn nesting_level(fns: &[FnState], mut fid: usize, target_compiled: u32) -> u16 {
        let mut level = 0u16;
        debug_assert!(target_compiled != 0);
        while fid + 1 != target_compiled as usize {
            level += 1;
            fid = fns[fid]
                .parent
                .expect("nesting_level: target not in an enclosing function");
        }
        level
    }

    /// `compile` for one function: expands calls, lays out the code, reports errors,
    /// and (if there were none) emits the code. Returns the error count and the
    /// subfunctions to compile next, as (`CLOSURE_CREATE` inst, subfunction id).
    fn compile_fn(
        &mut self,
        fns: &mut Vec<FnState>,
        fid: usize,
        b: Block,
        lf: LocFileId,
        globals: &mut Globals<'_>,
        cfunctions: &mut Vec<CFunction>,
    ) -> (usize, Vec<(InstId, usize)>) {
        let (b, mut errors) = self.expand_call_arglist(b, globals);
        let ret = self.gen_op_simple(RET);
        let b = self.block_join(b, ret);
        let compiled = fid as u32 + 1;

        let mut pos: usize = 0;
        let mut var_frame_idx: usize = 0;
        let mut nsubfunctions: u16 = 0;
        let mut localnames: Vec<String> = Vec::new();
        let mut cur = b.first;
        while let Some(i) = cur {
            let inst = self.inst(i);
            cur = inst.next;
            let op = inst.op;
            let mut length = op.describe().length;
            if op == CALL_JQ {
                length += 2 * self.iter(inst.arglist).count();
            }
            pos += length;
            let is_var_binder = flags(op) & OP_HAS_VARIABLE != 0 && inst.bound_by == Some(i);
            let symbol = inst.symbol.clone();
            let inst = self.inst_mut(i);
            inst.bytecode_pos = pos as i32;
            inst.compiled = compiled;
            debug_assert!(op != CLOSURE_REF && op != CLOSURE_PARAM);
            if is_var_binder {
                inst.intval = var_frame_idx as u16;
                var_frame_idx += 1;
                localnames.push(symbol.as_deref().unwrap_or("").to_string());
            }
            if op == CLOSURE_CREATE {
                debug_assert_eq!(inst.bound_by, Some(i));
                inst.intval = nsubfunctions;
                nsubfunctions += 1;
            }
            if op == CLOSURE_CREATE_C {
                debug_assert_eq!(inst.bound_by, Some(i));
                inst.intval = cfunctions.len() as u16;
                cfunctions.push(inst.cfunc.expect("CLOSURE_CREATE_C without cfunc"));
            }
        }
        if pos > 0xFFFF {
            // too long for program counter to fit in uint16_t
            self.locate(
                Some(lf),
                Loc::UNKNOWN,
                &format!("function compiled to {pos} bytes which is too long"),
            );
            errors += 1;
        }
        fns[fid].debuginfo.locals = localnames;

        let mut children = Vec::new();
        if nsubfunctions > 0 && errors == 0 {
            for i in self.iter(b).collect::<Vec<_>>() {
                if self.inst(i).op != CLOSURE_CREATE {
                    continue;
                }
                let sid = fns.len();
                let scompiled = sid as u32 + 1;
                let mut params = Vec::new();
                let mut p = self.inst(i).arglist.first;
                while let Some(param) = p {
                    debug_assert_eq!(self.inst(param).op, CLOSURE_PARAM);
                    debug_assert_eq!(self.inst(param).bound_by, Some(param));
                    let pi = self.inst_mut(param);
                    pi.intval = params.len() as u16;
                    pi.compiled = scompiled;
                    params.push(pi.symbol.as_deref().unwrap_or("").to_string());
                    p = pi.next;
                }
                let nclosures = params.len();
                fns.push(FnState {
                    code: Vec::new(),
                    nlocals: 0,
                    nclosures,
                    constants: Vec::new(),
                    debuginfo: DebugInfo {
                        name: Some(self.inst(i).symbol.as_deref().unwrap_or("").to_string()),
                        params,
                        locals: Vec::new(),
                    },
                    subfunctions: Vec::new(),
                    parent: Some(fid),
                });
                fns[fid].subfunctions.push(sid);
                children.push((i, sid));
            }
        }
        if errors > 0 {
            return (errors, children);
        }

        // Emit the code.
        let mut code: Vec<u16> = Vec::with_capacity(pos);
        let mut constants: Vec<Value> = Vec::new();
        let mut maxvar: i32 = -1;
        for i in self.iter(b) {
            let inst = self.inst(i);
            let op = inst.op;
            let desc = op.describe();
            if desc.length == 0 {
                continue;
            }
            code.push(op as u16);
            debug_assert!(op != CLOSURE_REF && op != CLOSURE_PARAM);
            if op == CALL_BUILTIN {
                let callee = inst.bound_by.unwrap();
                debug_assert_eq!(self.inst(callee).op, CLOSURE_CREATE_C);
                code.push(inst.intval);
                code.push(self.inst(callee).intval);
            } else if op == CALL_JQ {
                let callee = self.inst(inst.bound_by.unwrap());
                debug_assert!(matches!(callee.op, CLOSURE_CREATE | CLOSURE_PARAM));
                code.push(inst.intval);
                code.push(Self::nesting_level(fns, fid, callee.compiled));
                code.push(
                    callee.intval
                        | if callee.op == CLOSURE_CREATE {
                            ARG_NEWCLOSURE
                        } else {
                            0
                        },
                );
                for arg in self.iter(inst.arglist) {
                    let a = self.inst(arg);
                    let target = self.inst(a.bound_by.unwrap());
                    debug_assert!(a.op == CLOSURE_REF && target.op == CLOSURE_CREATE);
                    code.push(Self::nesting_level(fns, fid, target.compiled));
                    code.push(target.intval | ARG_NEWCLOSURE);
                }
            } else if desc.flags & OP_HAS_CONSTANT != 0 && desc.flags & OP_HAS_VARIABLE != 0 {
                // STORE_GLOBAL: constant global, basically
                code.push(constants.len() as u16);
                constants.push(inst.constant.clone());
                let binder = self.inst(inst.bound_by.unwrap());
                code.push(Self::nesting_level(fns, fid, binder.compiled));
                let var = binder.intval;
                code.push(var);
                maxvar = maxvar.max(var as i32);
            } else if desc.flags & OP_HAS_CONSTANT != 0 {
                code.push(constants.len() as u16);
                constants.push(inst.constant.clone());
            } else if desc.flags & OP_HAS_VARIABLE != 0 {
                let binder = self.inst(inst.bound_by.unwrap());
                code.push(Self::nesting_level(fns, fid, binder.compiled));
                let var = binder.intval;
                code.push(var);
                maxvar = maxvar.max(var as i32);
            } else if desc.flags & OP_HAS_BRANCH != 0 {
                let target = self.inst(inst.target.unwrap()).bytecode_pos;
                let here = code.len() as i32;
                debug_assert!(target != -1);
                debug_assert!(target > here, "only forward branches");
                code.push((target - (here + 1)) as u16);
            } else if desc.length > 1 {
                panic!("codegen not implemented for {op:?}");
            }
        }
        debug_assert_eq!(code.len(), pos);
        let f = &mut fns[fid];
        f.code = code;
        f.constants = constants;
        f.nlocals = (maxvar + 2) as usize;
        (0, children)
    }

    /// `block_compile`: compiles a bound program. Returns the top-level function, or
    /// the number of errors (their messages are in `self.messages`).
    pub fn block_compile(
        &mut self,
        b: Block,
        lf: LocFileId,
        globals: &mut Globals<'_>,
    ) -> Result<Rc<Bytecode>, usize> {
        let mut fns = vec![FnState {
            code: Vec::new(),
            nlocals: 0,
            nclosures: 0,
            constants: Vec::new(),
            debuginfo: DebugInfo::default(),
            subfunctions: Vec::new(),
            parent: None,
        }];
        let mut cfunctions = Vec::new();
        let mut nerrors = 0;
        // Depth-first, in subfunction order (the order jq reports errors in).
        let mut stack: Vec<(usize, Block)> = vec![(0, b)];
        while let Some((fid, body)) = stack.pop() {
            let (errors, children) =
                self.compile_fn(&mut fns, fid, body, lf, globals, &mut cfunctions);
            nerrors += errors;
            for &(inst, sid) in children.iter().rev() {
                let body = self.inst(inst).subfn;
                self.inst_mut(inst).subfn = Block::NOOP;
                stack.push((sid, body));
            }
        }
        if nerrors > 0 {
            return Err(nerrors);
        }
        Ok(build_bytecode(fns, &Rc::new(SymbolTable { cfunctions })))
    }
}

/// Builds the `Rc<Bytecode>` tree, bottom up, applying execute.c's tail-call
/// optimization to each function, then links the parents.
fn build_bytecode(fns: Vec<FnState>, globals: &Rc<SymbolTable>) -> Rc<Bytecode> {
    // Subfunction ids are always larger than their parent's.
    let mut built: Vec<Option<Rc<Bytecode>>> = vec![None; fns.len()];
    for (fid, mut f) in fns.into_iter().enumerate().rev() {
        bytecode::optimize_code(&mut f.code);
        let subfunctions = f
            .subfunctions
            .iter()
            .map(|&sid| built[sid].take().expect("subfunction built"))
            .collect();
        built[fid] = Some(Rc::new(Bytecode::new(
            f.code,
            f.nlocals,
            f.nclosures,
            f.constants,
            globals.clone(),
            subfunctions,
            f.debuginfo,
        )));
    }
    let root = built[0].take().expect("top-level function");
    bytecode::link_parents(&root);
    root
}
