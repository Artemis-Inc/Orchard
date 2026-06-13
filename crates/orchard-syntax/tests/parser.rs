//! Parser tests, mirroring v2's `test_parser.py` + `test_parse_errors.py`.

use orchard_syntax::ast::*;
use orchard_syntax::parse_source;

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
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(path).expect("read fixture")
}

#[test]
fn all_fixtures_parse() {
    for name in FIXTURES {
        let src = fixture(name);
        let r = parse_source(&src, name);
        assert!(r.is_ok(), "fixture {name} failed to parse: {:?}", r.err());
    }
}

fn parse(src: &str) -> Program {
    parse_source(src, "<t>").expect("parse ok")
}

/// Pull the single agent's single handler/skill body's first statement expr.
fn agent(p: &Program) -> &AgentDecl {
    p.items
        .iter()
        .find_map(|it| {
            if let TopItem::Agent(a) = it {
                Some(a)
            } else {
                None
            }
        })
        .expect("an agent")
}

#[test]
fn delegate_bare_ident_operand() {
    let p = parse("agent A { on message(text: str) -> str { delegate text } }");
    let a = agent(&p);
    let on = a
        .members
        .iter()
        .find_map(|m| {
            if let AgentMember::On(o) = m {
                Some(o)
            } else {
                None
            }
        })
        .unwrap();
    match &on.body.stmts[0].kind {
        StmtKind::Expr(e) => match &e.kind {
            ExprKind::Delegate { goal, .. } => {
                assert!(matches!(goal.kind, ExprKind::Ident(ref n) if n == "text"));
            }
            other => panic!("expected delegate, got {other:?}"),
        },
        other => panic!("expected expr stmt, got {other:?}"),
    }
}

#[test]
fn gen_pipe_binds_as_call_on_result() {
    // `gen "x" |> trim` parses as `trim(gen "x")` — pipe is looser than the gen primary.
    let p = parse("fn f() -> str { gen \"x\" |> trim }");
    let body = &fn_body(&p, "f").stmts[0];
    match &body.kind {
        StmtKind::Expr(e) => match &e.kind {
            ExprKind::BinOp { op, left, .. } => {
                assert_eq!(op, "|>");
                assert!(matches!(left.kind, ExprKind::Gen { .. }));
            }
            other => panic!("expected pipe binop, got {other:?}"),
        },
        _ => panic!("expected expr stmt"),
    }
}

fn fn_body<'a>(p: &'a Program, name: &str) -> &'a Block {
    for it in &p.items {
        if let TopItem::Fn(c) = it {
            if c.name == name {
                return &c.body;
            }
        }
        if let TopItem::Agent(a) = it {
            for m in &a.members {
                if let AgentMember::Fn(c) | AgentMember::Skill(c) | AgentMember::Tool(c) = m {
                    if c.name == name {
                        return &c.body;
                    }
                }
            }
        }
    }
    panic!("no fn {name}");
}

fn bind_value(b: &Block) -> &ExprKind {
    match &b.stmts[0].kind {
        StmtKind::Bind { value, .. } => &value.kind,
        other => panic!("expected binding, got {other:?}"),
    }
}

#[test]
fn map_vs_config_vs_empty_map() {
    // A bare `{` at statement start is a block, so these must be in expression
    // (binding) position. {"k": v} -> MapLit (string key).
    let p = parse("fn f() -> json { let m = {\"a\": 1}; m }");
    assert!(matches!(bind_value(fn_body(&p, "f")), ExprKind::MapLit(_)));
    // {a: 1} -> ConfigLit (ident key)
    let p = parse("fn f() -> json { let m = {a: 1}; m }");
    assert!(matches!(
        bind_value(fn_body(&p, "f")),
        ExprKind::ConfigLit { .. }
    ));
    // {:} -> empty MapLit
    let p = parse("fn f() -> json { let m = {:}; m }");
    assert!(matches!(bind_value(fn_body(&p, "f")), ExprKind::MapLit(v) if v.is_empty()));
}

#[test]
fn record_literal_with_type_name() {
    let p = parse("type T { x: int }\nfn f() -> T { T { x: 1 } }");
    match &fn_body(&p, "f").stmts[0].kind {
        StmtKind::Expr(e) => match &e.kind {
            ExprKind::ConfigLit { type_name, .. } => assert_eq!(type_name.as_deref(), Some("T")),
            other => panic!("expected record literal, got {other:?}"),
        },
        _ => panic!(),
    }
}

