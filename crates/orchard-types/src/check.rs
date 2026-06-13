//! The Orchard 3.0 static checker (`orch check`). Ports v2's `check.py`:
//! name resolution, local type checks at boundaries, `fn`/`tool` purity,
//! exhaustive `match`, tighten-only `budget`, secret-taint lint, config
//! validity, duplicate/name-rule, and `use` validity.
//!
//! Deviation from v2: concurrency (`spawn`/`await`/`parallel`) is NOT rejected;
//! instead a few concurrency-specific checks are added (parallel label
//! uniqueness; deeper escape analysis lands with the runtime in P9).

use crate::types::*;
use orchard_syntax::ast::*;
use orchard_syntax::{parse_source, suggest, Diagnostic, Span};
use std::collections::{BTreeMap, HashMap, HashSet};

const PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "ollama",
    "groq",
    "together",
    "openrouter",
    "mock",
];
const PACKS: &[&str] = &[
    "http",
    "files",
    "shell",
    "calculator",
    "time",
    "web",
    "memory",
];
const ALWAYS_BUILTINS: &[&str] = &["env", "recall_one", "http", "shell", "state", "this"];
const ALLOW_SHELL_VALUES: &[&str] = &["never", "ask", "always"];
const ON_VIOLATION_VALUES: &[&str] = &["stop", "ask"];

fn pack_tools(pack: &str) -> &'static [&'static str] {
    match pack {
        "http" => &["http_request"],
        "files" => &[
            "read_file",
            "write_file",
            "append_file",
            "list_dir",
            "file_info",
        ],
        "shell" => &["run_command"],
        "calculator" => &["calculate"],
        "time" => &["current_time"],
        "web" => &["web_search", "fetch_page"],
        "memory" => &["remember", "recall", "forget"],
        _ => &[],
    }
}

