//! Running simple programs on simdjson's tape, without building jq values.
//!
//! A program made only of paths (`.a.b`, `."a"`, `.["a"]`, and `.a?`),
//! iteration (`.[]`, `.[]?`), pipes, `length`, `keys`, `keys_unsorted`,
//! `type`, `has("k")` (a constant string key), `add` (of numbers and nulls:
//! other sums decline), array collection (`[...]`, `map(...)`), object
//! construction with constant keys (`{a, b: .c.d}`), `select` on a path's
//! truthiness or its comparison with a constant (`select(.type ==
//! "PushEvent")`, `select(.n > 0)`, `select(.a | type == "object")`),
//! combined with `and`, `or` and `not`, such conditions as values
//! (`map(.a == 1)`, `not`), builtin.jq's type filters (`numbers`, `strings`,
//! `values`, `scalars`, ...), `isnan`, `isinfinite`, `isnormal` and
//! `isfinite`, and of definitions without parameters of such programs
//! (inlined where they're called, as jq resolves the calls), has outputs
//! that are fully determined by its input, and can be computed on the tape
//! ([`super::tape`]). Those outputs are what the jq VM would produce
//! on the value the builder makes of the input: every step follows the
//! builtin or opcode it stands for (`jv_get` on objects, `EACH`,
//! `INDEX_OPT`, `EACH_OPT`, `f_length`, `f_keys`, `f_type`, `jv_has`,
//! `jv_equal`, `jv_cmp`, ...), and duplicate keys follow jq's rule.
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
use crate::jq::value::print::{DumpSink, write_json_string};

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
    /// `select(C)`.
    Select(Cond),
    /// builtin.jq's `def add(f): reduce f as $x (null; . + $x);` (and
    /// `add` for `add(.[])`), for sums of numbers (and nulls).
    Add(Box<Expr>),
    /// `type`: `f_type`, the name of the value's kind.
    Type,
    /// `has(k)` for a constant string `k`: `jv_has`, which answers for
    /// objects (and `null`, which has no keys) and is an error for anything
    /// else.
    Has(String),
    /// A condition's truth as a value: `not` (jq's bytecoded `if . then
    /// false else true end`), and comparisons with a constant, `and` and
    /// `or` outside `select` (`gen_and`, `gen_or`: `true` or `false`).
    Bool(Cond),
    /// `isnan`, `isinfinite`, `isnormal` (`f_isnan` and the others: the
    /// test on a number's `jv_number_value`, `false` for anything else)
    /// and builtin.jq's `def isfinite: type == "number" and (isinfinite |
    /// not);`.
    NumberIs(fn(f64) -> bool),
}

/// A condition of `select`.
#[derive(Debug)]
enum Cond {
    /// A single-valued `E`'s truthiness, or its comparison with a constant.
    Test(Box<Expr>, Test),
    /// `A and B` (`B` only when `A` holds).
    And(Box<Cond>, Box<Cond>),
    /// `A or B` (`B` only when `A` doesn't hold).
    Or(Box<Cond>, Box<Cond>),
    /// `A | not`.
    Not(Box<Cond>),
    /// `x | C` for a single-valued `x`: `C` on its output.
    Pipe(Box<Expr>, Box<Cond>),
    /// Whether the value's kind is in a set ([`kind_bit`]s): the conditions
    /// of builtin.jq's type filters ([`type_filter`]).
    Kinds(u8),
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
    /// A computed string (`type`'s kind names).
    Str(&'p str),
    /// A computed boolean (`has`, `not`, a comparison).
    Bool(bool),
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
            // builtin.jq: `def add: add(.[]);`
            ("add", []) => Expr::Add(Box::new(Expr::Each(Box::new(Expr::Identity)))),
            ("add", [f]) => Expr::Add(Box::new(convert(f, scope, budget)?)),
            ("type", []) => Expr::Type,
            ("has", [k]) => Expr::Has(const_string(k)?),
            // Bytecoded: `if . then false else true end`.
            ("not", []) => Expr::Bool(Cond::Not(Box::new(Cond::Test(
                Box::new(Expr::Identity),
                Test::Truthy,
            )))),
            ("isnan", []) => Expr::NumberIs(f64::is_nan),
            ("isinfinite", []) => Expr::NumberIs(f64::is_infinite),
            ("isnormal", []) => Expr::NumberIs(f64::is_normal),
            ("isfinite", []) => Expr::NumberIs(is_finite),
            (name, []) if type_filter(name).is_some() => {
                Expr::Select(Cond::Kinds(type_filter(name)?))
            }
            // builtin.jq: `def normals: select(isnormal);`, `def finites:
            // select(isfinite);`.
            ("normals", []) => Expr::Select(Cond::Test(
                Box::new(Expr::NumberIs(f64::is_normal)),
                Test::Truthy,
            )),
            ("finites", []) => Expr::Select(Cond::Test(
                Box::new(Expr::NumberIs(is_finite)),
                Test::Truthy,
            )),
            _ => return None,
        },
        // A comparison with a constant, `and`, `or`: the condition as a
        // boolean.
        NodeKind::Binary {
            op:
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or,
            ..
        } => Expr::Bool(cond(n, scope, budget)?),
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

/// `select(f)` for the conditions handled here.
fn select<'a>(f: &'a ast::Node, scope: &Scope<'a, '_>, budget: &mut usize) -> Option<Expr> {
    Some(Expr::Select(cond(f, scope, budget)?))
}

