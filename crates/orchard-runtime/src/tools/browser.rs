//! The `browser` tool pack: real headless-Chrome control over the Chrome
//! DevTools Protocol (CDP). The agent declares `use browser` to grant it, then
//! the autonomous loop can navigate pages, read rendered text (after JavaScript
//! runs), click and type into elements, evaluate JavaScript, and capture
//! screenshots.
//!
//! It is self-contained: it launches Chrome with an ephemeral profile and a
//! random debugging port, speaks CDP directly over a WebSocket, and tears the
//! browser down when the session drops. No external driver, no extra service.
//! Page content is untrusted, so every tool is marked `external` and its output
//! is sentinel-wrapped before the model sees it.

use crate::error::ToolError;
use crate::tools::NativeTool;
use crate::traits::Tool;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A live headless-Chrome session: the child process, its ephemeral profile
/// directory, and an open CDP WebSocket to the active page target.
struct BrowserSession {
    child: std::process::Child,
    profile_dir: PathBuf,
    ws: Ws,
    next_id: AtomicU64,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Best-effort cleanup of the throwaway profile.
        let _ = std::fs::remove_dir_all(&self.profile_dir);
    }
}

/// Find a Chrome/Chromium executable: honor `$ORCH_CHROME`, then probe the usual
/// install locations per platform.
fn find_chrome() -> Option<String> {
    if let Ok(p) = std::env::var("ORCH_CHROME") {
        if !p.is_empty() && Path::new(&p).exists() {
            return Some(p);
        }
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    ];
    candidates
        .iter()
        .find(|p| Path::new(p).exists())
        .map(|s| s.to_string())
}

impl BrowserSession {
    /// Launch Chrome and attach to its first page target.
    async fn launch(headless: bool) -> Result<BrowserSession, String> {
        let chrome = find_chrome().ok_or_else(|| {
            "no Chrome/Chromium found. Install Google Chrome, or set $ORCH_CHROME to its path."
                .to_string()
        })?;
        // A unique throwaway profile so we never touch the user's real Chrome and
        // can read the chosen debugging port from DevToolsActivePort.
        let pid = std::process::id();
        let nonce = Instant::now().elapsed().as_nanos();
        let profile_dir =
            std::env::temp_dir().join(format!("orch-chrome-{pid}-{nonce}"));
        let _ = std::fs::create_dir_all(&profile_dir);

        let mut cmd = std::process::Command::new(&chrome);
        cmd.arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile_dir.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            .arg("--disable-extensions")
            .arg("--disable-gpu")
            .arg("--window-size=1280,900")
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if headless {
            cmd.arg("--headless=new");
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to launch Chrome: {e}"))?;

        // Any failure before the session is built must kill the child so we don't
        // leak a Chrome process. `bringup` does the fallible work; we wrap it.
        let mut child = child;
        let bringup = async {
            // Chrome writes the actual port to <profile>/DevToolsActivePort once ready.
            let port_file = profile_dir.join("DevToolsActivePort");
            let port = read_devtools_port(&port_file).await?;
            // List targets and pick a page to drive.
            let ws_url = page_target_ws(port).await?;
            let (ws, _resp) = connect_async(&ws_url)
                .await
                .map_err(|e| format!("CDP websocket connect failed: {e}"))?;
            Ok::<Ws, String>(ws)
        };
        let ws = match bringup.await {
            Ok(ws) => ws,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&profile_dir);
                return Err(e);
            }
        };

