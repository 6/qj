//! Port of jq's `jv_parse.c`: the incremental JSON parser used for input
//! (`jv_parser_new` / `jv_parser_set_buf` / `jv_parser_next`), `--stream`
//! (`JV_PARSE_STREAMING`), `--stream-errors` and `--seq` (`JV_PARSE_SEQ`), and
//! `jv_parse_sized` (`fromjson`, `--argjson`).
//!
//! jq's grammar is looser than JSON: any run of characters other than
//! whitespace, quotes and `[{:,}]` is a literal, and literals other than
//! `true`/`false`/`null` go through decNumber (`nan`, `Infinity`, `01`, `+1`,
//! `.5` are all accepted). Error messages, positions (line and byte column
//! of the character where the error is noticed) and recovery all follow the
//! C code.

use super::unicode::utf8_encode;
use super::{Array, Error, Number, Object, Str, Value};

/// `MAX_PARSING_DEPTH`: containers *and pending object keys* on the stack.
pub const MAX_PARSING_DEPTH: usize = 10000;

const RS: u8 = 0x1E;
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Parser flags (`JV_PARSE_SEQ`, `JV_PARSE_STREAMING`,
/// `JV_PARSE_STREAM_ERRORS`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParseFlags {
    /// `--seq`: RS-separated JSON text sequences with error resync.
    pub seq: bool,
    /// `--stream`: emit `[path, leaf]` / `[path]` events.
    pub streaming: bool,
    /// `--stream-errors`: errors become `[message, path]` values (only
    /// meaningful together with `streaming`).
    pub stream_errors: bool,
}

impl ParseFlags {
    /// `JV_PARSE_SEQ`.
    pub const SEQ: u32 = 1;
    /// `JV_PARSE_STREAMING`.
    pub const STREAMING: u32 = 2;
    /// `JV_PARSE_STREAM_ERRORS`.
    pub const STREAM_ERRORS: u32 = 4;