#[test]
fn generics_ge_resplit() {
    // `list<int>= []` — the `>=` must split into `>` (close) and `=` (assign).
    let p = parse("fn f() { let xs: list<int>= [] }");
    match &fn_body(&p, "f").stmts[0].kind {
        StmtKind::Bind { ty, .. } => {
            let t = ty.as_ref().unwrap();
            assert_eq!(t.name, "list");
            assert_eq!(t.args[0].name, "int");
        }
        _ => panic!("expected binding"),
    }
}

#[test]
fn nested_generics_close() {
    let p = parse("fn f(x: list<list<int>>) {}");
    let c = match &p.items[0] {
        TopItem::Fn(c) => c,
        _ => panic!(),
    };
    let t = c.params[0].ty.as_ref().unwrap();
    assert_eq!(t.name, "list");
    assert_eq!(t.args[0].name, "list");
    assert_eq!(t.args[0].args[0].name, "int");
}

#[test]
fn gen_with_before_or_after() {
    let before = parse("fn f() -> str { gen with { temperature: 0.2 } \"hi\" }");
    let after = parse("fn f() -> str { gen \"hi\" with { temperature: 0.2 } }");
    for p in [before, after] {
        match &fn_body(&p, "f").stmts[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Gen { with_config, .. } => assert!(with_config.is_some()),
                _ => panic!("expected gen"),
            },
            _ => panic!(),
        }
    }
}

#[test]
fn gen_with_both_sides_errors() {
    let r = parse_source("fn f() -> str { gen with {a:1} \"hi\" with {b:2} }", "<t>");
    assert!(r.unwrap_err().diagnostic.message.contains("single with"));
}

#[test]
fn retry_until_lambda() {
    let p = parse("fn f() -> int { retry(3) { 1 } until (r) => r > 0 }");
    match &fn_body(&p, "f").stmts[0].kind {
        StmtKind::Expr(e) => match &e.kind {
            ExprKind::Retry { until, .. } => assert!(matches!(until.kind, ExprKind::Lambda { .. })),
            _ => panic!("expected retry"),
        },
        _ => panic!(),
    }
}

#[test]
fn dump_is_deterministic() {
    let p = parse("agent A { on message(text: str) -> str { return gen \"hi {text}\" } }");
    let d1 = p.dump();
    let d2 = parse("agent A { on message(text: str) -> str { return gen \"hi {text}\" } }").dump();
    assert_eq!(d1, d2);
    assert!(!d1.contains("Delegate"));
    assert!(d1.contains("\"Gen\""));
}

#[test]
fn ast_goldens() {
    // Golden AST dumps for the canonical examples. Regenerate with
    // ORCH_UPDATE_GOLDEN=1.
    let update = std::env::var("ORCH_UPDATE_GOLDEN").is_ok();
    for name in [
        "ex_14_1_hello.orch",
        "ex_14_2_triage.orch",
        "ex_14_3_research.orch",
        "ex_14_4_schedule.orch",
    ] {
        let src = fixture(name);
        let dump = parse(&src).dump();
        let golden = format!(
            "{}/tests/golden/parser/{}.ast",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        if update {
            std::fs::create_dir_all(format!(
                "{}/tests/golden/parser",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            std::fs::write(&golden, &dump).unwrap();
        } else {
            let want = std::fs::read_to_string(&golden)
                .unwrap_or_else(|_| panic!("missing golden {golden}; run ORCH_UPDATE_GOLDEN=1"));
            assert_eq!(dump, want, "AST golden mismatch for {name}");
        }
    }
}

// ---- parse errors ----

fn perr(src: &str) -> String {
    parse_source(src, "<t>").unwrap_err().diagnostic.message
}

#[test]
fn parse_errors() {
    assert!(perr("agent A { model { provider mock } }").contains("expected ':'"));
    assert!(perr("agent A { state x int }").contains("expected ':'"));
    assert!(perr("agent { }").contains("the agent name"));
    assert!(perr("agent A { skill s() }").contains("expected '{'"));
    assert!(perr("agent A { fn f() { let = 1 } }").contains("a variable name"));
    assert!(perr("agent A { on bogus() {} }").contains("unknown handler"));
    assert!(perr("agent A { on schedule(daily: 1) {} }").contains("'every' or 'cron'"));
    assert!(perr("stray").contains("top-level item"));
    assert!(perr("agent A { nonsense }").contains("agent member"));
    assert!(perr("use mcp(\"x\")").contains("expected 'as'"));
    assert!(perr("fn f() { let x = {} }").contains("empty block"));
    assert!(perr("fn f() { 1 + }").contains("expected an expression"));
    assert!(perr("fn f() { match x { 1 } }").contains("'=>'"));
    assert!(perr("agent A {").contains("expected")); // unclosed
    assert!(perr("fn f() { (1 }").contains("expected ')'"));
}
