//! AST for jq 1.8.1 programs, produced by [`super::parser`] (port of `parser.y`).
//!
//! # Shape
//!
//! The AST mirrors `parser.y`'s productions: every variant names the bison rule(s) it
//! comes from (rule numbers as in jq's generated `parser.c`, listed in
//! `parser_tables::RULE_NAMES`), and carries exactly what that rule's semantic action
//! needs, so Track C can port the actions (`gen_*` calls, constant folding,
//! `gen_update`, `gen_dictpair`, `$__loc__`, ...) node by node:
//!
//! * `Query`, `Expr`, `Term`, `Arg` and `DictExpr` values are all [`Node`]s. Rules
//!   that just pass a value through (`Query: Expr`, `Expr: Term`, `Arg: Query`,
//!   `DictExpr: Expr`, and `Term: '(' Query ')'`) create no node, exactly like jq's
//!   `$$ = $1`, so parentheses leave no trace except in the tree shape.
//! * [`Node::loc`] is the bison location `@$` of the rule that created the node.
//!   Passthrough rules do not widen it. Where an action uses some other location
//!   (`@1`, `@2`), the node stores it separately (`Call::name_loc`, `key_loc`, ...).
//! * Number literals keep their source text ([`Literal::Number`]); jq preserves
//!   literals (`1.000`, `1E2`, `100000000000000000001`).
//! * Strings keep their token-level parts ([`StringLit::parts`]): each
//!   `QQSTRING_TEXT` token (already unescaped) and each `\(...)` interpolation, plus the
//!   `@format` that applies to the interpolations. jq folds the parts with
//!   `gen_binop(..., '+')` starting from `""`.
//! * Error-recovery rules produce [`NodeKind::Error`] (jq: `gen_noop()`), or the
//!   same value jq's action returns (e.g. `"if" Query "then" error` yields the
//!   condition). A program with errors is never returned by [`super::parse`], but
//!   these values reach [`super::ParseHooks`] during parsing, as in jq.
//!
//! # Locations
//!
//! [`Loc`] is jq's `location`: byte offsets `[start, end)` into the program text.
//! Line/column and the caret excerpts of error messages come from
//! [`super::locfile::LocFile`] (port of `locfile.c`); `$__loc__`'s line number is
//! `LocFile::get_line(loc.start) + 1`.

use std::fmt::Write as _;

/// jq's `location`: a byte range `[start, end)` in the program text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Loc {
    pub start: u32,
    pub end: u32,
}

impl Loc {
    /// jq's `UNKNOWN_LOCATION` (`{-1, -1}`).
    pub const UNKNOWN: Loc = Loc {
        start: u32::MAX,
        end: u32::MAX,
    };

    pub fn new(start: u32, end: u32) -> Loc {
        Loc { start, end }
    }

    pub fn is_unknown(self) -> bool {
        self.start == u32::MAX
    }
}

/// `TopLevel: Module Imports Query | Module Imports FuncDefs` (rules 2 and 3).
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// `"module" Query ';'` (rule 5). `None` for rule 4 (no directive).
    pub module: Option<Module>,
    /// `Imports` (rules 6-7), in source order.
    pub imports: Vec<Import>,
    pub body: ProgramBody,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProgramBody {
    /// Rule 2: a main query. `block_has_main` is true.
    Main(Node),
    /// Rule 3: only function definitions (possibly none: an empty program). jq reports
    /// "Top-level program not given (try \".\")" for these as main programs, and
    /// accepts them as libraries.
    Library(Vec<FuncDef>),
}

/// `"module" Query ';'` (rule 5). jq requires `meta` to be a constant object; the check
/// (and its error message) belongs to [`super::ParseHooks::check_metadata`].
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub meta: Node,
    /// `@2`: location of the metadata query, including any parentheses.
    pub meta_loc: Loc,
    pub loc: Loc,
}

