//! Provider wire-format + egress-guard unit tests (offline).

use orchard_runtime::host_is_private;
use orchard_runtime::providers::remote::{anthropic_payload, openai_payload};
use orchard_runtime::{ChatRequest, Message, ToolDef};
use serde_json::json;

fn req(schema: Option<serde_json::Value>) -> ChatRequest {
    ChatRequest {
        system: "You are X.".into(),
        messages: vec![Message::user("hi")],
        tools: vec![ToolDef {
            name: "t".into(),
            description: "d".into(),
            schema: json!({"type": "object", "properties": {}}),
        }],
        temperature: Some(0.2),
        max_tokens: Some(256),
        schema,
    }
}

#[test]
fn anthropic_wire_shape() {
    let p = anthropic_payload(&req(None), "claude-opus-4-8");
    assert_eq!(p["model"], "claude-opus-4-8");
    assert_eq!(p["max_tokens"], 256);
    assert_eq!(p["system"], "You are X."); // system is top-level, not a message
    assert_eq!(p["messages"][0]["role"], "user");
    assert!(p["tools"].is_array());
    assert_eq!(p["tools"][0]["input_schema"]["type"], "object");
}

#[test]
fn anthropic_structured_output_forces_tool() {
    let p = anthropic_payload(&req(Some(json!({"type": "string"}))), "claude-sonnet-4-6");
    assert_eq!(p["tool_choice"]["name"], "respond_with_structured_output");
}

#[test]
fn openai_wire_uses_max_completion_tokens_and_json_schema() {
    let p = openai_payload(
        &req(Some(json!({"type": "object"}))),
        "gpt-5.2",
        "max_completion_tokens",
    );
    assert_eq!(p["max_completion_tokens"], 256);
    assert_eq!(p["messages"][0]["role"], "system"); // system as a leading message
    assert_eq!(p["response_format"]["type"], "json_schema");
    assert_eq!(p["tool_choice"], "auto");
}

#[test]
fn egress_blocks_private_addresses() {
    assert!(host_is_private("127.0.0.1"));
    assert!(host_is_private("10.0.0.1"));
    assert!(host_is_private("192.168.1.5"));
    assert!(host_is_private("169.254.1.1")); // link-local
    assert!(!host_is_private("8.8.8.8")); // public
}

#[test]
fn check_egress_rules() {
    use orchard_runtime::check_egress;
    let pub_url = url::Url::parse("https://api.example.com/x").unwrap();
    assert!(check_egress(&pub_url, &[], false).is_ok());
    let local = url::Url::parse("http://10.0.0.1/x").unwrap();
    assert!(check_egress(&local, &[], false).is_err()); // private blocked
    assert!(check_egress(&local, &[], true).is_ok()); // allow_local
                                                      // allowed_domains suffix match
    assert!(check_egress(&pub_url, &["example.com".into()], false).is_ok());
    assert!(check_egress(&pub_url, &["other.com".into()], false).is_err());
}
