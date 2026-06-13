//! Token kinds and the [`Token`] type.
//!
//! Ports v2's `tokens.py`. `value` is the decoded semantic payload; `text` is
//! the raw source lexeme (used by the formatter for byte-exact slicing and by
//! golden dumps). Comments/whitespace are not tokens.

use crate::span::Span;

/// The lexical category of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // literals
    Int,
    Float,
    Str,
    RawString,
    Duration,
    Money,
    True,
    False,
    Null,
    // interpolated-string sequence
    StrStart,
    StrChunk,
    InterpStart,
    InterpEnd,
    StrEnd,
    // names
    Ident,
    Keyword,
    // operators / punctuation
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    Arrow,
    FatArrow,
    Question,
    QDot,
    Coalesce,
    Range,
    RangeEq,
    Pipe,
    Dot,
    Comma,
    Colon,
    Semi,
    LParen,
    RParen,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    At,
    // structural / virtual
    Pragma,
    Newline,
    Eof,
}

impl TokenKind {
    /// The uppercase name used in golden token dumps (mirrors v2's kind names).
    pub fn name(self) -> &'static str {
        use TokenKind::*;
        match self {
            Int => "INT",
            Float => "FLOAT",
            Str => "STRING",
            RawString => "RAWSTRING",
            Duration => "DURATION",
            Money => "MONEY",
            True => "TRUE",
            False => "FALSE",
            Null => "NULL",
            StrStart => "STR_START",
            StrChunk => "STR_CHUNK",
            InterpStart => "INTERP_START",
            InterpEnd => "INTERP_END",
            StrEnd => "STR_END",
            Ident => "IDENT",
            Keyword => "KEYWORD",
            Plus => "PLUS",
            Minus => "MINUS",
            Star => "STAR",
            Slash => "SLASH",
            Percent => "PERCENT",
            Eq => "EQ",
            Ne => "NE",
            Lt => "LT",
            Le => "LE",
            Gt => "GT",
            Ge => "GE",
            Assign => "ASSIGN",
            PlusEq => "PLUSEQ",
            MinusEq => "MINUSEQ",
            StarEq => "STAREQ",
            SlashEq => "SLASHEQ",
            Arrow => "ARROW",
            FatArrow => "FATARROW",
            Question => "QUESTION",
            QDot => "QDOT",
            Coalesce => "COALESCE",
            Range => "RANGE",
            RangeEq => "RANGEEQ",
            Pipe => "PIPE",
            Dot => "DOT",
            Comma => "COMMA",
            Colon => "COLON",
            Semi => "SEMI",
            LParen => "LPAREN",
            RParen => "RPAREN",
            LBrack => "LBRACK",
            RBrack => "RBRACK",
            LBrace => "LBRACE",
            RBrace => "RBRACE",
            At => "AT",
            Pragma => "PRAGMA",
            Newline => "NEWLINE",
            Eof => "EOF",
        }
    }
}

/// The decoded semantic value carried by a token.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenValue {
    None,
    Int(i64),
    Float(f64),
    /// Decoded string content, or the name for `Ident`/`Keyword`, or the pragma
    /// version.
    Str(String),
    /// `(count, unit)` for a duration literal.
    Duration(i64, String),
    /// The amount string for a money literal (e.g. `"0.50"`) — kept as text to
    /// avoid float rounding.
    Money(String),
    Bool(bool),
}

/// A lexed token.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub value: TokenValue,
    pub text: String,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, value: TokenValue, text: impl Into<String>, span: Span) -> Self {
        Token {
            kind,
            value,
            text: text.into(),
            span,
        }
    }

    /// Keyword tokens carry their (possibly normalized) word.
    pub fn keyword_word(&self) -> Option<&str> {
        if self.kind == TokenKind::Keyword {
            if let TokenValue::Str(s) = &self.value {
                return Some(s.as_str());
            }
        }
        None
    }

    /// A deterministic golden-dump line for this token. Equivalently-structured
    /// to v2's `repr(Token)` (Rust string escaping rather than Python's).
    pub fn dump(&self) -> String {
        match self.kind {
            TokenKind::Newline => "NEWLINE".to_string(),
            TokenKind::Eof => "EOF".to_string(),
            _ => {
                let text = format!("{:?}", self.text);
                let value_redundant = matches!(&self.value, TokenValue::None)
                    || matches!(&self.value, TokenValue::Str(s) if *s == self.text);
                if value_redundant {
                    format!("{}({})", self.kind.name(), text)
                } else {
                    format!(
                        "{}({}, {})",
                        self.kind.name(),
                        text,
                        dump_value(&self.value)
                    )
                }
            }
        }
    }
}

fn dump_value(v: &TokenValue) -> String {
    match v {
        TokenValue::None => "None".to_string(),
        TokenValue::Int(i) => i.to_string(),
        TokenValue::Float(f) => format!("{f:?}"),
        TokenValue::Str(s) => format!("{s:?}"),
        TokenValue::Duration(c, u) => format!("({}, {:?})", c, u),
        TokenValue::Money(s) => format!("{s:?}"),
        TokenValue::Bool(b) => b.to_string(),
    }
}

/// Reserved keywords (may not be identifiers). `true`/`false`/`null` are
/// deliberately excluded — they lex to `True`/`False`/`Null`.
pub const KEYWORDS: &[&str] = &[
    "agent",
    "model",
    "memory",
    "persona",
    "knowledge",
    "policy",
    "use",
    "as",
    "state",
    "type",
    "enum",
    "fn",
    "tool",
    "skill",
    "on",
    "let",
    "var",
    "if",
    "else",
    "match",
    "for",
    "in",
    "while",
    "repeat",
    "return",
    "break",
    "continue",
    "try",
    "catch",
    "throw",
    "gen",
    "delegate",
    "spawn",
    "await",
    "parallel",
    "retry",
    "until",
    "budget",
    "remember",
    "recall",
    "forget",
    "reply",
    "emit",
    "halt",
    "and",
    "or",
    "not",
    "with",
    "this",
];

/// Duration unit suffixes, longest-disambiguated by set membership on the whole
/// glued unit (so `30min` → unit `"min"` → error, not `30m` + `in`).
pub const DURATION_UNITS: &[&str] = &["ms", "s", "m", "h", "d"];

pub fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(&word)
}
