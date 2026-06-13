//! P18 trigger tests — `on schedule` / `on file` dispatch (bounded; no serve
//! loop). Mirrors v2's `test_triggers.py`.

use orchard::{Agent, Runtime, Value};
use orchard_runtime::InMemoryStore;
use std::sync::Arc;

#[tokio::test]
async fn schedule_handler_fires_and_persists() {
    let store = Arc::new(InMemoryStore::new());
    let src = "agent S { model { provider: mock, name: \"echo\" } memory { facts: true } state fires: int = 0 on schedule(every: 1s) { fires += 1\n remember \"fires\" = \"{fires}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent)
        .store(store.clone())
        .build()
        .unwrap();
    for _ in 0..3 {
        s.schedule().await.unwrap();
    }
    use orchard_runtime::Store;
    assert_eq!(store.get_state("fires"), Some(serde_json::json!(3)));
}

#[tokio::test]
async fn file_handler_binds_path() {
    let src = "agent F { model { provider: mock, name: \"echo\" } on file(path: str) in \"./inbox\" { return \"saw {path}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    let out = s.file("inbox/note.txt").await.unwrap();
    assert!(
        matches!(out, Value::Str(ref t) if t == "saw inbox/note.txt"),
        "got {out:?}"
    );
}

#[tokio::test]
async fn schedule_spec_is_exposed() {
    let src = "agent S { model { provider: mock, name: \"echo\" } on schedule(every: 30m) { emit \"tick\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    assert_eq!(
        s.schedule_spec(),
        Some(("every".to_string(), "30m".to_string()))
    );
}
