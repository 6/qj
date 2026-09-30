//! Running simple programs on simdjson's tape, without building jq values.
//!
//! A program made only of paths (`.a.b`, `."a"`, `.["a"]`, and `.a?`),
//! iteration (`.[]`, `.[]?`), pipes, `length`, `keys`, `keys_unsorted`,
//! array collection (`[...]`, `map(...)`), object construction with constant
//! keys (`{a, b: .c.d}`) and `select` on a path's truthiness or its
//! comparison with a constant (`select(.type == "PushEvent")`,
//! `select(.n > 0)`), and of definitions without parameters of such
//! programs (inlined where they're called, as jq resolves the calls), has
//! outputs that are fully determined by its input, and can be computed on
//! the tape ([`super::tape`]). Those outputs are what the jq VM would produce
//! on the value the builder makes of the input: every step follows the
//! builtin or opcode it stands for (`jv_get` on objects, `EACH`,
//! `INDEX_OPT`, `EACH_OPT`, `f_length`, `f_keys`, `jv_equal`, `jv_cmp`, ...),
//! and duplicate keys follow jq's rule.
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
use std::cmp::Ordering;

use crate::jq::lang::ast::{self, BinOp, DictPairKind, Literal, NodeKind, ProgramBody};
use crate::jq::lang::parser::{NoHooks, parse};
use crate::jq::value::Number;
use crate::jq::value::print::DumpSink;

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
    /// `T | .[key]?`: jq's `INDEX_OPT`, which backtracks where `INDEX`
    /// raises an error (`T`'s own errors aren't caught).
    IndexOpt(Box<Expr>, String),
    /// `T | .[]`
    Each(Box<Expr>),
    /// `T | .[]?`: `EACH_OPT`, which backtracks on what `EACH` can't
    /// iterate.
    EachOpt(Box<Expr>),
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
    /// `E < c` and the other orderings: whether `accept` takes
    /// `jv_cmp(E, c)` (reversed with `flip`, for `c < E`).
    Order {
        accept: fn(Ordering) -> bool,
        flip: bool,
        c: Const,
    },
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
        let mut budget = INLINE_BUDGET;
        let expr = convert(node, &Scope::TOP, &mut budget)?;
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

/// The definitions (without parameters) an expression of the program sees:
/// the innermost, then the ones around it. jq resolves a call by name and
/// arity to the innermost definition in scope, and a definition's body sees
/// the definitions before it and itself.
struct Scope<'a, 'p> {
    /// `def name: body;` (`None` at the top: builtins only).
    def: Option<(&'a str, &'a ast::Node)>,
    parent: Option<&'p Scope<'a, 'p>>,
}

impl<'a> Scope<'a, '_> {
    const TOP: Scope<'static, 'static> = Scope {
        def: None,
        parent: None,
    };

    /// The body of the innermost definition of `name` (without
    /// parameters), and the scope it sees.
    fn find(&self, name: &str) -> Option<(&'a ast::Node, &Self)> {
        let mut s = self;
        loop {
            if let Some((n, body)) = s.def
                && n == name
            {
                return Some((body, s));
            }
            s = s.parent?;
        }
    }
}

/// How many calls of definitions a program may inline (each call anew, so
/// a recursive definition doesn't qualify, nor definitions that call each
/// other too often).
const INLINE_BUDGET: usize = 64;

