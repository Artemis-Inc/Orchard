//! Source spans. Every token, AST node, and IR node carries a [`Span`].
//!
//! Mirrors v2's `diagnostics.Span`: 1-based `line`/`col`, plus a closing
//! `end_line`/`end_col` and byte/char offsets `start`/`end` used by the
//! formatter to slice exact source lexemes. The IR JSON encodes only the
//! `{file, line, col}` triple (see [`Span::to_ir`]).

use serde::{Deserialize, Serialize};

/// A region of source text. `line`/`col` are 1-based; `start`/`end` are
/// character offsets into the source (for byte-exact formatter slicing).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub start: usize,
    pub end: usize,
}

impl Span {
    /// A full span with both endpoints and offsets.
    pub fn new(
        file: impl Into<String>,
        line: u32,
        col: u32,
        end_line: u32,
        end_col: u32,
        start: usize,
        end: usize,
    ) -> Self {
        Span {
            file: file.into(),
            line,
            col,
            end_line,
            end_col,
            start,
            end,
        }
    }

    /// A zero-width point span at `line:col` (offset `pos`).
    pub fn point(file: impl Into<String>, line: u32, col: u32, pos: usize) -> Self {
        let file = file.into();
        Span {
            file,
            line,
            col,
            end_line: line,
            end_col: col,
            start: pos,
            end: pos,
        }
    }

    /// The IR JSON form: the `{file, line, col}` triple only (no end position),
    /// matching v2's `span_to_dict`.
    pub fn to_ir(&self) -> IrSpan {
        IrSpan {
            file: self.file.clone(),
            line: self.line,
            col: self.col,
        }
    }
}

/// The reduced span shape serialized into the IR (`{file, line, col}` only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrSpan {
    pub file: String,
    pub line: u32,
    pub col: u32,
}
