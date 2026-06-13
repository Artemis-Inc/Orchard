//! Lexer behavior tests, mirroring v2's `test_lexer.py` (equivalently-structured
//! golden dumps rather than Python `repr`).

use orchard_syntax::tokens::TokenKind;
use orchard_syntax::{tokenize, TokenValue};

fn dump(src: &str) -> String {
    let toks = tokenize(src, "<t>").expect("lex ok");
    toks.iter().map(|t| t.dump()).collect::<Vec<_>>().join("\n")
}

fn kinds(src: &str) -> Vec<TokenKind> {
    tokenize(src, "<t>")
        .unwrap()
        .into_iter()
        .map(|t| t.kind)
        .collect()
}

#[test]
fn pragma_and_keywords() {
    let toks = tokenize("#!orchard 3.0\nagent Echo {}", "<t>").unwrap();
    assert_eq!(toks[0].kind, TokenKind::Pragma);
    assert_eq!(toks[0].value, TokenValue::Str("3.0".into()));
    // v2 emits a NEWLINE after the pragma line.
    assert_eq!(toks[1].kind, TokenKind::Newline);
    assert_eq!(toks[2].kind, TokenKind::Keyword);
    assert_eq!(toks[2].keyword_word(), Some("agent"));
    assert_eq!(toks[3].kind, TokenKind::Ident);
}

#[test]
fn numbers_durations_money() {
    let toks = tokenize("1_000_000 1.0e9 1e9 30s 15m 2h 500ms $0.50 $1", "<t>").unwrap();
    assert_eq!(toks[0].value, TokenValue::Int(1_000_000));
    assert_eq!(toks[1].value, TokenValue::Float(1_000_000_000.0));
    assert_eq!(toks[2].value, TokenValue::Float(1_000_000_000.0));
    assert_eq!(toks[3].value, TokenValue::Duration(30, "s".into()));
    assert_eq!(toks[4].value, TokenValue::Duration(15, "m".into()));
    assert_eq!(toks[5].value, TokenValue::Duration(2, "h".into()));
    assert_eq!(toks[6].value, TokenValue::Duration(500, "ms".into()));
    assert_eq!(toks[7].value, TokenValue::Money("0.50".into()));
    assert_eq!(toks[8].value, TokenValue::Money("1".into()));
}

#[test]
fn range_is_not_a_float() {
    // `1..5` lexes INT RANGE INT, not a malformed float.
    assert_eq!(
        kinds("1..5"),
        vec![
            TokenKind::Int,
            TokenKind::Range,
            TokenKind::Int,
            TokenKind::Newline,
            TokenKind::Eof
        ]
    );
}

#[test]
fn plain_string_is_one_token() {
    let toks = tokenize("\"qwen3\"", "<t>").unwrap();
    assert_eq!(toks[0].kind, TokenKind::Str);
    assert_eq!(toks[0].value, TokenValue::Str("qwen3".into()));
}

#[test]
fn interpolation_token_sequence() {
    let toks = tokenize("\"Hi {name}, {n} msgs\"", "<t>").unwrap();
    let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind).take(9).collect();
    use TokenKind::*;
    assert_eq!(
        ks,
        vec![
            StrStart,
            StrChunk,
            InterpStart,
            Ident,
            InterpEnd,
            StrChunk,
            InterpStart,
            Ident,
            InterpEnd
        ]
    );
}

#[test]
fn nested_braces_in_interpolation_are_balanced() {
    // `{f({a: 1})}` — the interp closes at the matching brace, not the inner map's.
    let toks = tokenize("\"v {f({a: 1})}\"", "<t>").unwrap();
    assert_eq!(toks[0].kind, TokenKind::StrStart);
    assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
    // There must be exactly one INTERP_START / INTERP_END pair at this level.
    let starts = toks
        .iter()
        .filter(|t| t.kind == TokenKind::InterpStart)
        .count();
    assert_eq!(starts, 1);
}

#[test]
fn literal_braces_escape() {
    let toks = tokenize("\"literal {{ and }} braces\"", "<t>").unwrap();
    assert_eq!(toks[0].kind, TokenKind::Str);
    assert_eq!(
        toks[0].value,
        TokenValue::Str("literal { and } braces".into())
    );
}

