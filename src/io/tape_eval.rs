//! Running simple programs on simdjson's tape, without building jq values.
//!
//! A program made only of paths (`.a.b`, `."a"`, `.["a"]`), iteration
//! (`.[]`), pipes, `length`, `keys`, `keys_unsorted`, array collection
//! (`[...]`, `map(...)`), object construction with constant keys (`{a, b:
//! .c.d}`) and `select` on a path's truthiness or its equality with a constant
//! (`select(.type == "PushEvent")`) has outputs that are fully determined by
//! its input, and can be computed on the tape ([`super::tape`]). Those outputs
//! are what the jq VM would produce on the value the builder makes of the
//! input: every step follows the builtin or opcode it stands for (`jv_get` on
//! objects, `EACH`, `f_length`, `f_keys`, `jv_equal`, ...), and duplicate keys
//! follow jq's rule.
//!
//! Anything else is [`Decline`]d: an error in jq (`.a` on a number, `length`
//! of a boolean, ...), or a document the tape view can't handle. The caller
//! must then run the program on the input's value as usual, from scratch:
//! nothing is output until a whole input has been evaluated, so a declined
//! input's partial results are simply dropped, and the VM reproduces
//! everything jq does (outputs, errors, their order).
//!
//! Which programs qualify is decided on the program text ([`TapeProgram::new`]);
//! callers must also check that the compiled program is jq's builtins only (no
//! definitions from `~/.jq` or modules shadow them) and that the output isn't
//! colored.

use std::cell::RefCell;

use crate::jq::lang::ast::{self, BinOp, DictPairKind, Literal, NodeKind, ProgramBody};
use crate::jq::lang::parser::{NoHooks, parse};
use crate::jq::value::Number;

use super::tape::{Doc, Layout, Node, NodeKind as Kind, Scratch};

/// The input can't be evaluated here (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decline;

/// A program that can run on the tape.
#[derive(Debug)]
pub struct TapeProgram {
    expr: Expr,
}

#[derive(Debug)]
enum Expr {
    /// `.`
    Identity,
    /// `T | .[key]` for a constant string key.
    Index(Box<Expr>, String),
    /// `T | .[]`
    Each(Box<Expr>),
    /// `A | B`
    Pipe(Box<Expr>, Box<Expr>),
    /// `length`
    Length,
    /// `keys` (sorted) or `keys_unsorted`.
    Keys { sorted: bool },
    /// `[E]`
    Collect(Box<Expr>),
    /// `{k: E, ...}` with distinct constant keys and single-valued `E`s.
    Object(Vec<(String, Expr)>),
    /// `select(E)` (truthiness) and `select(E == c)`, `select(E != c)` for
    /// a single-valued `E` and a constant `c`.
    Select(Box<Expr>, Test),
}

#[derive(Debug)]
enum Test {
    Truthy,
    Equal(Const),
    NotEqual(Const),
}

#[derive(Debug)]
enum Const {
    Null,
    False,
    True,
    /// A number literal's text (and whether it's negated), made into the
    /// literal jq's compiler makes of it when compared (numbers are `Rc`, so
    /// a program shared between threads keeps text).
    Number(String, bool),
    String(String),
}

/// A value computed on the tape.
#[derive(Clone, Debug)]
pub enum TVal<'p> {
    /// A value of the document.
    Node(Node),
    /// `null` (a missing key).
    Null,
    /// A computed number (`length`, `keys` of an array).
    Number(Number),
    /// A collected array.
    Array(Vec<TVal<'p>>),
    /// A constructed object: distinct keys, in order.
    Object(Vec<(&'p str, TVal<'p>)>),
}

impl TapeProgram {
    /// The program `text` compiles to, if it qualifies (see the module
    /// docs).
    pub fn new(text: &[u8]) -> Option<TapeProgram> {
        let program = parse(text, &mut NoHooks).ok()?;
        if program.module.is_some() || !program.imports.is_empty() {
            return None;
        }
        let ProgramBody::Main(node) = &program.body else {
            return None;
        };
        let expr = convert(node)?;
        Some(TapeProgram { expr })
    }