        let mut session = BrowserSession {
            child,
            profile_dir,
            ws,
            next_id: AtomicU64::new(1),
        };
        // Enable the domains we use.
        session.cmd("Page.enable", json!({})).await?;
        session.cmd("Runtime.enable", json!({})).await?;
        Ok(session)
    }

    /// Send one CDP command and wait for its matching reply, skipping events.
    async fn cmd(&mut self, method: &str, params: Json) -> Result<Json, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "id": id, "method": method, "params": params });
        self.ws
            .send(Message::Text(msg.to_string()))
            .await
            .map_err(|e| format!("CDP send failed: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("CDP timeout waiting for {method}"));
            }
            match tokio::time::timeout(remaining, self.ws.next()).await {
                Err(_) => return Err(format!("CDP timeout waiting for {method}")),
                Ok(None) => return Err("CDP connection closed".to_string()),
                Ok(Some(Err(e))) => return Err(format!("CDP read failed: {e}")),
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Json = serde_json::from_str(&t).unwrap_or(Json::Null);
                    if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                        if let Some(err) = v.get("error") {
                            return Err(format!("CDP error on {method}: {err}"));
                        }
                        return Ok(v.get("result").cloned().unwrap_or(Json::Null));
                    }
                    // otherwise an event for another id — ignore and keep reading
                }
                Ok(Some(Ok(Message::Close(_)))) => {
                    return Err("CDP connection closed".to_string())
                }
                Ok(Some(Ok(_))) => {} // ping/pong/binary
            }
        }
    }

    /// Evaluate a JavaScript expression in the page and return its value as JSON.
    async fn eval(&mut self, expr: &str) -> Result<Json, String> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "returnByValue": true,
                    "awaitPromise": true,
                    "userGesture": true,
                }),
            )
            .await?;
        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .or_else(|| exc.get("text").and_then(|t| t.as_str()))
                .unwrap_or("javascript error");
            return Err(text.to_string());
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Json::Null))
    }

    /// Navigate to a URL and wait (best-effort) for the document to finish.
    async fn navigate(&mut self, url: &str) -> Result<(), String> {
        let res = self.cmd("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = res.get("errorText").and_then(|e| e.as_str()) {
            if !err.is_empty() {
                return Err(format!("navigation failed: {err}"));
            }
        }
        // Poll document.readyState until complete (or a short timeout).
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = self
                .eval("document.readyState")
                .await
                .unwrap_or(Json::Null);
            if state.as_str() == Some("complete") {
                break;
            }
            if Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Ok(())
    }
}

/// Wait for Chrome to write its DevToolsActivePort file and return the port.
async fn read_devtools_port(path: &Path) -> Result<u16, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Some(first) = contents.lines().next() {
                if let Ok(port) = first.trim().parse::<u16>() {
                    return Ok(port);
                }
            }
        }
        if Instant::now() > deadline {
            return Err("Chrome did not become ready (no DevToolsActivePort)".to_string());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Query Chrome's HTTP control endpoint for a page target's WebSocket URL.
async fn page_target_ws(port: u16) -> Result<String, String> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    // Give the target list a couple of tries; the initial page can lag startup.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(resp) = client.get(format!("{base}/json")).send().await {
            if let Ok(list) = resp.json::<Json>().await {
                if let Some(arr) = list.as_array() {
                    if let Some(ws) = arr
                        .iter()
                        .find(|t| t.get("type").and_then(|x| x.as_str()) == Some("page"))
                        .and_then(|t| t.get("webSocketDebuggerUrl"))
                        .and_then(|u| u.as_str())
                    {
                        return Ok(ws.to_string());
                    }
                }
            }
        }
        // No page target yet — ask Chrome to open one.
        let _ = client.put(format!("{base}/json/new?about:blank")).send().await;
        if Instant::now() > deadline {
            return Err("Chrome exposed no page target to drive".to_string());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Shared, lazily-launched session behind the pack's tools.
type Shared = Arc<Mutex<Option<BrowserSession>>>;

/// Lock the shared slot, launching Chrome on first use.
async fn ensure<'a>(
    guard: &'a mut tokio::sync::MutexGuard<'_, Option<BrowserSession>>,
    headless: bool,
) -> Result<&'a mut BrowserSession, String> {
    if guard.is_none() {
        **guard = Some(BrowserSession::launch(headless).await?);
    }
    Ok(guard.as_mut().unwrap())
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

