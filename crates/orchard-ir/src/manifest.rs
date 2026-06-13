//! Build the static agent manifest (the 1.0 `AgentSpec` shape) from the
//! declarative config blocks. Ports v2's `build_manifest` + `_const`.

use orchard_syntax::ast::*;
use serde_json::{json, Map, Value};

const SPEC_VERSION: &str = "3.0";
const DEFAULT_ENV_FILE: &str = ".env.local";

/// Build the manifest Value for a program (the first agent, plus shared uses).
pub fn build_manifest(program: &Program) -> Value {
    let agent = program.items.iter().find_map(|i| match i {
        TopItem::Agent(a) => Some(a),
        _ => None,
    });

    // Collect config blocks (by name) and uses (top-level + agent-level).
    let mut blocks: std::collections::HashMap<&str, &ConfigBlock> =
        std::collections::HashMap::new();
    let mut uses: Vec<&UseDecl> = Vec::new();
    for i in &program.items {
        if let TopItem::Use(u) = i {
            uses.push(u);
        }
    }
    let mut name = String::new();
    let mut bodies: Vec<&Block> = Vec::new();
    if let Some(a) = agent {
        name = a.name.clone();
        for m in &a.members {
            match m {
                AgentMember::Config(cb) => {
                    blocks.entry(cb.name.as_str()).or_insert(cb);
                }
                AgentMember::Use(u) => uses.push(u),
                AgentMember::Tool(c) | AgentMember::Skill(c) | AgentMember::Fn(c) => {
                    bodies.push(&c.body)
                }
                AgentMember::On(o) => bodies.push(&o.body),
                _ => {}
            }
        }
    }

    let mut env_file = DEFAULT_ENV_FILE.to_string();
    let mut tools: Vec<Value> = Vec::new();
    for u in &uses {
        match u.form {
            UseForm::Env => {
                if let Some(t) = &u.target {
                    if let Value::String(s) = constval(t) {
                        env_file = s;
                    }
                }
            }
            UseForm::Pack => {
                let options = u
                    .options
                    .as_ref()
                    .map(|fields| {
                        let mut m = Map::new();
                        for f in fields {
                            m.insert(f.name.clone(), constval(&f.value));
                        }
                        Value::Object(m)
                    })
                    .unwrap_or_else(|| json!({}));
                tools.push(tool_spec("pack", &u.name, options, ""));
            }
            UseForm::Mcp => {
                let cmd = u.target.as_ref().map(conststr).unwrap_or_default();
                tools.push(tool_spec("mcp", &u.name, json!({}), &cmd));
            }
            UseForm::Import => {} // sub-agents have no 1.0 manifest entry
        }
    }

    let ingests_external_inline = bodies
        .iter()
        .any(|b| crate::lower::external_effects(b, true));

    json!({
        "spec_version": SPEC_VERSION,
        "name": name,
        "description": "",
        "version": "",
        "env_file": env_file,
        "source_path": "",
        "base_dir": "",
        "ingests_external_inline": ingests_external_inline,
        "personality": build_persona(blocks.get("persona").copied()),
        "model": build_model(blocks.get("model").copied()),
        "memory": build_memory(blocks.get("memory").copied()),
        "knowledge": build_knowledge(blocks.get("knowledge").copied()),
        "policy": build_policy(blocks.get("policy").copied()),
        "tools": tools,
        "skills": [],
        "triggers": [],
        "warnings": [],
    })
}

fn tool_spec(kind: &str, name: &str, options: Value, mcp_command: &str) -> Value {
    json!({
        "kind": kind,
        "name": name,
        "options": options,
        "mcp_command": mcp_command,
        "description": "",
        "http": Value::Null,
        "params": [],
        "python": "",
        "shell": "",
        "timeout": 10.0,
        "unsafe": false,
    })
}

fn setting<'a>(cb: &'a ConfigBlock, key: &str) -> Option<&'a Expr> {
    cb.settings.iter().find_map(|s| match s {
        Setting::KeyValue { key: k, value } if k == key => Some(value),
        _ => None,
    })
}

fn sub_block<'a>(cb: &'a ConfigBlock, name: &str) -> Option<&'a ConfigBlock> {
    cb.settings.iter().find_map(|s| match s {
        Setting::Block(b) if b.name == name => Some(b),
        _ => None,
    })
}