/// `Import: ImportWhat ';' | ImportWhat Query ';'` (rules 39-43).
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    /// `ImportFrom` (rule 44): the constant path string. If the path string had an
    /// interpolation, the parser already reported "Import path must be constant" and
    /// this is `""`, as in jq.
    pub path: String,
    /// `@` of the path string.
    pub path_loc: Loc,
    pub kind: ImportKind,
    /// Rule 40's metadata query (must be a constant object; see
    /// [`super::ParseHooks::check_metadata`]).
    pub meta: Option<Node>,
    /// `@2` of rule 40 (the metadata query, including parentheses).
    pub meta_loc: Option<Loc>,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportKind {
    /// Rule 42: `import "path" as name;` (`gen_import(path, name, 0)`).
    Code(String),
    /// Rule 41: `import "path" as $name;` (`gen_import(path, name, 1)`).
    Data(String),
    /// Rule 43: `include "path";` (`gen_import(path, NULL, 0)`).
    Include,
}

/// `FuncDef: "def" IDENT ':' Query ';' | "def" IDENT '(' Params ')' ':' Query ';'`
/// (rules 45-46).
#[derive(Clone, Debug, PartialEq)]
pub struct FuncDef {
    pub name: String,
    /// Empty for rule 45. Rule 46 always has at least one parameter.
    pub params: Vec<Param>,
    pub body: Node,
    pub loc: Loc,
}

/// `Param: BINDING | IDENT` (rules 49-50).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// Without the `$` for value parameters.
    pub name: String,
    pub kind: ParamKind,
    pub loc: Loc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    /// Rule 50: `IDENT`, a closure parameter (`gen_param`).
    Filter,
    /// Rule 49: `$name`, a value parameter (`gen_param_regular`).
    Value,
}

/// A `Query`, `Expr` or `Term`.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    /// `@$` of the rule that created this node.
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// Rule 10: `FuncDef Query` (`block_bind_referenced(def, rest, OP_IS_CALL_PSEUDO)`).
    FuncDef { def: Box<FuncDef>, rest: Box<Node> },
    /// Rule 11: `Expr "as" Patterns '|' Query` (`gen_destructure`).
    As {
        source: Box<Node>,
        patterns: Vec<Pattern>,
        body: Box<Node>,
    },
    /// Rule 12: `"label" BINDING '|' Query` (`gen_location(@$, gen_label("*label-" + name, body))`).
    Label { name: String, body: Box<Node> },
    /// Rule 13 `Query '|' Query` and rule 167 `DictExpr '|' DictExpr` (`block_join`).
    Pipe(Box<Node>, Box<Node>),
    /// Rule 14: `Query ',' Query` (`gen_both`).
    Comma(Box<Node>, Box<Node>),
    /// Rules 16-37: `Expr <op> Expr`.
    Binary {
        op: BinOp,
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
    /// Rule 60: `.` (`gen_noop()`).
    Identity,
    /// Rule 61: `..` (`gen_call("recurse", gen_noop())`).
    Recurse,
    /// Rule 62: `"break" BINDING`: loads the label variable `*label-<name>`, located at `@$`.
    Break(String),
    /// Index by constant field name, string, or query (`gen_index` / `gen_index_opt`):
    /// rules 64-71 (`Term FIELD`, `FIELD`, `Term '.' String`, `'.' String`, each with an
    /// optional `'?'`) and 74-77 (`Term '[' Query ']'`, `Term '.' '[' Query ']'`, `'?'`).
    /// `target: None` is the implicit `.` of `FIELD` / `'.' String` (`gen_noop()`).
    /// For `FIELD` the key is a [`Literal::String`] node located at the `FIELD` token.
    Index {
        target: Option<Box<Node>>,
        key: Box<Node>,
        optional: bool,
    },
    /// Rules 78-81: `Term '[' ']'` and `Term '.' '[' ']'` (`EACH` / `EACH_OPT`).
    Each { target: Box<Node>, optional: bool },
    /// Rules 82-87: `Term '[' Query? ':' Query? ']'` with an optional `'?'`
    /// (`gen_slice_index`); a missing bound is `gen_const(jv_null())`. At least one bound
    /// is always present.
    Slice {
        target: Box<Node>,
        from: Option<Box<Node>>,
        to: Option<Box<Node>>,
        optional: bool,
    },
    /// Rule 88: postfix `Term '?'` (`gen_try(term, gen_op_simple(BACKTRACK))`).
    Optional(Box<Node>),
    /// Rule 89 (`LITERAL`), rule 108 (`null`/`true`/`false`), and field names.
    Literal(Literal),
    /// Rule 90: `String`.
    Str(Box<StringLit>),
    /// Rule 91: a bare `@name` (`gen_format(gen_noop(), name)`).
    Format(String),
    /// Rule 92: `'-' Term` (`BLOCK(term, gen_call("_negate", gen_noop()))`). jq does not
    /// constant-fold this: `{(-1): 2}` is a runtime error, not a compile error.
    Neg(Box<Node>),
    /// Rule 94 `'[' Query ']'` (`gen_collect`) and rule 95 `'[' ']'` (`None`,
    /// `gen_const(jv_array())`).
    Array(Option<Box<Node>>),
    /// Rule 96: `'{' DictPairs '}'` (`gen_const_object` or the `gen_dictpair` block).
    Object(Vec<DictPair>),
    /// Rule 97: `"reduce" Expr "as" Patterns '(' Query ';' Query ')'` (`gen_reduce`).
    Reduce {
        source: Box<Node>,
        patterns: Vec<Pattern>,
        init: Box<Node>,
        update: Box<Node>,
    },
    /// Rules 98-99: `"foreach" Expr "as" Patterns '(' Query ';' Query [';' Query] ')'`
    /// (`gen_foreach`; a missing extract is `gen_noop()`).
    Foreach {
        source: Box<Node>,
        patterns: Vec<Pattern>,
        init: Box<Node>,
        update: Box<Node>,
        extract: Option<Box<Node>>,
    },
    /// Rule 100 `"if" Query "then" Query ElseBody` and rule 57 (`"elif"`, which
    /// becomes a nested `If` in `else_`). `else_` is rule 58's query, or `None` for
    /// rule 59 (`end`, i.e. `gen_noop()`).
    If {
        cond: Box<Node>,
        then_: Box<Node>,
        else_: Option<Box<Node>>,
    },
    /// Rule 102 `"try" Expr "catch" Expr` and rule 104 `"try" Expr`
    /// (`gen_try(body, handler or BACKTRACK)`).
    Try {
        body: Box<Node>,
        handler: Option<Box<Node>>,
    },
    /// Rule 105: `'$' '$' '$' BINDING`, jq's builtin-only `$$$$name` (`LOADVN`, at `@$`).
    VarTake(String),
    /// Rule 106: `BINDING` (`LOADV`, at `@$`). The name has no `$`.
    Var(String),
    /// Rule 107: `$__loc__` (`gen_loc_object`: `{"file": <locfile name>, "line": N}`).
    LocObject,
    /// Rules 108-109: `IDENT` / `IDENT '(' Args ')'` (`gen_call`, located at `name_loc`,
    /// which is `@1`). `null`, `true` and `false` without arguments are
    /// [`Literal`]s instead.
    Call {
        name: String,
        args: Vec<Node>,
        name_loc: Loc,
    },
    /// Error-recovery placeholder: `gen_noop()` in rules 63, 72, 73, 110, 111 and 113.
    /// Never constant.
    Error,
}

