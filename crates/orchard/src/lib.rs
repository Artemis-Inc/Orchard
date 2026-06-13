//! Orchard 3.0 — the public embedding API.
//!
//! The single surface every binding (CLI, C-FFI, PyO3, WASM) wraps. Load/check/
//! compile an agent, build a [`Session`] (injecting provider/store/tools), and
//! drive turns. Pure-Rust defaults mean a session builds with zero config.

use std::path::PathBuf;
use std::sync::Arc;

use orchard_ir::{compile_source, dumps, from_ir};
use orchard_runtime::{
    build_pack_tools, AgentRuntime, Engine, Environment, HostError, InMemoryStore, MockProvider,
    PolicyEngine, Provider, Store, Tool,
};
use serde_json::Value as Json;

pub use orchard_runtime::{NativeTool, Value};
// Boundary traits + data types hosts implement/observe.
pub use orchard_runtime::{
    ChatRequest, ChatResponse, Embedder, HttpClient, HttpRequest, HttpResponse, Message,
    ProviderError, ToolCall, ToolDef, ToolError,
};
pub use orchard_runtime::{Provider as ProviderTrait, Store as StoreTrait, Tool as ToolTrait};
pub use orchard_syntax::{suggest, Diagnostic, Severity, Span};

/// The Orchard language/runtime version.
pub const VERSION: &str = orchard_runtime::ORCHARD_VERSION;

