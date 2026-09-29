//! Port of jq 1.8.1's `lexer.l` (a flex scanner) to a hand-written scanner.
//!
//! The rules and their order are flex's: at each position the longest match wins, and
//! ties go to the rule listed first (so `if` is a keyword but `if::x` and `ifx` are
//! identifiers, and `$__loc__` beats the `BINDING` rule). Start conditions are kept
//! on a stack like flex's `yy_push_state`: `(`, `[`, `{` and `\(` push a state that
//! the matching closer pops. A closer that doesn't match the innermost opener is an
//! `INVALID_CHARACTER`, and so is any closer at the top level.
//!
//! Every matched lexeme, including whitespace and comments, updates the token
//! location (flex's `YY_USER_ACTION`). End of input does not, so the end-of-file
//! token keeps the location of the last lexeme, which is where jq's "unexpected end
//! of file" errors point.
//!
//! String escapes are decoded the way jq does it: runs of escapes are handed to
//! the JSON parser (`jv_parse_sized`), and its error messages are reported verbatim,
//! e.g. `Invalid escape at line 1, column 4 (while parsing '"\x"')`.

use std::borrow::Cow;

use super::ast::Loc;
use super::parser_tables::{YYTNAME, sym};

/// flex start conditions (`%s` inclusive, `%x` exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Initial,
    InParen,
    InBracket,
    InBrace,
    InQQInterp,
    /// `%x`: only the string rules apply.
    InQQString,
    /// `%x`: only the comment rules apply.
    InComment,
}

/// A token returned by the scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Bison symbol kind (`parser_tables::sym`); `sym::YYEOF` at end of input.
    pub sym: u8,
    /// Semantic value: the name for `IDENT`/`FIELD`/`BINDING`/`FORMAT` (without the `.`,
    /// `$` or `@`), the source text for `LITERAL`, the unescaped text for
    /// `QQSTRING_TEXT`.
    pub text: Option<String>,
    /// For `QQSTRING_TEXT`: the message jq's `yylex` wrapper reports when the escapes
    /// don't parse (`FAIL(*yylloc, msg)`); the text is then `""` (jq: `jv_null()`).
    pub error: Option<String>,
}

impl Token {
    fn simple(sym: u8) -> Token {
        Token {
            sym,
            text: None,
            error: None,
        }
    }

    fn with_text(sym: u8, text: String) -> Token {
        Token {
            sym,
            text: Some(text),
            error: None,
        }
    }

    /// The token's name as bison prints it in syntax errors (`IDENT`, `'|'`, `end`, ...).
    pub fn name(&self) -> &'static str {
        YYTNAME[self.sym as usize]
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    /// flex's `extra`: byte offset of the next lexeme.
    pos: usize,
    state: State,
    stack: Vec<State>,
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

#[inline]
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn keyword(ident: &[u8]) -> Option<u8> {
    Some(match ident {
        b"as" => sym::AS,
        b"import" => sym::IMPORT,
        b"include" => sym::INCLUDE,
        b"module" => sym::MODULE,
        b"def" => sym::DEF,
        b"if" => sym::IF,
        b"then" => sym::THEN,
        b"else" => sym::ELSE,
        b"elif" => sym::ELSE_IF,
        b"and" => sym::AND,
        b"or" => sym::OR,
        b"end" => sym::END,
        b"reduce" => sym::REDUCE,
        b"foreach" => sym::FOREACH,
        b"try" => sym::TRY,
        b"catch" => sym::CATCH,
        b"label" => sym::LABEL,
        b"break" => sym::BREAK,
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8]) -> Lexer<'a> {
        Lexer {
            src,
            pos: 0,
            state: State::Initial,
            stack: Vec::new(),
        }
    }

    #[inline]
    fn at(&self, i: usize) -> u8 {
        self.src.get(i).copied().unwrap_or(0)
    }

    #[inline]
    fn has(&self, i: usize) -> bool {
        i < self.src.len()
    }

