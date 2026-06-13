//! An in-memory [`Store`] for tests and WASM. The durable `redb` store (P10)
//! implements the same trait over the same logical schema.

use crate::traits::{Hit, Store};
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    messages: Vec<(String, Json)>,
    facts: IndexMap<String, String>,
    state: IndexMap<String, Json>,
    chunks: Vec<Chunk>,
    next_chunk_id: i64,
    archived: i64,
    index_meta: Option<(String, String, usize)>,
    trace: Vec<(String, String, Json)>,
}

struct Chunk {
    id: i64,
    source: String,
    content_hash: String,
    text: String,
    vec: Option<Vec<f32>>,
}

/// A process-local, in-memory store.
#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<Inner>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        InMemoryStore::default()
    }
}

impl Store for InMemoryStore {
    fn append_message(&self, role: &str, content: &Json) {
        self.inner
            .lock()
            .unwrap()
            .messages
            .push((role.to_string(), content.clone()));
    }
    fn append_messages(&self, items: &[(String, Json)]) {
        let mut g = self.inner.lock().unwrap();
        for (role, content) in items {
            g.messages.push((role.clone(), content.clone()));
        }
    }
    fn window(&self, n: i64) -> Vec<Json> {
        if n <= 0 {
            return vec![];
        }
        let g = self.inner.lock().unwrap();
        let len = g.messages.len();
        let start = len.saturating_sub(n as usize);
        g.messages[start..].iter().map(|(_, c)| c.clone()).collect()
    }
    fn message_count(&self) -> i64 {
        self.inner.lock().unwrap().messages.len() as i64
    }
    fn messages_before_window(&self, n: i64) -> Vec<Json> {
        let g = self.inner.lock().unwrap();
        let len = g.messages.len();
        let cut = len.saturating_sub(n.max(0) as usize);
        g.messages[..cut].iter().map(|(_, c)| c.clone()).collect()
    }
    fn clear_conversation(&self) {
        let mut g = self.inner.lock().unwrap();
        g.messages.clear();
        g.archived = 0;
    }
    fn archived_count(&self) -> i64 {
        self.inner.lock().unwrap().archived
    }
    fn set_archived_count(&self, count: i64) {
        self.inner.lock().unwrap().archived = count;
    }

    fn remember(&self, key: &str, value: &str) {
        self.inner
            .lock()
            .unwrap()
            .facts
            .insert(key.to_string(), value.to_string());
    }
    fn recall(&self, query: &str) -> Vec<(String, String)> {
        let g = self.inner.lock().unwrap();
        let q = query.to_lowercase();
        let mut out: Vec<(String, String)> = g
            .facts
            .iter()
            .filter(|(k, v)| {
                query.is_empty() || k.to_lowercase().contains(&q) || v.to_lowercase().contains(&q)
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn forget(&self, key: &str) -> bool {
        self.inner.lock().unwrap().facts.shift_remove(key).is_some()
    }
    fn all_facts(&self) -> Vec<(String, String)> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<(String, String)> = g
            .facts
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn clear_facts(&self) {
        self.inner.lock().unwrap().facts.clear();
    }

    fn get_state(&self, key: &str) -> Option<Json> {
        self.inner.lock().unwrap().state.get(key).cloned()
    }
    fn get_all_state(&self) -> Vec<(String, Json)> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<(String, Json)> = g
            .state
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn set_state_batch(&self, items: &[(String, Json)]) {
        let mut g = self.inner.lock().unwrap();
        for (k, v) in items {
            g.state.insert(k.clone(), v.clone());
        }
    }
    fn clear_state(&self) {
        self.inner.lock().unwrap().state.clear();
    }

    fn add_chunks(
        &self,
        source: &str,
        content_hash: &str,
        texts: &[String],
        vectors: Option<&[Vec<f32>]>,
    ) {
        let mut g = self.inner.lock().unwrap();
        g.chunks.retain(|c| c.source != source);
        for (i, text) in texts.iter().enumerate() {
            let id = g.next_chunk_id;
            g.next_chunk_id += 1;
            let vec = vectors.and_then(|vs| vs.get(i).cloned());
            g.chunks.push(Chunk {
                id,
                source: source.to_string(),
                content_hash: content_hash.to_string(),
                text: text.clone(),
                vec,
            });
        }
    }
    fn search_vec(&self, vector: &[f32], top_k: i64) -> Vec<Hit> {
        if top_k <= 0 || vector.is_empty() {
            return vec![];
        }
        let g = self.inner.lock().unwrap();
        let mut scored: Vec<Hit> = g
            .chunks
            .iter()
            .filter_map(|c| {
                c.vec.as_ref().filter(|v| v.len() == vector.len()).map(|v| {
                    let dot: f32 = v.iter().zip(vector).map(|(a, b)| a * b).sum();
                    (c.text.clone(), dot as f64, c.source.clone())
                })
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k as usize);
        scored
    }
    fn all_chunks(&self) -> Vec<(i64, String, String)> {
        let g = self.inner.lock().unwrap();
        g.chunks
            .iter()
            .map(|c| (c.id, c.text.clone(), c.source.clone()))
            .collect()
    }
    fn has_source(&self, source: &str, content_hash: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .chunks
            .iter()
            .any(|c| c.source == source && c.content_hash == content_hash)
    }
    fn delete_source(&self, source: &str) {
        self.inner
            .lock()
            .unwrap()
            .chunks
            .retain(|c| c.source != source);
    }
    fn chunk_count(&self) -> i64 {
        self.inner.lock().unwrap().chunks.len() as i64
    }
    fn index_meta(&self) -> Option<(String, String, usize)> {
        self.inner.lock().unwrap().index_meta.clone()
    }
    fn set_index_meta(&self, provider: &str, model: &str, dim: usize) {
        self.inner.lock().unwrap().index_meta =
            Some((provider.to_string(), model.to_string(), dim));
    }

    fn trace_event(&self, run_id: &str, kind: &str, payload: &Json) {
        self.inner.lock().unwrap().trace.push((
            run_id.to_string(),
            kind.to_string(),
            payload.clone(),
        ));
    }
    fn last_run_id(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .trace
            .last()
            .map(|(r, _, _)| r.clone())
    }
    fn run_trace(&self, run_id: &str) -> Vec<Json> {
        let g = self.inner.lock().unwrap();
        g.trace
            .iter()
            .filter(|(r, _, _)| r == run_id)
            .map(|(_, k, p)| serde_json::json!({ "kind": k, "payload": p }))
            .collect()
    }

    fn clear_all(&self) {
        *self.inner.lock().unwrap() = Inner::default();
    }
}
