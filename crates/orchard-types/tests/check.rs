//! Checker tests, mirroring v2's `test_check.py`.

use orchard_types::types::{assignable, from_typeref, to_json_schema, EnumType, RecordType, Type};
use orchard_types::{check_source, Type as _Ty};
use std::collections::BTreeMap;

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

fn errors(src: &str) -> Vec<String> {
    check_source(src, "<t>")
        .into_iter()
        .filter(|d| d.is_error())
        .map(|d| d.message)
        .collect()
}

#[test]
fn examples_check_clean() {
    for name in EXAMPLES {
        let src = fixture(name);
        let errs = errors(&src);
        assert!(errs.is_empty(), "example {name} has errors: {errs:?}");
    }
}

fn has_err(src: &str, needle: &str) -> bool {
    errors(src).iter().any(|m| m.contains(needle))
}

const A: &str = "agent A { model { provider: mock, name: \"m\" }";

#[test]
fn undefined_name_and_suggestion() {
    assert!(has_err(
        &format!("{A} on message(text: str) -> str {{ return mystery }} }}"),
        "undefined name 'mystery'"
    ));
    let src = format!("{A} tool get_weather(c: str) -> str {{ \"x\" }} skill s() -> str {{ return get_wether(c: \"x\") }} }}");
    assert!(
        has_err(&src, "did you mean 'get_weather'") || has_err(&src, "undefined name 'get_wether'")
    );
}

#[test]
fn unknown_type() {
    assert!(has_err(
        &format!("{A} skill s() -> Bogus {{ return \"x\" }} }}"),
        "unknown type 'Bogus'"
    ));
    assert!(has_err(
        &format!("{A} state n: Bogus = 0 }}"),
        "unknown type 'Bogus'"
    ));
}

#[test]
fn non_exhaustive_match() {
    let src = format!(
        "{A} enum Sev {{ low, high }} skill s() -> str {{ let x = gen as Sev \"q\"; match x {{ low => \"l\" }} }} }}"
    );
    assert!(has_err(&src, "non-exhaustive match"));
}

#[test]
fn enum_pattern_arity() {
    let src = format!(
        "{A} enum R {{ ok(v: str), err(reason: str) }} skill s() -> str {{ let x = gen as R \"q\"; match x {{ ok(a, b) => a  err(r) => r }} }} }}"
    );
    assert!(has_err(&src, "carries 1 value"));
}

#[test]
fn fn_purity() {
    assert!(has_err(
        "fn f() -> str { return gen \"x\" }",
        "must be pure"
    ));
    assert!(has_err(
        &format!("{A} state n: int = 0  fn f() {{ n = 1 }} }}"),
        "must be pure"
    ));
}

#[test]
fn tool_restrictions() {
    assert!(has_err(
        &format!("{A} tool t() -> str {{ return gen \"x\" }} }}"),
        "is not allowed in tool"
    ));
    assert!(has_err(
        &format!("{A} state n: int = 0  tool t() {{ n = 1 }} }}"),
        "may not modify state"
    ));
}

#[test]
fn duplicate_names() {
    assert!(has_err(
        &format!("{A} fn foo() {{}} skill foo() -> str {{ return \"x\" }} }}"),
        "duplicate name 'foo'"
    ));
    assert!(has_err(
        "type X { a: int }\nenum X { a }",
        "duplicate type/enum name 'X'"
    ));
}

#[test]
fn config_validity() {
    assert!(has_err(
        "agent A { model { provder: mock, name: \"m\" } }",
        "unknown key 'provder'"
    ));
    assert!(has_err(
        "agent A { model { provider: bogusprov, name: \"m\" } }",
        "is not one of"
    ));
    assert!(has_err(
        "agent A { model { provider: mock } }",
        "missing required key 'name'"
    ));
    assert!(has_err(
        "agent A { model { provider: mock, name: \"m\" } policy { allow_shell: maybe } }",
        "allow_shell: must be never"
    ));
}

#[test]
fn budget_tighten() {
    let src = format!(
        "{A} policy {{ max_spend: $1.00 }} on message(text: str) -> str {{ return budget(spend: $5.00) {{ delegate text }} }} }}"
    );
    assert!(has_err(&src, "looser than the enclosing"));
}

#[test]
fn record_literal_checks() {
    let src = format!(
        "{A} type T {{ x: int, y: str }} skill s() -> T {{ return T {{ x: 1, z: 2 }} }} }}"
    );
    assert!(has_err(&src, "unknown field 'z'"));
    let src =
        format!("{A} type T {{ x: int, y: str }} skill s() -> T {{ return T {{ x: 1 }} }} }}");
    assert!(has_err(&src, "missing required field 'y'"));
}