    fn push_state(&mut self, s: State) {
        self.stack.push(self.state);
        self.state = s;
    }

    fn pop_state(&mut self) {
        // flex aborts on underflow; every pop in lexer.l follows a matching push.
        self.state = self.stack.pop().unwrap_or(State::Initial);
    }

    /// Consume a lexeme of `len` bytes, updating the location (`YY_USER_ACTION`).
    #[inline]
    fn advance(&mut self, len: usize, lloc: &mut Loc) -> usize {
        let start = self.pos;
        self.pos += len;
        *lloc = Loc::new(start as u32, self.pos as u32);
        start
    }

    /// `([a-zA-Z_][a-zA-Z_0-9]*::)*[a-zA-Z_][a-zA-Z_0-9]*` at `p`: length of the longest
    /// match, or 0.
    fn match_ident_path(&self, p: usize) -> usize {
        if !is_ident_start(self.at(p)) {
            return 0;
        }
        let mut q = p;
        loop {
            q += 1;
            while is_ident_char(self.at(q)) {
                q += 1;
            }
            if self.at(q) == b':' && self.at(q + 1) == b':' && is_ident_start(self.at(q + 2)) {
                q += 2;
            } else {
                return q - p;
            }
        }
    }

    /// `([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?` at `p` (known to match).
    fn match_number(&self, p: usize) -> usize {
        let mut q = p;
        if self.at(q).is_ascii_digit() {
            while self.at(q).is_ascii_digit() {
                q += 1;
            }
            if self.at(q) == b'.' {
                q += 1;
                while self.at(q).is_ascii_digit() {
                    q += 1;
                }
            }
        } else {
            q += 1; // '.', followed by at least one digit
            while self.at(q).is_ascii_digit() {
                q += 1;
            }
        }
        if matches!(self.at(q), b'e' | b'E') {
            let mut r = q + 1;
            if matches!(self.at(r), b'+' | b'-') {
                r += 1;
            }
            if self.at(r).is_ascii_digit() {
                while self.at(r).is_ascii_digit() {
                    r += 1;
                }
                q = r;
            }
        }
        q - p
    }

    /// `yylex`: the next token. `lloc` is the parser's `yylloc`, updated for every
    /// lexeme (including skipped ones) but not at end of input.
    pub fn next_token(&mut self, lloc: &mut Loc) -> Token {
        loop {
            if self.pos >= self.src.len() {
                // Every state ends with the end-of-file token and leaves the location
                // alone (EOF rules don't run YY_USER_ACTION; IN_COMMENT's just pops
                // first, which makes no difference here).
                return Token::simple(sym::YYEOF);
            }
            match self.state {
                State::InComment => self.skip_comment(lloc),
                State::InQQString => return self.lex_qqstring(lloc),
                _ => {
                    if let Some(tok) = self.lex_normal(lloc) {
                        return tok;
                    }
                }
            }
        }
    }

    /// `<IN_COMMENT>`: `\\(\\|\r?\n)|.` continues the comment, `\r?\n` ends it.
    fn skip_comment(&mut self, lloc: &mut Loc) {
        let n = self.src.len();
        let mut p = self.pos;
        let mut last = (p, p);
        while p < n {
            let c = self.src[p];
            let (len, end) = match c {
                b'\\' => {
                    let c1 = self.at(p + 1);
                    if self.has(p + 1) && (c1 == b'\\' || c1 == b'\n') {
                        (2, false)
                    } else if self.has(p + 2) && c1 == b'\r' && self.at(p + 2) == b'\n' {
                        (3, false)
                    } else {
                        (1, false)
                    }
                }
                b'\r' if self.has(p + 1) && self.at(p + 1) == b'\n' => (2, true),
                b'\n' => (1, true),
                _ => (1, false),
            };
            last = (p, p + len);
            p += len;
            if end {
                self.pop_state();
                break;
            }
        }
        if last.1 > last.0 {
            *lloc = Loc::new(last.0 as u32, last.1 as u32);
        }
        self.pos = p;
        if p >= n && self.state == State::InComment {
            self.pop_state();
        }
    }

