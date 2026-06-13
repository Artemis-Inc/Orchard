//! The tree-walking interpreter over the IR. Async (model/tool nodes are
//! async); control flow uses the [`Flow`] enum. Ports v2's `interp.py`.
//!
//! P6 scope: control flow, expressions, fn/tool/method calls, plain `gen`,
//! memory, and the transactional `state` boundary. `delegate` (P7), `gen as T`
//! (P8), and `spawn`/`await`/`parallel` (P9) return a clear "not yet
//! implemented" host error until their phases land.

use crate::agent::AgentRuntime;
use crate::error::HostError;
use crate::flow::Flow;
use crate::value::{equal, make_error, Closure, Duration, Env, Money, Scope, Value};
use futures::future::{BoxFuture, FutureExt};
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Extract a `Value` from an eval, propagating any control-flow signal.
macro_rules! val {
    ($self:ident, $node:expr, $env:expr) => {
        match $self.eval($node, $env).await? {
            Flow::Value(v) => v,
            sig => return Ok(sig),
        }
    };
}

pub struct Engine {
    pub runtime: Arc<AgentRuntime>,
    fns: HashMap<String, Json>,
    skills: HashMap<String, Json>,
    tools_authored: HashMap<String, Json>,
    handlers: HashMap<String, Json>,
    enum_defs: HashMap<String, Json>,
    record_defs: HashMap<String, Json>,
    variant_to_enum: HashMap<String, String>,
    state_defs: HashMap<String, Json>,
    state_defaults: HashMap<String, Value>,
    global_env: Env,
    state_buffer: Mutex<IndexMap<String, Value>>,
    backend_tools: HashMap<String, usize>,
    delegate_depth: std::sync::atomic::AtomicU32,
    in_retry_until: std::sync::atomic::AtomicBool,
    /// Weak self-handle so `spawn` can move an `Arc<Engine>` into a task.
    me: std::sync::OnceLock<std::sync::Weak<Engine>>,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    future_counter: std::sync::atomic::AtomicU64,
    #[cfg(feature = "native")]
    futures: Mutex<HashMap<u64, tokio::task::JoinHandle<Result<Flow, HostError>>>>,
}

