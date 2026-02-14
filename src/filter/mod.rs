mod builtins;
pub mod eval;
pub mod lexer;
pub mod parser;
mod value_ops;

use crate::value::Value;
use std::collections::HashMap;
use std::rc::Rc;

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
    /// Variable binding: `expr as $name | body`
    Bind(Box<Filter>, String, Box<Filter>),
    /// Reduce: `reduce source as $var (init; update)`
    Reduce(Box<Filter>, String, Box<Filter>, Box<Filter>),
    /// Foreach: `foreach source as $var (init; update; extract?)`
    Foreach(
        Box<Filter>,
        String,
        Box<Filter>,
        Box<Filter>,
        Option<Box<Filter>>,
    ),
    /// Assignment: `path |= expr`, `path = expr`, `path += expr`, etc.
    Assign(Box<Filter>, AssignOp, Box<Filter>),
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

/// Evaluation environment: variable bindings for `as $var` / `reduce` / `foreach`.
#[derive(Debug, Clone)]
pub struct Env {
    vars: Rc<HashMap<String, Value>>,
}

impl Env {
    pub fn empty() -> Self {
        Env {
            vars: Rc::new(HashMap::new()),
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
        }
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
fn collect_field_chain(filter: &Filter, fields: &mut Vec<String>) -> bool {
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
fn decompose_field_builtin(filter: &Filter) -> Option<(Vec<String>, &str)> {
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
            | Filter::Bind(a, _, b)
            | Filter::TryCatch(a, b) => a.is_parallel_safe() && b.is_parallel_safe(),
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
            Filter::StringInterp(parts) => parts.iter().all(|p| match p {
                StringPart::Lit(_) => true,
                StringPart::Expr(f) => f.is_parallel_safe(),
            }),
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
