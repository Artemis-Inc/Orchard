//! Built-in tool packs, native host tools, and the pack builder. Tools capture
//! their context (store, base_dir) at construction.
//!
//! P11 ships the offline-relevant packs (calculator, memory, files, time) and
//! the native host-tool API (the embedding superpower). `http`/`shell`/`web`
//! tool bodies + MCP land with the HTTP client and gating (P12/P13).

#[cfg(feature = "native")]
pub mod browser;
pub mod calc;
#[cfg(feature = "native")]
pub mod mcp;
pub mod net;
pub mod shell;

use crate::error::ToolError;
use crate::traits::{Store, Tool};
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A synchronous built-in tool (the body is pure/local).
pub struct SyncTool {
    name: String,
    description: String,
    schema: Json,
    external: bool,
    #[allow(clippy::type_complexity)]
    f: Box<dyn Fn(Json) -> Result<Json, ToolError> + Send + Sync>,
}

impl SyncTool {
    fn make(
        name: &str,
        description: &str,
        schema: Json,
        f: impl Fn(Json) -> Result<Json, ToolError> + Send + Sync + 'static,
    ) -> Arc<dyn Tool> {
        Arc::new(SyncTool {
            name: name.into(),
            description: description.into(),
            schema,
            external: false,
            f: Box::new(f),
        })
    }
}

#[async_trait]
impl Tool for SyncTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &Json {
        &self.schema
    }
    fn external(&self) -> bool {
        self.external
    }
    async fn call(&self, args: Json) -> Result<Json, ToolError> {
        (self.f)(args)
    }
}

type NativeHandler = Arc<dyn Fn(Json) -> BoxFuture<'static, Result<Json, ToolError>> + Send + Sync>;

/// A host-registered native (Rust) tool the agent's `delegate` loop can call —
/// the v3 analogue of v2's pure-computation tools, generalized to async.
pub struct NativeTool {
    name: String,
    description: String,
    schema: Json,
    external: bool,
    handler: NativeHandler,
}

impl NativeTool {
    pub fn builder(name: &str) -> NativeToolBuilder {
        NativeToolBuilder {
            name: name.into(),
            description: String::new(),
            props: serde_json::Map::new(),
            required: Vec::new(),
            external: false,
            schema_override: None,
        }
    }
}

#[async_trait]
impl Tool for NativeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &Json {
        &self.schema
    }
    fn external(&self) -> bool {
        self.external
    }
    async fn call(&self, args: Json) -> Result<Json, ToolError> {
        (self.handler)(args).await
    }
}

pub struct NativeToolBuilder {
    name: String,
    description: String,
    props: serde_json::Map<String, Json>,
    required: Vec<Json>,
    external: bool,
    schema_override: Option<Json>,
}

impl NativeToolBuilder {
    pub fn description(mut self, d: &str) -> Self {
        self.description = d.into();
        self
    }
    /// Add a parameter. `ty` is a JSON-schema type string (`"string"`, etc.).
    pub fn param(mut self, name: &str, ty: &str, required: bool) -> Self {
        self.props.insert(name.into(), json!({ "type": ty }));
        if required {
            self.required.push(json!(name));
        }
        self
    }
    pub fn external(mut self, yes: bool) -> Self {
        self.external = yes;
        self
    }
    /// Override the generated JSON schema wholesale (e.g. an MCP `inputSchema`).
    pub fn schema(mut self, schema: Json) -> Self {
        self.schema_override = Some(schema);
        self
    }
    /// Provide the async handler and build the tool.
    pub fn handler<F, Fut>(self, f: F) -> Arc<dyn Tool>
    where
        F: Fn(Json) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Json, ToolError>> + Send + 'static,
    {
        use futures::FutureExt;
        let schema = self.schema_override.unwrap_or_else(|| {
            json!({
                "type": "object",
                "properties": Json::Object(self.props),
                "required": Json::Array(self.required),
            })
        });
        Arc::new(NativeTool {
            name: self.name,
            description: self.description,
            schema,
            external: self.external,
            handler: Arc::new(move |args| f(args).boxed()),
        })
    }
}

// ---- pack builder ----

/// Context for building tool packs: base dir, HTTP client, and the resolved
/// egress/shell policy.
pub struct PackCtx {
    pub base_dir: PathBuf,
    pub http: Option<Arc<dyn crate::traits::HttpClient>>,
    pub allowed_domains: Vec<String>,
    pub allow_local_http: bool,
    /// Effective `allow_shell` after the external-ingest downgrade.
    pub allow_shell: String,
    pub interactive: bool,
}

