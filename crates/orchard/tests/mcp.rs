//! MCP client end-to-end test. Spawns a tiny python3 MCP server (newline JSON-RPC
//! over stdio), declares it via `use mcp(...)`, and calls its namespaced tool
//! from a skill body. Skipped automatically if python3 is unavailable.

#![cfg(feature = "native")]

use orchard::{Agent, Runtime};

const SERVER: &str = r#"
import sys, json
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid = msg.get("id")
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "serverInfo": {"name": "fake", "version": "1.0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{
            "name": "shout",
            "description": "Uppercase the text.",
            "inputSchema": {"type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]}}]}})
    elif method == "tools/call":
        args = msg.get("params", {}).get("arguments", {})
        text = str(args.get("text", "")).upper()
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "content": [{"type": "text", "text": text}]}})
    else:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "no method"}})
"#;

fn python3() -> Option<String> {
    for cand in ["python3", "python"] {
        if std::process::Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(cand.to_string());
        }
    }
    None
}

#[tokio::test]
async fn mcp_namespaced_tool_roundtrip() {
    let Some(py) = python3() else {
        eprintln!("python3 unavailable — skipping MCP test");
        return;
    };
    // write the fake server to a temp file (path has no spaces on CI/macOS)
    let dir = std::env::temp_dir().join(format!("orch_mcp_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("server.py");
    std::fs::write(&script, SERVER).unwrap();

    let cmd = format!("{} {}", py, script.display());
    let src = format!(
        "agent A {{ model {{ provider: mock, name: \"echo\" }} policy {{ allow_mcp: true }} use mcp(\"{}\") as notes\n skill s() -> str {{ let r = notes_shout(text: \"hi there\")\n return \"{{r.content}}\" }} }}",
        cmd
    );
    let agent = Agent::load(&src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "HI THERE");

    let _ = std::fs::remove_dir_all(&dir);
}
