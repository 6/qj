//! Port of parser.y's semantic actions: lowers Track P's AST ([`super::ast`]) to
//! [`super::compile`] blocks, node by node, exactly as jq's actions build them while
//! parsing (constant folding, `gen_index`, `gen_update`, `gen_dictpair`,
//! `gen_destructure`, `$__loc__`, ...).
//!
//! Also implements [`ParseHooks`] ([`CompileHooks`]): the parse-time checks that need
//! the lowered block (`check_object_key`, module metadata), so they report exactly
//! what jq reports.
//!
//! # Depth
//!
//! jq runs these actions as bison reduces, so it never recurses over the tree. Here
//! the tree is walked in post-order with an explicit stack ([`Lowerer::run`]): a
//! node's action runs once its children's blocks are on the result stack. Nesting
//! is bounded only by the parser (about 10000 states), and left-associative chains
//! (`1 + 1 + ... + 1`, `.a.b.c...`) are unbounded, so recursion could overflow a
//! thread's stack.

use super::ast::*;
use super::bytecode::OP_IS_CALL_PSEUDO;
use super::bytecode::Opcode::{self, *};
use super::compile::{Block, Compiler, LocFileId};
use super::parser::ParseHooks;
use crate::jq::builtins::binops;
use crate::jq::value::{Object, Str, Value, dump_string_trunc, parse_sized};

/// Lowers AST nodes of one source file (`locations` in parser.y).
pub struct Lowerer<'a> {
    pub c: &'a mut Compiler,
    /// The file being lowered: locations of `$var`, calls and `$__loc__` refer to it.
    pub lf: LocFileId,
}