fn name_re_ok(name: &str) -> bool {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Parse + check; returns one diagnostic on a syntax error, else all check
/// diagnostics (errors + warnings) sorted by position.
pub fn check_source(source: &str, filename: &str) -> Vec<Diagnostic> {
    match parse_source(source, filename) {
        Ok(program) => check(&program),
        Err(e) => vec![e.diagnostic],
    }
}

/// Check a parsed program.
pub fn check(program: &Program) -> Vec<Diagnostic> {
    let mut c = Checker::new();
    c.run(program);
    c.diags.sort_by_key(|d| {
        d.span
            .as_ref()
            .map(|s| (s.line, s.col))
            .unwrap_or((1_000_000, 0))
    });
    c.diags
}

#[derive(Clone, Copy, PartialEq)]
enum BKind {
    Let,
    Var,
    Param,
    Loop,
    Catch,
    Match,
    Lambda,
    State,
    Tool,
    Skill,
    Fn,
    Type,
    Variant,
    Import,
    Builtin,
}

struct Binding {
    kind: BKind,
    ty: Option<Type>,
    span: Span,
    used: bool,
    tainted: bool,
}

#[derive(Clone)]
enum ConfigSpec {
    Scalar,
    Block(BTreeMap<&'static str, ConfigSpec>),
}

struct Checker {
    diags: Vec<Diagnostic>,
    scopes: Vec<HashMap<String, Binding>>,
    // type environment
    enums: HashMap<String, EnumType>,
    records: HashMap<String, RecordType>,
    type_names: HashSet<String>,
    variant_to_enum: HashMap<String, String>,
    callables: HashMap<String, Option<TypeRef>>,
    skill_names: HashSet<String>,
    import_names: HashSet<String>,
    state_names: HashSet<String>,
    pack_tool_names: HashSet<String>,
    /// MCP namespaces (from `use mcp(...) as ns`). Tool names are only known at
    /// runtime, so `ns_<tool>` calls are accepted dynamically (return `json`).
    mcp_namespaces: HashSet<String>,
    // budget caps
    policy_spend: Option<f64>,
    policy_steps: Option<i64>,
    policy_calls: Option<i64>,
    cur_spend: Option<f64>,
    cur_steps: Option<i64>,
    cur_calls: Option<i64>,
    in_retry_until: bool,
}

impl Checker {
    fn new() -> Self {
        Checker {
            diags: Vec::new(),
            scopes: Vec::new(),
            enums: HashMap::new(),
            records: HashMap::new(),
            type_names: HashSet::new(),
            variant_to_enum: HashMap::new(),
            callables: HashMap::new(),
            skill_names: HashSet::new(),
            import_names: HashSet::new(),
            state_names: HashSet::new(),
            pack_tool_names: HashSet::new(),
            mcp_namespaces: HashSet::new(),
            policy_spend: None,
            policy_steps: None,
            policy_calls: None,
            cur_spend: None,
            cur_steps: None,
            cur_calls: None,
            in_retry_until: false,
        }
    }

    fn err(&mut self, msg: impl Into<String>, span: &Span) {
        self.diags.push(Diagnostic::error(msg, Some(span.clone())));
    }
    fn err_hint(&mut self, msg: impl Into<String>, span: &Span, hint: String) {
        self.diags
            .push(Diagnostic::error(msg, Some(span.clone())).with_hint(hint));
    }
    fn warn(&mut self, msg: impl Into<String>, span: &Span) {
        self.diags
            .push(Diagnostic::warning(msg, Some(span.clone())));
    }

    // ---- top level ----

    fn run(&mut self, program: &Program) {
        // Register top-level types/enums first (shared).
        let mut top_types: Vec<&TypeDecl> = Vec::new();
        let mut top_enums: Vec<&EnumDecl> = Vec::new();
        let mut top_fns: Vec<&Callable> = Vec::new();
        let mut top_uses: Vec<&UseDecl> = Vec::new();
        let mut agents: Vec<&AgentDecl> = Vec::new();
        for item in &program.items {
            match item {
                TopItem::Type(t) => top_types.push(t),
                TopItem::Enum(e) => top_enums.push(e),
                TopItem::Fn(f) => top_fns.push(f),
                TopItem::Use(u) => top_uses.push(u),
                TopItem::Agent(a) => agents.push(a),
            }
        }

        // Duplicate top-level fn names.
        let mut seen_fn = HashSet::new();
        for f in &top_fns {
            if !seen_fn.insert(f.name.clone()) {
                self.err(format!("duplicate name '{}' at top level", f.name), &f.span);
            }
        }

        if agents.is_empty() {
            // Shared unit only.
            self.check_duplicate_types(&top_types, &top_enums);
            self.check_unit(&top_types, &top_enums, &top_fns, &top_uses, &[], &[], &[]);
        } else {
            for a in &agents {
                self.check_agent(a, &top_types, &top_enums, &top_fns, &top_uses);
            }
        }
    }

    /// Error on a type/enum name declared more than once (across both).
    fn check_duplicate_types(&mut self, types: &[&TypeDecl], enums: &[&EnumDecl]) {
        let mut seen: HashSet<String> = HashSet::new();
        for t in types {
            if !seen.insert(t.name.clone()) {
                self.err(format!("duplicate type/enum name '{}'", t.name), &t.span);
            }
        }
        for e in enums {
            if !seen.insert(e.name.clone()) {
                self.err(format!("duplicate type/enum name '{}'", e.name), &e.span);
            }
        }
    }

    fn check_agent(
        &mut self,
        agent: &AgentDecl,
        top_types: &[&TypeDecl],
        top_enums: &[&EnumDecl],
        top_fns: &[&Callable],
        top_uses: &[&UseDecl],
    ) {
        let mut types: Vec<&TypeDecl> = top_types.to_vec();
        let mut enums: Vec<&EnumDecl> = top_enums.to_vec();
        let mut fns: Vec<&Callable> = top_fns.to_vec();
        let mut uses: Vec<&UseDecl> = top_uses.to_vec();
        let mut tools: Vec<&Callable> = Vec::new();
        let mut skills: Vec<&Callable> = Vec::new();
        let mut states: Vec<&StateDecl> = Vec::new();
        let mut configs: Vec<&ConfigBlock> = Vec::new();
        let mut handlers: Vec<&OnDecl> = Vec::new();
        for m in &agent.members {
            match m {
                AgentMember::Type(t) => types.push(t),
                AgentMember::Enum(e) => enums.push(e),
                AgentMember::Fn(f) => fns.push(f),
                AgentMember::Tool(t) => tools.push(t),
                AgentMember::Skill(s) => skills.push(s),
                AgentMember::State(s) => states.push(s),
                AgentMember::Use(u) => uses.push(u),
                AgentMember::Config(cb) => configs.push(cb),
                AgentMember::On(o) => handlers.push(o),
            }
        }
        self.check_duplicate_types(&types, &enums);
        self.check_config_blocks(&configs);
        self.check_unit(&types, &enums, &fns, &uses, &tools, &skills, &states);
        self.check_states(&states);
        self.check_handlers(&handlers, &states, &fns);
    }

    /// The core checking unit: build the type/name context, validate types,
    /// resolve `use`s, then walk every body.
    #[allow(clippy::too_many_arguments)]
    fn check_unit(
        &mut self,
        types: &[&TypeDecl],
        enums: &[&EnumDecl],
        fns: &[&Callable],
        uses: &[&UseDecl],
        tools: &[&Callable],
        skills: &[&Callable],
        states: &[&StateDecl],
    ) {
        self.set_context(types, enums, fns, uses, tools, skills, states);
        self.check_duplicate_names(fns, tools, skills, states);
        // Validate declared types in signatures/fields.
        for t in types {
            for f in &t.fields {
                self.validate_type(&f.ty);
            }
        }
        for e in enums {
            for v in &e.variants {
                for p in &v.params {
                    if let Some(ty) = &p.ty {
                        self.validate_type(ty);
                    }
                }
            }
        }
        self.check_uses(uses);
        // Walk bodies.
        for f in fns {
            self.check_body(f, Some(Purity::Fn));
        }
        for t in tools {
            self.check_body(t, Some(Purity::Tool));
        }
        for s in skills {
            self.check_body(s, None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn set_context(
        &mut self,
        types: &[&TypeDecl],
        enums: &[&EnumDecl],
        fns: &[&Callable],
        uses: &[&UseDecl],
        tools: &[&Callable],
        skills: &[&Callable],
        states: &[&StateDecl],
    ) {
        self.enums.clear();
        self.records.clear();
        self.type_names.clear();
        self.variant_to_enum.clear();
        self.callables.clear();
        self.skill_names.clear();
        self.import_names.clear();
        self.state_names.clear();
        self.pack_tool_names.clear();
        self.mcp_namespaces.clear();

        for t in types {
            self.type_names.insert(t.name.clone());
            let fields = t
                .fields
                .iter()
                .map(|f| {
                    let required = !f.ty.optional && f.default.is_none();
                    (f.name.clone(), from_typeref(Some(&f.ty)), required)
                })
                .collect();
            self.records.insert(
                t.name.clone(),
                RecordType {
                    name: t.name.clone(),
                    fields,
                },
            );
        }
        for e in enums {
            self.type_names.insert(e.name.clone());
            let variants: Vec<(String, Vec<Type>)> = e
                .variants
                .iter()
                .map(|v| {
                    (
                        v.name.clone(),
                        v.params
                            .iter()
                            .map(|p| from_typeref(p.ty.as_ref()))
                            .collect(),
                    )
                })
                .collect();
            for (vn, _) in &variants {
                self.variant_to_enum.insert(vn.clone(), e.name.clone());
            }
            self.enums.insert(
                e.name.clone(),
                EnumType {
                    name: e.name.clone(),
                    variants,
                },
            );
        }
        for f in fns {
            self.callables.insert(f.name.clone(), f.return_type.clone());
        }
        for t in tools {
            self.callables.insert(t.name.clone(), t.return_type.clone());
        }
        for s in skills {
            self.callables.insert(s.name.clone(), s.return_type.clone());
            self.skill_names.insert(s.name.clone());
        }
        for s in states {
            self.state_names.insert(s.name.clone());
        }
        for u in uses {
            match u.form {
                UseForm::Pack => {
                    for t in pack_tools(&u.name) {
                        self.pack_tool_names.insert((*t).to_string());
                    }
                }
                UseForm::Mcp => {
                    self.import_names.insert(u.name.clone());
                    self.mcp_namespaces.insert(u.name.clone());
                }
                UseForm::Import => {
                    self.import_names.insert(u.name.clone());
                }
                UseForm::Env => {}
            }
        }
    }

    fn resolve_named(&self, t: Option<Type>) -> Option<Type> {
        match t {
            Some(Type::Named(n)) => {
                if let Some(e) = self.enums.get(&n) {
                    Some(Type::Enum(e.clone()))
                } else if let Some(r) = self.records.get(&n) {
                    Some(Type::Record(r.clone()))
                } else {
                    Some(Type::Named(n))
                }
            }
            other => other,
        }
    }

    // ---- scopes ----

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        if let Some(scope) = self.scopes.pop() {
            for (name, b) in &scope {
                if !b.used && matches!(b.kind, BKind::Let | BKind::Var) {
                    self.diags.push(Diagnostic::warning(
                        format!("unused binding '{name}'"),
                        Some(b.span.clone()),
                    ));
                }
            }
        }
    }

    fn declare(
        &mut self,
        name: &str,
        kind: BKind,
        ty: Option<Type>,
        span: &Span,
        used: bool,
        tainted: bool,
    ) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(
                name.to_string(),
                Binding {
                    kind,
                    ty,
                    span: span.clone(),
                    used,
                    tainted,
                },
            );
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(b);
            }
        }
        None
    }

    /// Is `name` a `ns_<tool>` call for a declared MCP namespace? Such tools are
    /// discovered at runtime, so they can't be validated statically.
    fn is_mcp_tool(&self, name: &str) -> bool {
        self.mcp_namespaces.iter().any(|ns| {
            name.len() > ns.len() + 1
                && name.starts_with(ns.as_str())
                && name.as_bytes()[ns.len()] == b'_'
        })
    }

    fn mark_used(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(b) = scope.get_mut(name) {
                b.used = true;
                return;
            }
        }
    }

    fn all_names(&self) -> Vec<String> {
        let mut names: HashSet<String> = HashSet::new();
        for scope in &self.scopes {
            for k in scope.keys() {
                names.insert(k.clone());
            }
        }
        let mut v: Vec<String> = names.into_iter().collect();
        v.sort();
        v
    }

    fn base_scope(&mut self) {
        self.push_scope();
        let dummy = Span::point("", 0, 0, 0);
        for b in ALWAYS_BUILTINS {
            self.declare(b, BKind::Builtin, None, &dummy, true, false);
        }
        let pack_tools: Vec<String> = self.pack_tool_names.iter().cloned().collect();
        for t in pack_tools {
            self.declare(&t, BKind::Tool, None, &dummy, true, false);
        }
        let type_names: Vec<String> = self.type_names.iter().cloned().collect();
        for t in type_names {
            self.declare(&t, BKind::Type, None, &dummy, true, false);
        }
        let variants: Vec<(String, String)> = self
            .variant_to_enum
            .iter()
            .map(|(v, e)| (v.clone(), e.clone()))
            .collect();
        for (v, e) in variants {
            self.declare(
                &v,
                BKind::Variant,
                Some(Type::Named(e)),
                &dummy,
                true,
                false,
            );
        }
        let callables: Vec<String> = self.callables.keys().cloned().collect();
        for n in callables {
            let kind = if self.skill_names.contains(&n) {
                BKind::Skill
            } else {
                BKind::Fn
            };
            self.declare(&n, kind, None, &dummy, true, false);
        }
        let imports: Vec<String> = self.import_names.iter().cloned().collect();
        for n in imports {
            self.declare(&n, BKind::Import, None, &dummy, true, false);
        }
        let states: Vec<String> = self.state_names.iter().cloned().collect();
        for s in states {
            self.declare(&s, BKind::State, None, &dummy, true, false);
        }
    }

    // ---- bodies ----

    fn check_body(&mut self, c: &Callable, purity: Option<Purity>) {
        match purity {
            Some(Purity::Fn) => self.check_fn_purity(c),
            Some(Purity::Tool) => self.check_tool_purity(c),
            None => {}
        }
        self.reset_caps();
        self.base_scope();
        self.push_scope();
        for p in &c.params {
            let ty = p.ty.as_ref().map(|t| from_typeref(Some(t)));
            self.declare(&p.name, BKind::Param, ty, &p.span, true, false);
            if let Some(t) = &p.ty {
                self.validate_type(t);
            }
        }
        if let Some(rt) = &c.return_type {
            self.validate_type(rt);
        }
        self.resolve_block(&c.body);
        self.pop_scope();
        self.pop_scope();
    }

    fn check_states(&mut self, states: &[&StateDecl]) {
        for s in states {
            self.validate_type(&s.ty);
            if let Some(default) = &s.default {
                let declared = from_typeref(Some(&s.ty));
                let actual = self.infer(default);
                if let Some(actual) = actual {
                    if !assignable(&actual, &declared) {
                        let msg = format!(
                            "state '{}' default has type {} but the field is declared {}",
                            s.name,
                            actual.display(),
                            declared.display()
                        );
                        self.err(msg, &s.span);
                    }
                }
            }
        }
    }

    fn check_handlers(&mut self, handlers: &[&OnDecl], _states: &[&StateDecl], _fns: &[&Callable]) {
        let mut seen: HashMap<String, ()> = HashMap::new();
        for h in handlers {
            let kind = match h.kind {
                HandlerKind::Start => "start",
                HandlerKind::Message => "message",
                HandlerKind::Schedule => "schedule",
                HandlerKind::File => "file",
            };
            if seen.insert(kind.to_string(), ()).is_some() {
                self.err(
                    format!("duplicate '{kind}' handler (exactly one handler per kind is allowed)"),
                    &h.span,
                );
            }
            self.reset_caps();
            self.base_scope();
            self.push_scope();
            if let Some(p) = &h.param {
                let ty = p.ty.as_ref().map(|t| from_typeref(Some(t)));
                self.declare(&p.name, BKind::Param, ty, &p.span, true, false);
                if let Some(t) = &p.ty {
                    self.validate_type(t);
                }
            }
            if let Some(rt) = &h.return_type {
                self.validate_type(rt);
            }
            self.resolve_block(&h.body);
            self.pop_scope();
            self.pop_scope();
        }
    }

    fn reset_caps(&mut self) {
        self.cur_spend = self.policy_spend;
        self.cur_steps = self.policy_steps;
        self.cur_calls = self.policy_calls;
    }

    // ---- statements ----

    fn resolve_block(&mut self, block: &Block) {
        self.push_scope();
        let mut terminated: Option<&'static str> = None;
        for stmt in &block.stmts {
            if let Some(kw) = terminated {
                self.err(
                    format!("unreachable code after '{kw}' (SPEC §9.9)"),
                    &stmt.span,
                );
                break;
            }
            self.resolve_stmt(stmt);
            terminated = term_kw(&stmt.kind);
        }
        self.pop_scope();
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Block(b) => self.resolve_block(b),
            StmtKind::Bind {
                name,
                ty,
                value,
                mutable,
            } => {
                self.resolve_expr(value);
                if let Some(t) = ty {
                    self.validate_type(t);
                }
                let declared = ty.as_ref().map(|t| from_typeref(Some(t)));
                let inferred = self.infer(value);
                if let (Some(d), Some(i)) = (&declared, &inferred) {
                    if !assignable(i, d) {
                        let msg = format!(
                            "binding '{}' has type {} but is annotated {}",
                            name,
                            i.display(),
                            d.display()
                        );
                        self.err(msg, &stmt.span);
                    }
                }
                let kind = if *mutable { BKind::Var } else { BKind::Let };
                let tainted = self.tainted(value);
                let bty = declared.or(inferred);
                self.declare(name, kind, bty, &stmt.span, false, tainted);
            }
            StmtKind::Assign { target, op, value } => {
                self.resolve_expr(value);
                if let ExprKind::Ident(name) = &target.kind {
                    match self.lookup(name) {
                        None => {
                            // undefined unless it's a state field
                            if !self.state_names.contains(name) {
                                let hint = suggest(name, self.all_names());
                                self.err_hint(
                                    format!("undefined name '{name}'"),
                                    &target.span,
                                    hint,
                                );
                            }
                        }
                        Some(b) => {
                            if b.kind == BKind::Let {
                                self.err(
                                    format!("cannot reassign immutable binding '{name}' (declare it with 'var')"),
                                    &target.span,
                                );
                            }
                        }
                    }
                    if op != "=" {
                        self.mark_used(name);
                    }
                    let t = self.tainted(value);
                    if let Some(b) = self.lookup_mut(name) {
                        b.tainted |= t;
                    }
                } else {
                    self.resolve_expr(target);
                }
            }
            StmtKind::If {
                branches,
                else_block,
            } => {
                for (cond, body) in branches {
                    self.resolve_expr(cond);
                    self.resolve_block(body);
                }
                if let Some(eb) = else_block {
                    self.resolve_block(eb);
                }
            }
            StmtKind::For { var, iter, body } => {
                self.resolve_expr(iter);
                let elem = self.iter_elem(iter);
                self.push_scope();
                self.declare(var, BKind::Loop, elem, &stmt.span, true, false);
                self.resolve_block(body);
                self.pop_scope();
            }
            StmtKind::While { cond, body } => {
                self.resolve_expr(cond);
                self.resolve_block(body);
            }
            StmtKind::Repeat { count, body } => {
                self.resolve_expr(count);
                self.resolve_block(body);
            }
            StmtKind::Return(v) => {
                if let Some(v) = v {
                    self.resolve_expr(v);
                    self.taint_sink(v, "return value");
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Try {
                body,
                catch_name,
                catch_block,
            } => {
                self.resolve_block(body);
                self.push_scope();
                let dummy = Span::point("", 0, 0, 0);
                self.declare(catch_name, BKind::Catch, None, &dummy, true, false);
                self.resolve_block(catch_block);
                self.pop_scope();
            }
            StmtKind::Throw(v) => self.resolve_expr(v),
            StmtKind::Remember { key, value, .. } => {
                if let Some(RememberKey::Expr(e)) = key {
                    self.resolve_expr(e);
                }
                self.resolve_expr(value);
            }
            StmtKind::Forget(v) => self.resolve_expr(v),
            StmtKind::Reply(v) => {
                self.resolve_expr(v);
                self.taint_sink(v, "reply");
            }
            StmtKind::Emit(v) => {
                self.resolve_expr(v);
                self.taint_sink(v, "emit");
            }
            StmtKind::Halt(v) => self.resolve_expr(v),
            StmtKind::Expr(e) => self.resolve_expr(e),
        }
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Binding> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(b) = scope.get_mut(name) {
                return Some(b);
            }
        }
        None
    }

    // ---- expressions: resolution ----

    fn resolve_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Literal(_) => {}
            ExprKind::InterpString(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.resolve_expr(x);
                    }
                }
            }
            ExprKind::Ident(name) => {
                if self.lookup(name).is_some() {
                    self.mark_used(name);
                } else if !self.is_mcp_tool(name) {
                    let hint = suggest(name, self.all_names());
                    self.err_hint(format!("undefined name '{name}'"), &e.span, hint);
                }
            }
            ExprKind::This => {}
            ExprKind::BinOp { left, right, .. } => {
                self.resolve_expr(left);
                self.resolve_expr(right);
            }
            ExprKind::UnOp { operand, .. } => self.resolve_expr(operand),
            ExprKind::Member { obj, .. } => self.resolve_expr(obj),
            ExprKind::Index { obj, index } => {
                self.resolve_expr(obj);
                self.resolve_expr(index);
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident(name) = &callee.kind {
                    if self.lookup(name).is_some() {
                        self.mark_used(name);
                    } else if !self.is_mcp_tool(name) {
                        let hint = suggest(name, self.all_names());
                        self.err_hint(format!("undefined name '{name}'"), &callee.span, hint);
                    }
                } else {
                    self.resolve_expr(callee);
                }
                for a in args {
                    self.resolve_expr(&a.value);
                }
            }
            ExprKind::ListLit(items) => {
                for i in items {
                    self.resolve_expr(i);
                }
            }
            ExprKind::MapLit(entries) => {
                for (k, v) in entries {
                    self.resolve_expr(k);
                    self.resolve_expr(v);
                }
            }
            ExprKind::ConfigLit { type_name, fields } => {
                if type_name.is_some() {
                    self.check_record_lit(e);
                }
                for f in fields {
                    self.resolve_expr(&f.value);
                }
            }
            ExprKind::Lambda { params, body } => {
                self.push_scope();
                for p in params {
                    self.declare(&p.name, BKind::Lambda, None, &p.span, true, false);
                }
                self.resolve_expr(body);
                self.pop_scope();
            }
            ExprKind::Match { .. } => self.resolve_match(e),
            ExprKind::Range { lo, hi, .. } => {
                self.resolve_expr(lo);
                self.resolve_expr(hi);
            }
            ExprKind::Gen {
                as_type,
                prompt,
                with_config,
            } => {
                if let Some(t) = as_type {
                    self.validate_type(t);
                }
                self.resolve_expr(prompt);
                self.taint_sink(prompt, "gen prompt");
                if let Some(w) = with_config {
                    self.resolve_expr(w);
                }
            }
            ExprKind::Delegate { goal, with_config } => {
                self.resolve_expr(goal);
                if let Some(w) = with_config {
                    self.resolve_expr(w);
                }
            }
            ExprKind::Spawn(t) => self.resolve_expr(t),
            ExprKind::Await(f) => self.resolve_expr(f),
            ExprKind::Recall { query, .. } => self.resolve_expr(query),
            ExprKind::Retry { max, body, until } => {
                self.resolve_expr(max);
                let saved = self.in_retry_until;
                self.in_retry_until = true;
                self.resolve_block(body);
                self.in_retry_until = saved;
                // `until` predicate gets `attempts: int` in scope.
                self.push_scope();
                let dummy = Span::point("", 0, 0, 0);
                self.declare(
                    "attempts",
                    BKind::Let,
                    Some(Type::int()),
                    &dummy,
                    true,
                    false,
                );
                self.resolve_expr(until);
                self.pop_scope();
            }
            ExprKind::Parallel(branches) => {
                let mut seen = HashSet::new();
                for b in branches {
                    if !seen.insert(b.name.clone()) {
                        self.err(
                            format!("duplicate parallel branch label '{}'", b.name),
                            &e.span,
                        );
                    }
                    self.resolve_expr(&b.value);
                }
            }
            ExprKind::Budget { args, body } => self.resolve_budget(args, body),
            ExprKind::Block(b) => self.resolve_block(b),
        }
    }

    fn resolve_budget(&mut self, args: &[Arg], body: &Block) {
        for a in args {
            self.resolve_expr(&a.value);
        }
        let spend = money_arg(args, "spend");
        let steps = int_arg(args, "steps");
        let calls = int_arg(args, "tool_calls");
        if let (Some(s), Some(c)) = (spend, self.cur_spend) {
            if s > c + 1e-9 {
                self.err(
                    format!(
                        "budget spend ${s:.2} is looser than the enclosing limit ${c:.2} — a budget may only tighten (SPEC §9.7)"
                    ),
                    &body.span,
                );
            }
        }
        if let (Some(s), Some(c)) = (steps, self.cur_steps) {
            if s > c {
                self.err(
                    format!("budget steps {s} is looser than the enclosing limit {c} — a budget may only tighten (SPEC §9.7)"),
                    &body.span,
                );
            }
        }
        if let (Some(s), Some(c)) = (calls, self.cur_calls) {
            if s > c {
                self.err(
                    format!("budget tool_calls {s} is looser than the enclosing limit {c} — a budget may only tighten (SPEC §9.7)"),
                    &body.span,
                );
            }
        }
        let (ss, st, sc) = (self.cur_spend, self.cur_steps, self.cur_calls);
        if let Some(s) = spend {
            self.cur_spend = Some(self.cur_spend.map_or(s, |c| c.min(s)));
        }
        if let Some(s) = steps {
            self.cur_steps = Some(self.cur_steps.map_or(s, |c| c.min(s)));
        }
        if let Some(s) = calls {
            self.cur_calls = Some(self.cur_calls.map_or(s, |c| c.min(s)));
        }
        self.resolve_block(body);
        self.cur_spend = ss;
        self.cur_steps = st;
        self.cur_calls = sc;
    }

    fn resolve_match(&mut self, e: &Expr) {
        let (subject, arms) = match &e.kind {
            ExprKind::Match { subject, arms } => (subject, arms),
            _ => return,
        };
        self.resolve_expr(subject);
        let subj = self.resolve_named(self.infer(subject));
        let is_enum = matches!(subj, Some(Type::Enum(_)));
        let variant_names: Vec<String> = match &subj {
            Some(Type::Enum(et)) => et.variants.iter().map(|(n, _)| n.clone()).collect(),
            _ => Vec::new(),
        };
        let mut covered: HashSet<String> = HashSet::new();
        let mut has_catchall = false;
        for arm in arms {
            self.push_scope();
            match &arm.pattern.kind {
                PatternKind::Wildcard => has_catchall = true,
                PatternKind::Ident(name) => {
                    if is_enum && variant_names.contains(name) {
                        covered.insert(name.clone());
                    } else {
                        has_catchall = true;
                        let dummy = Span::point("", 0, 0, 0);
                        self.declare(name, BKind::Match, None, &dummy, true, false);
                    }
                }
                PatternKind::Enum { name, binds } => {
                    if is_enum {
                        let arity = match &subj {
                            Some(Type::Enum(et)) => et
                                .variants
                                .iter()
                                .find(|(n, _)| n == name)
                                .map(|(_, p)| p.len()),
                            _ => None,
                        };
                        match arity {
                            None => {
                                let hint = suggest(name, variant_names.clone());
                                self.err_hint(
                                    format!(
                                        "enum '{}' has no variant '{}'",
                                        subj.as_ref().map(|t| t.display()).unwrap_or_default(),
                                        name
                                    ),
                                    &arm.pattern.span,
                                    hint,
                                );
                            }
                            Some(a) => {
                                covered.insert(name.clone());
                                if binds.len() != a {
                                    self.err(
                                        format!(
                                            "variant '{}' carries {} value(s) but the pattern binds {}",
                                            name,
                                            a,
                                            binds.len()
                                        ),
                                        &arm.pattern.span,
                                    );
                                }
                            }
                        }
                    }
                    let dummy = Span::point("", 0, 0, 0);
                    for b in binds {
                        self.declare(b, BKind::Match, None, &dummy, true, false);
                    }
                }
                PatternKind::Literal(lit) => self.resolve_expr(lit),
            }
            self.resolve_expr(&arm.body);
            self.pop_scope();
        }
        if is_enum && !has_catchall {
            let missing: Vec<String> = variant_names
                .iter()
                .filter(|v| !covered.contains(*v))
                .cloned()
                .collect();
            if !missing.is_empty() {
                let list: Vec<String> = missing.iter().map(|m| format!("'{m}'")).collect();
                self.err(
                    format!(
                        "non-exhaustive match: missing variant(s) {} (add the arms or a '_' wildcard)",
                        list.join(", ")
                    ),
                    &e.span,
                );
            }
        }
    }

    // ---- purity ----

    fn check_fn_purity(&mut self, c: &Callable) {
        let mut found: Vec<(String, Span)> = Vec::new();
        walk_exprs(&c.body, &mut |e| {
            let label = match &e.kind {
                ExprKind::Gen { .. } => Some("gen"),
                ExprKind::Delegate { .. } => Some("delegate"),
                ExprKind::Recall { .. } => Some("recall"),
                _ => None,
            };
            if let Some(l) = label {
                found.push((l.to_string(), e.span.clone()));
            }
        });
        walk_stmts(&c.body, &mut |s| match &s.kind {
            StmtKind::Remember { .. } | StmtKind::Forget(_) => {
                found.push(("memory".to_string(), s.span.clone()))
            }
            _ => {}
        });
        // effect calls and state writes
        let state_names = self.state_names.clone();
        let pack = self.pack_tool_names.clone();
        let callables = self.callables.clone();
        walk_exprs(&c.body, &mut |e| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                if let ExprKind::Ident(n) = &callee.kind {
                    if is_effect_call(n, &callables, &pack) {
                        found.push(("tool/skill calls".to_string(), e.span.clone()));
                    }
                }
            }
        });
        walk_stmts(&c.body, &mut |s| {
            if let StmtKind::Assign { target, .. } = &s.kind {
                if is_state_target(target, &state_names) {
                    found.push(("state writes".to_string(), s.span.clone()));
                }
            }
        });
        found.sort_by_key(|(_, sp)| (sp.line, sp.col));
        for (label, span) in found {
            self.err(
                format!(
                    "'{label}' is not allowed in fn '{}': fn bodies must be pure (no gen/delegate/tools/state/memory, SPEC §6.3)",
                    c.name
                ),
                &span,
            );
        }
    }

    fn check_tool_purity(&mut self, c: &Callable) {
        let state_names = self.state_names.clone();
        let mut errs: Vec<(String, Span)> = Vec::new();
        walk_exprs(&c.body, &mut |e| {
            match &e.kind {
            ExprKind::Gen { .. } => errs.push((
                format!("'gen' is not allowed in tool '{}': tool bodies are restricted (no gen/delegate/state, SPEC §6.4)", c.name),
                e.span.clone(),
            )),
            ExprKind::Delegate { .. } => errs.push((
                format!("'delegate' is not allowed in tool '{}': tool bodies are restricted (no gen/delegate/state, SPEC §6.4)", c.name),
                e.span.clone(),
            )),
            _ => {}
        }
        });
        walk_stmts(&c.body, &mut |s| {
            if let StmtKind::Assign { target, .. } = &s.kind {
                if is_state_target(target, &state_names) {
                    errs.push((
                        format!("tool '{}' may not modify state (tool bodies are restricted: no gen/delegate/state, SPEC §6.4)", c.name),
                        s.span.clone(),
                    ));
                }
            }
        });
        errs.sort_by_key(|(_, sp)| (sp.line, sp.col));
        for (msg, span) in errs {
            self.err(msg, &span);
        }
    }

    // ---- taint ----

    fn taint_sink(&mut self, e: &Expr, sink: &str) {
        if self.tainted(e) {
            self.warn(
                format!("a secret-derived value flows into a {sink} (it is redacted at the sink; avoid exposing secrets there, SPEC §5.6)"),
                &e.span,
            );
        }
    }

    fn tainted(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::This => false,
            ExprKind::Ident(name) => self.lookup(name).map(|b| b.tainted).unwrap_or(false),
            ExprKind::Member { obj, .. } => {
                if let ExprKind::Ident(n) = &obj.kind {
                    if n == "env" {
                        return true;
                    }
                }
                self.tainted(obj)
            }
            ExprKind::Index { obj, .. } => self.tainted(obj),
            ExprKind::BinOp { left, right, .. } => self.tainted(left) || self.tainted(right),
            ExprKind::UnOp { operand, .. } => self.tainted(operand),
            ExprKind::InterpString(parts) => parts.iter().any(|p| match p {
                InterpPart::Expr(x) => self.tainted(x),
                _ => false,
            }),
            ExprKind::Call { callee, args } => {
                self.tainted(callee) || args.iter().any(|a| self.tainted(&a.value))
            }
            ExprKind::ListLit(items) => items.iter().any(|i| self.tainted(i)),
            ExprKind::MapLit(entries) => entries.iter().any(|(_, v)| self.tainted(v)),
            ExprKind::ConfigLit { fields, .. } => fields.iter().any(|f| self.tainted(&f.value)),
            _ => false,
        }
    }

    // ---- inference ----

    fn infer(&self, e: &Expr) -> Option<Type> {
        match &e.kind {
            ExprKind::Literal(l) => Some(match l {
                Lit::Int(_) => Type::int(),
                Lit::Float(_) => Type::float(),
                Lit::Str(_) | Lit::RawStr(_) => Type::str_(),
                Lit::Bool(_) => Type::bool_(),
                Lit::Null => Type::null(),
                Lit::Duration(_, _) => Type::duration(),
                Lit::Money(_) => Type::money(),
            }),
            ExprKind::InterpString(_) => Some(Type::str_()),
            ExprKind::Ident(name) => self.lookup(name).and_then(|b| b.ty.clone()),
            ExprKind::Member { obj, name, .. } => {
                let base = self.resolve_named(self.infer(obj));
                match base {
                    Some(Type::Record(r)) => r
                        .fields
                        .iter()
                        .find(|(n, _, _)| n == name)
                        .map(|(_, t, _)| t.clone()),
                    Some(t) if t.is_dynamic() => Some(Type::any()),
                    _ => None,
                }
            }
            ExprKind::Index { obj, .. } => match self.resolve_named(self.infer(obj)) {
                Some(Type::List(e)) => Some(*e),
                Some(Type::Map(_, v)) => Some(*v),
                _ => None,
            },
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Ident(name) = &callee.kind {
                    if let Some(ret) = self.callables.get(name) {
                        return Some(from_typeref(ret.as_ref()));
                    }
                }
                None
            }
            ExprKind::Gen { as_type, .. } => Some(
                as_type
                    .as_ref()
                    .map(|t| from_typeref(Some(t)))
                    .unwrap_or_else(Type::str_),
            ),
            ExprKind::Delegate { .. } => Some(Type::str_()),
            ExprKind::Recall { one, .. } => {
                if *one {
                    Some(Type::Optional(Box::new(Type::str_())))
                } else {
                    Some(Type::Map(Box::new(Type::str_()), Box::new(Type::str_())))
                }
            }
            ExprKind::ListLit(items) => {
                let mut elem: Option<Type> = None;
                for i in items {
                    elem = unify(elem, self.infer(i));
                }
                Some(Type::List(Box::new(elem.unwrap_or_else(Type::any))))
            }
            ExprKind::MapLit(entries) => {
                let mut val: Option<Type> = None;
                for (_, v) in entries {
                    val = unify(val, self.infer(v));
                }
                Some(Type::Map(
                    Box::new(Type::str_()),
                    Box::new(val.unwrap_or_else(Type::any)),
                ))
            }
            ExprKind::ConfigLit { type_name, .. } => type_name.as_ref().map(|n| {
                self.records
                    .get(n)
                    .map(|r| Type::Record(r.clone()))
                    .unwrap_or_else(|| Type::Named(n.clone()))
            }),
            ExprKind::BinOp { op, left, right } if op == "??" => {
                unify(self.infer(left), self.infer(right))
            }
            ExprKind::Await(_) => None,
            _ => None,
        }
    }

    fn iter_elem(&self, iter: &Expr) -> Option<Type> {
        if let ExprKind::Range { .. } = &iter.kind {
            return Some(Type::int());
        }
        match self.resolve_named(self.infer(iter)) {
            Some(Type::List(e)) => Some(*e),
            _ => None,
        }
    }

    // ---- type validation ----

    fn validate_type(&mut self, tr: &TypeRef) {
        if !is_builtin_type_name(&tr.name) && !self.type_names.contains(&tr.name) {
            let mut cands: Vec<String> = self.type_names.iter().cloned().collect();
            cands.sort();
            for p in PRIMITIVE_NAMES {
                cands.push((*p).to_string());
            }
            let hint = suggest(&tr.name, cands);
            self.err_hint(format!("unknown type '{}'", tr.name), &tr.span, hint);
        }
        for a in &tr.args {
            self.validate_type(a);
        }
    }

    // ---- duplicate names ----

    fn check_duplicate_names(
        &mut self,
        fns: &[&Callable],
        tools: &[&Callable],
        skills: &[&Callable],
        states: &[&StateDecl],
    ) {
        let mut seen: HashMap<String, &'static str> = HashMap::new();
        let mut check = |this: &mut Self,
                         name: &str,
                         kind: &'static str,
                         span: &Span,
                         name_rule: bool| {
            if let Some(prev) = seen.get(name) {
                this.err(
                    format!("duplicate name '{name}' (a {prev} with this name already exists in the agent)"),
                    span,
                );
            } else {
                seen.insert(name.to_string(), kind);
            }
            if name_rule && !name_re_ok(name) {
                this.err(
                    format!("{kind} name '{name}' must match [a-z][a-z0-9_]*"),
                    span,
                );
            }
        };
        for f in fns {
            check(self, &f.name, "fn", &f.span, true);
        }
        for t in tools {
            check(self, &t.name, "tool", &t.span, true);
        }
        for s in skills {
            check(self, &s.name, "skill", &s.span, true);
        }
        for s in states {
            check(self, &s.name, "state", &s.span, false);
        }
    }

    // ---- record literals ----

    fn check_record_lit(&mut self, e: &Expr) {
        let (type_name, fields) = match &e.kind {
            ExprKind::ConfigLit {
                type_name: Some(n),
                fields,
            } => (n, fields),
            _ => return,
        };
        if let Some(rec) = self.records.get(type_name).cloned() {
            let field_names: Vec<String> = rec.fields.iter().map(|(n, _, _)| n.clone()).collect();
            let mut given: HashSet<String> = HashSet::new();
            for f in fields {
                if !field_names.contains(&f.name) {
                    let hint = suggest(&f.name, field_names.clone());
                    self.err_hint(
                        format!("unknown field '{}' on record '{}'", f.name, type_name),
                        &e.span,
                        hint,
                    );
                }
                given.insert(f.name.clone());
            }
            for (fname, _, required) in &rec.fields {
                if *required && !given.contains(fname) {
                    self.err(
                        format!("record '{type_name}' is missing required field '{fname}'"),
                        &e.span,
                    );
                }
            }
        } else if self.enums.contains_key(type_name) {
            self.err(
                format!("'{type_name}' is an enum, not a record — it cannot be constructed with record syntax"),
                &e.span,
            );
        } else {
            let mut cands: Vec<String> = self.records.keys().cloned().collect();
            cands.sort();
            let hint = suggest(type_name, cands);
            self.err_hint(
                format!("unknown type '{type_name}' in record literal"),
                &e.span,
                hint,
            );
        }
    }

    // ---- use validity ----

    fn check_uses(&mut self, uses: &[&UseDecl]) {
        for u in uses {
            if u.form == UseForm::Pack {
                if !PACKS.contains(&u.name.as_str()) {
                    let hint = suggest(
                        &u.name,
                        PACKS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    );
                    self.err_hint(format!("unknown tool pack '{}'", u.name), &u.span, hint);
                } else if u.name != "files" && u.options.is_some() {
                    self.err(format!("the '{}' pack takes no options", u.name), &u.span);
                }
            }
        }
    }

    // ---- config blocks ----

    fn check_config_blocks(&mut self, configs: &[&ConfigBlock]) {
        let specs = config_specs();
        let mut seen: HashSet<String> = HashSet::new();
        // Read policy caps for budget checks.
        for cb in configs {
            if cb.name == "policy" {
                self.read_policy_caps(cb);
            }
        }
        for cb in configs {
            if !seen.insert(cb.name.clone()) {
                self.err(
                    format!("duplicate '{}' block (each config block may appear at most once, SPEC §3.2)", cb.name),
                    &cb.span,
                );
            }
            if let Some(spec) = specs.get(cb.name.as_str()) {
                let where_ = cb.name.clone();
                self.check_block_keys(&cb.settings, spec, &where_);
                if cb.name == "model" {
                    self.check_model_values(cb);
                } else if cb.name == "policy" {
                    self.check_policy_values(cb);
                }
            }
        }
    }

    fn check_block_keys(&mut self, settings: &[Setting], spec: &ConfigSpec, where_: &str) {
        let block = match spec {
            ConfigSpec::Block(m) => m,
            ConfigSpec::Scalar => return,
        };
        for s in settings {
            match s {
                Setting::Block(nested) => match block.get(nested.name.as_str()) {
                    Some(sub @ ConfigSpec::Block(_)) => {
                        let w = format!("{where_}.{}", nested.name);
                        self.check_block_keys(&nested.settings, sub, &w);
                    }
                    Some(ConfigSpec::Scalar) => { /* scalar-or-block shorthand tolerated */ }
                    None => {
                        let cands: Vec<String> = block.keys().map(|k| k.to_string()).collect();
                        let hint = suggest(&nested.name, cands);
                        self.err_hint(
                            format!("{where_}: unknown key '{}'", nested.name),
                            &nested.span,
                            hint,
                        );
                    }
                },
                Setting::KeyValue { key, value } => {
                    if !block.contains_key(key.as_str()) {
                        let cands: Vec<String> = block.keys().map(|k| k.to_string()).collect();
                        let hint = suggest(key, cands);
                        self.err_hint(format!("{where_}: unknown key '{key}'"), &value.span, hint);
                    }
                }
            }
        }
    }

    fn setting<'a>(&self, cb: &'a ConfigBlock, key: &str) -> Option<&'a Expr> {
        cb.settings.iter().find_map(|s| match s {
            Setting::KeyValue { key: k, value } if k == key => Some(value),
            _ => None,
        })
    }

    fn check_model_values(&mut self, cb: &ConfigBlock) {
        match self.setting(cb, "provider") {
            None => self.err("model: missing required key 'provider'", &cb.span),
            Some(e) => {
                let prov = match &e.kind {
                    ExprKind::Ident(n) => Some(n.clone()),
                    ExprKind::Literal(Lit::Str(s)) => Some(s.clone()),
                    _ => None,
                };
                if let Some(p) = prov {
                    if !PROVIDERS.contains(&p.as_str()) {
                        let hint = suggest(
                            &p,
                            PROVIDERS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                        );
                        self.err_hint(
                            format!(
                                "model.provider: '{p}' is not one of {}",
                                PROVIDERS.join(", ")
                            ),
                            &e.span,
                            hint,
                        );
                    }
                }
            }
        }
        if self.setting(cb, "name").is_none() {
            self.err("model: missing required key 'name'", &cb.span);
        }
        if let Some(e) = self.setting(cb, "temperature") {
            match &e.kind {
                ExprKind::Literal(Lit::Int(i)) => {
                    if *i < 0 || *i > 2 {
                        self.err("model.temperature: must be between 0 and 2", &e.span);
                    }
                }
                ExprKind::Literal(Lit::Float(f)) => {
                    if *f < 0.0 || *f > 2.0 {
                        self.err("model.temperature: must be between 0 and 2", &e.span);
                    }
                }
                ExprKind::Literal(_) => self.err("model.temperature: expected a number", &e.span),
                _ => {}
            }
        }
        if let Some(e) = self.setting(cb, "max_tokens") {
            if matches!(&e.kind, ExprKind::Literal(l) if !matches!(l, Lit::Int(_))) {
                self.err("model.max_tokens: expected an integer", &e.span);
            }
        }
    }

    fn check_policy_values(&mut self, cb: &ConfigBlock) {
        if let Some(e) = self.setting(cb, "allow_shell") {
            if let ExprKind::Ident(n) = &e.kind {
                if !ALLOW_SHELL_VALUES.contains(&n.as_str()) {
                    self.err("policy.allow_shell: must be never, ask, or always", &e.span);
                }
            }
        }
        if let Some(e) = self.setting(cb, "on_violation") {
            if let ExprKind::Ident(n) = &e.kind {
                if !ON_VIOLATION_VALUES.contains(&n.as_str()) {
                    self.err("policy.on_violation: must be stop or ask", &e.span);
                }
            }
        }
        if let Some(e) = self.setting(cb, "max_steps") {
            if matches!(&e.kind, ExprKind::Literal(l) if !matches!(l, Lit::Int(_))) {
                self.err("policy.max_steps: expected an integer", &e.span);
            }
        }
        if let Some(e) = self.setting(cb, "max_spend") {
            if matches!(&e.kind, ExprKind::Literal(l) if !matches!(l, Lit::Money(_))) {
                self.err(
                    "policy.max_spend: expected a money literal (e.g. $1.00)",
                    &e.span,
                );
            }
        }
    }

    fn read_policy_caps(&mut self, cb: &ConfigBlock) {
        if let Some(ExprKind::Literal(Lit::Money(s))) =
            self.setting(cb, "max_spend").map(|e| &e.kind)
        {
            self.policy_spend = s.parse::<f64>().ok();
        }
        if let Some(ExprKind::Literal(Lit::Int(i))) = self.setting(cb, "max_steps").map(|e| &e.kind)
        {
            self.policy_steps = Some(*i);
        }
        if let Some(ExprKind::Literal(Lit::Int(i))) =
            self.setting(cb, "max_tool_calls").map(|e| &e.kind)
        {
            self.policy_calls = Some(*i);
        }
    }
}

