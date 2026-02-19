/// jq filter language tokenizer.
use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Dot,       // .
    Pipe,      // |
    LBrack,    // [
    RBrack,    // ]
    LBrace,    // {
    RBrace,    // }
    LParen,    // (
    RParen,    // )
    Comma,     // ,
    Colon,     // :
    Semicolon, // ;
    Question,  // ?
    // Comparison operators
    Eq, // ==
    Ne, // !=
    Lt, // <
    Le, // <=
    Gt, // >
    Ge, // >=
    // Arithmetic
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %
    // Literals and identifiers
    Ident(String),
    Str(String),
    InterpStr(Vec<StringSegment>), // "text \(expr) text"
    Format(String),                // @base64, @csv, etc.
    Int(i64),
    Float(f64),
    // Keywords
    True,
    False,
    Null,
    If,
    Then,
    Else,
    End,
    And,
    Or,
    Not,
    As,
    Elif,
    Try,
    Catch,
    Reduce,
    Foreach,
    Select,
    Def,
    Label,
    Break,
    // Module system
    Import,
    Include,
    Module,
    DoubleColon, // :: (namespace separator)
    // Logical
    DoubleSlash,         // // (alternative operator)
    QuestionDoubleSlash, // ?// (alternative match operator)
    // Assignment operators
    Assign,        // =
    UpdateAssign,  // |=
    PlusAssign,    // +=
    MinusAssign,   // -=
    StarAssign,    // *=
    SlashAssign,   // /=
    PercentAssign, // %=
    AltAssign,     // //=
}

#[derive(Debug, Clone, PartialEq)]
pub enum StringSegment {
    Lit(String),
    Expr(String),
}

