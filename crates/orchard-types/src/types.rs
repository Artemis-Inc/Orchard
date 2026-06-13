//! The Orchard 3.0 type model: representations, local inference helpers,
//! structural compatibility, unification, and JSON-Schema lowering. Ports v2's
//! `types.py`.
//!
//! "Unknown / not yet inferred" is represented as `Option::None` at inference
//! sites and treated as dynamic (compatible with everything). A *missing
//! annotation* lowers to [`Type::any`] (see [`from_typeref`]).

use orchard_syntax::ast::TypeRef;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};

/// An Orchard type. Composite types compare structurally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    /// A primitive: `str int float bool null duration money bytes json any`.
    Prim(String),
    List(Box<Type>),
    Map(Box<Type>, Box<Type>),
    Optional(Box<Type>),
    /// An unresolved user type name.
    Named(String),
    Enum(EnumType),
    Record(RecordType),
    #[allow(dead_code)]
    Func(Vec<Type>, Box<Type>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumType {
    pub name: String,
    /// `(variant_name, payload_types)`; payload-less variants have an empty vec.
    pub variants: Vec<(String, Vec<Type>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordType {
    pub name: String,
    /// `(field_name, field_type, required)`.
    pub fields: Vec<(String, Type, bool)>,
}

impl Type {
    pub fn prim(name: &str) -> Type {
        Type::Prim(name.to_string())
    }
    pub fn str_() -> Type {
        Type::prim("str")
    }
    pub fn int() -> Type {
        Type::prim("int")
    }
    pub fn float() -> Type {
        Type::prim("float")
    }
    pub fn bool_() -> Type {
        Type::prim("bool")
    }
    pub fn null() -> Type {
        Type::prim("null")
    }
    pub fn duration() -> Type {
        Type::prim("duration")
    }
    pub fn money() -> Type {
        Type::prim("money")
    }
    pub fn json() -> Type {
        Type::prim("json")
    }
    pub fn any() -> Type {
        Type::prim("any")
    }

    /// `json`/`any` are dynamic supertypes.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Type::Prim(n) if n == "json" || n == "any")
    }

    pub fn nominal_name(&self) -> Option<&str> {
        match self {
            Type::Named(n) => Some(n),
            Type::Enum(e) => Some(&e.name),
            Type::Record(r) => Some(&r.name),
            _ => None,
        }
    }

    /// Human-readable rendering for diagnostics.
    pub fn display(&self) -> String {
        match self {
            Type::Prim(n) => n.clone(),
            Type::List(e) => format!("list<{}>", e.display()),
            Type::Map(k, v) => format!("map<{}, {}>", k.display(), v.display()),
            Type::Optional(i) => format!("{}?", i.display()),
            Type::Named(n) => n.clone(),
            Type::Enum(e) => e.name.clone(),
            Type::Record(r) => r.name.clone(),
            Type::Func(ps, ret) => {
                let ps: Vec<String> = ps.iter().map(|p| p.display()).collect();
                format!("fn({}) -> {}", ps.join(", "), ret.display())
            }
        }
    }
}

/// Render an optional inferred type (`None` → `"?"`).
pub fn display_opt(t: &Option<Type>) -> String {
    match t {
        Some(t) => t.display(),
        None => "?".to_string(),
    }
}

/// Whether the optionally-inferred type is dynamic (`None` counts).
pub fn is_dynamic_opt(t: &Option<Type>) -> bool {
    match t {
        None => true,
        Some(t) => t.is_dynamic(),
    }
}

pub const PRIMITIVE_NAMES: &[&str] = &[
    "str", "int", "float", "bool", "null", "duration", "money", "bytes", "json", "any",
];

/// Names a bare type reference may use without being user-declared.
pub fn is_builtin_type_name(name: &str) -> bool {
    PRIMITIVE_NAMES.contains(&name) || name == "list" || name == "map"
}

/// Lower an AST [`TypeRef`] to a [`Type`]. A missing annotation (`None`) →
/// [`Type::any`]. User type names become [`Type::Named`] (resolved later).
pub fn from_typeref(tr: Option<&TypeRef>) -> Type {
    let tr = match tr {
        None => return Type::any(),
        Some(t) => t,
    };
    let base = if PRIMITIVE_NAMES.contains(&tr.name.as_str()) {
        Type::prim(&tr.name)
    } else if tr.name == "list" {
        let elem = tr
            .args
            .first()
            .map(|a| from_typeref(Some(a)))
            .unwrap_or_else(Type::any);
        Type::List(Box::new(elem))
    } else if tr.name == "map" {
        let key = tr
            .args
            .first()
            .map(|a| from_typeref(Some(a)))
            .unwrap_or_else(Type::str_);
        let val = tr
            .args
            .get(1)
            .map(|a| from_typeref(Some(a)))
            .unwrap_or_else(Type::any);
        Type::Map(Box::new(key), Box::new(val))
    } else {
        Type::Named(tr.name.clone())
    };
    if tr.optional && !matches!(base, Type::Optional(_)) && base != Type::null() {
        Type::Optional(Box::new(base))
    } else {
        base
    }
}

/// Structural compatibility: may a value of type `src` flow into a `dst` slot?
pub fn assignable(src: &Type, dst: &Type) -> bool {
    if src.is_dynamic() || dst.is_dynamic() {
        return true;
    }
    if src == dst {
        return true;
    }
    // int widens to float (only direction).
    if matches!(src, Type::Prim(s) if s == "int") && matches!(dst, Type::Prim(d) if d == "float") {
        return true;
    }
    // null satisfies an optional or null.
    if matches!(src, Type::Prim(s) if s == "null")
        && (matches!(dst, Type::Optional(_)) || matches!(dst, Type::Prim(d) if d == "null"))
    {
        return true;
    }
    if let Type::Optional(dst_inner) = dst {
        // null already handled; unwrap an optional src and recurse.
        if let Type::Optional(src_inner) = src {
            return assignable(src_inner, dst_inner);
        }
        return assignable(src, dst_inner);
    }
    if matches!(src, Type::Optional(_)) {
        // dst is not optional (handled above) → only via dynamic, which failed.
        return false;
    }
    match (src, dst) {
        (Type::List(s), Type::List(d)) => assignable(s, d),
        (Type::Map(sk, sv), Type::Map(dk, dv)) => assignable(sk, dk) && assignable(sv, dv),
        _ => match (src.nominal_name(), dst.nominal_name()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
    }
}

/// Unify two (optionally-unknown) types: the wider, else `json`.
pub fn unify(a: Option<Type>, b: Option<Type>) -> Option<Type> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => {
            if a == b {
                Some(a)
            } else if assignable(&a, &b) {
                Some(b)
            } else if assignable(&b, &a) {
                Some(a)
            } else {
                Some(Type::json())
            }
        }
    }
}

/// Lower a [`Type`] to a JSON Schema. `env` resolves [`Type::Named`].
pub fn to_json_schema(t: &Type, env: &BTreeMap<String, Type>) -> Value {
    to_json_schema_inner(t, env, &mut HashSet::new())
}

fn prim_schema(name: &str) -> Value {
    match name {
        "str" => json!({"type": "string"}),
        "int" => json!({"type": "integer"}),
        "float" => json!({"type": "number"}),
        "bool" => json!({"type": "boolean"}),
        "null" => json!({"type": "null"}),
        // money/duration travel as canonical strings, parsed on receipt.
        "money" | "duration" | "bytes" => json!({"type": "string"}),
        // json/any unconstrained
        _ => json!({}),
    }
}

fn to_json_schema_inner(
    t: &Type,
    env: &BTreeMap<String, Type>,
    seen: &mut HashSet<String>,
) -> Value {
    match t {
        Type::Prim(n) => prim_schema(n),
        Type::List(e) => json!({"type": "array", "items": to_json_schema_inner(e, env, seen)}),
        Type::Map(_, v) => {
            json!({"type": "object", "additionalProperties": to_json_schema_inner(v, env, seen)})
        }
        Type::Optional(inner) => {
            let s = to_json_schema_inner(inner, env, seen);
            if s.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                json!({})
            } else {
                json!({"anyOf": [s, {"type": "null"}]})
            }
        }
        Type::Enum(e) => {
            if e.variants.iter().all(|(_, params)| params.is_empty()) {
                let names: Vec<Value> = e.variants.iter().map(|(n, _)| json!(n)).collect();
                json!({"type": "string", "enum": names})
            } else {
                // tagged union → validate-and-retry
                json!({})
            }
        }
        Type::Record(r) => {
            if seen.contains(&r.name) {
                return json!({});
            }
            seen.insert(r.name.clone());
            let mut props = Map::new();
            let mut required = Vec::new();
            for (fname, fty, req) in &r.fields {
                // optional fields encode optionality via `required`; unwrap.
                let lowered_ty = match fty {
                    Type::Optional(inner) => inner.as_ref(),
                    other => other,
                };
                props.insert(fname.clone(), to_json_schema_inner(lowered_ty, env, seen));
                if *req {
                    required.push(json!(fname));
                }
            }
            seen.remove(&r.name);
            let mut obj = Map::new();
            obj.insert("type".into(), json!("object"));
            obj.insert("properties".into(), Value::Object(props));
            if !required.is_empty() {
                obj.insert("required".into(), Value::Array(required));
            }
            Value::Object(obj)
        }
        Type::Named(n) => {
            if !seen.contains(n) {
                if let Some(resolved) = env.get(n) {
                    return to_json_schema_inner(resolved, env, seen);
                }
            }
            json!({})
        }
        Type::Func(_, _) => json!({}),
    }
}