/// The program's AST as an [`Expr`], if it qualifies.
fn convert<'a>(n: &'a ast::Node, scope: &Scope<'a, '_>, budget: &mut usize) -> Option<Expr> {
    Some(match &n.kind {
        NodeKind::Identity => Expr::Identity,
        NodeKind::Index {
            target,
            key,
            optional,
        } => {
            let key = const_string(key)?;
            let target = Box::new(match target {
                None => Expr::Identity,
                Some(t) => convert(t, scope, budget)?,
            });
            if *optional {
                Expr::IndexOpt(target, key)
            } else {
                Expr::Index(target, key)
            }
        }
        NodeKind::Each { target, optional } => {
            let target = Box::new(convert(target, scope, budget)?);
            if *optional {
                Expr::EachOpt(target)
            } else {
                Expr::Each(target)
            }
        }
        NodeKind::Pipe(a, b) => Expr::Pipe(
            Box::new(convert(a, scope, budget)?),
            Box::new(convert(b, scope, budget)?),
        ),
        // A definition without parameters is inlined where it's called
        // (below); one with parameters makes the program not qualify.
        NodeKind::FuncDef { def, rest } => {
            if !def.params.is_empty() {
                return None;
            }
            let inner = Scope {
                def: Some((&def.name, &def.body)),
                parent: Some(scope),
            };
            return convert(rest, &inner, budget);
        }
        NodeKind::Call { name, args, .. } if args.is_empty() && scope.find(name).is_some() => {
            let (body, s) = scope.find(name)?;
            *budget = budget.checked_sub(1)?;
            return convert(body, s, budget);
        }
        NodeKind::Call { name, args, .. } => match (name.as_str(), args.as_slice()) {
            ("length", []) => Expr::Length,
            ("keys", []) => Expr::Keys { sorted: true },
            ("keys_unsorted", []) => Expr::Keys { sorted: false },
            // builtin.jq: `def map(f): [.[] | f];`
            ("map", [f]) => Expr::Collect(Box::new(Expr::Pipe(
                Box::new(Expr::Each(Box::new(Expr::Identity))),
                Box::new(convert(f, scope, budget)?),
            ))),
            // builtin.jq: `def select(f): if f then . else empty end;`
            ("select", [f]) => select(f, scope, budget)?,
            _ => return None,
        },
        NodeKind::Array(Some(e)) => Expr::Collect(Box::new(convert(e, scope, budget)?)),
        NodeKind::Object(pairs) => {
            let mut entries: Vec<(String, Expr)> = Vec::with_capacity(pairs.len());
            for p in pairs {
                let (key, value) = match &p.kind {
                    DictPairKind::Named { key, value } => {
                        (key.clone(), convert(value, scope, budget)?)
                    }
                    DictPairKind::Str { key, value } => {
                        (const_string_lit(key)?, convert(value, scope, budget)?)
                    }
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
fn select<'a>(f: &'a ast::Node, scope: &Scope<'a, '_>, budget: &mut usize) -> Option<Expr> {
    // `select(f)` with `def f: ...;` is `select` of f's body.
    if let NodeKind::Call { name, args, .. } = &f.kind
        && args.is_empty()
        && let Some((body, s)) = scope.find(name)
    {
        *budget = budget.checked_sub(1)?;
        return select(body, s, budget);
    }
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
        let e = convert(e, scope, budget)?;
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
    if let NodeKind::Binary { op, lhs, rhs } = &f.kind
        && let Some(accept) = order_op(*op)
    {
        // `c < E` is `E > c`.
        let (e, c, flip) = match (constant(lhs), constant(rhs)) {
            (None, Some(c)) => (lhs, c, false),
            (Some(c), None) => (rhs, c, true),
            _ => return None,
        };
        let e = convert(e, scope, budget)?;
        if !single(&e) {
            return None;
        }
        return Some(Expr::Select(Box::new(e), Test::Order { accept, flip, c }));
    }
    let e = convert(f, scope, budget)?;
    if !single(&e) {
        return None;
    }
    Some(Expr::Select(Box::new(e), Test::Truthy))
}

/// For `<`, `<=`, `>`, `>=`: which results of `jv_cmp(lhs, rhs)` make it
/// true (builtin.c's `order_cmp`).
fn order_op(op: BinOp) -> Option<fn(Ordering) -> bool> {
    Some(match op {
        BinOp::Lt => Ordering::is_lt,
        BinOp::Le => Ordering::is_le,
        BinOp::Gt => Ordering::is_gt,
        BinOp::Ge => Ordering::is_ge,
        _ => return None,
    })
}

/// Whether `e` always has exactly one output (unless it's an error).
fn single(e: &Expr) -> bool {
    match e {
        Expr::Identity | Expr::Length | Expr::Keys { .. } | Expr::Collect(_) => true,
        Expr::Index(t, _) => single(t),
        Expr::Pipe(a, b) => single(a) && single(b),
        Expr::Object(entries) => entries.iter().all(|(_, e)| single(e)),
        Expr::IndexOpt(..) | Expr::Each(_) | Expr::EachOpt(_) | Expr::Select(..) => false,
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

/// jq's order of kinds (`jv_kind`'s): null < false < true < numbers <
/// strings < arrays < objects.
fn kind_rank(kind: Kind) -> u8 {
    match kind {
        Kind::Null => 0,
        Kind::False => 1,
        Kind::True => 2,
        Kind::Number => 3,
        Kind::String => 4,
        Kind::Array => 5,
        Kind::Object => 6,
    }
}

impl Const {
    fn kind(&self) -> Kind {
        match self {
            Const::Null => Kind::Null,
            Const::False => Kind::False,
            Const::True => Kind::True,
            Const::Number(..) => Kind::Number,
            Const::String(_) => Kind::String,
        }
    }

    /// The literal jq's compiler makes of a number constant.
    fn number(&self) -> Option<Number> {
        let Const::Number(t, neg) = self else {
            return None;
        };
        let c = Number::from_literal(t.as_bytes()).expect("checked");
        Some(if *neg { c.negate() } else { c })
    }

    /// `jv_cmp(v, c)` for a value of `kind` (`number` gives it when it's a
    /// number, `string` when it's a string): by kind, then numbers by
    /// value (never NaN here: JSON has none, nor do constants or lengths)
    /// and strings by their bytes.
    fn order(&self, kind: Kind, number: impl FnOnce() -> Number, string: &str) -> Ordering {
        kind_rank(kind)
            .cmp(&kind_rank(self.kind()))
            .then_with(|| match self {
                Const::Number(..) => number().compare(&self.number().expect("a number")),
                Const::String(s) => string.as_bytes().cmp(s.as_bytes()),
                _ => Ordering::Equal,
            })
    }

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
            // (What the index or the iteration raises is caught; what `T`
            // or the rest of the program raises isn't.)
            Expr::IndexOpt(t, key) => self.run(t, input, &mut |v| match self.index(v, key) {
                Ok(x) => emit(x),
                Err(Decline) => Ok(()),
            }),
            Expr::Each(t) => {
                if single(t) {
                    let v = self.single(t, input)?;
                    self.each(v, emit)
                } else {
                    self.run(t, input, &mut |v| self.each(v, emit))
                }
            }
            Expr::EachOpt(t) => self.run(t, input, &mut |v| {
                if self.iterable(&v) {
                    self.each(v, emit)
                } else {
                    Ok(())
                }
            }),
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

    /// Whether `EACH` iterates `v` (an array or an object) rather than
    /// raising an error.
    fn iterable(&self, v: &TVal<'_>) -> bool {
        match v {
            TVal::Node(n) => matches!(self.doc.kind(*n), Kind::Array | Kind::Object),
            TVal::Array(_) | TVal::Object(_) => true,
            TVal::Null | TVal::Number(_) => false,
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
            Test::Order { accept, flip, c } => {
                let ord = match v {
                    TVal::Node(n) => {
                        let kind = doc.kind(*n);
                        let s = if kind == Kind::String {
                            doc.str(*n)
                        } else {
                            ""
                        };
                        c.order(kind, || doc.number(*n), s)
                    }
                    TVal::Null => c.order(Kind::Null, || unreachable!(), ""),
                    TVal::Number(x) => c.order(Kind::Number, || x.clone(), ""),
                    TVal::Array(_) => c.order(Kind::Array, || unreachable!(), ""),
                    TVal::Object(_) => c.order(Kind::Object, || unreachable!(), ""),
                };
                accept(if *flip { ord.reverse() } else { ord })
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
    pub fn dump<S: DumpSink>(&self, layout: &Layout, scratch: &mut Scratch, sink: &mut S) {
        dump(self.doc, self.val, 0, layout, scratch, sink)
    }
}

fn dump<S: DumpSink>(
    doc: &Doc<'_>,
    v: &TVal<'_>,
    depth: usize,
    layout: &Layout,
    scratch: &mut Scratch,
    sink: &mut S,
) {
    if depth > super::tape::PRINT_DEPTH {
        sink.buf().extend_from_slice(b"<skipped: too deep>");
        return;
    }
    match v {
        TVal::Node(n) => {
            doc.print(*n, depth, layout, scratch, sink);
        }
        TVal::Null => sink.buf().extend_from_slice(b"null"),
        TVal::Number(x) => {
            if x.is_nan() {
                sink.buf().extend_from_slice(b"null");
            } else {
                x.write_json(sink.buf());
            }
        }
        TVal::Array(items) => {
            if items.is_empty() {
                sink.buf().extend_from_slice(b"[]");
                return;
            }
            sink.buf().push(b'[');
            for (i, item) in items.iter().enumerate() {
                layout.before_element(i, depth, sink.buf());
                dump(doc, item, depth + 1, layout, scratch, sink);
                sink.checkpoint();
            }
            let out = sink.buf();
            layout.before_close(depth, out);
            out.push(b']');
        }
        TVal::Object(entries) => {
            if entries.is_empty() {
                sink.buf().extend_from_slice(b"{}");
                return;
            }
            sink.buf().push(b'{');
            let mut order: Vec<usize> = (0..entries.len()).collect();
            if layout.sort_keys() {
                order.sort_by(|&a, &b| entries[a].0.cmp(entries[b].0));
            }
            for (i, &at) in order.iter().enumerate() {
                let (k, v) = &entries[at];
                let out = sink.buf();
                layout.before_element(i, depth, out);
                layout.key(k, out);
                dump(doc, v, depth + 1, layout, scratch, sink);
                sink.checkpoint();
            }
            let out = sink.buf();
            layout.before_close(depth, out);
            out.push(b'}');
        }
    }
}
