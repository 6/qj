/// Recursive descent parser for jq filter expressions.
///
/// Grammar (simplified):
///   expr     = pipe
///   pipe     = comma ("|" comma)*
///   comma    = compare ("," compare)*
///   compare  = arith (("==" | "!=" | "<" | "<=" | ">" | ">=") arith)?
///   arith    = postfix (("+" | "-" | "*" | "/" | "%") postfix)*
///   postfix  = primary ("." ident | "[" expr "]" | "[]" | "?")*
///   primary  = "." | "." ident | literal | "(" expr ")" | "[" expr "]"
///            | "{" obj_pairs "}" | "select" "(" expr ")"
///            | ident ("(" args ")")? | "if" expr "then" expr ("else" expr)? "end"
///            | "-" primary | "not"
use anyhow::{Result, bail};

use super::lexer::Token;
use super::{ArithOp, BoolOp, CmpOp, Filter, ObjKey};
use crate::value::Value;
use std::rc::Rc;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, expected: &Token) -> Result<()> {
        match self.advance() {
            Some(tok) if tok == expected => Ok(()),
            Some(tok) => bail!("expected {expected:?}, got {tok:?}"),
            None => bail!("expected {expected:?}, got end of input"),
        }
    }

    // expr = pipe
    fn parse_expr(&mut self) -> Result<Filter> {
        self.parse_pipe()
    }

    // pipe = comma ("|" comma)*
    fn parse_pipe(&mut self) -> Result<Filter> {
        let mut left = self.parse_comma()?;
        while self.peek() == Some(&Token::Pipe) {
            self.advance();
            let right = self.parse_comma()?;
            left = Filter::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // comma = alternative ("," alternative)*
    fn parse_comma(&mut self) -> Result<Filter> {
        let first = self.parse_alternative()?;
        if self.peek() != Some(&Token::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.peek() == Some(&Token::Comma) {
            self.advance();
            items.push(self.parse_alternative()?);
        }
        Ok(Filter::Comma(items))
    }

    // alternative = bool_op ("//" bool_op)*
    fn parse_alternative(&mut self) -> Result<Filter> {
        let mut left = self.parse_bool_op()?;
        while self.peek() == Some(&Token::DoubleSlash) {
            self.advance();
            let right = self.parse_bool_op()?;
            left = Filter::Alternative(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // bool_op = compare (("and" | "or") compare)*
    fn parse_bool_op(&mut self) -> Result<Filter> {
        let mut left = self.parse_compare()?;
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.advance();
                    let right = self.parse_compare()?;
                    left = Filter::BoolOp(Box::new(left), BoolOp::And, Box::new(right));
                }
                Some(Token::Or) => {
                    self.advance();
                    let right = self.parse_compare()?;
                    left = Filter::BoolOp(Box::new(left), BoolOp::Or, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    // compare = arith (cmp_op arith)?
    fn parse_compare(&mut self) -> Result<Filter> {
        let left = self.parse_arith()?;
        let op = match self.peek() {
            Some(Token::Eq) => CmpOp::Eq,
            Some(Token::Ne) => CmpOp::Ne,
            Some(Token::Lt) => CmpOp::Lt,
            Some(Token::Le) => CmpOp::Le,
            Some(Token::Gt) => CmpOp::Gt,
            Some(Token::Ge) => CmpOp::Ge,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_arith()?;
        Ok(Filter::Compare(Box::new(left), op, Box::new(right)))
    }

    // arith = postfix (("+"|"-"|"*"|"/"|"%") postfix)*
    fn parse_arith(&mut self) -> Result<Filter> {
        let mut left = self.parse_postfix()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => ArithOp::Add,
                Some(Token::Minus) => ArithOp::Sub,
                Some(Token::Star) => ArithOp::Mul,
                Some(Token::Slash) => ArithOp::Div,
                Some(Token::Percent) => ArithOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_postfix()?;
            left = Filter::Arith(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    // postfix = primary ("." ident | "[" expr "]" | "[]" | "?")*
    fn parse_postfix(&mut self) -> Result<Filter> {
        let mut node = self.parse_primary()?;
        loop {
            match self.peek() {
                Some(Token::Dot) => {
                    self.advance();
                    match self.peek() {
                        Some(Token::Ident(_)) => {
                            let name = match self.advance().unwrap() {
                                Token::Ident(s) => s.clone(),
                                _ => unreachable!(),
                            };
                            node = Filter::Pipe(Box::new(node), Box::new(Filter::Field(name)));
                        }
                        _ => {
                            bail!("expected field name after '.'");
                        }
                    }
                }
                Some(Token::LBrack) => {
                    self.advance();
                    if self.peek() == Some(&Token::RBrack) {
                        // .[] — iterate
                        self.advance();
                        node = Filter::Pipe(Box::new(node), Box::new(Filter::Iterate));
                    } else {
                        // .[expr] — index
                        let idx = self.parse_expr()?;
                        self.expect(&Token::RBrack)?;
                        node = Filter::Pipe(Box::new(node), Box::new(Filter::Index(Box::new(idx))));
                    }
                }
                Some(Token::Question) => {
                    self.advance();
                    node = Filter::Try(Box::new(node));
                }
                _ => break,
            }
        }
        Ok(node)
    }

    // primary = "." (ident | "[" ... ) | literal | "(" expr ")" | "[" expr "]"
    //         | "{" obj "}" | select(...) | ident | if-then-else | "-" primary
    fn parse_primary(&mut self) -> Result<Filter> {
        match self.peek() {
            Some(Token::Dot) => {
                self.advance();
                match self.peek() {
                    Some(Token::Ident(_)) => {
                        let name = match self.advance().unwrap() {
                            Token::Ident(s) => s.clone(),
                            _ => unreachable!(),
                        };
                        Ok(Filter::Field(name))
                    }
                    Some(Token::LBrack) => {
                        self.advance();
                        if self.peek() == Some(&Token::RBrack) {
                            self.advance();
                            Ok(Filter::Iterate)
                        } else {
                            let idx = self.parse_expr()?;
                            self.expect(&Token::RBrack)?;
                            Ok(Filter::Index(Box::new(idx)))
                        }
                    }
                    Some(Token::Dot) => {
                        // ".." — recursive descent
                        self.advance();
                        Ok(Filter::Recurse)
                    }
                    _ => Ok(Filter::Identity),
                }
            }
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Some(Token::LBrack) => {
                // Array construction: [expr]
                self.advance();
                if self.peek() == Some(&Token::RBrack) {
                    self.advance();
                    return Ok(Filter::Literal(Value::Array(Rc::new(vec![]))));
                }
                let expr = self.parse_expr()?;
                self.expect(&Token::RBrack)?;
                Ok(Filter::ArrayConstruct(Box::new(expr)))
            }
            Some(Token::LBrace) => self.parse_object_construct(),
            Some(Token::Select) => {
                self.advance();
                self.expect(&Token::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(Filter::Select(Box::new(cond)))
            }
            Some(Token::If) => {
                self.advance();
                let cond = self.parse_expr()?;
                self.expect(&Token::Then)?;
                let then_branch = self.parse_expr()?;
                let else_branch = if self.peek() == Some(&Token::Else) {
                    self.advance();
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };
                self.expect(&Token::End)?;
                Ok(Filter::IfThenElse(
                    Box::new(cond),
                    Box::new(then_branch),
                    else_branch,
                ))
            }
            Some(Token::Not) => {
                self.advance();
                Ok(Filter::Not(Box::new(Filter::Identity)))
            }
            Some(Token::Minus) => {
                self.advance();
                let inner = self.parse_primary()?;
                Ok(Filter::Neg(Box::new(inner)))
            }
            // Literals
            Some(Token::Null) => {
                self.advance();
                Ok(Filter::Literal(Value::Null))
            }
            Some(Token::True) => {
                self.advance();
                Ok(Filter::Literal(Value::Bool(true)))
            }
            Some(Token::False) => {
                self.advance();
                Ok(Filter::Literal(Value::Bool(false)))
            }
            Some(Token::Int(_)) => {
                let n = match self.advance().unwrap() {
                    Token::Int(n) => *n,
                    _ => unreachable!(),
                };
                Ok(Filter::Literal(Value::Int(n)))
            }
            Some(Token::Float(_)) => {
                let f = match self.advance().unwrap() {
                    Token::Float(f) => *f,
                    _ => unreachable!(),
                };
                Ok(Filter::Literal(Value::Double(f, None)))
            }
            Some(Token::Str(_)) => {
                let s = match self.advance().unwrap() {
                    Token::Str(s) => s.clone(),
                    _ => unreachable!(),
                };
                // Check for string interpolation: if s contains \( patterns,
                // we would need to handle it. For now, just literal.
                Ok(Filter::Literal(Value::String(s)))
            }
            // Named identifier — builtin or function call
            Some(Token::Ident(_)) => {
                let name = match self.advance().unwrap() {
                    Token::Ident(s) => s.clone(),
                    _ => unreachable!(),
                };
                // Check for function call: name(args)
                if self.peek() == Some(&Token::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    if self.peek() != Some(&Token::RParen) {
                        args.push(self.parse_expr()?);
                        while self.peek() == Some(&Token::Semicolon) {
                            self.advance();
                            args.push(self.parse_expr()?);
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Filter::Builtin(name, args))
                } else {
                    // Zero-arg builtin: length, keys, values, type, etc.
                    Ok(Filter::Builtin(name, vec![]))
                }
            }
            Some(tok) => bail!("unexpected token: {tok:?}"),
            None => bail!("unexpected end of filter expression"),
        }
    }

    fn parse_object_construct(&mut self) -> Result<Filter> {
        self.expect(&Token::LBrace)?;
        let mut pairs = Vec::new();

        if self.peek() == Some(&Token::RBrace) {
            self.advance();
            return Ok(Filter::ObjectConstruct(pairs));
        }

        loop {
            let (key, val) = self.parse_obj_pair()?;
            pairs.push((key, Box::new(val)));
            if self.peek() != Some(&Token::Comma) {
                break;
            }
            self.advance(); // consume comma
        }

        self.expect(&Token::RBrace)?;
        Ok(Filter::ObjectConstruct(pairs))
    }

    fn parse_obj_pair(&mut self) -> Result<(ObjKey, Filter)> {
        // Key can be: ident, string, or (expr)
        let key = match self.peek() {
            Some(Token::Ident(_)) => {
                let name = match self.advance().unwrap() {
                    Token::Ident(s) => s.clone(),
                    _ => unreachable!(),
                };
                ObjKey::Name(name)
            }
            Some(Token::Str(_)) => {
                let s = match self.advance().unwrap() {
                    Token::Str(s) => s.clone(),
                    _ => unreachable!(),
                };
                ObjKey::Name(s)
            }
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                ObjKey::Expr(Box::new(expr))
            }
            _ => bail!("expected object key"),
        };

        // If no colon follows, it's a shorthand: {name} means {name: .name}
        if self.peek() != Some(&Token::Colon) {
            if let ObjKey::Name(ref name) = key {
                return Ok((key.clone(), Filter::Field(name.clone())));
            }
            bail!("computed key must have a value expression");
        }

        self.expect(&Token::Colon)?;
        // Parse value at pipe level — but NOT comma level,
        // since comma separates object pairs.
        let val = self.parse_pipe_no_comma()?;
        Ok((key, val))
    }

    // pipe without comma — used in object values and function args
    fn parse_pipe_no_comma(&mut self) -> Result<Filter> {
        let mut left = self.parse_alternative()?;
        while self.peek() == Some(&Token::Pipe) {
            self.advance();
            let right = self.parse_alternative()?;
            left = Filter::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
}

pub fn parse(tokens: &[Token]) -> Result<Filter> {
    let mut parser = Parser::new(tokens);
    let filter = parser.parse_expr()?;
    if parser.pos < parser.tokens.len() {
        bail!(
            "unexpected token after filter: {:?}",
            parser.tokens[parser.pos]
        );
    }
    Ok(filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::lexer;

    fn p(input: &str) -> Filter {
        let tokens = lexer::lex(input).unwrap();
        parse(&tokens).unwrap()
    }

    #[test]
    fn parse_identity() {
        assert_eq!(p("."), Filter::Identity);
    }

    #[test]
    fn parse_field() {
        assert_eq!(p(".name"), Filter::Field("name".into()));
    }

    #[test]
    fn parse_nested_field() {
        assert_eq!(
            p(".a.b"),
            Filter::Pipe(
                Box::new(Filter::Field("a".into())),
                Box::new(Filter::Field("b".into()))
            )
        );
    }

    #[test]
    fn parse_iterate() {
        assert_eq!(p(".[]"), Filter::Iterate);
    }

    #[test]
    fn parse_pipe() {
        assert_eq!(
            p(".[] | .name"),
            Filter::Pipe(
                Box::new(Filter::Iterate),
                Box::new(Filter::Field("name".into()))
            )
        );
    }

    #[test]
    fn parse_select() {
        assert_eq!(
            p("select(.age > 30)"),
            Filter::Select(Box::new(Filter::Compare(
                Box::new(Filter::Field("age".into())),
                CmpOp::Gt,
                Box::new(Filter::Literal(Value::Int(30))),
            )))
        );
    }

    #[test]
    fn parse_object_construct() {
        assert_eq!(
            p("{name: .name, id: .id}"),
            Filter::ObjectConstruct(vec![
                (
                    ObjKey::Name("name".into()),
                    Box::new(Filter::Field("name".into()))
                ),
                (
                    ObjKey::Name("id".into()),
                    Box::new(Filter::Field("id".into()))
                ),
            ])
        );
    }

    #[test]
    fn parse_object_shorthand() {
        assert_eq!(
            p("{name, id}"),
            Filter::ObjectConstruct(vec![
                (
                    ObjKey::Name("name".into()),
                    Box::new(Filter::Field("name".into()))
                ),
                (
                    ObjKey::Name("id".into()),
                    Box::new(Filter::Field("id".into()))
                ),
            ])
        );
    }

    #[test]
    fn parse_array_construct() {
        assert_eq!(
            p("[.[] | .x]"),
            Filter::ArrayConstruct(Box::new(Filter::Pipe(
                Box::new(Filter::Iterate),
                Box::new(Filter::Field("x".into())),
            )))
        );
    }

    #[test]
    fn parse_index() {
        assert_eq!(
            p(".[0]"),
            Filter::Index(Box::new(Filter::Literal(Value::Int(0))))
        );
    }

    #[test]
    fn parse_arithmetic() {
        assert_eq!(
            p(".x + 1"),
            Filter::Arith(
                Box::new(Filter::Field("x".into())),
                ArithOp::Add,
                Box::new(Filter::Literal(Value::Int(1))),
            )
        );
    }

    #[test]
    fn parse_comma() {
        assert_eq!(
            p(".a, .b"),
            Filter::Comma(vec![Filter::Field("a".into()), Filter::Field("b".into()),])
        );
    }

    #[test]
    fn parse_literal_null() {
        assert_eq!(p("null"), Filter::Literal(Value::Null));
    }

    #[test]
    fn parse_if_then_else() {
        assert_eq!(
            p("if .x then .a else .b end"),
            Filter::IfThenElse(
                Box::new(Filter::Field("x".into())),
                Box::new(Filter::Field("a".into())),
                Some(Box::new(Filter::Field("b".into()))),
            )
        );
    }

    #[test]
    fn parse_alternative() {
        assert_eq!(
            p(".x // null"),
            Filter::Alternative(
                Box::new(Filter::Field("x".into())),
                Box::new(Filter::Literal(Value::Null)),
            )
        );
    }

    #[test]
    fn parse_builtin_no_args() {
        assert_eq!(p("length"), Filter::Builtin("length".into(), vec![]));
    }

    #[test]
    fn parse_builtin_with_args() {
        assert_eq!(
            p("map(.x)"),
            Filter::Builtin("map".into(), vec![Filter::Field("x".into())])
        );
    }

    #[test]
    fn parse_try() {
        assert_eq!(
            p(".foo?"),
            Filter::Try(Box::new(Filter::Field("foo".into())))
        );
    }

    #[test]
    fn parse_complex_pipeline() {
        // .items[] | select(.active == true) | {name: .name, score: .score}
        let f = p(".items[] | select(.active == true) | {name: .name, score: .score}");
        match f {
            Filter::Pipe(_, _) => {} // Just verify it parses
            other => panic!("expected Pipe, got {other:?}"),
        }
    }
}