/// Build the backend tools from the manifest + a store + the pack context. Adds
/// the memory pack when facts are enabled and not already declared.
pub fn build_pack_tools(
    manifest: &Json,
    store: Option<Arc<dyn Store>>,
    ctx: &PackCtx,
) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    if let Some(arr) = manifest.get("tools").and_then(|t| t.as_array()) {
        for t in arr {
            if t.get("kind").and_then(|k| k.as_str()) == Some("pack") {
                if let Some(name) = t.get("name").and_then(|n| n.as_str()) {
                    declared.push(name.to_string());
                    let options = t.get("options").cloned().unwrap_or(json!({}));
                    add_pack(name, &options, &store, ctx, &mut tools);
                }
            }
        }
    }
    // auto-add memory pack
    let facts = manifest["memory"]
        .get("facts_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if facts && !declared.iter().any(|d| d == "memory") {
        add_pack("memory", &json!({}), &store, ctx, &mut tools);
    }
    tools
}

fn add_pack(
    name: &str,
    options: &Json,
    store: &Option<Arc<dyn Store>>,
    ctx: &PackCtx,
    out: &mut Vec<Arc<dyn Tool>>,
) {
    let egress = net::EgressCfg {
        allowed_domains: ctx.allowed_domains.clone(),
        allow_local: ctx.allow_local_http,
    };
    match name {
        "calculator" => out.push(calculator_tool()),
        "time" => out.push(time_tool()),
        "memory" => {
            if let Some(s) = store {
                out.extend(memory_pack(s.clone()));
            }
        }
        "files" => {
            let root = options.get("root").and_then(|r| r.as_str()).unwrap_or(".");
            let root = ctx.base_dir.join(root);
            out.extend(files_pack(root));
        }
        "http" => {
            if let Some(http) = &ctx.http {
                out.extend(net::http_pack(http.clone(), egress));
            }
        }
        "web" => {
            if let Some(http) = &ctx.http {
                out.extend(net::web_pack(http.clone(), egress));
            }
        }
        "shell" => out.extend(shell::shell_pack(
            ctx.allow_shell.clone(),
            ctx.interactive,
            ctx.base_dir.clone(),
        )),
        #[cfg(feature = "native")]
        "browser" => {
            let headless = options
                .get("headless")
                .and_then(|h| h.as_bool())
                .unwrap_or(true);
            out.extend(browser::browser_pack(headless));
        }
        _ => {}
    }
}

fn calculator_tool() -> Arc<dyn Tool> {
    SyncTool::make(
        "calculate",
        "Evaluate an arithmetic expression and return the numeric result.",
        json!({"type": "object", "properties": {"expression": {"type": "string"}}, "required": ["expression"]}),
        |args| {
            let expr = args
                .get("expression")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match calc::evaluate(expr) {
                Ok(r) => {
                    // integral results as integers, matching arithmetic intuition
                    let val = if r.fract() == 0.0 && r.abs() < 1e15 {
                        json!(r as i64)
                    } else {
                        json!(r)
                    };
                    Ok(json!({ "result": val }))
                }
                Err(e) => Err(ToolError::new(e)),
            }
        },
    )
}

fn time_tool() -> Arc<dyn Tool> {
    SyncTool::make(
        "current_time",
        "Return the current time (unix seconds + ISO-8601 UTC).",
        json!({"type": "object", "properties": {}}),
        |_args| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Ok(json!({ "unix": secs, "iso": iso_utc(secs) }))
        },
    )
}

