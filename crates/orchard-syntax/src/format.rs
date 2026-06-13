//! The canonical formatter (`orch fmt`). Re-emits the AST in one true style:
//! 4-space indent, one blank line between top-level items and agent members,
//! braces on the declaration line. Literals and strings are sliced byte-exact
//! from source (so triple-quoted prompts round-trip), and leading comments are
//! preserved by interleaving them at construct boundaries. Idempotent and
//! AST-preserving.

use crate::ast::*;
use crate::error::SyntaxError;
use crate::parser::parse_source;

const INDENT: &str = "    ";

/// Format Orchard source. Errors on malformed input (single-error-then-bail).
pub fn format_source(source: &str, filename: &str) -> Result<String, SyntaxError> {
    let program = parse_source(source, filename)?;
    let chars: Vec<char> = source.chars().collect();
    let comments = scan_comments(&chars);
    let mut f = Formatter {
        src: chars,
        out: String::new(),
        comments,
        ci: 0,
    };
    f.program(&program);
    let trimmed = f.out.trim_end_matches('\n');
    Ok(format!("{trimmed}\n"))
}

struct Formatter {
    src: Vec<char>,
    out: String,
    comments: Vec<(usize, String)>,
    ci: usize,
}

impl Formatter {
    fn slice(&self, start: usize, end: usize) -> String {
        self.src
            .get(start..end)
            .map(|s| s.iter().collect())
            .unwrap_or_default()
    }

