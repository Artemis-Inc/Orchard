//! P9 concurrency tests — spawn/await/parallel. Exercises thread-safety via the
//! multi-threaded tokio runtime.

use orchard::{Agent, Runtime, Value};

fn session(src: &str) -> orchard::Session {
    Runtime::builder(Agent::load(src, "<t>").expect("load"))
        .build()
        .expect("build")
}

const MODEL: &str = "model { provider: mock, name: \"echo\" }";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_await_roundtrip() {
    let src = format!(
        "agent A {{ {MODEL} skill work(n: int) -> int {{ return n * 2 }} skill s() -> int {{ let h = spawn work(n: 21)\n return await h }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert!(matches!(out, Value::Int(42)), "got {out:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_returns_record_in_order() {
    let src = format!(
        "agent A {{ {MODEL} skill s() -> str {{ let r = parallel {{ a: 1 + 1\n b: 2 + 2 }}\n return \"{{r.a}}:{{r.b}}\" }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "2:4");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_state_writes_are_serialized() {
    // Two concurrent branches each bump shared state; writes serialize safely.
    let src = format!(
        "agent A {{ {MODEL} state n: int = 0 skill bump() -> int {{ n += 1\n return n }} skill s() -> int {{ let _r = parallel {{ a: bump()\n b: bump() }}\n return n }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert!(matches!(out, Value::Int(2)), "expected n==2, got {out:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_first_error_propagates() {
    let src = format!(
        "agent A {{ {MODEL} skill s() -> str {{ try {{ let r = parallel {{ ok: 41 + 1\n bad: 1 / 0 }}\n return \"no throw\" }} catch e {{ return \"caught: {{e.kind}}\" }} }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "caught: arithmetic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn await_non_future_errors() {
    let src = format!(
        "agent A {{ {MODEL} skill s() -> str {{ try {{ let x = 5\n let _y = await x\n return \"no\" }} catch e {{ return \"caught: {{e.kind}}\" }} }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "caught: type");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_then_await_many() {
    // spawn several, await each — results in spawn order.
    let src = format!(
        "agent A {{ {MODEL} skill sq(n: int) -> int {{ return n * n }} skill s() -> str {{ let h1 = spawn sq(n: 2)\n let h2 = spawn sq(n: 3)\n let h3 = spawn sq(n: 4)\n return \"{{await h1}}:{{await h2}}:{{await h3}}\" }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "4:9:16");
}
