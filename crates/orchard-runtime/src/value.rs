//! The runtime [`Value`] model. Ports v2's interpreter value classes.
//!
//! Key fidelity points: `Money` is exact base-10 (`rust_decimal`), `bool` is
//! never a number, truthiness/equality/text-conversion match v2 exactly.
//! `Value` is `Send + Sync` so it can cross task boundaries (concurrency, P9).

use indexmap::IndexMap;
use rust_decimal::Decimal;
use serde_json::Value as Json;
use std::sync::{Arc, Mutex};

/// An exact monetary amount. Equality is by value; [`Money::text`] preserves the
/// original textual form (so `$5.50` round-trips, not `$5.5`).
#[derive(Clone, Debug)]
pub struct Money {
    pub amount: Decimal,
    /// The amount string without the `$` (e.g. `"5.50"`).
    pub text: String,
}

impl Money {
    pub fn from_text(s: &str) -> Money {
        let amount = s.parse::<Decimal>().unwrap_or_default();
        Money {
            amount,
            text: s.to_string(),
        }
    }
    pub fn from_decimal(d: Decimal) -> Money {
        Money {
            amount: d,
            text: d.to_string(),
        }
    }
    /// `"$<text>"`.
    pub fn display(&self) -> String {
        format!("${}", self.text)
    }
}

impl PartialEq for Money {
    fn eq(&self, other: &Self) -> bool {
        self.amount == other.amount
    }
}

/// A duration literal value.
#[derive(Clone, Debug)]
pub struct Duration {
    pub count: i64,
    pub unit: String,
}

impl Duration {
    pub fn new(count: i64, unit: impl Into<String>) -> Duration {
        Duration {
            count,
            unit: unit.into(),
        }
    }
    pub fn canonical(&self) -> String {
        format!("{}{}", self.count, self.unit)
    }
    pub fn seconds(&self) -> f64 {
        let factor = match self.unit.as_str() {
            "ms" => 0.001,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            "d" => 86400.0,
            "w" => 604800.0,
            _ => 1.0,
        };
        self.count as f64 * factor
    }
    /// Parse a duration from text like `"30m"` or `"90"` (bare → seconds).
    pub fn parse(text: &str) -> Duration {
        let t = text.trim();
        for unit in ["ms", "s", "m", "h", "d", "w"] {
            if let Some(num) = t.strip_suffix(unit) {
                if let Ok(n) = num.trim().parse::<f64>() {
                    return Duration::new(n as i64, unit);
                }
            }
        }
        Duration::new(0, "s")
    }
}

impl PartialEq for Duration {
    fn eq(&self, other: &Self) -> bool {
        self.seconds() == other.seconds()
    }
}

/// A lexical scope: a string→Value table chained to a parent. Interior mutability
/// (via `Mutex`) makes closures and concurrent tasks share scopes safely.
#[derive(Debug)]
pub struct Scope {
    vars: Mutex<IndexMap<String, Value>>,
    parent: Option<Env>,
}

/// A reference-counted scope chain.
pub type Env = Arc<Scope>;

impl Scope {
    pub fn root() -> Env {
        Arc::new(Scope {
            vars: Mutex::new(IndexMap::new()),
            parent: None,
        })
    }
    pub fn child(self: &Env) -> Env {
        Arc::new(Scope {
            vars: Mutex::new(IndexMap::new()),
            parent: Some(self.clone()),
        })
    }
    pub fn define(&self, name: &str, value: Value) {
        self.vars.lock().unwrap().insert(name.to_string(), value);
    }
    pub fn has(&self, name: &str) -> bool {
        if self.vars.lock().unwrap().contains_key(name) {
            return true;
        }
        self.parent.as_ref().map(|p| p.has(name)).unwrap_or(false)
    }
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.lock().unwrap().get(name) {
            return Some(v.clone());
        }
        self.parent.as_ref().and_then(|p| p.get(name))
    }
    /// Assign to the nearest scope that already defines `name`; returns whether
    /// it was found.
    pub fn assign(&self, name: &str, value: Value) -> bool {
        {
            let mut vars = self.vars.lock().unwrap();
            if vars.contains_key(name) {
                vars.insert(name.to_string(), value);
                return true;
            }
        }
        self.parent
            .as_ref()
            .map(|p| p.assign(name, value))
            .unwrap_or(false)
    }
}