pub fn lex(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Skip whitespace
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Single-char tokens
        match bytes[i] {
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
                continue;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
                continue;
            }
            b'[' => {
                tokens.push(Token::LBrack);
                i += 1;
                continue;
            }
            b']' => {
                tokens.push(Token::RBrack);
                i += 1;
                continue;
            }
            b'{' => {
                tokens.push(Token::LBrace);
                i += 1;
                continue;
            }
            b'}' => {
                tokens.push(Token::RBrace);
                i += 1;
                continue;
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::UpdateAssign);
                    i += 2;
                } else {
                    tokens.push(Token::Pipe);
                    i += 1;
                }
                continue;
            }
            b',' => {
                tokens.push(Token::Comma);
                i += 1;
                continue;
            }
            b':' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b':' {
                    tokens.push(Token::DoubleColon);
                    i += 2;
                } else {
                    tokens.push(Token::Colon);
                    i += 1;
                }
                continue;
            }
            b';' => {
                tokens.push(Token::Semicolon);
                i += 1;
                continue;
            }
            b'?' => {
                // Check for ?// (alternative match operator)
                if i + 2 < bytes.len() && bytes[i + 1] == b'/' && bytes[i + 2] == b'/' {
                    tokens.push(Token::QuestionDoubleSlash);
                    i += 3;
                } else {
                    tokens.push(Token::Question);
                    i += 1;
                }
                continue;
            }
            b'+' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::PlusAssign);
                    i += 2;
                } else {
                    tokens.push(Token::Plus);
                    i += 1;
                }
                continue;
            }
            b'-' => {
                // Could be negative number or minus operator.
                // It's a negative number if followed by a digit and the previous token
                // is not a value-producing token.
                if i + 1 < bytes.len()
                    && bytes[i + 1].is_ascii_digit()
                    && !is_value_token(tokens.last())
                {
                    let (tok, consumed) = lex_number(bytes, i)?;
                    tokens.push(tok);
                    i += consumed;
                    continue;
                }
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::MinusAssign);
                    i += 2;
                } else {
                    tokens.push(Token::Minus);
                    i += 1;
                }
                continue;
            }
            b'*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::StarAssign);
                    i += 2;
                } else {
                    tokens.push(Token::Star);
                    i += 1;
                }
                continue;
            }
            b'%' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::PercentAssign);
                    i += 2;
                } else {
                    tokens.push(Token::Percent);
                    i += 1;
                }
                continue;
            }
            _ => {}
        }

        // Three-char operators (must check before two-char)
        if i + 2 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' && bytes[i + 2] == b'=' {
            tokens.push(Token::AltAssign);
            i += 3;
            continue;
        }

        // Two-char operators
        if i + 1 < bytes.len() {
            match (bytes[i], bytes[i + 1]) {
                (b'=', b'=') => {
                    tokens.push(Token::Eq);
                    i += 2;
                    continue;
                }
                (b'!', b'=') => {
                    tokens.push(Token::Ne);
                    i += 2;
                    continue;
                }
                (b'<', b'=') => {
                    tokens.push(Token::Le);
                    i += 2;
                    continue;
                }
                (b'>', b'=') => {
                    tokens.push(Token::Ge);
                    i += 2;
                    continue;
                }
                (b'/', b'/') => {
                    tokens.push(Token::DoubleSlash);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }

        // Single < > / = (must come after two-char checks)
        match bytes[i] {
            b'<' => {
                tokens.push(Token::Lt);
                i += 1;
                continue;
            }
            b'>' => {
                tokens.push(Token::Gt);
                i += 1;
                continue;
            }
            b'/' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::SlashAssign);
                    i += 2;
                } else {
                    tokens.push(Token::Slash);
                    i += 1;
                }
                continue;
            }
            b'=' => {
                tokens.push(Token::Assign);
                i += 1;
                continue;
            }
            _ => {}
        }

        // Dot
        if bytes[i] == b'.' {
            // Check if it's a number like .5
            if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                let (tok, consumed) = lex_number(bytes, i)?;
                tokens.push(tok);
                i += consumed;
                continue;
            }
            tokens.push(Token::Dot);
            i += 1;
            continue;
        }

        // String literal
        if bytes[i] == b'"' {
            match lex_string(bytes, i)? {
                LexStringResult::Plain(s, consumed) => {
                    tokens.push(Token::Str(s));
                    i += consumed;
                }
                LexStringResult::Interp(segments, consumed) => {
                    tokens.push(Token::InterpStr(segments));
                    i += consumed;
                }
            }
            continue;
        }

        // Number
        if bytes[i].is_ascii_digit() {
            let (tok, consumed) = lex_number(bytes, i)?;
            tokens.push(tok);
            i += consumed;
            continue;
        }

        // Identifier or keyword
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' || bytes[i] == b'$' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            let word = &input[start..i];
            // $keyword variables: always produce Ident so they can be used as variable names
            let tok = if word.starts_with('$') {
                Token::Ident(word.to_string())
            } else {
                match word {
                    "true" => Token::True,
                    "false" => Token::False,
                    "null" => Token::Null,
                    "if" => Token::If,
                    "then" => Token::Then,
                    "elif" => Token::Elif,
                    "else" => Token::Else,
                    "end" => Token::End,
                    "and" => Token::And,
                    "or" => Token::Or,
                    "not" => Token::Not,
                    "as" => Token::As,
                    "try" => Token::Try,
                    "catch" => Token::Catch,
                    "reduce" => Token::Reduce,
                    "foreach" => Token::Foreach,
                    "select" => Token::Select,
                    "def" => Token::Def,
                    "import" => Token::Import,
                    "include" => Token::Include,
                    "module" => Token::Module,
                    "label" => Token::Label,
                    "break" => Token::Break,
                    _ => Token::Ident(word.to_string()),
                }
            };
            tokens.push(tok);
            continue;
        }

        // Format string: @name
        if bytes[i] == b'@' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name = &input[start..i];
            tokens.push(Token::Format(name.to_string()));
            continue;
        }

        bail!(
            "unexpected character '{}' at position {i}",
            bytes[i] as char
        );
    }

    Ok(tokens)
}

/// Returns true if the token is a value-producing token (for minus disambiguation).
fn is_value_token(tok: Option<&Token>) -> bool {
    matches!(
        tok,
        Some(
            Token::RParen
                | Token::RBrack
                | Token::RBrace
                | Token::Ident(_)
                | Token::Int(_)
                | Token::Float(_)
                | Token::Str(_)
                | Token::InterpStr(_)
                | Token::True
                | Token::False
                | Token::Null
                | Token::Dot
        )
    )
}

enum LexStringResult {
    Plain(String, usize),
    Interp(Vec<StringSegment>, usize),
}