fn memory_pack(store: Arc<dyn Store>) -> Vec<Arc<dyn Tool>> {
    let s1 = store.clone();
    let remember = SyncTool::make(
        "remember",
        "Store a durable fact (key = value).",
        json!({"type": "object", "properties": {"key": {"type": "string"}, "value": {"type": "string"}}, "required": ["key", "value"]}),
        move |args| {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");
            if key.is_empty() {
                return Err(ToolError::new("remember requires a key"));
            }
            s1.remember(key, value);
            Ok(json!({ "remembered": key }))
        },
    );
    let s2 = store.clone();
    let recall = SyncTool::make(
        "recall",
        "Recall facts matching a query (empty = all).",
        json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        move |args| {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let facts: serde_json::Map<String, Json> = s2
                .recall(q)
                .into_iter()
                .map(|(k, v)| (k, json!(v)))
                .collect();
            Ok(json!({ "facts": facts }))
        },
    );
    let s3 = store;
    let forget = SyncTool::make(
        "forget",
        "Forget a fact by key.",
        json!({"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]}),
        move |args| {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({ "forgotten": s3.forget(key) }))
        },
    );
    vec![remember, recall, forget]
}

fn files_pack(root: PathBuf) -> Vec<Arc<dyn Tool>> {
    let real_root = std::fs::canonicalize(&root).unwrap_or(root);
    let r1 = real_root.clone();
    let read = SyncTool::make(
        "read_file",
        "Read a UTF-8 file within the files root.",
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        move |args| {
            let p = confine(&r1, args.get("path").and_then(|v| v.as_str()).unwrap_or(""))?;
            let content = std::fs::read_to_string(&p)
                .map_err(|e| ToolError::new(format!("read failed: {e}")))?;
            Ok(
                json!({ "path": args.get("path").cloned().unwrap_or(json!("")), "content": content }),
            )
        },
    );
    let r2 = real_root.clone();
    let write = SyncTool::make(
        "write_file",
        "Write a UTF-8 file within the files root.",
        json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
        move |args| {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            deny_protected(rel)?;
            let p = confine(&r2, rel)?;
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            std::fs::write(&p, content)
                .map_err(|e| ToolError::new(format!("write failed: {e}")))?;
            Ok(json!({ "written": rel, "bytes": content.len() }))
        },
    );
    let r3 = real_root.clone();
    let append = SyncTool::make(
        "append_file",
        "Append to a UTF-8 file within the files root.",
        json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
        move |args| {
            use std::io::Write;
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            deny_protected(rel)?;
            let p = confine(&r3, rel)?;
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .map_err(|e| ToolError::new(format!("open failed: {e}")))?;
            f.write_all(content.as_bytes())
                .map_err(|e| ToolError::new(format!("append failed: {e}")))?;
            Ok(json!({ "appended": rel }))
        },
    );
    let r4 = real_root;
    let list = SyncTool::make(
        "list_dir",
        "List a directory within the files root.",
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        move |args| {
            let p = confine(
                &r4,
                args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
            )?;
            let mut entries = Vec::new();
            for e in std::fs::read_dir(&p)
                .map_err(|e| ToolError::new(format!("list failed: {e}")))?
                .flatten()
            {
                let ft = e.file_type().ok();
                let kind = match ft {
                    Some(t) if t.is_dir() => "dir",
                    Some(t) if t.is_file() => "file",
                    _ => "other",
                };
                entries.push(json!({ "name": e.file_name().to_string_lossy(), "type": kind }));
            }
            Ok(json!({ "entries": entries }))
        },
    );
    vec![read, write, append, list]
}

/// CVE-correct path containment: resolve the candidate (or its nearest existing
/// ancestor) and require it stays under `real_root`.
fn confine(real_root: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let candidate = real_root.join(rel);
    let resolved = if candidate.exists() {
        std::fs::canonicalize(&candidate).unwrap_or(candidate)
    } else {
        // canonicalize the nearest existing ancestor, then re-append the tail
        let mut existing = candidate.clone();
        let mut tail = Vec::new();
        while !existing.exists() {
            if let Some(name) = existing.file_name() {
                tail.push(name.to_os_string());
            }
            match existing.parent() {
                Some(p) => existing = p.to_path_buf(),
                None => break,
            }
        }
        let mut base = std::fs::canonicalize(&existing).unwrap_or(existing);
        for part in tail.iter().rev() {
            base.push(part);
        }
        base
    };
    if resolved == *real_root || resolved.starts_with(real_root) {
        Ok(resolved)
    } else {
        Err(ToolError::new("path escapes the files root"))
    }
}

fn deny_protected(rel: &str) -> Result<(), ToolError> {
    let base = Path::new(rel)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let protected = base == ".env"
        || base.starts_with(".env.")
        || rel.ends_with(".orchmem")
        || rel.ends_with(".orch")
        || rel.ends_with(".orchml");
    if protected {
        Err(ToolError::new(format!(
            "refusing to write protected file '{rel}'"
        )))
    } else {
        Ok(())
    }
}

/// Format unix seconds as an ISO-8601 UTC string (civil-from-days algorithm).
fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