/// Binary operators of rules 16-37, in rule order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// `//` (rule 16, `gen_definedor`)
    Alt,
    /// `=` (rule 17, `_assign`)
    Assign,
    /// `or` (rule 18, `gen_or`)
    Or,
    /// `and` (rule 19, `gen_and`)
    And,
    /// `//=` (rule 20, `gen_definedor_assign`)
    AltAssign,
    /// `|=` (rule 21, `_modify`)
    Update,
    /// `+` (rule 22, `gen_binop`)
    Add,
    /// `+=` (rule 23, `gen_update`)
    AddAssign,
    /// `-` (rule 24)
    Sub,
    /// `-=` (rule 25)
    SubAssign,
    /// `*` (rule 26)
    Mul,
    /// `*=` (rule 27)
    MulAssign,
    /// `/` (rule 28)
    Div,
    /// `%` (rule 29)
    Mod,
    /// `/=` (rule 30)
    DivAssign,
    /// `%=` (rule 31)
    ModAssign,
    /// `==` (rule 32)
    Eq,
    /// `!=` (rule 33)
    Ne,
    /// `<` (rule 34)
    Lt,
    /// `>` (rule 35)
    Gt,
    /// `<=` (rule 36)
    Le,
    /// `>=` (rule 37)
    Ge,
}

