//! AST → IR lowering. Ports v2's `lower.py`. The IR is a `serde_json::Value`
//! tree (mirroring v2's dict IR); every behavior/declaration node is
//! `{"node": tag, "span": {file,line,col}|null, ...fields}`.
//!
//! Deviation from v2: `spawn`/`await`/`parallel` are first-class and carry **no**
//! `v2_1` marker.

use crate::manifest::build_manifest;
use orchard_syntax::ast::*;
use orchard_syntax::Span;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// The Orchard version string emitted in the IR when no pragma is present.
pub const ORCHARD_VERSION: &str = "3.0";

/// Lower a whole program to the IR Value.
pub fn lower_program(program: &Program) -> Value {
    let version = program
        .pragma
        .clone()
        .unwrap_or_else(|| ORCHARD_VERSION.to_string());
    let mut agents = Vec::new();
    let mut types = Vec::new();
    let mut enums = Vec::new();
    let mut fns = Vec::new();
    for item in &program.items {
        match item {
            TopItem::Agent(a) => agents.push(lower_agent(a)),
            TopItem::Type(t) => types.push(lower_typedef(t)),
            TopItem::Enum(e) => enums.push(lower_enumdef(e)),
            TopItem::Fn(f) => fns.push(lower_callable("fn", f)),
            TopItem::Use(_) => {} // consumed by the manifest
        }
    }
    json!({
        "orchard": version,
        "manifest": build_manifest(program),
        "agents": agents,
        "types": types,
        "enums": enums,
        "fns": fns,
    })
}

// ---- node helpers ----

fn span_val(span: &Span) -> Value {
    json!({ "file": span.file, "line": span.line, "col": span.col })
}

fn opt_span(span: &Span) -> Value {
    if span.file.is_empty() && span.line == 0 {
        Value::Null
    } else {
        span_val(span)
    }
}

fn node(tag: &str, span: Value, fields: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    m.insert("node".into(), json!(tag));
    m.insert("span".into(), span);
    for (k, v) in fields {
        m.insert(k.into(), v);
    }
    Value::Object(m)
}

// ---- agent ----

fn lower_agent(a: &AgentDecl) -> Value {
    let mut state = Vec::new();
    let mut types = Vec::new();
    let mut enums = Vec::new();
    let mut tools = Vec::new();
    let mut skills = Vec::new();
    let mut handlers = Vec::new();
    let mut fns = Vec::new();
    for m in &a.members {
        match m {
            AgentMember::State(s) => state.push(lower_state(s)),
            AgentMember::Type(t) => types.push(lower_typedef(t)),
            AgentMember::Enum(e) => enums.push(lower_enumdef(e)),
            AgentMember::Tool(t) => tools.push(lower_callable("tool", t)),
            AgentMember::Skill(s) => skills.push(lower_callable("skill", s)),
            AgentMember::Fn(f) => fns.push(lower_callable("fn", f)),
            AgentMember::On(o) => handlers.push(lower_handler(o)),
            AgentMember::Config(_) | AgentMember::Use(_) => {} // consumed by the manifest
        }
    }
    node(
        "agent",
        opt_span(&a.span),
        vec![
            ("name", json!(a.name)),
            ("state", json!(state)),
            ("types", json!(types)),
            ("enums", json!(enums)),
            ("tools", json!(tools)),
            ("skills", json!(skills)),
            ("handlers", json!(handlers)),
            ("fns", json!(fns)),
        ],
    )
}

fn lower_state(s: &StateDecl) -> Value {
    node(
        "state",
        opt_span(&s.span),
        vec![
            ("name", json!(s.name)),
            ("type", lower_type(Some(&s.ty))),
            ("default", opt_expr(s.default.as_ref())),
        ],
    )
}

fn lower_typedef(t: &TypeDecl) -> Value {
    let fields: Vec<Value> = t
        .fields
        .iter()
        .map(|f| {
            node(
                "fielddef",
                opt_span(&f.span),
                vec![
                    ("name", json!(f.name)),
                    ("type", lower_type(Some(&f.ty))),
                    ("default", opt_expr(f.default.as_ref())),
                ],
            )
        })
        .collect();
    node(
        "typedef",
        opt_span(&t.span),
        vec![("name", json!(t.name)), ("fields", json!(fields))],
    )
}

