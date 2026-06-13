//! Remote provider adapters: Anthropic, OpenAI-compatible (openai/groq/
//! together/openrouter), and Ollama. Plus provider fallback chains and
//! retry/backoff. Ports v2's `providers/*`.

use crate::error::ProviderError;
use crate::traits::{
    ChatRequest, ChatResponse, HttpClient, HttpRequest, Message, Provider, ToolCall,
};
use async_trait::async_trait;
use serde_json::{json, Value as Json};
use std::sync::Arc;

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// `(base_url, max_tokens_param)` per OpenAI-compatible variant.
fn openai_variant(name: &str) -> (&'static str, &'static str) {
    match name {
        "groq" => ("https://api.groq.com/openai/v1", "max_completion_tokens"),
        "together" => ("https://api.together.xyz/v1", "max_tokens"),
        "openrouter" => ("https://openrouter.ai/api/v1", "max_tokens"),
        _ => ("https://api.openai.com/v1", "max_completion_tokens"),
    }
}

async fn post_json(
    http: &Arc<dyn HttpClient>,
    url: &str,
    headers: Vec<(String, String)>,
    payload: &Json,
) -> Result<(u16, Json), ProviderError> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    let mut hs = vec![("Content-Type".to_string(), "application/json".to_string())];
    hs.extend(headers);
    let req = HttpRequest {
        method: "POST".into(),
        url: url.to_string(),
        headers: hs,
        body: Some(body),
        timeout_secs: 120,
        allowed_domains: vec![],
        allow_local: true,
        enforce_egress: false,
    };
    let resp = http
        .request(req)
        .await
        .map_err(|e| ProviderError::new(e.message, true))?;
    let parsed: Json = serde_json::from_slice(&resp.body).unwrap_or(Json::Null);
    Ok((resp.status, parsed))
}

fn retryable(status: u16) -> bool {
    status == 429 || status >= 500
}

// ---- Anthropic ----

pub struct AnthropicProvider {
    http: Arc<dyn HttpClient>,
    model: String,
    api_key: String,
    base_url: String,
}

const STRUCTURED_TOOL: &str = "respond_with_structured_output";

pub fn anthropic_payload(req: &ChatRequest, model: &str) -> Json {
    let mut payload = json!({
        "model": model,
        "max_tokens": req.max_tokens.unwrap_or(4096),
        "messages": anthropic_messages(&req.messages),
    });
    if !req.system.is_empty() {
        payload["system"] = json!(req.system);
    }
    if let Some(t) = req.temperature {
        payload["temperature"] = json!(t);
    }
    let mut tools: Vec<Json> = req
        .tools
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
        .collect();
    if let Some(schema) = &req.schema {
        tools.push(json!({"name": STRUCTURED_TOOL, "description": "Return the structured result.", "input_schema": schema}));
        payload["tool_choice"] = json!({"type": "tool", "name": STRUCTURED_TOOL});
    }
    if !tools.is_empty() {
        payload["tools"] = json!(tools);
    }
    payload
}