    /// Evaluates the program on a document's root, appending its outputs to
    /// `results`. On [`Decline`], `results` holds partial results to drop.
    pub fn eval<'p>(
        &'p self,
        doc: &Doc<'_>,
        scratch: &mut Scratch,
        results: &mut Vec<TVal<'p>>,
    ) -> Result<(), Decline> {
        let ev = Eval {
            doc,
            scratch: RefCell::new(scratch),
        };
        ev.run(&self.expr, TVal::Node(doc.root()), &mut |v| {
            results.push(v);
            Ok(())
        })
    }
}

/// The program's AST as an [`Expr`], if it qualifies.
fn convert(n: &ast::Node) -> Option<Expr> {
    Some(match &n.kind {
        NodeKind::Identity => Expr::Identity,
        NodeKind::Index {
            target,
            key,
            optional: false,
        } => {
            let key = const_string(key)?;
            let target = match target {
                None => Expr::Identity,
                Some(t) => convert(t)?,
            };
            Expr::Index(Box::new(target), key)
        }
        NodeKind::Each {
            target,
            optional: false,
        } => Expr::Each(Box::new(convert(target)?)),
        NodeKind::Pipe(a, b) => Expr::Pipe(Box::new(convert(a)?), Box::new(convert(b)?)),
        NodeKind::Call { name, args, .. } => match (name.as_str(), args.as_slice()) {
            ("length", []) => Expr::Length,
            ("keys", []) => Expr::Keys { sorted: true },
            ("keys_unsorted", []) => Expr::Keys { sorted: false },
            // builtin.jq: `def map(f): [.[] | f];`
            ("map", [f]) => Expr::Collect(Box::new(Expr::Pipe(
                Box::new(Expr::Each(Box::new(Expr::Identity))),
                Box::new(convert(f)?),
            ))),
            // builtin.jq: `def select(f): if f then . else empty end;`
            ("select", [f]) => select(f)?,
            _ => return None,
        },
        NodeKind::Array(Some(e)) => Expr::Collect(Box::new(convert(e)?)),
        NodeKind::Object(pairs) => {
            let mut entries: Vec<(String, Expr)> = Vec::with_capacity(pairs.len());
            for p in pairs {
                let (key, value) = match &p.kind {
                    DictPairKind::Named { key, value } => (key.clone(), convert(value)?),
                    DictPairKind::Str { key, value } => (const_string_lit(key)?, convert(value)?),
                    // `{a}` is `{a: .a}`, `{"a"}` is `{"a": .["a"]}`.
                    DictPairKind::NameShorthand(key) => (
                        key.clone(),
                        Expr::Index(Box::new(Expr::Identity), key.clone()),
                    ),
                    DictPairKind::StrShorthand(key) => {
                        let key = const_string_lit(key)?;
                        let value = Expr::Index(Box::new(Expr::Identity), key.clone());
                        (key, value)
                    }
                    _ => return None,
                };
                if !single(&value) || entries.iter().any(|(k, _)| *k == key) {
                    return None;
                }
                entries.push((key, value));
            }
            Expr::Object(entries)
        }
        _ => return None,
    })
}

/// `select(f)` for the tests handled here.
fn select(f: &ast::Node) -> Option<Expr> {
    if let NodeKind::Binary {
        op: op @ (BinOp::Eq | BinOp::Ne),
        lhs,
        rhs,
    } = &f.kind
    {
        // The constant may be on either side (`==` is symmetric in jq).
        let (e, c) = match (constant(lhs), constant(rhs)) {
            (None, Some(c)) => (lhs, c),
            (Some(c), None) => (rhs, c),
            _ => return None,
        };
        let e = convert(e)?;
        if !single(&e) {
            return None;
        }
        let test = if *op == BinOp::Eq {
            Test::Equal(c)
        } else {
            Test::NotEqual(c)
        };
        return Some(Expr::Select(Box::new(e), test));
    }
    let e = convert(f)?;
    if !single(&e) {
        return None;
    }
    Some(Expr::Select(Box::new(e), Test::Truthy))
}

