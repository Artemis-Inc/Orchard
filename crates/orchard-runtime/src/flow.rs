//! Control flow. The interpreter threads a [`Flow`] through every eval/exec
//! instead of using exceptions (v2's signal classes). Only [`Flow::Throw`] is
//! caught by `try/catch`.

use crate::value::Value;

#[derive(Clone, Debug)]
pub enum Flow {
    /// Normal completion with a value.
    Value(Value),
    /// `return` — unwinds to the nearest fn/skill/handler (through lambdas).
    Return(Value),
    /// `reply` — unwinds to the handler.
    Reply(Value),
    /// `break` — unwinds to the nearest loop.
    Break,
    /// `continue` — unwinds to the nearest loop.
    Continue,
    /// `halt` — unwinds to the turn boundary (commits state).
    Halt(String),
    /// A catchable error (the Error record value).
    Throw(Value),
}

impl Flow {
    pub fn null() -> Flow {
        Flow::Value(Value::Null)
    }
    /// True for any non-normal variant (propagates through blocks).
    pub fn is_signal(&self) -> bool {
        !matches!(self, Flow::Value(_))
    }
    /// Extract a normal value, or `Null` for any signal (used where a block's
    /// value is needed but signals propagate separately).
    pub fn value_or_null(self) -> Value {
        match self {
            Flow::Value(v) => v,
            _ => Value::Null,
        }
    }
}