    fn line(&mut self, indent: usize, text: &str) {
        for _ in 0..indent {
            self.out.push_str(INDENT);
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// Emit any comments that start before `offset`, at `indent`.
    fn flush(&mut self, offset: usize, indent: usize) {
        while self.ci < self.comments.len() && self.comments[self.ci].0 < offset {
            let text = self.comments[self.ci].1.clone();
            for l in text.split('\n') {
                self.line(indent, l);
            }
            self.ci += 1;
        }
    }

    fn program(&mut self, p: &Program) {
        if let Some(v) = &p.pragma {
            self.line(0, &format!("#!orchard {v}"));
            self.out.push('\n');
        }
        for (i, item) in p.items.iter().enumerate() {
            let off = top_item_offset(item);
            self.flush(off, 0);
            if i > 0 {
                self.out.push('\n');
            }
            self.top_item(item);
        }
        // trailing comments
        self.flush(usize::MAX, 0);
    }

    fn top_item(&mut self, item: &TopItem) {
        match item {
            TopItem::Agent(a) => self.agent(a),
            TopItem::Type(t) => self.type_decl(0, t),
            TopItem::Enum(e) => self.enum_decl(0, e),
            TopItem::Fn(c) => self.callable(0, c),
            TopItem::Use(u) => self.use_decl(0, u),
        }
    }

    fn agent(&mut self, a: &AgentDecl) {
        self.line(0, &format!("agent {} {{", a.name));
        for (i, m) in a.members.iter().enumerate() {
            self.flush(member_offset(m), 1);
            if i > 0 {
                self.out.push('\n');
            }
            self.member(m);
        }
        self.line(0, "}");
    }

    fn member(&mut self, m: &AgentMember) {
        match m {
            AgentMember::Config(cb) => self.config_block(1, cb),
            AgentMember::Use(u) => self.use_decl(1, u),
            AgentMember::State(s) => self.state_decl(1, s),
            AgentMember::Type(t) => self.type_decl(1, t),
            AgentMember::Enum(e) => self.enum_decl(1, e),
            AgentMember::Fn(c) | AgentMember::Tool(c) | AgentMember::Skill(c) => {
                self.callable(1, c)
            }
            AgentMember::On(o) => self.on_decl(1, o),
        }
    }

    fn config_block(&mut self, indent: usize, cb: &ConfigBlock) {
        // Try an inline form when there are no nested sub-blocks and it fits.
        let all_kv = cb
            .settings
            .iter()
            .all(|s| matches!(s, Setting::KeyValue { .. }));
        if all_kv && !cb.settings.is_empty() {
            let inner: Vec<String> = cb
                .settings
                .iter()
                .map(|s| match s {
                    Setting::KeyValue { key, value } => format!("{key}: {}", self.expr(value, 0)),
                    Setting::Block(_) => unreachable!(),
                })
                .collect();
            let line = format!("{} {{ {} }}", cb.name, inner.join(", "));
            if line.len() + indent * 4 <= 96 && !line.contains('\n') {
                self.line(indent, &line);
                return;
            }
        }
        self.line(indent, &format!("{} {{", cb.name));
        for s in &cb.settings {
            match s {
                Setting::KeyValue { key, value } => {
                    let v = self.expr(value, 0);
                    self.line(indent + 1, &format!("{key}: {v}"));
                }
                Setting::Block(nested) => self.config_block(indent + 1, nested),
            }
        }
        self.line(indent, "}");
    }

    fn use_decl(&mut self, indent: usize, u: &UseDecl) {
        let text = match u.form {
            UseForm::Mcp => {
                let t = u
                    .target
                    .as_ref()
                    .map(|e| self.expr(e, 0))
                    .unwrap_or_default();
                format!("use mcp({t}) as {}", u.name)
            }
            UseForm::Env => {
                let t = u
                    .target
                    .as_ref()
                    .map(|e| self.expr(e, 0))
                    .unwrap_or_default();
                format!("use env {t}")
            }
            UseForm::Import => {
                let t = u
                    .target
                    .as_ref()
                    .map(|e| self.expr(e, 0))
                    .unwrap_or_default();
                format!("use {t} as {}", u.name)
            }
            UseForm::Pack => {
                if let Some(fields) = &u.options {
                    let inner: Vec<String> = fields
                        .iter()
                        .map(|fi| format!("{}: {}", fi.name, self.expr(&fi.value, 0)))
                        .collect();
                    format!("use {} {{ {} }}", u.name, inner.join(", "))
                } else {
                    format!("use {}", u.name)
                }
            }
        };
        self.line(indent, &text);
    }

    fn state_decl(&mut self, indent: usize, s: &StateDecl) {
        let mut t = format!("state {}: {}", s.name, self.type_ref(&s.ty));
        if let Some(d) = &s.default {
            t.push_str(&format!(" = {}", self.expr(d, 0)));
        }
        self.line(indent, &t);
    }

    fn type_decl(&mut self, indent: usize, t: &TypeDecl) {
        if t.fields.is_empty() {
            self.line(indent, &format!("type {} {{}}", t.name));
            return;
        }
        self.line(indent, &format!("type {} {{", t.name));
        for f in &t.fields {
            let mut line = format!("{}: {}", f.name, self.type_ref(&f.ty));
            if let Some(d) = &f.default {
                line.push_str(&format!(" = {}", self.expr(d, 0)));
            }
            self.line(indent + 1, &line);
        }
        self.line(indent, "}");
    }

    fn enum_decl(&mut self, indent: usize, e: &EnumDecl) {
        let all_simple = e.variants.iter().all(|v| v.params.is_empty());
        if all_simple {
            let names: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
            let inline = format!("enum {} {{ {} }}", e.name, names.join(", "));
            if inline.len() <= 96 {
                self.line(indent, &inline);
                return;
            }
        }
        self.line(indent, &format!("enum {} {{", e.name));
        for v in &e.variants {
            if v.params.is_empty() {
                self.line(indent + 1, &v.name);
            } else {
                let ps: Vec<String> = v
                    .params
                    .iter()
                    .map(|p| {
                        format!(
                            "{}: {}",
                            p.name,
                            p.ty.as_ref().map(|t| self.type_ref(t)).unwrap_or_default()
                        )
                    })
                    .collect();
                self.line(indent + 1, &format!("{}({})", v.name, ps.join(", ")));
            }
        }
        self.line(indent, "}");
    }

    fn callable(&mut self, indent: usize, c: &Callable) {
        for a in &c.annotations {
            let args = if a.args.is_empty() {
                String::new()
            } else {
                let inner: Vec<String> = a.args.iter().map(|arg| self.arg(arg)).collect();
                format!("({})", inner.join(", "))
            };
            self.line(indent, &format!("@{}{}", a.name, args));
        }
        let kw = match c.kind {
            CallableKind::Fn => "fn",
            CallableKind::Tool => "tool",
            CallableKind::Skill => "skill",
        };
        let params = self.params(&c.params);
        let ret = c
            .return_type
            .as_ref()
            .map(|t| format!(" -> {}", self.type_ref(t)))
            .unwrap_or_default();
        let header = format!("{kw} {}({params}){ret} {{", c.name);
        self.emit_block_header(indent, &header, &c.body);
    }

    fn on_decl(&mut self, indent: usize, o: &OnDecl) {
        let header = match o.kind {
            HandlerKind::Start => "on start() {".to_string(),
            HandlerKind::Message => {
                let p = o
                    .param
                    .as_ref()
                    .map(|p| self.single_param(p))
                    .unwrap_or_default();
                let ret = o
                    .return_type
                    .as_ref()
                    .map(|t| format!(" -> {}", self.type_ref(t)))
                    .unwrap_or_default();
                format!("on message({p}){ret} {{")
            }
            HandlerKind::Schedule => {
                let v = o
                    .schedule_value
                    .as_ref()
                    .map(|e| self.expr(e, 0))
                    .unwrap_or_default();
                format!("on schedule({}: {v}) {{", o.schedule_kind)
            }
            HandlerKind::File => {
                let p = o
                    .param
                    .as_ref()
                    .map(|p| self.single_param(p))
                    .unwrap_or_default();
                let path = o
                    .watch_path
                    .as_ref()
                    .map(|e| self.expr(e, 0))
                    .unwrap_or_default();
                format!("on file({p}) in {path} {{")
            }
        };
        self.emit_block_header(indent, &header, &o.body);
    }

    fn params(&self, ps: &[Param]) -> String {
        ps.iter()
            .map(|p| {
                let mut s = format!(
                    "{}: {}",
                    p.name,
                    p.ty.as_ref().map(|t| self.type_ref(t)).unwrap_or_default()
                );
                if let Some(d) = &p.default {
                    s.push_str(&format!(" = {}", self.expr(d, 0)));
                }
                s
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn single_param(&self, p: &Param) -> String {
        format!(
            "{}: {}",
            p.name,
            p.ty.as_ref().map(|t| self.type_ref(t)).unwrap_or_default()
        )
    }

    fn type_ref(&self, t: &TypeRef) -> String {
        let mut s = t.name.clone();
        if !t.args.is_empty() {
            let inner: Vec<String> = t.args.iter().map(|a| self.type_ref(a)).collect();
            s.push_str(&format!("<{}>", inner.join(", ")));
        }
        if t.optional {
            s.push('?');
        }
        s
    }

    /// Emit a brace block header line then its statements at indent+1.
    fn emit_block_header(&mut self, indent: usize, header: &str, body: &Block) {
        if body.stmts.is_empty() {
            // collapse to ` {}` on the header
            let h = header.trim_end_matches(" {");
            self.line(indent, &format!("{h} {{}}"));
            return;
        }
        self.line(indent, header);
        for st in &body.stmts {
            self.flush(st.span.start, indent + 1);
            self.stmt(indent + 1, st);
        }
        self.line(indent, "}");
    }

    fn stmt(&mut self, indent: usize, st: &Stmt) {
        match &st.kind {
            StmtKind::Block(b) => self.emit_block_header(indent, "{", b),
            StmtKind::Bind {
                name,
                ty,
                value,
                mutable,
            } => {
                let kw = if *mutable { "var" } else { "let" };
                let tyann = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_ref(t)))
                    .unwrap_or_default();
                let v = self.expr(value, indent);
                self.line(indent, &format!("{kw} {name}{tyann} = {v}"));
            }
            StmtKind::Assign { target, op, value } => {
                let t = self.expr(target, indent);
                let v = self.expr(value, indent);
                self.line(indent, &format!("{t} {op} {v}"));
            }
            StmtKind::If {
                branches,
                else_block,
            } => self.emit_if(indent, branches, else_block),
            StmtKind::For { var, iter, body } => {
                let it = self.expr(iter, indent);
                self.emit_block_header(indent, &format!("for {var} in {it} {{"), body);
            }
            StmtKind::While { cond, body } => {
                let c = self.expr(cond, indent);
                self.emit_block_header(indent, &format!("while {c} {{"), body);
            }
            StmtKind::Repeat { count, body } => {
                let c = self.expr(count, indent);
                self.emit_block_header(indent, &format!("repeat {c} {{"), body);
            }
            StmtKind::Return(v) => {
                let s = v
                    .as_ref()
                    .map(|e| format!("return {}", self.expr(e, indent)))
                    .unwrap_or_else(|| "return".into());
                self.line(indent, &s);
            }
            StmtKind::Break => self.line(indent, "break"),
            StmtKind::Continue => self.line(indent, "continue"),
            StmtKind::Try {
                body,
                catch_name,
                catch_block,
            } => {
                self.line(indent, "try {");
                for st in &body.stmts {
                    self.flush(st.span.start, indent + 1);
                    self.stmt(indent + 1, st);
                }
                self.line(indent, &format!("}} catch {catch_name} {{"));
                for st in &catch_block.stmts {
                    self.flush(st.span.start, indent + 1);
                    self.stmt(indent + 1, st);
                }
                self.line(indent, "}");
            }
            StmtKind::Throw(v) => {
                let s = self.expr(v, indent);
                self.line(indent, &format!("throw {s}"));
            }
            StmtKind::Remember {
                key,
                value,
                auto_key,
            } => {
                if *auto_key {
                    let v = self.expr(value, indent);
                    self.line(indent, &format!("remember {v}"));
                } else {
                    let k = match key {
                        Some(RememberKey::Ident(n)) => n.clone(),
                        Some(RememberKey::Expr(e)) => self.expr(e, indent),
                        None => String::new(),
                    };
                    let v = self.expr(value, indent);
                    self.line(indent, &format!("remember {k} = {v}"));
                }
            }
            StmtKind::Forget(v) => {
                let s = self.expr(v, indent);
                self.line(indent, &format!("forget {s}"));
            }
            StmtKind::Reply(v) => {
                let s = self.expr(v, indent);
                self.line(indent, &format!("reply {s}"));
            }
            StmtKind::Emit(v) => {
                let s = self.expr(v, indent);
                self.line(indent, &format!("emit {s}"));
            }
            StmtKind::Halt(v) => {
                let s = self.expr(v, indent);
                self.line(indent, &format!("halt {s}"));
            }
            StmtKind::Expr(e) => {
                let s = self.expr(e, indent);
                self.line(indent, &s);
            }
        }
    }

    fn emit_if(&mut self, indent: usize, branches: &[(Expr, Block)], else_block: &Option<Block>) {
        for (i, (cond, body)) in branches.iter().enumerate() {
            let c = self.expr(cond, indent);
            let kw = if i == 0 { "if" } else { "} else if" };
            self.line(indent, &format!("{kw} {c} {{"));
            for st in &body.stmts {
                self.flush(st.span.start, indent + 1);
                self.stmt(indent + 1, st);
            }
        }
        if let Some(eb) = else_block {
            self.line(indent, "} else {");
            for st in &eb.stmts {
                self.flush(st.span.start, indent + 1);
                self.stmt(indent + 1, st);
            }
        }
        self.line(indent, "}");
    }

    fn arg(&self, a: &Arg) -> String {
        match &a.label {
            Some(l) => format!("{l}: {}", self.expr(&a.value, 0)),
            None => self.expr(&a.value, 0),
        }
    }

    /// Render an expression (precedence-aware parens, byte-exact literals).
    fn expr(&self, e: &Expr, indent: usize) -> String {
        self.expr_prec(e, 0, indent).0
    }

    /// Returns `(text, precedence)`.
    fn expr_prec(&self, e: &Expr, _min: u8, indent: usize) -> (String, u8) {
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::InterpString(_) => {
                (self.slice(e.span.start, e.span.end), 11)
            }
            ExprKind::Ident(n) => (n.clone(), 11),
            ExprKind::This => ("this".into(), 11),
            ExprKind::BinOp { op, left, right } => {
                let prec = binop_prec(op);
                let l = self.wrap(left, prec, indent);
                let r = self.wrap(right, prec + 1, indent);
                (format!("{l} {op} {r}"), prec)
            }
            ExprKind::UnOp { op, operand } => {
                let inner = self.wrap(operand, 8, indent);
                if op == "not" {
                    (format!("not {inner}"), 8)
                } else {
                    (format!("{op}{inner}"), 8)
                }
            }
            ExprKind::Member {
                obj,
                name,
                optional,
            } => {
                let o = self.base(obj, indent);
                let dot = if *optional { "?." } else { "." };
                (format!("{o}{dot}{name}"), 10)
            }
            ExprKind::Index { obj, index } => {
                let o = self.base(obj, indent);
                (format!("{o}[{}]", self.expr(index, indent)), 10)
            }
            ExprKind::Call { callee, args } => {
                let c = self.base(callee, indent);
                let inner: Vec<String> = args.iter().map(|a| self.arg(a)).collect();
                (format!("{c}({})", inner.join(", ")), 10)
            }
            ExprKind::ListLit(items) => {
                let inner: Vec<String> = items.iter().map(|i| self.expr(i, indent)).collect();
                (format!("[{}]", inner.join(", ")), 11)
            }
            ExprKind::MapLit(entries) => {
                if entries.is_empty() {
                    ("{:}".into(), 11)
                } else {
                    let inner: Vec<String> = entries
                        .iter()
                        .map(|(k, v)| format!("{}: {}", self.expr(k, indent), self.expr(v, indent)))
                        .collect();
                    (format!("{{{}}}", inner.join(", ")), 11)
                }
            }
            ExprKind::ConfigLit { type_name, fields } => {
                let prefix = type_name
                    .as_ref()
                    .map(|n| format!("{n} "))
                    .unwrap_or_default();
                if fields.is_empty() {
                    (format!("{prefix}{{}}"), 11)
                } else {
                    let inner: Vec<String> = fields
                        .iter()
                        .map(|fi| format!("{}: {}", fi.name, self.expr(&fi.value, indent)))
                        .collect();
                    (format!("{prefix}{{ {} }}", inner.join(", ")), 11)
                }
            }
            ExprKind::Lambda { params, body } => {
                let ps: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                (
                    format!("({}) => {}", ps.join(", "), self.expr(body, indent)),
                    11,
                )
            }
            ExprKind::Match { subject, arms } => {
                let s = self.expr(subject, indent);
                let mut out = format!("match {s} {{\n");
                for arm in arms {
                    let pat = self.pattern(&arm.pattern);
                    let body = self.expr(&arm.body, indent + 1);
                    for _ in 0..indent + 1 {
                        out.push_str(INDENT);
                    }
                    out.push_str(&format!("{pat} => {body}\n"));
                }
                for _ in 0..indent {
                    out.push_str(INDENT);
                }
                out.push('}');
                (out, 10)
            }
            ExprKind::Range { lo, hi, inclusive } => {
                let op = if *inclusive { "..=" } else { ".." };
                (
                    format!(
                        "{}{op}{}",
                        self.wrap(lo, 5, indent),
                        self.wrap(hi, 5, indent)
                    ),
                    4,
                )
            }
            ExprKind::Gen {
                as_type,
                prompt,
                with_config,
            } => {
                let mut s = "gen".to_string();
                if let Some(t) = as_type {
                    s.push_str(&format!(" as {}", self.type_ref(t)));
                }
                s.push(' ');
                s.push_str(&self.operand(prompt, indent));
                if let Some(w) = with_config {
                    s.push_str(&format!(" with {}", self.expr(w, indent)));
                }
                (s, 10)
            }
            ExprKind::Delegate { goal, with_config } => {
                let mut s = format!("delegate {}", self.operand(goal, indent));
                if let Some(w) = with_config {
                    s.push_str(&format!(" with {}", self.expr(w, indent)));
                }
                (s, 10)
            }
            ExprKind::Spawn(t) => (format!("spawn {}", self.operand(t, indent)), 10),
            ExprKind::Await(t) => (format!("await {}", self.operand(t, indent)), 10),
            ExprKind::Recall { query, .. } => (format!("recall({})", self.expr(query, indent)), 10),
            ExprKind::Retry { max, body, until } => {
                let m = self.expr(max, indent);
                let b = self.block_expr(body, indent);
                let u = self.expr(until, indent);
                (format!("retry({m}) {b} until {u}"), 10)
            }
            ExprKind::Parallel(branches) => {
                let mut out = "parallel {\n".to_string();
                for fi in branches {
                    for _ in 0..indent + 1 {
                        out.push_str(INDENT);
                    }
                    out.push_str(&format!(
                        "{}: {}\n",
                        fi.name,
                        self.expr(&fi.value, indent + 1)
                    ));
                }
                for _ in 0..indent {
                    out.push_str(INDENT);
                }
                out.push('}');
                (out, 10)
            }
            ExprKind::Budget { args, body } => {
                let inner: Vec<String> = args.iter().map(|a| self.arg(a)).collect();
                let b = self.block_expr(body, indent);
                (format!("budget({}) {b}", inner.join(", ")), 10)
            }
            ExprKind::Block(b) => (self.block_expr(b, indent), 11),
        }
    }

    fn wrap(&self, e: &Expr, min: u8, indent: usize) -> String {
        let (text, prec) = self.expr_prec(e, min, indent);
        if prec < min {
            format!("({text})")
        } else {
            text
        }
    }

    fn base(&self, e: &Expr, indent: usize) -> String {
        let (text, prec) = self.expr_prec(e, 0, indent);
        if prec < 10 {
            format!("({text})")
        } else {
            text
        }
    }

    fn operand(&self, e: &Expr, indent: usize) -> String {
        match &e.kind {
            ExprKind::Literal(_)
            | ExprKind::InterpString(_)
            | ExprKind::Ident(_)
            | ExprKind::This
            | ExprKind::Member { .. }
            | ExprKind::Index { .. }
            | ExprKind::Call { .. }
            | ExprKind::ListLit(_)
            | ExprKind::MapLit(_) => self.expr(e, indent),
            _ => format!("({})", self.expr(e, indent)),
        }
    }

    fn block_expr(&self, b: &Block, indent: usize) -> String {
        if b.stmts.is_empty() {
            return "{}".into();
        }
        // A brace block used as a value: render inline-ish with statements.
        let mut out = "{\n".to_string();
        for st in &b.stmts {
            let mut sub = Formatter {
                src: self.src.clone(),
                out: String::new(),
                comments: vec![],
                ci: 0,
            };
            sub.stmt(indent + 1, st);
            out.push_str(&sub.out);
        }
        for _ in 0..indent {
            out.push_str(INDENT);
        }
        out.push('}');
        out
    }

    fn pattern(&self, p: &Pattern) -> String {
        match &p.kind {
            PatternKind::Wildcard => "_".into(),
            PatternKind::Ident(n) => n.clone(),
            PatternKind::Enum { name, binds } => {
                if binds.is_empty() {
                    name.clone()
                } else {
                    format!("{name}({})", binds.join(", "))
                }
            }
            PatternKind::Literal(e) => self.slice(e.span.start, e.span.end),
        }
    }
}

fn binop_prec(op: &str) -> u8 {
    match op {
        "or" => 1,
        "and" => 2,
        "==" | "!=" | "<" | "<=" | ">" | ">=" => 3,
        "|>" => 5,
        "+" | "-" => 6,
        "*" | "/" | "%" => 7,
        "??" => 9,
        _ => 1,
    }
}

fn top_item_offset(item: &TopItem) -> usize {
    match item {
        TopItem::Agent(a) => a.span.start,
        TopItem::Type(t) => t.span.start,
        TopItem::Enum(e) => e.span.start,
        TopItem::Fn(c) => c.span.start,
        TopItem::Use(u) => u.span.start,
    }
}

fn member_offset(m: &AgentMember) -> usize {
    match m {
        AgentMember::Config(cb) => cb.span.start,
        AgentMember::Use(u) => u.span.start,
        AgentMember::State(s) => s.span.start,
        AgentMember::Type(t) => t.span.start,
        AgentMember::Enum(e) => e.span.start,
        AgentMember::Fn(c) | AgentMember::Tool(c) | AgentMember::Skill(c) => c.span.start,
        AgentMember::On(o) => o.span.start,
    }
}

/// Scan source for `//` and nested `/* */` comments outside strings.
/// Returns `(char_offset, text)` in source order; line comments right-stripped.
fn scan_comments(src: &[char]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        match c {
            '"' => {
                // skip a string (triple or single), honoring escapes & {{ }}
                i = skip_string(src, i);
            }
            '`' => {
                i += 1;
                while i < n && src[i] != '`' {
                    i += 1;
                }
                i += 1;
            }
            '/' if i + 1 < n && src[i + 1] == '/' => {
                let start = i;
                while i < n && src[i] != '\n' {
                    i += 1;
                }
                let text: String = src[start..i].iter().collect();
                out.push((start, text.trim_end().to_string()));
            }
            '/' if i + 1 < n && src[i + 1] == '*' => {
                let start = i;
                i += 2;
                let mut depth = 1;
                while i < n && depth > 0 {
                    if i + 1 < n && src[i] == '/' && src[i + 1] == '*' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < n && src[i] == '*' && src[i + 1] == '/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                let text: String = src[start..i].iter().collect();
                out.push((start, text));
            }
            _ => i += 1,
        }
    }
    out
}

fn skip_string(src: &[char], start: usize) -> usize {
    let n = src.len();
    let triple = src.get(start..start + 3) == Some(&['"', '"', '"']);
    let delim_len = if triple { 3 } else { 1 };
    let mut i = start + delim_len;
    while i < n {
        if src[i] == '\\' {
            i += 2;
            continue;
        }
        if triple {
            if src.get(i..i + 3) == Some(&['"', '"', '"']) {
                return i + 3;
            }
        } else if src[i] == '"' {
            return i + 1;
        }
        i += 1;
    }
    n
}
