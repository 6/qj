mod builtins;
pub mod eval;
pub mod lexer;
pub mod parser;
mod value_ops;

use crate::value::Value;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A destructuring pattern for variable binding.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// Simple variable: `$x`
    Var(String),
    /// Array destructuring: `[$a, $b, $c]`
    Array(Vec<Pattern>),
    /// Object destructuring: `{a: $x, $y}` (shorthand $y means key="y", bind $y)
    Object(Vec<(PatternKey, Pattern)>),
}

/// Key in an object destructuring pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternKey {
    /// Literal string key: `{a: $x}` or `{"foo bar": $x}`
    Name(String),
    /// Variable shorthand: `{$x}` means key="x", bind to $x
    Var(String),
    /// Computed expression key: `{("expr"): $x}`
    Expr(Box<Filter>),
}

/// A jq filter AST node.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Identity: `.`
    Identity,
    /// Field access: `.foo`
    Field(String),
    /// Array/object index: `.[0]`, `.[-1]`
    Index(Box<Filter>),
    /// Pipe: `a | b`
    Pipe(Box<Filter>, Box<Filter>),
    /// Array/object iteration: `.[]`
    Iterate,
    /// Select: `select(expr)` — filters to inputs where expr is truthy
    Select(Box<Filter>),
    /// Object construction: `{a: .x, b: .y}`
    ObjectConstruct(Vec<(ObjKey, Box<Filter>)>),
    /// Array construction: `[expr]`
    ArrayConstruct(Box<Filter>),
    /// Literal value
    Literal(Value),
    /// Comparison: `a == b`, `a > b`, etc.
    Compare(Box<Filter>, CmpOp, Box<Filter>),
    /// Arithmetic: `a + b`, `a - b`, etc.
    Arith(Box<Filter>, ArithOp, Box<Filter>),
    /// Comma (multiple outputs): `a, b`
    Comma(Vec<Filter>),
    /// Recursive descent: `..`
    Recurse,
    /// Builtin function call: `length`, `keys`, `type`, etc.
    Builtin(String, Vec<Filter>),
    /// Negation: `not`
    Not(Box<Filter>),
    /// Boolean: `a and b`, `a or b`
    BoolOp(Box<Filter>, BoolOp, Box<Filter>),
    /// If-then-else: `if cond then a else b end`
    IfThenElse(Box<Filter>, Box<Filter>, Option<Box<Filter>>),
    /// Alternative: `a // b`
    Alternative(Box<Filter>, Box<Filter>),
    /// Try: `.foo?`
    Try(Box<Filter>),
    /// String interpolation: `"hello \(.name)"`
    StringInterp(Vec<StringPart>),
    /// Negate numeric value (unary minus)
    Neg(Box<Filter>),
    /// Try-catch: `try expr catch handler`
    TryCatch(Box<Filter>, Box<Filter>),
    /// Array/string slice: `.[start:end]`
    Slice(Option<Box<Filter>>, Option<Box<Filter>>),
    /// Variable reference: `$name`
    Var(String),
    /// Variable binding: `expr as pattern | body`
    Bind(Box<Filter>, Pattern, Box<Filter>),
    /// Reduce: `reduce source as pattern (init; update)`
    Reduce(Box<Filter>, Pattern, Box<Filter>, Box<Filter>),
    /// Foreach: `foreach source as pattern (init; update; extract?)`
    Foreach(
        Box<Filter>,
        Pattern,
        Box<Filter>,
        Box<Filter>,
        Option<Box<Filter>>,
    ),
    /// Assignment: `path |= expr`, `path = expr`, `path += expr`, etc.
    Assign(Box<Filter>, AssignOp, Box<Filter>),
    /// User-defined function: `def name(params): body; rest`
    Def {
        name: String,
        params: Vec<String>,
        body: Box<Filter>,
        rest: Box<Filter>,
    },
    /// Alternative match: `expr as pat1 ?// pat2 ?// ... | body`
    /// Tries each pattern left-to-right, uses first that matches.
    AltBind(Box<Filter>, Vec<Pattern>, Box<Filter>),
    /// Label: `label $name | body` — catches `break $name` signals
    Label(String, Box<Filter>),
    /// Break: `break $name` — signals an unwind to matching `label $name`
    Break(String),
    /// Postfix index: `A[B]` — evaluates A for navigation and B against
    /// the same (original) input, then indexes result-of-A with result-of-B.
    PostfixIndex(Box<Filter>, Box<Filter>),
    /// Postfix slice: `A[s:e]` — evaluates A, s, e against same input.
    PostfixSlice(Box<Filter>, Option<Box<Filter>>, Option<Box<Filter>>),
}