impl Engine {
    pub fn new(ir: &Json, runtime: Arc<AgentRuntime>) -> Engine {
        let mut fns = HashMap::new();
        let mut skills = HashMap::new();
        let mut tools_authored = HashMap::new();
        let mut handlers = HashMap::new();
        let mut enum_defs = HashMap::new();
        let mut record_defs = HashMap::new();
        let mut variant_to_enum = HashMap::new();
        let mut state_defs = HashMap::new();

        let arr = |v: &Json, k: &str| -> Vec<Json> {
            v.get(k)
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default()
        };

        // top-level types/enums/fns
        for t in arr(ir, "types") {
            if let Some(n) = t["name"].as_str() {
                record_defs.insert(n.to_string(), t.clone());
            }
        }
        for e in arr(ir, "enums") {
            register_enum(&e, &mut enum_defs, &mut variant_to_enum);
        }
        for f in arr(ir, "fns") {
            if let Some(n) = f["name"].as_str() {
                fns.insert(n.to_string(), f.clone());
            }
        }
        // first agent's members
        if let Some(agent) = ir
            .get("agents")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
        {
            for t in arr(agent, "types") {
                if let Some(n) = t["name"].as_str() {
                    record_defs.insert(n.to_string(), t.clone());
                }
            }
            for e in arr(agent, "enums") {
                register_enum(&e, &mut enum_defs, &mut variant_to_enum);
            }
            for f in arr(agent, "fns") {
                if let Some(n) = f["name"].as_str() {
                    fns.insert(n.to_string(), f.clone());
                }
            }
            for s in arr(agent, "skills") {
                if let Some(n) = s["name"].as_str() {
                    skills.insert(n.to_string(), s.clone());
                }
            }
            for t in arr(agent, "tools") {
                if let Some(n) = t["name"].as_str() {
                    tools_authored.insert(n.to_string(), t.clone());
                }
            }
            for h in arr(agent, "handlers") {
                if let Some(k) = h["kind"].as_str() {
                    handlers.insert(k.to_string(), h.clone());
                }
            }
            for s in arr(agent, "state") {
                if let Some(n) = s["name"].as_str() {
                    state_defs.insert(n.to_string(), s.clone());
                }
            }
        }

        let global_env = Scope::root();
        // backend (pack/mcp/native) tools registered by the host/facade.
        let mut backend_tools = HashMap::new();
        for (i, t) in runtime.tools.iter().enumerate() {
            backend_tools.insert(t.name().to_string(), i);
        }

        // Evaluate state defaults (constant expressions) once.
        let mut state_defaults = HashMap::new();
        for (name, decl) in &state_defs {
            let v = match decl.get("default") {
                Some(Json::Null) | None => Value::Null,
                Some(node) => eval_const(node),
            };
            state_defaults.insert(name.clone(), v);
        }

        Engine {
            runtime,
            fns,
            skills,
            tools_authored,
            handlers,
            enum_defs,
            record_defs,
            variant_to_enum,
            state_defs,
            state_defaults,
            global_env,
            state_buffer: Mutex::new(IndexMap::new()),
            backend_tools,
            delegate_depth: std::sync::atomic::AtomicU32::new(0),
            in_retry_until: std::sync::atomic::AtomicBool::new(false),
            me: std::sync::OnceLock::new(),
            future_counter: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "native")]
            futures: Mutex::new(HashMap::new()),
        }
    }

    /// Register the engine's own `Arc` so `spawn` can clone it into tasks.
    /// Called by the facade after `Arc::new(Engine::new(...))`.
    pub fn init_self(self: &Arc<Engine>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    pub fn has_handler(&self, kind: &str) -> bool {
        self.handlers.contains_key(kind)
    }

    /// The schedule trigger spec: `(kind, value_text)` where kind is
    /// `"every"`/`"cron"` and value_text is the duration (`"30s"`) or cron expr.
    pub fn schedule_spec(&self) -> Option<(String, String)> {
        let h = self.handlers.get("schedule")?;
        let kind = h["schedule_kind"].as_str()?.to_string();
        let v = &h["schedule_value"];
        let value = v["value"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_default();
        Some((kind, value))
    }

    /// The `on file` watch directory (a string literal).
    pub fn watch_spec(&self) -> Option<String> {
        let h = self.handlers.get("file")?;
        h["watch_path"]["value"].as_str().map(|s| s.to_string())
    }

    // ---- turn dispatch ----

    /// Redact tracked secrets from every string within a value, recursively.
    /// Applied at the host-output boundary so a `reply`/`return`/`emit` that
    /// embeds `env.SECRET` can't leak the live secret to the embedder/logs.
    pub fn redact_value(&self, v: Value) -> Value {
        match v {
            Value::Str(s) => Value::Str(self.runtime.env.redact(&s)),
            Value::List(items) => {
                Value::List(items.into_iter().map(|x| self.redact_value(x)).collect())
            }
            Value::Map(m) => Value::Map(
                m.into_iter()
                    .map(|(k, x)| (k, self.redact_value(x)))
                    .collect(),
            ),
            Value::Record { type_name, fields } => Value::Record {
                type_name,
                fields: fields
                    .into_iter()
                    .map(|(k, x)| (k, self.redact_value(x)))
                    .collect(),
            },
            Value::Enum {
                enum_name,
                variant,
                payload,
            } => Value::Enum {
                enum_name,
                variant,
                payload: payload.into_iter().map(|x| self.redact_value(x)).collect(),
            },
            other => other,
        }
    }

    /// Run the `on message` handler.
    pub async fn dispatch_message(&self, text: &str) -> Result<Value, HostError> {
        let handler = self
            .handlers
            .get("message")
            .cloned()
            .ok_or_else(|| HostError::Internal("no 'on message' handler".into()))?;
        self.run_handler(&handler, Some(("text", Value::str(text))))
            .await
    }

    /// Run a named skill with labeled args.
    pub async fn dispatch_skill(
        &self,
        name: &str,
        args: IndexMap<String, Value>,
    ) -> Result<Value, HostError> {
        let decl = self
            .skills
            .get(name)
            .cloned()
            .ok_or_else(|| HostError::Internal(format!("no skill '{name}'")))?;
        self.runtime.policy.begin_turn();
        *self.state_buffer.lock().unwrap() = IndexMap::new();
        let env = self.global_env.child();
        self.bind_params_map(&decl, &args, &env)?;
        let body = decl["body"].clone();
        self.finish_turn(async { self.run_body(&body, &env).await })
            .await
    }

    /// Run the `on schedule` handler.
    pub async fn dispatch_schedule(&self) -> Result<Value, HostError> {
        match self.handlers.get("schedule").cloned() {
            Some(h) => self.run_handler(&h, None).await,
            None => Ok(Value::Null),
        }
    }

    /// Run the `on file` handler, binding its param to `path`.
    pub async fn dispatch_file(&self, path: &str) -> Result<Value, HostError> {
        match self.handlers.get("file").cloned() {
            Some(h) => {
                let pname = h["param"]["name"].as_str().unwrap_or("path").to_string();
                self.run_handler(&h, Some((&pname, Value::str(path)))).await
            }
            None => Ok(Value::Null),
        }
    }

    /// Run `on start` if present.
    pub async fn dispatch_start(&self) -> Result<Value, HostError> {
        if let Some(handler) = self.handlers.get("start").cloned() {
            return self.run_handler(&handler, None).await;
        }
        Ok(Value::Null)
    }

    async fn run_handler(
        &self,
        handler: &Json,
        param: Option<(&str, Value)>,
    ) -> Result<Value, HostError> {
        self.runtime.policy.begin_turn();
        *self.state_buffer.lock().unwrap() = IndexMap::new();
        let env = self.global_env.child();
        if let Some((name, value)) = param {
            env.define(name, value);
        }
        let body = handler["body"].clone();
        self.finish_turn(async {
            // A handler returns its reply/return/trailing value.
            match self.exec_block(&body, &env).await? {
                Flow::Reply(v) | Flow::Return(v) | Flow::Value(v) => Ok(Flow::Value(v)),
                sig => Ok(sig),
            }
        })
        .await
    }

    /// Commit state on normal/return/halt; roll back on uncaught throw/host error.
    async fn finish_turn(
        &self,
        fut: impl std::future::Future<Output = Result<Flow, HostError>>,
    ) -> Result<Value, HostError> {
        match fut.await {
            Ok(Flow::Value(v)) | Ok(Flow::Return(v)) | Ok(Flow::Reply(v)) => {
                self.flush_state();
                Ok(v)
            }
            Ok(Flow::Halt(reason)) => {
                self.flush_state();
                Err(HostError::Halt(reason))
            }
            Ok(Flow::Throw(err)) => {
                self.state_buffer.lock().unwrap().clear();
                let msg = error_message(&err);
                Err(HostError::Internal(format!("uncaught error: {msg}")))
            }
            Ok(Flow::Break) | Ok(Flow::Continue) => {
                self.flush_state();
                Ok(Value::Null)
            }
            Err(e) => {
                self.state_buffer.lock().unwrap().clear();
                Err(e)
            }
        }
    }

    fn flush_state(&self) {
        let buffer = std::mem::take(&mut *self.state_buffer.lock().unwrap());
        if buffer.is_empty() {
            return;
        }
        if let Some(store) = &self.runtime.store {
            let items: Vec<(String, Json)> = buffer
                .iter()
                .map(|(k, v)| (k.clone(), v.to_jsonable()))
                .collect();
            store.set_state_batch(&items);
        }
    }

    /// Run a fn/skill/tool body, catching `return`.
    fn run_body<'a>(
        &'a self,
        body: &'a Json,
        env: &'a Env,
    ) -> BoxFuture<'a, Result<Flow, HostError>> {
        async move {
            match self.exec_block(body, env).await? {
                Flow::Return(v) => Ok(Flow::Value(v)),
                Flow::Value(v) => Ok(Flow::Value(v)),
                sig => Ok(sig),
            }
        }
        .boxed()
    }

    // ---- blocks ----

    fn exec_block<'a>(
        &'a self,
        node: &'a Json,
        env: &'a Env,
    ) -> BoxFuture<'a, Result<Flow, HostError>> {
        async move {
            let child = env.child();
            let mut result = Value::Null;
            let empty = Vec::new();
            let body = node
                .get("body")
                .and_then(|b| b.as_array())
                .unwrap_or(&empty);
            for stmt in body {
                match self.eval(stmt, &child).await? {
                    Flow::Value(v) => result = v,
                    sig => return Ok(sig),
                }
            }
            Ok(Flow::Value(result))
        }
        .boxed()
    }

    // ---- the main dispatch ----

    fn eval<'a>(&'a self, node: &'a Json, env: &'a Env) -> BoxFuture<'a, Result<Flow, HostError>> {
        async move {
            let tag = node["node"].as_str().unwrap_or("");
            match tag {
                // ---- statements ----
                "block" => self.exec_block(node, env).await,
                "bind" => {
                    let v = val!(self, &node["value"], env);
                    let name = node["name"].as_str().unwrap_or("");
                    env.define(name, v);
                    Ok(Flow::null())
                }
                "assign" => self.exec_assign(node, env).await,
                "if" => self.exec_if(node, env).await,
                "for" => self.exec_for(node, env).await,
                "while" => self.exec_while(node, env).await,
                "repeat" => self.exec_repeat(node, env).await,
                "return" => {
                    let v = match node.get("value") {
                        Some(Json::Null) | None => Value::Null,
                        Some(n) => val!(self, n, env),
                    };
                    Ok(Flow::Return(v))
                }
                "break" => Ok(Flow::Break),
                "continue" => Ok(Flow::Continue),
                "throw" => {
                    let v = val!(self, &node["value"], env);
                    Ok(Flow::Throw(to_thrown(v)))
                }
                "try" => self.exec_try(node, env).await,
                "remember" => self.exec_remember(node, env).await,
                "forget" => {
                    let key = val!(self, &node["target"], env).to_text();
                    if let Some(store) = &self.runtime.store {
                        store.forget(&key);
                    }
                    Ok(Flow::null())
                }
                "reply" => {
                    let v = val!(self, &node["value"], env);
                    Ok(Flow::Reply(v))
                }
                "emit" => {
                    let _v = val!(self, &node["value"], env);
                    // streamed output; surfaced via events in the facade later.
                    Ok(Flow::null())
                }
                "halt" => {
                    let v = val!(self, &node["value"], env);
                    Ok(Flow::Halt(v.to_text()))
                }
                // ---- expressions ----
                "lit" => Ok(Flow::Value(eval_const(node))),
                "interp" => self.eval_interp(node, env).await,
                "ref" => self.eval_ref(node, env),
                "this" => Ok(Flow::Value(Value::This)),
                "binop" => self.eval_binop(node, env).await,
                "unop" => self.eval_unop(node, env).await,
                "member" => self.eval_member(node, env).await,
                "index" => self.eval_index(node, env).await,
                "call" => self.eval_call(node, env).await,
                "list" => {
                    let mut items = Vec::new();
                    for it in node["items"].as_array().unwrap_or(&vec![]) {
                        items.push(val!(self, it, env));
                    }
                    Ok(Flow::Value(Value::List(items)))
                }
                "map" => self.eval_map(node, env).await,
                "configlit" => self.eval_configlit(node, env).await,
                "lambda" => Ok(Flow::Value(Value::Closure(Arc::new(Closure {
                    node: node.clone(),
                    env: env.clone(),
                })))),
                "match" => self.eval_match(node, env).await,
                "range" => {
                    let lo = val!(self, &node["lo"], env);
                    let hi = val!(self, &node["hi"], env);
                    let inclusive = node["inclusive"].as_bool().unwrap_or(false);
                    let (lo, hi) = (as_int(&lo), as_int(&hi));
                    Ok(Flow::Value(Value::Range {
                        start: lo,
                        end: if inclusive { hi + 1 } else { hi },
                    }))
                }
                "gen" => self.eval_gen(node, env).await,
                "recall" => self.eval_recall(node, env).await,
                "retry" => self.eval_retry(node, env).await,
                "budget" => self.eval_budget(node, env).await,
                "delegate" => self.eval_delegate(node, env).await,
                "spawn" => self.eval_spawn(node, env).await,
                "await" => self.eval_await(node, env).await,
                "parallel" => self.eval_parallel(node, env).await,
                other => Err(HostError::Internal(format!("unknown IR node '{other}'"))),
            }
        }
        .boxed()
    }

    // ---- statement helpers ----

    async fn exec_if(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let empty = vec![];
        for branch in node["branches"].as_array().unwrap_or(&empty) {
            let cond = val!(self, &branch["cond"], env);
            if cond.truthy() {
                return self.exec_block(&branch["then"], env).await;
            }
        }
        if let Some(eb) = node.get("else_block") {
            if !eb.is_null() {
                return self.exec_block(eb, env).await;
            }
        }
        Ok(Flow::null())
    }

    async fn exec_for(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let iter = val!(self, &node["iter"], env);
        let var = node["var"].as_str().unwrap_or("");
        let items = match iterate(&iter) {
            Some(items) => items,
            None => {
                return Ok(Flow::Throw(make_error(
                    "value is not iterable",
                    "type",
                    None,
                )))
            }
        };
        for item in items {
            let child = env.child();
            child.define(var, item);
            match self.exec_block(&node["body"], &child).await? {
                Flow::Break => break,
                Flow::Continue | Flow::Value(_) => {}
                sig => return Ok(sig),
            }
        }
        Ok(Flow::null())
    }

    async fn exec_while(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let mut guard = 0u64;
        loop {
            let cond = val!(self, &node["cond"], env);
            if !cond.truthy() {
                break;
            }
            guard += 1;
            if guard > 1_000_000 {
                return Ok(Flow::Throw(make_error(
                    "while loop exceeded 1000000 iterations",
                    "runtime",
                    None,
                )));
            }
            match self.exec_block(&node["body"], env).await? {
                Flow::Break => break,
                Flow::Continue | Flow::Value(_) => {}
                sig => return Ok(sig),
            }
        }
        Ok(Flow::null())
    }

    async fn exec_repeat(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let count = val!(self, &node["count"], env);
        // Mirror v2's `int(count)`: ints, floats (truncated), bools, and numeric
        // strings are all accepted; anything else throws.
        let n = match &count {
            Value::Int(i) => *i,
            Value::Float(f) => *f as i64,
            Value::Bool(b) => *b as i64,
            Value::Str(s) => match s.trim().parse::<i64>() {
                Ok(i) => i,
                Err(_) => match s.trim().parse::<f64>() {
                    Ok(f) => f as i64,
                    Err(_) => {
                        return Ok(Flow::Throw(make_error(
                            "repeat count must be an integer",
                            "type",
                            None,
                        )))
                    }
                },
            },
            _ => {
                return Ok(Flow::Throw(make_error(
                    "repeat count must be an integer",
                    "type",
                    None,
                )))
            }
        };
        for _ in 0..n.max(0) {
            match self.exec_block(&node["body"], env).await? {
                Flow::Break => break,
                Flow::Continue | Flow::Value(_) => {}
                sig => return Ok(sig),
            }
        }
        Ok(Flow::null())
    }

    async fn exec_try(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        match self.exec_block(&node["body"], env).await? {
            Flow::Throw(err) => {
                let child = env.child();
                if let Some(name) = node["catch_name"].as_str() {
                    child.define(name, err);
                }
                self.exec_block(&node["catch"], &child).await
            }
            other => Ok(other),
        }
    }

    async fn exec_remember(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let value = val!(self, &node["value"], env).to_text();
        let auto = node["auto_key"].as_bool().unwrap_or(false);
        let key = if auto {
            let digest = sha1_12(&value);
            format!("auto:{digest}")
        } else {
            val!(self, &node["key"], env).to_text()
        };
        if let Some(store) = &self.runtime.store {
            store.remember(&key, &value);
        }
        Ok(Flow::null())
    }

    async fn exec_assign(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let op = node["op"].as_str().unwrap_or("=");
        let rhs = val!(self, &node["value"], env);
        let target = &node["target"];
        // Resolve the lvalue to a root ref plus a path of keys.
        let (root, path) = match self.lvalue_path(target, env).await? {
            Ok(rp) => rp,
            Err(sig) => return Ok(sig),
        };
        let is_local = env.has(&root);
        let is_state = !is_local && self.state_defs.contains_key(&root);
        if !is_local && !is_state {
            return Ok(Flow::Throw(make_error(
                format!("undefined name '{root}'"),
                "name",
                None,
            )));
        }
        if path.is_empty() {
            // scalar assign
            let cur = if op == "=" {
                Value::Null
            } else if is_local {
                env.get(&root).unwrap_or(Value::Null)
            } else {
                self.read_state(&root)
            };
            let newv = match combine(op, cur, rhs) {
                Ok(v) => v,
                Err(t) => return Ok(t),
            };
            if is_local {
                env.assign(&root, newv);
            } else {
                self.state_buffer.lock().unwrap().insert(root, newv);
            }
            return Ok(Flow::null());
        }
        // path assign: read container, navigate, set, write back.
        let mut container = if is_local {
            env.get(&root).unwrap_or(Value::Null)
        } else {
            self.read_state(&root)
        };
        let cur = get_in(&container, &path);
        let newv = match combine(op, cur, rhs) {
            Ok(v) => v,
            Err(t) => return Ok(t),
        };
        if let Err(t) = set_in(&mut container, &path, newv) {
            return Ok(t);
        }
        if is_local {
            env.assign(&root, container);
        } else {
            self.state_buffer.lock().unwrap().insert(root, container);
        }
        Ok(Flow::null())
    }

    /// Resolve an lvalue to `(root_ref_name, path)`. The path is field/index
    /// keys from outside in. A `state.x`/`this.x` base resolves to root `x`.
    async fn lvalue_path(
        &self,
        target: &Json,
        env: &Env,
    ) -> Result<Result<(String, Vec<Key>), Flow>, HostError> {
        let mut path: Vec<Key> = Vec::new();
        let mut cur = target;
        loop {
            match cur["node"].as_str().unwrap_or("") {
                "ref" => {
                    let name = cur["name"].as_str().unwrap_or("").to_string();
                    path.reverse();
                    return Ok(Ok((name, path)));
                }
                "member" => {
                    let obj = &cur["obj"];
                    let name = cur["name"].as_str().unwrap_or("").to_string();
                    // state.x / this.x base → root is x
                    let is_state_root = matches!(obj["node"].as_str(), Some("ref") if obj["name"].as_str() == Some("state"))
                        || obj["node"].as_str() == Some("this");
                    if is_state_root {
                        path.reverse();
                        return Ok(Ok((name, path)));
                    }
                    path.push(Key::Field(name));
                    cur = obj;
                }
                "index" => {
                    let idx = val_or_sig(self.eval(&cur["index"], env).await?)?;
                    let idx = match idx {
                        Ok(v) => v,
                        Err(sig) => return Ok(Err(sig)),
                    };
                    path.push(Key::Index(idx));
                    cur = &cur["obj"];
                }
                _ => {
                    return Ok(Err(Flow::Throw(make_error(
                        "invalid assignment target",
                        "type",
                        None,
                    ))));
                }
            }
        }
    }

    // ---- expression helpers ----

    async fn eval_interp(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let mut out = String::new();
        let empty = vec![];
        for part in node["parts"].as_array().unwrap_or(&empty) {
            if let Some(s) = part.as_str() {
                out.push_str(s);
            } else {
                let v = val!(self, part, env);
                out.push_str(&v.to_text());
            }
        }
        Ok(Flow::Value(Value::Str(out)))
    }

    fn eval_ref(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let name = node["name"].as_str().unwrap_or("");
        if let Some(v) = env.get(name) {
            return Ok(Flow::Value(v));
        }
        if self.state_defs.contains_key(name) {
            return Ok(Flow::Value(self.read_state(name)));
        }
        if let Some(enum_name) = self.variant_to_enum.get(name) {
            return Ok(Flow::Value(Value::Enum {
                enum_name: enum_name.clone(),
                variant: name.to_string(),
                payload: vec![],
            }));
        }
        if self.enum_defs.contains_key(name) {
            return Ok(Flow::Value(Value::EnumTypeRef {
                enum_name: name.to_string(),
            }));
        }
        if self.fns.contains_key(name) {
            return Ok(Flow::Value(Value::FunctionRef {
                kind: "fn".into(),
                name: name.into(),
            }));
        }
        if self.skills.contains_key(name) {
            return Ok(Flow::Value(Value::FunctionRef {
                kind: "skill".into(),
                name: name.into(),
            }));
        }
        if self.tools_authored.contains_key(name) {
            return Ok(Flow::Value(Value::FunctionRef {
                kind: "tool".into(),
                name: name.into(),
            }));
        }
        if self.backend_tools.contains_key(name) {
            return Ok(Flow::Value(Value::FunctionRef {
                kind: "backend".into(),
                name: name.into(),
            }));
        }
        Ok(Flow::Throw(make_error(
            format!("undefined name '{name}'"),
            "name",
            None,
        )))
    }

    async fn eval_binop(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let op = node["op"].as_str().unwrap_or("");
        // short-circuit forms
        match op {
            "and" => {
                let l = val!(self, &node["left"], env);
                return if l.truthy() {
                    Ok(Flow::Value(val!(self, &node["right"], env)))
                } else {
                    Ok(Flow::Value(l))
                };
            }
            "or" => {
                let l = val!(self, &node["left"], env);
                return if l.truthy() {
                    Ok(Flow::Value(l))
                } else {
                    Ok(Flow::Value(val!(self, &node["right"], env)))
                };
            }
            "??" => {
                let l = val!(self, &node["left"], env);
                return if matches!(l, Value::Null) {
                    Ok(Flow::Value(val!(self, &node["right"], env)))
                } else {
                    Ok(Flow::Value(l))
                };
            }
            "|>" => return self.eval_pipe(node, env).await,
            _ => {}
        }
        let l = val!(self, &node["left"], env);
        let r = val!(self, &node["right"], env);
        Ok(binop(op, l, r))
    }

    async fn eval_unop(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let op = node["op"].as_str().unwrap_or("");
        let v = val!(self, &node["operand"], env);
        match op {
            "not" => Ok(Flow::Value(Value::Bool(!v.truthy()))),
            "-" => match v {
                Value::Int(i) => Ok(Flow::Value(Value::Int(-i))),
                Value::Float(f) => Ok(Flow::Value(Value::Float(-f))),
                Value::Money(m) => Ok(Flow::Value(Value::Money(Money::from_decimal(-m.amount)))),
                _ => Ok(Flow::Throw(make_error(
                    "cannot negate non-number",
                    "type",
                    None,
                ))),
            },
            _ => Ok(Flow::Throw(make_error(
                format!("unknown unary op '{op}'"),
                "type",
                None,
            ))),
        }
    }

    async fn eval_member(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let obj = &node["obj"];
        let name = node["name"].as_str().unwrap_or("");
        let optional = node["optional"].as_bool().unwrap_or(false);
        // env.NAME
        if obj["node"].as_str() == Some("ref")
            && obj["name"].as_str() == Some("env")
            && !env.has("env")
        {
            return self.builtin_env(name);
        }
        // state.field
        if obj["node"].as_str() == Some("ref")
            && obj["name"].as_str() == Some("state")
            && self.state_defs.contains_key(name)
        {
            return Ok(Flow::Value(self.read_state(name)));
        }
        // this.field
        if obj["node"].as_str() == Some("this") && self.state_defs.contains_key(name) {
            return Ok(Flow::Value(self.read_state(name)));
        }
        let base = val!(self, obj, env);
        if optional && matches!(base, Value::Null) {
            return Ok(Flow::Value(Value::Null));
        }
        Ok(member_get(&base, name, optional))
    }

    fn builtin_env(&self, name: &str) -> Result<Flow, HostError> {
        match self.runtime.env.lookup(name) {
            Some(v) if !v.is_empty() => {
                self.runtime.env.track_secret(&v, name);
                Ok(Flow::Value(Value::Str(v)))
            }
            _ => Err(HostError::Env(name.to_string())),
        }
    }

    /// `http.METHOD(url, headers:, body:)` in a tool/skill body.
    async fn builtin_http(
        &self,
        method: &str,
        pos: Vec<Value>,
        labeled: IndexMap<String, Value>,
    ) -> Result<Flow, HostError> {
        let http = match &self.runtime.http {
            Some(h) => h,
            None => {
                return Ok(Flow::Throw(make_error(
                    "http is unavailable on this target",
                    "http",
                    None,
                )))
            }
        };
        let url = labeled
            .get("url")
            .cloned()
            .or_else(|| pos.first().cloned())
            .map(|v| v.to_text())
            .unwrap_or_default();
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(Value::Map(h)) = labeled.get("headers") {
            for (k, v) in h {
                headers.push((k.clone(), v.to_text()));
            }
        }
        let body: Option<Vec<u8>> = match labeled.get("body") {
            Some(Value::Str(s)) => {
                headers.push(("Content-Type".into(), "text/plain".into()));
                Some(s.as_bytes().to_vec())
            }
            Some(other) => {
                headers.push(("Content-Type".into(), "application/json".into()));
                Some(other.to_jsonable().to_string().into_bytes())
            }
            None => None,
        };
        let pol = &self.runtime.manifest["policy"];
        let allowed_domains: Vec<String> = pol["allowed_domains"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let allow_local = pol
            .get("allow_local_http")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let req = crate::traits::HttpRequest {
            method: method.to_uppercase(),
            url,
            headers,
            body,
            timeout_secs: 60,
            allowed_domains,
            allow_local,
            enforce_egress: true,
        };
        match http.request(req).await {
            Ok(resp) => {
                let text = String::from_utf8_lossy(&resp.body);
                let parsed = serde_json::from_str::<Json>(&text)
                    .map(|j| Value::from_json(&j))
                    .unwrap_or_else(|_| Value::Str(text.into_owned()));
                Ok(Flow::Value(parsed))
            }
            Err(e) => Ok(Flow::Throw(make_error(e.message, "http", None))),
        }
    }

    /// `shell(command)` in a tool/skill body (policy-gated).
    async fn builtin_shell(&self, pos: &[Value]) -> Result<Flow, HostError> {
        let command = pos.first().map(|v| v.to_text()).unwrap_or_default();
        if let Err(msg) =
            crate::tools::shell::gate(&self.runtime.allow_shell, self.runtime.interactive)
        {
            return Ok(Flow::Throw(make_error(msg, "shell", None)));
        }
        let base = self.runtime.base_dir.clone();
        #[cfg(feature = "native")]
        {
            let result = tokio::task::spawn_blocking(move || {
                crate::tools::shell::execute(&command, &base, 60)
            })
            .await
            .map_err(|e| HostError::Internal(e.to_string()))?;
            match result {
                Ok(v) => Ok(Flow::Value(
                    v.get("stdout")
                        .and_then(|s| s.as_str())
                        .map(|s| Value::Str(s.to_string()))
                        .unwrap_or(Value::Null),
                )),
                Err(e) => Ok(Flow::Throw(make_error(e.0, "shell", None))),
            }
        }
        #[cfg(not(feature = "native"))]
        {
            let _ = (base, command);
            Ok(Flow::Throw(make_error(
                "shell is unavailable on this target",
                "shell",
                None,
            )))
        }
    }

    async fn eval_index(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let obj = val!(self, &node["obj"], env);
        let idx = val!(self, &node["index"], env);
        Ok(index_get(&obj, &idx))
    }

    async fn eval_map(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let mut m = IndexMap::new();
        let empty = vec![];
        for entry in node["entries"].as_array().unwrap_or(&empty) {
            let k = val!(self, &entry["key"], env);
            let v = val!(self, &entry["value"], env);
            let key = match k {
                Value::Str(s) => s,
                other => other.to_text(),
            };
            m.insert(key, v);
        }
        Ok(Flow::Value(Value::Map(m)))
    }

    async fn eval_configlit(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let type_name = node["type_name"].as_str().map(|s| s.to_string());
        let mut fields = IndexMap::new();
        let empty = vec![];
        for f in node["fields"].as_array().unwrap_or(&empty) {
            let name = f["name"].as_str().unwrap_or("").to_string();
            let v = val!(self, &f["value"], env);
            fields.insert(name, v);
        }
        match type_name {
            None => {
                // anonymous record → plain map
                Ok(Flow::Value(Value::Map(fields)))
            }
            Some(tn) => {
                // fill declared-field defaults
                if let Some(rec) = self.record_defs.get(&tn) {
                    for fd in rec["fields"].as_array().unwrap_or(&empty) {
                        let fname = fd["name"].as_str().unwrap_or("");
                        if !fields.contains_key(fname) {
                            if let Some(def) = fd.get("default") {
                                if !def.is_null() {
                                    fields.insert(fname.to_string(), eval_const(def));
                                }
                            }
                        }
                    }
                }
                Ok(Flow::Value(Value::Record {
                    type_name: Some(tn),
                    fields,
                }))
            }
        }
    }

    async fn eval_match(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let subject = val!(self, &node["subject"], env);
        let empty = vec![];
        for arm in node["arms"].as_array().unwrap_or(&empty) {
            if let Some(binds) = self.match_pattern(&arm["pattern"], &subject).await? {
                let child = env.child();
                for (k, v) in binds {
                    child.define(&k, v);
                }
                return self.eval(&arm["body"], &child).await;
            }
        }
        Ok(Flow::Throw(make_error(
            "no match arm matched",
            "match",
            None,
        )))
    }

    /// Returns `Some(bindings)` if the pattern matches.
    async fn match_pattern(
        &self,
        pat: &Json,
        subject: &Value,
    ) -> Result<Option<Vec<(String, Value)>>, HostError> {
        match pat["kind"].as_str().unwrap_or("") {
            "wildcard" => Ok(Some(vec![])),
            "literal" => {
                // Literal patterns are evaluated in the global env (matching v2),
                // so a local binding can't shadow names used inside the pattern.
                let v = match self.eval(&pat["value"], &self.global_env).await? {
                    Flow::Value(v) => v,
                    _ => return Ok(None),
                };
                Ok(if equal(&v, subject) {
                    Some(vec![])
                } else {
                    None
                })
            }
            "enum" => {
                let name = pat["name"].as_str().unwrap_or("");
                if let Value::Enum {
                    variant, payload, ..
                } = subject
                {
                    if variant == name {
                        let binds: Vec<(String, Value)> = pat["binds"]
                            .as_array()
                            .unwrap_or(&vec![])
                            .iter()
                            .enumerate()
                            .map(|(i, b)| {
                                (
                                    b.as_str().unwrap_or("").to_string(),
                                    payload.get(i).cloned().unwrap_or(Value::Null),
                                )
                            })
                            .collect();
                        return Ok(Some(binds));
                    }
                }
                Ok(None)
            }
            "ident" => {
                let name = pat["name"].as_str().unwrap_or("");
                if let Value::Enum {
                    enum_name, variant, ..
                } = subject
                {
                    // variant-name pattern matches only that variant
                    if self
                        .variant_to_enum
                        .get(name)
                        .map(|e| e == enum_name)
                        .unwrap_or(false)
                    {
                        return Ok(if variant == name { Some(vec![]) } else { None });
                    }
                }
                // irrefutable binding
                Ok(Some(vec![(name.to_string(), subject.clone())]))
            }
            _ => Ok(None),
        }
    }

    async fn eval_pipe(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let lhs = val!(self, &node["left"], env);
        let rhs = &node["right"];
        if rhs["node"].as_str() == Some("call") {
            // f(lhs, ...args)
            let (mut pos, labeled) = match self.eval_args(rhs, env).await? {
                Ok(a) => a,
                Err(sig) => return Ok(sig),
            };
            pos.insert(0, lhs);
            return self.invoke_callee(&rhs["callee"], pos, labeled, env).await;
        }
        // bare callee: rhs(lhs)
        self.invoke_callee(rhs, vec![lhs], IndexMap::new(), env)
            .await
    }

    // ---- calls ----

    async fn eval_args(
        &self,
        call: &Json,
        env: &Env,
    ) -> Result<Result<(Vec<Value>, IndexMap<String, Value>), Flow>, HostError> {
        let mut pos = Vec::new();
        let mut labeled = IndexMap::new();
        let empty = vec![];
        for a in call["args"].as_array().unwrap_or(&empty) {
            let v = match self.eval(&a["value"], env).await? {
                Flow::Value(v) => v,
                sig => return Ok(Err(sig)),
            };
            if let Some(label) = a["label"].as_str() {
                labeled.insert(label.to_string(), v);
            } else {
                pos.push(v);
            }
        }
        Ok(Ok((pos, labeled)))
    }

    async fn eval_call(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let (pos, labeled) = match self.eval_args(node, env).await? {
            Ok(a) => a,
            Err(sig) => return Ok(sig),
        };
        self.invoke_callee(&node["callee"], pos, labeled, env).await
    }

    fn invoke_callee<'a>(
        &'a self,
        callee: &'a Json,
        pos: Vec<Value>,
        labeled: IndexMap<String, Value>,
        env: &'a Env,
    ) -> BoxFuture<'a, Result<Flow, HostError>> {
        async move {
            match callee["node"].as_str().unwrap_or("") {
                "ref" => {
                    let name = callee["name"].as_str().unwrap_or("");
                    self.invoke_named(name, pos, labeled, env).await
                }
                "member" => {
                    let obj = &callee["obj"];
                    let mname = callee["name"].as_str().unwrap_or("");
                    if obj["node"].as_str() == Some("ref")
                        && obj["name"].as_str() == Some("http")
                        && !env.has("http")
                    {
                        return self.builtin_http(mname, pos, labeled).await;
                    }
                    let recv = val!(self, obj, env);
                    if let Value::EnumTypeRef { enum_name } = &recv {
                        return Ok(Flow::Value(Value::Enum {
                            enum_name: enum_name.clone(),
                            variant: mname.to_string(),
                            payload: pos,
                        }));
                    }
                    Ok(call_method(&recv, mname, &pos, self, env).await?)
                }
                _ => {
                    let v = val!(self, callee, env);
                    self.call_value(v, pos, labeled, env).await
                }
            }
        }
        .boxed()
    }

    async fn invoke_named(
        &self,
        name: &str,
        pos: Vec<Value>,
        labeled: IndexMap<String, Value>,
        env: &Env,
    ) -> Result<Flow, HostError> {
        // local closure/funcref
        if let Some(v) = env.get(name) {
            if matches!(v, Value::Closure(_) | Value::FunctionRef { .. }) {
                return self.call_value(v, pos, labeled, env).await;
            }
        }
        if let Some(decl) = self
            .fns
            .get(name)
            .or_else(|| self.skills.get(name))
            .or_else(|| self.tools_authored.get(name))
        {
            let decl = decl.clone();
            return self.call_user(&decl, pos, labeled).await;
        }
        if let Some(&i) = self.backend_tools.get(name) {
            // Direct backend-tool call from code: map positional args to the
            // schema's property order, labeled wins.
            let tool = &self.runtime.tools[i];
            let mut obj = serde_json::Map::new();
            let props: Vec<String> = tool.schema()["properties"]
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            for (idx, key) in props.iter().enumerate() {
                if let Some(v) = labeled.get(key) {
                    obj.insert(key.clone(), v.to_jsonable());
                } else if let Some(v) = pos.get(idx) {
                    obj.insert(key.clone(), v.to_jsonable());
                }
            }
            return match tool.call(Json::Object(obj)).await {
                Ok(v) => Ok(Flow::Value(Value::from_json(&v))),
                Err(e) => Ok(Flow::Throw(make_error(e.0, "tool", None))),
            };
        }
        if name == "recall_one" {
            let q = pos.first().map(|v| v.to_text()).unwrap_or_default();
            let first = self
                .runtime
                .store
                .as_ref()
                .and_then(|s| s.recall(&q).into_iter().next());
            return Ok(Flow::Value(
                first.map(|(_, v)| Value::Str(v)).unwrap_or(Value::Null),
            ));
        }
        if name == "shell" {
            return self.builtin_shell(&pos).await;
        }
        if let Some(enum_name) = self.variant_to_enum.get(name) {
            return Ok(Flow::Value(Value::Enum {
                enum_name: enum_name.clone(),
                variant: name.to_string(),
                payload: pos,
            }));
        }
        Ok(Flow::Throw(make_error(
            format!("unknown function '{name}'"),
            "name",
            None,
        )))
    }

    fn call_user<'a>(
        &'a self,
        decl: &'a Json,
        pos: Vec<Value>,
        labeled: IndexMap<String, Value>,
    ) -> BoxFuture<'a, Result<Flow, HostError>> {
        async move {
            let env = self.global_env.child();
            if let Err(t) = self.bind_params(decl, &pos, &labeled, &env) {
                return Ok(t);
            }
            self.run_body(&decl["body"], &env).await
        }
        .boxed()
    }

    fn bind_params(
        &self,
        decl: &Json,
        pos: &[Value],
        labeled: &IndexMap<String, Value>,
        env: &Env,
    ) -> Result<(), Flow> {
        let empty = vec![];
        let params = decl["params"].as_array().unwrap_or(&empty);
        let mut pi = 0usize;
        for p in params {
            let pname = p["name"].as_str().unwrap_or("");
            if let Some(v) = labeled.get(pname) {
                env.define(pname, v.clone());
            } else if pi < pos.len() {
                env.define(pname, pos[pi].clone());
                pi += 1;
            } else if let Some(def) = p.get("default") {
                if !def.is_null() {
                    env.define(pname, eval_const(def));
                } else {
                    return Err(Flow::Throw(make_error(
                        format!("missing argument '{pname}'"),
                        "argument",
                        None,
                    )));
                }
            } else {
                return Err(Flow::Throw(make_error(
                    format!("missing argument '{pname}'"),
                    "argument",
                    None,
                )));
            }
        }
        Ok(())
    }

    fn bind_params_map(
        &self,
        decl: &Json,
        args: &IndexMap<String, Value>,
        env: &Env,
    ) -> Result<(), HostError> {
        let empty = vec![];
        for p in decl["params"].as_array().unwrap_or(&empty) {
            let pname = p["name"].as_str().unwrap_or("");
            if let Some(v) = args.get(pname) {
                env.define(pname, v.clone());
            } else if let Some(def) = p.get("default") {
                if !def.is_null() {
                    env.define(pname, eval_const(def));
                } else {
                    return Err(HostError::Internal(format!("missing argument '{pname}'")));
                }
            }
        }
        Ok(())
    }

    fn call_value<'a>(
        &'a self,
        v: Value,
        pos: Vec<Value>,
        labeled: IndexMap<String, Value>,
        env: &'a Env,
    ) -> BoxFuture<'a, Result<Flow, HostError>> {
        async move {
            match v {
                Value::Closure(c) => {
                    let cenv = c.env.child();
                    if let Err(t) = self.bind_params(&c.node, &pos, &labeled, &cenv) {
                        return Ok(t);
                    }
                    // lambda body: block or expr; `return` propagates out (caught by enclosing fn).
                    self.eval(&c.node["body"], &cenv).await
                }
                Value::FunctionRef { name, .. } => {
                    self.invoke_named(&name, pos, labeled, env).await
                }
                _ => Ok(Flow::Throw(make_error(
                    "value is not callable",
                    "type",
                    None,
                ))),
            }
        }
        .boxed()
    }

    // ---- gen / recall / retry / budget ----

    async fn eval_gen(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let prompt = val!(self, &node["prompt"], env).to_text();
        let (temp, maxt, context) = self.gen_overrides(&node["with_config"], env).await?;
        if node["as_type"].is_null() {
            let text = self
                .runtime
                .generate(&prompt, None, temp, maxt, &context)
                .await?;
            return Ok(Flow::Value(Value::Str(text)));
        }
        self.gen_as_type(&node["as_type"], &prompt, temp, maxt, &context)
            .await
    }

    /// Constrained generation: validate-and-retry into a typed value.
    async fn gen_as_type(
        &self,
        type_node: &Json,
        prompt: &str,
        temp: Option<f64>,
        maxt: Option<i64>,
        context: &[String],
    ) -> Result<Flow, HostError> {
        use std::sync::atomic::Ordering;
        let target = self.ir_type_to_type(type_node, &mut Vec::new());
        let schema = orchard_types::to_json_schema(&target, &std::collections::BTreeMap::new());
        let schema = if schema.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            None
        } else {
            Some(schema)
        };
        let cap = if self.in_retry_until.load(Ordering::SeqCst) {
            1
        } else {
            1 + self.max_gen_retries()
        };
        let mut last_error = String::new();
        let mut last_text = String::new();
        for i in 0..cap {
            let call_prompt = if i == 0 {
                prompt.to_string()
            } else {
                format!("{prompt}\n\n{}", gen_retry_hint(&target, &last_error))
            };
            last_text = self
                .runtime
                .generate(&call_prompt, schema.clone(), temp, maxt, context)
                .await?;
            match crate::coerce::coerce_to_type(&last_text, &target) {
                Ok(v) => return Ok(Flow::Value(v)),
                Err(e) => last_error = e.0,
            }
        }
        let msg = format!(
            "gen as {} did not produce a valid value: {}",
            target.display(),
            if last_error.is_empty() {
                "no valid reply".to_string()
            } else {
                last_error
            }
        );
        Ok(Flow::Throw(make_error(
            msg,
            "GenError",
            Some(Value::Str(last_text)),
        )))
    }

    fn max_gen_retries(&self) -> i64 {
        self.runtime.manifest["policy"]
            .get("max_gen_retries")
            .and_then(|v| v.as_i64())
            .filter(|n| *n >= 0)
            .unwrap_or(2)
    }

    /// Convert an IR type node to an `orchard_types::Type`, resolving named
    /// enums/records from the agent's decls (with a recursion guard).
    fn ir_type_to_type(&self, node: &Json, seen: &mut Vec<String>) -> orchard_types::Type {
        use orchard_types::{EnumType, RecordType, Type};
        if node.is_null() {
            return Type::any();
        }
        let name = node["name"].as_str().unwrap_or("");
        let optional = node["optional"].as_bool().unwrap_or(false);
        let empty = vec![];
        let args = node["args"].as_array().unwrap_or(&empty);
        let base = match name {
            "str" | "int" | "float" | "bool" | "null" | "duration" | "money" | "bytes" | "json"
            | "any" => Type::prim(name),
            "list" => {
                let elem = args
                    .first()
                    .map(|a| self.ir_type_to_type(a, seen))
                    .unwrap_or_else(Type::any);
                Type::List(Box::new(elem))
            }
            "map" => {
                let key = args
                    .first()
                    .map(|a| self.ir_type_to_type(a, seen))
                    .unwrap_or_else(Type::str_);
                let val = args
                    .get(1)
                    .map(|a| self.ir_type_to_type(a, seen))
                    .unwrap_or_else(Type::any);
                Type::Map(Box::new(key), Box::new(val))
            }
            _ => {
                if seen.iter().any(|s| s == name) {
                    Type::Named(name.to_string())
                } else if let Some(e) = self.enum_defs.get(name) {
                    seen.push(name.to_string());
                    let variants = e["variants"]
                        .as_array()
                        .unwrap_or(&empty)
                        .iter()
                        .map(|v| {
                            let params = v["params"]
                                .as_array()
                                .unwrap_or(&empty)
                                .iter()
                                .map(|p| self.ir_type_to_type(&p["type"], seen))
                                .collect();
                            (v["name"].as_str().unwrap_or("").to_string(), params)
                        })
                        .collect();
                    seen.pop();
                    Type::Enum(EnumType {
                        name: name.to_string(),
                        variants,
                    })
                } else if let Some(r) = self.record_defs.get(name) {
                    seen.push(name.to_string());
                    let fields = r["fields"]
                        .as_array()
                        .unwrap_or(&empty)
                        .iter()
                        .map(|f| {
                            let fty = self.ir_type_to_type(&f["type"], seen);
                            let opt = f["type"]["optional"].as_bool().unwrap_or(false);
                            let has_default =
                                f.get("default").map(|d| !d.is_null()).unwrap_or(false);
                            (
                                f["name"].as_str().unwrap_or("").to_string(),
                                fty,
                                !opt && !has_default,
                            )
                        })
                        .collect();
                    seen.pop();
                    Type::Record(RecordType {
                        name: name.to_string(),
                        fields,
                    })
                } else {
                    Type::Named(name.to_string())
                }
            }
        };
        if optional && !matches!(base, Type::Optional(_)) && base != Type::null() {
            Type::Optional(Box::new(base))
        } else {
            base
        }
    }

    async fn gen_overrides(
        &self,
        with: &Json,
        env: &Env,
    ) -> Result<(Option<f64>, Option<i64>, Vec<String>), HostError> {
        let mut temp = None;
        let mut maxt = None;
        let mut context = Vec::new();
        if with["node"].as_str() == Some("configlit") {
            let empty = vec![];
            for f in with["fields"].as_array().unwrap_or(&empty) {
                let name = f["name"].as_str().unwrap_or("");
                let v = match self.eval(&f["value"], env).await? {
                    Flow::Value(v) => v,
                    _ => continue, // a control-flow signal in a with-config is ignored
                };
                match name {
                    "temperature" => temp = as_f64(&v),
                    "max_tokens" => maxt = Some(as_int(&v)),
                    "context" => match v {
                        Value::List(items) => context = items.iter().map(|i| i.to_text()).collect(),
                        other => context = vec![other.to_text()],
                    },
                    _ => {}
                }
            }
        }
        Ok((temp, maxt, context))
    }

    async fn eval_recall(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let query = val!(self, &node["query"], env).to_text();
        let one = node["one"].as_bool().unwrap_or(false);
        let facts = self
            .runtime
            .store
            .as_ref()
            .map(|s| s.recall(&query))
            .unwrap_or_default();
        if one {
            Ok(Flow::Value(
                facts
                    .into_iter()
                    .next()
                    .map(|(_, v)| Value::Str(v))
                    .unwrap_or(Value::Null),
            ))
        } else {
            let mut m = IndexMap::new();
            for (k, v) in facts {
                m.insert(k, Value::Str(v));
            }
            Ok(Flow::Value(Value::Map(m)))
        }
    }

    async fn eval_retry(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let max = as_int(&val!(self, &node["max"], env));
        let until = val!(self, &node["until"], env);
        use std::sync::atomic::Ordering;
        let mut attempts = 0i64;
        let last = loop {
            attempts += 1;
            // suppress gen-as-T internal retries inside the body (the outer
            // retry owns retry policy); save/restore for nesting.
            let prev = self.in_retry_until.swap(true, Ordering::SeqCst);
            let body_flow = self.exec_block(&node["body"], env).await;
            self.in_retry_until.store(prev, Ordering::SeqCst);
            let last = match body_flow? {
                Flow::Value(v) => v,
                sig => return Ok(sig),
            };
            // call until(last) with attempts in scope
            let stop = match &until {
                Value::Closure(c) => {
                    let cenv = c.env.child();
                    cenv.define("attempts", Value::Int(attempts));
                    if let Some(p) = c.node["params"].as_array().and_then(|p| p.first()) {
                        if let Some(pn) = p["name"].as_str() {
                            cenv.define(pn, last.clone());
                        }
                    }
                    match self.eval(&c.node["body"], &cenv).await? {
                        Flow::Value(v) => v.truthy(),
                        sig => return Ok(sig),
                    }
                }
                _ => true,
            };
            if stop || attempts >= max {
                break last;
            }
        };
        Ok(Flow::Value(last))
    }

    async fn eval_budget(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let empty = vec![];
        let mut spend = None;
        let mut steps = None;
        let mut calls = None;
        for a in node["args"].as_array().unwrap_or(&empty) {
            let label = a["label"].as_str().unwrap_or("");
            let v = val!(self, &a["value"], env);
            match label {
                "spend" => spend = as_spend(&v),
                "steps" => steps = Some(as_int(&v)),
                "tool_calls" => calls = Some(as_int(&v)),
                _ => {}
            }
        }
        let saved = self.runtime.policy.enter_budget(spend, steps, calls);
        let result = self.exec_block(&node["body"], env).await;
        self.runtime.policy.exit_budget(saved);
        result
    }

    // ---- delegate (the autonomous tool loop) ----

    async fn eval_delegate(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        use std::sync::atomic::Ordering;
        let goal = val!(self, &node["goal"], env).to_text();
        let with = &node["with_config"];
        let restrict = extract_tool_names(with);
        // budget/steps/tool_calls from the with-clause
        let (mut spend, mut steps, mut calls) = (None, None, None);
        if with["node"].as_str() == Some("configlit") {
            for f in with["fields"].as_array().unwrap_or(&vec![]) {
                let name = f["name"].as_str().unwrap_or("");
                match name {
                    "budget" => spend = as_spend(&val!(self, &f["value"], env)),
                    "max_steps" => steps = Some(as_int(&val!(self, &f["value"], env))),
                    "tool_calls" => calls = Some(as_int(&val!(self, &f["value"], env))),
                    _ => {}
                }
            }
        }

        let depth_cap = self.runtime.manifest["policy"]
            .get("max_delegate_depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(4) as u32;
        // Reserve our depth slot atomically; `prev` is the live depth before us,
        // so the cap check and the `top` decision can't race under `parallel`.
        let prev = self.delegate_depth.fetch_add(1, Ordering::SeqCst);
        if prev >= depth_cap {
            self.delegate_depth.fetch_sub(1, Ordering::SeqCst);
            return Ok(Flow::Value(Value::Str(format!(
                "delegation too deep: max_delegate_depth ({depth_cap}) exceeded; not recursing further — answer directly or use a tool"
            ))));
        }
        let top = prev == 0;
        let saved = self.runtime.policy.enter_budget(spend, steps, calls);
        let result = self.run_delegate_loop(&goal, restrict.as_ref(), top).await;
        self.delegate_depth.fetch_sub(1, Ordering::SeqCst);
        self.runtime.policy.exit_budget(saved);
        result.map(Value::Str).map(Flow::Value)
    }

    /// Build the exposed tool table, filtered by `restrict` (bare or `skill_`
    /// names). Returns `(advertised ToolDef, dispatch entries)`.
    fn build_exposed(
        &self,
        restrict: Option<&Vec<String>>,
    ) -> (Vec<crate::traits::ToolDef>, HashMap<String, Exposed>) {
        let mut defs = Vec::new();
        let mut dispatch = HashMap::new();
        let want = |selectors: &[String]| -> bool {
            match restrict {
                None => true,
                Some(r) => selectors.iter().any(|s| r.contains(s)),
            }
        };
        // authored tools
        for (name, decl) in &self.tools_authored {
            if want(std::slice::from_ref(name)) {
                defs.push(crate::traits::ToolDef {
                    name: name.clone(),
                    description: decl_description(decl, name),
                    schema: params_schema(decl),
                });
                dispatch.insert(name.clone(), Exposed::Authored(decl.clone()));
            }
        }
        // non-hidden skills → skill_<name>
        for (name, decl) in &self.skills {
            if skill_hidden(decl) {
                continue;
            }
            let advertised = format!("skill_{name}");
            if want(&[name.clone(), advertised.clone()]) {
                defs.push(crate::traits::ToolDef {
                    name: advertised.clone(),
                    description: decl_description(decl, name),
                    schema: params_schema(decl),
                });
                dispatch.insert(advertised, Exposed::Authored(decl.clone()));
            }
        }
        // backend tools (packs/mcp/native) — P11
        for (i, t) in self.runtime.tools.iter().enumerate() {
            let name = t.name().to_string();
            let pack = name.split_once('_').map(|(p, _)| p.to_string());
            let mut sels = vec![name.clone()];
            if let Some(p) = pack {
                sels.push(p);
            }
            if want(&sels) {
                defs.push(crate::traits::ToolDef {
                    name: name.clone(),
                    description: t.description().to_string(),
                    schema: t.schema().clone(),
                });
                dispatch.insert(name, Exposed::Backend(i));
            }
        }
        (defs, dispatch)
    }

    async fn run_delegate_loop(
        &self,
        goal: &str,
        restrict: Option<&Vec<String>>,
        top: bool,
    ) -> Result<String, HostError> {
        use crate::traits::{ChatRequest, Message, ToolDef};
        let (mut tooldefs, dispatch) = self.build_exposed(restrict);
        // synthetic task_complete tool
        tooldefs.push(ToolDef {
            name: "task_complete".into(),
            description: "Call when the task is finished, with the final result.".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"result": {"type": "string"}},
                "required": ["result"],
            }),
        });
        let temp = self.runtime.manifest["model"]
            .get("temperature")
            .and_then(|v| v.as_f64());
        let maxt = self.runtime.manifest["model"]
            .get("max_tokens")
            .and_then(|v| v.as_i64());

        // Top-level turns see the conversation window + semantic recall and
        // persist the exchange; nested spans get a fresh single-goal context.
        let mut messages: Vec<Message> = Vec::new();
        let mem = &self.runtime.manifest["memory"];
        if top {
            if let Some(store) = &self.runtime.store {
                if mem
                    .get("conversation_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true)
                {
                    let window = mem.get("window").and_then(|v| v.as_i64()).unwrap_or(40);
                    for m in store.window(window) {
                        let role = m
                            .get("role")
                            .and_then(|r| r.as_str())
                            .unwrap_or("user")
                            .to_string();
                        let content = m
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        messages.push(Message {
                            role,
                            content,
                            ..Default::default()
                        });
                    }
                }
            }
        }
        // Semantic recall (knowledge + archived memory), sentinel-wrapped.
        let mut user_input = goal.to_string();
        if top {
            if let Some(store) = &self.runtime.store {
                let top_k = mem.get("top_k").and_then(|v| v.as_i64()).unwrap_or(5);
                let hits = crate::embeddings::semantic_search(
                    store.as_ref(),
                    self.runtime.embedder.as_ref(),
                    goal,
                    top_k,
                )
                .await;
                if !hits.is_empty() {
                    let recall: Vec<String> = hits.into_iter().map(|(t, _, _)| t).collect();
                    user_input = format!(
                        "Relevant retrieved memory/knowledge:\n{}\n{}\n{}\n\n{goal}",
                        crate::SENTINEL_OPEN,
                        recall.join("\n---\n"),
                        crate::SENTINEL_CLOSE
                    );
                }
            }
        }
        messages.push(Message::user(&user_input));
        if top {
            if let Some(store) = &self.runtime.store {
                store.append_message(
                    "user",
                    &serde_json::json!({"role": "user", "content": goal}),
                );
            }
        }
        loop {
            // A policy violation ends the span gracefully with a partial result
            // (v2 catches PolicyViolation in run_loop), rather than aborting.
            if let Err(HostError::Policy(r)) = self.runtime.policy.check_step() {
                return Ok(format!("[stopped by policy: {r}]"));
            }
            let req = ChatRequest {
                system: self.runtime.system_prompt(false),
                messages: messages.clone(),
                tools: tooldefs.clone(),
                temperature: temp,
                max_tokens: maxt,
                schema: None,
            };
            let resp = self
                .runtime
                .provider
                .chat(req)
                .await
                .map_err(|e| HostError::Provider(e.message))?;
            let model = if resp.model.is_empty() {
                self.runtime.model_name.clone()
            } else {
                resp.model.clone()
            };
            self.runtime.policy.record_usage(
                &self.runtime.provider_name,
                &model,
                resp.input_tokens,
                resp.output_tokens,
            );
            let text = self.runtime.env.redact(&resp.text);

            if resp.tool_calls.is_empty() {
                if top {
                    if let Some(store) = &self.runtime.store {
                        store.append_message(
                            "assistant",
                            &serde_json::json!({"role": "assistant", "content": text}),
                        );
                    }
                }
                return Ok(text);
            }
            messages.push(Message {
                role: "assistant".into(),
                content: text,
                tool_calls: resp.tool_calls.clone(),
                ..Default::default()
            });
            for call in &resp.tool_calls {
                if let Err(HostError::Policy(r)) = self.runtime.policy.check_tool_call() {
                    return Ok(format!("[stopped by policy: {r}]"));
                }
                if call.name == "task_complete" {
                    let result = call
                        .args
                        .as_ref()
                        .and_then(|a| a.get("result"))
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    return Ok(result);
                }
                let output = self.dispatch_exposed(call, &dispatch).await?;
                messages.push(Message {
                    role: "tool".into(),
                    content: output,
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    ..Default::default()
                });
            }
        }
    }

    async fn dispatch_exposed(
        &self,
        call: &crate::traits::ToolCall,
        dispatch: &HashMap<String, Exposed>,
    ) -> Result<String, HostError> {
        let entry = match dispatch.get(&call.name) {
            Some(e) => e,
            None => return Ok(format!("{{\"error\": \"unknown tool '{}'\"}}", call.name)),
        };
        let args = match &call.args {
            Some(Json::Object(o)) => o.clone(),
            _ => return Ok("{\"error\": \"malformed tool arguments\"}".to_string()),
        };
        // Circuit breaker: a tool that failed 3× with the same args is disabled
        // for the rest of the session.
        let breaker_key =
            crate::policy::PolicyEngine::breaker_key(&call.name, &Json::Object(args.clone()));
        if self.runtime.policy.is_broken(&breaker_key) {
            return Ok(format!(
                "{{\"error\": \"tool '{}' is disabled for this run after repeated failures — do not call it again\"}}",
                call.name
            ));
        }
        match entry {
            Exposed::Backend(i) => {
                let tool = &self.runtime.tools[*i];
                let (result, failed) = match tool.call(Json::Object(args)).await {
                    Ok(v) => (self.runtime.env.redact(&v.to_string()), value_is_error(&v)),
                    Err(e) => (serde_json::json!({"error": e.0}).to_string(), true),
                };
                self.record_breaker(&breaker_key, failed);
                let truncated = truncate_result(&result);
                if tool.external() {
                    Ok(format!(
                        "{}\n{}\n{}",
                        crate::SENTINEL_OPEN,
                        truncated,
                        crate::SENTINEL_CLOSE
                    ))
                } else {
                    Ok(truncated)
                }
            }
            Exposed::Authored(decl) => {
                let mut labeled = IndexMap::new();
                for (k, v) in &args {
                    labeled.insert(k.clone(), Value::from_json(v));
                }
                // run the body in a fresh scope; throws become {"error": ...}
                let flow = self.call_user(decl, vec![], labeled).await?;
                let text = match flow {
                    Flow::Value(v) | Flow::Return(v) | Flow::Reply(v) => v.to_text(),
                    Flow::Throw(err) => {
                        self.record_breaker(&breaker_key, true);
                        return Ok(serde_json::json!({"error": error_message(&err)}).to_string());
                    }
                    _ => String::new(),
                };
                self.record_breaker(&breaker_key, false);
                let external = decl
                    .get("external")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let redacted = self.runtime.env.redact(&text);
                if external {
                    Ok(format!(
                        "{}\n{}\n{}",
                        crate::SENTINEL_OPEN,
                        redacted,
                        crate::SENTINEL_CLOSE
                    ))
                } else {
                    Ok(redacted)
                }
            }
        }
    }

    fn record_breaker(&self, key: &str, failed: bool) {
        if failed {
            self.runtime.policy.record_tool_failure(key);
        } else {
            self.runtime.policy.record_tool_success(key);
        }
    }

    // ---- concurrency (spawn / await / parallel) ----

    /// `parallel { a: e1, b: e2 }` — concurrent barrier; returns a record of
    /// label→value in declaration order; first error (by source order) wins.
    async fn eval_parallel(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let empty = vec![];
        let branches = node["branches"].as_array().unwrap_or(&empty);
        let futs: Vec<_> = branches
            .iter()
            .map(|b| self.eval(&b["value"], env))
            .collect();
        let results = futures::future::join_all(futs).await;
        let mut fields = IndexMap::new();
        for (b, r) in branches.iter().zip(results) {
            let name = b["name"].as_str().unwrap_or("").to_string();
            match r? {
                Flow::Value(v) => {
                    fields.insert(name, v);
                }
                sig => return Ok(sig), // first throw/signal by declaration order
            }
        }
        Ok(Flow::Value(Value::Record {
            type_name: None,
            fields,
        }))
    }

    #[cfg(feature = "native")]
    async fn eval_spawn(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        use std::sync::atomic::Ordering;
        let engine = self
            .me
            .get()
            .and_then(|w| w.upgrade())
            .ok_or_else(|| HostError::Internal("engine self-handle not initialized".into()))?;
        let target = node["target"].clone();
        let env = env.clone();
        let id = self.future_counter.fetch_add(1, Ordering::SeqCst);
        let handle = tokio::spawn(async move { engine.eval(&target, &env).await });
        self.futures.lock().unwrap().insert(id, handle);
        Ok(Flow::Value(Value::Future(crate::value::Future { id })))
    }

    #[cfg(feature = "native")]
    async fn eval_await(&self, node: &Json, env: &Env) -> Result<Flow, HostError> {
        let fv = val!(self, &node["future"], env);
        let id = match fv {
            Value::Future(f) => f.id,
            _ => {
                return Ok(Flow::Throw(make_error(
                    "await expects a future",
                    "type",
                    None,
                )))
            }
        };
        let handle = self.futures.lock().unwrap().remove(&id);
        match handle {
            None => Ok(Flow::Throw(make_error(
                "future already awaited or unknown",
                "concurrency",
                None,
            ))),
            Some(h) => match h.await {
                Ok(Ok(flow)) => Ok(flow), // Flow::Value, or Flow::Throw re-raised at the await site
                Ok(Err(host)) => Err(host),
                Err(_join) => Ok(Flow::Throw(make_error(
                    "spawned task failed",
                    "concurrency",
                    None,
                ))),
            },
        }
    }

    #[cfg(not(feature = "native"))]
    async fn eval_spawn(&self, _node: &Json, _env: &Env) -> Result<Flow, HostError> {
        Err(HostError::Internal(
            "spawn requires an executor (wasm cooperative executor: P17)".into(),
        ))
    }

    #[cfg(not(feature = "native"))]
    async fn eval_await(&self, _node: &Json, _env: &Env) -> Result<Flow, HostError> {
        Err(HostError::Internal(
            "await requires an executor (wasm cooperative executor: P17)".into(),
        ))
    }

    // ---- state ----

    fn read_state(&self, name: &str) -> Value {
        if let Some(v) = self.state_buffer.lock().unwrap().get(name) {
            return v.clone();
        }
        if let Some(store) = &self.runtime.store {
            if let Some(j) = store.get_state(name) {
                return Value::from_json(&j);
            }
        }
        self.state_defaults
            .get(name)
            .cloned()
            .unwrap_or(Value::Null)
    }
}