    /// From jq's bit flags.
    pub fn from_bits(bits: u32) -> ParseFlags {
        ParseFlags {
            seq: bits & Self::SEQ != 0,
            streaming: bits & Self::STREAMING != 0,
            stream_errors: bits & Self::STREAM_ERRORS != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastSeen {
    None,
    OpenArray,
    OpenObject,
    Colon,
    Comma,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Normal,
    String,
    StringEscape,
    /// parse error, waiting for RS
    WaitingForRs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChClass {
    Literal,
    Whitespace,
    Structure,
    Quote,
}

#[inline]
fn classify(c: u8) -> ChClass {
    match c {
        b' ' | b'\t' | b'\r' | b'\n' => ChClass::Whitespace,
        b'"' => ChClass::Quote,
        b'[' | b',' | b']' | b'{' | b':' | b'}' => ChClass::Structure,
        _ => ChClass::Literal,
    }
}

type PResult = Result<(), &'static str>;

/// jq's incremental JSON parser (`struct jv_parser`).
///
/// Feed it with [`Parser::set_buf`] and pull values with [`Parser::next`]
/// until it returns `None`; then either feed the next buffer (if the last
/// one was partial) or stop. Values may span buffers, and the parser keeps
/// its position (line/column) across them.
pub struct Parser {
    flags: ParseFlags,
    buf: Vec<u8>,
    buf_pos: usize,
    has_buf: bool,
    buf_is_partial: bool,
    eof: bool,
    bom_strip_position: u8,

    /// parser: containers and pending object keys
    stack: Vec<Value>,
    /// streamer: current path length
    stacklen: usize,
    /// streamer: current path
    path: Array,
    last_seen: LastSeen,
    /// streamer: pending event
    output: Option<Value>,
    next: Option<Value>,

    token: Vec<u8>,
    scratch: Vec<u8>,

    line: i32,
    column: i32,

    st: State,
    last_ch_was_ws: bool,
}

/// What `scan` reports: nothing yet, a value was produced, or an error.
type ScanResult = Result<bool, &'static str>;

impl Parser {
    /// `jv_parser_new(flags)`.
    pub fn new(flags: ParseFlags) -> Parser {
        Parser {
            flags,
            buf: Vec::new(),
            buf_pos: 0,
            has_buf: false,
            buf_is_partial: false,
            eof: false,
            bom_strip_position: 0,
            stack: Vec::new(),
            stacklen: 0,
            path: Array::new(),
            last_seen: LastSeen::None,
            output: None,
            next: None,
            token: Vec::new(),
            scratch: Vec::new(),
            line: 1,
            column: 0,
            st: if flags.seq {
                State::WaitingForRs
            } else {
                State::Normal
            },
            last_ch_was_ws: false,
        }
    }

    #[inline]
    fn streaming(&self) -> bool {
        self.flags.streaming
    }

    /// `jv_parser_set_buf`: hands the parser the next chunk of input
    /// (copied). `is_partial` is true when more input may follow. A UTF-8
    /// BOM at the very start of the input is skipped (even across chunks).
    pub fn set_buf(&mut self, buf: &[u8], is_partial: bool) {
        debug_assert!(
            !self.has_buf || self.buf_pos == self.buf.len(),
            "previous buffer not exhausted"
        );
        let mut b = buf;
        while !b.is_empty() && (self.bom_strip_position as usize) < UTF8_BOM.len() {
            if b[0] == UTF8_BOM[self.bom_strip_position as usize] {
                // matched a BOM character
                b = &b[1..];
                self.bom_strip_position += 1;
            } else if self.bom_strip_position == 0 {
                // no BOM in this document
                self.bom_strip_position = UTF8_BOM.len() as u8;
            } else {
                // malformed BOM (prefix present, rest missing)
                self.bom_strip_position = 0xff;
            }
        }
        self.buf.clear();
        self.buf.extend_from_slice(b);
        self.buf_pos = 0;
        self.has_buf = true;
        self.buf_is_partial = is_partial;
    }

    /// `jv_parser_remaining`: unconsumed bytes of the current buffer.
    pub fn remaining(&self) -> usize {
        if !self.has_buf {
            0
        } else {
            self.buf.len() - self.buf_pos
        }
    }

    /// The position counters used in error messages: `(line, column)`.
    pub fn position(&self) -> (i32, i32) {
        (self.line, self.column)
    }

    /// A parser that continues a stream at a top-level text boundary, as if
    /// input up to `line`/`column` had already been consumed. The input
    /// start (and its BOM) is behind it, so no BOM is stripped.
    pub fn resume(flags: ParseFlags, line: i32, column: i32) -> Parser {
        let mut p = Parser::new(flags);
        p.line = line;
        p.column = column;
        p.bom_strip_position = UTF8_BOM.len() as u8;
        p
    }

    /// Whether the parser is between top-level texts with nothing pending:
    /// no open container, partial token or string, or undelivered value.
    pub fn is_idle(&self) -> bool {
        self.st == State::Normal
            && self.token.is_empty()
            && self.next.is_none()
            && if self.streaming() {
                self.stacklen == 0 && self.output.is_none()
            } else {
                self.stack.is_empty()
            }
    }

    /// `parser_reset`.
    fn reset(&mut self) {
        if self.streaming() {
            self.path = Array::new();
            self.stacklen = 0;
        }
        self.last_seen = LastSeen::None;
        self.output = None;
        self.next = None;
        self.stack.clear();
        self.token.clear();
        self.st = State::Normal;
    }

    fn value(&mut self, val: Value) -> PResult {
        if self.streaming() {
            if self.next.is_some() || self.last_seen == LastSeen::Value {
                return Err("Expected separator between values");
            }
            self.last_seen = if self.stacklen > 0 {
                LastSeen::Value
            } else {
                LastSeen::None
            };
        } else if self.next.is_some() {
            return Err("Expected separator between values");
        }
        self.next = Some(val);
        Ok(())
    }

    fn parse_token(&mut self, ch: u8) -> PResult {
        match ch {
            b'[' | b'{' => {
                if self.stack.len() >= MAX_PARSING_DEPTH {
                    return Err("Exceeds depth limit for parsing");
                }
                if self.next.is_some() {
                    return Err("Expected separator between values");
                }
                self.stack.push(if ch == b'[' {
                    Value::Array(Array::new())
                } else {
                    Value::Object(Object::new())
                });
            }
            b':' => {
                if self.next.is_none() {
                    return Err("Expected string key before ':'");
                }
                if !matches!(self.stack.last(), Some(Value::Object(_))) {
                    return Err("':' not as part of an object");
                }
                if !matches!(self.next, Some(Value::String(_))) {
                    return Err("Object keys must be strings");
                }
                let key = self.next.take().expect("checked");
                self.stack.push(key);
            }
            b',' => {
                let Some(next) = self.next.take() else {
                    return Err("Expected value before ','");
                };
                match self.stack.last_mut() {
                    None => {
                        self.next = Some(next);
                        return Err("',' not as part of an object or array");
                    }
                    Some(Value::Array(a)) => a.push(next),
                    Some(Value::String(_)) => {
                        let Some(Value::String(key)) = self.stack.pop() else {
                            unreachable!()
                        };
                        match self.stack.last_mut() {
                            Some(Value::Object(o)) => o.insert(key, next),
                            _ => unreachable!("keys are only pushed on objects"),
                        }
                    }
                    Some(_) => {
                        // this case hits on input like {"a", "b"}
                        self.next = Some(next);
                        return Err("Objects must consist of key:value pairs");
                    }
                }
            }
            b']' => {
                let Some(Value::Array(top)) = self.stack.last_mut() else {
                    return Err("Unmatched ']'");
                };
                if let Some(next) = self.next.take() {
                    top.push(next);
                } else if !top.is_empty() {
                    // this case hits on input like [1,2,3,]
                    return Err("Expected another array element");
                }
                self.next = self.stack.pop();
            }
            b'}' => {
                if self.stack.is_empty() {
                    return Err("Unmatched '}'");
                }
                if self.next.is_some() {
                    if !matches!(self.stack.last(), Some(Value::String(_))) {
                        return Err("Objects must consist of key:value pairs");
                    }
                    let next = self.next.take().expect("checked");
                    let Some(Value::String(key)) = self.stack.pop() else {
                        unreachable!()
                    };
                    match self.stack.last_mut() {
                        Some(Value::Object(o)) => o.insert(key, next),
                        _ => unreachable!("keys are only pushed on objects"),
                    }
                } else {
                    match self.stack.last() {
                        Some(Value::Object(o)) => {
                            if !o.is_empty() {
                                return Err("Expected another key-value pair");
                            }
                        }
                        _ => return Err("Unmatched '}'"),
                    }
                }
                self.next = self.stack.pop();
            }
            _ => {}
        }
        Ok(())
    }

    /// `jv_array_get(p->path, p->stacklen - 1)` (the innermost path element).
    fn path_last(&self) -> Option<&Value> {
        if self.stacklen == 0 {
            None
        } else {
            self.path.get(self.stacklen - 1)
        }
    }

    fn event(parts: Vec<Value>) -> Value {
        let mut a = Array::new();
        for p in parts {
            a.push(p);
        }
        Value::Array(a)
    }

    fn stream_token(&mut self, ch: u8) -> PResult {
        match ch {
            b'[' => {
                if self.next.is_some() {
                    return Err("Expected a separator between values");
                }
                if self.last_seen == LastSeen::OpenObject {
                    // Looks like {["foo"]}
                    return Err("Expected string key after '{', not '['");
                }
                if self.last_seen == LastSeen::Comma
                    && !matches!(self.path_last(), Some(Value::Number(_)))
                {
                    // Looks like {"x":"y",["foo"]}
                    return Err("Expected string key after ',' in object, not '['");
                }
                self.path.push(Value::number(0.0)); // push
                self.last_seen = LastSeen::OpenArray;
                self.stacklen += 1;
            }
            b'{' => {
                if self.last_seen == LastSeen::Value {
                    return Err("Expected a separator between values");
                }
                if self.last_seen == LastSeen::OpenObject {
                    // Looks like {{"foo":"bar"}}
                    return Err("Expected string key after '{', not '{'");
                }
                if self.last_seen == LastSeen::Comma
                    && !matches!(self.path_last(), Some(Value::Number(_)))
                {
                    // Looks like {"x":"y",{"foo":"bar"}}
                    return Err("Expected string key after ',' in object, not '{'");
                }
                // Push object key: null, since we don't know it yet
                self.path.push(Value::Null); // push
                self.last_seen = LastSeen::OpenObject;
                self.stacklen += 1;
            }
            b':' => {
                if self.stacklen == 0 || matches!(self.path_last(), Some(Value::Number(_))) {
                    return Err("':' not as part of an object");
                }
                if self.next.is_none() || self.last_seen == LastSeen::None {
                    return Err("Expected string key before ':'");
                }
                if !matches!(self.next, Some(Value::String(_))) {
                    return Err("Object keys must be strings");
                }
                if self.last_seen != LastSeen::Value {
                    return Err("':' should follow a key");
                }
                self.last_seen = LastSeen::Colon;
                let key = self.next.take().expect("checked");
                self.path
                    .set(self.stacklen as i64 - 1, key)
                    .expect("in range");
            }
            b',' => {
                if self.last_seen != LastSeen::Value {
                    return Err("Expected value before ','");
                }
                if self.stacklen == 0 {
                    return Err("',' not as part of an object or array");
                }
                match self.path_last() {
                    Some(Value::Number(n)) => {
                        let idx = super::string::double_to_int(n.value());
                        if let Some(next) = self.next.take() {
                            self.output =
                                Some(Self::event(vec![Value::Array(self.path.clone()), next]));
                        }
                        self.path
                            .set(self.stacklen as i64 - 1, Value::from(idx + 1))
                            .expect("in range");
                        self.last_seen = LastSeen::Comma;
                    }
                    Some(Value::String(_)) => {
                        if let Some(next) = self.next.take() {
                            self.output =
                                Some(Self::event(vec![Value::Array(self.path.clone()), next]));
                        }
                        // ready for another key:value pair
                        self.path
                            .set(self.stacklen as i64 - 1, Value::Null)
                            .expect("in range");
                        self.last_seen = LastSeen::Comma;
                    }
                    _ => {
                        // this case hits on input like {,}
                        // make sure to handle input like {"a", "b"} and {"a":, ...}
                        return Err("Objects must consist of key:value pairs");
                    }
                }
            }
            b']' => {
                if self.stacklen == 0 {
                    return Err("Unmatched ']' at the top-level");
                }
                if self.last_seen == LastSeen::Comma {
                    return Err("Expected another array element");
                }
                if !matches!(self.path_last(), Some(Value::Number(_))) {
                    return Err("Unmatched ']' in the middle of an object");
                }
                if let Some(next) = self.next.take() {
                    self.output = Some(Self::event(vec![
                        Value::Array(self.path.clone()),
                        next,
                        Value::Bool(true),
                    ]));
                } else if self.last_seen != LastSeen::OpenArray {
                    self.output = Some(Self::event(vec![Value::Array(self.path.clone())]));
                }
                self.stacklen -= 1;
                self.path = self.path.slice(0, self.stacklen as i64); // pop
                self.next = None;
                if self.last_seen == LastSeen::OpenArray {
                    // Empty arrays are leaves
                    self.output = Some(Self::event(vec![
                        Value::Array(self.path.clone()),
                        Value::empty_array(),
                    ]));
                }
                self.last_seen = if self.stacklen == 0 {
                    LastSeen::None
                } else {
                    LastSeen::Value
                };
            }
            b'}' => {
                if self.stacklen == 0 {
                    return Err("Unmatched '}' at the top-level");
                }
                if self.last_seen == LastSeen::Comma {
                    return Err("Expected another key:value pair");
                }
                let last_is_string = matches!(self.path_last(), Some(Value::String(_)));
                if matches!(self.path_last(), Some(Value::Number(_))) {
                    return Err("Unmatched '}' in the middle of an array");
                }
                if self.next.is_some() {
                    if !last_is_string {
                        return Err("Objects must consist of key:value pairs");
                    }
                    let next = self.next.take().expect("checked");
                    self.output = Some(Self::event(vec![
                        Value::Array(self.path.clone()),
                        next,
                        Value::Bool(true),
                    ]));
                } else {
                    // Perhaps {"a":[]}
                    if self.last_seen == LastSeen::Colon {
                        // Looks like {"a":}
                        return Err("Missing value in key:value pair");
                    }
                    if self.last_seen == LastSeen::Comma {
                        // Looks like {"a":0,}
                        return Err("Expected another key-value pair");
                    }
                    if self.last_seen == LastSeen::OpenArray {
                        return Err("Unmatched '}' in the middle of an array");
                    }
                    if self.last_seen != LastSeen::Value && self.last_seen != LastSeen::OpenObject {
                        return Err("Unmatched '}'");
                    }
                    if self.last_seen != LastSeen::OpenObject {
                        self.output = Some(Self::event(vec![Value::Array(self.path.clone())]));
                    }
                }
                self.stacklen -= 1;
                self.path = self.path.slice(0, self.stacklen as i64); // pop
                self.next = None;
                if self.last_seen == LastSeen::OpenObject {
                    // Empty arrays are leaves
                    self.output = Some(Self::event(vec![
                        Value::Array(self.path.clone()),
                        Value::empty_object(),
                    ]));
                }
                self.last_seen = if self.stacklen == 0 {
                    LastSeen::None
                } else {
                    LastSeen::Value
                };
            }
            _ => {}
        }
        Ok(())
    }

    #[inline]
    fn token(&mut self, ch: u8) -> PResult {
        if self.streaming() {
            self.stream_token(ch)
        } else {
            self.parse_token(ch)
        }
    }

    fn found_string(&mut self) -> PResult {
        let tok = &self.token;
        // Fast path: nothing to unescape or reject.
        if !tok.iter().any(|&c| c == b'\\' || c < 0x20) {
            let s = Str::from_bytes(tok);
            self.value(Value::String(s))?;
            self.token.clear();
            return Ok(());
        }
        let out = &mut self.scratch;
        out.clear();
        let end = tok.len();
        let mut i = 0;
        while i < end {
            let c = tok[i];
            i += 1;
            if c == b'\\' {
                if i >= end {
                    return Err("Expected escape character at end of string");
                }
                let c = tok[i];
                i += 1;
                match c {
                    b'\\' | b'"' | b'/' => out.push(c),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b't' => out.push(b'\t'),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b'u' => {
                        // ahh, the complicated case
                        if i + 4 > end {
                            return Err("Invalid \\uXXXX escape");
                        }
                        let Some(hexvalue) = unhex4(&tok[i..i + 4]) else {
                            return Err("Invalid characters in \\uXXXX escape");
                        };
                        let mut codepoint = hexvalue;
                        i += 4;
                        if (0xD800..=0xDBFF).contains(&codepoint) {
                            // who thought UTF-16 surrogate pairs were a good idea?
                            if i + 6 > end || tok[i] != b'\\' || tok[i + 1] != b'u' {
                                return Err("Invalid \\uXXXX\\uXXXX surrogate pair escape");
                            }
                            let surrogate = unhex4(&tok[i + 2..i + 6]);
                            let Some(surrogate) =
                                surrogate.filter(|s| (0xDC00..=0xDFFF).contains(s))
                            else {
                                return Err("Invalid \\uXXXX\\uXXXX surrogate pair escape");
                            };
                            i += 6;
                            codepoint =
                                0x10000 + (((codepoint - 0xD800) << 10) | (surrogate - 0xDC00));
                        }
                        if codepoint > 0x10FFFF {
                            codepoint = 0xFFFD; // U+FFFD REPLACEMENT CHARACTER
                        }
                        // A lone low surrogate is encoded as-is (invalid UTF-8)
                        // and becomes U+FFFD below, exactly as in jq.
                        utf8_encode(codepoint, out);
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
        let s = Str::from_bytes(&self.scratch);
        self.value(Value::String(s))?;
        self.token.clear();
        Ok(())
    }

    fn check_literal(&mut self) -> PResult {
        if self.token.is_empty() {
            return Ok(());
        }
        let pattern: Option<(&[u8], Value)> = match self.token[0] {
            b't' => Some((b"true", Value::Bool(true))),
            b'f' => Some((b"false", Value::Bool(false))),
            b'\'' => return Err("Invalid string literal; expected \", but got '"),
            // if it starts with 'n', it could be a literal "nan"
            b'n' if self.token.len() > 1 && self.token[1] == b'u' => Some((b"null", Value::Null)),
            _ => None,
        };
        match pattern {
            Some((pat, v)) => {
                if self.token != pat {
                    return Err("Invalid literal");
                }
                self.value(v)?;
            }
            None => {
                // FIXME: better parser
                let Some(number) = Number::from_c_literal(&self.token) else {
                    return Err("Invalid numeric literal");
                };
                self.value(Value::Number(number))?;
            }
        }
        self.token.clear();
        Ok(())
    }

    fn parse_check_done(&mut self, out: &mut Option<Value>) -> bool {
        if self.stack.is_empty() && self.next.is_some() {
            *out = self.next.take();
            true
        } else {
            false
        }
    }

    fn stream_check_done(&mut self, out: &mut Option<Value>) -> bool {
        if self.stacklen == 0 && self.next.is_some() {
            let next = self.next.take().expect("checked");
            *out = Some(Self::event(vec![Value::Array(self.path.clone()), next]));
            true
        } else if let Some(output) = self.output.take() {
            let Value::Array(a) = &output else {
                unreachable!("events are arrays")
            };
            if a.len() > 2 {
                // At end of an array or object, necessitating one more output
                // by which to indicate this
                *out = Some(Value::Array(a.slice(0, 2)));
                self.output = Some(Value::Array(a.slice(0, 1))); // arrange one more output
            } else {
                // No further processing needed
                *out = Some(output);
            }
            true
        } else {
            false
        }
    }

    #[inline]
    fn check_done(&mut self, out: &mut Option<Value>) -> bool {
        if self.streaming() {
            self.stream_check_done(out)
        } else {
            self.parse_check_done(out)
        }
    }

    fn next_is_number(&self) -> bool {
        matches!(self.next, Some(Value::Number(_)))
    }

    fn check_truncation(&self) -> bool {
        if self.streaming() {
            // stream_seq_check_truncation
            self.stacklen > 0
                || matches!(
                    self.next,
                    Some(Value::Number(_) | Value::Bool(_) | Value::Null)
                )
        } else {
            // seq_check_truncation
            !self.last_ch_was_ws
                && (!self.stack.is_empty() || !self.token.is_empty() || self.next_is_number())
        }
    }

    fn is_top_num(&self) -> bool {
        if self.streaming() {
            self.stacklen == 0 && self.next_is_number()
        } else {
            self.stack.is_empty() && self.next_is_number()
        }
    }

    fn scan(&mut self, ch: u8, out: &mut Option<Value>) -> ScanResult {
        self.column += 1;
        if ch == b'\n' {
            self.line += 1;
            self.column = 0;
        }
        if self.flags.seq && ch == RS {
            if self.check_truncation() {
                if self.check_literal().is_ok() && self.is_top_num() {
                    return Err("Potentially truncated top-level numeric value");
                }
                return Err("Truncated value");
            }
            self.check_literal()?;
            if self.st == State::Normal && self.check_done(out) {
                return Ok(true);
            }
            // shouldn't happen?
            self.reset();
            *out = None;
            return Ok(true);
        }
        let mut answer = false;
        self.last_ch_was_ws = false;
        if self.st == State::Normal {
            let cls = classify(ch);
            if cls == ChClass::Whitespace {
                self.last_ch_was_ws = true;
            }
            if cls != ChClass::Literal {
                self.check_literal()?;
                if self.check_done(out) {
                    answer = true;
                }
            }
            match cls {
                ChClass::Literal => self.token.push(ch),
                ChClass::Whitespace => {}
                ChClass::Quote => self.st = State::String,
                ChClass::Structure => self.token(ch)?,
            }
            if self.check_done(out) {
                answer = true;
            }
        } else if ch == b'"' && self.st == State::String {
            self.found_string()?;
            self.st = State::Normal;
            if self.check_done(out) {
                answer = true;
            }
        } else {
            self.token.push(ch);
            if ch == b'\\' && self.st == State::String {
                self.st = State::StringEscape;
            } else {
                self.st = State::String;
            }
        }
        Ok(answer)
    }

    /// `make_error`: an error, or with `--stream-errors` a
    /// `[message, path]` value.
    fn make_error(&self, msg: String) -> Result<Value, Error> {
        if self.flags.stream_errors && self.streaming() {
            Ok(Self::event(vec![
                Value::from(msg),
                Value::Array(self.path.clone()),
            ]))
        } else {
            Err(Error::msg(msg))
        }
    }

    /// Consumes a run of bytes that `scan` would only append to the current
    /// string token (plain string bytes), keeping the column count exact.
    /// Returns the new position.
    #[inline]
    fn fast_string_run(&mut self, pos: usize) -> usize {
        let buf = &self.buf[pos..];
        let n = memchr::memchr3(b'"', b'\\', b'\n', buf).unwrap_or(buf.len());
        if n > 0 {
            self.token.extend_from_slice(&buf[..n]);
            self.column += n as i32;
            self.last_ch_was_ws = false;
        }
        pos + n
    }

    /// Consumes a run of literal characters (`scan` only appends them to
    /// the token: a literal character never completes a value). In `--seq`
    /// mode RS is left to `scan`.
    #[inline]
    fn fast_literal_run(&mut self, pos: usize) -> usize {
        let buf = &self.buf[pos..];
        let seq = self.flags.seq;
        let n = buf
            .iter()
            .position(|&c| classify(c) != ChClass::Literal || (seq && c == RS))
            .unwrap_or(buf.len());
        if n > 0 {
            self.token.extend_from_slice(&buf[..n]);
            self.column += n as i32;
            self.last_ch_was_ws = false;
        }
        pos + n
    }

    /// After `scan` handled a whitespace character without producing a
    /// value, the whitespace that follows cannot produce one either (the
    /// token is flushed and any finished value was already returned), so it
    /// only moves the position.
    #[inline]
    fn skip_whitespace_run(&mut self, pos: usize) -> usize {
        let buf = &self.buf;
        let mut p = pos;
        while p < buf.len() {
            match buf[p] {
                b'\n' => {
                    self.line += 1;
                    self.column = 0;
                }
                b' ' | b'\t' | b'\r' => self.column += 1,
                _ => break,
            }
            p += 1;
        }
        if p > pos {
            self.last_ch_was_ws = true;
        }
        p
    }

    /// `jv_parser_next`: the next value, `Some(Err)` for a parse error, or
    /// `None` when the current buffer is exhausted (feed more if it was
    /// partial) or the input has ended. In `--seq` mode a stray RS can also
    /// yield `None` with input remaining; callers should keep calling while
    /// [`Parser::remaining`] is non-zero, as jq's input loop does.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Result<Value, Error>> {
        if self.eof {
            return None;
        }
        if !self.has_buf {
            return None; // Need a buffer
        }
        if self.bom_strip_position == 0xff {
            if !self.flags.seq {
                return Some(Err(Error::msg("Malformed BOM")));
            }
            self.st = State::WaitingForRs;
            self.reset();
        }
        let mut value: Option<Value> = None;
        if self.streaming() && self.stream_check_done(&mut value) {
            return value.map(Ok);
        }
        let mut ch: u8 = 0;
        let mut msg: ScanResult = Ok(false);
        let fast_strings = !self.flags.seq;
        while matches!(msg, Ok(false)) && self.buf_pos < self.buf.len() {
            match self.st {
                State::String if fast_strings => {
                    self.buf_pos = self.fast_string_run(self.buf_pos);
                    if self.buf_pos >= self.buf.len() {
                        break;
                    }
                }
                State::Normal => {
                    let c = self.buf[self.buf_pos];
                    if classify(c) == ChClass::Literal && !(self.flags.seq && c == RS) {
                        self.buf_pos = self.fast_literal_run(self.buf_pos);
                        continue;
                    }
                }
                _ => {}
            }
            ch = self.buf[self.buf_pos];
            self.buf_pos += 1;
            if self.st == State::WaitingForRs {
                if ch == b'\n' {
                    self.line += 1;
                    self.column = 0;
                } else {
                    self.column += 1;
                }
                if ch == RS {
                    self.st = State::Normal;
                }
                continue; // need to resync, wait for RS
            }
            msg = self.scan(ch, &mut value);
            if matches!(msg, Ok(false))
                && self.st == State::Normal
                && classify(ch) == ChClass::Whitespace
            {
                self.buf_pos = self.skip_whitespace_run(self.buf_pos);
            }
        }
        match msg {
            Ok(true) => value.map(Ok),
            Err(m) => {
                drop(value);
                if ch != RS && self.flags.seq {
                    // Skip to the next RS
                    self.st = State::WaitingForRs;
                    let v = self.make_error(format!(
                        "{} at line {}, column {} (need RS to resync)",
                        m, self.line, self.column
                    ));
                    // NB: parser_reset() sets the state back to normal, so jq
                    // does not actually wait for an RS here (verified with
                    // `printf '\x1e[}2 3\x1e' | jq --seq .`, which outputs 2).
                    self.reset();
                    return Some(v);
                }
                let v = self.make_error(format!(
                    "{} at line {}, column {}",
                    m, self.line, self.column
                ));
                self.reset();
                if !self.flags.seq {
                    // We're not parsing a JSON text sequence; throw this buffer away.
                    self.has_buf = false;
                    self.buf_pos = 0;
                    self.buf.clear();
                } // Else ch must be RS; don't clear buf so we can start parsing again after this ch
                Some(v)
            }
            Ok(false) => {
                if self.buf_is_partial {
                    // need another buffer
                    return None;
                }
                // at EOF
                self.eof = true;
                drop(value);
                if self.st == State::WaitingForRs {
                    return Some(self.make_error(format!(
                        "Unfinished abandoned text at EOF at line {}, column {}",
                        self.line, self.column
                    )));
                }
                if self.st != State::Normal {
                    let v = self.make_error(format!(
                        "Unfinished string at EOF at line {}, column {}",
                        self.line, self.column
                    ));
                    self.reset();
                    self.st = State::WaitingForRs;
                    return Some(v);
                }
                if let Err(m) = self.check_literal() {
                    let v = self.make_error(format!(
                        "{} at EOF at line {}, column {}",
                        m, self.line, self.column
                    ));
                    self.reset();
                    self.st = State::WaitingForRs;
                    return Some(v);
                }
                if (self.streaming() && self.stacklen != 0)
                    || (!self.streaming() && !self.stack.is_empty())
                {
                    let v = self.make_error(format!(
                        "Unfinished JSON term at EOF at line {}, column {}",
                        self.line, self.column
                    ));
                    self.reset();
                    self.st = State::WaitingForRs;
                    return Some(v);
                }
                // p->next is either invalid (nothing here, but no syntax error)
                // or valid (this is the value). either way it's the thing to return
                let value = match self.next.take() {
                    Some(next) if self.streaming() => {
                        Some(Self::event(vec![Value::Array(self.path.clone()), next]))
                    }
                    other => other,
                };
                if self.flags.seq && !self.last_ch_was_ws && matches!(value, Some(Value::Number(_)))
                {
                    return Some(self.make_error(format!(
                        "Potentially truncated top-level numeric value at EOF at line {}, column {}",
                        self.line, self.column
                    )));
                }
                value.map(Ok)
            }
        }
    }
}

fn unhex4(hex: &[u8]) -> Option<u32> {
    let mut r: u32 = 0;
    for &c in &hex[..4] {
        let n = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        };
        r = (r << 4) | n as u32;
    }
    Some(r)
}

/// The C `%s` of a byte buffer: stops at the first NUL, and (as
/// `jv_string_fmt` does) repairs invalid UTF-8.
fn c_str_lossy(s: &[u8]) -> String {
    let end = memchr::memchr(0, s).unwrap_or(s.len());
    super::unicode::decode_lossy(&s[..end])
}

/// `jv_parse_sized` (used by `fromjson`, `--argjson`, `--jsonargs`, and for
/// number literals in programs): exactly one JSON value. Errors are
/// suffixed with ` (while parsing '<text>')`; no value is
/// `Expected JSON value`, more than one is `Unexpected extra JSON values`.
pub fn parse_sized(text: &[u8]) -> Result<Value, Error> {
    let mut parser = Parser::new(ParseFlags::default());
    parser.set_buf(text, false);
    let result = match parser.next() {
        Some(Ok(value)) => match parser.next() {
            // multiple JSON values, we only wanted one
            Some(Ok(_)) => Err(Error::msg("Unexpected extra JSON values")),
            // parser error after the first JSON value
            Some(Err(e)) => Err(e),
            // a single valid JSON value
            None => Ok(value),
        },
        // parse error, we'll return it
        Some(Err(e)) => Err(e),
        // no value at all
        None => Err(Error::msg("Expected JSON value")),
    };
    result.map_err(|e| {
        let msg = e.to_string();
        Error::msg(format!("{} (while parsing '{}')", msg, c_str_lossy(text)))
    })
}

/// `jv_parse`: like [`parse_sized`] for a C string (stops at NUL).
pub fn parse(text: &str) -> Result<Value, Error> {
    let end = text.find('\0').unwrap_or(text.len());
    parse_sized(&text.as_bytes()[..end])
}

/// Parses every value in a complete buffer, as jq's input loop would,
/// stopping after the first error unless `flags.seq` is set.
pub fn parse_all(text: &[u8], flags: ParseFlags) -> Vec<Result<Value, Error>> {
    let mut parser = Parser::new(flags);
    parser.set_buf(text, false);
    let mut out = Vec::new();
    loop {
        match parser.next() {
            Some(Ok(v)) => out.push(Ok(v)),
            Some(Err(e)) => {
                out.push(Err(e));
                if !flags.seq {
                    break;
                }
            }
            None => {
                if parser.remaining() == 0 {
                    break;
                }
            }
        }
    }
    out
}