/// `f` as a condition: its one output's truthiness.
fn cond<'a>(f: &'a ast::Node, scope: &Scope<'a, '_>, budget: &mut usize) -> Option<Cond> {
    // A call of `def f: ...;` is f's body.
    if let NodeKind::Call { name, args, .. } = &f.kind
        && args.is_empty()
        && let Some((body, s)) = scope.find(name)
    {
        *budget = budget.checked_sub(1)?;
        return cond(body, s, budget);
    }
    // builtin.jq: `def not: if . then false else true end;`
    let is_not = |n: &ast::Node| {
        matches!(&n.kind, NodeKind::Call { name, args, .. }
            if name == "not" && args.is_empty() && scope.find(name).is_none())
    };
    if is_not(f) {
        let this = Cond::Test(Box::new(Expr::Identity), Test::Truthy);
        return Some(Cond::Not(Box::new(this)));
    }
    if let NodeKind::Pipe(x, y) = &f.kind {
        // `x | not | not ...` (pipes group to the right).
        let mut nots = 0;
        let mut rest = &**y;
        loop {
            match &rest.kind {
                _ if is_not(rest) => {
                    nots += 1;
                    break;
                }
                NodeKind::Pipe(n, more) if is_not(n) => {
                    nots += 1;
                    rest = more;
                }
                _ => {
                    nots = 0;
                    break;
                }
            }
        }
        if nots > 0 {
            let mut c = cond(x, scope, budget)?;
            for _ in 0..nots {
                c = Cond::Not(Box::new(c));
            }
            return Some(c);
        }
        // `x | C` (`select(.a | type == "object")`): C on x's one output.
        let x = single_expr(x, scope, budget)?;
        let c = cond(y, scope, budget)?;
        return Some(Cond::Pipe(Box::new(x), Box::new(c)));
    }
    if let NodeKind::Binary { op, lhs, rhs } = &f.kind {
        match op {
            // `a and b` is `if a then (if b then true else false) else
            // false` (b only when a is true); `or` likewise.
            BinOp::And => {
                let a = cond(lhs, scope, budget)?;
                let b = cond(rhs, scope, budget)?;
                return Some(Cond::And(Box::new(a), Box::new(b)));
            }
            BinOp::Or => {
                let a = cond(lhs, scope, budget)?;
                let b = cond(rhs, scope, budget)?;
                return Some(Cond::Or(Box::new(a), Box::new(b)));
            }
            BinOp::Eq | BinOp::Ne => {
                // The constant may be on either side (`==` is symmetric).
                let (e, c) = match (constant(lhs), constant(rhs)) {
                    (None, Some(c)) => (lhs, c),
                    (Some(c), None) => (rhs, c),
                    _ => return None,
                };
                let e = single_expr(e, scope, budget)?;
                let test = if *op == BinOp::Eq {
                    Test::Equal(c)
                } else {
                    Test::NotEqual(c)
                };
                return Some(Cond::Test(Box::new(e), test));
            }
            _ => {
                let accept = order_op(*op)?;
                // `c < E` is `E > c`.
                let (e, c, flip) = match (constant(lhs), constant(rhs)) {
                    (None, Some(c)) => (lhs, c, false),
                    (Some(c), None) => (rhs, c, true),
                    _ => return None,
                };
                let e = single_expr(e, scope, budget)?;
                let test = Test::Order { accept, flip, c };
                return Some(Cond::Test(Box::new(e), test));
            }
        }
    }
    let e = single_expr(f, scope, budget)?;
    Some(Cond::Test(Box::new(e), Test::Truthy))
}

