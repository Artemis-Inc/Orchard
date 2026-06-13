//! redb durable store: persistence across reopen + the logical schema.

use orchard_runtime::{RedbStore, Store};
use serde_json::json;

fn tmp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "orchard_redb_{}_{:p}.orchmem",
        std::process::id(),
        &0u8
    ))
}

#[test]
fn persists_across_reopen() {
    let path = tmp();
    let _ = std::fs::remove_file(&path);
    {
        let store = RedbStore::open(&path).unwrap();
        store.append_message("user", &json!({"role": "user", "content": "hi"}));
        store.remember("city", "Wellington");
        store.set_state_batch(&[("turn".to_string(), json!(3))]);
        store.trace_event("run1", "start", &json!({"goal": "x"}));
        store.add_chunks(
            "doc",
            "h1",
            &["alpha beta".to_string()],
            Some(&[vec![1.0, 0.0]]),
        );
    }
    {
        let store = RedbStore::open(&path).unwrap();
        assert_eq!(store.message_count(), 1);
        assert_eq!(
            store.recall("city"),
            vec![("city".to_string(), "Wellington".to_string())]
        );
        assert_eq!(store.get_state("turn"), Some(json!(3)));
        assert_eq!(store.last_run_id().as_deref(), Some("run1"));
        assert!(store.has_source("doc", "h1"));
        let hits = store.search_vec(&[1.0, 0.0], 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "alpha beta");
        // state vs facts are separate namespaces
        store.remember("turn", "fact-value");
        assert_eq!(store.get_state("turn"), Some(json!(3))); // unchanged
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn window_and_clear() {
    let path =
        std::env::temp_dir().join(format!("orchard_redb_win_{}.orchmem", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = RedbStore::open(&path).unwrap();
    for i in 0..5 {
        store.append_message("user", &json!(format!("m{i}")));
    }
    let w = store.window(2);
    assert_eq!(w.len(), 2);
    assert_eq!(w[1], json!("m4"));
    store.clear_all();
    assert_eq!(store.message_count(), 0);
    drop(store);
    let _ = std::fs::remove_file(&path);
}
