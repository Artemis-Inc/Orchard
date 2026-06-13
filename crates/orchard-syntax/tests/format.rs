//! Formatter tests: idempotent, AST-preserving, comment-preserving.

use orchard_syntax::{format_source, parse_source};

const FIXTURES: &[&str] = &[
    "assistant.orch",
    "coder.orch",
    "demo-offline.orch",
    "ex_14_1_hello.orch",
    "ex_14_2_triage.orch",
    "ex_14_3_research.orch",
    "ex_14_4_schedule.orch",
    "mcp-notes.orch",
    "ollama-local.orch",
    "pipeline.orch",
    "researcher.orch",
    "triage.orch",
];

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    ))
    .unwrap()
}

#[test]
fn formatting_is_idempotent() {
    for name in FIXTURES {
        let src = fixture(name);
        let once = format_source(&src, name).unwrap_or_else(|e| panic!("fmt {name}: {e}"));
        let twice = format_source(&once, name).unwrap();
        assert_eq!(once, twice, "fmt not idempotent for {name}");
    }
}

#[test]
fn formatting_preserves_ast() {
    for name in FIXTURES {
        let src = fixture(name);
        let formatted = format_source(&src, name).unwrap();
        let a = parse_source(&src, name).unwrap().dump();
        let b = parse_source(&formatted, name).unwrap().dump();
        assert_eq!(a, b, "fmt changed the AST for {name}");
    }
}

#[test]
fn output_ends_in_one_newline() {
    for name in FIXTURES {
        let formatted = format_source(&fixture(name), name).unwrap();
        assert!(formatted.ends_with('\n'));
        assert!(!formatted.ends_with("\n\n"));
    }
}

#[test]
fn comments_are_preserved() {
    let src = "agent A {\n    // a leading comment\n    model { provider: mock, name: \"echo\" }\n    // before the handler\n    on message(text: str) -> str { return gen \"hi {text}\" }\n}\n";
    let out = format_source(src, "<t>").unwrap();
    assert!(
        out.contains("// a leading comment"),
        "lost leading comment: {out}"
    );
    assert!(
        out.contains("// before the handler"),
        "lost handler comment: {out}"
    );
}

#[test]
fn canonical_style() {
    let src = "agent   A{model{provider:mock,name:\"echo\"}\non message(text:str)->str{return gen \"hi\"}}";
    let out = format_source(src, "<t>").unwrap();
    assert!(out.contains("agent A {"));
    assert!(out.contains("    model { provider: mock, name: \"echo\" }"));
    assert!(out.contains("    on message(text: str) -> str {"));
}