fn lex_string(bytes: &[u8], start: usize) -> Result<LexStringResult> {
    debug_assert_eq!(bytes[start], b'"');
    let mut i = start + 1;
    let mut s = String::new();
    let mut segments: Vec<StringSegment> = Vec::new();
    let mut has_interp = false;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let consumed = i + 1 - start;
                if has_interp {
                    if !s.is_empty() {
                        segments.push(StringSegment::Lit(s));
                    }
                    return Ok(LexStringResult::Interp(segments, consumed));
                }
                return Ok(LexStringResult::Plain(s, consumed));
            }
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    bail!("unterminated string escape");
                }
                match bytes[i] {
                    b'(' => {
                        // String interpolation: \(expr)
                        has_interp = true;
                        if !s.is_empty() {
                            segments.push(StringSegment::Lit(std::mem::take(&mut s)));
                        }
                        i += 1;
                        // Scan forward to find matching ), tracking nesting
                        let expr_start = i;
                        let mut depth = 1u32;
                        let mut in_str = false;
                        while i < bytes.len() && depth > 0 {
                            if in_str {
                                if bytes[i] == b'\\' {
                                    i += 1; // skip next char in string
                                } else if bytes[i] == b'"' {
                                    in_str = false;
                                }
                            } else {
                                match bytes[i] {
                                    b'(' => depth += 1,
                                    b')' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            break;
                                        }
                                    }
                                    b'"' => in_str = true,
                                    _ => {}
                                }
                            }
                            i += 1;
                        }
                        if depth != 0 {
                            bail!("unterminated string interpolation");
                        }
                        let expr_text = std::str::from_utf8(&bytes[expr_start..i])?;
                        segments.push(StringSegment::Expr(expr_text.to_string()));
                        i += 1; // skip closing )
                    }
                    b'"' => {
                        s.push('"');
                        i += 1;
                    }
                    b'\\' => {
                        s.push('\\');
                        i += 1;
                    }
                    b'/' => {
                        s.push('/');
                        i += 1;
                    }
                    b'n' => {
                        s.push('\n');
                        i += 1;
                    }
                    b'r' => {
                        s.push('\r');
                        i += 1;
                    }
                    b't' => {
                        s.push('\t');
                        i += 1;
                    }
                    b'b' => {
                        s.push('\x08');
                        i += 1;
                    }
                    b'f' => {
                        s.push('\x0c');
                        i += 1;
                    }
                    b'u' => {
                        // \uXXXX
                        if i + 4 >= bytes.len() {
                            bail!("incomplete \\u escape");
                        }
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 5])?;
                        let cp = u16::from_str_radix(hex, 16)?;
                        if let Some(c) = char::from_u32(cp as u32) {
                            s.push(c);
                        }
                        i += 5;
                    }
                    c => bail!("unknown escape '\\{}'", c as char),
                }
            }
            _ => {
                // Fast path: scan for next special char
                let chunk_start = i;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\\' {
                    i += 1;
                }
                s.push_str(std::str::from_utf8(&bytes[chunk_start..i])?);
            }
        }
    }
    bail!("unterminated string starting at position {start}");
}