/// Object construction key — can be a literal string or computed.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjKey {
    Name(String),
    Expr(Box<Filter>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Update, // |=
    Set,    // =
    Add,    // +=
    Sub,    // -=
    Mul,    // *=
    Div,    // /=
    Mod,    // %=
    Alt,    // //=
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Lit(String),
    Expr(Filter),
}

/// A user-defined function captured from a `def` expression.
#[derive(Debug, Clone)]
pub struct UserFunc {
    pub params: Vec<String>,
    pub body: Filter,
    pub closure_env: Env,
    /// True for real `def` functions, false for filter parameter wrappers.
    /// Used by the evaluator to decide whether to self-register for recursion.
    pub is_def: bool,
}

/// Evaluation environment: variable bindings + user-defined functions.
#[derive(Debug, Clone)]
pub struct Env {
    vars: Rc<HashMap<String, Value>>,
    /// User-defined functions keyed by (name, arity).
    funcs: Rc<HashMap<(String, usize), UserFunc>>,
}

impl Env {
    pub fn empty() -> Self {
        Env {
            vars: Rc::new(HashMap::new()),
            funcs: Rc::new(HashMap::new()),
        }
    }

    /// Returns true if the environment has no variable bindings.
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// Look up a variable binding (e.g., "$x").
    pub fn get_var(&self, name: &str) -> Option<&Value> {
        self.vars.get(name)
    }

    /// Create a new env with an additional variable binding.
    pub fn bind_var(&self, name: String, value: Value) -> Env {
        let mut new_vars = (*self.vars).clone();
        new_vars.insert(name, value);
        Env {
            vars: Rc::new(new_vars),
            funcs: self.funcs.clone(),
        }
    }

    /// Register a user-defined function.
    pub fn bind_func(&self, name: String, arity: usize, func: UserFunc) -> Env {
        let mut new_funcs = (*self.funcs).clone();
        new_funcs.insert((name, arity), func);
        Env {
            vars: self.vars.clone(),
            funcs: Rc::new(new_funcs),
        }
    }

    /// Look up a user-defined function by (name, arity).
    pub fn get_func(&self, name: &str, arity: usize) -> Option<&UserFunc> {
        self.funcs.get(&(name.to_string(), arity))
    }
}

/// Detected passthrough-eligible filter patterns that can bypass the
/// full DOM parse → Value → eval → output pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassthroughPath {
    /// `.` — identity; with compact output, use simdjson::minify() directly.
    Identity,
    /// `.field | length` or bare `length` — compute length in C++.
    FieldLength(Vec<String>),
    /// `.field | keys` or bare `keys` — compute keys in C++.
    FieldKeys(Vec<String>),
}

impl PassthroughPath {
    /// Whether this passthrough requires compact output mode (`-c`).
    /// Scalar results like `length` look the same in any mode.
    pub fn requires_compact(&self) -> bool {
        match self {
            PassthroughPath::Identity => true,
            PassthroughPath::FieldLength(_) => false,
            PassthroughPath::FieldKeys(_) => false,
        }
    }
}

/// Collect a chain of Field accesses from a Pipe tree.
/// Returns true if the entire tree is a chain of `.field` accesses.
/// Public for use in NDJSON fast-path detection.
pub fn collect_field_chain(filter: &Filter, fields: &mut Vec<String>) -> bool {
    match filter {
        Filter::Field(name) => {
            fields.push(name.clone());
            true
        }
        Filter::Pipe(a, b) => collect_field_chain(a, fields) && collect_field_chain(b, fields),
        _ => false,
    }
}

/// Decompose `Pipe(field_chain, Builtin(name, []))` patterns.
/// Returns `Some((fields, builtin_name))` if the filter is a field chain piped
/// into a zero-arg builtin; `None` otherwise.
pub(crate) fn decompose_field_builtin(filter: &Filter) -> Option<(Vec<String>, &str)> {
    match filter {
        Filter::Pipe(lhs, rhs) => {
            if let Filter::Builtin(name, args) = rhs.as_ref() {
                if !args.is_empty() {
                    return None;
                }
                let mut fields = Vec::new();
                if collect_field_chain(lhs, &mut fields) {
                    return Some((fields, name.as_str()));
                }
            }
            None
        }
        _ => None,
    }
}

