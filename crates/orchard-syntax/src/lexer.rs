//! The Orchard 3.0 lexer. A faithful port of v2's `lexer.py`.
//!
//! Source → tokens with spans, including the hard parts: Go-style automatic
//! semicolon insertion (virtual `NEWLINE`s), the string-interpolation sub-lexer
//! (`STR_START · (STR_CHUNK | INTERP_START … INTERP_END)* · STR_END`), nested
//! block comments, triple-quote common-indent stripping, and duration/money
//! literals with maximal-munch glue rules.

use crate::diagnostics::Diagnostic;
use crate::error::SyntaxError;
use crate::span::Span;
use crate::tokens::{is_keyword, Token, TokenKind, TokenValue, DURATION_UNITS};

/// Tokenize `source`. Errors are single-error-then-bail.
pub fn tokenize(source: &str, filename: &str) -> Result<Vec<Token>, SyntaxError> {
    Lexer::new(source, filename).run()
}

/// Last-token kinds that suppress a following virtual `NEWLINE` (binary/assign
/// operators, separators, and an open `{`).
fn is_continuation_kind(k: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        k,
        Plus | Minus
            | Star
            | Slash
            | Percent
            | Eq
            | Ne
            | Lt
            | Le
            | Gt
            | Ge
            | Assign
            | PlusEq
            | MinusEq
            | StarEq
            | SlashEq
            | Comma
            | Dot
            | Arrow
            | FatArrow
            | Question
            | QDot
            | Colon
            | Pipe
            | Coalesce
            | Range
            | RangeEq
            | LBrace
    )
}

struct Lexer {
    src: Vec<char>,
    file: String,
    pos: usize,
    line: u32,
    col: u32,
    tokens: Vec<Token>,
    paren_depth: i32,
    triple_interp: u32,
}

/// A fragment of a (possibly interpolated) string literal.
enum Part {
    Text(String),
    Interp(Vec<Token>),
}

