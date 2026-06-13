//! Orchard 3.0 front end.
//!
//! This crate owns the source-facing layers: spans, diagnostics, tokens, the
//! lexer, the AST, the recursive-descent parser, and the canonical formatter.
//! It has no async or runtime dependencies so it is usable from every embedding
//! target (native, WASM, FFI) and from tests with zero weight.

pub mod ast;
pub mod diagnostics;
pub mod error;
pub mod format;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod tokens;

pub use diagnostics::{suggest, Diagnostic, Severity};
pub use error::SyntaxError;
pub use format::format_source;
pub use lexer::tokenize;
pub use parser::parse_source;
pub use span::{IrSpan, Span};
pub use tokens::{Token, TokenKind, TokenValue};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_with_span_matches_v2_format() {
        let src = "agent X {\n    model { provder: mock }\n}\n";
        let span = Span::new("demo.orch", 2, 13, 2, 20, 0, 0);
        let d = Diagnostic::error("model: unknown key 'provder'", Some(span))
            .with_hint(suggest("provder", ["provider", "name", "temperature"]));
        let out = d.render(src);
        let expected = "error: model: unknown key 'provder'\n \u{250c}\u{2500} demo.orch:2:13\n \u{2502}\n2\u{2502}     model { provder: mock }\n \u{2502}             ^ did you mean 'provider'?";
        assert_eq!(out, expected);
    }

    #[test]
    fn render_without_span_is_header_only() {
        let d = Diagnostic::warning("no span here", None);
        assert_eq!(d.render(""), "warning: no span here");
    }

    #[test]
    fn suggest_picks_closest() {
        assert_eq!(
            suggest("get_wether", ["get_weather", "post"]),
            "did you mean 'get_weather'?"
        );
        assert_eq!(suggest("zzzzzz", ["get_weather", "post"]), "");
    }
}
