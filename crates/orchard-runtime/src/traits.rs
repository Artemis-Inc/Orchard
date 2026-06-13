//! The injectable boundary traits: [`Provider`], [`Tool`], [`Store`],
//! [`HttpClient`], [`Embedder`], [`Clock`]. Hosts supply implementations; the
//! runtime ships pure-Rust defaults. (The `Executor` abstraction lands with
//! concurrency in P9.)

use crate::error::{HttpError, ProviderError, ToolError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::time::{SystemTime, UNIX_EPOCH};

// ---- provider ----

/// A normalized chat message (provider-agnostic).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Message {
        Message {
            role: "user".into(),
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn assistant(content: impl Into<String>) -> Message {
        Message {
            role: "assistant".into(),
            content: content.into(),
            ..Default::default()
        }
    }
}

/// A model-requested tool call. `args = None` means the model emitted malformed
/// JSON arguments (kept in `raw_args`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Option<Json>,
    #[serde(default)]
    pub raw_args: String,
}

/// A tool advertised to the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Json,
}

/// A single chat request to a provider.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    /// JSON Schema for native structured output (capability-gated).
    pub schema: Option<Json>,
}

/// A provider response.
#[derive(Clone, Debug, Default)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model: String,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn supports_schema(&self) -> bool {
        false
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;
    fn describe(&self) -> String {
        "provider".to_string()
    }
}

// ---- tool ----

/// A callable tool (built-in pack, custom declarative, MCP, or native host fn).
/// Tools capture whatever context they need at construction.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> &Json;
    /// External tools have their output sentinel-wrapped before the model sees it.
    fn external(&self) -> bool {
        false
    }
    async fn call(&self, args: Json) -> Result<Json, ToolError>;
}

// ---- store ----

/// A semantic-search hit: `(text, score, source)`.
pub type Hit = (String, f64, String);

/// The persistent store (conversation/facts/state/semantic/trace). The default
/// is `redb` (native) or in-memory (WASM/tests); a host may inject its own.
/// Implementations serialize all access internally (single-writer).
pub trait Store: Send + Sync {
    // conversation
    fn append_message(&self, role: &str, content: &Json);
    fn append_messages(&self, items: &[(String, Json)]);
    fn window(&self, n: i64) -> Vec<Json>;
    fn message_count(&self) -> i64;
    fn messages_before_window(&self, n: i64) -> Vec<Json>;
    fn clear_conversation(&self);
    fn archived_count(&self) -> i64;
    fn set_archived_count(&self, count: i64);

    // facts
    fn remember(&self, key: &str, value: &str);
    fn recall(&self, query: &str) -> Vec<(String, String)>;
    fn forget(&self, key: &str) -> bool;
    fn all_facts(&self) -> Vec<(String, String)>;
    fn clear_facts(&self);

    // typed state (separate namespace)
    fn get_state(&self, key: &str) -> Option<Json>;
    fn get_all_state(&self) -> Vec<(String, Json)>;
    fn set_state_batch(&self, items: &[(String, Json)]);
    fn clear_state(&self);

    // semantic index
    fn add_chunks(
        &self,
        source: &str,
        content_hash: &str,
        texts: &[String],
        vectors: Option<&[Vec<f32>]>,
    );
    fn search_vec(&self, vector: &[f32], top_k: i64) -> Vec<Hit>;
    fn all_chunks(&self) -> Vec<(i64, String, String)>;
    fn has_source(&self, source: &str, content_hash: &str) -> bool;
    fn delete_source(&self, source: &str);
    fn chunk_count(&self) -> i64;
    fn index_meta(&self) -> Option<(String, String, usize)>;
    fn set_index_meta(&self, provider: &str, model: &str, dim: usize);

    // trace
    fn trace_event(&self, run_id: &str, kind: &str, payload: &Json);
    fn last_run_id(&self) -> Option<String>;
    fn run_trace(&self, run_id: &str) -> Vec<Json>;

    // bookkeeping
    fn clear_all(&self);
}

// ---- http ----

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout_secs: u64,
    pub allowed_domains: Vec<String>,
    pub allow_local: bool,
    pub enforce_egress: bool,
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
}

// ---- embedder ----

#[async_trait]
pub trait Embedder: Send + Sync {
    fn provider(&self) -> &str;
    fn dim(&self) -> Option<usize>;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderError>;
}

// ---- clock ----

/// A swappable time source (so tests/WASM can control time).
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
    fn now_millis(&self) -> u128 {
        self.now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }
}

/// The default system clock.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}