impl Lexer {
    fn new(source: &str, filename: &str) -> Self {
        let mut chars: Vec<char> = source.chars().collect();
        // Strip a leading BOM.
        if chars.first() == Some(&'\u{feff}') {
            chars.remove(0);
        }
        Lexer {
            src: chars,
            file: filename.to_string(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
            paren_depth: 0,
            triple_interp: 0,
        }
    }

    // ---- char helpers ----

    fn n(&self) -> usize {
        self.src.len()
    }

    fn at(&self, i: usize) -> Option<char> {
        self.src.get(i).copied()
    }

    fn cur(&self) -> Option<char> {
        self.at(self.pos)
    }

    fn advance(&mut self, count: usize) {
        for _ in 0..count {
            if self.pos >= self.n() {
                break;
            }
            if self.src[self.pos] == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.pos += 1;
        }
    }

    fn here_span(&self) -> Span {
        Span::point(self.file.clone(), self.line, self.col, self.pos)
    }

    fn span_from(&self, start: usize, sl: u32, sc: u32) -> Span {
        Span::new(
            self.file.clone(),
            sl,
            sc,
            self.line,
            self.col,
            start,
            self.pos,
        )
    }

    fn slice(&self, start: usize, end: usize) -> String {
        self.src[start..end].iter().collect()
    }

    fn err(&self, message: &str, span: Span, hint: &str) -> SyntaxError {
        SyntaxError::new(Diagnostic::error(message, Some(span)).with_hint(hint))
    }

    // ---- run ----

    fn run(mut self) -> Result<Vec<Token>, SyntaxError> {
        self.scan_pragma()?;
        loop {
            let saw_newline = self.skip_ws_comments()?;
            if self.pos >= self.n() {
                break;
            }
            if saw_newline && self.should_emit_newline() {
                let sp = self.here_span();
                self.tokens
                    .push(Token::new(TokenKind::Newline, TokenValue::None, "", sp));
            }
            self.scan_main_token()?;
            self.track_paren_depth();
        }
        // Terminating NEWLINE (Go-style) then EOF.
        if let Some(last) = self.tokens.last() {
            if last.kind != TokenKind::Newline {
                let sp = self.here_span();
                self.tokens
                    .push(Token::new(TokenKind::Newline, TokenValue::None, "", sp));
            }
        }
        let sp = self.here_span();
        self.tokens
            .push(Token::new(TokenKind::Eof, TokenValue::None, "", sp));
        Ok(self.tokens)
    }

    // ---- pragma ----

    fn scan_pragma(&mut self) -> Result<(), SyntaxError> {
        if self.cur() != Some('#') {
            return Ok(());
        }
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        if self.at(self.pos + 1) != Some('!') {
            return Err(self.err(
                "unexpected character '#'",
                self.here_span(),
                "'#' is only valid in the leading '#!orchard' pragma",
            ));
        }
        // Consume to end of line (not the newline).
        while let Some(c) = self.cur() {
            if c == '\n' {
                break;
            }
            self.advance(1);
        }
        let line_text = self.slice(start, self.pos);
        let version = parse_pragma(&line_text).ok_or_else(|| {
            self.err(
                "malformed pragma line",
                self.span_from(start, sl, sc),
                "expected '#!orchard 3.0'",
            )
        })?;
        let sp = self.span_from(start, sl, sc);
        self.tokens.push(Token::new(
            TokenKind::Pragma,
            TokenValue::Str(version),
            line_text,
            sp,
        ));
        Ok(())
    }

    // ---- whitespace & comments ----

    /// Skip whitespace and comments. Returns whether a line terminator was
    /// crossed (for ASI).
    fn skip_ws_comments(&mut self) -> Result<bool, SyntaxError> {
        let mut saw_newline = false;
        loop {
            match self.cur() {
                Some(' ') | Some('\t') => self.advance(1),
                Some('\r') => {
                    self.advance(1);
                    if self.cur() == Some('\n') {
                        self.advance(1);
                    }
                    saw_newline = true;
                }
                Some('\n') => {
                    self.advance(1);
                    saw_newline = true;
                }
                Some('/') if self.at(self.pos + 1) == Some('/') => {
                    while let Some(c) = self.cur() {
                        if c == '\n' {
                            break;
                        }
                        self.advance(1);
                    }
                }
                Some('/') if self.at(self.pos + 1) == Some('*') => {
                    if self.consume_block_comment()? {
                        saw_newline = true;
                    }
                }
                _ => break,
            }
        }
        Ok(saw_newline)
    }

    /// Consume a nested block comment. Returns whether it spanned a newline.
    fn consume_block_comment(&mut self) -> Result<bool, SyntaxError> {
        let (sl, sc) = (self.line, self.col);
        let start = self.pos;
        self.advance(2); // /*
        let mut depth = 1;
        let mut saw_newline = false;
        while depth > 0 {
            match self.cur() {
                None => {
                    return Err(self.err(
                        "unterminated block comment",
                        self.span_from(start, sl, sc),
                        "add a closing '*/'",
                    ));
                }
                Some('/') if self.at(self.pos + 1) == Some('*') => {
                    self.advance(2);
                    depth += 1;
                }
                Some('*') if self.at(self.pos + 1) == Some('/') => {
                    self.advance(2);
                    depth -= 1;
                }
                Some('\n') => {
                    self.advance(1);
                    saw_newline = true;
                }
                Some(_) => self.advance(1),
            }
        }
        Ok(saw_newline)
    }

    // ---- ASI ----

    fn should_emit_newline(&self) -> bool {
        let last = match self.tokens.last() {
            None => return false,
            Some(t) => t,
        };
        if last.kind == TokenKind::Newline {
            return false;
        }
        if self.paren_depth > 0 {
            return false;
        }
        if is_continuation_kind(last.kind) {
            return false;
        }
        if let Some(w) = last.keyword_word() {
            if matches!(w, "and" | "or" | "not") {
                return false;
            }
        }
        if self.next_suppresses_newline() {
            return false;
        }
        true
    }

    fn next_suppresses_newline(&self) -> bool {
        match self.cur() {
            Some(')') | Some(']') | Some('}') => true,
            Some(c) if is_ident_start(c) => {
                let w = self.peek_word();
                matches!(w.as_str(), "else" | "catch" | "until" | "in")
            }
            _ => false,
        }
    }

    fn peek_word(&self) -> String {
        let mut i = self.pos;
        let mut s = String::new();
        while let Some(c) = self.at(i) {
            if is_ident_cont(c) {
                s.push(c);
                i += 1;
            } else {
                break;
            }
        }
        s
    }

    fn track_paren_depth(&mut self) {
        if let Some(last) = self.tokens.last() {
            match last.kind {
                TokenKind::LParen | TokenKind::LBrack => self.paren_depth += 1,
                TokenKind::RParen | TokenKind::RBrack => {
                    self.paren_depth = (self.paren_depth - 1).max(0)
                }
                _ => {}
            }
        }
    }

    // ---- main token dispatch ----

    fn scan_main_token(&mut self) -> Result<(), SyntaxError> {
        let c = self.cur().expect("scan_main_token at EOF");
        if is_ident_start(c) {
            self.scan_ident_or_keyword();
            Ok(())
        } else if c.is_ascii_digit() {
            self.scan_number()
        } else if c == '$' {
            self.scan_money()
        } else if c == '`' {
            self.scan_rawstring()
        } else if c == '"' {
            let triple = self.src[self.pos..].starts_with(&['"', '"', '"']);
            self.scan_quoted(triple)
        } else {
            self.scan_operator()
        }
    }

    fn scan_ident_or_keyword(&mut self) {
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        while let Some(c) = self.cur() {
            if is_ident_cont(c) {
                self.advance(1);
            } else {
                break;
            }
        }
        let text = self.slice(start, self.pos);
        let sp = self.span_from(start, sl, sc);
        let (kind, value) = match text.as_str() {
            "true" => (TokenKind::True, TokenValue::Bool(true)),
            "false" => (TokenKind::False, TokenValue::Bool(false)),
            "null" => (TokenKind::Null, TokenValue::None),
            w if is_keyword(w) => (TokenKind::Keyword, TokenValue::Str(text.clone())),
            _ => (TokenKind::Ident, TokenValue::Str(text.clone())),
        };
        self.tokens.push(Token::new(kind, value, text, sp));
    }

    // ---- numbers ----

    fn scan_number(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        self.consume_digit_run();
        let mut is_float = false;
        // Fractional part only if '.' is followed by a digit.
        if self.cur() == Some('.')
            && self
                .at(self.pos + 1)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            self.advance(1);
            self.consume_digit_run();
            is_float = true;
        }
        // Exponent: e/E [+/-] digit+ (at least one digit required).
        if matches!(self.cur(), Some('e') | Some('E')) {
            let mut k = self.pos + 1;
            if matches!(self.at(k), Some('+') | Some('-')) {
                k += 1;
            }
            if self.at(k).map(|c| c.is_ascii_digit()).unwrap_or(false) {
                // consume e, sign, digits
                self.advance(1);
                if matches!(self.cur(), Some('+') | Some('-')) {
                    self.advance(1);
                }
                self.consume_digit_run();
                is_float = true;
            }
        }
        // Glue: an identifier-start immediately following.
        if let Some(c) = self.cur() {
            if is_ident_start(c) {
                if is_float {
                    // consume the ident for the error span
                    while let Some(ch) = self.cur() {
                        if is_ident_cont(ch) {
                            self.advance(1);
                        } else {
                            break;
                        }
                    }
                    return Err(self.err(
                        "number immediately followed by an identifier",
                        self.span_from(start, sl, sc),
                        "insert a space (durations must be integer-valued, e.g. 90m)",
                    ));
                }
                let int_text = self.slice(start, self.pos);
                let unit_start = self.pos;
                while let Some(ch) = self.cur() {
                    if is_ident_cont(ch) {
                        self.advance(1);
                    } else {
                        break;
                    }
                }
                let unit = self.slice(unit_start, self.pos);
                if DURATION_UNITS.contains(&unit.as_str()) {
                    let count: i64 = int_text.replace('_', "").parse().map_err(|_| {
                        self.err("invalid duration count", self.span_from(start, sl, sc), "")
                    })?;
                    let text = self.slice(start, self.pos);
                    let sp = self.span_from(start, sl, sc);
                    self.tokens.push(Token::new(
                        TokenKind::Duration,
                        TokenValue::Duration(count, unit),
                        text,
                        sp,
                    ));
                    return Ok(());
                }
                return Err(self.err(
                    &format!("number immediately followed by identifier '{unit}'"),
                    self.span_from(start, sl, sc),
                    "insert a space, or use a duration unit (ms, s, m, h, d)",
                ));
            }
        }
        let text = self.slice(start, self.pos);
        let sp = self.span_from(start, sl, sc);
        let cleaned = text.replace('_', "");
        if is_float {
            let f: f64 = cleaned
                .parse()
                .map_err(|_| self.err("invalid float literal", sp.clone(), ""))?;
            self.tokens
                .push(Token::new(TokenKind::Float, TokenValue::Float(f), text, sp));
        } else {
            let i: i64 = cleaned
                .parse()
                .map_err(|_| self.err("invalid integer literal", sp.clone(), ""))?;
            self.tokens
                .push(Token::new(TokenKind::Int, TokenValue::Int(i), text, sp));
        }
        Ok(())
    }

