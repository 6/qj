/// Recursive descent parser for jq filter expressions.
///
/// Grammar (simplified):
///   expr     = pipe
///   pipe     = comma ("|" comma)*
///   comma    = compare ("," compare)*
///   compare  = arith (("==" | "!=" | "<" | "<=" | ">" | ">=") arith)?
///   arith    = mul_div (("+" | "-") mul_div)*
///   mul_div  = postfix (("*" | "/" | "%") postfix)*
///   postfix  = primary ("." ident | "[" expr "]" | "[]" | "?")*
///   primary  = "." | "." ident | literal | "(" expr ")" | "[" expr "]"
///            | "{" obj_pairs "}" | "select" "(" expr ")"
///            | ident ("(" args ")")? | "if" expr "then" expr ("else" expr)? "end"
///            | "-" primary | "not"
use anyhow::{Result, bail};

use super::lexer::{StringSegment, Token};
use super::{ArithOp, AssignOp, BoolOp, CmpOp, Filter, ObjKey, Pattern, PatternKey, StringPart};
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

    // pipe = def | comma ("|" comma)*
    fn parse_pipe(&mut self) -> Result<Filter> {
        // Check for `def` at the start of a pipe expression
        if self.peek() == Some(&Token::Def) {
            return self.parse_def();
        }

        let mut left = self.parse_comma()?;

        while self.peek() == Some(&Token::Pipe) {
            self.advance();
            let right = self.parse_comma()?;
            left = Filter::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn peek_assign_op(&self) -> Option<AssignOp> {
        match self.peek()? {
            Token::UpdateAssign => Some(AssignOp::Update),
            Token::Assign => Some(AssignOp::Set),
            Token::PlusAssign => Some(AssignOp::Add),
            Token::MinusAssign => Some(AssignOp::Sub),
            Token::StarAssign => Some(AssignOp::Mul),
            Token::SlashAssign => Some(AssignOp::Div),
            Token::PercentAssign => Some(AssignOp::Mod),
            Token::AltAssign => Some(AssignOp::Alt),
            _ => None,
        }
    }

    /// Parse an if-elif-else-end chain. Called after `if` or `elif` is consumed.
    /// Desugars `elif` into nested IfThenElse.
    fn parse_if_chain(&mut self) -> Result<Filter> {
        let cond = self.parse_expr()?;
        self.expect(&Token::Then)?;
        let then_branch = self.parse_expr()?;
        let else_branch = match self.peek() {
            Some(Token::Elif) => {
                self.advance();
                Some(Box::new(self.parse_if_chain()?))
            }
            Some(Token::Else) => {
                self.advance();
                let e = self.parse_expr()?;
                self.expect(&Token::End)?;
                Some(Box::new(e))
            }
            _ => {
                self.expect(&Token::End)?;
                None
            }
        };
        Ok(Filter::IfThenElse(
            Box::new(cond),
            Box::new(then_branch),
            else_branch,
        ))
    }

    /// Parse `def name: body;` or `def name(p1; p2): body;`
    /// followed by a continuation (the rest of the expression).
    fn parse_def(&mut self) -> Result<Filter> {
        self.advance(); // consume `def`

        // Parse function name
        let name = match self.advance() {
            Some(Token::Ident(s)) => s.clone(),
            Some(tok) => bail!("expected function name after 'def', got {tok:?}"),
            None => bail!("expected function name after 'def', got end of input"),
        };

        // Parse optional parameters: (p1; p2) or ($p1; $p2)
        let mut params = Vec::new();
        if self.peek() == Some(&Token::LParen) {
            self.advance(); // consume '('
            if self.peek() != Some(&Token::RParen) {
                params.push(self.parse_def_param()?);
                while self.peek() == Some(&Token::Semicolon) {
                    self.advance();
                    params.push(self.parse_def_param()?);
                }
            }
            self.expect(&Token::RParen)?;
        }

        self.expect(&Token::Colon)?;

        // Parse body (everything until `;`)
        let body = self.parse_expr()?;

        self.expect(&Token::Semicolon)?;

        // Parse continuation (the rest of the expression after the `;`)
        let rest = self.parse_pipe()?;

        Ok(Filter::Def {
            name,
            params,
            body: Box::new(body),
            rest: Box::new(rest),
        })
    }

    /// Parse a single def parameter name. Accepts `name` or `$name`.
    fn parse_def_param(&mut self) -> Result<String> {
        match self.advance() {
            Some(Token::Ident(s)) => Ok(s.clone()),
            Some(tok) => bail!("expected parameter name in def, got {tok:?}"),
            None => bail!("expected parameter name in def, got end of input"),
        }
    }

    /// Parse `as pattern | body` or `as pat1 ?// pat2 ?// ... | body`
    fn parse_as_binding(&mut self, expr: Filter) -> Result<Filter> {
        self.advance(); // consume `as`
        let first_pattern = self.parse_pattern()?;

        // Check for ?// chain
        if self.peek() == Some(&Token::QuestionDoubleSlash) {
            let mut patterns = vec![first_pattern];
            while self.peek() == Some(&Token::QuestionDoubleSlash) {
                self.advance(); // consume ?//
                patterns.push(self.parse_pattern()?);
            }
            self.expect(&Token::Pipe)?;
            let body = self.parse_pipe()?;
            Ok(Filter::AltBind(Box::new(expr), patterns, Box::new(body)))
        } else {
            self.expect(&Token::Pipe)?;
            let body = self.parse_pipe()?;
            Ok(Filter::Bind(Box::new(expr), first_pattern, Box::new(body)))
        }
    }

    /// Parse a destructuring pattern: $var, [$a, $b], {key: $var, $shorthand}
    fn parse_pattern(&mut self) -> Result<Pattern> {
        match self.peek() {
            Some(Token::Ident(s)) if s.starts_with('$') => {
                let name = s.clone();
                self.advance();
                Ok(Pattern::Var(name))
            }
            Some(Token::LBrack) => {
                self.advance();
                let mut patterns = Vec::new();
                if self.peek() != Some(&Token::RBrack) {
                    patterns.push(self.parse_pattern()?);
                    while self.peek() == Some(&Token::Comma) {
                        self.advance();
                        patterns.push(self.parse_pattern()?);
                    }
                }
                self.expect(&Token::RBrack)?;
                Ok(Pattern::Array(patterns))
            }
            Some(Token::LBrace) => {
                self.advance();
                let mut pairs = Vec::new();
                if self.peek() != Some(&Token::RBrace) {
                    pairs.push(self.parse_pattern_obj_pair()?);
                    while self.peek() == Some(&Token::Comma) {
                        self.advance();
                        pairs.push(self.parse_pattern_obj_pair()?);
                    }
                }
                self.expect(&Token::RBrace)?;
                Ok(Pattern::Object(pairs))
            }
            Some(tok) => bail!("expected pattern ($var, [...], or {{...}}), got {tok:?}"),
            None => bail!("expected pattern, got end of input"),
        }
    }

    /// Parse a single key: pattern pair in an object destructuring pattern.
    fn parse_pattern_obj_pair(&mut self) -> Result<(PatternKey, Pattern)> {
        match self.peek() {
            // $shorthand: `{$x}` means key="x", bind $x
            Some(Token::Ident(s)) if s.starts_with('$') => {
                let name = s.clone();
                self.advance();
                // Check for `: pattern` (explicit value binding)
                if self.peek() == Some(&Token::Colon) {
                    self.advance();
                    let pat = self.parse_pattern()?;
                    Ok((PatternKey::Var(name), pat))
                } else {
                    // Shorthand: {$x} means key from variable name
                    Ok((PatternKey::Var(name.clone()), Pattern::Var(name)))
                }
            }
            // key: pattern
            Some(Token::Ident(_)) => {
                let key = match self.advance().unwrap() {
                    Token::Ident(s) => s.clone(),
                    _ => unreachable!(),
                };
                self.expect(&Token::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((PatternKey::Name(key), pat))
            }
            Some(Token::Str(_)) => {
                let key = match self.advance().unwrap() {
                    Token::Str(s) => s.clone(),
                    _ => unreachable!(),
                };
                self.expect(&Token::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((PatternKey::Name(key), pat))
            }
            // Expression key: `("expr"): $var`
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                self.expect(&Token::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((PatternKey::Expr(Box::new(expr)), pat))
            }
            _ if self.peek_keyword_as_obj_key().is_some() => {
                let key = self.peek_keyword_as_obj_key().unwrap().to_string();
                self.advance();
                self.expect(&Token::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((PatternKey::Name(key), pat))
            }
            Some(tok) => bail!("expected pattern key, got {tok:?}"),
            None => bail!("expected pattern key, got end of input"),
        }
    }

    // comma = alternative ("," alternative)*
    fn parse_comma(&mut self) -> Result<Filter> {
        let first = self.parse_assign()?;
        if self.peek() != Some(&Token::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.peek() == Some(&Token::Comma) {
            self.advance();
            items.push(self.parse_assign()?);
        }
        Ok(Filter::Comma(items))
    }

    // assign = alternative (assign_op assign | "as" pattern "|" pipe)?
    fn parse_assign(&mut self) -> Result<Filter> {
        let left = self.parse_alternative()?;

        // Check for assignment operators (right-recursive within assign level)
        if let Some(op) = self.peek_assign_op() {
            self.advance();
            let right = self.parse_assign()?;
            return Ok(Filter::Assign(Box::new(left), op, Box::new(right)));
        }

        // Check for `expr as $var | body`
        if self.peek() == Some(&Token::As) {
            return self.parse_as_binding(left);
        }

        Ok(left)
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

    // arith = mul_div (("+"|"-") mul_div)*
    fn parse_arith(&mut self) -> Result<Filter> {
        let mut left = self.parse_mul_div()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => ArithOp::Add,
                Some(Token::Minus) => ArithOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul_div()?;
            left = Filter::Arith(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    // mul_div = postfix (("*"|"/"|"%") postfix)*
    fn parse_mul_div(&mut self) -> Result<Filter> {
        let mut left = self.parse_postfix()?;
        loop {
            let op = match self.peek() {
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
                        Some(Token::Str(_)) => {
                            let name = match self.advance().unwrap() {
                                Token::Str(s) => s.clone(),
                                _ => unreachable!(),
                            };
                            node = Filter::Pipe(Box::new(node), Box::new(Filter::Field(name)));
                        }
                        _ if self.peek_keyword_as_field().is_some() => {
                            let name = self.peek_keyword_as_field().unwrap().to_string();
                            self.advance();
                            node = Filter::Pipe(Box::new(node), Box::new(Filter::Field(name)));
                        }
                        Some(Token::LBrack) => {
                            // .foo.[] or .foo.[expr] — treat as postfix bracket
                            // Don't consume the dot; fall through to LBrack case
                            continue;
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
                        // .[expr] or .[start:end] — postfix form
                        // In jq, index/slice expressions evaluate against
                        // the original input, not the navigated result.
                        let inner = self.parse_bracket_index_or_slice()?;
                        match inner {
                            Filter::Index(idx) => {
                                node = Filter::PostfixIndex(Box::new(node), idx);
                            }
                            Filter::Slice(s, e) => {
                                node = Filter::PostfixSlice(Box::new(node), s, e);
                            }
                            _ => {
                                node = Filter::Pipe(Box::new(node), Box::new(inner));
                            }
                        }
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
                    Some(Token::Str(_)) => {
                        let name = match self.advance().unwrap() {
                            Token::Str(s) => s.clone(),
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
                            self.parse_bracket_index_or_slice()
                        }
                    }
                    Some(Token::Dot) => {
                        // ".." — recursive descent
                        self.advance();
                        Ok(Filter::Recurse)
                    }
                    _ if self.peek_keyword_as_field().is_some() => {
                        let name = self.peek_keyword_as_field().unwrap().to_string();
                        self.advance();
                        Ok(Filter::Field(name))
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
                self.parse_if_chain()
            }
            Some(Token::Reduce) => {
                // reduce source as pattern (init; update)
                self.advance();
                let source = self.parse_compare()?;
                self.expect(&Token::As)?;
                let pattern = self.parse_pattern()?;
                self.expect(&Token::LParen)?;
                let init = self.parse_expr()?;
                self.expect(&Token::Semicolon)?;
                let update = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(Filter::Reduce(
                    Box::new(source),
                    pattern,
                    Box::new(init),
                    Box::new(update),
                ))
            }
            Some(Token::Foreach) => {
                // foreach source as pattern (init; update) or
                // foreach source as pattern (init; update; extract)
                self.advance();
                let source = self.parse_compare()?;
                self.expect(&Token::As)?;
                let pattern = self.parse_pattern()?;
                self.expect(&Token::LParen)?;
                let init = self.parse_expr()?;
                self.expect(&Token::Semicolon)?;
                let update = self.parse_expr()?;
                let extract = if self.peek() == Some(&Token::Semicolon) {
                    self.advance();
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };
                self.expect(&Token::RParen)?;
                Ok(Filter::Foreach(
                    Box::new(source),
                    pattern,
                    Box::new(init),
                    Box::new(update),
                    extract,
                ))
            }
            Some(Token::Try) => {
                self.advance();
                let body = self.parse_postfix()?;
                if self.peek() == Some(&Token::Catch) {
                    self.advance();
                    let handler = self.parse_postfix()?;
                    Ok(Filter::TryCatch(Box::new(body), Box::new(handler)))
                } else {
                    Ok(Filter::Try(Box::new(body)))
                }
            }
            Some(Token::Label) => {
                self.advance();
                let name = match self.advance() {
                    Some(Token::Ident(s)) if s.starts_with('$') => s.clone(),
                    Some(tok) => bail!("expected $name after 'label', got {tok:?}"),
                    None => bail!("expected $name after 'label', got end of input"),
                };
                self.expect(&Token::Pipe)?;
                let body = self.parse_pipe()?;
                Ok(Filter::Label(name, Box::new(body)))
            }
            Some(Token::Break) => {
                self.advance();
                let name = match self.advance() {
                    Some(Token::Ident(s)) if s.starts_with('$') => s.clone(),
                    Some(tok) => bail!("expected $name after 'break', got {tok:?}"),
                    None => bail!("expected $name after 'break', got end of input"),
                };
                Ok(Filter::Break(name))
            }
            Some(Token::Not) => {
                self.advance();
                Ok(Filter::Not(Box::new(Filter::Identity)))
            }
            Some(Token::Minus) => {
                self.advance();
                let inner = self.parse_postfix()?;
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
                Ok(Filter::Literal(Value::String(s)))
            }
            Some(Token::InterpStr(_)) => {
                let segments = match self.advance().unwrap() {
                    Token::InterpStr(s) => s.clone(),
                    _ => unreachable!(),
                };
                let mut parts = Vec::new();
                for seg in segments {
                    match seg {
                        StringSegment::Lit(s) => parts.push(StringPart::Lit(s)),
                        StringSegment::Expr(expr_text) => {
                            let tokens = super::lexer::lex(&expr_text)?;
                            let filter = parse(&tokens)?;
                            parts.push(StringPart::Expr(filter));
                        }
                    }
                }
                Ok(Filter::StringInterp(parts))
            }
            Some(Token::Format(_)) => {
                let name = match self.advance().unwrap() {
                    Token::Format(s) => s.clone(),
                    _ => unreachable!(),
                };
                // @format "string_interp" — apply format to interpolated expressions
                if matches!(self.peek(), Some(Token::Str(_) | Token::InterpStr(_))) {
                    let str_filter = self.parse_primary()?;
                    // Wrap each interpolated Expr part with the format builtin
                    if let Filter::StringInterp(parts) = str_filter {
                        let wrapped_parts = parts
                            .into_iter()
                            .map(|part| match part {
                                crate::filter::StringPart::Lit(s) => {
                                    crate::filter::StringPart::Lit(s)
                                }
                                crate::filter::StringPart::Expr(f) => {
                                    crate::filter::StringPart::Expr(Filter::Pipe(
                                        Box::new(f),
                                        Box::new(Filter::Builtin(name.clone(), vec![])),
                                    ))
                                }
                            })
                            .collect();
                        Ok(Filter::StringInterp(wrapped_parts))
                    } else {
                        // Plain string, no interpolation — apply format to whole thing
                        Ok(Filter::Pipe(
                            Box::new(str_filter),
                            Box::new(Filter::Builtin(name, vec![])),
                        ))
                    }
                } else {
                    Ok(Filter::Builtin(name, vec![]))
                }
            }
            // Named identifier — builtin, function call, or variable
            Some(Token::Ident(_)) => {
                let name = match self.advance().unwrap() {
                    Token::Ident(s) => s.clone(),
                    _ => unreachable!(),
                };
                // Variable reference: $name
                if name.starts_with('$') {
                    return Ok(Filter::Var(name));
                }
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

    /// Parse the contents of `[...]` after the `[` has been consumed.
    /// Returns Index or Slice depending on whether `:` is present.
    fn parse_bracket_index_or_slice(&mut self) -> Result<Filter> {
        // [:end] — no start
        if self.peek() == Some(&Token::Colon) {
            self.advance();
            let end = if self.peek() == Some(&Token::RBrack) {
                None
            } else {
                Some(Box::new(self.parse_expr()?))
            };
            self.expect(&Token::RBrack)?;
            return Ok(Filter::Slice(None, end));
        }

        let first = self.parse_expr()?;

        // [start:end] or [start:]
        if self.peek() == Some(&Token::Colon) {
            self.advance();
            let end = if self.peek() == Some(&Token::RBrack) {
                None
            } else {
                Some(Box::new(self.parse_expr()?))
            };
            self.expect(&Token::RBrack)?;
            return Ok(Filter::Slice(Some(Box::new(first)), end));
        }

        // Regular [index]
        self.expect(&Token::RBrack)?;
        Ok(Filter::Index(Box::new(first)))
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

    /// Check if the current token is a keyword that can be used as an identifier
    /// in object key context (e.g., `{if: 1, then: 2}`).
    fn peek_keyword_as_obj_key(&self) -> Option<&str> {
        match self.peek()? {
            Token::If => Some("if"),
            Token::Then => Some("then"),
            Token::Else => Some("else"),
            Token::Elif => Some("elif"),
            Token::End => Some("end"),
            Token::And => Some("and"),
            Token::Or => Some("or"),
            Token::Not => Some("not"),
            Token::As => Some("as"),
            Token::Try => Some("try"),
            Token::Catch => Some("catch"),
            Token::Reduce => Some("reduce"),
            Token::Foreach => Some("foreach"),
            Token::Select => Some("select"),
            Token::Def => Some("def"),
            Token::Label => Some("label"),
            Token::Break => Some("break"),
            Token::True => Some("true"),
            Token::False => Some("false"),
            Token::Null => Some("null"),
            _ => None,
        }
    }

    /// Check if the current token is a keyword that can be used as a field name
    /// after `.` (e.g., `.not`, `.and`). Only includes keywords that cannot appear
    /// after `.` in any other syntactic context.
    fn peek_keyword_as_field(&self) -> Option<&str> {
        match self.peek()? {
            Token::Not => Some("not"),
            _ => None,
        }
    }

    fn parse_obj_pair(&mut self) -> Result<(ObjKey, Filter)> {
        // Key can be: ident, keyword-as-ident, string, interp-string, or (expr)
        let key = match self.peek() {
            Some(Token::Ident(s)) if s.starts_with('$') => {
                // $var reference: {$x} means key="x" (name without $), value=$x
                let name = s.clone();
                self.advance();
                // Check for colon → explicit value: {$x: expr}
                if self.peek() == Some(&Token::Colon) {
                    self.advance();
                    let val = self.parse_pipe_no_comma()?;
                    return Ok((ObjKey::Expr(Box::new(Filter::Var(name))), val));
                }
                // Shorthand: {$x} → key="x", value=$x
                let key_name = name[1..].to_string();
                let val = Filter::Var(name);
                return Ok((ObjKey::Name(key_name), val));
            }
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
            Some(Token::InterpStr(_)) => {
                // String interpolation key: {"key\(expr)": value}
                let segments = match self.advance().unwrap() {
                    Token::InterpStr(s) => s.clone(),
                    _ => unreachable!(),
                };
                let mut parts = Vec::new();
                for seg in segments {
                    match seg {
                        super::lexer::StringSegment::Lit(s) => parts.push(StringPart::Lit(s)),
                        super::lexer::StringSegment::Expr(expr_text) => {
                            let tokens = super::lexer::lex(&expr_text)?;
                            let filter = parse(&tokens)?;
                            parts.push(StringPart::Expr(filter));
                        }
                    }
                }
                ObjKey::Expr(Box::new(Filter::StringInterp(parts)))
            }
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                ObjKey::Expr(Box::new(expr))
            }
            _ if self.peek_keyword_as_obj_key().is_some() => {
                let name = self.peek_keyword_as_obj_key().unwrap().to_string();
                self.advance();
                ObjKey::Name(name)
            }
            _ => bail!("expected object key"),
        };

        // If no colon follows, it's a shorthand: {name} means {name: .name}
        if self.peek() != Some(&Token::Colon) {
            match key {
                ObjKey::Name(name) => {
                    let val = Filter::Field(name.clone());
                    return Ok((ObjKey::Name(name), val));
                }
                ObjKey::Expr(expr) => {
                    let val = Filter::Index(Box::new((*expr).clone()));
                    return Ok((ObjKey::Expr(expr), val));
                }
            }
        }

        self.expect(&Token::Colon)?;
        // Parse value at pipe level — but NOT comma level,
        // since comma separates object pairs.
        let val = self.parse_pipe_no_comma()?;
        Ok((key, val))
    }

    // pipe without comma — used in object values and function args
    fn parse_pipe_no_comma(&mut self) -> Result<Filter> {
        let mut left = self.parse_assign()?;

        while self.peek() == Some(&Token::Pipe) {
            self.advance();
            let right = self.parse_assign()?;
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
    fn parse_operator_precedence_mul_add() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3), not (1 + 2) * 3
        assert_eq!(
            p("1 + 2 * 3"),
            Filter::Arith(
                Box::new(Filter::Literal(Value::Int(1))),
                ArithOp::Add,
                Box::new(Filter::Arith(
                    Box::new(Filter::Literal(Value::Int(2))),
                    ArithOp::Mul,
                    Box::new(Filter::Literal(Value::Int(3))),
                )),
            )
        );
    }

    #[test]
    fn parse_operator_precedence_div_sub() {
        // 10 - 6 / 2 should parse as 10 - (6 / 2)
        assert_eq!(
            p("10 - 6 / 2"),
            Filter::Arith(
                Box::new(Filter::Literal(Value::Int(10))),
                ArithOp::Sub,
                Box::new(Filter::Arith(
                    Box::new(Filter::Literal(Value::Int(6))),
                    ArithOp::Div,
                    Box::new(Filter::Literal(Value::Int(2))),
                )),
            )
        );
    }

    #[test]
    fn parse_operator_precedence_mod() {
        // 5 + 7 % 3 should parse as 5 + (7 % 3)
        assert_eq!(
            p("5 + 7 % 3"),
            Filter::Arith(
                Box::new(Filter::Literal(Value::Int(5))),
                ArithOp::Add,
                Box::new(Filter::Arith(
                    Box::new(Filter::Literal(Value::Int(7))),
                    ArithOp::Mod,
                    Box::new(Filter::Literal(Value::Int(3))),
                )),
            )
        );
    }

    #[test]
    fn parse_same_precedence_left_assoc() {
        // 2 * 3 / 4 should parse as (2 * 3) / 4 (left-to-right)
        assert_eq!(
            p("2 * 3 / 4"),
            Filter::Arith(
                Box::new(Filter::Arith(
                    Box::new(Filter::Literal(Value::Int(2))),
                    ArithOp::Mul,
                    Box::new(Filter::Literal(Value::Int(3))),
                )),
                ArithOp::Div,
                Box::new(Filter::Literal(Value::Int(4))),
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

    // --- Phase 2: variables ---

    #[test]
    fn parse_var_reference() {
        assert_eq!(p("$x"), Filter::Var("$x".into()));
    }

    #[test]
    fn parse_as_binding() {
        // `. as $x | $x` → Bind(Identity, Var("$x"), Var("$x"))
        assert_eq!(
            p(". as $x | $x"),
            Filter::Bind(
                Box::new(Filter::Identity),
                Pattern::Var("$x".into()),
                Box::new(Filter::Var("$x".into())),
            )
        );
    }

    #[test]
    fn parse_as_binding_in_pipe() {
        // `.[] | . as $x | $x` → Pipe(Iterate, Bind(Identity, Var("$x"), Var("$x")))
        let f = p(".[] | . as $x | $x");
        match f {
            Filter::Pipe(left, right) => {
                assert_eq!(*left, Filter::Iterate);
                match *right {
                    Filter::Bind(expr, ref pat, _) => {
                        assert_eq!(*expr, Filter::Identity);
                        assert_eq!(*pat, Pattern::Var("$x".into()));
                    }
                    other => panic!("expected Bind, got {other:?}"),
                }
            }
            other => panic!("expected Pipe, got {other:?}"),
        }
    }

    #[test]
    fn parse_chained_bindings() {
        // `1 as $x | 2 as $y | $x + $y`
        let f = p("1 as $x | 2 as $y | $x + $y");
        match f {
            Filter::Bind(_, ref pat, ref body) => {
                assert_eq!(*pat, Pattern::Var("$x".into()));
                match body.as_ref() {
                    Filter::Bind(_, pat2, _) => {
                        assert_eq!(*pat2, Pattern::Var("$y".into()))
                    }
                    other => panic!("expected nested Bind, got {other:?}"),
                }
            }
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    // --- Phase 2: slicing ---

    #[test]
    fn parse_slice_both() {
        // `.[2:4]` → Slice(Some(2), Some(4))
        match p(".[2:4]") {
            Filter::Slice(Some(s), Some(e)) => {
                assert_eq!(*s, Filter::Literal(Value::Int(2)));
                assert_eq!(*e, Filter::Literal(Value::Int(4)));
            }
            other => panic!("expected Slice, got {other:?}"),
        }
    }

    #[test]
    fn parse_slice_no_start() {
        // `.[:3]` → Slice(None, Some(3))
        match p(".[:3]") {
            Filter::Slice(None, Some(e)) => {
                assert_eq!(*e, Filter::Literal(Value::Int(3)));
            }
            other => panic!("expected Slice(None, Some), got {other:?}"),
        }
    }

    #[test]
    fn parse_slice_no_end() {
        // `.[2:]` → Slice(Some(2), None)
        match p(".[2:]") {
            Filter::Slice(Some(s), None) => {
                assert_eq!(*s, Filter::Literal(Value::Int(2)));
            }
            other => panic!("expected Slice(Some, None), got {other:?}"),
        }
    }

    // --- Phase 2: elif ---

    #[test]
    fn parse_elif() {
        let f = p("if . then 1 elif . then 2 else 3 end");
        match f {
            Filter::IfThenElse(_, _, Some(else_branch)) => match *else_branch {
                Filter::IfThenElse(_, _, Some(_)) => {} // nested if from elif
                other => panic!("expected nested IfThenElse from elif, got {other:?}"),
            },
            other => panic!("expected IfThenElse, got {other:?}"),
        }
    }

    // --- Phase 2: try-catch ---

    #[test]
    fn parse_try_keyword() {
        // `try .foo` → Try(Field("foo"))
        assert_eq!(
            p("try .foo"),
            Filter::Try(Box::new(Filter::Field("foo".into())))
        );
    }

    #[test]
    fn parse_try_catch() {
        // `try .foo catch .bar` → TryCatch(Field("foo"), Field("bar"))
        assert_eq!(
            p("try .foo catch .bar"),
            Filter::TryCatch(
                Box::new(Filter::Field("foo".into())),
                Box::new(Filter::Field("bar".into())),
            )
        );
    }

    // --- Phase 2: reduce ---

    #[test]
    fn parse_reduce() {
        let f = p("reduce .[] as $x (0; . + $x)");
        match f {
            Filter::Reduce(source, pat, init, _update) => {
                assert_eq!(*source, Filter::Iterate);
                assert_eq!(pat, Pattern::Var("$x".into()));
                assert_eq!(*init, Filter::Literal(Value::Int(0)));
            }
            other => panic!("expected Reduce, got {other:?}"),
        }
    }

    // --- Phase 2: foreach ---

    #[test]
    fn parse_foreach_two_arg() {
        let f = p("foreach .[] as $x (0; . + $x)");
        match f {
            Filter::Foreach(_, pat, _, _, extract) => {
                assert_eq!(pat, Pattern::Var("$x".into()));
                assert!(extract.is_none());
            }
            other => panic!("expected Foreach, got {other:?}"),
        }
    }

    #[test]
    fn parse_foreach_three_arg() {
        let f = p("foreach .[] as $x (0; . + $x; . * 2)");
        match f {
            Filter::Foreach(_, pat, _, _, extract) => {
                assert_eq!(pat, Pattern::Var("$x".into()));
                assert!(extract.is_some());
            }
            other => panic!("expected Foreach with extract, got {other:?}"),
        }
    }

    // --- Assignment operators ---

    #[test]
    fn parse_update_assign() {
        // `.foo |= . + 1`
        let f = p(".foo |= . + 1");
        match f {
            Filter::Assign(lhs, op, _rhs) => {
                assert_eq!(*lhs, Filter::Field("foo".into()));
                assert_eq!(op, AssignOp::Update);
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_plain_assign() {
        let f = p(".a = 42");
        match f {
            Filter::Assign(lhs, op, rhs) => {
                assert_eq!(*lhs, Filter::Field("a".into()));
                assert_eq!(op, AssignOp::Set);
                assert_eq!(*rhs, Filter::Literal(Value::Int(42)));
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_plus_assign() {
        let f = p(".[] += 2");
        match f {
            Filter::Assign(lhs, op, rhs) => {
                assert_eq!(*lhs, Filter::Iterate);
                assert_eq!(op, AssignOp::Add);
                assert_eq!(*rhs, Filter::Literal(Value::Int(2)));
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_minus_assign() {
        let f = p(".x -= 1");
        match f {
            Filter::Assign(_, op, _) => assert_eq!(op, AssignOp::Sub),
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_star_assign() {
        let f = p(".x *= 2");
        match f {
            Filter::Assign(_, op, _) => assert_eq!(op, AssignOp::Mul),
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_slash_assign() {
        let f = p(".x /= 2");
        match f {
            Filter::Assign(_, op, _) => assert_eq!(op, AssignOp::Div),
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_percent_assign() {
        let f = p(".x %= 3");
        match f {
            Filter::Assign(_, op, _) => assert_eq!(op, AssignOp::Mod),
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_alt_assign() {
        let f = p(r#".a //= "default""#);
        match f {
            Filter::Assign(_, op, _) => assert_eq!(op, AssignOp::Alt),
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parse_assign_in_pipe() {
        // `.[] | .foo |= . + 1` → Pipe(Iterate, Assign(Field("foo"), Update, ...))
        let f = p(".[] | .foo |= . + 1");
        match f {
            Filter::Pipe(lhs, rhs) => {
                assert_eq!(*lhs, Filter::Iterate);
                match *rhs {
                    Filter::Assign(_, op, _) => assert_eq!(op, AssignOp::Update),
                    other => panic!("expected Assign in pipe, got {other:?}"),
                }
            }
            other => panic!("expected Pipe, got {other:?}"),
        }
    }

    #[test]
    fn parse_assign_right_recursive() {
        // `.a = .b = 1` should be right-recursive: `.a = (.b = 1)`
        let f = p(".a = .b = 1");
        match f {
            Filter::Assign(_, AssignOp::Set, rhs) => match *rhs {
                Filter::Assign(_, AssignOp::Set, _) => {}
                other => panic!("expected nested Assign, got {other:?}"),
            },
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    // --- String interpolation ---

    #[test]
    fn parse_string_interp() {
        let f = p(r#""\(.x)""#);
        match f {
            Filter::StringInterp(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    StringPart::Expr(Filter::Field(name)) => assert_eq!(name, "x"),
                    other => panic!("expected Expr(Field), got {other:?}"),
                }
            }
            other => panic!("expected StringInterp, got {other:?}"),
        }
    }

    #[test]
    fn parse_string_interp_with_lit() {
        let f = p(r#""hello \(.name)!""#);
        match f {
            Filter::StringInterp(parts) => {
                assert_eq!(parts.len(), 3);
                assert_eq!(parts[0], StringPart::Lit("hello ".into()));
                match &parts[1] {
                    StringPart::Expr(Filter::Field(name)) => assert_eq!(name, "name"),
                    other => panic!("expected Expr(Field), got {other:?}"),
                }
                assert_eq!(parts[2], StringPart::Lit("!".into()));
            }
            other => panic!("expected StringInterp, got {other:?}"),
        }
    }

    #[test]
    fn parse_string_interp_arithmetic() {
        // "\(.x + 1)" should parse the expression inside
        let f = p(r#""\(.x + 1)""#);
        match f {
            Filter::StringInterp(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    StringPart::Expr(Filter::Arith(_, ArithOp::Add, _)) => {}
                    other => panic!("expected Expr(Arith(Add)), got {other:?}"),
                }
            }
            other => panic!("expected StringInterp, got {other:?}"),
        }
    }

    // --- Format strings ---

    #[test]
    fn parse_format_string() {
        assert_eq!(p("@base64"), Filter::Builtin("@base64".into(), vec![]));
    }

    #[test]
    fn parse_format_in_pipe() {
        let f = p(". | @csv");
        match f {
            Filter::Pipe(lhs, rhs) => {
                assert_eq!(*lhs, Filter::Identity);
                assert_eq!(*rhs, Filter::Builtin("@csv".into(), vec![]));
            }
            other => panic!("expected Pipe, got {other:?}"),
        }
    }
}
