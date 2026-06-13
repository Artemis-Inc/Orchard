//! Orchard 3.0 runtime.
//!
//! The heavy crate: the [`value`] model, the [`flow`] control enum, the
//! tree-walking async engine, the two verbs (`gen`/`delegate`), `gen as T`
//! coercion, memory/[`traits::Store`], embeddings, the policy engine,
//! taint/redaction, the egress-guarded HTTP client, tools, the MCP client,
//! providers, and the executor abstraction.
//!
//! Built out across P5–P13. `native` (default) pulls tokio/reqwest/redb;
//! `wasm` swaps in a cooperative executor + in-memory store.

pub mod agent;
pub mod coerce;
pub mod cron;
pub mod embeddings;
pub mod engine;
pub mod error;
pub mod flow;
pub mod http;
pub mod policy;
pub mod pricing;
pub mod providers;
pub mod secrets;
pub mod store;
pub mod tools;
pub mod traits;
pub mod value;

pub use agent::AgentRuntime;
pub use cron::{civil_from_unix, matches as cron_matches, validate as cron_validate};
pub use embeddings::{chunk_text, content_hash, semantic_search, KeywordScorer};
pub use engine::Engine;
pub use error::{HostError, HttpError, ProviderError, ToolError};
pub use flow::Flow;
#[cfg(feature = "native")]
pub use http::ReqwestClient;
pub use http::{check_egress, host_is_private};
pub use policy::PolicyEngine;
pub use providers::{get_provider, MockProvider};
pub use secrets::Environment;
pub use store::InMemoryStore;
#[cfg(feature = "native")]
pub use store::RedbStore;
pub use tools::{build_pack_tools, NativeTool, PackCtx};
pub use traits::HttpClient as HttpClientTrait;
pub use traits::{
    ChatRequest, ChatResponse, Clock, Embedder, HttpClient, HttpRequest, HttpResponse, Message,
    Provider, Store, SystemClock, Tool, ToolCall, ToolDef,
};
pub use value::{equal, make_error, Closure, Duration, Env, Money, Scope, Value};

/// The Orchard version string emitted in the IR and by `orch --version`.
pub const ORCHARD_VERSION: &str = "3.0";

/// Untrusted-content sentinels wrapping external tool output and recalled memory.
pub const SENTINEL_OPEN: &str = "<<<external>>>";
pub const SENTINEL_CLOSE: &str = "<<<end-external>>>";
/// Max bytes of a tool result before truncation.
pub const TOOL_RESULT_MAX_BYTES: usize = 48 * 1024;

#[cfg(test)]
mod tests {
    use super::value::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn money_is_exact_and_preserves_text() {
        let m = Money::from_text("5.50");
        assert_eq!(m.display(), "$5.50");
        assert_eq!(m.amount, Decimal::from_str("5.50").unwrap());
        // equality is by value: $5.50 == $5.5
        assert_eq!(Money::from_text("5.50"), Money::from_text("5.5"));
    }

    #[test]
    fn bool_is_never_a_number() {
        assert!(!equal(&Value::Bool(true), &Value::Int(1)));
        assert!(!equal(&Value::Int(0), &Value::Bool(false)));
        assert!(equal(&Value::Bool(true), &Value::Bool(true)));
        assert!(equal(&Value::Int(1), &Value::Float(1.0)));
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Null.truthy());
        assert!(!Value::Int(0).truthy());
        assert!(!Value::Str(String::new()).truthy());
        assert!(Value::Money(Money::from_text("0")).truthy()); // money always truthy
        assert!(Value::Int(3).truthy());
    }

    #[test]
    fn text_conversion() {
        assert_eq!(Value::Null.to_text(), "null");
        assert_eq!(Value::Bool(true).to_text(), "true");
        assert_eq!(Value::Float(1.0).to_text(), "1.0");
        assert_eq!(Value::Money(Money::from_text("0.50")).to_text(), "$0.50");
        assert_eq!(Value::Duration(Duration::new(30, "m")).to_text(), "30m");
        let list = Value::List(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(list.to_text(), "[1,2]"); // compact JSON
    }

    #[test]
    fn duration_seconds() {
        assert_eq!(Duration::new(1, "m").seconds(), 60.0);
        assert_eq!(Duration::new(2, "h").seconds(), 7200.0);
        assert_eq!(Duration::parse("30m").seconds(), 1800.0);
        // 60s == 1m
        assert_eq!(Duration::new(60, "s"), Duration::new(1, "m"));
    }

    #[test]
    fn scope_chain() {
        let root = Scope::root();
        root.define("a", Value::Int(1));
        let child = root.child();
        child.define("b", Value::Int(2));
        assert!(child.has("a"));
        assert!(matches!(child.get("a"), Some(Value::Int(1))));
        assert!(child.assign("a", Value::Int(9)));
        assert!(matches!(root.get("a"), Some(Value::Int(9)))); // assigned in defining scope
        assert!(!child.assign("missing", Value::Int(0)));
    }
}