impl BinOp {
    /// The operator as written in jq source.
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Alt => "//",
            BinOp::Assign => "=",
            BinOp::Or => "or",
            BinOp::And => "and",
            BinOp::AltAssign => "//=",
            BinOp::Update => "|=",
            BinOp::Add => "+",
            BinOp::AddAssign => "+=",
            BinOp::Sub => "-",
            BinOp::SubAssign => "-=",
            BinOp::Mul => "*",
            BinOp::MulAssign => "*=",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::DivAssign => "/=",
            BinOp::ModAssign => "%=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
        }
    }

    /// For the arithmetic update-assignments (`+=` ...), the arithmetic operator that
    /// `gen_update` applies.
    pub fn update_arith(self) -> Option<BinOp> {
        Some(match self {
            BinOp::AddAssign => BinOp::Add,
            BinOp::SubAssign => BinOp::Sub,
            BinOp::MulAssign => BinOp::Mul,
            BinOp::DivAssign => BinOp::Div,
            BinOp::ModAssign => BinOp::Mod,
            _ => return None,
        })
    }
}

/// Constants created by the parser (`gen_const`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    Null,
    True,
    False,
    /// A `LITERAL` token's source text, e.g. `1`, `1.50`, `.5`, `1.`, `1e3`, `007`.
    /// jq keeps it as a decNumber literal (`jv_number_with_literal`).
    Number(String),
    /// A constant string from a `FIELD` token (`.foo` → `"foo"`).
    String(String),
}

/// `String: StringStart QQString QQSTRING_END` (rules 51-56).
#[derive(Clone, Debug, PartialEq)]
pub struct StringLit {
    /// Rule 51's `FORMAT` (without `@`), or `None` for rule 52, where jq uses `"text"`.
    /// It applies to the interpolations only (`gen_format(query, fmt)`).
    pub format: Option<String>,
    /// Rules 55-56, in order. Consecutive `Text` parts are separate tokens (jq's lexer
    /// splits raw text and escape runs); they are not merged.
    pub parts: Vec<StrPart>,
    pub loc: Loc,
}

impl StringLit {
    /// The format applied to interpolations (`"text"` for a plain string).
    pub fn format_name(&self) -> &str {
        self.format.as_deref().unwrap_or("text")
    }

    /// jq's `block_is_const` for a `String`: true iff there are no interpolations
    /// (text parts fold into one constant).
    pub fn is_constant(&self) -> bool {
        self.parts.iter().all(|p| matches!(p, StrPart::Text(_)))
    }

    /// The folded value of a constant string.
    pub fn constant_value(&self) -> Option<String> {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                StrPart::Text(t) => out.push_str(t),
                StrPart::Interp(_) => return None,
            }
        }
        Some(out)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// A `QQSTRING_TEXT` token, unescaped. A token whose escapes failed to parse (the
    /// lexer reported an error) is `""`, which folds like jq's `jv_null()`.
    Text(String),
    /// `\( Query )`.
    Interp(Node),
}

/// `Pattern: BINDING | '[' ArrayPats ']' | '{' ObjPats '}'` (rules 121-123).
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    /// Rule 121: `$name` (`STOREV`).
    Var(String),
    /// Rule 122 with `ArrayPats` (rules 124-125, at least one element;
    /// `gen_array_matcher`).
    Array(Vec<Pattern>),
    /// Rule 123 with `ObjPats` (rules 126-127, at least one entry).
    Object(Vec<ObjPat>),
}