fn lower_enumdef(e: &EnumDecl) -> Value {
    let variants: Vec<Value> = e
        .variants
        .iter()
        .map(|v| {
            let params: Vec<Value> = v.params.iter().map(lower_param).collect();
            node(
                "variant",
                opt_span(&v.span),
                vec![("name", json!(v.name)), ("params", json!(params))],
            )
        })
        .collect();
    node(
        "enumdef",
        opt_span(&e.span),
        vec![("name", json!(e.name)), ("variants", json!(variants))],
    )
}

fn lower_param(p: &Param) -> Value {
    node(
        "param",
        opt_span(&p.span),
        vec![
            ("name", json!(p.name)),
            ("type", lower_type(p.ty.as_ref())),
            ("default", opt_expr(p.default.as_ref())),
        ],
    )
}

fn lower_callable(tag: &str, c: &Callable) -> Value {
    let params: Vec<Value> = c.params.iter().map(lower_param).collect();
    let annotations: Vec<Value> = c
        .annotations
        .iter()
        .map(|a| {
            let args: Vec<Value> = a.args.iter().map(lower_arg).collect();
            node(
                "annotation",
                opt_span(&a.span),
                vec![("name", json!(a.name)), ("args", json!(args))],
            )
        })
        .collect();
    node(
        tag,
        opt_span(&c.span),
        vec![
            ("name", json!(c.name)),
            ("params", json!(params)),
            ("return_type", lower_type(c.return_type.as_ref())),
            ("annotations", json!(annotations)),
            ("body", lower_block(&c.body)),
            ("external", json!(external_effects(&c.body, false))),
        ],
    )
}

fn lower_handler(o: &OnDecl) -> Value {
    let kind = match o.kind {
        HandlerKind::Start => "start",
        HandlerKind::Message => "message",
        HandlerKind::Schedule => "schedule",
        HandlerKind::File => "file",
    };
    node(
        "handler",
        opt_span(&o.span),
        vec![
            ("kind", json!(kind)),
            (
                "param",
                o.param.as_ref().map(lower_param).unwrap_or(Value::Null),
            ),
            ("schedule_kind", json!(o.schedule_kind)),
            ("schedule_value", opt_expr(o.schedule_value.as_ref())),
            ("watch_path", opt_expr(o.watch_path.as_ref())),
            ("return_type", lower_type(o.return_type.as_ref())),
            ("body", lower_block(&o.body)),
        ],
    )
}

fn lower_type(tr: Option<&TypeRef>) -> Value {
    match tr {
        None => Value::Null,
        Some(t) => {
            let args: Vec<Value> = t.args.iter().map(|a| lower_type(Some(a))).collect();
            node(
                "type",
                opt_span(&t.span),
                vec![
                    ("name", json!(t.name)),
                    ("args", json!(args)),
                    ("optional", json!(t.optional)),
                ],
            )
        }
    }
}

// ---- statements ----

fn lower_block(b: &Block) -> Value {
    let body: Vec<Value> = b.stmts.iter().map(lower_stmt).collect();
    node("block", opt_span(&b.span), vec![("body", json!(body))])
}