/// Whether `e` always has exactly one output (unless it's an error).
fn single(e: &Expr) -> bool {
    match e {
        Expr::Identity | Expr::Length | Expr::Keys { .. } | Expr::Collect(_) => true,
        Expr::Index(t, _) => single(t),
        Expr::Pipe(a, b) => single(a) && single(b),
        Expr::Object(entries) => entries.iter().all(|(_, e)| single(e)),
        Expr::Each(_) | Expr::Select(..) => false,
    }
}

fn constant(n: &ast::Node) -> Option<Const> {
    Some(match &n.kind {
        NodeKind::Literal(Literal::Null) => Const::Null,
        NodeKind::Literal(Literal::False) => Const::False,
        NodeKind::Literal(Literal::True) => Const::True,
        NodeKind::Literal(Literal::Number(t)) => {
            Number::from_literal(t.as_bytes())?;
            Const::Number(t.clone(), false)
        }
        // `-1` is `1 | _negate` (not folded).
        NodeKind::Neg(inner) => match &inner.kind {
            NodeKind::Literal(Literal::Number(t)) => {
                Number::from_literal(t.as_bytes())?;
                Const::Number(t.clone(), true)
            }
            _ => return None,
        },
        NodeKind::Str(s) => Const::String(const_string_lit(s)?),
        _ => return None,
    })
}

fn const_string(key: &ast::Node) -> Option<String> {
    match &key.kind {
        NodeKind::Literal(Literal::String(s)) => Some(s.clone()),
        NodeKind::Str(s) => const_string_lit(s),
        _ => None,
    }
}

fn const_string_lit(s: &ast::StringLit) -> Option<String> {
    if s.format.is_some() {
        return None;
    }
    s.constant_value()
}

impl Const {
    /// `jv_equal(v, c)` for a value of `kind` (`number` gives it when it's a
    /// number, `string` when it's a string).
    fn equals(&self, kind: Kind, number: impl FnOnce() -> Number, string: &str) -> bool {
        match (self, kind) {
            (Const::Null, Kind::Null) | (Const::False, Kind::False) | (Const::True, Kind::True) => {
                true
            }
            (Const::Number(t, neg), Kind::Number) => {
                let mut c = Number::from_literal(t.as_bytes()).expect("checked");
                if *neg {
                    c = c.negate();
                }
                number().equal(&c)
            }
            (Const::String(s), Kind::String) => s == string,
            _ => false,
        }
    }
}

struct Eval<'d, 's> {
    doc: &'d Doc<'d>,
    scratch: RefCell<&'s mut Scratch>,
}