    /// Rules active in INITIAL and the inclusive states. Returns `None` for lexemes
    /// that produce no token (whitespace, `#`).
    fn lex_normal(&mut self, lloc: &mut Loc) -> Option<Token> {
        let p = self.pos;
        let c = self.src[p];
        let c1 = self.at(p + 1);
        let tok = |s| Some(Token::simple(s));
        match c {
            b'#' => {
                self.advance(1, lloc);
                self.push_state(State::InComment);
                None
            }
            b' ' | b'\t' | b'\r' | b'\n' => {
                let mut q = p + 1;
                while matches!(self.at(q), b' ' | b'\t' | b'\r' | b'\n') && self.has(q) {
                    q += 1;
                }
                self.advance(q - p, lloc);
                None
            }
            b'!' if c1 == b'=' => {
                self.advance(2, lloc);
                tok(sym::NEQ)
            }
            b'=' => {
                if c1 == b'=' {
                    self.advance(2, lloc);
                    tok(sym::EQ)
                } else {
                    self.advance(1, lloc);
                    tok(sym::ASSIGN)
                }
            }
            b'/' => {
                if c1 == b'/' && self.at(p + 2) == b'=' {
                    self.advance(3, lloc);
                    tok(sym::SETDEFINEDOR)
                } else if c1 == b'/' {
                    self.advance(2, lloc);
                    tok(sym::DEFINEDOR)
                } else if c1 == b'=' {
                    self.advance(2, lloc);
                    tok(sym::SETDIV)
                } else {
                    self.advance(1, lloc);
                    tok(sym::SLASH)
                }
            }
            b'|' | b'+' | b'-' | b'*' | b'%' | b'<' | b'>' => {
                let (single, with_eq) = match c {
                    b'|' => (sym::PIPE, sym::SETPIPE),
                    b'+' => (sym::PLUS, sym::SETPLUS),
                    b'-' => (sym::MINUS, sym::SETMINUS),
                    b'*' => (sym::STAR, sym::SETMULT),
                    b'%' => (sym::PERCENT, sym::SETMOD),
                    b'<' => (sym::LESS, sym::LESSEQ),
                    _ => (sym::GREATER, sym::GREATEREQ),
                };
                if c1 == b'=' {
                    self.advance(2, lloc);
                    tok(with_eq)
                } else {
                    self.advance(1, lloc);
                    tok(single)
                }
            }
            b'?' => {
                if c1 == b'/' && self.at(p + 2) == b'/' {
                    self.advance(3, lloc);
                    tok(sym::ALTERNATION)
                } else {
                    self.advance(1, lloc);
                    tok(sym::QUESTION)
                }
            }
            b';' => {
                self.advance(1, lloc);
                tok(sym::SEMICOLON)
            }
            b',' => {
                self.advance(1, lloc);
                tok(sym::COMMA)
            }
            b':' => {
                self.advance(1, lloc);
                tok(sym::COLON)
            }
            b'.' => {
                if c1 == b'.' {
                    self.advance(2, lloc);
                    tok(sym::REC)
                } else if is_ident_start(c1) {
                    let mut q = p + 2;
                    while is_ident_char(self.at(q)) {
                        q += 1;
                    }
                    self.advance(q - p, lloc);
                    let name = ascii(&self.src[p + 1..q]);
                    Some(Token::with_text(sym::FIELD, name))
                } else if c1.is_ascii_digit() && self.has(p + 1) {
                    let len = self.match_number(p);
                    self.advance(len, lloc);
                    Some(Token::with_text(sym::LITERAL, ascii(&self.src[p..p + len])))
                } else {
                    self.advance(1, lloc);
                    tok(sym::DOT)
                }
            }
            b'$' => {
                let blen = self.match_ident_path(p + 1);
                if blen == 7 && &self.src[p + 1..p + 8] == b"__loc__" {
                    // "$__loc__" and BINDING match the same length; the earlier rule wins.
                    self.advance(8, lloc);
                    tok(sym::LOC)
                } else if blen > 0 {
                    self.advance(blen + 1, lloc);
                    let name = ascii(&self.src[p + 1..p + 1 + blen]);
                    Some(Token::with_text(sym::BINDING, name))
                } else {
                    self.advance(1, lloc);
                    tok(sym::DOLLAR)
                }
            }
            b'[' | b'{' | b'(' => {
                self.advance(1, lloc);
                let (state, s) = match c {
                    b'(' => (State::InParen, sym::LPAREN),
                    b'[' => (State::InBracket, sym::LBRACKET),
                    _ => (State::InBrace, sym::LBRACE),
                };
                self.push_state(state);
                tok(s)
            }
            b']' | b'}' | b')' => {
                self.advance(1, lloc);
                tok(self.try_exit(c))
            }
            b'@' if is_ident_char(c1) => {
                let mut q = p + 2;
                while is_ident_char(self.at(q)) {
                    q += 1;
                }
                self.advance(q - p, lloc);
                Some(Token::with_text(sym::FORMAT, ascii(&self.src[p + 1..q])))
            }
            b'0'..=b'9' => {
                let len = self.match_number(p);
                self.advance(len, lloc);
                Some(Token::with_text(sym::LITERAL, ascii(&self.src[p..p + len])))
            }
            b'"' => {
                self.advance(1, lloc);
                self.push_state(State::InQQString);
                tok(sym::QQSTRING_START)
            }
            _ if is_ident_start(c) => {
                let len = self.match_ident_path(p);
                self.advance(len, lloc);
                let text = &self.src[p..p + len];
                match keyword(text) {
                    Some(k) => tok(k),
                    None => Some(Token::with_text(sym::IDENT, ascii(text))),
                }
            }
            _ => {
                self.advance(1, lloc);
                tok(sym::INVALID_CHARACTER)
            }
        }
    }

