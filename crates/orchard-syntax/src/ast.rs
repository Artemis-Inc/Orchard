//! The Orchard 3.0 AST. Ports v2's `ast.py`.
//!
//! Every node carries a [`Span`] (skipped from serialization so golden AST
//! dumps omit spans, matching v2's `ast.dump`). Golden dumps use
//! `serde_json::to_string_pretty` over these types.
//!
//! AST enums hold variants of varying size; the tree is built once and
//! pattern-matched (recursive children are already boxed in [`Expr`]), so we
//! accept the size disparity rather than box every leaf.
#![allow(clippy::large_enum_variant)]

use crate::span::Span;
use serde::Serialize;

// ---- literals ----

/// A literal value. `kind` strings in the IR are derived from the variant.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    RawStr(String),
    Bool(bool),
    Null,
    /// `(count, unit)`.
    Duration(i64, String),
    /// The exact amount string (e.g. `"0.50"`).
    Money(String),
}

impl Lit {
    /// The IR `type` tag for this literal.
    pub fn ir_type(&self) -> &'static str {
        match self {
            Lit::Int(_) => "int",
            Lit::Float(_) => "float",
            Lit::Str(_) => "str",
            Lit::RawStr(_) => "rawstr",
            Lit::Bool(_) => "bool",
            Lit::Null => "null",
            Lit::Duration(_, _) => "duration",
            Lit::Money(_) => "money",
        }
    }
}

// ---- expressions ----

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Expr {
    pub kind: ExprKind,
    #[serde(skip)]
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum ExprKind {
    Literal(Lit),
    /// Interpolated string: text chunks interleaved with sub-expressions.
    InterpString(Vec<InterpPart>),
    Ident(String),
    This,
    BinOp {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    UnOp {
        op: String,
        operand: Box<Expr>,
    },
    Member {
        obj: Box<Expr>,
        name: String,
        optional: bool,
    },
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    ListLit(Vec<Expr>),
    MapLit(Vec<(Expr, Expr)>),
    ConfigLit {
        type_name: Option<String>,
        fields: Vec<FieldInit>,
    },
    Lambda {
        params: Vec<Param>,
        body: Box<Expr>,
    },
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },
    Gen {
        as_type: Option<TypeRef>,
        prompt: Box<Expr>,
        with_config: Option<Box<Expr>>,
    },
    Delegate {
        goal: Box<Expr>,
        with_config: Option<Box<Expr>>,
    },
    Spawn(Box<Expr>),
    Await(Box<Expr>),
    Recall {
        query: Box<Expr>,
        one: bool,
    },
    Retry {
        max: Box<Expr>,
        body: Block,
        until: Box<Expr>,
    },
    Parallel(Vec<FieldInit>),
    Budget {
        args: Vec<Arg>,
        body: Block,
    },
    /// A brace block used as a value (lambda/match-arm bodies, retry/budget).
    Block(Block),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum InterpPart {
    Chunk(String),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Arg {
    pub label: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Box<Expr>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pattern {
    pub kind: PatternKind,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum PatternKind {
    Wildcard,
    Ident(String),
    Enum { name: String, binds: Vec<String> },
    Literal(Expr),
}

// ---- types ----

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TypeRef {
    pub name: String,
    pub args: Vec<TypeRef>,
    pub optional: bool,
    #[serde(skip)]
    pub span: Span,
}

// ---- statements ----

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stmt {
    pub kind: StmtKind,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum StmtKind {
    Block(Block),
    Bind {
        name: String,
        ty: Option<TypeRef>,
        value: Expr,
        mutable: bool,
    },
    Assign {
        target: Expr,
        op: String,
        value: Expr,
    },
    If {
        branches: Vec<(Expr, Block)>,
        else_block: Option<Block>,
    },
    For {
        var: String,
        iter: Expr,
        body: Block,
    },
    While {
        cond: Expr,
        body: Block,
    },
    Repeat {
        count: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Try {
        body: Block,
        catch_name: String,
        catch_block: Block,
    },
    Throw(Expr),
    Remember {
        key: Option<RememberKey>,
        value: Expr,
        auto_key: bool,
    },
    Forget(Expr),
    Reply(Expr),
    Emit(Expr),
    Halt(Expr),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum RememberKey {
    Ident(String),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    #[serde(skip)]
    pub span: Span,
}

// ---- declarations ----

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Program {
    pub pragma: Option<String>,
    pub items: Vec<TopItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum TopItem {
    Agent(AgentDecl),
    Type(TypeDecl),
    Enum(EnumDecl),
    Fn(Callable),
    Use(UseDecl),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentDecl {
    pub name: String,
    pub members: Vec<AgentMember>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum AgentMember {
    Config(ConfigBlock),
    Use(UseDecl),
    State(StateDecl),
    Type(TypeDecl),
    Enum(EnumDecl),
    Fn(Callable),
    Tool(Callable),
    Skill(Callable),
    On(OnDecl),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConfigBlock {
    pub name: String,
    pub settings: Vec<Setting>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Setting {
    KeyValue { key: String, value: Expr },
    Block(ConfigBlock),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum UseForm {
    Pack,
    Mcp,
    Import,
    Env,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UseDecl {
    pub form: UseForm,
    pub name: String,
    pub target: Option<Expr>,
    pub options: Option<Vec<FieldInit>>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateDecl {
    pub name: String,
    pub ty: TypeRef,
    pub default: Option<Expr>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
    pub default: Option<Expr>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TypeDecl {
    pub name: String,
    pub fields: Vec<Field>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeRef>,
    pub default: Option<Expr>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Variant {
    pub name: String,
    pub params: Vec<Param>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<Variant>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Annotation {
    pub name: String,
    pub args: Vec<Arg>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum CallableKind {
    Fn,
    Tool,
    Skill,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Callable {
    pub kind: CallableKind,
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeRef>,
    pub body: Block,
    pub annotations: Vec<Annotation>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum HandlerKind {
    Start,
    Message,
    Schedule,
    File,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OnDecl {
    pub kind: HandlerKind,
    pub param: Option<Param>,
    /// For `schedule`: `"every"` or `"cron"`.
    pub schedule_kind: String,
    pub schedule_value: Option<Expr>,
    pub watch_path: Option<Expr>,
    pub return_type: Option<TypeRef>,
    pub body: Block,
    #[serde(skip)]
    pub span: Span,
}

impl Program {
    /// A deterministic, span-free dump for golden tests (pretty JSON).
    pub fn dump(&self) -> String {
        serde_json::to_string_pretty(self).expect("AST serializes")
    }
}
