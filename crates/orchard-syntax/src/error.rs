//! Syntax errors: a single [`Diagnostic`] raised by the lexer or parser
//! (v2's `OrchardSyntaxError`). The front end is single-error-then-bail.

use crate::diagnostics::Diagnostic;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub diagnostic: Diagnostic,
}

impl SyntaxError {
    pub fn new(diagnostic: Diagnostic) -> Self {
        SyntaxError { diagnostic }
    }
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.diagnostic.severity.as_str(),
            self.diagnostic.message
        )
    }
}

impl std::error::Error for SyntaxError {}