enum Purity {
    Fn,
    Tool,
}

fn term_kw(kind: &StmtKind) -> Option<&'static str> {
    match kind {
        StmtKind::Reply(_) => Some("reply"),
        StmtKind::Return(_) => Some("return"),
        StmtKind::Halt(_) => Some("halt"),
        StmtKind::Throw(_) => Some("throw"),
        StmtKind::Break => Some("break"),
        StmtKind::Continue => Some("continue"),
        _ => None,
    }
}

fn money_arg(args: &[Arg], label: &str) -> Option<f64> {
    args.iter()
        .find(|a| a.label.as_deref() == Some(label))
        .and_then(|a| match &a.value.kind {
            ExprKind::Literal(Lit::Money(s)) => s.parse::<f64>().ok(),
            _ => None,
        })
}

fn int_arg(args: &[Arg], label: &str) -> Option<i64> {
    args.iter()
        .find(|a| a.label.as_deref() == Some(label))
        .and_then(|a| match &a.value.kind {
            ExprKind::Literal(Lit::Int(i)) => Some(*i),
            _ => None,
        })
}

fn is_state_target(target: &Expr, state_names: &HashSet<String>) -> bool {
    match &target.kind {
        ExprKind::Ident(n) => state_names.contains(n),
        ExprKind::Member { obj, .. } => {
            if let ExprKind::Ident(n) = &obj.kind {
                n == "state"
            } else {
                false
            }
        }
        _ => false,
    }
}

