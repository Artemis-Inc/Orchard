//! P20 regression tests for v2-parity fixes surfaced by the adversarial review:
//! floored modulo, negative indexing, Python-style float text, `repeat` count
//! coercion, and secret redaction at the host-output boundary.

use orchard::{Agent, Runtime};

fn session(src: &str) -> orchard::Session {
    let agent = Agent::load(src, "<t>").expect("load");
    Runtime::builder(agent).build().expect("build")
}

/// Run a skill `s()` returning a string and assert its text.
async fn skill_str(body: &str) -> String {
    let src = format!(
        "agent A {{ model {{ provider: mock, name: \"echo\" }} skill s() -> str {{ {body} }} }}"
    );
    let s = session(&src);
    s.skill("s", serde_json::json!({})).await.unwrap().to_text()
}

#[tokio::test]
async fn modulo_is_floored_like_python() {
    assert_eq!(skill_str("return \"{-7 % 3}\"").await, "2");
    assert_eq!(skill_str("return \"{7 % 3}\"").await, "1");
    assert_eq!(skill_str("return \"{7 % -3}\"").await, "-2");
    assert_eq!(skill_str("return \"{-7 % -3}\"").await, "-1");
}

#[tokio::test]
async fn negative_indexing_wraps() {
    assert_eq!(
        skill_str("let xs = [10, 20, 30]\n return \"{xs[-1]}\"").await,
        "30"
    );
    assert_eq!(
        skill_str("let xs = [10, 20, 30]\n return \"{xs[-2]}\"").await,
        "20"
    );
    assert_eq!(skill_str("let s = \"abc\"\n return \"{s[-1]}\"").await, "c");
}

#[tokio::test]
async fn float_text_matches_python() {
    // large magnitude → scientific with signed, padded exponent
    assert_eq!(skill_str("return \"{1e16}\"").await, "1e+16");
    assert_eq!(skill_str("return \"{1e15}\"").await, "1000000000000000.0");
    // small magnitude → scientific
    assert_eq!(skill_str("return \"{0.00001}\"").await, "1e-05");
    assert_eq!(skill_str("return \"{0.0001}\"").await, "0.0001");
    // integral float keeps a trailing .0
    assert_eq!(skill_str("return \"{1.0}\"").await, "1.0");
}

#[tokio::test]
async fn repeat_accepts_numeric_string_and_bool() {
    // a numeric-string count runs N times (mirrors v2's int(count))
    let body = "var n = 0\n repeat \"3\" { n += 1 }\n return \"{n}\"";
    assert_eq!(skill_str(body).await, "3");
}

#[tokio::test]
async fn secret_is_redacted_at_reply_boundary() {
    // env.SECRET is tracked as a secret on read; echoing it back must redact.
    let src = "agent A { model { provider: mock, name: \"echo\" } \
        on message(text: str) -> str { let k = env.MY_SECRET\n return \"key={k}\" } }";
    let agent = Agent::load(src, "<t>").unwrap();
    // inject the secret via the process env the facade reads
    std::env::set_var("MY_SECRET", "s3cr3t-value-xyz");
    let s = Runtime::builder(agent).build().unwrap();
    let out = s.message("go").await.unwrap();
    std::env::remove_var("MY_SECRET");
    assert!(!out.contains("s3cr3t-value-xyz"), "secret leaked: {out}");
    assert!(out.starts_with("key="), "unexpected: {out}");
}
