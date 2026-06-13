//! A durable, pure-Rust [`Store`] backed by `redb` (native). Implements the
//! same logical schema as the in-memory store; redb provides ACID transactions
//! and single-writer file locking, so this is the on-disk default for
//! `orch run`/`serve`. Errors are best-effort (the `Store` trait is infallible,
//! matching v2's "it just works" store).

use crate::traits::{Hit, Store};
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde_json::{json, Value as Json};
use std::path::Path;
use std::sync::Mutex;

const MESSAGES: TableDefinition<u64, &str> = TableDefinition::new("messages");
const FACTS: TableDefinition<&str, &str> = TableDefinition::new("facts");
const STATE: TableDefinition<&str, &str> = TableDefinition::new("state");
const CHUNKS: TableDefinition<u64, &str> = TableDefinition::new("chunks");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");
const TRACE: TableDefinition<u64, &str> = TableDefinition::new("trace");

pub struct RedbStore {
    db: Database,
    write_lock: Mutex<()>,
}

impl RedbStore {
    /// Open (or create) a store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<RedbStore, String> {
        let db = Database::create(path).map_err(|e| e.to_string())?;
        Ok(RedbStore {
            db,
            write_lock: Mutex::new(()),
        })
    }

    fn write<F: FnOnce(&redb::WriteTransaction)>(&self, f: F) {
        let _g = self.write_lock.lock().unwrap();
        if let Ok(tx) = self.db.begin_write() {
            f(&tx);
            let _ = tx.commit();
        }
    }
}

/// Append `val` under an id allocated *inside* this write transaction, so id
/// derivation and insertion are atomic under the write lock (no read-then-write
/// id race between concurrent writers).
fn append_auto(tx: &redb::WriteTransaction, def: TableDefinition<u64, &str>, val: &str) {
    if let Ok(mut t) = tx.open_table(def) {
        let id = t
            .last()
            .ok()
            .flatten()
            .map(|(k, _)| k.value() + 1)
            .unwrap_or(1);
        let _ = t.insert(id, val);
    }
}