fn is_effect_call(
    name: &str,
    callables: &HashMap<String, Option<TypeRef>>,
    pack: &HashSet<String>,
) -> bool {
    callables.contains_key(name)
        || pack.contains(name)
        || matches!(name, "recall_one" | "http" | "shell")
}

// ---- generic AST walkers ----

fn walk_stmts(block: &Block, f: &mut impl FnMut(&Stmt)) {
    for s in &block.stmts {
        f(s);
        walk_stmt_children(s, f);
    }
}

fn walk_stmt_children(s: &Stmt, f: &mut impl FnMut(&Stmt)) {
    match &s.kind {
        StmtKind::Block(b) => walk_stmts(b, f),
        StmtKind::If {
            branches,
            else_block,
        } => {
            for (_, b) in branches {
                walk_stmts(b, f);
            }
            if let Some(b) = else_block {
                walk_stmts(b, f);
            }
        }
        StmtKind::For { body, .. }
        | StmtKind::While { body, .. }
        | StmtKind::Repeat { body, .. } => walk_stmts(body, f),
        StmtKind::Try {
            body, catch_block, ..
        } => {
            walk_stmts(body, f);
            walk_stmts(catch_block, f);
        }
        _ => {}
    }
    // Also descend into expression-embedded blocks (retry/budget/match/lambda).
    walk_stmt_exprs(s, &mut |e| walk_expr_blocks_stmts(e, f));
}