/// `ObjPat` (rules 128-134).
#[derive(Clone, Debug, PartialEq)]
pub struct ObjPat {
    pub kind: ObjPatKind,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObjPatKind {
    /// Rule 128: `$name`: binds `$name` to `.name`.
    Var(String),
    /// Rule 129: `$name: Pattern`: binds `$name` to `.name` and destructures it.
    VarPattern(String, Pattern),
    /// Rule 130 `IDENT ':' Pattern` and rule 131 `Keyword ':' Pattern`.
    Named(String, Pattern),
    /// Rule 132: `String ':' Pattern`.
    Str(StringLit, Pattern),
    /// Rule 133: `'(' Query ')' ':' Pattern` (`check_object_key` on the query).
    Computed {
        key: Node,
        /// `@2`: the query, without the parentheses.
        key_loc: Loc,
        pattern: Pattern,
    },
    /// Rule 134: `error ':' Pattern` (recovery; jq's value is just the pattern).
    Error(Pattern),
}

/// `DictPair` (rules 156-166).
#[derive(Clone, Debug, PartialEq)]
pub struct DictPair {
    pub kind: DictPairKind,
    /// `@$` of the pair. Rules 160, 161 and 163 locate their `LOADV` / `$__loc__` here.
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DictPairKind {
    /// Rule 156 `IDENT ':' DictExpr` and rule 157 `Keyword ':' DictExpr`: constant key.
    Named { key: String, value: Node },
    /// Rule 158: `String ':' DictExpr`.
    Str { key: StringLit, value: Node },
    /// Rule 159: `String` alone: `{"a"}` is `{"a": .["a"]}` (the key may be
    /// interpolated).
    StrShorthand(StringLit),
    /// Rule 160: `BINDING ':' DictExpr`: the key is the *value* of `$name`.
    VarKey { name: String, value: Node },
    /// Rule 161: `BINDING` alone: `{$x}` is `{"x": $x}`.
    Var(String),
    /// Rule 162 `IDENT` and rule 164 `Keyword` alone: `{a}` is `{"a": .a}`.
    NameShorthand(String),
    /// Rule 163: `{$__loc__}` is `{"__loc__": {"file": ..., "line": ...}}`.
    LocObject,
    /// Rule 165: `'(' Query ')' ':' DictExpr` (`check_object_key` on the query).
    Computed {
        key: Node,
        /// `@2`: the query, without the parentheses.
        key_loc: Loc,
        value: Node,
    },
    /// Rule 166: `error ':' DictExpr` (recovery; jq's value is just the `DictExpr`).
    Error(Node),
}

impl Node {
    pub fn new(kind: NodeKind, loc: Loc) -> Node {
        Node { kind, loc }
    }

