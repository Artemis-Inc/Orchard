//! The `http` and `web` tool packs, built over the injected [`HttpClient`].
//! Tools are marked `external` so their output is sentinel-wrapped. The `web`
//! pack's `fetch_page` strips HTML to text; `web_search` queries DuckDuckGo
//! Lite (keyless).

use crate::error::ToolError;
use crate::tools::NativeTool;
use crate::traits::{HttpClient, HttpRequest, Tool};
use serde_json::{json, Value as Json};
use std::sync::Arc;

#[derive(Clone)]
pub struct EgressCfg {
    pub allowed_domains: Vec<String>,
    pub allow_local: bool,
}

async fn do_request(
    http: &Arc<dyn HttpClient>,
    cfg: &EgressCfg,
    method: &str,
    url: &str,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    timeout: u64,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), ToolError> {
    let req = HttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers,
        body,
        timeout_secs: timeout,
        allowed_domains: cfg.allowed_domains.clone(),
        allow_local: cfg.allow_local,
        enforce_egress: true,
    };
    let resp = http
        .request(req)
        .await
        .map_err(|e| ToolError::new(e.message))?;
    Ok((resp.status, resp.headers, resp.body))
}

fn response_result(status: u16, headers: &[(String, String)], body: &[u8]) -> Json {
    let text = String::from_utf8_lossy(body);
    let is_json = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.to_lowercase().contains("json"));
    let body_val = if is_json {
        serde_json::from_str::<Json>(&text).unwrap_or(Json::String(truncate(&text)))
    } else {
        Json::String(truncate(&text))
    };
    json!({ "status": status, "body": body_val })
}

fn truncate(s: &str) -> String {
    const MAX: usize = 48 * 1024;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX).collect();
        out.push_str("\n...[truncated]");
        out
    }
}

/// The `http` pack: a generic `http_request` tool.
pub fn http_pack(http: Arc<dyn HttpClient>, cfg: EgressCfg) -> Vec<Arc<dyn Tool>> {
    let tool = NativeTool::builder("http_request")
        .description("Make an HTTP request (egress-guarded). Returns {status, body}.")
        .param("method", "string", true)
        .param("url", "string", true)
        .param("headers", "object", false)
        .param("body", "string", false)
        .external(true)
        .handler(move |args| {
            let http = http.clone();
            let cfg = cfg.clone();
            async move {
                let method = args
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET")
                    .to_string();
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let headers = headers_from(args.get("headers"));
                let body = args
                    .get("body")
                    .and_then(|v| v.as_str())
                    .map(|s| s.as_bytes().to_vec());
                let (status, hs, b) =
                    do_request(&http, &cfg, &method, &url, headers, body, 60).await?;
                Ok(response_result(status, &hs, &b))
            }
        });
    vec![tool]
}

/// The `web` pack: `fetch_page` (HTML→text) and `web_search` (DuckDuckGo Lite).
pub fn web_pack(http: Arc<dyn HttpClient>, cfg: EgressCfg) -> Vec<Arc<dyn Tool>> {
    let http1 = http.clone();
    let cfg1 = cfg.clone();
    let fetch = NativeTool::builder("fetch_page")
        .description("Fetch a web page and return its text content.")
        .param("url", "string", true)
        .external(true)
        .handler(move |args| {
            let http = http1.clone();
            let cfg = cfg1.clone();
            async move {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let (_status, _hs, body) =
                    do_request(&http, &cfg, "GET", &url, vec![], None, 30).await?;
                let html = String::from_utf8_lossy(&body);
                let (title, text) = html_to_text(&html);
                Ok(json!({ "url": url, "title": title, "text": truncate(&text) }))
            }
        });
    let search = NativeTool::builder("web_search")
        .description("Search the web (keyless DuckDuckGo Lite). Returns {results}.")
        .param("query", "string", true)
        .param("max_results", "integer", false)
        .external(true)
        .handler(move |args| {
            let http = http.clone();
            let cfg = cfg.clone();
            async move {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let max = args
                    .get("max_results")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(5) as usize;
                let url = format!("https://lite.duckduckgo.com/lite/?q={}", urlencode(&query));
                let headers = vec![(
                    "User-Agent".to_string(),
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"
                        .to_string(),
                )];
                match do_request(&http, &cfg, "GET", &url, headers, None, 30).await {
                    Ok((_s, _h, body)) => {
                        let results = parse_ddg(&String::from_utf8_lossy(&body), max);
                        Ok(json!({ "results": results }))
                    }
                    // Failures return a structured error, never crash the loop.
                    Err(e) => Ok(json!({ "error": "search unavailable", "detail": e.0 })),
                }
            }
        });
    vec![fetch, search]
}

fn headers_from(v: Option<&Json>) -> Vec<(String, String)> {
    match v.and_then(|h| h.as_object()) {
        Some(o) => o
            .iter()
            .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
            .collect(),
        None => vec![],
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Strip HTML to `(title, text)`: drop script/style/head, turn block tags into
/// newlines, unescape common entities, collapse whitespace.
pub fn html_to_text(html: &str) -> (String, String) {
    let mut title = String::new();
    let mut out = String::new();
    let mut chars = html.chars().peekable();
    let mut skip_depth: i32 = 0;
    let lower = html.to_lowercase();
    let _ = &lower;
    let mut in_title = false;
    while let Some(c) = chars.next() {
        if c == '<' {
            // read the tag
            let mut tag = String::new();
            for tc in chars.by_ref() {
                if tc == '>' {
                    break;
                }
                tag.push(tc);
            }
            let t = tag.trim().to_lowercase();
            let name: String = t
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let closing = t.starts_with('/');
            match name.as_str() {
                "script" | "style" | "noscript" | "head" | "template" => {
                    skip_depth += if closing { -1 } else { 1 };
                    skip_depth = skip_depth.max(0);
                }
                "title" => in_title = !closing,
                "p" | "div" | "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                | "article" | "section"
                    if skip_depth == 0 =>
                {
                    out.push('\n');
                }
                _ => {}
            }
            continue;
        }
        if in_title {
            // title lives inside <head> (which we skip) — capture it anyway
            title.push(c);
            continue;
        }
        if skip_depth > 0 {
            continue;
        }
        out.push(c);
    }
    let text = unescape(&out);
    let collapsed: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect();
    (unescape(title.trim()), collapsed.join("\n"))
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Parse DuckDuckGo Lite result rows into `[{title, url, snippet}]`.
fn parse_ddg(html: &str, max: usize) -> Vec<Json> {
    let mut results = Vec::new();
    // result links carry the real URL in a `uddg=` query param.
    for part in html.split("uddg=").skip(1) {
        if results.len() >= max {
            break;
        }
        let enc: String = part
            .chars()
            .take_while(|c| *c != '&' && *c != '"' && *c != '\'')
            .collect();
        let url = urldecode(&enc);
        // title = the link text after the next '>'
        let title = part
            .split_once('>')
            .map(|(_, rest)| rest.chars().take_while(|c| *c != '<').collect::<String>())
            .unwrap_or_default();
        if !url.is_empty() {
            results.push(json!({ "title": unescape(title.trim()), "url": url, "snippet": "" }));
        }
    }
    results
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