fn walk_expr_blocks_stmts(e: &Expr, f: &mut impl FnMut(&Stmt)) {
    match &e.kind {
        ExprKind::Retry { body, .. } | ExprKind::Budget { body, .. } => walk_stmts(body, f),
        ExprKind::Block(b) => walk_stmts(b, f),
        _ => {}
    }
}

fn walk_exprs(block: &Block, f: &mut impl FnMut(&Expr)) {
    walk_stmts_for_exprs(block, &mut |s| walk_stmt_exprs(s, f));
}

fn walk_stmts_for_exprs(block: &Block, g: &mut impl FnMut(&Stmt)) {
    for s in &block.stmts {
        g(s);
        match &s.kind {
            StmtKind::Block(b) => walk_stmts_for_exprs(b, g),
            StmtKind::If {
                branches,
                else_block,
            } => {
                for (_, b) in branches {
                    walk_stmts_for_exprs(b, g);
                }
                if let Some(b) = else_block {
                    walk_stmts_for_exprs(b, g);
                }
            }
            StmtKind::For { body, .. }
            | StmtKind::While { body, .. }
            | StmtKind::Repeat { body, .. } => walk_stmts_for_exprs(body, g),
            StmtKind::Try {
                body, catch_block, ..
            } => {
                walk_stmts_for_exprs(body, g);
                walk_stmts_for_exprs(catch_block, g);
            }
            _ => {}
        }
    }
}