fn lower_stmt(s: &Stmt) -> Value {
    let sp = opt_span(&s.span);
    match &s.kind {
        StmtKind::Block(b) => lower_block(b),
        StmtKind::Bind {
            name,
            ty,
            value,
            mutable,
        } => node(
            "bind",
            sp,
            vec![
                ("name", json!(name)),
                ("mutable", json!(mutable)),
                ("type", lower_type(ty.as_ref())),
                ("value", lower_expr(value)),
            ],
        ),
        StmtKind::Assign { target, op, value } => node(
            "assign",
            sp,
            vec![
                ("target", lower_expr(target)),
                ("op", json!(op)),
                ("value", lower_expr(value)),
            ],
        ),
        StmtKind::If {
            branches,
            else_block,
        } => {
            let brs: Vec<Value> = branches
                .iter()
                .map(|(cond, body)| {
                    node(
                        "branch",
                        opt_span(&cond.span),
                        vec![("cond", lower_expr(cond)), ("then", lower_block(body))],
                    )
                })
                .collect();
            node(
                "if",
                sp,
                vec![
                    ("branches", json!(brs)),
                    (
                        "else_block",
                        else_block.as_ref().map(lower_block).unwrap_or(Value::Null),
                    ),
                ],
            )
        }
        StmtKind::For { var, iter, body } => node(
            "for",
            sp,
            vec![
                ("var", json!(var)),
                ("iter", lower_expr(iter)),
                ("body", lower_block(body)),
            ],
        ),
        StmtKind::While { cond, body } => node(
            "while",
            sp,
            vec![("cond", lower_expr(cond)), ("body", lower_block(body))],
        ),
        StmtKind::Repeat { count, body } => node(
            "repeat",
            sp,
            vec![("count", lower_expr(count)), ("body", lower_block(body))],
        ),
        StmtKind::Return(v) => node("return", sp, vec![("value", opt_expr(v.as_ref()))]),
        StmtKind::Break => node("break", sp, vec![]),
        StmtKind::Continue => node("continue", sp, vec![]),
        StmtKind::Try {
            body,
            catch_name,
            catch_block,
        } => node(
            "try",
            sp,
            vec![
                ("body", lower_block(body)),
                ("catch_name", json!(catch_name)),
                ("catch", lower_block(catch_block)),
            ],
        ),
        StmtKind::Throw(v) => node("throw", sp, vec![("value", lower_expr(v))]),
        StmtKind::Remember {
            key,
            value,
            auto_key,
        } => {
            let key_v = if *auto_key {
                Value::Null
            } else {
                match key {
                    Some(RememberKey::Ident(n)) => node(
                        "lit",
                        sp.clone(),
                        vec![("type", json!("str")), ("value", json!(n))],
                    ),
                    Some(RememberKey::Expr(e)) => lower_expr(e),
                    None => Value::Null,
                }
            };
            node(
                "remember",
                sp,
                vec![
                    ("key", key_v),
                    ("value", lower_expr(value)),
                    ("auto_key", json!(auto_key)),
                ],
            )
        }
        StmtKind::Forget(v) => node("forget", sp, vec![("target", lower_expr(v))]),
        StmtKind::Reply(v) => node("reply", sp, vec![("value", lower_expr(v))]),
        StmtKind::Emit(v) => node("emit", sp, vec![("value", lower_expr(v))]),
        StmtKind::Halt(v) => node("halt", sp, vec![("value", lower_expr(v))]),
        StmtKind::Expr(e) => lower_expr(e),
    }
}

// ---- expressions ----

fn opt_expr(e: Option<&Expr>) -> Value {
    e.map(lower_expr).unwrap_or(Value::Null)
}

fn lower_arg(a: &Arg) -> Value {
    node(
        "arg",
        opt_span(&a.value.span),
        vec![
            (
                "label",
                a.label.as_ref().map(|l| json!(l)).unwrap_or(Value::Null),
            ),
            ("value", lower_expr(&a.value)),
        ],
    )
}

fn lower_field(f: &FieldInit) -> Value {
    node(
        "field",
        opt_span(&f.value.span),
        vec![("name", json!(f.name)), ("value", lower_expr(&f.value))],
    )
}

