//! MCP client: launch an MCP server subprocess and expose its tools to the
//! agent, namespaced `ns_<tool>`. Newline-delimited JSON-RPC 2.0 over the
//! child's stdio. Native-only (needs subprocesses). Ports v2's `tools/mcp.py`.

#![cfg(feature = "native")]

use crate::error::ToolError;
use crate::tools::NativeTool;
use crate::traits::Tool;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: &str = "2025-06-18";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

struct InboxState {
    /// Responses keyed by the *canonical string form* of their JSON-RPC id, so
    /// we match whether the server echoes the id as a number or a string.
    responses: HashMap<String, Json>,
    /// Set when the reader thread exits (child closed stdout / died), so waiters
    /// fail fast instead of blocking until their deadline.
    closed: bool,
}

struct Inbox {
    state: Mutex<InboxState>,
    cv: Condvar,
}

/// The canonical string key for a JSON-RPC id (number or string forms unify).
fn id_key(v: &Json) -> Option<String> {
    if let Some(i) = v.as_i64() {
        Some(i.to_string())
    } else if let Some(u) = v.as_u64() {
        Some(u.to_string())
    } else if let Some(f) = v.as_f64() {
        if f.fract() == 0.0 {
            Some((f as i64).to_string())
        } else {
            Some(f.to_string())
        }
    } else {
        v.as_str().map(|s| s.to_string())
    }
}

pub struct McpClient {
    namespace: String,
    stdin: Mutex<ChildStdin>,
    inbox: Arc<Inbox>,
    next_id: AtomicU64,
    _child: Mutex<Child>,
}

/// A started MCP client paired with its discovered, namespaced tools.
pub type StartedMcp = (Arc<McpClient>, Vec<Arc<dyn Tool>>);

impl McpClient {
    /// Launch `command`, handshake, and return the client + its tool list.
    pub fn start(command: &str, namespace: &str) -> Result<StartedMcp, String> {
        let parts = shell_words(command);
        if parts.is_empty() {
            return Err("empty mcp command".into());
        }
        let mut child = Command::new(&parts[0])
            .args(&parts[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("mcp launch failed: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let inbox = Arc::new(Inbox {
            state: Mutex::new(InboxState {
                responses: HashMap::new(),
                closed: false,
            }),
            cv: Condvar::new(),
        });
        // reader thread
        {
            let inbox = inbox.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Json>(line) {
                        if let Some(key) = v.get("id").and_then(id_key) {
                            inbox.state.lock().unwrap().responses.insert(key, v);
                            inbox.cv.notify_all();
                        }
                        // server-initiated requests/notifications are ignored (V1)
                    }
                }
                // stdout closed → the server is gone; wake waiters to fail fast.
                inbox.state.lock().unwrap().closed = true;
                inbox.cv.notify_all();
            });
        }
        let client = Arc::new(McpClient {
            namespace: namespace.to_string(),
            stdin: Mutex::new(stdin),
            inbox,
            next_id: AtomicU64::new(1),
            _child: Mutex::new(child),
        });

        // handshake
        client.request(
            "initialize",
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "orchard", "version": "3.0"}}),
            HANDSHAKE_TIMEOUT,
        )?;
        client.notify("notifications/initialized", json!({}))?;
        let listed = client.request("tools/list", json!({}), HANDSHAKE_TIMEOUT)?;
        let tools = client.build_tools(&listed);
        Ok((client, tools))
    }

    fn send(&self, msg: &Json) -> Result<(), String> {
        let line = format!("{msg}\n");
        let mut stdin = self.stdin.lock().unwrap();
        stdin
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())
    }

    fn notify(&self, method: &str, params: Json) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(&self, method: &str, params: Json, timeout: Duration) -> Result<Json, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let key = id.to_string();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        let mut guard = self.inbox.state.lock().unwrap();
        loop {
            if let Some(v) = guard.responses.remove(&key) {
                if let Some(err) = v.get("error") {
                    return Err(err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("mcp error")
                        .to_string());
                }
                return Ok(v.get("result").cloned().unwrap_or(Json::Null));
            }
            if guard.closed {
                return Err(format!("mcp server closed before answering '{method}'"));
            }
            let now = Instant::now();
            if now >= deadline {
                // drop any late reply for this id so it can't leak in the map
                guard.responses.remove(&key);
                return Err(format!("mcp request '{method}' timed out"));
            }
            let (g, _) = self.inbox.cv.wait_timeout(guard, deadline - now).unwrap();
            guard = g;
        }
    }

    fn build_tools(self: &Arc<Self>, listed: &Json) -> Vec<Arc<dyn Tool>> {
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        let empty = vec![];
        for entry in listed
            .get("tools")
            .and_then(|t| t.as_array())
            .unwrap_or(&empty)
        {
            let original = match entry.get("name").and_then(|n| n.as_str()) {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            let advertised = format!("{}_{}", self.namespace, original);
            let desc = entry
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let schema = entry
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            let client = self.clone();
            let orig = original.clone();
            out.push(
                NativeTool::builder(&advertised)
                    .description(&desc)
                    .external(true)
                    .schema(schema)
                    .handler(move |args| {
                        let client = client.clone();
                        let orig = orig.clone();
                        async move {
                            tokio::task::spawn_blocking(move || client.call_tool(&orig, args))
                                .await
                                .map_err(|e| ToolError::new(e.to_string()))?
                        }
                    }),
            );
        }
        out
    }

    fn call_tool(&self, name: &str, args: Json) -> Result<Json, ToolError> {
        let result = self
            .request(
                "tools/call",
                json!({"name": name, "arguments": args}),
                CALL_TIMEOUT,
            )
            .map_err(ToolError::new)?;
        if result
            .get("isError")
            .and_then(|e| e.as_bool())
            .unwrap_or(false)
        {
            return Err(ToolError::new(flatten_content(&result)));
        }
        Ok(json!({ "content": flatten_content(&result) }))
    }
}

/// Flatten an MCP result's `content` items to text.
fn flatten_content(result: &Json) -> String {
    let mut out = Vec::new();
    if let Some(items) = result.get("content").and_then(|c| c.as_array()) {
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                out.push(
                    item.get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
            } else {
                out.push(item.to_string());
            }
        }
    }
    out.join("\n")
}

/// Minimal POSIX-ish word splitter (handles simple quotes). An empty quoted
/// token (`""`) is preserved as an empty argument.
fn shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut had_word = false; // a token started (even if it's empty via quotes)
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    had_word = true;
                }
                ' ' | '\t' => {
                    if had_word {
                        words.push(std::mem::take(&mut cur));
                        had_word = false;
                    }
                }
                _ => {
                    cur.push(c);
                    had_word = true;
                }
            },
        }
    }
    if had_word {
        words.push(cur);
    }
    words
}