// ---- pure value operations ----

#[derive(Clone)]
enum Key {
    Field(String),
    Index(Value),
}

/// An exposed tool: an authored tool/skill (its IR decl) or a backend tool index.
enum Exposed {
    Authored(Json),
    Backend(usize),
}

/// Syntactically extract tool names from a `with { tools: [...] }` clause.
fn extract_tool_names(with: &Json) -> Option<Vec<String>> {
    if with["node"].as_str() != Some("configlit") {
        return None;
    }
    let field = with["fields"]
        .as_array()?
        .iter()
        .find(|f| f["name"].as_str() == Some("tools"))?;
    let list = &field["value"];
    if list["node"].as_str() != Some("list") {
        return None;
    }
    let mut names = Vec::new();
    for item in list["items"].as_array().unwrap_or(&vec![]) {
        match item["node"].as_str() {
            Some("ref") => {
                if let Some(n) = item["name"].as_str() {
                    names.push(n.to_string());
                }
            }
            Some("member") => {
                if let Some(n) = item["name"].as_str() {
                    names.push(n.to_string());
                }
            }
            _ => {}
        }
    }
    Some(names)
}

/// A tool result counts as a failure if it's an object with an `error` key.
fn value_is_error(v: &Json) -> bool {
    v.as_object()
        .map(|o| o.contains_key("error"))
        .unwrap_or(false)
}