/// Check if a parsed filter is eligible for a fast passthrough path.
pub fn passthrough_path(filter: &Filter) -> Option<PassthroughPath> {
    match filter {
        Filter::Identity => Some(PassthroughPath::Identity),
        // Bare `length` or `keys` (no field prefix)
        Filter::Builtin(name, args) if args.is_empty() => match name.as_str() {
            "length" => Some(PassthroughPath::FieldLength(vec![])),
            "keys" => Some(PassthroughPath::FieldKeys(vec![])),
            _ => None,
        },
        Filter::Pipe(_, _) => {
            // Check for .field | length / .field | keys first
            if let Some((fields, builtin)) = decompose_field_builtin(filter) {
                match builtin {
                    "length" => return Some(PassthroughPath::FieldLength(fields)),
                    "keys" => return Some(PassthroughPath::FieldKeys(fields)),
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse a jq filter expression string into a `Filter` AST.
pub fn parse(input: &str) -> anyhow::Result<Filter> {
    let tokens = lexer::lex(input)?;
    parser::parse(&tokens)
}

impl Filter {
    /// Check if this filter can be safely shared across threads.
    ///
    /// Returns `false` if the filter tree contains any `Value::Array` or
    /// `Value::Object` literals (which use `Rc` and are not safe to clone
    /// from multiple threads simultaneously).
    pub fn is_parallel_safe(&self) -> bool {
        match self {
            Filter::Literal(Value::Array(_) | Value::Object(_)) => false,
            Filter::Literal(_)
            | Filter::Identity
            | Filter::Iterate
            | Filter::Recurse
            | Filter::Field(_)
            | Filter::Var(_) => true,
            Filter::Index(f)
            | Filter::Select(f)
            | Filter::ArrayConstruct(f)
            | Filter::Not(f)
            | Filter::Try(f)
            | Filter::Neg(f) => f.is_parallel_safe(),
            Filter::Pipe(a, b)
            | Filter::Compare(a, _, b)
            | Filter::Arith(a, _, b)
            | Filter::BoolOp(a, _, b)
            | Filter::Alternative(a, b)
            | Filter::TryCatch(a, b) => a.is_parallel_safe() && b.is_parallel_safe(),
            Filter::Bind(a, _, b) => a.is_parallel_safe() && b.is_parallel_safe(),
            Filter::Comma(filters) | Filter::Builtin(_, filters) => {
                filters.iter().all(|f| f.is_parallel_safe())
            }
            Filter::ObjectConstruct(pairs) => pairs.iter().all(|(k, v)| {
                (match k {
                    ObjKey::Name(_) => true,
                    ObjKey::Expr(f) => f.is_parallel_safe(),
                }) && v.is_parallel_safe()
            }),
            Filter::Slice(s, e) => {
                s.as_ref().is_none_or(|f| f.is_parallel_safe())
                    && e.as_ref().is_none_or(|f| f.is_parallel_safe())
            }
            Filter::IfThenElse(c, t, e) => {
                c.is_parallel_safe()
                    && t.is_parallel_safe()
                    && e.as_ref().is_none_or(|f| f.is_parallel_safe())
            }
            Filter::Reduce(src, _, init, update) => {
                src.is_parallel_safe() && init.is_parallel_safe() && update.is_parallel_safe()
            }
            Filter::Foreach(src, _, init, update, extract) => {
                src.is_parallel_safe()
                    && init.is_parallel_safe()
                    && update.is_parallel_safe()
                    && extract.as_ref().is_none_or(|f| f.is_parallel_safe())
            }
            Filter::Assign(path, _, rhs) => path.is_parallel_safe() && rhs.is_parallel_safe(),
            Filter::Def { body, rest, .. } => body.is_parallel_safe() && rest.is_parallel_safe(),
            Filter::AltBind(expr, _, body) => expr.is_parallel_safe() && body.is_parallel_safe(),
            Filter::Label(_, body) => body.is_parallel_safe(),
            Filter::Break(_) => true,
            Filter::PostfixIndex(base, idx) => base.is_parallel_safe() && idx.is_parallel_safe(),
            Filter::PostfixSlice(base, s, e) => {
                base.is_parallel_safe()
                    && s.as_ref().is_none_or(|f| f.is_parallel_safe())
                    && e.as_ref().is_none_or(|f| f.is_parallel_safe())
            }
            Filter::StringInterp(parts) => parts.iter().all(|p| match p {
                StringPart::Lit(_) => true,
                StringPart::Expr(f) => f.is_parallel_safe(),
            }),
        }
    }
}

impl Filter {
    /// Check if this filter AST uses `input` or `inputs` builtins,
    /// which require sequential processing (not parallel NDJSON).
    pub fn uses_input_builtins(&self) -> bool {
        match self {
            Filter::Builtin(name, args) => {
                if name == "input" || name == "inputs" {
                    return true;
                }
                args.iter().any(|f| f.uses_input_builtins())
            }
            Filter::Identity
            | Filter::Iterate
            | Filter::Recurse
            | Filter::Field(_)
            | Filter::Var(_)
            | Filter::Literal(_)
            | Filter::Break(_) => false,
            Filter::Index(f)
            | Filter::Select(f)
            | Filter::ArrayConstruct(f)
            | Filter::Not(f)
            | Filter::Try(f)
            | Filter::Neg(f) => f.uses_input_builtins(),
            Filter::Pipe(a, b)
            | Filter::Compare(a, _, b)
            | Filter::Arith(a, _, b)
            | Filter::BoolOp(a, _, b)
            | Filter::Alternative(a, b)
            | Filter::TryCatch(a, b)
            | Filter::Assign(a, _, b) => a.uses_input_builtins() || b.uses_input_builtins(),
            Filter::Bind(a, _, b) | Filter::AltBind(a, _, b) => {
                a.uses_input_builtins() || b.uses_input_builtins()
            }
            Filter::Comma(filters) => filters.iter().any(|f| f.uses_input_builtins()),
            Filter::ObjectConstruct(pairs) => pairs.iter().any(|(k, v)| {
                (match k {
                    ObjKey::Name(_) => false,
                    ObjKey::Expr(f) => f.uses_input_builtins(),
                }) || v.uses_input_builtins()
            }),
            Filter::Slice(s, e) => {
                s.as_ref().is_some_and(|f| f.uses_input_builtins())
                    || e.as_ref().is_some_and(|f| f.uses_input_builtins())
            }
            Filter::IfThenElse(c, t, e) => {
                c.uses_input_builtins()
                    || t.uses_input_builtins()
                    || e.as_ref().is_some_and(|f| f.uses_input_builtins())
            }
            Filter::Reduce(src, _, init, update) => {
                src.uses_input_builtins()
                    || init.uses_input_builtins()
                    || update.uses_input_builtins()
            }
            Filter::Foreach(src, _, init, update, extract) => {
                src.uses_input_builtins()
                    || init.uses_input_builtins()
                    || update.uses_input_builtins()
                    || extract.as_ref().is_some_and(|f| f.uses_input_builtins())
            }
            Filter::Def { body, rest, .. } => {
                body.uses_input_builtins() || rest.uses_input_builtins()
            }
            Filter::Label(_, body) => body.uses_input_builtins(),
            Filter::PostfixIndex(base, idx) => {
                base.uses_input_builtins() || idx.uses_input_builtins()
            }
            Filter::PostfixSlice(base, s, e) => {
                base.uses_input_builtins()
                    || s.as_ref().is_some_and(|f| f.uses_input_builtins())
                    || e.as_ref().is_some_and(|f| f.uses_input_builtins())
            }
            Filter::StringInterp(parts) => parts.iter().any(|p| match p {
                StringPart::Lit(_) => false,
                StringPart::Expr(f) => f.uses_input_builtins(),
            }),
        }
    }
}

impl Filter {
    /// Collect all variable references (`$name`) from the filter AST.
    pub fn collect_var_refs(&self, out: &mut HashSet<String>) {
        match self {
            Filter::Var(name) => {
                out.insert(name.clone());
            }
            Filter::Identity
            | Filter::Iterate
            | Filter::Recurse
            | Filter::Field(_)
            | Filter::Literal(_)
            | Filter::Break(_) => {}
            Filter::Index(f)
            | Filter::Select(f)
            | Filter::ArrayConstruct(f)
            | Filter::Not(f)
            | Filter::Try(f)
            | Filter::Neg(f) => f.collect_var_refs(out),
            Filter::Pipe(a, b)
            | Filter::Compare(a, _, b)
            | Filter::Arith(a, _, b)
            | Filter::BoolOp(a, _, b)
            | Filter::Alternative(a, b)
            | Filter::TryCatch(a, b)
            | Filter::Assign(a, _, b) => {
                a.collect_var_refs(out);
                b.collect_var_refs(out);
            }
            Filter::Bind(a, pat, b) => {
                a.collect_var_refs(out);
                collect_pattern_var_refs(pat, out);
                b.collect_var_refs(out);
            }
            Filter::AltBind(expr, pats, body) => {
                expr.collect_var_refs(out);
                for pat in pats {
                    collect_pattern_var_refs(pat, out);
                }
                body.collect_var_refs(out);
            }
            Filter::Comma(filters) | Filter::Builtin(_, filters) => {
                for f in filters {
                    f.collect_var_refs(out);
                }
            }
            Filter::ObjectConstruct(pairs) => {
                for (k, v) in pairs {
                    if let ObjKey::Expr(f) = k {
                        f.collect_var_refs(out);
                    }
                    v.collect_var_refs(out);
                }
            }
            Filter::Slice(s, e) => {
                if let Some(f) = s {
                    f.collect_var_refs(out);
                }
                if let Some(f) = e {
                    f.collect_var_refs(out);
                }
            }
            Filter::IfThenElse(c, t, e) => {
                c.collect_var_refs(out);
                t.collect_var_refs(out);
                if let Some(f) = e {
                    f.collect_var_refs(out);
                }
            }
            Filter::Reduce(src, pat, init, update) => {
                src.collect_var_refs(out);
                collect_pattern_var_refs(pat, out);
                init.collect_var_refs(out);
                update.collect_var_refs(out);
            }
            Filter::Foreach(src, pat, init, update, extract) => {
                src.collect_var_refs(out);
                collect_pattern_var_refs(pat, out);
                init.collect_var_refs(out);
                update.collect_var_refs(out);
                if let Some(f) = extract {
                    f.collect_var_refs(out);
                }
            }
            Filter::Def { body, rest, .. } => {
                body.collect_var_refs(out);
                rest.collect_var_refs(out);
            }
            Filter::Label(_, body) => body.collect_var_refs(out),
            Filter::PostfixIndex(base, idx) => {
                base.collect_var_refs(out);
                idx.collect_var_refs(out);
            }
            Filter::PostfixSlice(base, s, e) => {
                base.collect_var_refs(out);
                if let Some(f) = s {
                    f.collect_var_refs(out);
                }
                if let Some(f) = e {
                    f.collect_var_refs(out);
                }
            }
            Filter::StringInterp(parts) => {
                for p in parts {
                    if let StringPart::Expr(f) = p {
                        f.collect_var_refs(out);
                    }
                }
            }
        }
    }
}

/// Collect variable references from a destructuring pattern.
fn collect_pattern_var_refs(pat: &Pattern, out: &mut HashSet<String>) {
    match pat {
        Pattern::Var(name) => {
            out.insert(name.clone());
        }
        Pattern::Array(pats) => {
            for p in pats {
                collect_pattern_var_refs(p, out);
            }
        }
        Pattern::Object(pairs) => {
            for (key, p) in pairs {
                if let PatternKey::Var(name) = key {
                    out.insert(name.clone());
                }
                if let PatternKey::Expr(f) = key {
                    f.collect_var_refs(out);
                }
                collect_pattern_var_refs(p, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_safety_check() {
        // Simple filters are parallel-safe
        assert!(Filter::Identity.is_parallel_safe());
        assert!(Filter::Field("name".into()).is_parallel_safe());
        assert!(Filter::Literal(Value::Int(42)).is_parallel_safe());
        assert!(Filter::Literal(Value::String("hello".into())).is_parallel_safe());

        // Literal arrays/objects are NOT parallel-safe
        assert!(!Filter::Literal(Value::Array(std::rc::Rc::new(vec![]))).is_parallel_safe());
        assert!(!Filter::Literal(Value::Object(std::rc::Rc::new(vec![]))).is_parallel_safe());

        // Nested unsafe literal
        assert!(
            !Filter::Pipe(
                Box::new(Filter::Identity),
                Box::new(Filter::Literal(Value::Array(std::rc::Rc::new(vec![])))),
            )
            .is_parallel_safe()
        );
    }
}
