//! P8 constrained-generation tests — offline. Mirrors v2's `test_gen_as_t.py`.

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

const SEV: &str = "enum Severity { low, medium, high, critical }";

#[tokio::test]
async fn gen_as_enum_synthesizes_valid_variant() {
    // echo mode + schema → synthesizes the first enum variant.
    let src = format!(
        "agent A {{ model {{ provider: mock, name: \"echo\" }} {SEV} skill s() -> Severity {{ return gen as Severity \"how bad?\" }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    match out {
        Value::Enum {
            enum_name, variant, ..
        } => {
            assert_eq!(enum_name, "Severity");
            assert_eq!(variant, "low"); // first variant
        }
        other => panic!("expected enum, got {other:?}"),
    }
}

#[tokio::test]
async fn gen_as_record_coerces_fields() {
    let src = "agent A { model { provider: mock, name: \"echo\" } type Extract { name: str, count: int } skill s() -> Extract { return gen as Extract \"pull fields\" } }";
    let out = session(src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    match out {
        Value::Record { type_name, fields } => {
            assert_eq!(type_name.as_deref(), Some("Extract"));
            assert!(matches!(fields.get("name"), Some(Value::Str(_))));
            assert!(matches!(fields.get("count"), Some(Value::Int(_))));
        }
        other => panic!("expected record, got {other:?}"),
    }
}

#[tokio::test]
async fn gen_as_list_of_str() {
    let src = "agent A { model { provider: mock, name: \"echo\" } skill s() -> list<str> { return gen as list<str> \"three tags\" } }";
    let out = session(src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    match out {
        Value::List(items) => assert!(items.iter().all(|i| matches!(i, Value::Str(_)))),
        other => panic!("expected list, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_then_valid_retries_once() {
    let src = format!(
        "agent A {{ model {{ provider: mock, name: \"invalid_then_valid.yaml\" }} {SEV} skill s() -> Severity {{ return gen as Severity \"how bad?\" }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    match out {
        Value::Enum { variant, .. } => assert_eq!(variant, "high"),
        other => panic!("expected enum high, got {other:?}"),
    }
}

#[tokio::test]
async fn exhaustion_raises_catchable_gen_error() {
    let src = format!(
        "agent A {{ model {{ provider: mock, name: \"exhaust.yaml\" }} {SEV} skill s() -> str {{ try {{ let x = gen as Severity \"how bad?\"\n return \"got: {{x}}\" }} catch e {{ return \"err: {{e.kind}}\" }} }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "err: GenError");
}

#[tokio::test]
async fn retry_until_composes() {
    // retry drives re-generation until the predicate holds; low,low,high.
    let src = format!(
        "agent A {{ model {{ provider: mock, name: \"retry_compose.yaml\" }} {SEV} skill s() -> Severity {{ return retry(3) {{ gen as Severity \"be decisive\" }} until (r) => r != Severity.low }} }}"
    );
    let out = session(&src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    match out {
        Value::Enum { variant, .. } => assert_eq!(variant, "high"),
        other => panic!("expected high, got {other:?}"),
    }
}

#[tokio::test]
async fn money_duration_roundtrip() {
    // a scripted reply with money/duration strings coerces to typed values.
    let src = "agent A { model { provider: mock, name: \"invoice.yaml\" } type Inv { amount: money, window: duration } skill s() -> str { let e = gen as Inv \"x\"\n return \"{e.amount}:{e.window}\" } }";
    let out = session(src)
        .skill("s", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(out.to_text(), "$0.50:30m");
}
