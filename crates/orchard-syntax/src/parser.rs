//! The Orchard 3.0 recursive-descent parser. A faithful port of v2's
//! `parser.py`: precedence-climbing for expressions, recursive descent for the
//! rest, single-error-then-bail. Honors every disambiguation (tight operands,
//! `expr_nostruct`, map/config/block, generics `>=` re-split, `with`
//! before/after).

use crate::ast::*;
use crate::diagnostics::{suggest, Diagnostic};
use crate::error::SyntaxError;
use crate::lexer::tokenize;
use crate::span::Span;
use crate::tokens::{Token, TokenKind, TokenValue};

type PResult<T> = Result<T, SyntaxError>;

const CONFIG_KEYWORDS: &[&str] = &["model", "memory", "persona", "knowledge", "policy"];

/// Parse Orchard source into a [`Program`].
pub fn parse_source(source: &str, filename: &str) -> PResult<Program> {
    let toks = tokenize(source, filename)?;
    Parser::new(toks).parse_program()
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
    no_struct: bool,
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Parser {
            toks,
            i: 0,
            no_struct: false,
        }
    }

    // ---- cursor ----

    fn cur(&self) -> &Token {
        &self.toks[self.i]
    }

    fn peek(&self, n: usize) -> &Token {
        let idx = (self.i + n).min(self.toks.len() - 1);
        &self.toks[idx]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.cur().kind == kind
    }

    fn is_kw(&self, word: &str) -> bool {
        self.cur().keyword_word() == Some(word)
    }

    fn advance(&mut self) -> Token {
        let t = self.toks[self.i].clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn error<T>(&self, msg: impl Into<String>, span: Span, hint: &str) -> PResult<T> {
        Err(SyntaxError::new(
            Diagnostic::error(msg, Some(span)).with_hint(hint),
        ))
    }

    fn cur_span(&self) -> Span {
        self.cur().span.clone()
    }

    fn describe(tok: &Token) -> String {
        match tok.kind {
            TokenKind::Eof => "end of file".to_string(),
            TokenKind::Newline => "end of line".to_string(),
            _ => format!("'{}'", tok.text),
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.advance())
        } else {
            let d = Self::describe(self.cur());
            self.error(format!("expected {what}, found {d}"), self.cur_span(), "")
        }
    }

    fn expect_kw(&mut self, word: &str) -> PResult<Token> {
        if self.is_kw(word) {
            Ok(self.advance())
        } else {
            let d = Self::describe(self.cur());
            self.error(format!("expected '{word}', found {d}"), self.cur_span(), "")
        }
    }

    fn expect_ident(&mut self, what: &str) -> PResult<String> {
        if self.at(TokenKind::Ident) {
            if let TokenValue::Str(s) = &self.advance().value {
                return Ok(s.clone());
            }
            unreachable!("ident token carries a string value")
        }
        let d = Self::describe(self.cur());
        self.error(format!("expected {what}, found {d}"), self.cur_span(), "")
    }

    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.advance();
        }
    }

    fn skip_seps(&mut self) {
        while matches!(
            self.cur().kind,
            TokenKind::Newline | TokenKind::Comma | TokenKind::Semi
        ) {
            self.advance();
        }
    }

    // ---- program ----

    fn parse_program(&mut self) -> PResult<Program> {
        self.skip_newlines();
        let mut pragma = None;
        if self.at(TokenKind::Pragma) {
            if let TokenValue::Str(v) = &self.cur().value {
                pragma = Some(v.clone());
            }
            self.advance();
        }
        let mut items = Vec::new();
        loop {
            self.skip_seps();
            if self.at(TokenKind::Eof) {
                break;
            }
            items.push(self.parse_top_item()?);
        }
        Ok(Program { pragma, items })
    }

    fn parse_top_item(&mut self) -> PResult<TopItem> {
        let annotations = self.parse_annotations()?;
        if self.is_kw("agent") {
            self.no_annotations(&annotations)?;
            Ok(TopItem::Agent(self.parse_agent()?))
        } else if self.is_kw("type") {
            self.no_annotations(&annotations)?;
            Ok(TopItem::Type(self.parse_type_decl()?))
        } else if self.is_kw("enum") {
            self.no_annotations(&annotations)?;
            Ok(TopItem::Enum(self.parse_enum_decl()?))
        } else if self.is_kw("use") {
            self.no_annotations(&annotations)?;
            Ok(TopItem::Use(self.parse_use()?))
        } else if self.is_kw("fn") {
            Ok(TopItem::Fn(
                self.parse_callable(CallableKind::Fn, annotations)?,
            ))
        } else {
            let d = Self::describe(self.cur());
            self.error(
                format!("expected a top-level item (agent, type, enum, fn, use), found {d}"),
                self.cur_span(),
                "",
            )
        }
    }

    fn no_annotations(&self, ann: &[Annotation]) -> PResult<()> {
        if let Some(a) = ann.first() {
            return self.error("annotations are not allowed here", a.span.clone(), "");
        }
        Ok(())
    }

    fn parse_annotations(&mut self) -> PResult<Vec<Annotation>> {
        let mut out = Vec::new();
        while self.at(TokenKind::At) {
            let sp = self.cur_span();
            self.advance();
            let name = self.expect_ident("an annotation name")?;
            let args = if self.at(TokenKind::LParen) {
                self.parse_call_args()?
            } else {
                Vec::new()
            };
            out.push(Annotation {
                name,
                args,
                span: sp,
            });
            self.skip_newlines();
        }
        Ok(out)
    }

    // ---- agent ----

    fn parse_agent(&mut self) -> PResult<AgentDecl> {
        let sp = self.cur_span();
        self.expect_kw("agent")?;
        let name = self.expect_ident("the agent name")?;
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut members = Vec::new();
        loop {
            self.skip_seps();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            members.push(self.parse_member()?);
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(AgentDecl {
            name,
            members,
            span: sp,
        })
    }

    fn parse_member(&mut self) -> PResult<AgentMember> {
        let annotations = self.parse_annotations()?;
        if let Some(w) = self.cur().keyword_word() {
            if CONFIG_KEYWORDS.contains(&w) {
                self.no_annotations(&annotations)?;
                return Ok(AgentMember::Config(self.parse_config_block()?));
            }
            match w {
                "use" => {
                    self.no_annotations(&annotations)?;
                    return Ok(AgentMember::Use(self.parse_use()?));
                }
                "state" => {
                    self.no_annotations(&annotations)?;
                    return Ok(AgentMember::State(self.parse_state()?));
                }
                "type" => {
                    self.no_annotations(&annotations)?;
                    return Ok(AgentMember::Type(self.parse_type_decl()?));
                }
                "enum" => {
                    self.no_annotations(&annotations)?;
                    return Ok(AgentMember::Enum(self.parse_enum_decl()?));
                }
                "fn" => {
                    return Ok(AgentMember::Fn(
                        self.parse_callable(CallableKind::Fn, annotations)?,
                    ))
                }
                "tool" => {
                    return Ok(AgentMember::Tool(
                        self.parse_callable(CallableKind::Tool, annotations)?,
                    ))
                }
                "skill" => {
                    return Ok(AgentMember::Skill(
                        self.parse_callable(CallableKind::Skill, annotations)?,
                    ))
                }
                "on" => {
                    self.no_annotations(&annotations)?;
                    return Ok(AgentMember::On(self.parse_on()?));
                }
                _ => {}
            }
        }
        let d = Self::describe(self.cur());
        self.error(
            format!("expected an agent member (model/memory/persona/knowledge/policy/use/state/type/enum/fn/tool/skill/on), found {d}"),
            self.cur_span(),
            "",
        )
    }

    // ---- config blocks ----

    fn parse_config_block(&mut self) -> PResult<ConfigBlock> {
        let kw = self.advance();
        let name = kw.keyword_word().unwrap_or("").to_string();
        self.expect(TokenKind::LBrace, "'{'")?;
        let settings = self.parse_settings()?;
        Ok(ConfigBlock {
            name,
            settings,
            span: kw.span,
        })
    }

    fn parse_settings(&mut self) -> PResult<Vec<Setting>> {
        let mut out = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let key_is_name = matches!(self.cur().kind, TokenKind::Ident | TokenKind::Keyword);
            if key_is_name && self.peek(1).kind == TokenKind::LBrace {
                let name_tok = self.advance();
                let name = name_word(&name_tok);
                self.expect(TokenKind::LBrace, "'{'")?;
                let nested = self.parse_settings()?;
                out.push(Setting::Block(ConfigBlock {
                    name,
                    settings: nested,
                    span: name_tok.span,
                }));
            } else {
                if !key_is_name {
                    let d = Self::describe(self.cur());
                    return self.error(
                        format!("expected a setting name, found {d}"),
                        self.cur_span(),
                        "",
                    );
                }
                let key_tok = self.advance();
                let key = name_word(&key_tok);
                self.expect(TokenKind::Colon, "':'")?;
                let value = self.parse_expr()?;
                out.push(Setting::KeyValue { key, value });
            }
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(out)
    }

    // ---- use ----

    fn parse_use(&mut self) -> PResult<UseDecl> {
        let sp = self.cur_span();
        self.expect_kw("use")?;
        // mcp(...) as alias
        if self.at(TokenKind::Ident)
            && name_word(self.cur()) == "mcp"
            && self.peek(1).kind == TokenKind::LParen
        {
            self.advance(); // mcp
            self.expect(TokenKind::LParen, "'('")?;
            let target = self.parse_expr()?;
            self.expect(TokenKind::RParen, "')'")?;
            self.expect_kw("as")?;
            let alias = self.expect_ident("an alias name")?;
            return Ok(UseDecl {
                form: UseForm::Mcp,
                name: alias,
                target: Some(target),
                options: None,
                span: sp,
            });
        }
        // env "path"
        if self.at(TokenKind::Ident)
            && name_word(self.cur()) == "env"
            && matches!(self.peek(1).kind, TokenKind::Str | TokenKind::StrStart)
        {
            self.advance(); // env
            let path = self.parse_string_expr()?;
            return Ok(UseDecl {
                form: UseForm::Env,
                name: String::new(),
                target: Some(path),
                options: None,
                span: sp,
            });
        }
        // "file.orch" as alias
        if matches!(self.cur().kind, TokenKind::Str | TokenKind::StrStart) {
            let path = self.parse_string_expr()?;
            self.expect_kw("as")?;
            let alias = self.expect_ident("an alias name")?;
            return Ok(UseDecl {
                form: UseForm::Import,
                name: alias,
                target: Some(path),
                options: None,
                span: sp,
            });
        }
        // pack [options]
        let pack = self.expect_ident("a tool pack name")?;
        let options = if self.at(TokenKind::LBrace) {
            match self.parse_config_lit(None)?.kind {
                ExprKind::ConfigLit { fields, .. } => Some(fields),
                _ => None,
            }
        } else {
            None
        };
        Ok(UseDecl {
            form: UseForm::Pack,
            name: pack,
            target: None,
            options,
            span: sp,
        })
    }

    // ---- types ----

    fn parse_type(&mut self) -> PResult<TypeRef> {
        let sp = self.cur_span();
        let name = self.expect_ident("a type name")?;
        let mut args = Vec::new();
        if self.at(TokenKind::Lt) {
            self.advance();
            args.push(self.parse_type()?);
            while self.at(TokenKind::Comma) {
                self.advance();
                args.push(self.parse_type()?);
            }
            self.expect_type_close()?;
        }
        let optional = if self.at(TokenKind::Question) {
            self.advance();
            true
        } else {
            false
        };
        Ok(TypeRef {
            name,
            args,
            optional,
            span: sp,
        })
    }

    /// Close a type-argument list. `>` closes directly; a `>=` is split in place
    /// into a consumed `>` and a remaining `=` (ASSIGN). `>>` is two `>`.
    fn expect_type_close(&mut self) -> PResult<()> {
        if self.at(TokenKind::Gt) {
            self.advance();
            Ok(())
        } else if self.at(TokenKind::Ge) {
            let span = self.cur_span();
            self.toks[self.i] = Token::new(TokenKind::Assign, TokenValue::None, "=", span);
            Ok(())
        } else {
            let d = Self::describe(self.cur());
            self.error(
                format!("expected '>' to close type arguments, found {d}"),
                self.cur_span(),
                "",
            )
        }
    }

    // ---- members ----

    fn parse_state(&mut self) -> PResult<StateDecl> {
        let sp = self.cur_span();
        self.expect_kw("state")?;
        let name = self.expect_ident("a state field name")?;
        self.expect(TokenKind::Colon, "':'")?;
        let ty = self.parse_type()?;
        let default = if self.at(TokenKind::Assign) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(StateDecl {
            name,
            ty,
            default,
            span: sp,
        })
    }

    fn parse_type_decl(&mut self) -> PResult<TypeDecl> {
        let sp = self.cur_span();
        self.expect_kw("type")?;
        let name = self.expect_ident("a type name")?;
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut fields = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let fsp = self.cur_span();
            let fname = self.expect_ident("a field name")?;
            self.expect(TokenKind::Colon, "':'")?;
            let ty = self.parse_type()?;
            let default = if self.at(TokenKind::Assign) {
                self.advance();
                Some(self.parse_expr()?)
            } else {
                None
            };
            fields.push(Field {
                name: fname,
                ty,
                default,
                span: fsp,
            });
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(TypeDecl {
            name,
            fields,
            span: sp,
        })
    }

    fn parse_enum_decl(&mut self) -> PResult<EnumDecl> {
        let sp = self.cur_span();
        self.expect_kw("enum")?;
        let name = self.expect_ident("an enum name")?;
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut variants = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let vsp = self.cur_span();
            let vname = self.expect_ident("a variant name")?;
            let params = if self.at(TokenKind::LParen) {
                self.parse_params()?
            } else {
                Vec::new()
            };
            variants.push(Variant {
                name: vname,
                params,
                span: vsp,
            });
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(EnumDecl {
            name,
            variants,
            span: sp,
        })
    }

    fn parse_callable(
        &mut self,
        kind: CallableKind,
        annotations: Vec<Annotation>,
    ) -> PResult<Callable> {
        let sp = self.cur_span();
        self.advance(); // fn/tool/skill
        let name = self.expect_ident("a name")?;
        let params = self.parse_params()?;
        let return_type = if self.at(TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        Ok(Callable {
            kind,
            name,
            params,
            return_type,
            body,
            annotations,
            span: sp,
        })
    }

    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.expect(TokenKind::LParen, "'('")?;
        let mut out = Vec::new();
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let psp = self.cur_span();
            let name = self.expect_ident("a parameter name")?;
            self.expect(TokenKind::Colon, "':'")?;
            let ty = self.parse_type()?;
            let default = if self.at(TokenKind::Assign) {
                self.advance();
                Some(self.parse_expr()?)
            } else {
                None
            };
            out.push(Param {
                name,
                ty: Some(ty),
                default,
                span: psp,
            });
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'")?;
        Ok(out)
    }

    fn parse_on(&mut self) -> PResult<OnDecl> {
        let sp = self.cur_span();
        self.expect_kw("on")?;
        let kind_name = self.expect_ident("a handler kind (start/message/schedule/file)")?;
        match kind_name.as_str() {
            "start" => {
                self.expect(TokenKind::LParen, "'('")?;
                self.expect(TokenKind::RParen, "')'")?;
                let body = self.parse_block()?;
                Ok(OnDecl {
                    kind: HandlerKind::Start,
                    param: None,
                    schedule_kind: String::new(),
                    schedule_value: None,
                    watch_path: None,
                    return_type: None,
                    body,
                    span: sp,
                })
            }
            "message" => {
                let param = self.parse_single_param()?;
                let return_type = if self.at(TokenKind::Arrow) {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let body = self.parse_block()?;
                Ok(OnDecl {
                    kind: HandlerKind::Message,
                    param: Some(param),
                    schedule_kind: String::new(),
                    schedule_value: None,
                    watch_path: None,
                    return_type,
                    body,
                    span: sp,
                })
            }
            "schedule" => {
                self.expect(TokenKind::LParen, "'('")?;
                let sk = self.expect_ident("'every' or 'cron'")?;
                self.expect(TokenKind::Colon, "':'")?;
                let value = match sk.as_str() {
                    "every" => {
                        if self.at(TokenKind::Duration) {
                            let t = self.advance();
                            if let TokenValue::Duration(c, u) = &t.value {
                                Expr::new(ExprKind::Literal(Lit::Duration(*c, u.clone())), t.span)
                            } else {
                                unreachable!()
                            }
                        } else {
                            let d = Self::describe(self.cur());
                            return self.error(
                                format!("schedule 'every' expects a duration, found {d}"),
                                self.cur_span(),
                                "",
                            );
                        }
                    }
                    "cron" => self.parse_string_expr()?,
                    other => {
                        let hint = suggest(other, ["every", "cron"]);
                        return self.error(
                            format!("schedule expects 'every' or 'cron', found '{other}'"),
                            sp,
                            &hint,
                        );
                    }
                };
                self.expect(TokenKind::RParen, "')'")?;
                let body = self.parse_block()?;
                Ok(OnDecl {
                    kind: HandlerKind::Schedule,
                    param: None,
                    schedule_kind: sk,
                    schedule_value: Some(value),
                    watch_path: None,
                    return_type: None,
                    body,
                    span: sp,
                })
            }
            "file" => {
                let param = self.parse_single_param()?;
                self.expect_kw("in")?;
                let path = self.parse_string_expr()?;
                let body = self.parse_block()?;
                Ok(OnDecl {
                    kind: HandlerKind::File,
                    param: Some(param),
                    schedule_kind: String::new(),
                    schedule_value: None,
                    watch_path: Some(path),
                    return_type: None,
                    body,
                    span: sp,
                })
            }
            other => {
                let hint = suggest(other, ["start", "message", "schedule", "file"]);
                self.error(
                    format!("unknown handler '{other}' (use start/message/schedule/file)"),
                    sp,
                    &hint,
                )
            }
        }
    }

    fn parse_single_param(&mut self) -> PResult<Param> {
        self.expect(TokenKind::LParen, "'('")?;
        let psp = self.cur_span();
        let name = self.expect_ident("a parameter name")?;
        self.expect(TokenKind::Colon, "':'")?;
        let ty = self.parse_type()?;
        self.expect(TokenKind::RParen, "')'")?;
        Ok(Param {
            name,
            ty: Some(ty),
            default: None,
            span: psp,
        })
    }

    // ---- statements ----

    fn parse_block(&mut self) -> PResult<Block> {
        let sp = self.cur_span();
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut stmts = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(Block { stmts, span: sp })
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        let sp = self.cur_span();
        if self.at(TokenKind::LBrace) {
            return Ok(Stmt {
                kind: StmtKind::Block(self.parse_block()?),
                span: sp,
            });
        }
        if let Some(w) = self.cur().keyword_word() {
            match w {
                "let" | "var" => return self.parse_binding(),
                "if" => return self.parse_if(),
                "for" => return self.parse_for(),
                "while" => {
                    self.advance();
                    let cond = self.parse_expr_nostruct()?;
                    let body = self.parse_block()?;
                    return Ok(Stmt {
                        kind: StmtKind::While { cond, body },
                        span: sp,
                    });
                }
                "repeat" => {
                    self.advance();
                    let count = self.parse_expr_nostruct()?;
                    let body = self.parse_block()?;
                    return Ok(Stmt {
                        kind: StmtKind::Repeat { count, body },
                        span: sp,
                    });
                }
                "return" => {
                    self.advance();
                    let value = if self.at_stmt_end() {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };
                    return Ok(Stmt {
                        kind: StmtKind::Return(value),
                        span: sp,
                    });
                }
                "break" => {
                    self.advance();
                    return Ok(Stmt {
                        kind: StmtKind::Break,
                        span: sp,
                    });
                }
                "continue" => {
                    self.advance();
                    return Ok(Stmt {
                        kind: StmtKind::Continue,
                        span: sp,
                    });
                }
                "try" => return self.parse_try(),
                "throw" => {
                    self.advance();
                    let v = self.parse_expr()?;
                    return Ok(Stmt {
                        kind: StmtKind::Throw(v),
                        span: sp,
                    });
                }
                "remember" => return self.parse_remember(),
                "forget" => {
                    self.advance();
                    let v = self.parse_expr()?;
                    return Ok(Stmt {
                        kind: StmtKind::Forget(v),
                        span: sp,
                    });
                }
                "reply" => {
                    self.advance();
                    let v = self.parse_expr()?;
                    return Ok(Stmt {
                        kind: StmtKind::Reply(v),
                        span: sp,
                    });
                }
                "emit" => {
                    self.advance();
                    let v = self.parse_expr()?;
                    return Ok(Stmt {
                        kind: StmtKind::Emit(v),
                        span: sp,
                    });
                }
                "halt" => {
                    self.advance();
                    let v = self.parse_expr()?;
                    return Ok(Stmt {
                        kind: StmtKind::Halt(v),
                        span: sp,
                    });
                }
                _ => {}
            }
        }
        // expression / assignment
        let expr = self.parse_expr()?;
        if let Some(op) = assign_op(self.cur().kind) {
            self.advance();
            let value = self.parse_expr()?;
            if !matches!(
                expr.kind,
                ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. }
            ) {
                return self.error(
                    format!("the left side of '{op}' must be a variable, field, or index"),
                    expr.span.clone(),
                    "",
                );
            }
            return Ok(Stmt {
                kind: StmtKind::Assign {
                    target: expr,
                    op: op.to_string(),
                    value,
                },
                span: sp,
            });
        }
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span: sp,
        })
    }

    fn at_stmt_end(&self) -> bool {
        matches!(
            self.cur().kind,
            TokenKind::Newline | TokenKind::Semi | TokenKind::RBrace | TokenKind::Eof
        )
    }

    fn parse_binding(&mut self) -> PResult<Stmt> {
        let sp = self.cur_span();
        let mutable = self.is_kw("var");
        self.advance(); // let/var
        let name = self.expect_ident("a variable name")?;
        let ty = if self.at(TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(TokenKind::Assign, "'='")?;
        let value = self.parse_expr()?;
        Ok(Stmt {
            kind: StmtKind::Bind {
                name,
                ty,
                value,
                mutable,
            },
            span: sp,
        })
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        let sp = self.cur_span();
        self.expect_kw("if")?;
        let mut branches = Vec::new();
        let cond = self.parse_expr_nostruct()?;
        let block = self.parse_block()?;
        branches.push((cond, block));
        let mut else_block = None;
        while self.is_kw("else") {
            self.advance();
            if self.is_kw("if") {
                self.advance();
                let c = self.parse_expr_nostruct()?;
                let b = self.parse_block()?;
                branches.push((c, b));
            } else {
                else_block = Some(self.parse_block()?);
                break;
            }
        }
        Ok(Stmt {
            kind: StmtKind::If {
                branches,
                else_block,
            },
            span: sp,
        })
    }

    fn parse_for(&mut self) -> PResult<Stmt> {
        let sp = self.cur_span();
        self.expect_kw("for")?;
        let var = self.expect_ident("a loop variable name")?;
        self.expect_kw("in")?;
        let iter = self.parse_expr_nostruct()?;
        let body = self.parse_block()?;
        Ok(Stmt {
            kind: StmtKind::For { var, iter, body },
            span: sp,
        })
    }

    fn parse_try(&mut self) -> PResult<Stmt> {
        let sp = self.cur_span();
        self.expect_kw("try")?;
        let body = self.parse_block()?;
        self.expect_kw("catch")?;
        let catch_name = self.expect_ident("an error variable name")?;
        let catch_block = self.parse_block()?;
        Ok(Stmt {
            kind: StmtKind::Try {
                body,
                catch_name,
                catch_block,
            },
            span: sp,
        })
    }

    fn parse_remember(&mut self) -> PResult<Stmt> {
        let sp = self.cur_span();
        self.expect_kw("remember")?;
        enum Target {
            Ident(String),
            Str(Expr),
        }
        let target = if matches!(self.cur().kind, TokenKind::Str | TokenKind::StrStart) {
            Target::Str(self.parse_string_expr()?)
        } else if self.at(TokenKind::Ident) {
            Target::Ident(self.expect_ident("an identifier")?)
        } else {
            let d = Self::describe(self.cur());
            return self.error(
                format!("remember expects an identifier or a string key, found {d}"),
                self.cur_span(),
                "",
            );
        };
        if self.at(TokenKind::Assign) {
            self.advance();
            let value = self.parse_expr()?;
            let key = match target {
                Target::Ident(n) => RememberKey::Ident(n),
                Target::Str(e) => RememberKey::Expr(e),
            };
            Ok(Stmt {
                kind: StmtKind::Remember {
                    key: Some(key),
                    value,
                    auto_key: false,
                },
                span: sp,
            })
        } else {
            let value = match target {
                Target::Str(e) => e,
                Target::Ident(n) => Expr::new(ExprKind::Literal(Lit::Str(n)), sp.clone()),
            };
            Ok(Stmt {
                kind: StmtKind::Remember {
                    key: None,
                    value,
                    auto_key: true,
                },
                span: sp,
            })
        }
    }

    // ---- expression precedence ladder ----

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_or()
    }

    fn parse_expr_nostruct(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let r = self.parse_or();
        self.no_struct = saved;
        r
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut left = self.parse_and()?;
        while self.is_kw("or") {
            let sp = self.advance().span;
            let right = self.parse_and()?;
            left = bin("or", left, right, sp);
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut left = self.parse_cmp()?;
        while self.is_kw("and") {
            let sp = self.advance().span;
            let right = self.parse_cmp()?;
            left = bin("and", left, right, sp);
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> PResult<Expr> {
        let mut left = self.parse_range()?;
        while let Some(op) = cmp_op(self.cur().kind) {
            let sp = self.advance().span;
            let right = self.parse_range()?;
            left = bin(op, left, right, sp);
        }
        Ok(left)
    }

    fn parse_range(&mut self) -> PResult<Expr> {
        let left = self.parse_pipe()?;
        if matches!(self.cur().kind, TokenKind::Range | TokenKind::RangeEq) {
            let inclusive = self.at(TokenKind::RangeEq);
            let sp = self.advance().span;
            let hi = self.parse_pipe()?;
            return Ok(Expr::new(
                ExprKind::Range {
                    lo: Box::new(left),
                    hi: Box::new(hi),
                    inclusive,
                },
                sp,
            ));
        }
        Ok(left)
    }

    fn parse_pipe(&mut self) -> PResult<Expr> {
        let mut left = self.parse_add()?;
        while self.at(TokenKind::Pipe) {
            let sp = self.advance().span;
            let right = self.parse_add()?;
            left = bin("|>", left, right, sp);
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> PResult<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.cur().kind {
                TokenKind::Plus => "+",
                TokenKind::Minus => "-",
                _ => break,
            };
            let sp = self.advance().span;
            let right = self.parse_mul()?;
            left = bin(op, left, right, sp);
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> PResult<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.cur().kind {
                TokenKind::Star => "*",
                TokenKind::Slash => "/",
                TokenKind::Percent => "%",
                _ => break,
            };
            let sp = self.advance().span;
            let right = self.parse_unary()?;
            left = bin(op, left, right, sp);
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if self.is_kw("not") {
            let sp = self.advance().span;
            let operand = self.parse_unary()?;
            return Ok(Expr::new(
                ExprKind::UnOp {
                    op: "not".into(),
                    operand: Box::new(operand),
                },
                sp,
            ));
        }
        if self.at(TokenKind::Minus) {
            let sp = self.advance().span;
            let operand = self.parse_unary()?;
            return Ok(Expr::new(
                ExprKind::UnOp {
                    op: "-".into(),
                    operand: Box::new(operand),
                },
                sp,
            ));
        }
        self.parse_coalesce()
    }

    fn parse_coalesce(&mut self) -> PResult<Expr> {
        let mut left = self.parse_postfix()?;
        while self.at(TokenKind::Coalesce) {
            let sp = self.advance().span;
            let right = self.parse_postfix()?;
            left = bin("??", left, right, sp);
        }
        Ok(left)
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.cur().kind {
                TokenKind::Dot => {
                    let sp = self.advance().span;
                    let name = self.expect_ident("a member name")?;
                    expr = Expr::new(
                        ExprKind::Member {
                            obj: Box::new(expr),
                            name,
                            optional: false,
                        },
                        sp,
                    );
                }
                TokenKind::QDot => {
                    let sp = self.advance().span;
                    let name = self.expect_ident("a member name")?;
                    expr = Expr::new(
                        ExprKind::Member {
                            obj: Box::new(expr),
                            name,
                            optional: true,
                        },
                        sp,
                    );
                }
                TokenKind::LBrack => {
                    let sp = self.advance().span;
                    let index = self.parse_expr()?;
                    self.expect(TokenKind::RBrack, "']'")?;
                    expr = Expr::new(
                        ExprKind::Index {
                            obj: Box::new(expr),
                            index: Box::new(index),
                        },
                        sp,
                    );
                }
                TokenKind::LParen => {
                    let sp = self.cur_span();
                    let args = self.parse_call_args()?;
                    expr = Expr::new(
                        ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        sp,
                    );
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_call_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(TokenKind::LParen, "'('")?;
        let mut out = Vec::new();
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let label = if self.at(TokenKind::Ident) && self.peek(1).kind == TokenKind::Colon {
                let n = name_word(self.cur());
                self.advance(); // ident
                self.advance(); // colon
                Some(n)
            } else {
                None
            };
            let value = self.parse_expr()?;
            out.push(Arg { label, value });
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'")?;
        Ok(out)
    }

    /// A tight operand: a postfix-level atom under `no_struct` (so `delegate
    /// text` and `gen "x"` parse, and `gen "x" |> f` is `f(gen "x")`).
    fn parse_operand(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let r = self.parse_postfix();
        self.no_struct = saved;
        r
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        match self.cur().kind {
            TokenKind::Int
            | TokenKind::Float
            | TokenKind::Money
            | TokenKind::Duration
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Null => {
                let t = self.advance();
                Ok(Expr::new(ExprKind::Literal(lit_from_token(&t)), sp))
            }
            TokenKind::Str | TokenKind::RawString | TokenKind::StrStart => self.parse_string_expr(),
            TokenKind::LParen => self.parse_paren_or_lambda(),
            TokenKind::LBrack => self.parse_list_lit(),
            TokenKind::LBrace => self.parse_brace_expr(),
            TokenKind::Keyword => self.parse_keyword_primary(),
            TokenKind::Ident => {
                let name = name_word(self.cur());
                self.advance();
                if !self.no_struct && self.at(TokenKind::LBrace) {
                    self.parse_config_lit(Some(name))
                } else {
                    Ok(Expr::new(ExprKind::Ident(name), sp))
                }
            }
            _ => {
                let d = Self::describe(self.cur());
                self.error(format!("expected an expression, found {d}"), sp, "")
            }
        }
    }

    fn parse_keyword_primary(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        let w = self.cur().keyword_word().unwrap().to_string();
        match w.as_str() {
            "this" => {
                self.advance();
                Ok(Expr::new(ExprKind::This, sp))
            }
            "state" => {
                self.advance();
                Ok(Expr::new(ExprKind::Ident("state".into()), sp))
            }
            "gen" => self.parse_gen(),
            "delegate" => {
                self.advance();
                let goal = self.parse_operand()?;
                let with = self.maybe_with()?;
                Ok(Expr::new(
                    ExprKind::Delegate {
                        goal: Box::new(goal),
                        with_config: with.map(Box::new),
                    },
                    sp,
                ))
            }
            "spawn" => {
                self.advance();
                let target = self.parse_operand()?;
                Ok(Expr::new(ExprKind::Spawn(Box::new(target)), sp))
            }
            "await" => {
                self.advance();
                let fut = self.parse_operand()?;
                Ok(Expr::new(ExprKind::Await(Box::new(fut)), sp))
            }
            "recall" => {
                self.advance();
                self.expect(TokenKind::LParen, "'('")?;
                let query = self.parse_expr()?;
                self.expect(TokenKind::RParen, "')'")?;
                Ok(Expr::new(
                    ExprKind::Recall {
                        query: Box::new(query),
                        one: false,
                    },
                    sp,
                ))
            }
            "retry" => self.parse_retry(),
            "parallel" => self.parse_parallel(),
            "budget" => {
                self.advance();
                let args = self.parse_call_args()?;
                let body = self.parse_block()?;
                Ok(Expr::new(ExprKind::Budget { args, body }, sp))
            }
            "match" => self.parse_match(),
            other => self.error(format!("'{other}' cannot start an expression"), sp, ""),
        }
    }

    fn parse_gen(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect_kw("gen")?;
        let as_type = if self.is_kw("as") {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let leading = self.maybe_with()?;
        let prompt = self.parse_operand()?;
        let trailing = self.maybe_with()?;
        if leading.is_some() && trailing.is_some() {
            return self.error(
                "gen takes a single with { … } clause — put it either before or after the prompt, not both",
                sp,
                "",
            );
        }
        let with_config = leading.or(trailing);
        Ok(Expr::new(
            ExprKind::Gen {
                as_type,
                prompt: Box::new(prompt),
                with_config: with_config.map(Box::new),
            },
            sp,
        ))
    }

    fn maybe_with(&mut self) -> PResult<Option<Expr>> {
        if self.is_kw("with") {
            self.advance();
            Ok(Some(self.parse_config_lit(None)?))
        } else {
            Ok(None)
        }
    }

    fn parse_retry(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect_kw("retry")?;
        self.expect(TokenKind::LParen, "'('")?;
        let max = self.parse_expr()?;
        self.expect(TokenKind::RParen, "')'")?;
        let body = self.parse_block()?;
        self.expect_kw("until")?;
        if !(self.at(TokenKind::LParen) && self.is_lambda_ahead()) {
            return self.error(
                "'until' requires a lambda, e.g. (r) => r.valid",
                self.cur_span(),
                "",
            );
        }
        let until = self.parse_lambda()?;
        Ok(Expr::new(
            ExprKind::Retry {
                max: Box::new(max),
                body,
                until: Box::new(until),
            },
            sp,
        ))
    }

    fn parse_parallel(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect_kw("parallel")?;
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut branches = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let name = self.expect_ident("a branch label")?;
            self.expect(TokenKind::Colon, "':'")?;
            let value = self.parse_expr()?;
            branches.push(FieldInit { name, value });
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(Expr::new(ExprKind::Parallel(branches), sp))
    }

    fn parse_match(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect_kw("match")?;
        let subject = self.parse_expr_nostruct()?;
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut arms = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let pattern = self.parse_pattern()?;
            self.expect(TokenKind::FatArrow, "'=>'")?;
            let body = if self.at(TokenKind::LBrace) {
                Expr::new(ExprKind::Block(self.parse_block()?), self.cur_span())
            } else {
                self.parse_expr()?
            };
            arms.push(MatchArm {
                pattern,
                body: Box::new(body),
            });
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(Expr::new(
            ExprKind::Match {
                subject: Box::new(subject),
                arms,
            },
            sp,
        ))
    }

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        let sp = self.cur_span();
        if self.at(TokenKind::Ident) {
            let name = name_word(self.cur());
            if name == "_" {
                self.advance();
                return Ok(Pattern {
                    kind: PatternKind::Wildcard,
                    span: sp,
                });
            }
            self.advance();
            if self.at(TokenKind::LParen) {
                self.advance();
                let mut binds = Vec::new();
                binds.push(self.expect_ident("a binding name")?);
                while self.at(TokenKind::Comma) {
                    self.advance();
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                    binds.push(self.expect_ident("a binding name")?);
                }
                self.expect(TokenKind::RParen, "')'")?;
                return Ok(Pattern {
                    kind: PatternKind::Enum { name, binds },
                    span: sp,
                });
            }
            return Ok(Pattern {
                kind: PatternKind::Ident(name),
                span: sp,
            });
        }
        if matches!(
            self.cur().kind,
            TokenKind::Int
                | TokenKind::Float
                | TokenKind::Str
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Null
        ) {
            let lit = self.parse_primary()?;
            return Ok(Pattern {
                kind: PatternKind::Literal(lit),
                span: sp,
            });
        }
        let d = Self::describe(self.cur());
        self.error(format!("expected a match pattern, found {d}"), sp, "")
    }

    fn parse_paren_or_lambda(&mut self) -> PResult<Expr> {
        if self.is_lambda_ahead() {
            return self.parse_lambda();
        }
        self.expect(TokenKind::LParen, "'('")?;
        let saved = self.no_struct;
        self.no_struct = false;
        self.skip_newlines();
        let e = self.parse_expr()?;
        self.skip_newlines();
        self.expect(TokenKind::RParen, "')'")?;
        self.no_struct = saved;
        Ok(e)
    }

    fn is_lambda_ahead(&self) -> bool {
        // Scan from a `(`, track paren depth; when it returns to 0 after the
        // matching `)`, skip NEWLINEs and check for `=>`.
        let mut idx = self.i;
        let mut depth = 0i32;
        loop {
            let t = match self.toks.get(idx) {
                None => return false,
                Some(t) => t,
            };
            match t.kind {
                TokenKind::Eof => return false,
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        let mut j = idx + 1;
                        while self.toks.get(j).map(|t| t.kind) == Some(TokenKind::Newline) {
                            j += 1;
                        }
                        return self.toks.get(j).map(|t| t.kind) == Some(TokenKind::FatArrow);
                    }
                }
                _ => {}
            }
            idx += 1;
        }
    }

    fn parse_lambda(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect(TokenKind::LParen, "'('")?;
        let mut params = Vec::new();
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let psp = self.cur_span();
            if self.is_kw("this") {
                self.advance();
                params.push(Param {
                    name: "this".into(),
                    ty: None,
                    default: None,
                    span: psp,
                });
            } else {
                let name = self.expect_ident("a parameter name")?;
                let ty = if self.at(TokenKind::Colon) {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(Param {
                    name,
                    ty,
                    default: None,
                    span: psp,
                });
            }
            if self.at(TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'")?;
        self.expect(TokenKind::FatArrow, "'=>'")?;
        let body = if self.at(TokenKind::LBrace) {
            Expr::new(ExprKind::Block(self.parse_block()?), self.cur_span())
        } else {
            self.parse_expr()?
        };
        Ok(Expr::new(
            ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            sp,
        ))
    }

    fn parse_list_lit(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect(TokenKind::LBrack, "'['")?;
        let mut items = Vec::new();
        self.skip_newlines();
        while !self.at(TokenKind::RBrack) && !self.at(TokenKind::Eof) {
            items.push(self.parse_expr()?);
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RBrack, "']'")?;
        Ok(Expr::new(ExprKind::ListLit(items), sp))
    }

    fn parse_brace_expr(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        let nxt = self.peek(1).kind;
        let nxt2 = self.peek(2).kind;
        if nxt == TokenKind::RBrace {
            return self.error(
                "'{}' is an empty block, not a map — use '{:}' for an empty map",
                sp,
                "",
            );
        }
        if nxt == TokenKind::Colon && nxt2 == TokenKind::RBrace {
            self.advance(); // {
            self.advance(); // :
            self.advance(); // }
            return Ok(Expr::new(ExprKind::MapLit(Vec::new()), sp));
        }
        if nxt == TokenKind::Ident && nxt2 == TokenKind::Colon {
            return self.parse_config_lit(None);
        }
        self.parse_map_lit()
    }

    fn parse_map_lit(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut entries = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let key = self.parse_expr()?;
            self.expect(TokenKind::Colon, "':'")?;
            let value = self.parse_expr()?;
            entries.push((key, value));
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(Expr::new(ExprKind::MapLit(entries), sp))
    }

    fn parse_config_lit(&mut self, type_name: Option<String>) -> PResult<Expr> {
        let sp = self.cur_span();
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut fields = Vec::new();
        self.skip_seps();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if !matches!(self.cur().kind, TokenKind::Ident | TokenKind::Keyword) {
                let d = Self::describe(self.cur());
                return self.error(
                    format!("expected a field name, found {d}"),
                    self.cur_span(),
                    "",
                );
            }
            let name = name_word(self.cur());
            self.advance();
            self.expect(TokenKind::Colon, "':'")?;
            let value = self.parse_expr()?;
            fields.push(FieldInit { name, value });
            self.skip_seps();
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(Expr::new(ExprKind::ConfigLit { type_name, fields }, sp))
    }

    fn parse_string_expr(&mut self) -> PResult<Expr> {
        let sp = self.cur_span();
        match self.cur().kind {
            TokenKind::Str => {
                let t = self.advance();
                let s = match t.value {
                    TokenValue::Str(s) => s,
                    _ => String::new(),
                };
                Ok(Expr::new(ExprKind::Literal(Lit::Str(s)), sp))
            }
            TokenKind::RawString => {
                let t = self.advance();
                let s = match t.value {
                    TokenValue::Str(s) => s,
                    _ => String::new(),
                };
                Ok(Expr::new(ExprKind::Literal(Lit::RawStr(s)), sp))
            }
            TokenKind::StrStart => {
                self.advance();
                let mut parts = Vec::new();
                loop {
                    match self.cur().kind {
                        TokenKind::StrChunk => {
                            let t = self.advance();
                            if let TokenValue::Str(s) = t.value {
                                parts.push(InterpPart::Chunk(s));
                            }
                        }
                        TokenKind::InterpStart => {
                            self.advance();
                            let e = self.parse_expr()?;
                            self.expect(TokenKind::InterpEnd, "'}'")?;
                            parts.push(InterpPart::Expr(e));
                        }
                        TokenKind::StrEnd => {
                            self.advance();
                            break;
                        }
                        TokenKind::Eof => {
                            return self.error("unterminated interpolated string", sp, "");
                        }
                        _ => {
                            let d = Self::describe(self.cur());
                            return self.error(
                                format!("unexpected token in string: {d}"),
                                self.cur_span(),
                                "",
                            );
                        }
                    }
                }
                Ok(Expr::new(ExprKind::InterpString(parts), sp))
            }
            _ => {
                let d = Self::describe(self.cur());
                self.error(format!("expected a string, found {d}"), sp, "")
            }
        }
    }
}

// ---- free helpers ----

fn bin(op: &str, left: Expr, right: Expr, span: Span) -> Expr {
    Expr::new(
        ExprKind::BinOp {
            op: op.to_string(),
            left: Box::new(left),
            right: Box::new(right),
        },
        span,
    )
}

fn cmp_op(kind: TokenKind) -> Option<&'static str> {
    match kind {
        TokenKind::Eq => Some("=="),
        TokenKind::Ne => Some("!="),
        TokenKind::Lt => Some("<"),
        TokenKind::Le => Some("<="),
        TokenKind::Gt => Some(">"),
        TokenKind::Ge => Some(">="),
        _ => None,
    }
}

fn assign_op(kind: TokenKind) -> Option<&'static str> {
    match kind {
        TokenKind::Assign => Some("="),
        TokenKind::PlusEq => Some("+="),
        TokenKind::MinusEq => Some("-="),
        TokenKind::StarEq => Some("*="),
        TokenKind::SlashEq => Some("/="),
        _ => None,
    }
}

fn name_word(t: &Token) -> String {
    match &t.value {
        TokenValue::Str(s) => s.clone(),
        _ => t.text.clone(),
    }
}

fn lit_from_token(t: &Token) -> Lit {
    match &t.value {
        TokenValue::Int(i) => Lit::Int(*i),
        TokenValue::Float(f) => Lit::Float(*f),
        TokenValue::Money(s) => Lit::Money(s.clone()),
        TokenValue::Duration(c, u) => Lit::Duration(*c, u.clone()),
        TokenValue::Bool(b) => Lit::Bool(*b),
        TokenValue::None if t.kind == TokenKind::Null => Lit::Null,
        TokenValue::Str(s) => Lit::Str(s.clone()),
        _ => Lit::Null,
    }
}