fn lex_number(bytes: &[u8], start: usize) -> Result<(Token, usize)> {
    let mut i = start;
    let mut is_float = false;

    // Optional minus
    if i < bytes.len() && bytes[i] == b'-' {
        i += 1;
    }

    // Leading dot (e.g., .5) means it's a float
    if i < bytes.len() && bytes[i] == b'.' {
        is_float = true;
        i += 1;
    }

    // Integer part
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }

    // Decimal point (if not already seen)
    if !is_float && i < bytes.len() && bytes[i] == b'.' {
        // Make sure this isn't a field access like 1.field
        if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            is_float = true;
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }

    // Exponent
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        is_float = true;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }

    let text = std::str::from_utf8(&bytes[start..i])?;
    let consumed = i - start;

    if is_float {
        let f: f64 = text.parse()?;
        Ok((Token::Float(f), consumed))
    } else {
        let n: i64 = text.parse()?;
        Ok((Token::Int(n), consumed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_identity() {
        assert_eq!(lex(".").unwrap(), vec![Token::Dot]);
    }

    #[test]
    fn lex_field_access() {
        assert_eq!(
            lex(".foo").unwrap(),
            vec![Token::Dot, Token::Ident("foo".into())]
        );
    }

    #[test]
    fn lex_nested_field() {
        assert_eq!(
            lex(".a.b.c").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("a".into()),
                Token::Dot,
                Token::Ident("b".into()),
                Token::Dot,
                Token::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn lex_pipe() {
        assert_eq!(
            lex(".[] | .name").unwrap(),
            vec![
                Token::Dot,
                Token::LBrack,
                Token::RBrack,
                Token::Pipe,
                Token::Dot,
                Token::Ident("name".into()),
            ]
        );
    }

    #[test]
    fn lex_select() {
        assert_eq!(
            lex("select(.age > 30)").unwrap(),
            vec![
                Token::Select,
                Token::LParen,
                Token::Dot,
                Token::Ident("age".into()),
                Token::Gt,
                Token::Int(30),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn lex_object_construct() {
        assert_eq!(
            lex("{name: .name, id: .id}").unwrap(),
            vec![
                Token::LBrace,
                Token::Ident("name".into()),
                Token::Colon,
                Token::Dot,
                Token::Ident("name".into()),
                Token::Comma,
                Token::Ident("id".into()),
                Token::Colon,
                Token::Dot,
                Token::Ident("id".into()),
                Token::RBrace,
            ]
        );
    }

    #[test]
    fn lex_string_literal() {
        assert_eq!(lex(r#""hello""#).unwrap(), vec![Token::Str("hello".into())]);
    }

    #[test]
    fn lex_comparison_operators() {
        assert_eq!(
            lex(".x == .y").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::Eq,
                Token::Dot,
                Token::Ident("y".into()),
            ]
        );
        assert_eq!(
            lex(".x != .y").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::Ne,
                Token::Dot,
                Token::Ident("y".into()),
            ]
        );
    }

    #[test]
    fn lex_negative_number() {
        assert_eq!(lex("-42").unwrap(), vec![Token::Int(-42)]);
    }

    #[test]
    fn lex_float() {
        assert_eq!(lex("3.14").unwrap(), vec![Token::Float(3.14)]);
    }

    #[test]
    fn lex_array_index() {
        assert_eq!(
            lex(".[0]").unwrap(),
            vec![Token::Dot, Token::LBrack, Token::Int(0), Token::RBrack]
        );
    }

    #[test]
    fn lex_arithmetic() {
        assert_eq!(
            lex(".x + 1").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::Plus,
                Token::Int(1),
            ]
        );
    }

    #[test]
    fn lex_alternative_operator() {
        assert_eq!(
            lex(".x // null").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::DoubleSlash,
                Token::Null,
            ]
        );
    }

    #[test]
    fn lex_keywords() {
        assert_eq!(lex("true").unwrap(), vec![Token::True]);
        assert_eq!(lex("false").unwrap(), vec![Token::False]);
        assert_eq!(lex("null").unwrap(), vec![Token::Null]);
        assert_eq!(
            lex("if . then 1 else 2 end").unwrap(),
            vec![
                Token::If,
                Token::Dot,
                Token::Then,
                Token::Int(1),
                Token::Else,
                Token::Int(2),
                Token::End,
            ]
        );
    }

    #[test]
    fn lex_array_construct() {
        assert_eq!(
            lex("[.[] | .x]").unwrap(),
            vec![
                Token::LBrack,
                Token::Dot,
                Token::LBrack,
                Token::RBrack,
                Token::Pipe,
                Token::Dot,
                Token::Ident("x".into()),
                Token::RBrack,
            ]
        );
    }

    #[test]
    fn lex_subtraction_vs_negative() {
        // "1 - 2" should be Int(1), Minus, Int(2), not Int(1), Int(-2)
        assert_eq!(
            lex("1 - 2").unwrap(),
            vec![Token::Int(1), Token::Minus, Token::Int(2)]
        );
    }

    // --- Assignment operators ---

    #[test]
    fn lex_update_assign() {
        assert_eq!(
            lex(".foo |= . + 1").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("foo".into()),
                Token::UpdateAssign,
                Token::Dot,
                Token::Plus,
                Token::Int(1),
            ]
        );
    }

    #[test]
    fn lex_plain_assign() {
        assert_eq!(
            lex(".a = 42").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("a".into()),
                Token::Assign,
                Token::Int(42),
            ]
        );
    }

    #[test]
    fn lex_plus_assign() {
        assert_eq!(
            lex(".[] += 2").unwrap(),
            vec![
                Token::Dot,
                Token::LBrack,
                Token::RBrack,
                Token::PlusAssign,
                Token::Int(2),
            ]
        );
    }

    #[test]
    fn lex_minus_assign() {
        assert_eq!(
            lex(".x -= 1").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::MinusAssign,
                Token::Int(1),
            ]
        );
    }

    #[test]
    fn lex_star_assign() {
        assert_eq!(
            lex(".x *= 2").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::StarAssign,
                Token::Int(2),
            ]
        );
    }

    #[test]
    fn lex_slash_assign() {
        assert_eq!(
            lex(".x /= 2").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::SlashAssign,
                Token::Int(2),
            ]
        );
    }

    #[test]
    fn lex_percent_assign() {
        assert_eq!(
            lex(".x %= 3").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::PercentAssign,
                Token::Int(3),
            ]
        );
    }

    #[test]
    fn lex_alt_assign() {
        // //= is 3 chars — must not be confused with // followed by =
        assert_eq!(
            lex(r#".a //= "default""#).unwrap(),
            vec![
                Token::Dot,
                Token::Ident("a".into()),
                Token::AltAssign,
                Token::Str("default".into()),
            ]
        );
    }

    #[test]
    fn lex_double_slash_vs_alt_assign() {
        // ".x // null" should produce DoubleSlash, not AltAssign
        assert_eq!(
            lex(".x // null").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::DoubleSlash,
                Token::Null,
            ]
        );
    }

    #[test]
    fn lex_eq_vs_assign() {
        // == must still work
        assert_eq!(
            lex(".x == 1").unwrap(),
            vec![
                Token::Dot,
                Token::Ident("x".into()),
                Token::Eq,
                Token::Int(1),
            ]
        );
    }

    // --- String interpolation ---

    #[test]
    fn lex_string_interp_basic() {
        let tokens = lex(r#""\(.x)""#).unwrap();
        assert_eq!(
            tokens,
            vec![Token::InterpStr(vec![StringSegment::Expr(".x".into()),])]
        );
    }

    #[test]
    fn lex_string_interp_with_text() {
        let tokens = lex(r#""hello \(.name)!""#).unwrap();
        assert_eq!(
            tokens,
            vec![Token::InterpStr(vec![
                StringSegment::Lit("hello ".into()),
                StringSegment::Expr(".name".into()),
                StringSegment::Lit("!".into()),
            ])]
        );
    }

    #[test]
    fn lex_string_interp_multiple() {
        let tokens = lex(r#""\(.a) and \(.b)""#).unwrap();
        assert_eq!(
            tokens,
            vec![Token::InterpStr(vec![
                StringSegment::Expr(".a".into()),
                StringSegment::Lit(" and ".into()),
                StringSegment::Expr(".b".into()),
            ])]
        );
    }

    #[test]
    fn lex_string_interp_nested_parens() {
        // Expression with nested parens: \((.x + 1))
        let tokens = lex(r#""\((.x + 1))""#).unwrap();
        assert_eq!(
            tokens,
            vec![Token::InterpStr(vec![StringSegment::Expr(
                "(.x + 1)".into()
            ),])]
        );
    }

    #[test]
    fn lex_plain_string_unchanged() {
        // No interpolation — should still be a plain Str token
        assert_eq!(lex(r#""hello""#).unwrap(), vec![Token::Str("hello".into())]);
    }

    // --- Format tokens ---

    #[test]
    fn lex_format_base64() {
        assert_eq!(
            lex("@base64").unwrap(),
            vec![Token::Format("@base64".into())]
        );
    }

    #[test]
    fn lex_format_csv() {
        assert_eq!(lex("@csv").unwrap(), vec![Token::Format("@csv".into())]);
    }

    #[test]
    fn lex_format_in_pipe() {
        assert_eq!(
            lex(". | @html").unwrap(),
            vec![Token::Dot, Token::Pipe, Token::Format("@html".into())]
        );
    }
}