/// A lambda closure: the lowered lambda IR node plus its captured environment.
#[derive(Clone, Debug)]
pub struct Closure {
    pub node: Json,
    pub env: Env,
}

/// A handle to a spawned computation (concurrency, wired in P9).
#[derive(Clone, Debug)]
pub struct Future {
    pub id: u64,
}

/// A runtime value.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Money(Money),
    Duration(Duration),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Map(IndexMap<String, Value>),
    Record {
        type_name: Option<String>,
        fields: IndexMap<String, Value>,
    },
    Enum {
        enum_name: String,
        variant: String,
        payload: Vec<Value>,
    },
    /// An integer range value (`lo..hi`, exclusive end).
    Range {
        start: i64,
        end: i64,
    },
    Closure(Arc<Closure>),
    /// A reference to a named callable (`fn`/`skill`/`tool`/`backend`).
    FunctionRef {
        kind: String,
        name: String,
    },
    /// A reference to an enum *type* (e.g. bare `Severity`).
    EnumTypeRef {
        enum_name: String,
    },
    /// The `this` sentinel.
    This,
    Future(Future),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }

    /// Truthiness (v2 `_truthy`): null/false/0/0.0/empty-str/list/map → false;
    /// everything else (incl. Money/Duration/Enum/Record) → true.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Map(m) => !m.is_empty(),
            Value::Record { fields, .. } => !fields.is_empty(),
            Value::Range { start, end } => start < end,
            _ => true,
        }
    }

    /// Deterministic text conversion (SPEC §4.7).
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_float(*f),
            Value::Str(s) => s.clone(),
            Value::Money(m) => m.display(),
            Value::Duration(d) => d.canonical(),
            Value::Bytes(b) => String::from_utf8_lossy(b).to_string(),
            Value::Enum {
                variant, payload, ..
            } => {
                if payload.is_empty() {
                    variant.clone()
                } else {
                    let ps: Vec<String> = payload.iter().map(|p| p.to_text()).collect();
                    format!("{}({})", variant, ps.join(", "))
                }
            }
            // collections/records → compact JSON
            _ => serde_json::to_string(&self.to_jsonable()).unwrap_or_default(),
        }
    }

    /// Convert to a JSON value (the store/tool/http boundary). Money→float,
    /// Duration→canonical string, Enum→string or `{variant:[...]}`, Record→object.
    pub fn to_jsonable(&self) -> Json {
        match self {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Int(i) => Json::from(*i),
            Value::Float(f) => serde_json::Number::from_f64(*f)
                .map(Json::Number)
                .unwrap_or(Json::Null),
            Value::Str(s) => Json::String(s.clone()),
            Value::Money(m) => serde_json::Number::from_f64(m.amount.try_into().unwrap_or(0.0))
                .map(Json::Number)
                .unwrap_or(Json::Null),
            Value::Duration(d) => Json::String(d.canonical()),
            Value::Bytes(b) => Json::String(String::from_utf8_lossy(b).to_string()),
            Value::List(l) => Json::Array(l.iter().map(|v| v.to_jsonable()).collect()),
            Value::Range { start, end } => Json::Array((*start..*end).map(Json::from).collect()),
            Value::Map(m) => {
                let mut o = serde_json::Map::new();
                for (k, v) in m {
                    o.insert(k.clone(), v.to_jsonable());
                }
                Json::Object(o)
            }
            Value::Record { fields, .. } => {
                let mut o = serde_json::Map::new();
                for (k, v) in fields {
                    o.insert(k.clone(), v.to_jsonable());
                }
                Json::Object(o)
            }
            Value::Enum {
                variant, payload, ..
            } => {
                if payload.is_empty() {
                    Json::String(variant.clone())
                } else {
                    let mut o = serde_json::Map::new();
                    o.insert(
                        variant.clone(),
                        Json::Array(payload.iter().map(|p| p.to_jsonable()).collect()),
                    );
                    Json::Object(o)
                }
            }
            _ => Json::String(self.to_text()),
        }
    }

    /// Build a Value from a JSON value (tool results, decoded model JSON).
    pub fn from_json(j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Json::String(s) => Value::Str(s.clone()),
            Json::Array(a) => Value::List(a.iter().map(Value::from_json).collect()),
            Json::Object(o) => {
                let mut m = IndexMap::new();
                for (k, v) in o {
                    m.insert(k.clone(), Value::from_json(v));
                }
                Value::Map(m)
            }
        }
    }
}