    /// `try_exit`: a closing bracket must match the innermost open one.
    fn try_exit(&mut self, c: u8) -> u8 {
        let (matching, ret) = match self.state {
            State::InParen => (b')', sym::RPAREN),
            State::InBracket => (b']', sym::RBRACKET),
            State::InBrace => (b'}', sym::RBRACE),
            State::InQQInterp => (b')', sym::QQSTRING_INTERP_END),
            // "may not be the best error to give"
            _ => return sym::INVALID_CHARACTER,
        };
        if c == matching {
            self.pop_state();
            ret
        } else {
            sym::INVALID_CHARACTER
        }
    }

    /// `<IN_QQSTRING>` rules.
    fn lex_qqstring(&mut self, lloc: &mut Loc) -> Token {
        let p = self.pos;
        let n = self.src.len();
        match self.src[p] {
            b'\\' if self.has(p + 1) && self.at(p + 1) == b'(' => {
                self.advance(2, lloc);
                self.push_state(State::InQQInterp);
                Token::simple(sym::QQSTRING_INTERP_START)
            }
            b'\\' if self.has(p + 1) => {
                // (\\[^u(]|\\u[a-zA-Z0-9]{0,4})+
                let mut q = p;
                while q + 1 < n && self.src[q] == b'\\' && self.src[q + 1] != b'(' {
                    if self.src[q + 1] == b'u' {
                        q += 2;
                        let mut k = 0;
                        while k < 4 && self.at(q).is_ascii_alphanumeric() && q < n {
                            q += 1;
                            k += 1;
                        }
                    } else {
                        q += 2;
                    }
                }
                self.advance(q - p, lloc);
                match parse_escapes(&self.src[p..q]) {
                    Ok(s) => Token::with_text(sym::QQSTRING_TEXT, s),
                    Err(msg) => Token {
                        sym: sym::QQSTRING_TEXT,
                        text: Some(String::new()),
                        error: Some(msg),
                    },
                }
            }
            b'\\' => {
                // a lone backslash at the end of the input only matches `.`
                self.advance(1, lloc);
                Token::simple(sym::INVALID_CHARACTER)
            }
            b'"' => {
                self.advance(1, lloc);
                self.pop_state();
                Token::simple(sym::QQSTRING_END)
            }
            _ => {
                let mut q = p + 1;
                while q < n && self.src[q] != b'\\' && self.src[q] != b'"' {
                    q += 1;
                }
                self.advance(q - p, lloc);
                Token::with_text(
                    sym::QQSTRING_TEXT,
                    jv_string_sized(&self.src[p..q]).into_owned(),
                )
            }
        }
    }
}