/// JSON-string a value for safe embedding inside an evaluated JS expression.
fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Build the `browser` pack. All tools share one lazily-launched Chrome.
pub fn browser_pack(headless: bool) -> Vec<Arc<dyn Tool>> {
    let shared: Shared = Arc::new(Mutex::new(None));
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    // browser_open(url)
    {
        let shared = shared.clone();
        tools.push(
            NativeTool::builder("browser_open")
                .description(
                    "Open a URL in a real headless browser and wait for it to load. \
                     Returns {url, title}. JavaScript runs, so this sees the rendered page.",
                )
                .param("url", "string", true)
                .external(true)
                .handler(move |args| {
                    let shared = shared.clone();
                    async move {
                        let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        if url.is_empty() {
                            return Err(ToolError::new("browser_open needs a url"));
                        }
                        let url = normalize_url(url);
                        let mut guard = shared.lock().await;
                        let s = ensure(&mut guard, headless).await.map_err(ToolError::new)?;
                        s.navigate(&url).await.map_err(ToolError::new)?;
                        let title = s.eval("document.title").await.unwrap_or(Json::Null);
                        let cur = s.eval("location.href").await.unwrap_or(Json::Null);
                        Ok(json!({
                            "url": cur.as_str().unwrap_or(&url),
                            "title": title.as_str().unwrap_or(""),
                        }))
                    }
                }),
        );
    }

    // browser_read() -> rendered text of the current page
    {
        let shared = shared.clone();
        tools.push(
            NativeTool::builder("browser_read")
                .description(
                    "Read the visible text of the current page (post-JavaScript). \
                     Returns {title, url, text}.",
                )
                .external(true)
                .handler(move |_args| {
                    let shared = shared.clone();
                    async move {
                        let mut guard = shared.lock().await;
                        let s = ensure(&mut guard, headless).await.map_err(ToolError::new)?;
                        let text = s
                            .eval("document.body ? document.body.innerText : ''")
                            .await
                            .map_err(ToolError::new)?;
                        let title = s.eval("document.title").await.unwrap_or(Json::Null);
                        let url = s.eval("location.href").await.unwrap_or(Json::Null);
                        Ok(json!({
                            "title": title.as_str().unwrap_or(""),
                            "url": url.as_str().unwrap_or(""),
                            "text": truncate(text.as_str().unwrap_or("")),
                        }))
                    }
                }),
        );
    }

    // browser_click(selector)
    {
        let shared = shared.clone();
        tools.push(
            NativeTool::builder("browser_click")
                .description(
                    "Click the first element matching a CSS selector. Returns {clicked}.",
                )
                .param("selector", "string", true)
                .external(true)
                .handler(move |args| {
                    let shared = shared.clone();
                    async move {
                        let sel = args.get("selector").and_then(|v| v.as_str()).unwrap_or("");
                        if sel.is_empty() {
                            return Err(ToolError::new("browser_click needs a selector"));
                        }
                        let expr = format!(
                            "(() => {{ const el = document.querySelector({}); \
                             if (!el) return false; el.scrollIntoView(); el.click(); return true; }})()",
                            js_string(sel)
                        );
                        let mut guard = shared.lock().await;
                        let s = ensure(&mut guard, headless).await.map_err(ToolError::new)?;
                        let clicked = s.eval(&expr).await.map_err(ToolError::new)?;
                        // Let any resulting navigation/render settle.
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        let _ = s.eval("document.readyState").await;
                        Ok(json!({ "clicked": clicked.as_bool().unwrap_or(false) }))
                    }
                }),
        );
    }

    // browser_type(selector, text)
    {
        let shared = shared.clone();
        tools.push(
            NativeTool::builder("browser_type")
                .description(
                    "Type text into the input/textarea matching a CSS selector \
                     (fires input/change events). Returns {typed}.",
                )
                .param("selector", "string", true)
                .param("text", "string", true)
                .external(true)
                .handler(move |args| {
                    let shared = shared.clone();
                    async move {
                        let sel = args.get("selector").and_then(|v| v.as_str()).unwrap_or("");
                        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if sel.is_empty() {
                            return Err(ToolError::new("browser_type needs a selector"));
                        }
                        let expr = format!(
                            "(() => {{ const el = document.querySelector({}); if (!el) return false; \
                             el.focus(); el.value = {}; \
                             el.dispatchEvent(new Event('input', {{bubbles:true}})); \
                             el.dispatchEvent(new Event('change', {{bubbles:true}})); return true; }})()",
                            js_string(sel),
                            js_string(text)
                        );
                        let mut guard = shared.lock().await;
                        let s = ensure(&mut guard, headless).await.map_err(ToolError::new)?;
                        let ok = s.eval(&expr).await.map_err(ToolError::new)?;
                        Ok(json!({ "typed": ok.as_bool().unwrap_or(false) }))
                    }
                }),
        );
    }

    // browser_eval(script)
    {
        let shared = shared.clone();
        tools.push(
            NativeTool::builder("browser_eval")
                .description(
                    "Evaluate a JavaScript expression in the current page and return its \
                     JSON-serializable value. Use for scraping structured data.",
                )
                .param("script", "string", true)
                .external(true)
                .handler(move |args| {
                    let shared = shared.clone();
                    async move {
                        let script =
                            args.get("script").and_then(|v| v.as_str()).unwrap_or("");
                        if script.is_empty() {
                            return Err(ToolError::new("browser_eval needs a script"));
                        }
                        let mut guard = shared.lock().await;
                        let s = ensure(&mut guard, headless).await.map_err(ToolError::new)?;
                        let val = s.eval(script).await.map_err(ToolError::new)?;
                        Ok(json!({ "result": val }))
                    }
                }),
        );
    }

    // browser_screenshot(path?)
    {
        let shared = shared.clone();
        tools.push(
            NativeTool::builder("browser_screenshot")
                .description(
                    "Capture a PNG screenshot of the current page to a file. \
                     Returns {path, bytes}. Defaults to ./screenshot.png.",
                )
                .param("path", "string", false)
                .param("full_page", "boolean", false)
                .external(true)
                .handler(move |args| {
                    let shared = shared.clone();
                    async move {
                        let path = args
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("screenshot.png")
                            .to_string();
                        let full = args
                            .get("full_page")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let mut params = json!({ "format": "png" });
                        if full {
                            params["captureBeyondViewport"] = json!(true);
                        }
                        let mut guard = shared.lock().await;
                        let s = ensure(&mut guard, headless).await.map_err(ToolError::new)?;
                        let res = s
                            .cmd("Page.captureScreenshot", params)
                            .await
                            .map_err(ToolError::new)?;
                        let b64 = res
                            .get("data")
                            .and_then(|d| d.as_str())
                            .ok_or_else(|| ToolError::new("screenshot returned no data"))?;
                        let bytes = base64_decode(b64)
                            .map_err(|e| ToolError::new(format!("bad screenshot data: {e}")))?;
                        std::fs::write(&path, &bytes)
                            .map_err(|e| ToolError::new(format!("write {path} failed: {e}")))?;
                        Ok(json!({ "path": path, "bytes": bytes.len() }))
                    }
                }),
        );
    }

    tools
}