impl Store for RedbStore {
    fn append_message(&self, role: &str, content: &Json) {
        let payload = json!({ "role": role, "content": content }).to_string();
        self.write(|tx| append_auto(tx, MESSAGES, &payload));
    }
    fn append_messages(&self, items: &[(String, Json)]) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(MESSAGES) {
                let base = t
                    .last()
                    .ok()
                    .flatten()
                    .map(|(k, _)| k.value() + 1)
                    .unwrap_or(1);
                for (i, (role, content)) in items.iter().enumerate() {
                    let payload = json!({ "role": role, "content": content }).to_string();
                    let _ = t.insert(base + i as u64, payload.as_str());
                }
            }
        });
    }
    fn window(&self, n: i64) -> Vec<Json> {
        if n <= 0 {
            return vec![];
        }
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let table = match tx.open_table(MESSAGES) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let mut all: Vec<Json> = table
            .iter()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|(_, v)| serde_json::from_str::<Json>(v.value()).ok())
            .collect();
        let start = all.len().saturating_sub(n as usize);
        all.drain(..start);
        all.into_iter()
            .map(|m| m.get("content").cloned().unwrap_or(Json::Null))
            .collect()
    }
    fn message_count(&self) -> i64 {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        tx.open_table(MESSAGES)
            .map(|t| t.len().unwrap_or(0) as i64)
            .unwrap_or(0)
    }
    fn messages_before_window(&self, n: i64) -> Vec<Json> {
        let mut all = self.all_messages();
        let cut = all.len().saturating_sub(n.max(0) as usize);
        all.truncate(cut);
        all.into_iter()
            .map(|m| m.get("content").cloned().unwrap_or(Json::Null))
            .collect()
    }
    fn clear_conversation(&self) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(MESSAGES) {
                let _ = t.retain(|_, _| false);
            }
        });
    }
    fn archived_count(&self) -> i64 {
        self.meta_get("conversation_archived_count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
    fn set_archived_count(&self, count: i64) {
        self.meta_set("conversation_archived_count", &count.to_string());
    }

    fn remember(&self, key: &str, value: &str) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(FACTS) {
                let _ = t.insert(key, value);
            }
        });
    }
    fn recall(&self, query: &str) -> Vec<(String, String)> {
        let q = query.to_lowercase();
        let mut out: Vec<(String, String)> = self
            .all_facts()
            .into_iter()
            .filter(|(k, v)| {
                query.is_empty() || k.to_lowercase().contains(&q) || v.to_lowercase().contains(&q)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn forget(&self, key: &str) -> bool {
        let mut existed = false;
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(FACTS) {
                existed = t.remove(key).ok().flatten().is_some();
            }
        });
        existed
    }
    fn all_facts(&self) -> Vec<(String, String)> {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let table = match tx.open_table(FACTS) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let mut out: Vec<(String, String)> = table
            .iter()
            .into_iter()
            .flatten()
            .flatten()
            .map(|(k, v)| (k.value().to_string(), v.value().to_string()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn clear_facts(&self) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(FACTS) {
                let _ = t.retain(|_, _| false);
            }
        });
    }

    fn get_state(&self, key: &str) -> Option<Json> {
        let tx = self.db.begin_read().ok()?;
        let table = tx.open_table(STATE).ok()?;
        let v = table.get(key).ok().flatten()?;
        serde_json::from_str(v.value()).ok()
    }
    fn get_all_state(&self) -> Vec<(String, Json)> {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let table = match tx.open_table(STATE) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let mut out: Vec<(String, Json)> = table
            .iter()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|(k, v)| {
                serde_json::from_str(v.value())
                    .ok()
                    .map(|j| (k.value().to_string(), j))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn set_state_batch(&self, items: &[(String, Json)]) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(STATE) {
                for (k, v) in items {
                    let _ = t.insert(k.as_str(), v.to_string().as_str());
                }
            }
        });
    }
    fn clear_state(&self) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(STATE) {
                let _ = t.retain(|_, _| false);
            }
        });
    }

    fn add_chunks(
        &self,
        source: &str,
        content_hash: &str,
        texts: &[String],
        vectors: Option<&[Vec<f32>]>,
    ) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(CHUNKS) {
                // Scan + delete this source's stale rows and allocate ids all
                // within one write txn, so a concurrent add_chunks can't race.
                let stale: Vec<u64> = t
                    .iter()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|(k, v)| {
                        let j: Json = serde_json::from_str(v.value()).ok()?;
                        (j.get("source").and_then(|s| s.as_str()) == Some(source)).then(|| k.value())
                    })
                    .collect();
                for s in &stale {
                    let _ = t.remove(*s);
                }
                let base = t.last().ok().flatten().map(|(k, _)| k.value() + 1).unwrap_or(1);
                for (i, text) in texts.iter().enumerate() {
                    let vec = vectors.and_then(|vs| vs.get(i)).cloned();
                    let row = json!({ "source": source, "content_hash": content_hash, "text": text, "vec": vec });
                    let _ = t.insert(base + i as u64, row.to_string().as_str());
                }
            }
        });
    }
    fn search_vec(&self, vector: &[f32], top_k: i64) -> Vec<Hit> {
        if top_k <= 0 || vector.is_empty() {
            return vec![];
        }
        let mut scored: Vec<Hit> = self
            .all_chunk_rows()
            .into_iter()
            .filter_map(|(_, j)| {
                let v: Vec<f32> = j
                    .get("vec")?
                    .as_array()?
                    .iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect();
                if v.len() != vector.len() {
                    return None;
                }
                let dot: f32 = v.iter().zip(vector).map(|(a, b)| a * b).sum();
                Some((
                    j.get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    dot as f64,
                    j.get("source")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                ))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k as usize);
        scored
    }
    fn all_chunks(&self) -> Vec<(i64, String, String)> {
        self.all_chunk_rows()
            .into_iter()
            .map(|(id, j)| {
                (
                    id as i64,
                    j.get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    j.get("source")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect()
    }
    fn has_source(&self, source: &str, content_hash: &str) -> bool {
        self.all_chunk_rows().iter().any(|(_, j)| {
            j.get("source").and_then(|s| s.as_str()) == Some(source)
                && j.get("content_hash").and_then(|s| s.as_str()) == Some(content_hash)
        })
    }
    fn delete_source(&self, source: &str) {
        let stale: Vec<u64> = self
            .all_chunk_rows()
            .into_iter()
            .filter(|(_, j)| j.get("source").and_then(|s| s.as_str()) == Some(source))
            .map(|(id, _)| id)
            .collect();
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(CHUNKS) {
                for s in &stale {
                    let _ = t.remove(*s);
                }
            }
        });
    }
    fn chunk_count(&self) -> i64 {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        tx.open_table(CHUNKS)
            .map(|t| t.len().unwrap_or(0) as i64)
            .unwrap_or(0)
    }
    fn index_meta(&self) -> Option<(String, String, usize)> {
        let p = self.meta_get("index_provider")?;
        let m = self.meta_get("index_model")?;
        let d = self.meta_get("index_dim")?.parse().ok()?;
        Some((p, m, d))
    }
    fn set_index_meta(&self, provider: &str, model: &str, dim: usize) {
        self.meta_set("index_provider", provider);
        self.meta_set("index_model", model);
        self.meta_set("index_dim", &dim.to_string());
    }

    fn trace_event(&self, run_id: &str, kind: &str, payload: &Json) {
        let row = json!({ "run_id": run_id, "kind": kind, "payload": payload }).to_string();
        self.write(|tx| append_auto(tx, TRACE, &row));
    }
    fn last_run_id(&self) -> Option<String> {
        let tx = self.db.begin_read().ok()?;
        let table = tx.open_table(TRACE).ok()?;
        let (_, v) = table.last().ok().flatten()?;
        serde_json::from_str::<Json>(v.value())
            .ok()?
            .get("run_id")?
            .as_str()
            .map(|s| s.to_string())
    }
    fn run_trace(&self, run_id: &str) -> Vec<Json> {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let table = match tx.open_table(TRACE) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        table
            .iter()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|(_, v)| serde_json::from_str::<Json>(v.value()).ok())
            .filter(|j| j.get("run_id").and_then(|r| r.as_str()) == Some(run_id))
            .map(|j| json!({ "kind": j.get("kind").cloned().unwrap_or(Json::Null), "payload": j.get("payload").cloned().unwrap_or(Json::Null) }))
            .collect()
    }

    fn clear_all(&self) {
        self.clear_conversation();
        self.clear_facts();
        self.clear_state();
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(CHUNKS) {
                let _ = t.retain(|_, _| false);
            }
            if let Ok(mut t) = tx.open_table(TRACE) {
                let _ = t.retain(|_, _| false);
            }
            if let Ok(mut t) = tx.open_table(META) {
                let _ = t.retain(|_, _| false);
            }
        });
    }
}

impl RedbStore {
    fn meta_get(&self, key: &str) -> Option<String> {
        let tx = self.db.begin_read().ok()?;
        let table = tx.open_table(META).ok()?;
        table.get(key).ok().flatten().map(|v| v.value().to_string())
    }
    fn meta_set(&self, key: &str, value: &str) {
        self.write(|tx| {
            if let Ok(mut t) = tx.open_table(META) {
                let _ = t.insert(key, value);
            }
        });
    }
    fn all_messages(&self) -> Vec<Json> {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let table = match tx.open_table(MESSAGES) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        table
            .iter()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|(_, v)| serde_json::from_str(v.value()).ok())
            .collect()
    }
    fn all_chunk_rows(&self) -> Vec<(u64, Json)> {
        let tx = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let table = match tx.open_table(CHUNKS) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        table
            .iter()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|(k, v)| serde_json::from_str(v.value()).ok().map(|j| (k.value(), j)))
            .collect()
    }
}
