pub mod eval;
pub mod lexer;
pub mod parser;

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

/// Parse a jq filter expression string into a `Filter` AST.
pub fn parse(input: &str) -> anyhow::Result<Filter> {
    let tokens = lexer::lex(input)?;
    parser::parse(&tokens)
}