/// Structural equality (v2 `_equal`): bool is never equal to a non-bool number.
pub fn equal(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        // a bool only equals a bool
        (Bool(x), Bool(y)) => x == y,
        (Bool(_), _) | (_, Bool(_)) => false,
        (Null, Null) => true,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Int(x), Float(y)) | (Float(y), Int(x)) => (*x as f64) == *y,
        (Str(x), Str(y)) => x == y,
        (Money(x), Money(y)) => x == y,
        (Duration(x), Duration(y)) => x == y,
        (Bytes(x), Bytes(y)) => x == y,
        (List(x), List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equal(a, b)),
        (Range { start: s1, end: e1 }, Range { start: s2, end: e2 }) => s1 == s2 && e1 == e2,
        (Map(x), Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).map(|w| equal(v, w)).unwrap_or(false))
        }
        (
            Record {
                type_name: t1,
                fields: f1,
            },
            Record {
                type_name: t2,
                fields: f2,
            },
        ) => {
            t1 == t2
                && f1.len() == f2.len()
                && f1
                    .iter()
                    .all(|(k, v)| f2.get(k).map(|w| equal(v, w)).unwrap_or(false))
        }
        (
            Enum {
                enum_name: e1,
                variant: v1,
                payload: p1,
            },
            Enum {
                enum_name: e2,
                variant: v2,
                payload: p2,
            },
        ) => {
            e1 == e2
                && v1 == v2
                && p1.len() == p2.len()
                && p1.iter().zip(p2).all(|(a, b)| equal(a, b))
        }
        _ => false,
    }
}

/// Format a float as Python's `str()`/`repr()` would, so `gen` prompts and
/// interpolation produce byte-identical text to v2. Python uses the shortest
/// round-tripping digits and switches to scientific notation when the decimal
/// exponent is `>= 16` or `<= -5` (otherwise fixed-point, integers keep `.0`).
fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    if f == 0.0 {
        // preserves -0.0 → "-0.0" like Python
        return format!("{f:.1}");
    }
    // Rust's `{:e}` gives the shortest mantissa and the base-10 exponent.
    let sci = format!("{f:e}"); // e.g. "1.5e16", "1e-5", "-2.3e-7"
    let (mant, exp_str) = sci.split_once('e').expect("scientific form has an 'e'");
    let exp: i32 = exp_str.parse().expect("valid exponent");
    if exp >= 16 || exp <= -5 {
        // scientific: Python pads the exponent to >=2 digits and always signs it
        format!("{mant}e{exp:+03}")
    } else if f.fract() == 0.0 {
        format!("{f:.1}") // integral float keeps a trailing ".0"
    } else {
        format!("{f}") // shortest fixed-point (matches Python in this range)
    }
}

/// Build an Error record value (`{message, kind, detail}`).
pub fn make_error(
    message: impl Into<String>,
    kind: impl Into<String>,
    detail: Option<Value>,
) -> Value {
    let mut fields = IndexMap::new();
    fields.insert("message".to_string(), Value::Str(message.into()));
    fields.insert("kind".to_string(), Value::Str(kind.into()));
    fields.insert("detail".to_string(), detail.unwrap_or(Value::Null));
    Value::Record {
        type_name: Some("Error".to_string()),
        fields,
    }
}
