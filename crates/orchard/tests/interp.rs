//! P6 interpreter e2e tests — offline via the mock provider. Mirrors v2's
//! `test_interp.py` (state persistence, rollback, control flow, try/catch,
//! IR-equivalence).

use orchard::{Agent, Runtime, Value};
use orchard_runtime::InMemoryStore;
use std::sync::Arc;

fn session(src: &str) -> orchard::Session {
    let agent = Agent::load(src, "<t>").expect("load");
    Runtime::builder(agent).build().expect("build")
}

#[tokio::test]
async fn echo_gen_reflects_prompt() {
    let s = session("agent E { model { provider: mock, name: \"echo\" } on message(text: str) -> str { return gen \"Reply to: {text}\" } }");
    let out = s.message("hi").await.unwrap();
    assert!(out.contains("[mock:echo]"), "got: {out}");
    assert!(out.contains("Reply to: hi"), "got: {out}");
}

#[tokio::test]
async fn handler_auto_returns_trailing_expr() {
    // A bare trailing `gen` is the response (no explicit return).
    let s = session("agent E { model { provider: mock, name: \"echo\" } on message(text: str) -> str { gen \"X {text}\" } }");
    let out = s.message("yo").await.unwrap();
    assert!(out.contains("X yo"), "got: {out}");
}

#[tokio::test]
async fn state_counter_persists_across_turns() {
    let s = session("agent C { model { provider: mock, name: \"echo\" } state n: int = 0 on message(text: str) -> str { n += 1\n return \"{n}\" } }");
    assert_eq!(s.message("a").await.unwrap(), "1");
    assert_eq!(s.message("b").await.unwrap(), "2");
    assert_eq!(s.message("c").await.unwrap(), "3");
}

#[tokio::test]
async fn state_persists_across_sessions_on_shared_store() {
    let store = Arc::new(InMemoryStore::new());
    let src = "agent C { model { provider: mock, name: \"echo\" } state n: int = 0 on message(text: str) -> str { n += 1\n return \"{n}\" } }";
    {
        let agent = Agent::load(src, "<t>").unwrap();
        let s = Runtime::builder(agent)
            .store(store.clone())
            .build()
            .unwrap();
        assert_eq!(s.message("a").await.unwrap(), "1");
    }
    {
        let agent = Agent::load(src, "<t>").unwrap();
        let s = Runtime::builder(agent)
            .store(store.clone())
            .build()
            .unwrap();
        assert_eq!(s.message("b").await.unwrap(), "2");
    }
}

#[tokio::test]
async fn crash_mid_skill_rolls_back_state() {
    let src = "agent R { model { provider: mock, name: \"echo\" } state n: int = 0 skill bump() { n = 99\n throw \"boom\" } skill peek() -> int { return n } }";
    let s = session(src);
    let r = s.skill("bump", serde_json::json!({})).await;
    assert!(r.is_err(), "bump should error");
    let peek = s.skill("peek", serde_json::json!({})).await.unwrap();
    assert!(
        matches!(peek, Value::Int(0)),
        "state should roll back to 0, got {peek:?}"
    );
}

#[tokio::test]
async fn control_flow_for_and_match() {
    let src = "agent G { model { provider: mock, name: \"echo\" } enum Sz { small, big } skill compute() -> str { var total = 0\n for i in 1..=5 { total += i }\n let sz = match total { 15 => Sz.big  _ => Sz.small }\n return \"G:{total}:{sz}\" } }";
    let s = session(src);
    let out = s.skill("compute", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "G:15:big");
}

#[tokio::test]
async fn try_catch_recovers() {
    let src = "agent T { model { provider: mock, name: \"echo\" } skill s() -> str { try { throw \"boom\" } catch e { return \"caught: {e.message}\" } } }";
    let s = session(src);
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "caught: boom");
}

#[tokio::test]
async fn pure_fn_and_methods() {
    let src = "agent F { model { provider: mock, name: \"echo\" } fn double(x: int) -> int { return x * 2 } skill s() -> str { let parts = \"a,b,c\".split(\",\")\n return \"{double(x: 21)}:{parts.length}\" } }";
    let s = session(src);
    let out = s.skill("s", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "42:3");
}

#[tokio::test]
async fn fact_memory_roundtrip() {
    let src = "agent M { model { provider: mock, name: \"echo\" } memory { facts: true } skill save() { remember city = \"Wellington\" } skill load() -> str { return recall_one(\"city\") ?? \"?\" } }";
    let s = session(src);
    s.skill("save", serde_json::json!({})).await.unwrap();
    let out = s.skill("load", serde_json::json!({})).await.unwrap();
    assert_eq!(out.to_text(), "Wellington");
}

#[tokio::test]
async fn ir_equivalence_source_eq_compiled() {
    let src = "agent E { model { provider: mock, name: \"echo\" } skill s() -> str { var t = 0\n for i in 0..4 { t += i }\n return \"{t}\" } }";
    // run from source
    let from_src = session(src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    // run from compiled IR
    let ir = Agent::compile(src, "<t>").unwrap();
    let agent = Agent::from_ir(&ir).unwrap();
    let from_ir = Runtime::builder(agent)
        .build()
        .unwrap()
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(from_src.to_text(), from_ir.to_text());
    assert_eq!(from_src.to_text(), "6");
}