/// Something with an action: a node, or one of the grammar's other nonterminals.
#[derive(Clone, Copy)]
enum Item<'a> {
    Node(&'a Node),
    FuncDef(&'a FuncDef),
    Str(&'a StringLit),
    Patterns(&'a [Pattern]),
    Pattern(&'a Pattern),
    ObjPat(&'a ObjPat),
    DictPair(&'a DictPair),
}

enum Task<'a> {
    /// Push the item's children (then its `Finish`), or its block if it has none.
    Visit(Item<'a>),
    /// The children's blocks are on the result stack: run the action.
    Finish(Item<'a>),
}

/// Pops the last `n` results, in order.
fn pop_n(results: &mut Vec<Block>, n: usize) -> Vec<Block> {
    let at = results.len() - n;
    results.split_off(at)
}

fn pop(results: &mut Vec<Block>) -> Block {
    results.pop().expect("lowering: missing child block")
}

impl Lowerer<'_> {
    pub fn new(c: &mut Compiler, lf: LocFileId) -> Lowerer<'_> {
        Lowerer { c, lf }
    }

    /// `TopLevel`: `BLOCK(module, imports, TOP, query)` for a main program, or
    /// `BLOCK(module, imports, funcdefs)` for a library.
    pub fn lower_program(&mut self, p: &Program) -> Block {
        let module = match &p.module {
            Some(m) => {
                let meta = self.lower(&m.meta);
                self.c.gen_module(meta)
            }
            None => Block::NOOP,
        };
        let mut imports = Block::NOOP;
        for imp in &p.imports {
            let b = self.lower_import(imp);
            imports = self.c.block_join(imports, b);
        }
        match &p.body {
            ProgramBody::Main(q) => {
                let top = self.c.gen_op_simple(TOP);
                let q = self.lower(q);
                self.c.blocks(&[module, imports, top, q])
            }
            ProgramBody::Library(defs) => {
                let mut fs = Block::NOOP;
                for d in defs {
                    let f = self.lower_funcdef(d);
                    fs = self.c.block_join(fs, f);
                }
                self.c.block3(module, imports, fs)
            }
        }
    }

    /// `Import`.
    fn lower_import(&mut self, imp: &Import) -> Block {
        let b = match &imp.kind {
            ImportKind::Data(name) => self.c.gen_import(&imp.path, Some(name), true),
            ImportKind::Code(name) => self.c.gen_import(&imp.path, Some(name), false),
            ImportKind::Include => self.c.gen_import(&imp.path, None, false),
        };
        match &imp.meta {
            Some(meta) => {
                let meta = self.lower(meta);
                self.c.gen_import_meta(b, meta)
            }
            None => b,
        }
    }

    /// `FuncDef`: `gen_function(name, params, body)`.
    pub fn lower_funcdef(&mut self, d: &FuncDef) -> Block {
        self.run(Item::FuncDef(d))
    }

    /// Lowers a `Query`/`Expr`/`Term`.
    pub fn lower(&mut self, n: &Node) -> Block {
        self.run(Item::Node(n))
    }

    /// The post-order walk.
    fn run(&mut self, root: Item<'_>) -> Block {
        let mut tasks = vec![Task::Visit(root)];
        let mut results: Vec<Block> = Vec::new();
        while let Some(task) = tasks.pop() {
            match task {
                Task::Visit(item) => {
                    if let Some(b) = self.leaf(item) {
                        results.push(b);
                        continue;
                    }
                    tasks.push(Task::Finish(item));
                    let mark = tasks.len();
                    push_children(item, &mut tasks);
                    // Children run left to right.
                    tasks[mark..].reverse();
                }
                Task::Finish(item) => {
                    let b = self.finish(item, &mut results);
                    results.push(b);
                }
            }
        }
        debug_assert_eq!(results.len(), 1);
        pop(&mut results)
    }

    /// The block of an item without children, or `None`.
    fn leaf(&mut self, item: Item<'_>) -> Option<Block> {
        let c = &mut *self.c;
        Some(match item {
            Item::Node(n) => match &n.kind {
                NodeKind::Identity | NodeKind::Error => Block::NOOP,
                NodeKind::Recurse => c.gen_call("recurse", Block::NOOP),
                NodeKind::Break(name) => {
                    // impossible symbol
                    let v = c.gen_op_unbound(LOADV, &format!("*label-{name}"));
                    let e = c.gen_call("error", Block::NOOP);
                    let b = c.block_join(v, e);
                    c.gen_location(n.loc, self.lf, b)
                }
                NodeKind::Literal(lit) => c.gen_const(literal_value(lit)),
                NodeKind::Format(name) => self.gen_format(Block::NOOP, Value::from(name.as_str())),
                NodeKind::Array(None) => c.gen_const(Value::empty_array()),
                NodeKind::VarTake(name) => {
                    let b = c.gen_op_unbound(LOADVN, name);
                    c.gen_location(n.loc, self.lf, b)
                }
                NodeKind::Var(name) => {
                    let b = c.gen_op_unbound(LOADV, name);
                    c.gen_location(n.loc, self.lf, b)
                }
                NodeKind::LocObject => self.gen_loc_object(n.loc),
                _ => return None,
            },
            Item::Pattern(p) => match &p.kind {
                PatternKind::Var(name) => c.gen_op_unbound(STOREV, name),
                _ => return None,
            },
            Item::ObjPat(e) => match &e.kind {
                ObjPatKind::Var(name) => {
                    let k = c.gen_const(Value::from(name.as_str()));
                    let store = c.gen_op_unbound(STOREV, name);
                    c.gen_object_matcher(k, store)
                }
                _ => return None,
            },
            Item::DictPair(p) => match &p.kind {
                DictPairKind::Var(name) => {
                    let k = c.gen_const(Value::from(name.as_str()));
                    let v = c.gen_op_unbound(LOADV, name);
                    let v = c.gen_location(p.loc, self.lf, v);
                    c.gen_dictpair(k, v)
                }
                DictPairKind::NameShorthand(name) => {
                    // `gen_const(jv_copy($1))` and `gen_const($1)`: both constants are
                    // the same string (observable in `--debug-trace=all` refcounts).
                    let name = Value::from(name.as_str());
                    let k = c.gen_const(name.clone());
                    let kk = c.gen_const(name);
                    let v = self.gen_index(Block::NOOP, kk, false);
                    self.c.gen_dictpair(k, v)
                }
                DictPairKind::LocObject => {
                    let k = c.gen_const(Value::from("__loc__"));
                    let v = self.gen_loc_object(p.loc);
                    self.c.gen_dictpair(k, v)
                }
                _ => return None,
            },
            Item::FuncDef(_) | Item::Str(_) | Item::Patterns(_) => return None,
        })
    }

    /// The action of an item whose children's blocks are on `results`.
    fn finish(&mut self, item: Item<'_>, results: &mut Vec<Block>) -> Block {
        match item {
            Item::Node(n) => self.finish_node(n, results),
            Item::FuncDef(d) => {
                let body = pop(results);
                let mut formals = Block::NOOP;
                for p in &d.params {
                    let b = match p.kind {
                        ParamKind::Value => self.c.gen_param_regular(&p.name),
                        ParamKind::Filter => self.c.gen_param(&p.name),
                    };
                    formals = self.c.block_join(formals, b);
                }
                self.c.gen_function(&d.name, formals, body)
            }
            Item::Str(s) => {
                // QQString: fold the parts with `+`, starting from "".
                let ninterp = s
                    .parts
                    .iter()
                    .filter(|p| matches!(p, StrPart::Interp(_)))
                    .count();
                let mut interps = pop_n(results, ninterp).into_iter();
                // Every interpolation's format constant is the same string
                // (`jv_copy($<literal>0)`).
                let fmt = Value::from(s.format_name());
                let mut acc = self.c.gen_const(Value::from(""));
                for part in &s.parts {
                    let b = match part {
                        StrPart::Text(t) => self.c.gen_const(Value::from(t.as_str())),
                        StrPart::Interp(_) => {
                            let q = interps.next().unwrap();
                            self.gen_format(q, fmt.clone())
                        }
                    };
                    acc = self.gen_binop(acc, b, BinOp::Add);
                }
                acc
            }
            Item::Patterns(pats) => {
                // RepPatterns "?//" Pattern: each alternative but the last becomes a
                // DESTRUCTURE_ALT.
                let ms = pop_n(results, pats.len());
                let (last, alts) = ms.split_last().expect("at least one pattern");
                let mut acc = Block::NOOP;
                for &m in alts {
                    let alt = self.c.gen_destructure_alt(m);
                    acc = self.c.block_join(acc, alt);
                }
                self.c.block_join(acc, *last)
            }
            Item::Pattern(p) => match &p.kind {
                PatternKind::Array(elems) => {
                    let ms = pop_n(results, elems.len());
                    let mut acc = Block::NOOP;
                    for m in ms {
                        acc = self.c.gen_array_matcher(acc, m);
                    }
                    let pop = self.c.gen_op_simple(POP);
                    self.c.block_join(acc, pop)
                }
                PatternKind::Object(entries) => {
                    let ms = pop_n(results, entries.len());
                    let acc = self.c.blocks(&ms);
                    let pop = self.c.gen_op_simple(POP);
                    self.c.block_join(acc, pop)
                }
                PatternKind::Var(_) => unreachable!("leaf"),
            },
            Item::ObjPat(e) => match &e.kind {
                ObjPatKind::VarPattern(name, _) => {
                    let m = pop(results);
                    let k = self.c.gen_const(Value::from(name.as_str()));
                    let dup = self.c.gen_op_simple(DUP);
                    let store = self.c.gen_op_unbound(STOREV, name);
                    let curr = self.c.block3(dup, store, m);
                    self.c.gen_object_matcher(k, curr)
                }
                ObjPatKind::Named(name, _) => {
                    let m = pop(results);
                    let k = self.c.gen_const(Value::from(name.as_str()));
                    self.c.gen_object_matcher(k, m)
                }
                ObjPatKind::Str(..) | ObjPatKind::Computed { .. } => {
                    let m = pop(results);
                    let k = pop(results);
                    self.c.gen_object_matcher(k, m)
                }
                ObjPatKind::Error(_) => pop(results),
                ObjPatKind::Var(_) => unreachable!("leaf"),
            },
            Item::DictPair(p) => match &p.kind {
                DictPairKind::Named { key, .. } => {
                    let v = pop(results);
                    let k = self.c.gen_const(Value::from(key.as_str()));
                    self.c.gen_dictpair(k, v)
                }
                DictPairKind::Str { .. } | DictPairKind::Computed { .. } => {
                    let v = pop(results);
                    let k = pop(results);
                    self.c.gen_dictpair(k, v)
                }
                DictPairKind::StrShorthand(_) => {
                    let k = pop(results);
                    let pop_ = self.c.gen_op_simple(POP);
                    let d1 = self.c.gen_op_simple(DUP2);
                    let d2 = self.c.gen_op_simple(DUP2);
                    let idx = self.c.gen_op_simple(INDEX);
                    let v = self.c.blocks(&[pop_, d1, d2, idx]);
                    self.c.gen_dictpair(k, v)
                }
                DictPairKind::VarKey { name, .. } => {
                    let v = pop(results);
                    let k = self.c.gen_op_unbound(LOADV, name);
                    let k = self.c.gen_location(p.loc, self.lf, k);
                    self.c.gen_dictpair(k, v)
                }
                DictPairKind::Error(_) => pop(results),
                DictPairKind::Var(_) | DictPairKind::NameShorthand(_) | DictPairKind::LocObject => {
                    unreachable!("leaf")
                }
            },
        }
    }

    fn finish_node(&mut self, n: &Node, results: &mut Vec<Block>) -> Block {
        match &n.kind {
            NodeKind::FuncDef { .. } => {
                let rest = pop(results);
                let def = pop(results);
                self.c.block_bind_referenced(def, rest, OP_IS_CALL_PSEUDO)
            }
            NodeKind::As { .. } => {
                let body = pop(results);
                let matchers = pop(results);
                let source = pop(results);
                self.c.gen_destructure(source, matchers, body)
            }
            NodeKind::Label { name, .. } => {
                let body = pop(results);
                let l = self.c.gen_label(&format!("*label-{name}"), body);
                self.c.gen_location(n.loc, self.lf, l)
            }
            NodeKind::Pipe(..) => {
                let b = pop(results);
                let a = pop(results);
                self.c.block_join(a, b)
            }
            NodeKind::Comma(..) => {
                let b = pop(results);
                let a = pop(results);
                self.c.gen_both(a, b)
            }
            NodeKind::Binary { op, .. } => {
                let b = pop(results);
                let a = pop(results);
                self.binary(*op, a, b)
            }
            NodeKind::Index {
                target, optional, ..
            } => {
                let key = pop(results);
                let obj = match target {
                    Some(_) => pop(results),
                    None => Block::NOOP,
                };
                self.gen_index(obj, key, *optional)
            }
            NodeKind::Each { optional, .. } => {
                let t = pop(results);
                let each = self
                    .c
                    .gen_op_simple(if *optional { EACH_OPT } else { EACH });
                self.c.block_join(t, each)
            }
            NodeKind::Slice {
                from, to, optional, ..
            } => {
                let end = match to {
                    Some(_) => pop(results),
                    None => self.c.gen_const(Value::Null),
                };
                let start = match from {
                    Some(_) => pop(results),
                    None => self.c.gen_const(Value::Null),
                };
                let obj = pop(results);
                self.gen_slice_index(obj, start, end, if *optional { INDEX_OPT } else { INDEX })
            }
            NodeKind::Optional(_) => {
                let t = pop(results);
                let backtrack = self.c.gen_op_simple(BACKTRACK);
                self.c.gen_try(t, backtrack)
            }
            NodeKind::Str(_) => pop(results),
            NodeKind::Neg(_) => {
                let t = pop(results);
                let neg = self.c.gen_call("_negate", Block::NOOP);
                self.c.block_join(t, neg)
            }
            NodeKind::Array(Some(_)) => {
                let q = pop(results);
                self.c.gen_collect(q)
            }
            NodeKind::Object(pairs) => {
                let ps = pop_n(results, pairs.len());
                let dp = self.c.blocks(&ps);
                let o = self.c.gen_const_object(dp);
                if o.first.is_some() {
                    o
                } else {
                    let empty = self.c.gen_const(Value::empty_object());
                    let empty = self.c.gen_subexp(empty);
                    let pop_ = self.c.gen_op_simple(POP);
                    self.c.block3(empty, dp, pop_)
                }
            }
            NodeKind::Reduce { .. } => {
                let update = pop(results);
                let init = pop(results);
                let matcher = pop(results);
                let source = pop(results);
                self.c.gen_reduce(source, matcher, init, update)
            }
            NodeKind::Foreach { extract, .. } => {
                let extract = match extract {
                    Some(_) => pop(results),
                    None => Block::NOOP,
                };
                let update = pop(results);
                let init = pop(results);
                let matcher = pop(results);
                let source = pop(results);
                self.c.gen_foreach(source, matcher, init, update, extract)
            }
            NodeKind::If { else_, .. } => {
                let else_ = match else_ {
                    Some(_) => pop(results),
                    None => Block::NOOP,
                };
                let then_ = pop(results);
                let cond = pop(results);
                self.c.gen_cond(cond, then_, else_)
            }
            NodeKind::Try { handler, .. } => {
                let handler = match handler {
                    Some(_) => pop(results),
                    None => self.c.gen_op_simple(BACKTRACK),
                };
                let body = pop(results);
                self.c.gen_try(body, handler)
            }
            NodeKind::Call {
                name,
                args,
                name_loc,
            } => {
                let bodies = pop_n(results, args.len());
                let mut arglist = Block::NOOP;
                for a in bodies {
                    let l = self.c.gen_lambda(a);
                    arglist = self.c.block_join(arglist, l);
                }
                let call = self.c.gen_call(name, arglist);
                self.c.gen_location(*name_loc, self.lf, call)
            }
            NodeKind::Identity
            | NodeKind::Recurse
            | NodeKind::Break(_)
            | NodeKind::Literal(_)
            | NodeKind::Format(_)
            | NodeKind::Array(None)
            | NodeKind::VarTake(_)
            | NodeKind::Var(_)
            | NodeKind::LocObject
            | NodeKind::Error => unreachable!("leaf"),
        }
    }

    /// `Expr op Expr` (rules 16-37).
    fn binary(&mut self, op: BinOp, a: Block, b: Block) -> Block {
        match op {
            BinOp::Alt => self.c.gen_definedor(a, b),
            BinOp::Assign => {
                let la = self.c.gen_lambda(a);
                let lb = self.c.gen_lambda(b);
                let args = self.c.block_join(la, lb);
                self.c.gen_call("_assign", args)
            }
            BinOp::Or => self.c.gen_or(a, b),
            BinOp::And => self.c.gen_and(a, b),
            BinOp::AltAssign => self.gen_definedor_assign(a, b),
            BinOp::Update => {
                let la = self.c.gen_lambda(a);
                let lb = self.c.gen_lambda(b);
                let args = self.c.block_join(la, lb);
                self.c.gen_call("_modify", args)
            }
            BinOp::AddAssign
            | BinOp::SubAssign
            | BinOp::MulAssign
            | BinOp::DivAssign
            | BinOp::ModAssign => self.gen_update(a, b, op.update_arith().unwrap()),
            _ => self.gen_binop(a, b, op),
        }
    }

    /// parser.y `constant_fold`.
    fn constant_fold(&mut self, a: Block, b: Block, op: BinOp) -> Block {
        let c = &mut *self.c;
        if !c.block_is_single(a)
            || !c.block_is_const(a)
            || !c.block_is_single(b)
            || !c.block_is_const(b)
        {
            return Block::NOOP;
        }
        let ja = c.block_const(a);
        let jb = c.block_const(b);
        let res = match op {
            BinOp::Add => binops::binop_plus(ja, jb),
            BinOp::Sub => binops::binop_minus(ja, jb),
            BinOp::Mul => binops::binop_multiply(ja, jb),
            BinOp::Div => binops::binop_divide(ja, jb),
            BinOp::Mod => binops::binop_mod(ja, jb),
            BinOp::Eq => binops::binop_equal(ja, jb),
            BinOp::Ne => binops::binop_notequal(ja, jb),
            BinOp::Lt => binops::binop_less(ja, jb),
            BinOp::Gt => binops::binop_greater(ja, jb),
            BinOp::Le => binops::binop_lesseq(ja, jb),
            BinOp::Ge => binops::binop_greatereq(ja, jb),
            // (jq leaves `res` invalid without a message for other operators)
            _ => return c.gen_error(Value::Null),
        };
        match res {
            Ok(v) => c.gen_const(v),
            Err(e) => c.gen_error(e.into_value()),
        }
    }

    /// parser.y `gen_binop`.
    fn gen_binop(&mut self, a: Block, b: Block, op: BinOp) -> Block {
        let folded = self.constant_fold(a, b, op);
        if !folded.is_noop() {
            return folded;
        }
        let funcname = match op {
            BinOp::Add => "_plus",
            BinOp::Sub => "_minus",
            BinOp::Mul => "_multiply",
            BinOp::Div => "_divide",
            BinOp::Mod => "_mod",
            BinOp::Eq => "_equal",
            BinOp::Ne => "_notequal",
            BinOp::Lt => "_less",
            BinOp::Gt => "_greater",
            BinOp::Le => "_lesseq",
            BinOp::Ge => "_greatereq",
            _ => unreachable!("gen_binop: {op:?}"),
        };
        let la = self.c.gen_lambda(a);
        let lb = self.c.gen_lambda(b);
        let args = self.c.block_join(la, lb);
        self.c.gen_call(funcname, args)
    }

    /// parser.y `gen_format`.
    fn gen_format(&mut self, a: Block, fmt: Value) -> Block {
        let k = self.c.gen_const(fmt);
        let l = self.c.gen_lambda(k);
        let call = self.c.gen_call("format", l);
        self.c.block_join(a, call)
    }

    /// parser.y `gen_definedor_assign`: `a //= b`.
    fn gen_definedor_assign(&mut self, object: Block, val: Block) -> Block {
        let tmp = self.c.gen_op_var_fresh(STOREV, "tmp");
        let dup = self.c.gen_op_simple(DUP);
        let lo = self.c.gen_lambda(object);
        let load = self.c.gen_op_bound(LOADV, tmp);
        let d = self.c.gen_definedor(Block::NOOP, load);
        let ld = self.c.gen_lambda(d);
        let args = self.c.block_join(lo, ld);
        let call = self.c.gen_call("_modify", args);
        self.c.blocks(&[dup, val, tmp, call])
    }

    /// parser.y `gen_update`: `a op= b`.
    fn gen_update(&mut self, object: Block, val: Block, op: BinOp) -> Block {
        let tmp = self.c.gen_op_var_fresh(STOREV, "tmp");
        let dup = self.c.gen_op_simple(DUP);
        let lo = self.c.gen_lambda(object);
        let load = self.c.gen_op_bound(LOADV, tmp);
        let bin = self.gen_binop(Block::NOOP, load, op);
        let lb = self.c.gen_lambda(bin);
        let args = self.c.block_join(lo, lb);
        let call = self.c.gen_call("_modify", args);
        self.c.blocks(&[dup, val, tmp, call])
    }

    /// parser.y `gen_index` / `gen_index_opt`.
    fn gen_index(&mut self, obj: Block, key: Block, optional: bool) -> Block {
        let key = self.c.gen_subexp(key);
        let idx = self
            .c
            .gen_op_simple(if optional { INDEX_OPT } else { INDEX });
        self.c.block3(key, obj, idx)
    }

    /// parser.y `gen_slice_index`.
    fn gen_slice_index(&mut self, obj: Block, start: Block, end: Block, idx_op: Opcode) -> Block {
        let o = self.c.gen_const(Value::empty_object());
        let o = self.c.gen_subexp(o);
        let ks = self.c.gen_const(Value::from("start"));
        let ks = self.c.gen_subexp(ks);
        let start = self.c.gen_subexp(start);
        let ins1 = self.c.gen_op_simple(INSERT);
        let ke = self.c.gen_const(Value::from("end"));
        let ke = self.c.gen_subexp(ke);
        let end = self.c.gen_subexp(end);
        let ins2 = self.c.gen_op_simple(INSERT);
        let key = self.c.blocks(&[o, ks, start, ins1, ke, end, ins2]);
        let idx = self.c.gen_op_simple(idx_op);
        self.c.block3(key, obj, idx)
    }

    /// parser.y `gen_loc_object`: `{"file": <locfile name>, "line": N}`.
    fn gen_loc_object(&mut self, loc: Loc) -> Block {
        // `jv_copy(locations->fname)`: one string per source file.
        let file = self.c.locfile_name(self.lf);
        let line = self.c.locfile(self.lf).get_line(loc.start) + 1;
        let mut o = Object::new();
        o.insert(Str::from("file"), file);
        o.insert(Str::from("line"), Value::number(line as f64));
        self.c.gen_const(Value::Object(o))
    }
}

/// Pushes `Visit` tasks for an item's children, left to right.
fn push_children<'a>(item: Item<'a>, tasks: &mut Vec<Task<'a>>) {
    fn visit<'a>(tasks: &mut Vec<Task<'a>>, n: &'a Node) {
        tasks.push(Task::Visit(Item::Node(n)));
    }
    match item {
        Item::Node(n) => match &n.kind {
            NodeKind::FuncDef { def, rest } => {
                tasks.push(Task::Visit(Item::FuncDef(def)));
                tasks.push(Task::Visit(Item::Node(rest)));
            }
            NodeKind::As {
                source,
                patterns,
                body,
            } => {
                tasks.push(Task::Visit(Item::Node(source)));
                tasks.push(Task::Visit(Item::Patterns(patterns)));
                tasks.push(Task::Visit(Item::Node(body)));
            }
            NodeKind::Label { body, .. } => visit(tasks, body),
            NodeKind::Pipe(a, b) | NodeKind::Comma(a, b) => {
                visit(tasks, a);
                visit(tasks, b);
            }
            NodeKind::Binary { lhs, rhs, .. } => {
                visit(tasks, lhs);
                visit(tasks, rhs);
            }
            NodeKind::Index { target, key, .. } => {
                if let Some(t) = target {
                    visit(tasks, t);
                }
                visit(tasks, key);
            }
            NodeKind::Each { target, .. } | NodeKind::Optional(target) | NodeKind::Neg(target) => {
                visit(tasks, target)
            }
            NodeKind::Slice {
                target, from, to, ..
            } => {
                visit(tasks, target);
                if let Some(f) = from {
                    visit(tasks, f);
                }
                if let Some(t) = to {
                    visit(tasks, t);
                }
            }
            NodeKind::Str(s) => tasks.push(Task::Visit(Item::Str(s))),
            NodeKind::Array(Some(q)) => visit(tasks, q),
            NodeKind::Object(pairs) => {
                for p in pairs {
                    tasks.push(Task::Visit(Item::DictPair(p)));
                }
            }
            NodeKind::Reduce {
                source,
                patterns,
                init,
                update,
            } => {
                tasks.push(Task::Visit(Item::Node(source)));
                tasks.push(Task::Visit(Item::Patterns(patterns)));
                tasks.push(Task::Visit(Item::Node(init)));
                tasks.push(Task::Visit(Item::Node(update)));
            }
            NodeKind::Foreach {
                source,
                patterns,
                init,
                update,
                extract,
            } => {
                tasks.push(Task::Visit(Item::Node(source)));
                tasks.push(Task::Visit(Item::Patterns(patterns)));
                tasks.push(Task::Visit(Item::Node(init)));
                tasks.push(Task::Visit(Item::Node(update)));
                if let Some(e) = extract {
                    tasks.push(Task::Visit(Item::Node(e)));
                }
            }
            NodeKind::If { cond, then_, else_ } => {
                visit(tasks, cond);
                visit(tasks, then_);
                if let Some(e) = else_ {
                    visit(tasks, e);
                }
            }
            NodeKind::Try { body, handler } => {
                visit(tasks, body);
                if let Some(h) = handler {
                    visit(tasks, h);
                }
            }
            NodeKind::Call { args, .. } => {
                for a in args {
                    visit(tasks, a);
                }
            }
            _ => {}
        },
        Item::FuncDef(d) => visit(tasks, &d.body),
        Item::Str(s) => {
            for part in &s.parts {
                if let StrPart::Interp(q) = part {
                    visit(tasks, q);
                }
            }
        }
        Item::Patterns(ps) => {
            for p in ps {
                tasks.push(Task::Visit(Item::Pattern(p)));
            }
        }
        Item::Pattern(p) => match &p.kind {
            PatternKind::Array(elems) => {
                for e in elems {
                    tasks.push(Task::Visit(Item::Pattern(e)));
                }
            }
            PatternKind::Object(entries) => {
                for e in entries {
                    tasks.push(Task::Visit(Item::ObjPat(e)));
                }
            }
            PatternKind::Var(_) => {}
        },
        Item::ObjPat(e) => match &e.kind {
            ObjPatKind::VarPattern(_, pat) | ObjPatKind::Named(_, pat) | ObjPatKind::Error(pat) => {
                tasks.push(Task::Visit(Item::Pattern(pat)))
            }
            ObjPatKind::Str(key, pat) => {
                tasks.push(Task::Visit(Item::Str(key)));
                tasks.push(Task::Visit(Item::Pattern(pat)));
            }
            ObjPatKind::Computed { key, pattern, .. } => {
                tasks.push(Task::Visit(Item::Node(key)));
                tasks.push(Task::Visit(Item::Pattern(pattern)));
            }
            ObjPatKind::Var(_) => {}
        },
        Item::DictPair(p) => match &p.kind {
            DictPairKind::Named { value, .. }
            | DictPairKind::VarKey { value, .. }
            | DictPairKind::Error(value) => visit(tasks, value),
            DictPairKind::Str { key, value } => {
                tasks.push(Task::Visit(Item::Str(key)));
                tasks.push(Task::Visit(Item::Node(value)));
            }
            DictPairKind::StrShorthand(key) => tasks.push(Task::Visit(Item::Str(key))),
            DictPairKind::Computed { key, value, .. } => {
                tasks.push(Task::Visit(Item::Node(key)));
                tasks.push(Task::Visit(Item::Node(value)));
            }
            DictPairKind::Var(_) | DictPairKind::NameShorthand(_) | DictPairKind::LocObject => {}
        },
    }
}

/// The constant of a `LITERAL` (`jv_parse_sized` of its text), `true`, `false`,
/// `null` or a field name.
fn literal_value(lit: &Literal) -> Value {
    match lit {
        Literal::Null => Value::Null,
        Literal::True => Value::Bool(true),
        Literal::False => Value::Bool(false),
        Literal::Number(text) => parse_sized(text.as_bytes()).unwrap_or(Value::Null),
        Literal::String(s) => Value::from(s.as_str()),
    }
}

/// The compiler's [`ParseHooks`]: `check_object_key` and the module-metadata checks,
/// judged on the lowered (constant-folded) block like jq's actions do.
pub struct CompileHooks<'a> {
    pub c: &'a mut Compiler,
    pub lf: LocFileId,
}

impl ParseHooks for CompileHooks<'_> {
    fn check_object_key(&mut self, key: &Node) -> Option<String> {
        let b = Lowerer::new(self.c, self.lf).lower(key);
        if self.c.block_is_const(b) {
            let k = self.c.block_const(b);
            if !matches!(k, Value::String(_)) {
                return Some(format!(
                    "Cannot use {} ({}) as object key",
                    k.kind_name(),
                    dump_string_trunc(&k, 15)
                ));
            }
        }
        None
    }

    fn check_metadata(&mut self, meta: &Node) -> Option<String> {
        let b = Lowerer::new(self.c, self.lf).lower(meta);
        if !self.c.block_is_const(b) {
            Some("Module metadata must be constant".into())
        } else if !matches!(self.c.block_const(b), Value::Object(_)) {
            Some("Module metadata must be an object".into())
        } else {
            None
        }
    }
}
