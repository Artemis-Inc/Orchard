//! Inject a custom Provider and drive a delegate turn through it (the embedding
//! deliverable: route model calls through a host-supplied provider).

use async_trait::async_trait;
use orchard::{Agent, ChatRequest, ChatResponse, ProviderError, ProviderTrait, Runtime, ToolCall};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A scripted fake provider: turn 1 calls a tool, turn 2 returns text.
struct FakeProvider {
    turn: AtomicUsize,
}

#[async_trait]
impl ProviderTrait for FakeProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "calculate".into(),
                    args: Some(serde_json::json!({"expression": "2 + 2"})),
                    raw_args: "{}".into(),
                }],
                stop_reason: "tool_use".into(),
                ..Default::default()
            })
        } else {
            Ok(ChatResponse {
                text: "the answer is 4".into(),
                stop_reason: "stop".into(),
                ..Default::default()
            })
        }
    }
}

#[tokio::test]
async fn custom_provider_drives_delegate() {
    let src = "agent A { model { provider: anthropic, name: \"claude-opus-4-8\" } use calculator\n on message(text: str) -> str { return delegate text } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let provider = Arc::new(FakeProvider {
        turn: AtomicUsize::new(0),
    });
    let session = Runtime::builder(agent).provider(provider).build().unwrap();
    let out = session.message("what is 2+2").await.unwrap();
    assert_eq!(out, "the answer is 4");
}
