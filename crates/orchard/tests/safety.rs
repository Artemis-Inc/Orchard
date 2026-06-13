//! P13 safety tests: circuit breaker in the delegate loop + secret redaction.

use orchard::{Agent, NativeTool, Runtime};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn scripts_dir() -> String {
    format!("{}/tests/scripts", env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
async fn circuit_breaker_disables_a_failing_tool() {
    // A native tool that always fails; the loop calls it 4×. After 3 identical
    // failures it is disabled, so the 4th call is short-circuited (never runs).
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    let flaky = NativeTool::builder("flaky")
        .description("always fails")
        .handler(move |_args| {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "error": "boom" }))
            }
        });
    let src = "agent A { model { provider: mock, name: \"breaker.yaml\" } on message(text: str) -> str { return delegate text } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent)
        .base_dir(scripts_dir())
        .register_tool(flaky)
        .build()
        .unwrap();
    let _ = s.message("go").await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "4th identical call should be short-circuited"
    );
}

#[tokio::test]
async fn env_secret_is_redacted_in_output() {
    std::env::set_var("ORCH_TEST_SECRET", "supersecretvalue123");
    let src = "agent A { model { provider: mock, name: \"echo\" } skill leak() -> str { return gen \"the key is {env.ORCH_TEST_SECRET}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    let s = Runtime::builder(agent).build().unwrap();
    let out = s
        .skill("leak", serde_json::json!({}))
        .await
        .unwrap()
        .to_text();
    assert!(
        out.contains("«ORCH_TEST_SECRET»"),
        "secret not redacted: {out}"
    );
    assert!(
        !out.contains("supersecretvalue123"),
        "raw secret leaked: {out}"
    );
}