/// A facade error.
#[derive(Debug)]
pub enum Error {
    /// Parse/check diagnostics (load/compile failures).
    Diagnostics(Vec<Diagnostic>),
    /// A runtime/host error.
    Host(String),
    /// A configuration error building the session.
    Config(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Diagnostics(d) => write!(f, "{} diagnostic(s)", d.len()),
            Error::Host(s) => write!(f, "{s}"),
            Error::Config(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for Error {}

impl From<HostError> for Error {
    fn from(e: HostError) -> Self {
        Error::Host(e.to_string())
    }
}

/// A validated agent (its IR + manifest).
#[derive(Clone)]
pub struct Agent {
    ir: Json,
}

impl Agent {
    /// Parse + check + lower. Returns diagnostics on failure.
    pub fn load(source: &str, filename: &str) -> Result<Agent, Error> {
        compile_source(source, filename)
            .map(|ir| Agent { ir })
            .map_err(Error::Diagnostics)
    }

    /// Static analysis only (errors + warnings).
    pub fn check(source: &str, filename: &str) -> Vec<Diagnostic> {
        orchard_types::check_source(source, filename)
    }

    /// Lower to the stable JSON IR.
    pub fn compile(source: &str, filename: &str) -> Result<String, Error> {
        compile_source(source, filename)
            .map(|ir| dumps(&ir))
            .map_err(Error::Diagnostics)
    }

    /// Build from precompiled IR JSON.
    pub fn from_ir(ir_json: &str) -> Result<Agent, Error> {
        from_ir(ir_json)
            .map(|ir| Agent { ir })
            .map_err(Error::Config)
    }

    /// The agent manifest (model/memory/persona/policy/tools).
    pub fn manifest(&self) -> &Json {
        &self.ir["manifest"]
    }

    /// The agent's declared name.
    pub fn name(&self) -> &str {
        self.manifest()
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    pub fn ir(&self) -> &Json {
        &self.ir
    }
}

/// Builder for a [`Session`].
pub struct Runtime;

impl Runtime {
    pub fn builder(agent: Agent) -> RuntimeBuilder {
        RuntimeBuilder {
            agent,
            provider: None,
            store: None,
            base_dir: PathBuf::from("."),
            unattended: false,
            native_tools: Vec::new(),
            http_client: None,
        }
    }
}

pub struct RuntimeBuilder {
    agent: Agent,
    provider: Option<Arc<dyn Provider>>,
    store: Option<Arc<dyn Store>>,
    base_dir: PathBuf,
    unattended: bool,
    native_tools: Vec<Arc<dyn Tool>>,
    http_client: Option<Arc<dyn orchard_runtime::HttpClientTrait>>,
}

impl RuntimeBuilder {
    /// Inject a custom provider (route model calls / use a fake).
    pub fn provider(mut self, p: Arc<dyn Provider>) -> Self {
        self.provider = Some(p);
        self
    }
    /// Inject a custom store (share a host DB / in-memory).
    pub fn store(mut self, s: Arc<dyn Store>) -> Self {
        self.store = Some(s);
        self
    }
    /// The base directory for resolving relative paths (mock scripts, files).
    pub fn base_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.base_dir = dir.into();
        self
    }
    /// Mark this as an unattended run (applies the default spend cap).
    pub fn unattended(mut self, yes: bool) -> Self {
        self.unattended = yes;
        self
    }
    /// Register a native (Rust) tool the agent's `delegate` loop can call.
    pub fn register_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.native_tools.push(tool);
        self
    }
    /// Inject a custom HTTP client (egress-guarded by default).
    pub fn http_client(mut self, http: Arc<dyn orchard_runtime::HttpClientTrait>) -> Self {
        self.http_client = Some(http);
        self
    }

    pub fn build(self) -> Result<Session, Error> {
        let manifest = self.agent.manifest().clone();
        let model = &manifest["model"];
        let provider_name = model
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("mock")
            .to_string();
        let model_name = model
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let env = Arc::new(Environment::new());

        // One egress-guarded HTTP client, shared by the provider + http/web packs.
        // An injected client wins; else the native default; else none (wasm).
        let http_client: Option<Arc<dyn orchard_runtime::HttpClientTrait>> =
            self.http_client.clone().or({
                #[cfg(feature = "native")]
                {
                    Some(Arc::new(orchard_runtime::ReqwestClient::new())
                        as Arc<dyn orchard_runtime::HttpClientTrait>)
                }
                #[cfg(not(feature = "native"))]
                {
                    None
                }
            });

        // Provider: injected, else built from the manifest. `mock` resolves
        // offline; remote adapters resolve their API key from env/secrets.
        let provider: Arc<dyn Provider> = match self.provider {
            Some(p) => p,
            None if provider_name == "mock" => Arc::new(
                MockProvider::new(&model_name, &self.base_dir)
                    .map_err(|e| Error::Config(e.message))?,
            ),
            None => {
                let http = http_client.clone().ok_or_else(|| {
                    Error::Config(format!(
                        "provider '{provider_name}' needs a host HttpClient on this target; inject a provider via .provider()"
                    ))
                })?;
                let env2 = env.clone();
                let resolve = move |provider: &str, key_ref: Option<&str>| -> Option<String> {
                    let var = key_ref
                        .and_then(|r| r.strip_prefix("${").and_then(|s| s.strip_suffix('}')))
                        .map(|s| s.to_string())
                        .or_else(|| {
                            orchard_runtime::secrets::provider_key_var(provider)
                                .map(|v| v.to_string())
                        })?;
                    let val = env2.lookup(&var)?;
                    if val.is_empty() {
                        return None;
                    }
                    env2.track_secret(&val, &var);
                    Some(val)
                };
                orchard_runtime::get_provider(&manifest["model"], http, &resolve)
                    .map_err(Error::Config)?
            }
        };

        let store: Arc<dyn Store> = self.store.unwrap_or_else(|| Arc::new(InMemoryStore::new()));
        let policy = Arc::new(PolicyEngine::from_manifest(
            &manifest["policy"],
            self.unattended,
        ));

        let pol = &manifest["policy"];
        let allow_shell = effective_allow_shell(&manifest);
        let pack_ctx = orchard_runtime::PackCtx {
            base_dir: self.base_dir.clone(),
            http: http_client.clone(),
            allowed_domains: pol["allowed_domains"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            allow_local_http: pol
                .get("allow_local_http")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            allow_shell: allow_shell.clone(),
            interactive: !self.unattended,
        };
        let mut tools = build_pack_tools(&manifest, Some(store.clone()), &pack_ctx);
        tools.extend(start_mcp_tools(&manifest, !self.unattended));
        tools.extend(self.native_tools);

        let runtime = Arc::new(AgentRuntime {
            provider,
            store: Some(store),
            policy,
            env,
            manifest,
            provider_name,
            model_name,
            base_dir: self.base_dir,
            tools,
            embedder: None,
            http: http_client,
            allow_shell,
            interactive: !self.unattended,
        });
        runtime.index_knowledge();
        let engine = Arc::new(Engine::new(self.agent.ir(), runtime));
        engine.init_self();
        Ok(Session { engine })
    }
}

/// A live agent session. Reused across turns (memory persists).
pub struct Session {
    engine: Arc<Engine>,
}

impl Session {
    /// Fire `on start` (if present).
    pub async fn start(&self) -> Result<Value, Error> {
        let v = self.engine.dispatch_start().await?;
        Ok(self.engine.redact_value(v))
    }

    /// Drive one `on message` turn.
    pub async fn message(&self, text: &str) -> Result<String, Error> {
        let v = self.engine.dispatch_message(text).await?;
        Ok(self.engine.redact_value(v).to_text())
    }

    /// A one-shot task (via the message handler), same as [`Session::message`].
    pub async fn task(&self, text: &str) -> Result<String, Error> {
        self.message(text).await
    }

    /// Invoke a named skill with labeled args (a JSON object).
    pub async fn skill(&self, name: &str, args: Json) -> Result<Value, Error> {
        let mut map = indexmap_from_json(&args);
        let _ = &mut map;
        let v = self.engine.dispatch_skill(name, map).await?;
        Ok(self.engine.redact_value(v))
    }

    pub fn has_handler(&self, kind: &str) -> bool {
        self.engine.has_handler(kind)
    }

    /// Fire the `on schedule` handler once.
    pub async fn schedule(&self) -> Result<Value, Error> {
        let v = self.engine.dispatch_schedule().await?;
        Ok(self.engine.redact_value(v))
    }

    /// Fire the `on file` handler for `path`.
    pub async fn file(&self, path: &str) -> Result<Value, Error> {
        let v = self.engine.dispatch_file(path).await?;
        Ok(self.engine.redact_value(v))
    }

    /// The schedule spec `(kind, value)` — `kind` is `"every"`/`"cron"`.
    pub fn schedule_spec(&self) -> Option<(String, String)> {
        self.engine.schedule_spec()
    }

    /// The `on file` watch directory.
    pub fn watch_dir(&self) -> Option<String> {
        self.engine.watch_spec()
    }
}

/// `allow_shell` after the external-ingest downgrade: `always` → `ask` when the
/// agent ingests external content (http/web/mcp tools, knowledge urls, or an
/// inline-http body), unless `i_understand_injection_risk`.
fn effective_allow_shell(manifest: &Json) -> String {
    let pol = &manifest["policy"];
    let mode = pol
        .get("allow_shell")
        .and_then(|v| v.as_str())
        .unwrap_or("never")
        .to_string();
    if mode != "always" {
        return mode;
    }
    if pol
        .get("i_understand_injection_risk")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return mode;
    }
    let ingests_inline = manifest
        .get("ingests_external_inline")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ingests_tools = manifest["tools"]
        .as_array()
        .map(|a| {
            a.iter().any(|t| {
                matches!(
                    t.get("name").and_then(|n| n.as_str()),
                    Some("http") | Some("web")
                ) || t.get("kind").and_then(|k| k.as_str()) == Some("mcp")
            })
        })
        .unwrap_or(false);
    let ingests_knowledge = manifest["knowledge"]
        .as_array()
        .map(|a| a.iter().any(|k| k.get("url").is_some()))
        .unwrap_or(false);
    if ingests_inline || ingests_tools || ingests_knowledge {
        "ask".to_string()
    } else {
        mode
    }
}

/// Launch declared MCP servers and collect their namespaced tools. Gated by
/// `allow_mcp` (or an interactive session, which implies user opt-in). A server
/// that fails to launch is logged and skipped — it never aborts the build.
#[cfg(feature = "native")]
fn start_mcp_tools(manifest: &Json, interactive: bool) -> Vec<Arc<dyn Tool>> {
    let allow_mcp = manifest["policy"]
        .get("allow_mcp")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let specs: Vec<(String, String)> = manifest["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|t| t.get("kind").and_then(|k| k.as_str()) == Some("mcp"))
                .filter_map(|t| {
                    let ns = t.get("name").and_then(|n| n.as_str())?;
                    let cmd = t.get("mcp_command").and_then(|c| c.as_str())?;
                    Some((ns.to_string(), cmd.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    if specs.is_empty() {
        return Vec::new();
    }
    if !allow_mcp && !interactive {
        eprintln!(
            "orchard: {} MCP server(s) declared but allow_mcp is false in unattended mode — skipping",
            specs.len()
        );
        return Vec::new();
    }
    let mut tools = Vec::new();
    for (ns, cmd) in specs {
        match orchard_runtime::tools::mcp::McpClient::start(&cmd, &ns) {
            // each tool closure holds an Arc<McpClient>, so the client (and its
            // subprocess) lives exactly as long as its tools are reachable.
            Ok((_client, mut t)) => tools.append(&mut t),
            Err(e) => eprintln!("orchard: MCP server '{ns}' ({cmd}) failed to start: {e}"),
        }
    }
    tools
}

#[cfg(not(feature = "native"))]
fn start_mcp_tools(_manifest: &Json, _interactive: bool) -> Vec<Arc<dyn Tool>> {
    Vec::new()
}

fn indexmap_from_json(args: &Json) -> indexmap::IndexMap<String, Value> {
    let mut m = indexmap::IndexMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            m.insert(k.clone(), Value::from_json(v));
        }
    }
    m
}