#[test]
fn escapes_decode() {
    let toks = tokenize("\"tab\\tnl\\nq\\\"slash\\\\brace\\{ u\\u{1F333}\"", "<t>").unwrap();
    assert_eq!(
        toks[0].value,
        TokenValue::Str("tab\tnl\nq\"slash\\brace{ u🌳".into())
    );
}

#[test]
fn raw_string_keeps_backslash() {
    let toks = tokenize("`{\"json\": \"no \\n escape {braces}\"}`", "<t>").unwrap();
    assert_eq!(toks[0].kind, TokenKind::RawString);
    assert_eq!(
        toks[0].value,
        TokenValue::Str("{\"json\": \"no \\n escape {braces}\"}".into())
    );
}

#[test]
fn triple_quote_strips_common_indent() {
    let src = "\"\"\"\n    line one\n      line two\n    \"\"\"";
    let toks = tokenize(src, "<t>").unwrap();
    assert_eq!(toks[0].kind, TokenKind::Str);
    assert_eq!(
        toks[0].value,
        TokenValue::Str("line one\n  line two".into())
    );
}

#[test]
fn asi_inserts_newline_between_statements() {
    let ks = kinds("let a = 1\nlet b = 2");
    assert_eq!(ks.iter().filter(|k| **k == TokenKind::Newline).count(), 2); // between + trailing
}

#[test]
fn asi_suppressed_after_operator_and_before_else() {
    // trailing operator suppresses
    let ks = kinds("a +\nb");
    assert_eq!(ks.iter().filter(|k| **k == TokenKind::Newline).count(), 1); // only trailing
                                                                            // `}` then `else` suppresses
    let src = "if x {\n}\nelse {\n}";
    let toks = tokenize(src, "<t>").unwrap();
    // No NEWLINE directly before the `else` keyword.
    for w in toks.windows(2) {
        if w[1].keyword_word() == Some("else") {
            assert_ne!(w[0].kind, TokenKind::Newline);
        }
    }
}

#[test]
fn operators_maximal_munch() {
    use TokenKind::*;
    let ks = kinds("..= .. ?. ?? <= >= == != -> => |> += -= *= /=");
    let expected = [
        RangeEq, Range, QDot, Coalesce, Le, Ge, Eq, Ne, Arrow, FatArrow, Pipe, PlusEq, MinusEq,
        StarEq, SlashEq, Newline, Eof,
    ];
    assert_eq!(ks, expected);
}

#[test]
fn logical_aliases_normalize() {
    let toks = tokenize("a && b || !c", "<t>").unwrap();
    assert_eq!(toks[1].keyword_word(), Some("and"));
    assert_eq!(toks[1].text, "&&"); // raw lexeme preserved for diagnostics
    assert_eq!(toks[3].keyword_word(), Some("or"));
    assert_eq!(toks[4].keyword_word(), Some("not"));
}

#[test]
fn nested_block_comments() {
    let ks = kinds("a /* outer /* inner */ still */ b");
    assert_eq!(
        ks,
        vec![
            TokenKind::Ident,
            TokenKind::Ident,
            TokenKind::Newline,
            TokenKind::Eof
        ]
    );
}

// ---- error cases ----

fn err(src: &str) -> String {
    let e = tokenize(src, "<t>").unwrap_err();
    e.diagnostic.message
}

#[test]
fn lex_errors() {
    assert!(err("2x").contains("number immediately followed by identifier"));
    assert!(err("1.5h").contains("number immediately followed by an identifier"));
    assert!(err("\"unterminated").contains("unterminated string"));
    assert!(err("`unterminated").contains("unterminated raw string"));
    assert!(err("/* unclosed").contains("unterminated block comment"));
    assert!(err("$x").contains("expected digits after '$'"));
    assert!(err("\"bad \\q\"").contains("invalid escape"));
    assert!(err("\"open {x").contains("unterminated interpolation"));
}

#[test]
fn dump_format_is_stable() {
    let d = dump("agent Echo {\n  on message(text: str) -> str { gen \"hi {text}\" }\n}");
    assert!(d.contains("KEYWORD(\"agent\")"));
    assert!(d.contains("IDENT(\"Echo\")"));
    assert!(d.contains("STR_START"));
}