fn truncate_result(s: &str) -> String {
    if s.len() <= crate::TOOL_RESULT_MAX_BYTES {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(crate::TOOL_RESULT_MAX_BYTES).collect();
        out.push_str("\n...[truncated by orchard]");
        out
    }
}

fn gen_retry_hint(target: &orchard_types::Type, error: &str) -> String {
    format!(
        "Your previous reply was not valid {}: {}.\nReply with ONLY valid JSON matching the schema — no prose, no code fences.",
        target.display(),
        if error.is_empty() { "could not be parsed" } else { error }
    )
}

fn decl_description(decl: &Json, fallback: &str) -> String {
    for ann in decl
        .get("annotations")
        .and_then(|a| a.as_array())
        .unwrap_or(&vec![])
    {
        if ann["name"].as_str() == Some("description") {
            if let Some(arg) = ann["args"].as_array().and_then(|a| a.first()) {
                if let Some(s) = arg["value"]["value"].as_str() {
                    return s.to_string();
                }
            }
        }
    }
    fallback.to_string()
}

fn skill_hidden(decl: &Json) -> bool {
    for ann in decl
        .get("annotations")
        .and_then(|a| a.as_array())
        .unwrap_or(&vec![])
    {
        if ann["name"].as_str() == Some("expose") {
            if let Some(arg) = ann["args"].as_array().and_then(|a| a.first()) {
                // @expose(false) hides
                if arg["value"]["type"].as_str() == Some("bool")
                    && arg["value"]["value"].as_bool() == Some(false)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Build a JSON-schema object for a callable's params (model-facing).
fn params_schema(decl: &Json) -> Json {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for p in decl["params"].as_array().unwrap_or(&vec![]) {
        let name = p["name"].as_str().unwrap_or("");
        let ty = &p["type"];
        props.insert(name.to_string(), ir_type_schema(ty));
        let optional = ty["optional"].as_bool().unwrap_or(false);
        let has_default = p.get("default").map(|d| !d.is_null()).unwrap_or(false);
        if !optional && !has_default {
            required.push(Json::String(name.to_string()));
        }
    }
    serde_json::json!({ "type": "object", "properties": props, "required": required })
}

fn ir_type_schema(ty: &Json) -> Json {
    match ty["name"].as_str() {
        Some("str") => serde_json::json!({"type": "string"}),
        Some("int") => serde_json::json!({"type": "integer"}),
        Some("float") => serde_json::json!({"type": "number"}),
        Some("bool") => serde_json::json!({"type": "boolean"}),
        Some("money") | Some("duration") | Some("bytes") => serde_json::json!({"type": "string"}),
        Some("list") => serde_json::json!({"type": "array"}),
        Some("map") => serde_json::json!({"type": "object"}),
        _ => serde_json::json!({}),
    }
}

fn val_or_sig(flow: Flow) -> Result<Result<Value, Flow>, HostError> {
    Ok(match flow {
        Flow::Value(v) => Ok(v),
        sig => Err(sig),
    })
}

fn register_enum(
    e: &Json,
    enum_defs: &mut HashMap<String, Json>,
    v2e: &mut HashMap<String, String>,
) {
    if let Some(name) = e["name"].as_str() {
        for v in e["variants"].as_array().unwrap_or(&vec![]) {
            if let Some(vn) = v["name"].as_str() {
                v2e.insert(vn.to_string(), name.to_string());
            }
        }
        enum_defs.insert(name.to_string(), e.clone());
    }
}

/// Evaluate a constant IR expression (literals/collections) — used for state &
/// param defaults and record-field defaults.
fn eval_const(node: &Json) -> Value {
    match node["node"].as_str().unwrap_or("") {
        "lit" => {
            let ty = node["type"].as_str().unwrap_or("");
            let v = &node["value"];
            match ty {
                "int" => Value::Int(v.as_i64().unwrap_or(0)),
                "float" => Value::Float(v.as_f64().unwrap_or(0.0)),
                "str" | "rawstr" => Value::Str(v.as_str().unwrap_or("").to_string()),
                "bool" => Value::Bool(v.as_bool().unwrap_or(false)),
                "null" => Value::Null,
                "duration" => Value::Duration(Duration::parse(v.as_str().unwrap_or("0s"))),
                "money" => Value::Money(Money::from_text(v.as_str().unwrap_or("0"))),
                _ => Value::Null,
            }
        }
        "list" => Value::List(
            node["items"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(eval_const)
                .collect(),
        ),
        "map" => {
            let mut m = IndexMap::new();
            for e in node["entries"].as_array().unwrap_or(&vec![]) {
                let k = eval_const(&e["key"]).to_text();
                m.insert(k, eval_const(&e["value"]));
            }
            Value::Map(m)
        }
        "configlit" => {
            let mut fields = IndexMap::new();
            for f in node["fields"].as_array().unwrap_or(&vec![]) {
                fields.insert(
                    f["name"].as_str().unwrap_or("").to_string(),
                    eval_const(&f["value"]),
                );
            }
            match node["type_name"].as_str() {
                Some(tn) => Value::Record {
                    type_name: Some(tn.to_string()),
                    fields,
                },
                None => Value::Map(fields),
            }
        }
        _ => Value::Null,
    }
}

fn iterate(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Range { start, end } => Some((*start..*end).map(Value::Int).collect()),
        Value::List(l) => Some(l.clone()),
        Value::Str(s) => Some(s.chars().map(|c| Value::Str(c.to_string())).collect()),
        Value::Map(m) => Some(m.keys().map(|k| Value::Str(k.clone())).collect()),
        _ => None,
    }
}

fn member_get(base: &Value, name: &str, optional: bool) -> Flow {
    match base {
        Value::EnumTypeRef { enum_name } => Flow::Value(Value::Enum {
            enum_name: enum_name.clone(),
            variant: name.to_string(),
            payload: vec![],
        }),
        Value::Record { fields, .. } => {
            Flow::Value(fields.get(name).cloned().unwrap_or(Value::Null))
        }
        Value::Map(m) if name == "length" => Flow::Value(Value::Int(m.len() as i64)),
        Value::Map(m) => Flow::Value(m.get(name).cloned().unwrap_or(Value::Null)),
        Value::Str(s) if name == "length" => Flow::Value(Value::Int(s.chars().count() as i64)),
        Value::List(l) if name == "length" => Flow::Value(Value::Int(l.len() as i64)),
        Value::Null => {
            if optional {
                Flow::Value(Value::Null)
            } else {
                Flow::Throw(make_error("cannot read member of null", "type", None))
            }
        }
        _ => Flow::Throw(make_error("value has no member", "type", None)),
    }
}

/// Resolve a possibly-negative index against `len`, Python-style: `-1` is the
/// last element. Returns `None` if out of range after wrapping.
fn norm_idx(i: i64, len: usize) -> Option<usize> {
    let resolved = if i < 0 { i + len as i64 } else { i };
    if resolved >= 0 && (resolved as usize) < len {
        Some(resolved as usize)
    } else {
        None
    }
}

fn index_get(obj: &Value, idx: &Value) -> Flow {
    match obj {
        Value::Record { fields, .. } => {
            Flow::Value(fields.get(&idx.to_text()).cloned().unwrap_or(Value::Null))
        }
        Value::Map(m) => Flow::Value(m.get(&idx.to_text()).cloned().unwrap_or(Value::Null)),
        Value::List(l) => match norm_idx(as_int(idx), l.len()) {
            Some(i) => Flow::Value(l[i].clone()),
            None => Flow::Throw(make_error("index out of range", "index", None)),
        },
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            match norm_idx(as_int(idx), chars.len()) {
                Some(i) => Flow::Value(Value::Str(chars[i].to_string())),
                None => Flow::Throw(make_error("index out of range", "index", None)),
            }
        }
        _ => Flow::Throw(make_error("value is not indexable", "type", None)),
    }
}

fn get_in(container: &Value, path: &[Key]) -> Value {
    let mut cur = container.clone();
    for key in path {
        cur = match (&cur, key) {
            (Value::Map(m), Key::Field(f)) => m.get(f).cloned().unwrap_or(Value::Null),
            (Value::Record { fields, .. }, Key::Field(f)) => {
                fields.get(f).cloned().unwrap_or(Value::Null)
            }
            (Value::Map(m), Key::Index(i)) => m.get(&i.to_text()).cloned().unwrap_or(Value::Null),
            (Value::List(l), Key::Index(i)) => match norm_idx(as_int(i), l.len()) {
                Some(idx) => l[idx].clone(),
                None => Value::Null,
            },
            _ => Value::Null,
        };
    }
    cur
}

fn set_in(container: &mut Value, path: &[Key], newv: Value) -> Result<(), Flow> {
    if path.is_empty() {
        *container = newv;
        return Ok(());
    }
    let (key, rest) = path.split_first().unwrap();
    match (container, key) {
        (Value::Map(m), Key::Field(f)) => {
            let entry = m.entry(f.clone()).or_insert(Value::Null);
            set_in(entry, rest, newv)
        }
        (Value::Record { fields, .. }, Key::Field(f)) => {
            let entry = fields.entry(f.clone()).or_insert(Value::Null);
            set_in(entry, rest, newv)
        }
        (Value::Map(m), Key::Index(i)) => {
            let entry = m.entry(i.to_text()).or_insert(Value::Null);
            set_in(entry, rest, newv)
        }
        (Value::List(l), Key::Index(i)) => {
            let len = l.len();
            match norm_idx(as_int(i), len) {
                Some(idx) => set_in(&mut l[idx], rest, newv),
                None => Err(Flow::Throw(make_error("index out of range", "index", None))),
            }
        }
        (c @ Value::Null, Key::Field(f)) => {
            // auto-vivify a map
            let mut m = IndexMap::new();
            m.insert(f.clone(), Value::Null);
            *c = Value::Map(m);
            if let Value::Map(m) = c {
                set_in(m.get_mut(f).unwrap(), rest, newv)
            } else {
                unreachable!()
            }
        }
        _ => Err(Flow::Throw(make_error(
            "cannot assign into this value",
            "type",
            None,
        ))),
    }
}

fn combine(op: &str, cur: Value, rhs: Value) -> Result<Value, Flow> {
    if op == "=" {
        return Ok(rhs);
    }
    let base = match op {
        "+=" => "+",
        "-=" => "-",
        "*=" => "*",
        "/=" => "/",
        "%=" => "%",
        _ => "+",
    };
    match binop(base, cur, rhs) {
        Flow::Value(v) => Ok(v),
        t => Err(t),
    }
}

fn binop(op: &str, l: Value, r: Value) -> Flow {
    match op {
        "==" => Flow::Value(Value::Bool(equal(&l, &r))),
        "!=" => Flow::Value(Value::Bool(!equal(&l, &r))),
        "<" | "<=" | ">" | ">=" => compare(op, &l, &r),
        "+" => add(l, r),
        "-" | "*" | "/" | "%" => arith(op, l, r),
        _ => Flow::Throw(make_error(
            format!("unknown binary op '{op}'"),
            "type",
            None,
        )),
    }
}

fn compare(op: &str, l: &Value, r: &Value) -> Flow {
    let ord = match (l, r) {
        (Value::Money(a), Value::Money(b)) => a.amount.partial_cmp(&b.amount),
        (Value::Duration(a), Value::Duration(b)) => a.seconds().partial_cmp(&b.seconds()),
        (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (Value::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
        (Value::Str(a), Value::Str(b)) => a.partial_cmp(b),
        _ => return Flow::Throw(make_error("cannot compare these values", "type", None)),
    };
    let ord = match ord {
        Some(o) => o,
        None => return Flow::Throw(make_error("cannot compare these values", "type", None)),
    };
    use std::cmp::Ordering::*;
    let res = match op {
        "<" => ord == Less,
        "<=" => ord != Greater,
        ">" => ord == Greater,
        ">=" => ord != Less,
        _ => false,
    };
    Flow::Value(Value::Bool(res))
}

fn add(l: Value, r: Value) -> Flow {
    match (l, r) {
        (Value::Money(a), Value::Money(b)) => {
            Flow::Value(Value::Money(Money::from_decimal(a.amount + b.amount)))
        }
        (Value::Str(a), Value::Str(b)) => Flow::Value(Value::Str(a + &b)),
        (Value::List(mut a), Value::List(b)) => {
            a.extend(b);
            Flow::Value(Value::List(a))
        }
        (Value::Bool(_), _) | (_, Value::Bool(_)) => {
            Flow::Throw(make_error("cannot apply '+' to bool", "type", None))
        }
        (Value::Int(a), Value::Int(b)) => Flow::Value(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Flow::Value(Value::Float(a + b)),
        (Value::Int(a), Value::Float(b)) => Flow::Value(Value::Float(a as f64 + b)),
        (Value::Float(a), Value::Int(b)) => Flow::Value(Value::Float(a + b as f64)),
        _ => Flow::Throw(make_error("cannot apply '+' to these values", "type", None)),
    }
}

fn arith(op: &str, l: Value, r: Value) -> Flow {
    if matches!(l, Value::Money(_)) && matches!(r, Value::Money(_)) && op == "-" {
        if let (Value::Money(a), Value::Money(b)) = (&l, &r) {
            return Flow::Value(Value::Money(Money::from_decimal(a.amount - b.amount)));
        }
    }
    // Integer modulo uses Python's floored semantics (sign follows the divisor),
    // computed on i64 so it matches v2 exactly (not truncated f64 remainder).
    if op == "%" {
        if let (Value::Int(a), Value::Int(b)) = (&l, &r) {
            if *b == 0 {
                return Flow::Throw(make_error("modulo by zero", "arithmetic", None));
            }
            return Flow::Value(Value::Int(((a % b) + b) % b));
        }
    }
    let (a, b, both_int) = match (&l, &r) {
        (Value::Bool(_), _) | (_, Value::Bool(_)) => {
            return Flow::Throw(make_error(
                format!("cannot apply '{op}' to non-numbers"),
                "type",
                None,
            ))
        }
        (Value::Int(a), Value::Int(b)) => (*a as f64, *b as f64, true),
        (Value::Float(a), Value::Float(b)) => (*a, *b, false),
        (Value::Int(a), Value::Float(b)) => (*a as f64, *b, false),
        (Value::Float(a), Value::Int(b)) => (*a, *b as f64, false),
        _ => {
            return Flow::Throw(make_error(
                format!("cannot apply '{op}' to non-numbers"),
                "type",
                None,
            ))
        }
    };
    match op {
        "-" => num_result(a - b, both_int),
        "*" => num_result(a * b, both_int),
        "/" => {
            if b == 0.0 {
                Flow::Throw(make_error("division by zero", "arithmetic", None))
            } else {
                Flow::Value(Value::Float(a / b))
            }
        }
        "%" => {
            if b == 0.0 {
                Flow::Throw(make_error("modulo by zero", "arithmetic", None))
            } else {
                num_result(a % b, both_int)
            }
        }
        _ => Flow::Throw(make_error("unknown arithmetic op", "type", None)),
    }
}

fn num_result(f: f64, both_int: bool) -> Flow {
    if both_int {
        Flow::Value(Value::Int(f as i64))
    } else {
        Flow::Value(Value::Float(f))
    }
}

async fn call_method(
    recv: &Value,
    name: &str,
    pos: &[Value],
    engine: &Engine,
    env: &Env,
) -> Result<Flow, HostError> {
    match recv {
        Value::Str(s) => Ok(str_method(s, name, pos)),
        Value::List(l) => list_method(l, name, pos, engine, env).await,
        Value::Map(m) => Ok(map_method(m, name, pos)),
        _ => Ok(Flow::Throw(make_error(
            format!("no method '{name}' on this value"),
            "type",
            None,
        ))),
    }
}

fn str_method(s: &str, name: &str, pos: &[Value]) -> Flow {
    let v = match name {
        "lower" | "lowercase" => Value::Str(s.to_lowercase()),
        "upper" | "uppercase" => Value::Str(s.to_uppercase()),
        "trim" | "strip" => Value::Str(s.trim().to_string()),
        "length" => Value::Int(s.chars().count() as i64),
        "split" => {
            let sep = pos.first().map(|v| v.to_text());
            let parts: Vec<Value> = match sep {
                Some(sep) if !sep.is_empty() => {
                    s.split(&sep).map(|p| Value::Str(p.to_string())).collect()
                }
                _ => s
                    .split_whitespace()
                    .map(|p| Value::Str(p.to_string()))
                    .collect(),
            };
            Value::List(parts)
        }
        "replace" => {
            let a = pos.first().map(|v| v.to_text()).unwrap_or_default();
            let b = pos.get(1).map(|v| v.to_text()).unwrap_or_default();
            Value::Str(s.replace(&a, &b))
        }
        "contains" => {
            Value::Bool(s.contains(&pos.first().map(|v| v.to_text()).unwrap_or_default()))
        }
        "starts_with" | "startswith" => {
            Value::Bool(s.starts_with(&pos.first().map(|v| v.to_text()).unwrap_or_default()))
        }
        "ends_with" | "endswith" => {
            Value::Bool(s.ends_with(&pos.first().map(|v| v.to_text()).unwrap_or_default()))
        }
        "basename" => Value::Str(s.rsplit('/').next().unwrap_or(s).to_string()),
        "substring" => {
            let chars: Vec<char> = s.chars().collect();
            let start = pos.first().map(as_int).unwrap_or(0).max(0) as usize;
            let end = pos
                .get(1)
                .map(as_int)
                .map(|e| e.max(0) as usize)
                .unwrap_or(chars.len());
            let slice: String = chars
                .iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect();
            Value::Str(slice)
        }
        _ => {
            return Flow::Throw(make_error(
                format!("no method '{name}' on str"),
                "type",
                None,
            ))
        }
    };
    Flow::Value(v)
}

async fn list_method(
    l: &[Value],
    name: &str,
    pos: &[Value],
    engine: &Engine,
    env: &Env,
) -> Result<Flow, HostError> {
    let v = match name {
        "length" => Value::Int(l.len() as i64),
        "contains" => Value::Bool(
            pos.first()
                .map(|x| l.iter().any(|e| equal(e, x)))
                .unwrap_or(false),
        ),
        "join" => {
            let sep = pos.first().map(|v| v.to_text()).unwrap_or_default();
            Value::Str(l.iter().map(|e| e.to_text()).collect::<Vec<_>>().join(&sep))
        }
        "first" | "head" => l.first().cloned().unwrap_or(Value::Null),
        "last" | "tail" => l.last().cloned().unwrap_or(Value::Null),
        "reverse" | "reversed" => Value::List(l.iter().rev().cloned().collect()),
        "map" => {
            let f = pos.first().cloned().unwrap_or(Value::Null);
            let mut out = Vec::new();
            for item in l {
                match engine
                    .call_value(f.clone(), vec![item.clone()], IndexMap::new(), env)
                    .await?
                {
                    Flow::Value(v) => out.push(v),
                    sig => return Ok(sig),
                }
            }
            Value::List(out)
        }
        "filter" => {
            let f = pos.first().cloned().unwrap_or(Value::Null);
            let mut out = Vec::new();
            for item in l {
                match engine
                    .call_value(f.clone(), vec![item.clone()], IndexMap::new(), env)
                    .await?
                {
                    Flow::Value(v) => {
                        if v.truthy() {
                            out.push(item.clone());
                        }
                    }
                    sig => return Ok(sig),
                }
            }
            Value::List(out)
        }
        _ => {
            return Ok(Flow::Throw(make_error(
                format!("no method '{name}' on list"),
                "type",
                None,
            )))
        }
    };
    Ok(Flow::Value(v))
}

fn map_method(m: &IndexMap<String, Value>, name: &str, pos: &[Value]) -> Flow {
    let v = match name {
        "length" => Value::Int(m.len() as i64),
        "keys" => Value::List(m.keys().map(|k| Value::Str(k.clone())).collect()),
        "values" => Value::List(m.values().cloned().collect()),
        "contains" => Value::Bool(
            pos.first()
                .map(|k| m.contains_key(&k.to_text()))
                .unwrap_or(false),
        ),
        _ => {
            return Flow::Throw(make_error(
                format!("no method '{name}' on map"),
                "type",
                None,
            ))
        }
    };
    Flow::Value(v)
}

fn to_thrown(v: Value) -> Value {
    if let Value::Record { type_name, fields } = &v {
        if type_name.as_deref() == Some("Error") || fields.contains_key("message") {
            return v;
        }
    }
    make_error(v.to_text(), "thrown", None)
}

fn error_message(err: &Value) -> String {
    if let Value::Record { fields, .. } = err {
        if let Some(Value::Str(m)) = fields.get("message") {
            return m.clone();
        }
    }
    err.to_text()
}

fn as_int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::Float(f) => *f as i64,
        Value::Bool(b) => *b as i64,
        Value::Str(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn as_spend(v: &Value) -> Option<f64> {
    match v {
        Value::Money(m) => m.amount.try_into().ok(),
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn sha1_12(s: &str) -> String {
    // SHA-256 (we don't ship sha1); first 12 hex chars. Deterministic auto-keys.
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(12).collect()
}
