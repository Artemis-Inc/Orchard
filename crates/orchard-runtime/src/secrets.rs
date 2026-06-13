//! Secret tracking + redaction (v2's `envloader.Environment`). Tracked values
//! (length ≥ 6) are replaced everywhere with `«VAR»` (guillemets) before they
//! reach a model, the trace, memory, or an embedding provider.

use std::collections::HashMap;
use std::sync::RwLock;

/// Provider → standard API-key env var.
pub fn provider_key_var(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        "together" => Some("TOGETHER_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        _ => None,
    }
}

/// The environment: a name→value table (process env + agent env file) plus a
/// registry of secret values to redact. Thread-safe (concurrency).
pub struct Environment {
    values: HashMap<String, String>,
    /// value → var name (for `«VAR»` redaction).
    secrets: RwLock<HashMap<String, String>>,
}

impl Default for Environment {
    fn default() -> Self {
        Environment::new()
    }
}

impl Environment {
    pub fn new() -> Self {
        let values = std::env::vars().collect();
        Environment {
            values,
            secrets: RwLock::new(HashMap::new()),
        }
    }

    /// Construct with an explicit value table (tests / WASM).
    pub fn with_values(values: HashMap<String, String>) -> Self {
        Environment {
            values,
            secrets: RwLock::new(HashMap::new()),
        }
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_string(), value.to_string());
    }

    pub fn lookup(&self, var: &str) -> Option<String> {
        self.values.get(var).cloned()
    }

    /// Register a value (≥ 6 chars) for redaction under `name`.
    pub fn track_secret(&self, value: &str, name: &str) {
        if value.len() >= 6 {
            self.secrets
                .write()
                .unwrap()
                .insert(value.to_string(), name.to_string());
        }
    }

    /// Replace every tracked secret value in `text` with `«VAR»`.
    pub fn redact(&self, text: &str) -> String {
        let secrets = self.secrets.read().unwrap();
        let mut out = text.to_string();
        for (value, var) in secrets.iter() {
            if out.contains(value) {
                out = out.replace(value, &format!("\u{ab}{var}\u{bb}"));
            }
        }
        out
    }

    /// Recursively redact a JSON value.
    pub fn redact_json(&self, v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::String(s) => Value::String(self.redact(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.redact_json(x)).collect()),
            Value::Object(o) => {
                let mut m = serde_json::Map::new();
                for (k, val) in o {
                    m.insert(k.clone(), self.redact_json(val));
                }
                Value::Object(m)
            }
            other => other.clone(),
        }
    }
}