fn build_model(cb: Option<&ConfigBlock>) -> Value {
    let mut provider = Value::Null;
    let mut name = Value::Null;
    let mut temperature = Value::Null;
    let mut max_tokens = json!(4096);
    let mut api_key = Value::Null;
    let mut base_url = Value::Null;
    let mut fallback = json!([]);
    let mut pricing = Value::Null;
    if let Some(cb) = cb {
        if let Some(e) = setting(cb, "provider") {
            provider = constval(e);
        }
        if let Some(e) = setting(cb, "name") {
            name = constval(e);
        }
        if let Some(e) = setting(cb, "temperature") {
            temperature = constval(e);
        }
        if let Some(e) = setting(cb, "max_tokens") {
            max_tokens = constval(e);
        }
        if let Some(e) = setting(cb, "api_key") {
            api_key = constval(e);
        }
        if let Some(e) = setting(cb, "base_url") {
            base_url = constval(e);
        }
        if let Some(e) = setting(cb, "fallback") {
            fallback = constval(e);
        }
        if let Some(pb) = sub_block(cb, "pricing") {
            let mut m = Map::new();
            for key in ["input_per_mtok", "output_per_mtok"] {
                if let Some(e) = setting(pb, key) {
                    m.insert(key.into(), constval(e));
                }
            }
            pricing = Value::Object(m);
        }
    }
    json!({
        "provider": provider,
        "name": name,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "api_key": api_key,
        "base_url": base_url,
        "pricing": pricing,
        "fallback": fallback,
    })
}

fn build_memory(cb: Option<&ConfigBlock>) -> Value {
    let mut store = Value::Null;
    let mut conversation_enabled = true;
    let mut window = 40i64;
    let mut facts_enabled = true;
    let mut semantic_enabled = false;
    let mut top_k = 5i64;
    let mut emb_provider = "none".to_string();
    let mut emb_model = "text-embedding-3-small".to_string();
    if let Some(cb) = cb {
        if let Some(e) = setting(cb, "store") {
            store = constval(e);
        }
        if let Some(e) = setting(cb, "facts") {
            facts_enabled = constbool(e, true);
        }
        if let Some(conv) = sub_block(cb, "conversation") {
            if let Some(e) = setting(conv, "enabled") {
                conversation_enabled = constbool(e, true);
            }
            if let Some(e) = setting(conv, "window") {
                window = constint(e, 40);
            }
        }
        if let Some(sem) = sub_block(cb, "semantic") {
            if let Some(e) = setting(sem, "enabled") {
                semantic_enabled = constbool(e, false);
            }
            if let Some(e) = setting(sem, "top_k") {
                top_k = constint(e, 5);
            }
            if let Some(emb) = sub_block(sem, "embeddings") {
                if let Some(e) = setting(emb, "provider") {
                    emb_provider = conststr(e);
                }
                if let Some(e) = setting(emb, "model") {
                    emb_model = conststr(e);
                }
            }
        }
    }
    json!({
        "store": store,
        "conversation_enabled": conversation_enabled,
        "window": window,
        "facts_enabled": facts_enabled,
        "semantic_enabled": semantic_enabled,
        "embeddings": { "provider": emb_provider, "model": emb_model },
        "top_k": top_k,
    })
}

fn build_persona(cb: Option<&ConfigBlock>) -> Value {
    let mut tone = String::new();
    let mut traits = json!([]);
    let mut language = String::new();
    let mut instructions = String::new();
    let mut system_prompt = String::new();
    if let Some(cb) = cb {
        if let Some(e) = setting(cb, "tone") {
            tone = conststr(e);
        }
        if let Some(e) = setting(cb, "traits") {
            traits = constval(e);
        }
        if let Some(e) = setting(cb, "language") {
            language = conststr(e);
        }
        if let Some(e) = setting(cb, "instructions") {
            instructions = conststr(e);
        }
        if let Some(e) = setting(cb, "system_prompt") {
            system_prompt = conststr(e);
        }
    }
    json!({
        "tone": tone,
        "traits": traits,
        "language": language,
        "instructions": instructions,
        "system_prompt": system_prompt,
    })
}

fn build_knowledge(cb: Option<&ConfigBlock>) -> Value {
    let mut items = Vec::new();
    if let Some(cb) = cb {
        for s in &cb.settings {
            if let Setting::KeyValue { key, value } = s {
                let v = constval(value);
                match key.as_str() {
                    "file" => items.push(json!({ "path": v })),
                    "text" => items.push(json!({ "text": v })),
                    "url" => items.push(json!({ "url": v })),
                    _ => {}
                }
            }
        }
    }
    Value::Array(items)
}

