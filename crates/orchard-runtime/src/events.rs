//! Real-time agent events.
//!
//! The engine fires these as a run unfolds, so a host (the `orch` TUI, a desktop
//! app, a web UI) can render the live pipeline: model calls, tool calls and their
//! results, `emit` output, token usage, and the final answer. A host installs a
//! sink with `RuntimeBuilder::on_event`. When no sink is installed the engine
//! does no extra work.

use serde_json::Value as Json;
use std::sync::Arc;

/// Which model verb produced a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    /// A step of the autonomous `delegate` loop.
    Delegate,
    /// A single `gen` / `gen as T` generation.
    Gen,
}

/// A single thing that happened during a run, in order.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// A model request is about to be sent.
    ModelStart {
        model: String,
        messages: usize,
        tools: usize,
        kind: ModelKind,
    },
    /// A text delta streamed from the model (token-level streaming).
    Token { text: String },
    /// A model response arrived.
    ModelEnd {
        model: String,
        input_tokens: i64,
        output_tokens: i64,
        stop_reason: String,
        tool_calls: usize,
        /// The assistant text (redacted), if any.
        text: String,
    },
    /// A tool is about to run.
    ToolStart {
        id: String,
        name: String,
        args: Json,
    },
    /// A tool finished (or failed).
    ToolEnd {
        id: String,
        name: String,
        ok: bool,
        /// The tool output or error message (redacted, may be truncated by the host).
        output: String,
        ms: u64,
    },
    /// An `emit "..."` from agent code (intermediate streamed output).
    Emit { text: String },
    /// The `delegate` loop produced its final result.
    TaskComplete { result: String },
    /// A non-fatal notice (policy downgrade, retry, stopped-by-policy, ...).
    Notice { level: String, text: String },
}

/// A host-installed event observer. Cheap to clone; called from the async engine.
pub type EventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;