/// `n` converted, if it always has exactly one output (unless it's an
/// error).
fn single_expr<'a>(n: &'a ast::Node, scope: &Scope<'a, '_>, budget: &mut usize) -> Option<Expr> {
    let e = convert(n, scope, budget)?;
    single(&e).then_some(e)
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
        Expr::Identity
        | Expr::Length
        | Expr::Keys { .. }
        | Expr::Collect(_)
        | Expr::Add(_)
        | Expr::Type
        | Expr::Has(_)
        | Expr::Bool(_)
        | Expr::NumberIs(_) => true,
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
const fn kind_rank(kind: Kind) -> u8 {
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

/// A kind's bit in a set of kinds ([`Cond::Kinds`]).
const fn kind_bit(kind: Kind) -> u8 {
    1 << kind_rank(kind)
}

/// Every kind.
const ALL_KINDS: u8 = (1 << 7) - 1;

/// builtin.jq's type filters that test only the kind, as the set of kinds
/// they keep: `def numbers: select(type == "number");`, `def iterables:
/// select(type|. == "array" or . == "object");`, `def scalars:
/// select(type|. != "array" and . != "object");`, `def nulls: select(. ==
/// null);` (only `null` equals `null`), `def values: select(. != null);`,
/// and the others.
fn type_filter(name: &str) -> Option<u8> {
    let array = kind_bit(Kind::Array);
    let object = kind_bit(Kind::Object);
    let null = kind_bit(Kind::Null);
    Some(match name {
        "arrays" => array,
        "objects" => object,
        "iterables" => array | object,
        "booleans" => kind_bit(Kind::False) | kind_bit(Kind::True),
        "numbers" => kind_bit(Kind::Number),
        "strings" => kind_bit(Kind::String),
        "nulls" => null,
        "values" => ALL_KINDS & !null,
        "scalars" => ALL_KINDS & !(array | object),
        _ => return None,
    })
}

/// builtin.jq's `isfinite` on a number's value: `isinfinite | not`, which
/// holds for NaN.
fn is_finite(x: f64) -> bool {
    !x.is_infinite()
}

/// `jv_kind_name`, which `f_type` returns.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Null => "null",
        Kind::False | Kind::True => "boolean",
        Kind::Number => "number",
        Kind::String => "string",
        Kind::Array => "array",
        Kind::Object => "object",
    }
}

