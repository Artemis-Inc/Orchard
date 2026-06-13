//! IR & lowering tests, mirroring v2's `test_ir.py`.

use orchard_ir::{compile_source, dumps, from_ir};
use serde_json::Value;

const EXAMPLES: &[&str] = &[
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
    let path = format!(
        "{}/../orchard-syntax/tests/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    std::fs::read_to_string(path).expect("read fixture")
}

fn compile(name: &str) -> Value {
    compile_source(&fixture(name), name).unwrap_or_else(|e| panic!("compile {name}: {e:?}"))
}

#[test]
fn all_examples_compile() {
    for name in EXAMPLES {
        let _ = compile(name);
    }
}

#[test]
fn round_trip() {
    for name in EXAMPLES {
        let ir = compile(name);
        let reparsed = from_ir(&dumps(&ir)).unwrap();
        assert_eq!(ir, reparsed, "round-trip mismatch for {name}");
    }
}

#[test]
fn program_wrapper_shape() {
    let ir = compile("ex_14_1_hello.orch");
    for key in ["orchard", "manifest", "agents", "types", "enums", "fns"] {
        assert!(ir.get(key).is_some(), "missing top key {key}");
    }
    let agent = &ir["agents"][0];
    for key in [
        "name", "state", "types", "enums", "tools", "skills", "handlers", "fns",
    ] {
        assert!(agent.get(key).is_some(), "missing agent key {key}");
    }
}

#[test]
fn totality_every_node_has_span() {
    let ir = compile("ex_14_2_triage.orch");
    check_node_totality(&ir);
}

fn check_node_totality(v: &Value) {
    if let Value::Object(m) = v {
        if m.contains_key("node") {
            assert!(m.contains_key("span"), "node missing span: {m:?}");
        }
        for (_, val) in m {
            check_node_totality(val);
        }
    } else if let Value::Array(a) = v {
        for item in a {
            check_node_totality(item);
        }
    }
}

#[test]
fn keys_are_sorted() {
    // serde_json default Map is BTreeMap-backed → sorted keys (no preserve_order).
    let ir = compile("ex_14_1_hello.orch");
    let s = dumps(&ir);
    let agents = s.find("\"agents\"").unwrap();
    let manifest = s.find("\"manifest\"").unwrap();
    let orchard = s.find("\"orchard\"").unwrap();
    assert!(
        agents < manifest && manifest < orchard,
        "top-level keys not sorted"
    );
}

#[test]
fn concurrency_no_v2_1_flag() {
    let src = "agent A { model { provider: mock, name: \"m\" } use \"./r.orch\" as r\n skill s() -> str { let h = spawn r(\"x\"); return await h } }";
    let ir = compile_source(src, "<t>").unwrap();
    let dumped = dumps(&ir);
    assert!(
        !dumped.contains("v2_1"),
        "v3 IR must not carry the v2_1 flag"
    );
    assert!(dumped.contains("\"spawn\""));
    assert!(dumped.contains("\"await\""));
}

#[test]
fn literal_lowering() {
    let src = "agent A { model { provider: mock, name: \"m\" } on schedule(every: 30m) { let p = $0.50 } }";
    let ir = compile_source(src, "<t>").unwrap();
    let s = dumps(&ir);
    // duration → canonical string in IR
    assert!(s.contains("\"30m\""));
    // money → exact amount string in IR
    assert!(s.contains("\"0.50\""));
}

#[test]
fn manifest_basics() {
    let src = "agent Ivy { model { provider: anthropic, name: \"claude-opus-4-8\", api_key: env.ANTHROPIC_API_KEY } memory { facts: true } use web\n policy { max_spend: $1.50 } on message(t: str) -> str { return delegate t } }";
    let ir = compile_source(src, "<t>").unwrap();
    let m = &ir["manifest"];
    assert_eq!(m["name"], "Ivy");
    assert_eq!(m["model"]["provider"], "anthropic");
    assert_eq!(m["model"]["api_key"], "${ANTHROPIC_API_KEY}"); // env → secret form
    assert_eq!(m["model"]["max_tokens"], 4096); // default
    assert_eq!(m["policy"]["max_spend_usd"], 1.5); // money → float
    assert_eq!(m["memory"]["window"], 40); // default
    assert_eq!(m["policy"]["allow_shell"], "never");
    // `use web` → a pack tool entry
    assert!(m["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["kind"] == "pack" && t["name"] == "web"));
}

#[test]
fn ir_goldens() {
    let update = std::env::var("ORCH_UPDATE_GOLDEN").is_ok();
    for name in [
        "ex_14_1_hello.orch",
        "ex_14_2_triage.orch",
        "ex_14_3_research.orch",
        "ex_14_4_schedule.orch",
    ] {
        let ir = compile(name);
        let dumped = dumps(&ir) + "\n";
        let golden = format!(
            "{}/tests/golden/ir/{}.ir.json",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        if update {
            std::fs::create_dir_all(format!("{}/tests/golden/ir", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
            std::fs::write(&golden, &dumped).unwrap();
        } else {
            let want = std::fs::read_to_string(&golden)
                .unwrap_or_else(|_| panic!("missing golden {golden}; run ORCH_UPDATE_GOLDEN=1"));
            assert_eq!(dumped, want, "IR golden mismatch for {name}");
        }
    }
}