    fn consume_digit_run(&mut self) {
        while let Some(c) = self.cur() {
            if c.is_ascii_digit() || c == '_' {
                self.advance(1);
            } else {
                break;
            }
        }
    }

    fn scan_money(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        self.advance(1); // $
        if !self.cur().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            return Err(self.err(
                "expected digits after '$'",
                self.span_from(start, sl, sc),
                "money literals look like $0.50 or $1",
            ));
        }
        let amount_start = self.pos;
        while self.cur().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.advance(1);
        }
        if self.cur() == Some('.')
            && self
                .at(self.pos + 1)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            self.advance(1);
            while self.cur().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                self.advance(1);
            }
        }
        let amount = self.slice(amount_start, self.pos);
        let text = self.slice(start, self.pos);
        let sp = self.span_from(start, sl, sc);
        self.tokens.push(Token::new(
            TokenKind::Money,
            TokenValue::Money(amount),
            text,
            sp,
        ));
        Ok(())
    }

    // ---- raw strings ----

    fn scan_rawstring(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        self.advance(1); // opening `
        let content_start = self.pos;
        loop {
            match self.cur() {
                None => {
                    return Err(self.err(
                        "unterminated raw string",
                        self.span_from(start, sl, sc),
                        "raw strings are delimited by backticks and cannot contain one",
                    ));
                }
                Some('`') => break,
                Some(_) => self.advance(1),
            }
        }
        let content = self.slice(content_start, self.pos);
        self.advance(1); // closing `
        let text = self.slice(start, self.pos);
        let sp = self.span_from(start, sl, sc);
        self.tokens.push(Token::new(
            TokenKind::RawString,
            TokenValue::Str(content),
            text,
            sp,
        ));
        Ok(())
    }

    // ---- quoted strings & interpolation ----

    fn scan_quoted(&mut self, triple: bool) -> Result<(), SyntaxError> {
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        if triple && self.triple_interp > 0 {
            return Err(self.err(
                "a triple-quoted string may not be nested inside a triple-quoted interpolation",
                self.here_span(),
                "bind it to a `let` outside the string instead",
            ));
        }
        let delim_len = if triple { 3 } else { 1 };
        self.advance(delim_len);
        let mut parts: Vec<Part> = Vec::new();
        let mut buf = String::new();

        loop {
            if self.pos >= self.n() {
                return Err(self.err(
                    "unterminated string",
                    self.span_from(start, sl, sc),
                    if triple {
                        "add a closing \"\"\""
                    } else {
                        "add a closing \""
                    },
                ));
            }
            // Closing delimiter?
            if triple {
                if self.src[self.pos..].starts_with(&['"', '"', '"']) {
                    self.advance(3);
                    break;
                }
            } else if self.cur() == Some('"') {
                self.advance(1);
                break;
            }
            let c = self.cur().unwrap();
            if !triple && c == '\n' {
                return Err(self.err(
                    "unterminated string",
                    self.span_from(start, sl, sc),
                    "single-line strings cannot span lines; use \"\"\"...\"\"\"",
                ));
            }
            if c == '\\' {
                buf.push(self.scan_escape()?);
            } else if c == '{' && self.at(self.pos + 1) == Some('{') {
                buf.push('{');
                self.advance(2);
            } else if c == '}' && self.at(self.pos + 1) == Some('}') {
                buf.push('}');
                self.advance(2);
            } else if c == '{' {
                if !buf.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut buf)));
                }
                self.advance(1); // {
                let interior = self.scan_interp_tokens(triple)?;
                parts.push(Part::Interp(interior));
            } else {
                buf.push(c);
                self.advance(1);
            }
        }
        if !buf.is_empty() {
            parts.push(Part::Text(buf));
        }
        if triple {
            parts = strip_triple_indent(parts);
        }
        let outer = self.span_from(start, sl, sc);
        let text = self.slice(start, self.pos);
        self.emit_string_parts(parts, outer, text, triple);
        Ok(())
    }

    fn scan_escape(&mut self) -> Result<char, SyntaxError> {
        let (sl, sc) = (self.line, self.col);
        let start = self.pos;
        self.advance(1); // backslash
        let c = match self.cur() {
            None => {
                return Err(self.err(
                    "unterminated escape sequence",
                    self.span_from(start, sl, sc),
                    "",
                ));
            }
            Some(c) => c,
        };
        let mapped = match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '"' => '"',
            '\\' => '\\',
            '{' => '{',
            '}' => '}',
            'u' => return self.scan_unicode_escape(start, sl, sc),
            other => {
                return Err(self.err(
                    &format!("invalid escape sequence '\\{other}'"),
                    self.span_from(start, sl, sc),
                    "valid escapes: \\n \\t \\r \\\" \\\\ \\{ \\} \\u{...}",
                ));
            }
        };
        self.advance(1);
        Ok(mapped)
    }

    fn scan_unicode_escape(&mut self, start: usize, sl: u32, sc: u32) -> Result<char, SyntaxError> {
        self.advance(1); // u
        if self.cur() != Some('{') {
            return Err(self.err(
                "invalid unicode escape",
                self.span_from(start, sl, sc),
                "use \\u{1F333}",
            ));
        }
        self.advance(1); // {
        let hex_start = self.pos;
        while let Some(c) = self.cur() {
            if c.is_ascii_hexdigit() {
                self.advance(1);
            } else {
                break;
            }
        }
        let hex = self.slice(hex_start, self.pos);
        if hex.is_empty() || self.cur() != Some('}') {
            return Err(self.err(
                "invalid unicode escape",
                self.span_from(start, sl, sc),
                "use \\u{1F333}",
            ));
        }
        self.advance(1); // }
        let code = u32::from_str_radix(&hex, 16).map_err(|_| {
            self.err(
                "invalid unicode escape",
                self.span_from(start, sl, sc),
                "use \\u{1F333}",
            )
        })?;
        if code > 0x10FFFF {
            return Err(self.err(
                "unicode escape out of range",
                self.span_from(start, sl, sc),
                "code points must be <= U+10FFFF",
            ));
        }
        char::from_u32(code).ok_or_else(|| {
            self.err(
                "unicode escape out of range",
                self.span_from(start, sl, sc),
                "code points must be <= U+10FFFF",
            )
        })
    }

    fn scan_interp_tokens(&mut self, in_triple: bool) -> Result<Vec<Token>, SyntaxError> {
        if in_triple {
            self.triple_interp += 1;
        }
        let saved = std::mem::take(&mut self.tokens);
        let result = self.scan_interp_inner();
        let interior = std::mem::replace(&mut self.tokens, saved);
        if in_triple {
            self.triple_interp -= 1;
        }
        result.map(|()| interior)
    }

    fn scan_interp_inner(&mut self) -> Result<(), SyntaxError> {
        let mut brace_depth: i32 = 0;
        loop {
            self.skip_ws_comments()?;
            match self.cur() {
                None => {
                    return Err(self.err(
                        "unterminated interpolation",
                        self.here_span(),
                        "add a closing '}'",
                    ));
                }
                Some('}') if brace_depth == 0 => {
                    self.advance(1);
                    return Ok(());
                }
                _ => {
                    self.scan_main_token()?;
                    match self.tokens.last().map(|t| t.kind) {
                        Some(TokenKind::LBrace) => brace_depth += 1,
                        Some(TokenKind::RBrace) => brace_depth -= 1,
                        _ => {}
                    }
                }
            }
        }
    }

    fn emit_string_parts(&mut self, parts: Vec<Part>, span: Span, text: String, triple: bool) {
        let has_interp = parts.iter().any(|p| matches!(p, Part::Interp(_)));
        if !has_interp {
            let mut value = String::new();
            for p in &parts {
                if let Part::Text(s) = p {
                    value.push_str(s);
                }
            }
            self.tokens.push(Token::new(
                TokenKind::Str,
                TokenValue::Str(value),
                text,
                span,
            ));
            return;
        }
        let delim = if triple { "\"\"\"" } else { "\"" };
        self.tokens.push(Token::new(
            TokenKind::StrStart,
            TokenValue::None,
            delim,
            span.clone(),
        ));
        for p in parts {
            match p {
                Part::Text(s) => {
                    if !s.is_empty() {
                        self.tokens.push(Token::new(
                            TokenKind::StrChunk,
                            TokenValue::Str(s.clone()),
                            s,
                            span.clone(),
                        ));
                    }
                }
                Part::Interp(toks) => {
                    self.tokens.push(Token::new(
                        TokenKind::InterpStart,
                        TokenValue::None,
                        "{",
                        span.clone(),
                    ));
                    self.tokens.extend(toks);
                    self.tokens.push(Token::new(
                        TokenKind::InterpEnd,
                        TokenValue::None,
                        "}",
                        span.clone(),
                    ));
                }
            }
        }
        self.tokens
            .push(Token::new(TokenKind::StrEnd, TokenValue::None, delim, span));
    }

    // ---- operators ----

    fn scan_operator(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        let (sl, sc) = (self.line, self.col);
        let c0 = self.cur().unwrap();
        let c1 = self.at(self.pos + 1);
        let c2 = self.at(self.pos + 2);

        // 3-char
        if c0 == '.' && c1 == Some('.') && c2 == Some('=') {
            self.push_op(TokenKind::RangeEq, 3, start, sl, sc);
            return Ok(());
        }
        // 2-char
        if let Some(c1) = c1 {
            let two = [c0, c1];
            let kind = match two {
                ['=', '='] => Some(TokenKind::Eq),
                ['!', '='] => Some(TokenKind::Ne),
                ['<', '='] => Some(TokenKind::Le),
                ['>', '='] => Some(TokenKind::Ge),
                ['+', '='] => Some(TokenKind::PlusEq),
                ['-', '='] => Some(TokenKind::MinusEq),
                ['*', '='] => Some(TokenKind::StarEq),
                ['/', '='] => Some(TokenKind::SlashEq),
                ['-', '>'] => Some(TokenKind::Arrow),
                ['=', '>'] => Some(TokenKind::FatArrow),
                ['?', '.'] => Some(TokenKind::QDot),
                ['?', '?'] => Some(TokenKind::Coalesce),
                ['.', '.'] => Some(TokenKind::Range),
                ['|', '>'] => Some(TokenKind::Pipe),
                _ => None,
            };
            if let Some(k) = kind {
                self.push_op(k, 2, start, sl, sc);
                return Ok(());
            }
            if two == ['&', '&'] {
                self.push_kw_op("and", 2, start, sl, sc);
                return Ok(());
            }
            if two == ['|', '|'] {
                self.push_kw_op("or", 2, start, sl, sc);
                return Ok(());
            }
        }
        // 1-char
        let single = match c0 {
            '+' => Some(TokenKind::Plus),
            '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star),
            '/' => Some(TokenKind::Slash),
            '%' => Some(TokenKind::Percent),
            '<' => Some(TokenKind::Lt),
            '>' => Some(TokenKind::Gt),
            '=' => Some(TokenKind::Assign),
            '?' => Some(TokenKind::Question),
            '.' => Some(TokenKind::Dot),
            ',' => Some(TokenKind::Comma),
            ':' => Some(TokenKind::Colon),
            ';' => Some(TokenKind::Semi),
            '@' => Some(TokenKind::At),
            '(' => Some(TokenKind::LParen),
            ')' => Some(TokenKind::RParen),
            '[' => Some(TokenKind::LBrack),
            ']' => Some(TokenKind::RBrack),
            '{' => Some(TokenKind::LBrace),
            '}' => Some(TokenKind::RBrace),
            _ => None,
        };
        if let Some(k) = single {
            self.push_op(k, 1, start, sl, sc);
            return Ok(());
        }
        if c0 == '!' {
            self.push_kw_op("not", 1, start, sl, sc);
            return Ok(());
        }
        if c0 == '&' {
            return Err(self.err(
                "unexpected character '&'",
                self.here_span(),
                "did you mean '&&' (logical and)?",
            ));
        }
        if c0 == '|' {
            return Err(self.err(
                "unexpected character '|'",
                self.here_span(),
                "did you mean '|>' (pipe) or '||' (logical or)?",
            ));
        }
        Err(self.err(
            &format!("unexpected character {c0:?}"),
            self.here_span(),
            "",
        ))
    }

    fn push_op(&mut self, kind: TokenKind, len: usize, start: usize, sl: u32, sc: u32) {
        self.advance(len);
        let text = self.slice(start, self.pos);
        let sp = self.span_from(start, sl, sc);
        self.tokens
            .push(Token::new(kind, TokenValue::None, text, sp));
    }

    fn push_kw_op(&mut self, word: &str, len: usize, start: usize, sl: u32, sc: u32) {
        self.advance(len);
        let text = self.slice(start, self.pos);
        let sp = self.span_from(start, sl, sc);
        self.tokens.push(Token::new(
            TokenKind::Keyword,
            TokenValue::Str(word.to_string()),
            text,
            sp,
        ));
    }
}

