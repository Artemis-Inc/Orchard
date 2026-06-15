//! P11 tools tests — packs, native host tools, backend dispatch. Includes the
//! demo-offline parity run.

use orchard::{Agent, NativeTool, Runtime};
use std::sync::Arc;

fn scripts_dir() -> String {
    format!("{}/tests/scripts", env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
async fn demo_offline_runs_end_to_end() {
    // The full agent loop offline: calculate (real), remember (real), exposed
    // skill_note (real), final reply — driven by demo-script.yaml.
    let src = std::fs::read_to_string(format!("{}/demo-offline.orch", scripts_dir())).unwrap();
    let agent = Agent::load(&src, "demo-offline.orch").expect("load");
    let store = Arc::new(orchard_runtime::InMemoryStore::new());
    let session = Runtime::builder(agent)
        .base_dir(scripts_dir())
        .store(store.clone())
        .build()
        .expect("build");
    let out = session.task("demo").await.unwrap();
    assert_eq!(
        out,
        "Done. 6 × 7 = 42, saved to memory as 'last_answer'. Ran fully offline."
    );
    // the calculator + remember really ran: the fact is in the store.
    use orchard_runtime::Store;
    assert_eq!(
        store.recall("last_answer"),
        vec![("last_answer".to_string(), "42".to_string())]
    );
}

#[tokio::test]
async fn calculator_direct_call_from_code() {
    let src = "agent A { model { provider: mock, name: \"echo\" } use calculator\n skill s() -> str { let r = calculate(expression: \"2 ** 10 + 1\")\n return \"{r.result}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "1025");
}

#[tokio::test]
async fn native_host_tool_via_delegate() {
    // Register a native Rust tool and let the delegate loop call it.
    let lookup = NativeTool::builder("lookup_user")
        .description("Look up a user by id.")
        .param("id", "integer", true)
        .handler(|args| async move {
            let id = args.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(serde_json::json!({ "id": id, "name": "Ada" }))
        });
    let src = "agent A { model { provider: mock, name: \"native.yaml\" } on message(text: str) -> str { return delegate text } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent)
        .base_dir(scripts_dir())
        .register_tool(lookup)
        .build()
        .unwrap();
    let out = s.message("who is user 7").await.unwrap();
    assert_eq!(out, "user is Ada");
}

#[tokio::test]
async fn files_pack_containment() {
    let tmp = std::env::temp_dir().join(format!("orchard_files_test_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let src = "agent A { model { provider: mock, name: \"echo\" } use files { root: \".\" }\n skill w() -> str { let r = write_file(path: \"out.txt\", content: \"hello\")\n return \"{r.bytes}\" } skill r() -> str { let f = read_file(path: \"out.txt\")\n return f.content } skill bad() -> str { try { let _ = read_file(path: \"../escape.txt\")\n return \"no\" } catch e { return \"blocked\" } } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).base_dir(&tmp).build().unwrap();
    assert_eq!(
        s.skill("w", serde_json::json!({})).await.unwrap().to_text(),
        "5"
    );
    assert_eq!(
        s.skill("r", serde_json::json!({})).await.unwrap().to_text(),
        "hello"
    );
    assert_eq!(
        s.skill("bad", serde_json::json!({}))
            .await
            .unwrap()
            .to_text(),
        "blocked"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