type Emit<'e, 'p> = dyn FnMut(TVal<'p>) -> Result<(), Decline> + 'e;

impl Eval<'_, '_> {
    fn run<'p>(
        &self,
        e: &'p Expr,
        input: TVal<'p>,
        emit: &mut Emit<'_, 'p>,
    ) -> Result<(), Decline> {
        match e {
            Expr::Identity => emit(input),
            Expr::Index(t, key) => {
                if single(t) {
                    let v = self.single(t, input)?;
                    emit(self.index(v, key)?)
                } else {
                    self.run(t, input, &mut |v| emit(self.index(v, key)?))
                }
            }
            Expr::Each(t) => {
                if single(t) {
                    let v = self.single(t, input)?;
                    self.each(v, emit)
                } else {
                    self.run(t, input, &mut |v| self.each(v, emit))
                }
            }
            Expr::Pipe(a, b) => self.run(a, input, &mut |v| self.run(b, v, emit)),
            Expr::Length => emit(self.length(input)?),
            Expr::Keys { sorted } => emit(self.keys(input, *sorted)?),
            Expr::Collect(inner) => {
                let mut items = Vec::new();
                self.run(inner, input, &mut |v| {
                    items.push(v);
                    Ok(())
                })?;
                emit(TVal::Array(items))
            }
            Expr::Object(entries) => {
                let mut out = Vec::with_capacity(entries.len());
                for (k, e) in entries {
                    let v = self.single(e, input.clone())?;
                    out.push((k.as_str(), v));
                }
                emit(TVal::Object(out))
            }
            Expr::Select(e, test) => {
                let v = self.single(e, input.clone())?;
                if self.test(&v, test) {
                    emit(input)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// The one output of a single-valued expression.
    fn single<'p>(&self, e: &'p Expr, input: TVal<'p>) -> Result<TVal<'p>, Decline> {
        match e {
            Expr::Identity => return Ok(input),
            Expr::Index(t, key) if matches!(**t, Expr::Identity) => return self.index(input, key),
            _ => {}
        }
        let mut out = None;
        let mut n = 0;
        self.run(e, input, &mut |v| {
            n += 1;
            out = Some(v);
            Ok(())
        })?;
        match (n, out) {
            (1, Some(v)) => Ok(v),
            _ => Err(Decline),
        }
    }

    /// `jv_get(v, key)` for a string key (the `INDEX` opcode).
    fn index<'p>(&self, v: TVal<'p>, key: &str) -> Result<TVal<'p>, Decline> {
        match v {
            TVal::Node(n) => match self.doc.kind(n) {
                Kind::Object => Ok(self.doc.get(n, key).map_or(TVal::Null, TVal::Node)),
                Kind::Null => Ok(TVal::Null),
                _ => Err(Decline),
            },
            TVal::Null => Ok(TVal::Null),
            TVal::Object(entries) => Ok(entries
                .into_iter()
                .find(|(k, _)| *k == key)
                .map_or(TVal::Null, |(_, v)| v)),
            TVal::Number(_) | TVal::Array(_) => Err(Decline),
        }
    }

    /// `EACH` on a value.
    fn each<'p>(&self, v: TVal<'p>, emit: &mut Emit<'_, 'p>) -> Result<(), Decline> {
        let doc = self.doc;
        match v {
            TVal::Node(n) => match doc.kind(n) {
                Kind::Array => doc.for_each_element(n, |e| emit(TVal::Node(e))),
                Kind::Object => {
                    let dedup = doc.dedup_entries(n, &mut self.scratch.borrow_mut());
                    match dedup {
                        Some(entries) => {
                            for &(_, v) in &entries {
                                emit(TVal::Node(v))?;
                            }
                            Ok(())
                        }
                        None => doc.for_each_entry(n, |_, v| emit(TVal::Node(v))),
                    }
                }
                _ => Err(Decline),
            },
            TVal::Array(items) => {
                for v in items {
                    emit(v)?;
                }
                Ok(())
            }
            TVal::Object(entries) => {
                for (_, v) in entries {
                    emit(v)?;
                }
                Ok(())
            }
            TVal::Null | TVal::Number(_) => Err(Decline),
        }
    }

    /// `f_length`.
    fn length<'p>(&self, v: TVal<'p>) -> Result<TVal<'p>, Decline> {
        let count = |n: usize| TVal::Number(Number::from_f64(n as f64));
        let doc = self.doc;
        Ok(match v {
            TVal::Node(n) => match doc.kind(n) {
                Kind::Array => count(doc.count(n)),
                Kind::Object => match doc.dedup_entries(n, &mut self.scratch.borrow_mut()) {
                    Some(entries) => count(entries.len()),
                    None => count(doc.count(n)),
                },
                Kind::String => count(doc.str(n).chars().count()),
                Kind::Number => TVal::Number(doc.number(n).abs()),
                Kind::Null => TVal::Number(Number::from_f64(0.0)),
                Kind::True | Kind::False => return Err(Decline),
            },
            TVal::Null => TVal::Number(Number::from_f64(0.0)),
            TVal::Number(x) => TVal::Number(x.abs()),
            TVal::Array(items) => count(items.len()),
            TVal::Object(entries) => count(entries.len()),
        })
    }

    /// `f_keys` / `f_keys_unsorted`.
    fn keys<'p>(&self, v: TVal<'p>, sorted: bool) -> Result<TVal<'p>, Decline> {
        let TVal::Node(n) = v else {
            return Err(Decline);
        };
        let doc = self.doc;
        match doc.kind(n) {
            Kind::Object => {
                let dedup = doc.dedup_entries(n, &mut self.scratch.borrow_mut());
                let mut keys: Vec<Node> = match dedup {
                    Some(entries) => entries.into_iter().map(|(k, _)| k).collect(),
                    None => {
                        let mut keys = Vec::with_capacity(doc.count(n));
                        doc.for_each_key(n, |k| {
                            keys.push(k);
                            Ok::<(), Decline>(())
                        })?;
                        keys
                    }
                };
                if sorted {
                    keys.sort_by(|a, b| doc.str(*a).cmp(doc.str(*b)));
                }
                Ok(TVal::Array(keys.into_iter().map(TVal::Node).collect()))
            }
            Kind::Array => {
                let n = doc.count(n);
                Ok(TVal::Array(
                    (0..n)
                        .map(|i| TVal::Number(Number::from_f64(i as f64)))
                        .collect(),
                ))
            }
            _ => Err(Decline),
        }
    }

    fn test(&self, v: &TVal<'_>, test: &Test) -> bool {
        let doc = self.doc;
        match test {
            Test::Truthy => match v {
                TVal::Node(n) => !matches!(doc.kind(*n), Kind::Null | Kind::False),
                TVal::Null => false,
                _ => true,
            },
            Test::Equal(c) | Test::NotEqual(c) => {
                let eq = match v {
                    TVal::Node(n) => {
                        let kind = doc.kind(*n);
                        let s = if kind == Kind::String {
                            doc.str(*n)
                        } else {
                            ""
                        };
                        c.equals(kind, || doc.number(*n), s)
                    }
                    TVal::Null => matches!(c, Const::Null),
                    TVal::Number(x) => c.equals(Kind::Number, || x.clone(), ""),
                    // A container is never equal to a scalar.
                    TVal::Array(_) | TVal::Object(_) => false,
                };
                eq == matches!(test, Test::Equal(_))
            }
        }
    }
}

