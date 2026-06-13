//! http/web/shell pack + builtin tests. HTTP is exercised with an injected fake
//! client (offline); shell runs real `/bin/sh` (offline).

use async_trait::async_trait;
use orchard::{Agent, HttpClient, HttpRequest, HttpResponse, Runtime};
use orchard_runtime::error::HttpError;
use std::sync::{Arc, Mutex};

/// A fake HTTP client that records requests and returns a canned response.
struct FakeHttp {
    last: Mutex<Option<HttpRequest>>,
    status: u16,
    body: Vec<u8>,
    content_type: String,
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        *self.last.lock().unwrap() = Some(req);
        Ok(HttpResponse {
            status: self.status,
            headers: vec![("content-type".into(), self.content_type.clone())],
            body: self.body.clone(),
        })
    }
}

fn fake(body: &str, ct: &str) -> Arc<FakeHttp> {
    Arc::new(FakeHttp {
        last: Mutex::new(None),
        status: 200,
        body: body.as_bytes().to_vec(),
        content_type: ct.into(),
    })
}

#[tokio::test]
async fn http_builtin_in_tool_body() {
    let http = fake(r#"{"temp": 21}"#, "application/json");
    let src = "agent A { model { provider: mock, name: \"echo\" } tool weather(city: str) -> json { http.get(\"https://api.example.com/{city}\") } skill s() -> str { let w = weather(city: \"paris\")\n return \"{w.temp}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent)
        .http_client(http.clone())
        .build()
        .unwrap();
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "21");
    // egress was enforced and the URL interpolated
    let req = http.last.lock().unwrap().clone().unwrap();
    assert_eq!(req.url, "https://api.example.com/paris");
    assert!(req.enforce_egress);
}

#[tokio::test]
async fn http_pack_request_tool() {
    let http = fake("hello body", "text/plain");
    let src = "agent A { model { provider: mock, name: \"echo\" } use http\n skill s() -> str { let r = http_request(method: \"GET\", url: \"https://example.com\")\n return \"{r.status}:{r.body}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).http_client(http).build().unwrap();
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "200:hello body");
}

#[tokio::test]
async fn shell_builtin_runs_when_allowed() {
    let src = "agent A { model { provider: mock, name: \"echo\" } policy { allow_shell: always } tool sh(cmd: str) -> str { shell(cmd) } skill s() -> str { return sh(cmd: \"echo orchard\").trim() } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "orchard");
}

#[tokio::test]
async fn shell_denied_by_default() {
    let src = "agent A { model { provider: mock, name: \"echo\" } tool sh(cmd: str) -> str { shell(cmd) } skill s() -> str { try { return sh(cmd: \"echo no\") } catch e { return \"blocked: {e.kind}\" } } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "blocked: shell");
}

#[test]
fn html_to_text_strips_tags() {
    let (title, text) = orchard_runtime::tools::net::html_to_text(
        "<html><head><title>Hi</title><style>x{}</style></head><body><p>Hello <b>world</b></p><script>bad()</script></body></html>",
    );
    assert_eq!(title, "Hi");
    assert!(text.contains("Hello world"));
    assert!(!text.contains("bad()"));
    assert!(!text.contains("x{}"));
}