fn build_policy(cb: Option<&ConfigBlock>) -> Value {
    let mut m = Map::new();
    m.insert("max_steps".into(), json!(25));
    m.insert("max_tool_calls".into(), json!(100));
    m.insert("max_requests_per_run".into(), json!(50));
    m.insert("max_spend_usd".into(), Value::Null);
    m.insert("allow_shell".into(), json!("never"));
    m.insert("allow_unsafe_tools".into(), json!(false));
    m.insert("allow_mcp".into(), json!(false));
    m.insert("allow_local_http".into(), json!(false));
    m.insert("allowed_domains".into(), json!([]));
    m.insert("on_violation".into(), json!("stop"));
    m.insert("i_understand_injection_risk".into(), json!(false));
    if let Some(cb) = cb {
        for (key, mkey) in [
            ("max_steps", "max_steps"),
            ("max_tool_calls", "max_tool_calls"),
            ("max_requests_per_run", "max_requests_per_run"),
            ("allow_shell", "allow_shell"),
            ("allow_unsafe_tools", "allow_unsafe_tools"),
            ("allow_mcp", "allow_mcp"),
            ("allow_local_http", "allow_local_http"),
            ("allowed_domains", "allowed_domains"),
            ("on_violation", "on_violation"),
            ("i_understand_injection_risk", "i_understand_injection_risk"),
        ] {
            if let Some(e) = setting(cb, key) {
                m.insert(mkey.into(), constval(e));
            }
        }
        if let Some(e) = setting(cb, "max_spend") {
            // money → float USD
            if let ExprKind::Literal(Lit::Money(s)) = &e.kind {
                m.insert(
                    "max_spend_usd".into(),
                    json!(s.parse::<f64>().unwrap_or(0.0)),
                );
            }
        }
    }
    Value::Object(m)
}

// ---- constant-expression evaluator ----

fn constval(e: &Expr) -> Value {
    match &e.kind {
        ExprKind::Literal(l) => match l {
            Lit::Int(i) => json!(i),
            Lit::Float(f) => json!(f),
            Lit::Str(s) | Lit::RawStr(s) => json!(s),
            Lit::Bool(b) => json!(b),
            Lit::Null => Value::Null,
            Lit::Money(s) => json!(s.parse::<f64>().unwrap_or(0.0)),
            Lit::Duration(c, u) => json!(format!("{c}{u}")),
        },
        ExprKind::Ident(n) => json!(n),
        ExprKind::Member { obj, name, .. } => {
            if let ExprKind::Ident(o) = &obj.kind {
                if o == "env" {
                    return json!(format!("${{{name}}}"));
                }
                return json!(format!("{o}.{name}"));
            }
            json!(format!("{}.{name}", conststr(obj)))
        }
        ExprKind::InterpString(parts) => {
            let mut s = String::new();
            for p in parts {
                match p {
                    InterpPart::Chunk(c) => s.push_str(c),
                    InterpPart::Expr(x) => s.push_str(&conststr(x)),
                }
            }
            json!(s)
        }
        ExprKind::ListLit(items) => Value::Array(items.iter().map(constval).collect()),
        ExprKind::MapLit(entries) => {
            let mut m = Map::new();
            for (k, v) in entries {
                m.insert(conststr(k), constval(v));
            }
            Value::Object(m)
        }
        ExprKind::ConfigLit { fields, .. } => {
            let mut m = Map::new();
            for f in fields {
                m.insert(f.name.clone(), constval(&f.value));
            }
            Value::Object(m)
        }
        ExprKind::BinOp { op, left, right } => const_binop(op, left, right),
        ExprKind::UnOp { op, operand } => {
            if op == "-" {
                match constval(operand) {
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            json!(-i)
                        } else if let Some(f) = n.as_f64() {
                            json!(-f)
                        } else {
                            Value::Null
                        }
                    }
                    _ => Value::Null,
                }
            } else {
                Value::Null
            }
        }
        _ => Value::Null,
    }
}

fn const_binop(op: &str, left: &Expr, right: &Expr) -> Value {
    let l = constval(left);
    let r = constval(right);
    match (op, &l, &r) {
        ("+", Value::String(a), Value::String(b)) => json!(format!("{a}{b}")),
        ("+", Value::Array(a), Value::Array(b)) => {
            let mut v = a.clone();
            v.extend(b.clone());
            Value::Array(v)
        }
        (_, Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap_or(0.0), b.as_f64().unwrap_or(0.0));
            let res = match op {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" if b != 0.0 => a / b,
                _ => return Value::Null,
            };
            if res.fract() == 0.0 && l.is_i64() && r.is_i64() {
                json!(res as i64)
            } else {
                json!(res)
            }
        }
        _ => Value::Null,
    }
}

fn conststr(e: &Expr) -> String {
    match constval(e) {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn constbool(e: &Expr, default: bool) -> bool {
    match constval(e) {
        Value::Bool(b) => b,
        _ => default,
    }
}

fn constint(e: &Expr, default: i64) -> i64 {
    match constval(e) {
        Value::Number(n) => n.as_i64().unwrap_or(default),
        _ => default,
    }
}