/// The kind of a boolean.
fn bool_kind(b: bool) -> Kind {
    if b { Kind::True } else { Kind::False }
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
            Expr::Select(c) => {
                if self.cond(c, &input)? {
                    emit(input)
                } else {
                    Ok(())
                }
            }
            Expr::Add(f) => {
                let mut acc = TVal::Null;
                self.run(f, input, &mut |v| {
                    let a = std::mem::replace(&mut acc, TVal::Null);
                    acc = self.plus(a, v)?;
                    Ok(())
                })?;
                emit(acc)
            }
            Expr::Type => emit(TVal::Str(kind_name(self.kind(&input)))),
            Expr::Has(key) => {
                let has = self.has(input, key)?;
                emit(TVal::Bool(has))
            }
            Expr::Bool(c) => {
                let holds = self.cond(c, &input)?;
                emit(TVal::Bool(holds))
            }
            Expr::NumberIs(test) => emit(TVal::Bool(self.number_is(&input, *test))),
        }
    }

    /// The kind of a value.
    fn kind(&self, v: &TVal<'_>) -> Kind {
        match v {
            TVal::Node(n) => self.doc.kind(*n),
            TVal::Null => Kind::Null,
            TVal::Number(_) => Kind::Number,
            TVal::Str(_) => Kind::String,
            TVal::Bool(b) => bool_kind(*b),
            TVal::Array(_) => Kind::Array,
            TVal::Object(_) => Kind::Object,
        }
    }

    /// `jv_has(v, key)` for a string key: whether an object has the key
    /// (`null` has none); anything else is jq's error.
    fn has(&self, v: TVal<'_>, key: &str) -> Result<bool, Decline> {
        match v {
            TVal::Node(n) => match self.doc.kind(n) {
                Kind::Object => Ok(self.doc.has_key(n, key)),
                Kind::Null => Ok(false),
                _ => Err(Decline),
            },
            TVal::Null => Ok(false),
            TVal::Object(entries) => Ok(entries.iter().any(|(k, _)| *k == key)),
            TVal::Number(_) | TVal::Str(_) | TVal::Bool(_) | TVal::Array(_) => Err(Decline),
        }
    }

    /// `f_isnan` and the other number predicates: `test` on a number's
    /// `jv_number_value` (for the document's numbers, the double its
    /// literal converts to), `false` for anything else.
    fn number_is(&self, v: &TVal<'_>, test: fn(f64) -> bool) -> bool {
        match v {
            TVal::Node(n) if self.doc.kind(*n) == Kind::Number => test(self.doc.number(*n).value()),
            TVal::Number(x) => test(x.value()),
            _ => false,
        }
    }

    /// builtin.c's `binop_plus` for nulls and numbers (`null` is the other
    /// side; numbers add as doubles). Anything else declines: strings,
    /// arrays and objects concatenate, which isn't done here, and other
    /// kinds are jq's error.
    fn plus<'p>(&self, a: TVal<'p>, b: TVal<'p>) -> Result<TVal<'p>, Decline> {
        let doc = self.doc;
        let is_null = |v: &TVal<'_>| match v {
            TVal::Null => true,
            TVal::Node(n) => doc.kind(*n) == Kind::Null,
            _ => false,
        };
        if is_null(&a) {
            return Ok(b);
        }
        if is_null(&b) {
            return Ok(a);
        }
        let number = |v: &TVal<'_>| match v {
            TVal::Node(n) if doc.kind(*n) == Kind::Number => Some(doc.number(*n)),
            TVal::Number(x) => Some(x.clone()),
            _ => None,
        };
        match (number(&a), number(&b)) {
            (Some(x), Some(y)) => Ok(TVal::Number(Number::from_f64(x.value() + y.value()))),
            _ => Err(Decline),
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
            TVal::Number(_) | TVal::Str(_) | TVal::Bool(_) | TVal::Array(_) => Err(Decline),
        }
    }

    /// Whether `EACH` iterates `v` (an array or an object) rather than
    /// raising an error.
    fn iterable(&self, v: &TVal<'_>) -> bool {
        matches!(self.kind(v), Kind::Array | Kind::Object)
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
            TVal::Null | TVal::Number(_) | TVal::Str(_) | TVal::Bool(_) => Err(Decline),
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
            TVal::Str(s) => count(s.chars().count()),
            TVal::Bool(_) => return Err(Decline),
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

    /// Whether a condition holds for `input` (evaluating as jq's `and`,
    /// `or` and `not` do: the right side only when the left doesn't decide).
    fn cond<'p>(&self, c: &'p Cond, input: &TVal<'p>) -> Result<bool, Decline> {
        Ok(match c {
            Cond::Test(e, test) => {
                let v = self.single(e, input.clone())?;
                self.test(&v, test)
            }
            Cond::And(a, b) => self.cond(a, input)? && self.cond(b, input)?,
            Cond::Or(a, b) => self.cond(a, input)? || self.cond(b, input)?,
            Cond::Not(a) => !self.cond(a, input)?,
            Cond::Pipe(x, c) => {
                let v = self.single(x, input.clone())?;
                self.cond(c, &v)?
            }
            Cond::Kinds(set) => set & kind_bit(self.kind(input)) != 0,
        })
    }

    fn test(&self, v: &TVal<'_>, test: &Test) -> bool {
        let doc = self.doc;
        match test {
            Test::Truthy => !matches!(self.kind(v), Kind::Null | Kind::False),
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
                    TVal::Str(s) => c.equals(Kind::String, || unreachable!(), s),
                    TVal::Bool(b) => c.equals(bool_kind(*b), || unreachable!(), ""),
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
                    TVal::Str(s) => c.order(Kind::String, || unreachable!(), s),
                    TVal::Bool(b) => c.order(bool_kind(*b), || unreachable!(), ""),
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
            TVal::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Whether this is `null` or `false` (for main.c's status).
    pub fn is_null_or_false(&self) -> bool {
        match self.val {
            TVal::Node(n) => matches!(self.doc.kind(*n), Kind::Null | Kind::False),
            TVal::Null | TVal::Bool(false) => true,
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
        TVal::Str(s) => write_json_string(s, layout.ascii(), sink.buf()),
        TVal::Bool(false) => sink.buf().extend_from_slice(b"false"),
        TVal::Bool(true) => sink.buf().extend_from_slice(b"true"),
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
