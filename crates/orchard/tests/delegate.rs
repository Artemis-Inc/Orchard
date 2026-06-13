//! P7 delegate + skill-exposure tests — offline via mock scripts. Mirrors v2's
//! `test_delegate.py`.

use orchard::{Agent, Runtime, Value};

fn scripts_dir() -> String {
    format!("{}/tests/scripts", env!("CARGO_MANIFEST_DIR"))
}

fn session(src: &str) -> orchard::Session {
    let agent = Agent::load(src, "<t>").expect("load");
    Runtime::builder(agent)
        .base_dir(scripts_dir())
        .build()
        .expect("build")
}

const TRIAGE: &str = r#"
agent Triage {
    model { provider: mock, name: "triage.yaml" }
    memory { facts: true }
    state handled: int = 0
    skill triage(report: str) -> str {
        state.handled += 1
        remember "last" = report
        return "triaged: {report}"
    }
    skill peek() -> int { return handled }
    on message(text: str) -> str { return delegate text }
}
"#;

#[tokio::test]
async fn delegate_runs_exposed_skill_end_to_end() {
    let s = session(TRIAGE);
    let out = s.message("triage this").await.unwrap();
    assert_eq!(out, "Done — triaged.");
    // the skill body really ran: state bumped + fact written
    let handled = s.skill("peek", serde_json::json!({})).await.unwrap();
    assert!(matches!(handled, Value::Int(1)), "handled = {handled:?}");
}

#[tokio::test]
async fn budget_stops_delegate_at_cap() {
    let src = r#"
agent B {
    model { provider: mock, name: "budget.yaml" }
    tool noop() -> str { return "ok" }
    on message(text: str) -> str { return budget(steps: 1) { delegate text } }
}
"#;
    let out = session(src).message("go").await.unwrap();
    assert!(out.contains("stopped by policy"), "got: {out}");
}

#[tokio::test]
async fn with_tools_restricts_then_allows() {
    // skill_triage is advertised when listed → the script's call succeeds.
    let src = r#"
agent R {
    model { provider: mock, name: "restrict.yaml" }
    skill triage(report: str) -> str { return "t:{report}" }
    on message(text: str) -> str { return delegate text with { tools: [triage] } }
}
"#;
    let out = session(src).message("go").await.unwrap();
    assert_eq!(out, "ok");
}

#[tokio::test]
async fn with_tools_excluding_skill_makes_call_fail() {
    // skill_triage is NOT advertised (only some other tool) → the mock errors.
    let src = r#"
agent R {
    model { provider: mock, name: "restrict.yaml" }
    skill triage(report: str) -> str { return "t:{report}" }
    skill other() -> str { return "o" }
    on message(text: str) -> str { return delegate text with { tools: [other] } }
}
"#;
    let r = session(src).message("go").await;
    assert!(
        r.is_err(),
        "expected error when skill not advertised, got {r:?}"
    );
}

#[tokio::test]
async fn direct_skill_call_still_works() {
    let s = session(TRIAGE);
    let out = s
        .skill("triage", serde_json::json!({"report": "direct"}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "triaged: direct");
}