fn lower_expr(e: &Expr) -> Value {
    let sp = opt_span(&e.span);
    match &e.kind {
        ExprKind::Literal(l) => {
            let value = match l {
                Lit::Int(i) => json!(i),
                Lit::Float(f) => json!(f),
                Lit::Str(s) | Lit::RawStr(s) => json!(s),
                Lit::Bool(b) => json!(b),
                Lit::Null => Value::Null,
                Lit::Duration(c, u) => json!(format!("{c}{u}")),
                Lit::Money(s) => json!(s),
            };
            node(
                "lit",
                sp,
                vec![("type", json!(l.ir_type())), ("value", value)],
            )
        }
        ExprKind::InterpString(parts) => {
            let ps: Vec<Value> = parts
                .iter()
                .map(|p| match p {
                    InterpPart::Chunk(s) => json!(s),
                    InterpPart::Expr(x) => lower_expr(x),
                })
                .collect();
            node("interp", sp, vec![("parts", json!(ps))])
        }
        ExprKind::Ident(name) => node("ref", sp, vec![("name", json!(name))]),
        ExprKind::This => node("this", sp, vec![]),
        ExprKind::BinOp { op, left, right } => node(
            "binop",
            sp,
            vec![
                ("op", json!(op)),
                ("left", lower_expr(left)),
                ("right", lower_expr(right)),
            ],
        ),
        ExprKind::UnOp { op, operand } => node(
            "unop",
            sp,
            vec![("op", json!(op)), ("operand", lower_expr(operand))],
        ),
        ExprKind::Member {
            obj,
            name,
            optional,
        } => node(
            "member",
            sp,
            vec![
                ("obj", lower_expr(obj)),
                ("name", json!(name)),
                ("optional", json!(optional)),
            ],
        ),
        ExprKind::Index { obj, index } => node(
            "index",
            sp,
            vec![("obj", lower_expr(obj)), ("index", lower_expr(index))],
        ),
        ExprKind::Call { callee, args } => {
            let a: Vec<Value> = args.iter().map(lower_arg).collect();
            node(
                "call",
                sp,
                vec![("callee", lower_expr(callee)), ("args", json!(a))],
            )
        }
        ExprKind::ListLit(items) => {
            let xs: Vec<Value> = items.iter().map(lower_expr).collect();
            node("list", sp, vec![("items", json!(xs))])
        }
        ExprKind::MapLit(entries) => {
            let es: Vec<Value> = entries
                .iter()
                .map(|(k, v)| {
                    node(
                        "entry",
                        opt_span(&k.span),
                        vec![("key", lower_expr(k)), ("value", lower_expr(v))],
                    )
                })
                .collect();
            node("map", sp, vec![("entries", json!(es))])
        }
        ExprKind::ConfigLit { type_name, fields } => {
            let fs: Vec<Value> = fields.iter().map(lower_field).collect();
            node(
                "configlit",
                sp,
                vec![
                    (
                        "type_name",
                        type_name.as_ref().map(|n| json!(n)).unwrap_or(Value::Null),
                    ),
                    ("fields", json!(fs)),
                ],
            )
        }
        ExprKind::Lambda { params, body } => {
            let ps: Vec<Value> = params.iter().map(lower_param).collect();
            let captures = free_vars(e);
            node(
                "lambda",
                sp,
                vec![
                    ("params", json!(ps)),
                    ("body", lower_expr(body)),
                    ("captures", json!(captures)),
                ],
            )
        }
        ExprKind::Match { subject, arms } => {
            let ams: Vec<Value> = arms
                .iter()
                .map(|arm| {
                    node(
                        "arm",
                        opt_span(&arm.pattern.span),
                        vec![
                            ("pattern", lower_pattern(&arm.pattern)),
                            ("body", lower_expr(&arm.body)),
                        ],
                    )
                })
                .collect();
            node(
                "match",
                sp,
                vec![("subject", lower_expr(subject)), ("arms", json!(ams))],
            )
        }
        ExprKind::Range { lo, hi, inclusive } => node(
            "range",
            sp,
            vec![
                ("lo", lower_expr(lo)),
                ("hi", lower_expr(hi)),
                ("inclusive", json!(inclusive)),
            ],
        ),
        ExprKind::Gen {
            as_type,
            prompt,
            with_config,
        } => node(
            "gen",
            sp,
            vec![
                ("as_type", lower_type(as_type.as_ref())),
                ("prompt", lower_expr(prompt)),
                ("with_config", opt_expr(with_config.as_deref())),
            ],
        ),
        ExprKind::Delegate { goal, with_config } => node(
            "delegate",
            sp,
            vec![
                ("goal", lower_expr(goal)),
                ("with_config", opt_expr(with_config.as_deref())),
            ],
        ),
        ExprKind::Spawn(t) => node("spawn", sp, vec![("target", lower_expr(t))]),
        ExprKind::Await(f) => node("await", sp, vec![("future", lower_expr(f))]),
        ExprKind::Recall { query, one } => node(
            "recall",
            sp,
            vec![("query", lower_expr(query)), ("one", json!(one))],
        ),
        ExprKind::Retry { max, body, until } => node(
            "retry",
            sp,
            vec![
                ("max", lower_expr(max)),
                ("body", lower_block(body)),
                ("until", lower_expr(until)),
            ],
        ),
        ExprKind::Parallel(branches) => {
            let bs: Vec<Value> = branches.iter().map(lower_field).collect();
            node("parallel", sp, vec![("branches", json!(bs))])
        }
        ExprKind::Budget { args, body } => {
            let a: Vec<Value> = args.iter().map(lower_arg).collect();
            node(
                "budget",
                sp,
                vec![("args", json!(a)), ("body", lower_block(body))],
            )
        }
        ExprKind::Block(b) => lower_block(b),
    }
}