    /// A compact s-expression rendering of the tree, for tests and debugging.
    pub fn to_sexpr(&self) -> String {
        let mut s = String::new();
        sexpr_node(&mut s, self);
        s
    }
}

impl Program {
    /// A compact s-expression rendering of the whole program.
    pub fn to_sexpr(&self) -> String {
        let mut s = String::new();
        if let Some(m) = &self.module {
            s.push_str("(module ");
            sexpr_node(&mut s, &m.meta);
            s.push_str(") ");
        }
        for imp in &self.imports {
            s.push('(');
            match &imp.kind {
                ImportKind::Code(name) => {
                    let _ = write!(s, "import {} {}", json_str(&imp.path), name);
                }
                ImportKind::Data(name) => {
                    let _ = write!(s, "import {} ${}", json_str(&imp.path), name);
                }
                ImportKind::Include => {
                    let _ = write!(s, "include {}", json_str(&imp.path));
                }
            }
            if let Some(meta) = &imp.meta {
                s.push(' ');
                sexpr_node(&mut s, meta);
            }
            s.push_str(") ");
        }
        match &self.body {
            ProgramBody::Main(n) => sexpr_node(&mut s, n),
            ProgramBody::Library(defs) => {
                s.push_str("(library");
                for d in defs {
                    s.push(' ');
                    sexpr_def(&mut s, d);
                }
                s.push(')');
            }
        }
        s
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn sexpr_def(s: &mut String, d: &FuncDef) {
    let _ = write!(s, "(def {}", d.name);
    if !d.params.is_empty() {
        s.push_str(" (");
        for (i, p) in d.params.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            if p.kind == ParamKind::Value {
                s.push('$');
            }
            s.push_str(&p.name);
        }
        s.push(')');
    }
    s.push(' ');
    sexpr_node(s, &d.body);
    s.push(')');
}

fn sexpr_string(s: &mut String, lit: &StringLit) {
    if let (None, [StrPart::Text(t)]) = (&lit.format, lit.parts.as_slice()) {
        s.push_str(&json_str(t));
        return;
    }
    s.push_str("(str");
    if let Some(f) = &lit.format {
        let _ = write!(s, " @{f}");
    }
    for p in &lit.parts {
        s.push(' ');
        match p {
            StrPart::Text(t) => s.push_str(&json_str(t)),
            StrPart::Interp(n) => {
                s.push_str("(interp ");
                sexpr_node(s, n);
                s.push(')');
            }
        }
    }
    s.push(')');
}

fn sexpr_patterns(s: &mut String, pats: &[Pattern]) {
    if pats.len() == 1 {
        sexpr_pattern(s, &pats[0]);
    } else {
        s.push_str("(?//");
        for p in pats {
            s.push(' ');
            sexpr_pattern(s, p);
        }
        s.push(')');
    }
}

fn sexpr_pattern(s: &mut String, p: &Pattern) {
    match &p.kind {
        PatternKind::Var(name) => {
            let _ = write!(s, "${name}");
        }
        PatternKind::Array(elems) => {
            s.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                sexpr_pattern(s, e);
            }
            s.push(']');
        }
        PatternKind::Object(entries) => {
            s.push('{');
            for (i, e) in entries.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                match &e.kind {
                    ObjPatKind::Var(name) => {
                        let _ = write!(s, "${name}");
                    }
                    ObjPatKind::VarPattern(name, pat) => {
                        let _ = write!(s, "(${name} ");
                        sexpr_pattern(s, pat);
                        s.push(')');
                    }
                    ObjPatKind::Named(name, pat) => {
                        let _ = write!(s, "({} ", json_str(name));
                        sexpr_pattern(s, pat);
                        s.push(')');
                    }
                    ObjPatKind::Str(key, pat) => {
                        s.push('(');
                        sexpr_string(s, key);
                        s.push(' ');
                        sexpr_pattern(s, pat);
                        s.push(')');
                    }
                    ObjPatKind::Computed { key, pattern, .. } => {
                        s.push_str("((");
                        sexpr_node(s, key);
                        s.push_str(") ");
                        sexpr_pattern(s, pattern);
                        s.push(')');
                    }
                    ObjPatKind::Error(pat) => {
                        s.push_str("(error ");
                        sexpr_pattern(s, pat);
                        s.push(')');
                    }
                }
            }
            s.push('}');
        }
    }
}

fn sexpr_opt(s: &mut String, n: &Option<Box<Node>>) {
    match n {
        Some(n) => sexpr_node(s, n),
        None => s.push('_'),
    }
}

fn sexpr_node(s: &mut String, n: &Node) {
    match &n.kind {
        NodeKind::FuncDef { def, rest } => {
            s.push_str("(defs ");
            sexpr_def(s, def);
            s.push(' ');
            sexpr_node(s, rest);
            s.push(')');
        }
        NodeKind::As {
            source,
            patterns,
            body,
        } => {
            s.push_str("(as ");
            sexpr_node(s, source);
            s.push(' ');
            sexpr_patterns(s, patterns);
            s.push(' ');
            sexpr_node(s, body);
            s.push(')');
        }
        NodeKind::Label { name, body } => {
            let _ = write!(s, "(label ${name} ");
            sexpr_node(s, body);
            s.push(')');
        }
        NodeKind::Pipe(a, b) => {
            s.push_str("(| ");
            sexpr_node(s, a);
            s.push(' ');
            sexpr_node(s, b);
            s.push(')');
        }
        NodeKind::Comma(a, b) => {
            s.push_str("(, ");
            sexpr_node(s, a);
            s.push(' ');
            sexpr_node(s, b);
            s.push(')');
        }
        NodeKind::Binary { op, lhs, rhs } => {
            let _ = write!(s, "({} ", op.as_str());
            sexpr_node(s, lhs);
            s.push(' ');
            sexpr_node(s, rhs);
            s.push(')');
        }
        NodeKind::Identity => s.push('.'),
        NodeKind::Recurse => s.push_str(".."),
        NodeKind::Break(name) => {
            let _ = write!(s, "(break ${name})");
        }
        NodeKind::Index {
            target,
            key,
            optional,
        } => {
            s.push_str(if *optional { "(index? " } else { "(index " });
            match target {
                Some(t) => sexpr_node(s, t),
                None => s.push('.'),
            }
            s.push(' ');
            sexpr_node(s, key);
            s.push(')');
        }
        NodeKind::Each { target, optional } => {
            s.push_str(if *optional { "(each? " } else { "(each " });
            sexpr_node(s, target);
            s.push(')');
        }
        NodeKind::Slice {
            target,
            from,
            to,
            optional,
        } => {
            s.push_str(if *optional { "(slice? " } else { "(slice " });
            sexpr_node(s, target);
            s.push(' ');
            sexpr_opt(s, from);
            s.push(' ');
            sexpr_opt(s, to);
            s.push(')');
        }
        NodeKind::Optional(t) => {
            s.push_str("(? ");
            sexpr_node(s, t);
            s.push(')');
        }
        NodeKind::Literal(lit) => match lit {
            Literal::Null => s.push_str("null"),
            Literal::True => s.push_str("true"),
            Literal::False => s.push_str("false"),
            Literal::Number(text) => s.push_str(text),
            Literal::String(text) => s.push_str(&json_str(text)),
        },
        NodeKind::Str(lit) => sexpr_string(s, lit),
        NodeKind::Format(name) => {
            let _ = write!(s, "@{name}");
        }
        NodeKind::Neg(t) => {
            s.push_str("(neg ");
            sexpr_node(s, t);
            s.push(')');
        }
        NodeKind::Array(q) => match q {
            Some(q) => {
                s.push_str("(array ");
                sexpr_node(s, q);
                s.push(')');
            }
            None => s.push_str("(array)"),
        },
        NodeKind::Object(pairs) => {
            s.push_str("(object");
            for p in pairs {
                s.push(' ');
                sexpr_pair(s, p);
            }
            s.push(')');
        }
        NodeKind::Reduce {
            source,
            patterns,
            init,
            update,
        } => {
            s.push_str("(reduce ");
            sexpr_node(s, source);
            s.push(' ');
            sexpr_patterns(s, patterns);
            s.push(' ');
            sexpr_node(s, init);
            s.push(' ');
            sexpr_node(s, update);
            s.push(')');
        }
        NodeKind::Foreach {
            source,
            patterns,
            init,
            update,
            extract,
        } => {
            s.push_str("(foreach ");
            sexpr_node(s, source);
            s.push(' ');
            sexpr_patterns(s, patterns);
            s.push(' ');
            sexpr_node(s, init);
            s.push(' ');
            sexpr_node(s, update);
            if let Some(e) = extract {
                s.push(' ');
                sexpr_node(s, e);
            }
            s.push(')');
        }
        NodeKind::If { cond, then_, else_ } => {
            s.push_str("(if ");
            sexpr_node(s, cond);
            s.push(' ');
            sexpr_node(s, then_);
            if let Some(e) = else_ {
                s.push(' ');
                sexpr_node(s, e);
            }
            s.push(')');
        }
        NodeKind::Try { body, handler } => {
            s.push_str("(try ");
            sexpr_node(s, body);
            if let Some(h) = handler {
                s.push(' ');
                sexpr_node(s, h);
            }
            s.push(')');
        }
        NodeKind::VarTake(name) => {
            let _ = write!(s, "$$$${name}");
        }
        NodeKind::Var(name) => {
            let _ = write!(s, "${name}");
        }
        NodeKind::LocObject => s.push_str("$__loc__"),
        NodeKind::Call { name, args, .. } => {
            if args.is_empty() {
                s.push_str(name);
            } else {
                let _ = write!(s, "(call {name}");
                for a in args {
                    s.push(' ');
                    sexpr_node(s, a);
                }
                s.push(')');
            }
        }
        NodeKind::Error => s.push_str("(error)"),
    }
}

fn sexpr_pair(s: &mut String, p: &DictPair) {
    match &p.kind {
        DictPairKind::Named { key, value } => {
            let _ = write!(s, "({} ", json_str(key));
            sexpr_node(s, value);
            s.push(')');
        }
        DictPairKind::Str { key, value } => {
            s.push('(');
            sexpr_string(s, key);
            s.push(' ');
            sexpr_node(s, value);
            s.push(')');
        }
        DictPairKind::StrShorthand(key) => {
            s.push_str("(shorthand ");
            sexpr_string(s, key);
            s.push(')');
        }
        DictPairKind::VarKey { name, value } => {
            let _ = write!(s, "(${name} ");
            sexpr_node(s, value);
            s.push(')');
        }
        DictPairKind::Var(name) => {
            let _ = write!(s, "${name}");
        }
        DictPairKind::NameShorthand(name) => s.push_str(name),
        DictPairKind::LocObject => s.push_str("$__loc__"),
        DictPairKind::Computed { key, value, .. } => {
            s.push_str("((");
            sexpr_node(s, key);
            s.push_str(") ");
            sexpr_node(s, value);
            s.push(')');
        }
        DictPairKind::Error(value) => {
            s.push_str("(error ");
            sexpr_node(s, value);
            s.push(')');
        }
    }
}