// ---- free helpers ----

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_ident_cont(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// Parse `#!orchard <major>[.<minor>]` → `"major.minor"` (minor defaults 0).
fn parse_pragma(line: &str) -> Option<String> {
    let rest = line.strip_prefix('#')?.strip_prefix('!')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("orchard")?;
    let rest = rest.trim_start();
    let mut chars = rest.chars().peekable();
    let mut major = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            major.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if major.is_empty() {
        return None;
    }
    let mut minor = String::new();
    if chars.peek() == Some(&'.') {
        chars.next();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                minor.push(c);
                chars.next();
            } else {
                break;
            }
        }
    }
    if minor.is_empty() {
        minor.push('0');
    }
    Some(format!("{major}.{minor}"))
}

/// Swift-style common-leading-indent stripping for triple-quoted strings.
fn strip_triple_indent(parts: Vec<Part>) -> Vec<Part> {
    enum Item {
        Text(String),
        Interp(Vec<Token>),
    }
    // Build logical lines.
    let mut lines: Vec<Vec<Item>> = vec![vec![]];
    for part in parts {
        match part {
            Part::Text(s) => {
                let segs: Vec<&str> = s.split('\n').collect();
                for (i, seg) in segs.iter().enumerate() {
                    if i > 0 {
                        lines.push(vec![]);
                    }
                    if !seg.is_empty() {
                        lines
                            .last_mut()
                            .unwrap()
                            .push(Item::Text((*seg).to_string()));
                    }
                }
            }
            Part::Interp(toks) => lines.last_mut().unwrap().push(Item::Interp(toks)),
        }
    }

    let is_blank = |line: &Vec<Item>| {
        line.iter()
            .all(|it| matches!(it, Item::Text(s) if s.trim().is_empty()))
    };
    let leading_ws = |line: &Vec<Item>| -> usize {
        match line.first() {
            Some(Item::Text(s)) => s.chars().take_while(|c| *c == ' ' || *c == '\t').count(),
            _ => 0,
        }
    };

    // Drop bordering blank lines (only when >1 line).
    if lines.len() > 1 && is_blank(&lines[0]) {
        lines.remove(0);
    }
    if lines.len() > 1 && is_blank(lines.last().unwrap()) {
        lines.pop();
    }

    // Min leading ws over non-blank lines.
    let strip = lines
        .iter()
        .filter(|l| !is_blank(l))
        .map(leading_ws)
        .min()
        .unwrap_or(0);

    // Reconstruct.
    let mut out: Vec<Part> = Vec::new();
    let mut buf = String::new();
    for (li, line) in lines.into_iter().enumerate() {
        if li > 0 {
            buf.push('\n');
        }
        let mut first = true;
        for item in line {
            match item {
                Item::Text(s) => {
                    let s = if first {
                        strip_n_leading_ws(&s, strip)
                    } else {
                        s
                    };
                    buf.push_str(&s);
                }
                Item::Interp(toks) => {
                    out.push(Part::Text(std::mem::take(&mut buf)));
                    out.push(Part::Interp(toks));
                }
            }
            first = false;
        }
    }
    out.push(Part::Text(buf));
    if out
        .iter()
        .all(|p| matches!(p, Part::Text(s) if s.is_empty()))
        && out.len() == 1
    {
        return vec![Part::Text(String::new())];
    }
    out
}

fn strip_n_leading_ws(s: &str, n: usize) -> String {
    let mut removed = 0;
    let mut chars = s.chars().peekable();
    while removed < n {
        match chars.peek() {
            Some(' ') | Some('\t') => {
                chars.next();
                removed += 1;
            }
            _ => break,
        }
    }
    chars.collect()
}
