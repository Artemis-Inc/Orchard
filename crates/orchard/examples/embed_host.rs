//! A Rust host embedding Orchard: register a native tool + a custom provider,
//! then drive a `delegate` turn that calls the tool. Fully offline.
//!
//!     cargo run -p orchard --example embed_host

use async_trait::async_trait;
use orchard::{
    Agent, ChatRequest, ChatResponse, NativeTool, ProviderError, ProviderTrait, Runtime, ToolCall,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A host-supplied provider: turn 1 calls our native tool, turn 2 answers.
struct HostGateway {
    turn: AtomicUsize,
}

#[async_trait]
impl ProviderTrait for HostGateway {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        if self.turn.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "lookup_user".into(),
                    args: Some(serde_json::json!({ "id": 7 })),
                    raw_args: "{}".into(),
                }],
                stop_reason: "tool_use".into(),
                ..Default::default()
            })
        } else {
            Ok(ChatResponse {
                text: "User 7 is Ada Lovelace.".into(),
                stop_reason: "stop".into(),
                ..Default::default()
            })
        }
    }
}

#[tokio::main]
async fn main() {
    let src = r#"
        agent Concierge {
            model { provider: anthropic, name: "claude-opus-4-8" }
            on message(text: str) -> str { return delegate text }
        }
    "#;

    // A native Rust tool the delegate loop can call.
    let lookup = NativeTool::builder("lookup_user")
        .description("Look up a user by id in the host database.")
        .param("id", "integer", true)
        .handler(|args| async move {
            let id = args.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            // ... a real host would query its database here ...
            Ok(serde_json::json!({ "id": id, "name": "Ada Lovelace" }))
        });

    let agent = Agent::load(src, "concierge.orch").expect("valid agent");
    let session = Runtime::builder(agent)
        .provider(Arc::new(HostGateway {
            turn: AtomicUsize::new(0),
        }))
        .register_tool(lookup)
        .build()
        .expect("session");

    let reply = session.message("who is user 7?").await.expect("turn");
    println!("{reply}");
    assert!(reply.contains("Ada Lovelace"));
}