/// Prefix a bare host/path with https:// so `browser_open("example.com")` works.
fn normalize_url(url: &str) -> String {
    let u = url.trim();
    if u.starts_with("http://")
        || u.starts_with("https://")
        || u.starts_with("file://")
        || u.starts_with("about:")
        || u.starts_with("data:")
    {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

/// Decode standard base64 (CDP screenshot payloads). No external crate.
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c).ok_or("invalid base64 character")? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_known_vectors() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        // PNG magic header, base64 of \x89PNG\r\n\x1a\n
        assert_eq!(
            base64_decode("iVBORw0KGgo=").unwrap(),
            vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
    }

    #[test]
    fn normalize_url_adds_scheme() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(normalize_url("https://x.io"), "https://x.io");
        assert_eq!(normalize_url("file:///tmp/a.html"), "file:///tmp/a.html");
        assert_eq!(normalize_url("about:blank"), "about:blank");
    }

    #[test]
    fn js_string_escapes() {
        assert_eq!(js_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(js_string("#id .cls"), "\"#id .cls\"");
    }

    // Real headless-Chrome smoke test. Ignored by default (needs Chrome
    // installed). Run with: cargo test -p orchard-runtime --lib browser_live -- --ignored
    #[tokio::test]
    #[ignore]
    async fn browser_live_navigate_read_eval_screenshot() {
        if find_chrome().is_none() {
            eprintln!("skipping: no Chrome found");
            return;
        }
        let mut s = BrowserSession::launch(true).await.expect("launch chrome");
        let page = "data:text/html,<title>Orchard Test</title>\
            <body><h1 id=h>Hello</h1><input id=q><button onclick=\"document.getElementById('h').textContent='Clicked'\">go</button></body>";
        s.navigate(page).await.expect("navigate");

        let title = s.eval("document.title").await.unwrap();
        assert_eq!(title.as_str(), Some("Orchard Test"));

        let text = s
            .eval("document.body.innerText")
            .await
            .unwrap();
        assert!(text.as_str().unwrap().contains("Hello"), "got: {text:?}");

        // Click the button via the same expression browser_click builds.
        let clicked = s
            .eval("(() => { const el = document.querySelector('button'); if(!el) return false; el.click(); return true; })()")
            .await
            .unwrap();
        assert_eq!(clicked.as_bool(), Some(true));
        let after = s.eval("document.getElementById('h').textContent").await.unwrap();
        assert_eq!(after.as_str(), Some("Clicked"));

        // Screenshot returns real PNG bytes.
        let res = s
            .cmd("Page.captureScreenshot", json!({ "format": "png" }))
            .await
            .unwrap();
        let b64 = res.get("data").and_then(|d| d.as_str()).unwrap();
        let bytes = base64_decode(b64).unwrap();
        assert!(bytes.len() > 100, "screenshot too small");
        assert_eq!(&bytes[0..8], &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
    }
}
