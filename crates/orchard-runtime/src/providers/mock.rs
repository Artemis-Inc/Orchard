//! The mock provider: the offline test oracle. Echo mode reflects the prompt;
//! script mode replays a YAML list of turns; under a schema, an empty turn
//! synthesizes deterministic JSON. Ports v2's `providers/mock.py`.

use crate::error::ProviderError;
use crate::traits::{ChatRequest, ChatResponse, Provider, ToolCall};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value as Json;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Deserialize)]
struct MockTurn {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool_calls: Vec<MockCall>,
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MockCall {
    name: String,
    #[serde(default)]
    args: Json,
}

pub struct MockProvider {
    /// `None` → echo mode.
    turns: Option<Vec<MockTurn>>,
    cursor: Mutex<usize>,
    counter: Mutex<u64>,
}

impl MockProvider {
    /// `model == "echo"` → echo mode; otherwise `model` is a YAML script path
    /// resolved against `base_dir`.
    pub fn new(model: &str, base_dir: &Path) -> Result<MockProvider, ProviderError> {
        if model == "echo" {
            return Ok(MockProvider {
                turns: None,
                cursor: Mutex::new(0),
                counter: Mutex::new(0),
            });
        }
        let path = if Path::new(model).is_absolute() {
            std::path::PathBuf::from(model)
        } else {
            base_dir.join(model)
        };
        let text = std::fs::read_to_string(&path).map_err(|e| {
            ProviderError::new(
                format!("mock script not found: {} ({e})", path.display()),
                false,
            )
        })?;
        let turns: Vec<MockTurn> = serde_yaml::from_str(&text).map_err(|e| {
            ProviderError::new(format!("mock script is not a valid YAML list: {e}"), false)
        })?;
        Ok(MockProvider {
            turns: Some(turns),
            cursor: Mutex::new(0),
            counter: Mutex::new(0),
        })
    }

    fn next_id(&self) -> String {
        let mut c = self.counter.lock().unwrap();
        *c += 1;
        format!("call_{:08x}", *c)
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn supports_schema(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        "mock".to_string()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        match &self.turns {
            None => {
                // echo mode
                if let Some(schema) = &req.schema {
                    return Ok(ChatResponse {
                        text: synthesize_json(schema),
                        input_tokens: 10,
                        output_tokens: 20,
                        stop_reason: "stop".into(),
                        model: "mock".into(),
                        ..Default::default()
                    });
                }
                let last_user = req.messages.iter().rev().find(|m| m.role == "user");
                let content = last_user.map(|m| m.content.clone()).unwrap_or_default();
                let trimmed: String = content.chars().take(400).collect();
                Ok(ChatResponse {
                    text: format!("[mock:echo] You said: {trimmed}"),
                    input_tokens: (content.len() / 4) as i64,
                    output_tokens: 20,
                    stop_reason: "stop".into(),
                    model: "mock".into(),
                    ..Default::default()
                })
            }
            Some(turns) => {
                let mut cursor = self.cursor.lock().unwrap();
                if *cursor >= turns.len() {
                    return Ok(ChatResponse {
                        text: "(mock script exhausted)".into(),
                        input_tokens: 10,
                        output_tokens: 5,
                        stop_reason: "stop".into(),
                        model: "mock".into(),
                        ..Default::default()
                    });
                }
                let turn = &turns[*cursor];
                *cursor += 1;
                let advertised: std::collections::HashSet<&str> =
                    req.tools.iter().map(|t| t.name.as_str()).collect();
                let mut calls = Vec::new();
                for c in &turn.tool_calls {
                    if !advertised.contains(c.name.as_str()) {
                        return Err(ProviderError::new(
                            format!(
                                "mock script calls tool '{}' which is not advertised (available: {})",
                                c.name,
                                req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                            ),
                            false,
                        ));
                    }
                    calls.push(ToolCall {
                        id: self.next_id(),
                        name: c.name.clone(),
                        args: Some(c.args.clone()),
                        raw_args: serde_json::to_string(&c.args).unwrap_or_default(),
                    });
                }
                let mut text = turn.text.clone().unwrap_or_default();
                if text.is_empty() && calls.is_empty() {
                    if let Some(schema) = &req.schema {
                        text = synthesize_json(schema);
                    }
                }
                let stop_reason = if calls.is_empty() { "stop" } else { "tool_use" };
                Ok(ChatResponse {
                    text,
                    tool_calls: calls,
                    stop_reason: stop_reason.into(),
                    input_tokens: turn.input_tokens.unwrap_or(100),
                    output_tokens: turn.output_tokens.unwrap_or(50),
                    model: "mock".into(),
                })
            }
        }
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        on_token: &(dyn Fn(String) + Send + Sync),
    ) -> Result<ChatResponse, ProviderError> {
        // Resolve the response with the normal mock logic, then stream its text
        // back in small chunks so the offline experience shows token streaming.
        let resp = self.chat(req).await?;
        let text = resp.text.clone();
        if !text.is_empty() {
            let mut chunk = String::new();
            for ch in text.chars() {
                chunk.push(ch);
                if ch.is_whitespace() || chunk.chars().count() >= 5 {
                    on_token(std::mem::take(&mut chunk));
                    #[cfg(feature = "native")]
                    tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                }
            }
            if !chunk.is_empty() {
                on_token(chunk);
            }
        }
        Ok(resp)
    }
}

/// Deterministic JSON synthesis from a JSON Schema (v2's `_synthesize_json`).
pub fn synthesize_json(schema: &Json) -> String {
    serde_json::to_string(&value_for_schema(schema)).unwrap_or_else(|_| "\"text\"".into())
}

fn value_for_schema(schema: &Json) -> Json {
    let obj = match schema.as_object() {
        Some(o) => o,
        None => return Json::String("text".into()),
    };
    if let Some(en) = obj.get("enum").and_then(|e| e.as_array()) {
        if let Some(first) = en.first() {
            return first.clone();
        }
    }
    let ty = match obj.get("type") {
        Some(Json::String(s)) => Some(s.clone()),
        Some(Json::Array(a)) => a.first().and_then(|v| v.as_str()).map(|s| s.to_string()),
        _ => None,
    };
    match ty.as_deref() {
        Some("object") => {
            let mut m = serde_json::Map::new();
            if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
                for (k, v) in props {
                    m.insert(k.clone(), value_for_schema(v));
                }
            }
            Json::Object(m)
        }
        Some("array") => match obj.get("items") {
            Some(items) if items.is_object() => Json::Array(vec![value_for_schema(items)]),
            _ => Json::Array(vec![]),
        },
        Some("integer") | Some("number") => Json::from(0),
        Some("boolean") => Json::Bool(true),
        Some("null") => Json::Null,
        _ => Json::String("text".into()),
    }
}