fn lower_pattern(p: &Pattern) -> Value {
    let (kind, name, binds, value): (&str, Value, Value, Value) = match &p.kind {
        PatternKind::Wildcard => ("wildcard", Value::Null, json!([]), Value::Null),
        PatternKind::Ident(n) => ("ident", json!(n), json!([]), Value::Null),
        PatternKind::Enum { name, binds } => ("enum", json!(name), json!(binds), Value::Null),
        PatternKind::Literal(e) => ("literal", Value::Null, json!([]), lower_expr(e)),
    };
    node(
        "pattern",
        opt_span(&p.span),
        vec![
            ("kind", json!(kind)),
            ("name", name),
            ("binds", binds),
            ("value", value),
        ],
    )
}

// ---- free variables (lambda captures) ----

fn free_vars(lambda: &Expr) -> Vec<String> {
    let (params, body) = match &lambda.kind {
        ExprKind::Lambda { params, body } => (params, body),
        _ => return vec![],
    };
    let mut used = BTreeSet::new();
    let mut bound = BTreeSet::new();
    for p in params {
        bound.insert(p.name.clone());
    }
    collect_used_bound(body, &mut used, &mut bound);
    used.difference(&bound).cloned().collect()
}

fn collect_used_bound(e: &Expr, used: &mut BTreeSet<String>, bound: &mut BTreeSet<String>) {
    match &e.kind {
        ExprKind::Ident(n) => {
            used.insert(n.clone());
        }
        ExprKind::Lambda { params, body } => {
            for p in params {
                bound.insert(p.name.clone());
            }
            collect_used_bound(body, used, bound);
        }
        ExprKind::Block(b) => collect_block_used_bound(b, used, bound),
        ExprKind::InterpString(parts) => {
            for p in parts {
                if let InterpPart::Expr(x) = p {
                    collect_used_bound(x, used, bound);
                }
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_used_bound(left, used, bound);
            collect_used_bound(right, used, bound);
        }
        ExprKind::UnOp { operand, .. } => collect_used_bound(operand, used, bound),
        ExprKind::Member { obj, .. } => collect_used_bound(obj, used, bound),
        ExprKind::Index { obj, index } => {
            collect_used_bound(obj, used, bound);
            collect_used_bound(index, used, bound);
        }
        ExprKind::Call { callee, args } => {
            collect_used_bound(callee, used, bound);
            for a in args {
                collect_used_bound(&a.value, used, bound);
            }
        }
        ExprKind::ListLit(items) => items
            .iter()
            .for_each(|i| collect_used_bound(i, used, bound)),
        ExprKind::MapLit(entries) => entries.iter().for_each(|(k, v)| {
            collect_used_bound(k, used, bound);
            collect_used_bound(v, used, bound);
        }),
        ExprKind::ConfigLit { fields, .. } => fields
            .iter()
            .for_each(|f| collect_used_bound(&f.value, used, bound)),
        ExprKind::Match { subject, arms } => {
            collect_used_bound(subject, used, bound);
            for arm in arms {
                if let PatternKind::Enum { binds, .. } = &arm.pattern.kind {
                    for b in binds {
                        bound.insert(b.clone());
                    }
                }
                if let PatternKind::Ident(n) = &arm.pattern.kind {
                    bound.insert(n.clone());
                }
                collect_used_bound(&arm.body, used, bound);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            collect_used_bound(lo, used, bound);
            collect_used_bound(hi, used, bound);
        }
        ExprKind::Gen {
            prompt,
            with_config,
            ..
        } => {
            collect_used_bound(prompt, used, bound);
            if let Some(w) = with_config {
                collect_used_bound(w, used, bound);
            }
        }
        ExprKind::Delegate { goal, with_config } => {
            collect_used_bound(goal, used, bound);
            if let Some(w) = with_config {
                collect_used_bound(w, used, bound);
            }
        }
        ExprKind::Spawn(t) | ExprKind::Await(t) => collect_used_bound(t, used, bound),
        ExprKind::Recall { query, .. } => collect_used_bound(query, used, bound),
        ExprKind::Retry { max, body, until } => {
            collect_used_bound(max, used, bound);
            collect_block_used_bound(body, used, bound);
            collect_used_bound(until, used, bound);
        }
        ExprKind::Parallel(branches) => branches
            .iter()
            .for_each(|b| collect_used_bound(&b.value, used, bound)),
        ExprKind::Budget { args, body } => {
            for a in args {
                collect_used_bound(&a.value, used, bound);
            }
            collect_block_used_bound(body, used, bound);
        }
        ExprKind::Literal(_) | ExprKind::This => {}
    }
}

fn collect_block_used_bound(b: &Block, used: &mut BTreeSet<String>, bound: &mut BTreeSet<String>) {
    for s in &b.stmts {
        collect_stmt_used_bound(s, used, bound);
    }
}

fn collect_stmt_used_bound(s: &Stmt, used: &mut BTreeSet<String>, bound: &mut BTreeSet<String>) {
    match &s.kind {
        StmtKind::Bind { name, value, .. } => {
            bound.insert(name.clone());
            collect_used_bound(value, used, bound);
        }
        StmtKind::Assign { target, value, .. } => {
            collect_used_bound(target, used, bound);
            collect_used_bound(value, used, bound);
        }
        StmtKind::If {
            branches,
            else_block,
        } => {
            for (c, b) in branches {
                collect_used_bound(c, used, bound);
                collect_block_used_bound(b, used, bound);
            }
            if let Some(b) = else_block {
                collect_block_used_bound(b, used, bound);
            }
        }
        StmtKind::For { var, iter, body } => {
            bound.insert(var.clone());
            collect_used_bound(iter, used, bound);
            collect_block_used_bound(body, used, bound);
        }
        StmtKind::While { cond, body } => {
            collect_used_bound(cond, used, bound);
            collect_block_used_bound(body, used, bound);
        }
        StmtKind::Repeat { count, body } => {
            collect_used_bound(count, used, bound);
            collect_block_used_bound(body, used, bound);
        }
        StmtKind::Return(Some(v))
        | StmtKind::Throw(v)
        | StmtKind::Forget(v)
        | StmtKind::Reply(v)
        | StmtKind::Emit(v)
        | StmtKind::Halt(v) => collect_used_bound(v, used, bound),
        StmtKind::Try {
            body,
            catch_name,
            catch_block,
        } => {
            collect_block_used_bound(body, used, bound);
            bound.insert(catch_name.clone());
            collect_block_used_bound(catch_block, used, bound);
        }
        StmtKind::Remember { key, value, .. } => {
            if let Some(RememberKey::Expr(x)) = key {
                collect_used_bound(x, used, bound);
            }
            collect_used_bound(value, used, bound);
        }
        StmtKind::Block(b) => collect_block_used_bound(b, used, bound),
        StmtKind::Expr(x) => collect_used_bound(x, used, bound),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
    }
}

// ---- external-effect analysis ----

/// Whether a body statically performs an external effect. With
/// `http_web_only`, bare `shell(...)` is excluded (used for
/// `ingests_external_inline`).
pub fn external_effects(body: &Block, http_web_only: bool) -> bool {
    let mut found = false;
    let mut visit = |e: &Expr| {
        if let ExprKind::Call { callee, .. } = &e.kind {
            match &callee.kind {
                ExprKind::Member { obj, .. } => {
                    if let ExprKind::Ident(n) = &obj.kind {
                        if n == "http" {
                            found = true;
                        }
                    }
                }
                ExprKind::Ident(n) => {
                    if n == "web_search"
                        || n == "fetch_page"
                        || n == "browser_open"
                        || n == "browser_read"
                        || n == "browser_eval"
                    {
                        found = true;
                    }
                    if !http_web_only && n == "shell" {
                        found = true;
                    }
                }
                _ => {}
            }
        }
    };
    walk_block_exprs(body, &mut visit);
    found
}

fn walk_block_exprs(b: &Block, f: &mut impl FnMut(&Expr)) {
    for s in &b.stmts {
        walk_stmt_exprs(s, f);
    }
}

fn walk_stmt_exprs(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    match &s.kind {
        StmtKind::Bind { value, .. } => walk_expr_rec(value, f),
        StmtKind::Assign { target, value, .. } => {
            walk_expr_rec(target, f);
            walk_expr_rec(value, f);
        }
        StmtKind::If {
            branches,
            else_block,
        } => {
            for (c, b) in branches {
                walk_expr_rec(c, f);
                walk_block_exprs(b, f);
            }
            if let Some(b) = else_block {
                walk_block_exprs(b, f);
            }
        }
        StmtKind::For { iter, body, .. } => {
            walk_expr_rec(iter, f);
            walk_block_exprs(body, f);
        }
        StmtKind::While { cond, body } => {
            walk_expr_rec(cond, f);
            walk_block_exprs(body, f);
        }
        StmtKind::Repeat { count, body } => {
            walk_expr_rec(count, f);
            walk_block_exprs(body, f);
        }
        StmtKind::Return(Some(v))
        | StmtKind::Throw(v)
        | StmtKind::Forget(v)
        | StmtKind::Reply(v)
        | StmtKind::Emit(v)
        | StmtKind::Halt(v) => walk_expr_rec(v, f),
        StmtKind::Try {
            body, catch_block, ..
        } => {
            walk_block_exprs(body, f);
            walk_block_exprs(catch_block, f);
        }
        StmtKind::Remember { key, value, .. } => {
            if let Some(RememberKey::Expr(x)) = key {
                walk_expr_rec(x, f);
            }
            walk_expr_rec(value, f);
        }
        StmtKind::Block(b) => walk_block_exprs(b, f),
        StmtKind::Expr(x) => walk_expr_rec(x, f),
        _ => {}
    }
}

fn walk_expr_rec(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::InterpString(parts) => parts.iter().for_each(|p| {
            if let InterpPart::Expr(x) = p {
                walk_expr_rec(x, f);
            }
        }),
        ExprKind::BinOp { left, right, .. } => {
            walk_expr_rec(left, f);
            walk_expr_rec(right, f);
        }
        ExprKind::UnOp { operand, .. } => walk_expr_rec(operand, f),
        ExprKind::Member { obj, .. } => walk_expr_rec(obj, f),
        ExprKind::Index { obj, index } => {
            walk_expr_rec(obj, f);
            walk_expr_rec(index, f);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_rec(callee, f);
            args.iter().for_each(|a| walk_expr_rec(&a.value, f));
        }
        ExprKind::ListLit(items) => items.iter().for_each(|i| walk_expr_rec(i, f)),
        ExprKind::MapLit(entries) => entries.iter().for_each(|(k, v)| {
            walk_expr_rec(k, f);
            walk_expr_rec(v, f);
        }),
        ExprKind::ConfigLit { fields, .. } => {
            fields.iter().for_each(|x| walk_expr_rec(&x.value, f))
        }
        ExprKind::Lambda { body, .. } => walk_expr_rec(body, f),
        ExprKind::Match { subject, arms } => {
            walk_expr_rec(subject, f);
            arms.iter().for_each(|a| walk_expr_rec(&a.body, f));
        }
        ExprKind::Range { lo, hi, .. } => {
            walk_expr_rec(lo, f);
            walk_expr_rec(hi, f);
        }
        ExprKind::Gen {
            prompt,
            with_config,
            ..
        } => {
            walk_expr_rec(prompt, f);
            if let Some(w) = with_config {
                walk_expr_rec(w, f);
            }
        }
        ExprKind::Delegate { goal, with_config } => {
            walk_expr_rec(goal, f);
            if let Some(w) = with_config {
                walk_expr_rec(w, f);
            }
        }
        ExprKind::Spawn(t) | ExprKind::Await(t) => walk_expr_rec(t, f),
        ExprKind::Recall { query, .. } => walk_expr_rec(query, f),
        ExprKind::Retry { max, body, until } => {
            walk_expr_rec(max, f);
            walk_block_exprs(body, f);
            walk_expr_rec(until, f);
        }
        ExprKind::Parallel(branches) => branches.iter().for_each(|b| walk_expr_rec(&b.value, f)),
        ExprKind::Budget { args, body } => {
            args.iter().for_each(|a| walk_expr_rec(&a.value, f));
            walk_block_exprs(body, f);
        }
        ExprKind::Block(b) => walk_block_exprs(b, f),
        ExprKind::Literal(_) | ExprKind::This | ExprKind::Ident(_) => {}
    }
}