fn anthropic_messages(messages: &[Message]) -> Json {
    // Tool results travel inside user messages as tool_result blocks; assistant
    // tool calls as tool_use blocks. Consecutive tool results merge.
    let mut out: Vec<Json> = Vec::new();
    for m in messages {
        match m.role.as_str() {
            "tool" => {
                let block = json!({"type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content});
                if let Some(Json::Object(last)) = out.last_mut() {
                    if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                        if let Some(Json::Array(c)) = last.get_mut("content") {
                            c.push(block);
                            continue;
                        }
                    }
                }
                out.push(json!({"role": "user", "content": [block]}));
            }
            "assistant" => {
                let mut content: Vec<Json> = Vec::new();
                if !m.content.is_empty() {
                    content.push(json!({"type": "text", "text": m.content}));
                }
                for tc in &m.tool_calls {
                    content.push(json!({"type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.args.clone().unwrap_or(json!({}))}));
                }
                out.push(json!({"role": "assistant", "content": content}));
            }
            _ => out.push(json!({"role": "user", "content": m.content})),
        }
    }
    json!(out)
}

fn anthropic_parse(body: &Json) -> ChatResponse {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut structured: Option<String> = None;
    if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
        for block in content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    text.push_str(block.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                }
                Some("tool_use") => {
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    if name == STRUCTURED_TOOL {
                        structured = Some(input.to_string());
                    } else {
                        tool_calls.push(ToolCall {
                            id: block
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: name.to_string(),
                            raw_args: input.to_string(),
                            args: input.as_object().map(|_| input.clone()),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(s) = structured {
        text = s;
    }
    let usage = body.get("usage");
    ChatResponse {
        text,
        tool_calls,
        stop_reason: map_stop(body.get("stop_reason").and_then(|s| s.as_str())),
        input_tokens: usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        output_tokens: usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        model: body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn map_stop(s: Option<&str>) -> String {
    match s {
        Some("end_turn") | Some("stop") => "stop",
        Some("tool_use") | Some("tool_calls") => "tool_use",
        Some("max_tokens") | Some("length") => "length",
        _ => "other",
    }
    .to_string()
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn supports_schema(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        format!("anthropic:{}", self.model)
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let payload = anthropic_payload(&req, &self.model);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let headers = vec![
            ("x-api-key".into(), self.api_key.clone()),
            ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ];
        let (status, body) = post_json(&self.http, &url, headers, &payload).await?;
        if status != 200 {
            return Err(ProviderError::new(
                format!("anthropic {status}: {body}"),
                retryable(status),
            ));
        }
        Ok(anthropic_parse(&body))
    }
}

// ---- OpenAI-compatible ----

pub struct OpenAICompatProvider {
    http: Arc<dyn HttpClient>,
    variant: String,
    model: String,
    api_key: String,
    base_url: String,
    max_tokens_param: String,
}

pub fn openai_payload(req: &ChatRequest, model: &str, max_tokens_param: &str) -> Json {
    let mut messages: Vec<Json> = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    for m in &req.messages {
        match m.role.as_str() {
            "tool" => messages.push(json!({"role": "tool", "tool_call_id": m.tool_call_id.clone().unwrap_or_default(), "name": m.name.clone().unwrap_or_default(), "content": m.content})),
            "assistant" if !m.tool_calls.is_empty() => {
                let calls: Vec<Json> = m
                    .tool_calls
                    .iter()
                    .map(|tc| json!({"id": tc.id, "type": "function", "function": {"name": tc.name, "arguments": tc.args.clone().unwrap_or(json!({})).to_string()}}))
                    .collect();
                messages.push(json!({"role": "assistant", "content": m.content, "tool_calls": calls}));
            }
            _ => messages.push(json!({"role": m.role, "content": m.content})),
        }
    }
    let mut payload = json!({ "model": model, "messages": messages });
    if let Some(mt) = req.max_tokens {
        payload[max_tokens_param] = json!(mt);
    }
    if let Some(t) = req.temperature {
        payload["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Json> = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
            .collect();
        payload["tools"] = json!(tools);
        payload["tool_choice"] = json!("auto");
    }
    if let Some(schema) = &req.schema {
        payload["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "orchard_result", "schema": schema}});
    }
    payload
}

fn openai_parse(body: &Json) -> Result<ChatResponse, ProviderError> {
    let choice = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .ok_or_else(|| ProviderError::new("openai: no choices", false))?;
    let msg = &choice["message"];
    let text = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let mut tool_calls = Vec::new();
    if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
        for (i, c) in calls.iter().enumerate() {
            let func = &c["function"];
            let raw = func
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let args = serde_json::from_str::<Json>(&raw)
                .ok()
                .filter(|v| v.is_object());
            tool_calls.push(ToolCall {
                id: c
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{i}")),
                name: func
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                args,
                raw_args: raw,
            });
        }
    }
    let usage = body.get("usage");
    Ok(ChatResponse {
        text,
        tool_calls,
        stop_reason: map_stop(choice.get("finish_reason").and_then(|f| f.as_str())),
        input_tokens: usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        output_tokens: usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        model: body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

#[async_trait]
impl Provider for OpenAICompatProvider {
    fn supports_schema(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        format!("{}:{}", self.variant, self.model)
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let payload = openai_payload(&req, &self.model, &self.max_tokens_param);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut headers = vec![("Authorization".into(), format!("Bearer {}", self.api_key))];
        if self.variant == "openrouter" {
            headers.push((
                "HTTP-Referer".into(),
                "https://github.com/orchard-lang/orchard".into(),
            ));
            headers.push(("X-Title".into(), "Orchard".into()));
        }
        let (status, body) = post_json(&self.http, &url, headers, &payload).await?;
        if status != 200 {
            return Err(ProviderError::new(
                format!("{} {status}: {body}", self.variant),
                retryable(status),
            ));
        }
        openai_parse(&body)
    }
}

// ---- Ollama ----

pub struct OllamaProvider {
    http: Arc<dyn HttpClient>,
    model: String,
    base_url: String,
}

#[async_trait]
impl Provider for OllamaProvider {
    fn supports_schema(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        format!("ollama:{}", self.model)
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut messages: Vec<Json> = Vec::new();
        if !req.system.is_empty() {
            messages.push(json!({"role": "system", "content": req.system}));
        }
        for m in &req.messages {
            if m.role == "tool" {
                messages.push(json!({"role": "tool", "tool_name": m.name.clone().unwrap_or_default(), "content": m.content}));
            } else {
                messages.push(json!({"role": m.role, "content": m.content}));
            }
        }
        let mut payload = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "options": {"num_predict": req.max_tokens.unwrap_or(4096)},
        });
        if let Some(t) = req.temperature {
            payload["options"]["temperature"] = json!(t);
        }
        if let Some(schema) = &req.schema {
            payload["format"] = schema.clone();
        }
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let (status, body) = post_json(&self.http, &url, vec![], &payload).await?;
        if status != 200 {
            return Err(ProviderError::new(
                format!("ollama {status}: {body}"),
                retryable(status),
            ));
        }
        let msg = &body["message"];
        let text = msg
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let func = &c["function"];
                let args = func.get("arguments").cloned().filter(|v| v.is_object());
                tool_calls.push(ToolCall {
                    id: format!("call_{:08x}", tool_calls.len()),
                    name: func
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string(),
                    args,
                    raw_args: func
                        .get("arguments")
                        .map(|a| a.to_string())
                        .unwrap_or_default(),
                });
            }
        }
        let stop = if tool_calls.is_empty() {
            "stop"
        } else {
            "tool_use"
        };
        Ok(ChatResponse {
            text,
            tool_calls,
            stop_reason: stop.into(),
            input_tokens: body
                .get("prompt_eval_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            output_tokens: body.get("eval_count").and_then(|v| v.as_i64()).unwrap_or(0),
            model: self.model.clone(),
        })
    }
}

// ---- fallback chain + retry ----

pub struct FallbackProvider {
    chain: Vec<Arc<dyn Provider>>,
}

impl FallbackProvider {
    pub fn new(chain: Vec<Arc<dyn Provider>>) -> Self {
        FallbackProvider { chain }
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    fn supports_schema(&self) -> bool {
        self.chain
            .first()
            .map(|p| p.supports_schema())
            .unwrap_or(false)
    }
    fn describe(&self) -> String {
        self.chain.first().map(|p| p.describe()).unwrap_or_default()
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut last: Option<ProviderError> = None;
        for p in &self.chain {
            match call_with_retry(p.as_ref(), &req, 3).await {
                Ok(r) => return Ok(r),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| ProviderError::new("no providers configured", false)))
    }
}

async fn call_with_retry(
    p: &dyn Provider,
    req: &ChatRequest,
    attempts: u32,
) -> Result<ChatResponse, ProviderError> {
    let mut last = ProviderError::new("no attempt", false);
    for attempt in 0..attempts {
        match p.chat(req.clone()).await {
            Ok(r) => return Ok(r),
            Err(e) => {
                if !e.retryable || attempt + 1 == attempts {
                    return Err(e);
                }
                last = e;
                #[cfg(feature = "native")]
                tokio::time::sleep(std::time::Duration::from_secs(1u64 << attempt)).await;
            }
        }
    }
    Err(last)
}

// ---- factory ----

/// Build a provider (+ fallback chain) from the manifest `model` object.
/// `resolve_key(provider) -> Option<key>` reads the API key (from env/secrets).
pub fn get_provider(
    model: &Json,
    http: Arc<dyn HttpClient>,
    resolve_key: &dyn Fn(&str, Option<&str>) -> Option<String>,
) -> Result<Arc<dyn Provider>, String> {
    let primary = build_one(model, &http, resolve_key)?;
    let mut chain: Vec<Arc<dyn Provider>> = vec![primary];
    if let Some(fb) = model.get("fallback").and_then(|f| f.as_array()) {
        for entry in fb {
            if let Ok(p) = build_one(entry, &http, resolve_key) {
                chain.push(p);
            }
        }
    }
    if chain.len() == 1 {
        Ok(chain.into_iter().next().unwrap())
    } else {
        Ok(Arc::new(FallbackProvider::new(chain)))
    }
}

fn build_one(
    model: &Json,
    http: &Arc<dyn HttpClient>,
    resolve_key: &dyn Fn(&str, Option<&str>) -> Option<String>,
) -> Result<Arc<dyn Provider>, String> {
    let provider = model.get("provider").and_then(|p| p.as_str()).unwrap_or("");
    let name = model
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let api_key_ref = model.get("api_key").and_then(|k| k.as_str());
    let base_url = model.get("base_url").and_then(|b| b.as_str());
    match provider {
        "anthropic" => {
            let key = resolve_key("anthropic", api_key_ref).ok_or("ANTHROPIC_API_KEY not set")?;
            Ok(Arc::new(AnthropicProvider {
                http: http.clone(),
                model: name,
                api_key: key,
                base_url: base_url.unwrap_or("https://api.anthropic.com").to_string(),
            }))
        }
        "openai" | "groq" | "together" | "openrouter" => {
            let key = resolve_key(provider, api_key_ref)
                .ok_or_else(|| format!("{provider} API key not set"))?;
            let (default_base, max_param) = openai_variant(provider);
            Ok(Arc::new(OpenAICompatProvider {
                http: http.clone(),
                variant: provider.to_string(),
                model: name,
                api_key: key,
                base_url: base_url.unwrap_or(default_base).to_string(),
                max_tokens_param: max_param.to_string(),
            }))
        }
        "ollama" => Ok(Arc::new(OllamaProvider {
            http: http.clone(),
            model: name,
            base_url: base_url.unwrap_or("http://localhost:11434").to_string(),
        })),
        other => Err(format!("unknown provider '{other}'")),
    }
}
