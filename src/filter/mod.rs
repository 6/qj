mod builtins;
pub mod eval;
pub mod lexer;
pub mod parser;
mod value_ops;

use crate::value::Value;

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
pub enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Lit(String),
    Expr(Filter),
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