fn ascii(bytes: &[u8]) -> String {
    // IDENT/FIELD/BINDING/FORMAT/LITERAL texts are ASCII by construction.
    String::from_utf8_lossy(bytes).into_owned()
}

/// Tokenize a whole program (for tests and debugging): `(token, location)` pairs,
/// ending with the end-of-file token.
pub fn tokenize(src: &[u8]) -> Vec<(Token, Loc)> {
    let mut lexer = Lexer::new(src);
    let mut lloc = Loc::default();
    let mut out = Vec::new();
    loop {
        let tok = lexer.next_token(&mut lloc);
        let eof = tok.sym == sym::YYEOF;
        out.push((tok, lloc));
        if eof {
            return out;
        }
    }
}

// ---------------------------------------------------------------------------
// jv string helpers (ports of jv_unicode.c / jv.c / jv_parse.c pieces the lexer and
// error formatting need)
// ---------------------------------------------------------------------------

/// `utf8_coding_length[b]`: sequence length for a lead byte, 0 for invalid bytes,
/// 255 for continuation bytes.
fn utf8_coding_length(b: u8) -> u8 {
    match b {
        0x00..=0x7F => 1,
        0x80..=0xBF => 255,
        0xC0 | 0xC1 => 0,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

fn utf8_coding_bits(b: u8) -> u32 {
    match b {
        0x00..=0x7F => 0x7F,
        0x80..=0xBF => 0x3F,
        0xC2..=0xDF => 0x1F,
        0xE0..=0xEF => 0x0F,
        0xF0..=0xF4 => 0x07,
        _ => 0,
    }
}

const UTF8_FIRST_CODEPOINT: [i32; 5] = [0x00, 0x00, 0x80, 0x800, 0x10000];

/// `jvp_utf8_next`: decode one codepoint, returning `(codepoint or -1, length)`.
fn jvp_utf8_next(input: &[u8]) -> (i32, usize) {
    let first = input[0];
    let mut length = utf8_coding_length(first) as usize;
    if first & 0x80 == 0 {
        return (first as i32, 1);
    }
    if length == 0 || length == 255 {
        return (-1, 1);
    }
    if length > input.len() {
        // String ends before the sequence ends: everything left is one bad char.
        return (-1, input.len());
    }
    let mut codepoint = (first as u32 & utf8_coding_bits(first)) as i32;
    for (i, &ch) in input.iter().enumerate().take(length).skip(1) {
        if utf8_coding_length(ch) != 255 {
            codepoint = -1;
            length = i;
            break;
        }
        codepoint = (codepoint << 6) | (ch as i32 & 0x3f);
    }
    if codepoint < UTF8_FIRST_CODEPOINT[length] {
        codepoint = -1; // overlong
    }
    if (0xD800..=0xDFFF).contains(&codepoint) {
        codepoint = -1; // surrogate
    }
    if codepoint > 0x10FFFF {
        codepoint = -1;
    }
    (codepoint, length)
}

/// `jv_string_sized`: bytes to a jq string, replacing invalid UTF-8 the way jq does
/// (`jvp_string_copy_replace_bad`). This differs from `String::from_utf8_lossy`: an
/// overlong or surrogate sequence becomes a single U+FFFD, and a truncated sequence at
/// the very end swallows the remaining bytes.
pub fn jv_string_sized(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    let mut i = 0;
    while i < bytes.len() {
        let (cp, len) = jvp_utf8_next(&bytes[i..]);
        out.push(if cp < 0 {
            '\u{FFFD}'
        } else {
            char::from_u32(cp as u32).unwrap_or('\u{FFFD}')
        });
        i += len;
    }
    Cow::Owned(out)
}

/// `jvp_utf8_encode`, which (unlike `char`) also encodes lone surrogates.
fn jvp_utf8_encode(codepoint: u32, out: &mut Vec<u8>) {
    if codepoint <= 0x7F {
        out.push(codepoint as u8);
    } else if codepoint <= 0x7FF {
        out.push(0xC0 + ((codepoint & 0x7C0) >> 6) as u8);
        out.push(0x80 + (codepoint & 0x03F) as u8);
    } else if codepoint <= 0xFFFF {
        out.push(0xE0 + ((codepoint & 0xF000) >> 12) as u8);
        out.push(0x80 + ((codepoint & 0x0FC0) >> 6) as u8);
        out.push(0x80 + (codepoint & 0x003F) as u8);
    } else {
        out.push(0xF0 + ((codepoint & 0x1C0000) >> 18) as u8);
        out.push(0x80 + ((codepoint & 0x03F000) >> 12) as u8);
        out.push(0x80 + ((codepoint & 0x000FC0) >> 6) as u8);
        out.push(0x80 + (codepoint & 0x00003F) as u8);
    }
}

/// `unhex4`
fn unhex4(hex: &[u8]) -> i32 {
    let mut r: i32 = 0;
    for &c in &hex[..4] {
        let n = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return -1,
        };
        r = (r << 4) | n as i32;
    }
    r
}

/// `found_string` (jv_parse.c): unescape the body of a JSON string.
fn found_string(token: &[u8]) -> Result<String, &'static str> {
    let mut out = Vec::with_capacity(token.len());
    let end = token.len();
    let mut i = 0;
    while i < end {
        let c = token[i];
        i += 1;
        if c == b'\\' {
            if i >= end {
                return Err("Expected escape character at end of string");
            }
            let c = token[i];
            i += 1;
            match c {
                b'\\' | b'"' | b'/' => out.push(c),
                b'b' => out.push(0x08),
                b'f' => out.push(0x0c),
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b'u' => {
                    if i + 4 > end {
                        return Err("Invalid \\uXXXX escape");
                    }
                    let hexvalue = unhex4(&token[i..]);
                    if hexvalue < 0 {
                        return Err("Invalid characters in \\uXXXX escape");
                    }
                    let mut codepoint = hexvalue as u32;
                    i += 4;
                    if (0xD800..=0xDBFF).contains(&codepoint) {
                        if i + 6 > end || token[i] != b'\\' || token[i + 1] != b'u' {
                            return Err("Invalid \\uXXXX\\uXXXX surrogate pair escape");
                        }
                        let surrogate = unhex4(&token[i + 2..]);
                        if !(0xDC00..=0xDFFF).contains(&surrogate) {
                            return Err("Invalid \\uXXXX\\uXXXX surrogate pair escape");
                        }
                        i += 6;
                        codepoint =
                            0x10000 + (((codepoint - 0xD800) << 10) | (surrogate as u32 - 0xDC00));
                    }
                    if codepoint > 0x10FFFF {
                        codepoint = 0xFFFD;
                    }
                    jvp_utf8_encode(codepoint, &mut out);
                }
                _ => return Err("Invalid escape"),
            }
        } else {
            if c & !0x1F == 0 {
                return Err(
                    "Invalid string: control characters from U+0000 through U+001F must be escaped",
                );
            }
            out.push(c);
        }
    }
    Ok(jv_string_sized(&out).into_owned())
}

