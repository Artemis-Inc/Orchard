//! Runtime error types. [`HostError`] is the engine-level error that bypasses
//! `try/catch` (v2's `OrchardError`/`EnvError`) and triggers state rollback;
//! catchable language errors travel as [`crate::flow::Flow::Throw`].

use thiserror::Error;

/// An error that ends the run (not catchable by `try/catch`).
#[derive(Debug, Clone, Error)]
pub enum HostError {
    #[error("environment variable not set: {0}")]
    Env(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("run stopped by policy: {0}")]
    Policy(String),
    #[error("store error: {0}")]
    Store(String),
    #[error("halted: {0}")]
    Halt(String),
    #[error("{0}")]
    Internal(String),
}

/// A provider call failure (retryable or not).
#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct ProviderError {
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        ProviderError {
            message: message.into(),
            retryable,
        }
    }
}

/// A tool failure surfaced to the model as a structured error (never crashes the
/// loop).
#[derive(Debug, Clone, Error)]
#[error("{0}")]
pub struct ToolError(pub String);

impl ToolError {
    pub fn new(msg: impl Into<String>) -> Self {
        ToolError(msg.into())
    }
}

/// An HTTP/egress failure.
#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct HttpError {
    pub message: String,
    pub status: Option<u16>,
}

impl HttpError {
    pub fn new(message: impl Into<String>) -> Self {
        HttpError {
            message: message.into(),
            status: None,
        }
    }
    pub fn with_status(message: impl Into<String>, status: u16) -> Self {
        HttpError {
            message: message.into(),
            status: Some(status),
        }
    }
}
