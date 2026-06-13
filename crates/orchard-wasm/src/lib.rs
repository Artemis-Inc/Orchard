//! WebAssembly bindings for Orchard 3.0 (wasm-bindgen). A thin adapter over the
//! `orchard` facade for browsers, edge, and serverless. The runtime is built
//! with the `wasm` feature (cooperative executor, in-memory store, no tokio).
//!
//! Build with wasm-pack: `wasm-pack build crates/orchard-wasm --target web`.

use orchard::{Agent as CoreAgent, Runtime};
use wasm_bindgen::prelude::*;

/// The Orchard version.
#[wasm_bindgen]
pub fn version() -> String {
    orchard::VERSION.to_string()
}

/// Static analysis → rendered diagnostics (empty string if clean).
#[wasm_bindgen]
pub fn check(source: &str, filename: &str) -> String {
    CoreAgent::check(source, filename)
        .iter()
        .map(|d| d.render(source))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A handle to an agent source. Each `message` call runs a turn on a fresh
/// session (the single-threaded wasm executor needs an owned, `'static`
/// future); conversation/state persistence across turns in the browser is a
/// host concern (inject a persistent `Store` via the Rust API).
#[wasm_bindgen]
pub struct Agent {
    source: String,
    filename: String,
}

#[wasm_bindgen]
impl Agent {
    /// Load + check an agent. Throws on diagnostics.
    #[wasm_bindgen(constructor)]
    pub fn new(source: String, filename: String) -> Result<Agent, JsValue> {
        CoreAgent::load(&source, &filename).map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Agent { source, filename })
    }

    /// Drive one `on message` turn. Returns a `Promise<string>`.
    pub fn message(&self, text: String) -> js_sys::Promise {
        let source = self.source.clone();
        let filename = self.filename.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let agent = CoreAgent::load(&source, &filename)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let session = Runtime::builder(agent)
                .build()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            match session.message(&text).await {
                Ok(reply) => Ok(JsValue::from_str(&reply)),
                Err(e) => Err(JsValue::from_str(&e.to_string())),
            }
        })
    }
}