/// The escape-run rule's action: `jv_parse_sized(jv_string_fmt("\"%.*s\"", tok))`.
///
/// `jv_string_fmt` replaces invalid UTF-8 before the JSON parser sees the text, which
/// shifts the reported column (`"\é"` fails at column 6, not 4).
fn parse_escapes(tok: &[u8]) -> Result<String, String> {
    let mut raw = Vec::with_capacity(tok.len() + 2);
    raw.push(b'"');
    raw.extend_from_slice(tok);
    raw.push(b'"');
    let text = jv_string_sized(&raw);
    let buf = text.as_bytes();

    // The JSON parser's scan loop (jv_parser_next/scan) on a single string value.
    let mut line = 1;
    let mut column = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut token: Vec<u8> = Vec::with_capacity(buf.len());
    let mut result = None;
    for &ch in buf {
        column += 1;
        if ch == b'\n' {
            line += 1;
            column = 0;
        }
        if !in_string {
            // Only the opening quote is seen outside the string.
            in_string = ch == b'"';
            continue;
        }
        if ch == b'"' && !escape {
            match found_string(&token) {
                Ok(s) => result = Some(s),
                Err(msg) => {
                    // "%s at line %d, column %d" + " (while parsing '%s')"; the text is
                    // printed with %s, so it stops at a NUL byte.
                    let shown = text.split('\0').next().unwrap_or("");
                    return Err(format!(
                        "{msg} at line {line}, column {column} (while parsing '{shown}')"
                    ));
                }
            }
            in_string = false;
            continue;
        }
        token.push(ch);
        escape = ch == b'\\' && !escape;
    }
    Ok(result.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        tokenize(src.as_bytes())
            .into_iter()
            .map(|(t, loc)| match &t.text {
                Some(text) => format!("{}({})@{}-{}", t.name(), text, loc.start, loc.end),
                None => format!("{}@{}-{}", t.name(), loc.start, loc.end),
            })
            .collect()
    }

    #[test]
    fn longest_match_and_keywords() {
        assert_eq!(
            names("if ifx if::x $__loc__ $__loc__x"),
            [
                "if@0-2",
                "IDENT(ifx)@3-6",
                "IDENT(if::x)@7-12",
                "$__loc__@13-21",
                "BINDING(__loc__x)@22-31",
                "end of file@22-31",
            ]
        );
        assert_eq!(
            names("..a .. . .5 .e5 1.e5 1.a"),
            [
                "..@0-2",
                "IDENT(a)@2-3",
                "..@4-6",
                "'.'@7-8",
                "LITERAL(.5)@9-11",
                "FIELD(e5)@12-15",
                "LITERAL(1.e5)@16-20",
                "LITERAL(1.)@21-23",
                "IDENT(a)@23-24",
                "end of file@23-24",
            ]
        );
        assert_eq!(
            names("?// //= // /= |= ?"),
            [
                "?//@0-3",
                "//=@4-7",
                "//@8-10",
                "/=@11-13",
                "|=@14-16",
                "'?'@17-18",
                "end of file@17-18",
            ]
        );
        assert_eq!(
            names("a::b:: a:::b"),
            [
                "IDENT(a::b)@0-4",
                "':'@4-5",
                "':'@5-6",
                "IDENT(a)@7-8",
                "':'@8-9",
                "':'@9-10",
                "':'@10-11",
                "IDENT(b)@11-12",
                "end of file@11-12",
            ]
        );
        assert_eq!(
            names("$$$$x $ @ @b64 !"),
            [
                "'$'@0-1",
                "'$'@1-2",
                "'$'@2-3",
                "BINDING(x)@3-5",
                "'$'@6-7",
                "INVALID_CHARACTER@8-9",
                "FORMAT(b64)@10-14",
                "INVALID_CHARACTER@15-16",
                "end of file@15-16",
            ]
        );
    }

    #[test]
    fn brackets_must_match() {
        assert_eq!(
            names("[1)"),
            [
                "'['@0-1",
                "LITERAL(1)@1-2",
                "INVALID_CHARACTER@2-3",
                "end of file@2-3"
            ]
        );
        assert_eq!(names(")"), ["INVALID_CHARACTER@0-1", "end of file@0-1"]);
        assert_eq!(
            names(r#""\(1)""#),
            [
                "QQSTRING_START@0-1",
                "QQSTRING_INTERP_START@1-3",
                "LITERAL(1)@3-4",
                "QQSTRING_INTERP_END@4-5",
                "QQSTRING_END@5-6",
                "end of file@5-6",
            ]
        );
    }

    #[test]
    fn comments_and_eof_location() {
        // EOF keeps the location of the last lexeme, including whitespace/comments.
        assert_eq!(
            names("1 +   "),
            ["LITERAL(1)@0-1", "'+'@2-3", "end of file@3-6"]
        );
        assert_eq!(
            names("1 # c \\\n 2\n3"),
            ["LITERAL(1)@0-1", "LITERAL(3)@11-12", "end of file@11-12"]
        );
        assert_eq!(
            names("1 # c \\\\\n2"),
            ["LITERAL(1)@0-1", "LITERAL(2)@9-10", "end of file@9-10"]
        );
        assert_eq!(names("1 # abc"), ["LITERAL(1)@0-1", "end of file@6-7"]);
        assert_eq!(names("1 #\r\n"), ["LITERAL(1)@0-1", "end of file@3-5"]);
    }

    #[test]
    fn string_escapes() {
        let t = tokenize(r#""a\né😀b""#.as_bytes());
        let texts: Vec<_> = t.iter().filter_map(|(t, _)| t.text.clone()).collect();
        assert_eq!(texts, ["a", "\n", "\u{e9}\u{1F600}b"]);
        // escape runs and raw text are separate tokens
        let t = tokenize(r#""\t\/é😀\"xé😀""#.as_bytes());
        let texts: Vec<_> = t.iter().filter_map(|(t, _)| t.text.clone()).collect();
        assert_eq!(texts, ["\t/", "\u{e9}\u{1F600}", "\"", "x\u{e9}\u{1F600}"]);

        let err = |src: &[u8]| {
            tokenize(src)
                .into_iter()
                .find_map(|(t, _)| t.error)
                .unwrap()
        };
        assert_eq!(
            err(br#""\x""#),
            r#"Invalid escape at line 1, column 4 (while parsing '"\x"')"#
        );
        assert_eq!(
            err(br#""\u12""#),
            r#"Invalid \uXXXX escape at line 1, column 6 (while parsing '"\u12"')"#
        );
        assert_eq!(
            err(br#""\uZZZZ""#),
            r#"Invalid characters in \uXXXX escape at line 1, column 8 (while parsing '"\uZZZZ"')"#
        );
        assert_eq!(
            err(br#""\ud83dx""#),
            r#"Invalid \uXXXX\uXXXX surrogate pair escape at line 1, column 8 (while parsing '"\ud83d"')"#
        );
        // invalid UTF-8 is replaced before the JSON parser sees it
        assert_eq!(
            err("\"a\\é\"".as_bytes()),
            "Invalid escape at line 1, column 6 (while parsing '\"\\\u{FFFD}\"')"
        );
        // lone low surrogate: encoded, then replaced
        let t = tokenize(br#""\udc00""#);
        assert_eq!(t[1].0.text.as_deref(), Some("\u{FFFD}"));
    }

    #[test]
    fn jq_utf8_replacement() {
        assert_eq!(jv_string_sized(b"a\xe0\x80\x80b"), "a\u{FFFD}b"); // overlong: one U+FFFD
        assert_eq!(jv_string_sized(b"a\xed\xa0\x80b"), "a\u{FFFD}b"); // surrogate
        assert_eq!(jv_string_sized(b"a\xc3"), "a\u{FFFD}");
        assert_eq!(jv_string_sized(b"a\xe0A"), "a\u{FFFD}"); // truncated at end swallows 'A'
        assert_eq!(jv_string_sized(b"\xe0Ab"), "\u{FFFD}Ab");
        assert_eq!(jv_string_sized(b"\xff\x80"), "\u{FFFD}\u{FFFD}");
    }
}
