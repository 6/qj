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
//! Left-associative chains (`1 + 1 + ... + 1`, `a, b, c, ...`, `.a.b.c...`) are
//! unbounded in jq (bison reduces them as it goes), so they are lowered with a loop.
//! Everything else nests through the parser's stack, which jq bounds at 10000 states,
//! and is lowered recursively.

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

/// The child that forms left-deep chains (lowered iteratively).
fn chain_child(n: &Node) -> Option<&Node> {
    match &n.kind {
        NodeKind::Binary { lhs, .. } => Some(lhs),
        NodeKind::Comma(a, _) | NodeKind::Pipe(a, _) => Some(a),
        NodeKind::Index {
            target: Some(t), ..
        } => Some(t),
        NodeKind::Each { target, .. } | NodeKind::Slice { target, .. } => Some(target),
        NodeKind::Optional(t) | NodeKind::Neg(t) => Some(t),
        _ => None,
    }
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
        let mut formals = Block::NOOP;
        for p in &d.params {
            let b = match p.kind {
                ParamKind::Value => self.c.gen_param_regular(&p.name),
                ParamKind::Filter => self.c.gen_param(&p.name),
            };
            formals = self.c.block_join(formals, b);
        }
        let body = self.lower(&d.body);
        self.c.gen_function(&d.name, formals, body)
    }

    /// Lowers a `Query`/`Expr`/`Term`.
    pub fn lower(&mut self, n: &Node) -> Block {
        let mut chain = Vec::new();
        let mut cur = n;
        while let Some(child) = chain_child(cur) {
            chain.push(cur);
            cur = child;
        }
        let mut acc = self.lower_leaf(cur);
        for node in chain.into_iter().rev() {
            acc = self.lower_on(node, acc);
        }
        acc
    }

    /// The action of a chain node, given its lowered chain child.
    fn lower_on(&mut self, n: &Node, first: Block) -> Block {
        match &n.kind {
            NodeKind::Binary { op, rhs, .. } => {
                let b = self.lower(rhs);
                self.binary(*op, first, b)
            }
            NodeKind::Comma(_, b) => {
                let b = self.lower(b);
                self.c.gen_both(first, b)
            }
            NodeKind::Pipe(_, b) => {
                let b = self.lower(b);
                self.c.block_join(first, b)
            }
            NodeKind::Index { key, optional, .. } => {
                let key = self.lower(key);
                self.gen_index(first, key, *optional)
            }
            NodeKind::Each { optional, .. } => {
                let each = self
                    .c
                    .gen_op_simple(if *optional { EACH_OPT } else { EACH });
                self.c.block_join(first, each)
            }
            NodeKind::Slice {
                from, to, optional, ..
            } => {
                let start = match from {
                    Some(f) => self.lower(f),
                    None => self.c.gen_const(Value::Null),
                };
                let end = match to {
                    Some(t) => self.lower(t),
                    None => self.c.gen_const(Value::Null),
                };
                self.gen_slice_index(first, start, end, if *optional { INDEX_OPT } else { INDEX })
            }
            NodeKind::Optional(_) => {
                let backtrack = self.c.gen_op_simple(BACKTRACK);
                self.c.gen_try(first, backtrack)
            }
            NodeKind::Neg(_) => {
                let neg = self.c.gen_call("_negate", Block::NOOP);
                self.c.block_join(first, neg)
            }
            _ => unreachable!("not a chain node"),
        }
    }

    /// Nodes without a chain child.
    fn lower_leaf(&mut self, n: &Node) -> Block {
        match &n.kind {
            NodeKind::FuncDef { def, rest } => {
                let d = self.lower_funcdef(def);
                let r = self.lower(rest);
                self.c.block_bind_referenced(d, r, OP_IS_CALL_PSEUDO)
            }
            NodeKind::As {
                source,
                patterns,
                body,
            } => {
                let source = self.lower(source);
                let matchers = self.lower_patterns(patterns);
                let body = self.lower(body);
                self.c.gen_destructure(source, matchers, body)
            }
            NodeKind::Label { name, body } => {
                let body = self.lower(body);
                let l = self.c.gen_label(&format!("*label-{name}"), body);
                self.c.gen_location(n.loc, self.lf, l)
            }
            NodeKind::Identity => Block::NOOP,
            NodeKind::Recurse => self.c.gen_call("recurse", Block::NOOP),
            NodeKind::Break(name) => {
                // impossible symbol
                let v = self.c.gen_op_unbound(LOADV, &format!("*label-{name}"));
                let e = self.c.gen_call("error", Block::NOOP);
                let b = self.c.block_join(v, e);
                self.c.gen_location(n.loc, self.lf, b)
            }
            NodeKind::Index {
                target: None,
                key,
                optional,
            } => {
                let key = self.lower(key);
                self.gen_index(Block::NOOP, key, *optional)
            }
            NodeKind::Literal(lit) => {
                let v = literal_value(lit);
                self.c.gen_const(v)
            }
            NodeKind::Str(s) => self.lower_string(s),
            NodeKind::Format(name) => self.gen_format(Block::NOOP, name),
            NodeKind::Array(q) => match q {
                Some(q) => {
                    let q = self.lower(q);
                    self.c.gen_collect(q)
                }
                None => self.c.gen_const(Value::empty_array()),
            },
            NodeKind::Object(pairs) => {
                let mut dp = Block::NOOP;
                for p in pairs {
                    let b = self.lower_dictpair(p);
                    dp = self.c.block_join(dp, b);
                }
                let o = self.c.gen_const_object(dp);
                if o.first.is_some() {
                    o
                } else {
                    let empty = self.c.gen_const(Value::empty_object());
                    let empty = self.c.gen_subexp(empty);
                    let pop = self.c.gen_op_simple(POP);
                    self.c.block3(empty, dp, pop)
                }
            }
            NodeKind::Reduce {
                source,
                patterns,
                init,
                update,
            } => {
                let source = self.lower(source);
                let matcher = self.lower_patterns(patterns);
                let init = self.lower(init);
                let update = self.lower(update);
                self.c.gen_reduce(source, matcher, init, update)
            }
            NodeKind::Foreach {
                source,
                patterns,
                init,
                update,
                extract,
            } => {
                let source = self.lower(source);
                let matcher = self.lower_patterns(patterns);
                let init = self.lower(init);
                let update = self.lower(update);
                let extract = match extract {
                    Some(e) => self.lower(e),
                    None => Block::NOOP,
                };
                self.c.gen_foreach(source, matcher, init, update, extract)
            }
            NodeKind::If { cond, then_, else_ } => {
                let cond = self.lower(cond);
                let then_ = self.lower(then_);
                let else_ = match else_ {
                    Some(e) => self.lower(e),
                    None => Block::NOOP,
                };
                self.c.gen_cond(cond, then_, else_)
            }
            NodeKind::Try { body, handler } => {
                let body = self.lower(body);
                let handler = match handler {
                    Some(h) => self.lower(h),
                    None => self.c.gen_op_simple(BACKTRACK),
                };
                self.c.gen_try(body, handler)
            }
            NodeKind::VarTake(name) => {
                let b = self.c.gen_op_unbound(LOADVN, name);
                self.c.gen_location(n.loc, self.lf, b)
            }
            NodeKind::Var(name) => {
                let b = self.c.gen_op_unbound(LOADV, name);
                self.c.gen_location(n.loc, self.lf, b)
            }
            NodeKind::LocObject => self.gen_loc_object(n.loc),
            NodeKind::Call {
                name,
                args,
                name_loc,
            } => {
                let mut arglist = Block::NOOP;
                for a in args {
                    let a = self.lower(a);
                    let l = self.c.gen_lambda(a);
                    arglist = self.c.block_join(arglist, l);
                }
                let call = self.c.gen_call(name, arglist);
                self.c.gen_location(*name_loc, self.lf, call)
            }
            NodeKind::Error => Block::NOOP,
            // Chain nodes are handled by `lower`.
            NodeKind::Binary { .. }
            | NodeKind::Comma(..)
            | NodeKind::Pipe(..)
            | NodeKind::Index { .. }
            | NodeKind::Each { .. }
            | NodeKind::Slice { .. }
            | NodeKind::Optional(_)
            | NodeKind::Neg(_) => unreachable!("chain node in lower_leaf"),
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
    fn gen_format(&mut self, a: Block, fmt: &str) -> Block {
        let k = self.c.gen_const(Value::from(fmt));
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
        let lf = self.c.locfile(self.lf);
        let file = Value::from(lf.fname());
        let line = lf.get_line(loc.start) + 1;
        let mut o = Object::new();
        o.insert(Str::from("file"), file);
        o.insert(Str::from("line"), Value::number(line as f64));
        self.c.gen_const(Value::Object(o))
    }

    /// `String`: `QQString` folds its parts with `gen_binop(..., '+')` starting from
    /// `""`; interpolations go through `gen_format` with the string's format.
    pub fn lower_string(&mut self, s: &StringLit) -> Block {
        let fmt = s.format_name();
        let mut acc = self.c.gen_const(Value::from(""));
        for part in &s.parts {
            let b = match part {
                StrPart::Text(t) => self.c.gen_const(Value::from(t.as_str())),
                StrPart::Interp(q) => {
                    let q = self.lower(q);
                    self.gen_format(q, fmt)
                }
            };
            acc = self.gen_binop(acc, b, BinOp::Add);
        }
        acc
    }

    /// `Patterns`: `RepPatterns "?//" Pattern` or `Pattern`.
    fn lower_patterns(&mut self, pats: &[Pattern]) -> Block {
        let (last, alts) = pats.split_last().expect("at least one pattern");
        let mut acc = Block::NOOP;
        for p in alts {
            let m = self.lower_pattern(p);
            let alt = self.c.gen_destructure_alt(m);
            acc = self.c.block_join(acc, alt);
        }
        let last = self.lower_pattern(last);
        self.c.block_join(acc, last)
    }

    /// `Pattern`.
    fn lower_pattern(&mut self, p: &Pattern) -> Block {
        match &p.kind {
            PatternKind::Var(name) => self.c.gen_op_unbound(STOREV, name),
            PatternKind::Array(elems) => {
                let mut acc = Block::NOOP;
                for e in elems {
                    let m = self.lower_pattern(e);
                    acc = self.c.gen_array_matcher(acc, m);
                }
                let pop = self.c.gen_op_simple(POP);
                self.c.block_join(acc, pop)
            }
            PatternKind::Object(entries) => {
                let mut acc = Block::NOOP;
                for e in entries {
                    let m = self.lower_objpat(e);
                    acc = self.c.block_join(acc, m);
                }
                let pop = self.c.gen_op_simple(POP);
                self.c.block_join(acc, pop)
            }
        }
    }

    /// `ObjPat`.
    fn lower_objpat(&mut self, e: &ObjPat) -> Block {
        match &e.kind {
            ObjPatKind::Var(name) => {
                let k = self.c.gen_const(Value::from(name.as_str()));
                let store = self.c.gen_op_unbound(STOREV, name);
                self.c.gen_object_matcher(k, store)
            }
            ObjPatKind::VarPattern(name, pat) => {
                let k = self.c.gen_const(Value::from(name.as_str()));
                let dup = self.c.gen_op_simple(DUP);
                let store = self.c.gen_op_unbound(STOREV, name);
                let m = self.lower_pattern(pat);
                let curr = self.c.block3(dup, store, m);
                self.c.gen_object_matcher(k, curr)
            }
            ObjPatKind::Named(name, pat) => {
                let k = self.c.gen_const(Value::from(name.as_str()));
                let m = self.lower_pattern(pat);
                self.c.gen_object_matcher(k, m)
            }
            ObjPatKind::Str(key, pat) => {
                let k = self.lower_string(key);
                let m = self.lower_pattern(pat);
                self.c.gen_object_matcher(k, m)
            }
            ObjPatKind::Computed { key, pattern, .. } => {
                let k = self.lower(key);
                let m = self.lower_pattern(pattern);
                self.c.gen_object_matcher(k, m)
            }
            ObjPatKind::Error(pat) => self.lower_pattern(pat),
        }
    }

    /// `DictPair`.
    fn lower_dictpair(&mut self, p: &DictPair) -> Block {
        match &p.kind {
            DictPairKind::Named { key, value } => {
                let k = self.c.gen_const(Value::from(key.as_str()));
                let v = self.lower(value);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::Str { key, value } => {
                let k = self.lower_string(key);
                let v = self.lower(value);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::StrShorthand(key) => {
                let k = self.lower_string(key);
                let pop = self.c.gen_op_simple(POP);
                let d1 = self.c.gen_op_simple(DUP2);
                let d2 = self.c.gen_op_simple(DUP2);
                let idx = self.c.gen_op_simple(INDEX);
                let v = self.c.blocks(&[pop, d1, d2, idx]);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::VarKey { name, value } => {
                let k = self.c.gen_op_unbound(LOADV, name);
                let k = self.c.gen_location(p.loc, self.lf, k);
                let v = self.lower(value);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::Var(name) => {
                let k = self.c.gen_const(Value::from(name.as_str()));
                let v = self.c.gen_op_unbound(LOADV, name);
                let v = self.c.gen_location(p.loc, self.lf, v);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::NameShorthand(name) => {
                let k = self.c.gen_const(Value::from(name.as_str()));
                let kk = self.c.gen_const(Value::from(name.as_str()));
                let v = self.gen_index(Block::NOOP, kk, false);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::LocObject => {
                let k = self.c.gen_const(Value::from("__loc__"));
                let v = self.gen_loc_object(p.loc);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::Computed { key, value, .. } => {
                let k = self.lower(key);
                let v = self.lower(value);
                self.c.gen_dictpair(k, v)
            }
            DictPairKind::Error(value) => self.lower(value),
        }
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