/// One output, for printing.
pub struct Output<'a, 'p> {
    pub doc: &'a Doc<'a>,
    pub val: &'a TVal<'p>,
}

impl Output<'_, '_> {
    /// The contents, if this is a string (for `-r`).
    pub fn as_str(&self) -> Option<&str> {
        match self.val {
            TVal::Node(n) if self.doc.kind(*n) == Kind::String => Some(self.doc.str(*n)),
            _ => None,
        }
    }

    /// Whether this is `null` or `false` (for main.c's status).
    pub fn is_null_or_false(&self) -> bool {
        match self.val {
            TVal::Node(n) => matches!(self.doc.kind(*n), Kind::Null | Kind::False),
            TVal::Null => true,
            _ => false,
        }
    }

    /// Writes it as `dump_to_vec` writes the value jq would have.
    pub fn dump(&self, layout: &Layout, scratch: &mut Scratch, out: &mut Vec<u8>) {
        dump(self.doc, self.val, 0, layout, scratch, out)
    }
}

fn dump(
    doc: &Doc<'_>,
    v: &TVal<'_>,
    depth: usize,
    layout: &Layout,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) {
    if depth > super::tape::PRINT_DEPTH {
        out.extend_from_slice(b"<skipped: too deep>");
        return;
    }
    match v {
        TVal::Node(n) => {
            doc.print(*n, depth, layout, scratch, out);
        }
        TVal::Null => out.extend_from_slice(b"null"),
        TVal::Number(x) => {
            if x.is_nan() {
                out.extend_from_slice(b"null");
            } else {
                x.write_json(out);
            }
        }
        TVal::Array(items) => {
            if items.is_empty() {
                out.extend_from_slice(b"[]");
                return;
            }
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                layout.before_element(i, depth, out);
                dump(doc, item, depth + 1, layout, scratch, out);
            }
            layout.before_close(depth, out);
            out.push(b']');
        }
        TVal::Object(entries) => {
            if entries.is_empty() {
                out.extend_from_slice(b"{}");
                return;
            }
            out.push(b'{');
            let mut order: Vec<usize> = (0..entries.len()).collect();
            if layout.sort_keys() {
                order.sort_by(|&a, &b| entries[a].0.cmp(entries[b].0));
            }
            for (i, &at) in order.iter().enumerate() {
                let (k, v) = &entries[at];
                layout.before_element(i, depth, out);
                layout.key(k, out);
                dump(doc, v, depth + 1, layout, scratch, out);
            }
            layout.before_close(depth, out);
            out.push(b'}');
        }
    }
}