#[test]
fn dead_code_and_immutable() {
    assert!(has_err(
        &format!("{A} on message(text: str) -> str {{ reply \"a\"\n reply \"b\" }} }}"),
        "unreachable code after 'reply'"
    ));
    assert!(has_err(
        "fn f() { let x = 1\n x = 2 }",
        "cannot reassign immutable binding 'x'"
    ));
}

#[test]
fn unknown_pack() {
    assert!(has_err(
        "agent A { use boguspack\n model { provider: mock, name: \"m\" } }",
        "unknown tool pack 'boguspack'"
    ));
}

#[test]
fn duplicate_handler() {
    let src = format!("{A} on message(t: str) -> str {{ return \"a\" }} on message(t: str) -> str {{ return \"b\" }} }}");
    assert!(has_err(&src, "duplicate 'message' handler"));
}

#[test]
fn tool_name_regex() {
    assert!(has_err(
        &format!("{A} tool BadName() -> str {{ \"x\" }} }}"),
        "must match [a-z]"
    ));
}

#[test]
fn concurrency_is_allowed() {
    // v3 deviation: spawn/await/parallel must NOT be rejected.
    let src = format!(
        "{A} use \"./r.orch\" as r  skill s() -> str {{ let h = spawn r(\"x\"); return await h }} }}"
    );
    let errs = errors(&src);
    assert!(
        !errs.iter().any(|m| m.contains("v2.1")),
        "concurrency wrongly rejected: {errs:?}"
    );
}

#[test]
fn lint_warnings_not_errors() {
    // secret-into-reply and unused binding are warnings, not errors.
    let src = format!("{A} on message(text: str) -> str {{ reply env.SECRET_KEY }} }}");
    let diags = check_source(&src, "<t>");
    assert!(diags
        .iter()
        .any(|d| !d.is_error() && d.message.contains("secret-derived")));
    assert!(errors(&src).is_empty());
}

// ---- type-level unit tests ----

#[test]
fn assignable_rules() {
    assert!(assignable(&Type::int(), &Type::float())); // int widens
    assert!(!assignable(&Type::str_(), &Type::int()));
    assert!(assignable(
        &Type::null(),
        &Type::Optional(Box::new(Type::str_()))
    ));
    assert!(assignable(&Type::json(), &Type::int())); // dynamic
    assert!(assignable(&Type::int(), &Type::json()));
    assert!(assignable(
        &Type::List(Box::new(Type::int())),
        &Type::List(Box::new(Type::float()))
    ));
}

#[test]
fn schema_lowering() {
    let env = BTreeMap::new();
    // enum (all payload-less) -> string enum
    let sev = Type::Enum(EnumType {
        name: "Sev".into(),
        variants: vec![("low".into(), vec![]), ("high".into(), vec![])],
    });
    let s = to_json_schema(&sev, &env);
    assert_eq!(s["type"], "string");
    assert_eq!(s["enum"][0], "low");
    // tagged-union enum -> {}
    let r = Type::Enum(EnumType {
        name: "R".into(),
        variants: vec![("ok".into(), vec![Type::str_()])],
    });
    assert_eq!(to_json_schema(&r, &env), serde_json::json!({}));
    // money -> string
    assert_eq!(
        to_json_schema(&Type::money(), &env),
        serde_json::json!({"type": "string"})
    );
    // json -> {}
    assert_eq!(to_json_schema(&Type::json(), &env), serde_json::json!({}));
    // record with required + optional
    let rec = Type::Record(RecordType {
        name: "T".into(),
        fields: vec![
            ("a".into(), Type::str_(), true),
            ("b".into(), Type::Optional(Box::new(Type::int())), false),
        ],
    });
    let rs = to_json_schema(&rec, &env);
    assert_eq!(rs["type"], "object");
    assert_eq!(rs["required"], serde_json::json!(["a"]));
    assert_eq!(rs["properties"]["b"]["type"], "integer");
}

#[test]
fn from_typeref_basics() {
    use orchard_syntax::ast::TypeRef;
    use orchard_syntax::Span;
    let sp = Span::point("", 0, 0, 0);
    let opt = TypeRef {
        name: "str".into(),
        args: vec![],
        optional: true,
        span: sp,
    };
    assert!(matches!(from_typeref(Some(&opt)), Type::Optional(_)));
    let _ = _Ty::any();
}