fn walk_stmt_exprs(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    let mut visit = |e: &Expr| walk_expr(e, f);
    match &s.kind {
        StmtKind::Bind { value, .. } => visit(value),
        StmtKind::Assign { target, value, .. } => {
            visit(target);
            visit(value);
        }
        StmtKind::If { branches, .. } => {
            for (c, _) in branches {
                visit(c);
            }
        }
        StmtKind::For { iter, .. } => visit(iter),
        StmtKind::While { cond, .. } => visit(cond),
        StmtKind::Repeat { count, .. } => visit(count),
        StmtKind::Return(Some(v)) => visit(v),
        StmtKind::Throw(v)
        | StmtKind::Forget(v)
        | StmtKind::Reply(v)
        | StmtKind::Emit(v)
        | StmtKind::Halt(v) => visit(v),
        StmtKind::Remember { key, value, .. } => {
            if let Some(RememberKey::Expr(e)) = key {
                visit(e);
            }
            visit(value);
        }
        StmtKind::Expr(e) => visit(e),
        _ => {}
    }
}

fn walk_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::InterpString(parts) => {
            for p in parts {
                if let InterpPart::Expr(x) = p {
                    walk_expr(x, f);
                }
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            walk_expr(left, f);
            walk_expr(right, f);
        }
        ExprKind::UnOp { operand, .. } => walk_expr(operand, f),
        ExprKind::Member { obj, .. } => walk_expr(obj, f),
        ExprKind::Index { obj, index } => {
            walk_expr(obj, f);
            walk_expr(index, f);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(&a.value, f);
            }
        }
        ExprKind::ListLit(items) => items.iter().for_each(|i| walk_expr(i, f)),
        ExprKind::MapLit(entries) => entries.iter().for_each(|(k, v)| {
            walk_expr(k, f);
            walk_expr(v, f);
        }),
        ExprKind::ConfigLit { fields, .. } => fields.iter().for_each(|x| walk_expr(&x.value, f)),
        ExprKind::Lambda { body, .. } => walk_expr(body, f),
        ExprKind::Match { subject, arms } => {
            walk_expr(subject, f);
            for a in arms {
                walk_expr(&a.body, f);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            walk_expr(lo, f);
            walk_expr(hi, f);
        }
        ExprKind::Gen {
            prompt,
            with_config,
            ..
        } => {
            walk_expr(prompt, f);
            if let Some(w) = with_config {
                walk_expr(w, f);
            }
        }
        ExprKind::Delegate { goal, with_config } => {
            walk_expr(goal, f);
            if let Some(w) = with_config {
                walk_expr(w, f);
            }
        }
        ExprKind::Spawn(t) | ExprKind::Await(t) => walk_expr(t, f),
        ExprKind::Recall { query, .. } => walk_expr(query, f),
        ExprKind::Retry { max, body, until } => {
            walk_expr(max, f);
            for s in &body.stmts {
                walk_stmt_exprs(s, f);
            }
            walk_expr(until, f);
        }
        ExprKind::Parallel(branches) => branches.iter().for_each(|b| walk_expr(&b.value, f)),
        ExprKind::Budget { args, body } => {
            for a in args {
                walk_expr(&a.value, f);
            }
            for s in &body.stmts {
                walk_stmt_exprs(s, f);
            }
        }
        ExprKind::Block(b) => {
            for s in &b.stmts {
                walk_stmt_exprs(s, f);
            }
        }
        _ => {}
    }
}

