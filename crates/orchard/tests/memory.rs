//! Semantic recall (knowledge → delegate context) + conversation-window memory.

use async_trait::async_trait;
use orchard::{Agent, ChatRequest, ChatResponse, ProviderError, ProviderTrait, Runtime};
use orchard_runtime::InMemoryStore;
use std::sync::{Arc, Mutex};

/// Captures every user message it sees, then returns a fixed reply.
struct Capturing {
    seen: Mutex<Vec<String>>,
    reply: String,
}

#[async_trait]
impl ProviderTrait for Capturing {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let users: Vec<String> = req
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.clone())
            .collect();
        self.seen.lock().unwrap().push(users.join("\n"));
        Ok(ChatResponse {
            text: self.reply.clone(),
            stop_reason: "stop".into(),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn knowledge_is_recalled_into_delegate_context() {
    let src = r#"
        agent Fern {
            model { provider: anthropic, name: "claude-opus-4-8" }
            memory { facts: true, semantic { enabled: true, embeddings { provider: none } } }
            knowledge { text: "The office wifi password is hunter2 and lives in 1Password." }
            on message(text: str) -> str { return delegate text }
        }
    "#;
    let agent = Agent::load(src, "<t>").unwrap();
    let provider = Arc::new(Capturing {
        seen: Mutex::new(vec![]),
        reply: "done".into(),
    });
    let session = Runtime::builder(agent)
        .provider(provider.clone())
        .build()
        .unwrap();
    let _ = session.message("what is the wifi password?").await.unwrap();
    let seen = provider.seen.lock().unwrap().join("\n");
    assert!(
        seen.contains("wifi password is hunter2"),
        "knowledge not recalled: {seen}"
    );
    assert!(
        seen.contains("<<<external>>>"),
        "recall not sentinel-wrapped: {seen}"
    );
}

#[tokio::test]
async fn conversation_window_carries_across_turns() {
    let store = Arc::new(InMemoryStore::new());
    let src = r#"
        agent Chat {
            model { provider: anthropic, name: "claude-opus-4-8" }
            memory { conversation { enabled: true, window: 40 } }
            on message(text: str) -> str { return delegate text }
        }
    "#;
    let agent = Agent::load(src, "<t>").unwrap();
    let provider = Arc::new(Capturing {
        seen: Mutex::new(vec![]),
        reply: "ok".into(),
    });
    let session = Runtime::builder(agent)
        .provider(provider.clone())
        .store(store)
        .build()
        .unwrap();
    session.message("my name is Ada").await.unwrap();
    session.message("what is my name?").await.unwrap();
    // The second turn's context includes the first turn (user + assistant).
    let second = provider.seen.lock().unwrap()[1].clone();
    assert!(
        second.contains("my name is Ada"),
        "prior turn not in context: {second}"
    );
}