fn config_specs() -> HashMap<&'static str, ConfigSpec> {
    use ConfigSpec::*;
    let mut m = HashMap::new();
    let mut model = BTreeMap::new();
    for k in [
        "provider",
        "name",
        "temperature",
        "max_tokens",
        "api_key",
        "base_url",
        "fallback",
    ] {
        model.insert(k, Scalar);
    }
    let mut pricing = BTreeMap::new();
    pricing.insert("input_per_mtok", Scalar);
    pricing.insert("output_per_mtok", Scalar);
    model.insert("pricing", Block(pricing));
    m.insert("model", Block(model));

    let mut memory = BTreeMap::new();
    memory.insert("store", Scalar);
    memory.insert("facts", Scalar);
    let mut conv = BTreeMap::new();
    conv.insert("enabled", Scalar);
    conv.insert("window", Scalar);
    memory.insert("conversation", Block(conv));
    let mut sem = BTreeMap::new();
    sem.insert("enabled", Scalar);
    sem.insert("top_k", Scalar);
    let mut emb = BTreeMap::new();
    emb.insert("provider", Scalar);
    emb.insert("model", Scalar);
    sem.insert("embeddings", Block(emb));
    memory.insert("semantic", Block(sem));
    m.insert("memory", Block(memory));

    let mut persona = BTreeMap::new();
    for k in [
        "tone",
        "traits",
        "language",
        "instructions",
        "system_prompt",
    ] {
        persona.insert(k, Scalar);
    }
    m.insert("persona", Block(persona));

    let mut knowledge = BTreeMap::new();
    for k in ["file", "text", "url"] {
        knowledge.insert(k, Scalar);
    }
    m.insert("knowledge", Block(knowledge));

    let mut policy = BTreeMap::new();
    for k in [
        "max_steps",
        "max_tool_calls",
        "max_requests_per_run",
        "max_spend",
        "max_gen_retries",
        "max_delegate_depth",
        "allow_shell",
        "allow_unsafe_tools",
        "allow_mcp",
        "allow_local_http",
        "allowed_domains",
        "on_violation",
        "i_understand_injection_risk",
    ] {
        policy.insert(k, Scalar);
    }
    m.insert("policy", Block(policy));
    m
}
