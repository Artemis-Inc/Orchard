//! `gen as T` coercion: decode-from-prose + recursive coercion of a model reply
//! into a typed [`Value`] against an [`orchard_types::Type`]. Ports v2's
//! `_coerce_to_type` / `_decode_reply` / `_coerce_value`.

use crate::value::{Duration, Money, Value};
use indexmap::IndexMap;
use orchard_types::Type;
use rust_decimal::Decimal;
use serde_json::Value as Json;
use std::str::FromStr;

/// A coercion failure (internal to `gen as T`; never escapes as-is).
pub struct CoerceError(pub String);

fn err<T>(msg: impl Into<String>) -> Result<T, CoerceError> {
    Err(CoerceError(msg.into()))
}

/// Coerce a model reply `text` into a value of `target`.
pub fn coerce_to_type(text: &str, target: &Type) -> Result<Value, CoerceError> {
    let base = match target {
        Type::Optional(inner) => inner.as_ref(),
        other => other,
    };
    let (parsed, raw) = decode_reply(text);
    // str target: use the reply verbatim
    if matches!(base, Type::Prim(p) if p == "str") {
        return match &parsed {
            Some(Json::String(s)) => Ok(Value::Str(s.clone())),
            _ => Ok(Value::Str(raw)),
        };
    }
    let parsed = match parsed {
        Some(j) => j,
        None => {
            if matches!(base, Type::Record(_) | Type::List(_) | Type::Map(_, _)) {
                let preview: String = raw.chars().take(80).collect();
                return err(format!(
                    "expected JSON for {}, got: {}",
                    base.display(),
                    if preview.is_empty() {
                        "(empty reply)".to_string()
                    } else {
                        preview
                    }
                ));
            }
            Json::String(raw)
        }
    };
    coerce_value(&parsed, target, "value")
}

fn coerce_value(j: &Json, target: &Type, _where: &str) -> Result<Value, CoerceError> {
    match target {
        Type::Optional(inner) => {
            if j.is_null() {
                Ok(Value::Null)
            } else {
                coerce_value(j, inner, _where)
            }
        }
        Type::Prim(name) => coerce_prim(j, name),
        Type::Enum(e) => coerce_enum(j, e),
        Type::Record(r) => coerce_record(j, r),
        Type::List(elem) => coerce_list(j, elem),
        Type::Map(_, val) => coerce_map(j, val),
        _ => Ok(Value::from_json(j)),
    }
}

fn coerce_prim(j: &Json, name: &str) -> Result<Value, CoerceError> {
    match name {
        "json" | "any" => Ok(Value::from_json(j)),
        "str" | "bytes" => match j {
            Json::String(s) => Ok(Value::Str(s.clone())),
            Json::Bool(b) => Ok(Value::Str(if *b { "true" } else { "false" }.into())),
            Json::Number(n) => Ok(Value::Str(n.to_string())),
            _ => err("expected a string"),
        },
        "int" => match j {
            Json::Bool(_) => err("expected an integer, got a boolean"),
            Json::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Int(i))
                } else if let Some(f) = n.as_f64() {
                    if f.fract() == 0.0 {
                        Ok(Value::Int(f as i64))
                    } else {
                        err("expected an integer, got a non-integral number")
                    }
                } else {
                    err("expected an integer")
                }
            }
            Json::String(s) => s
                .trim()
                .parse::<i64>()
                .map(Value::Int)
                .or_else(|_| {
                    s.trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|f| f.fract() == 0.0)
                        .map(|f| Value::Int(f as i64))
                        .ok_or(())
                })
                .map_err(|_| CoerceError("expected an integer".into())),
            _ => err("expected an integer"),
        },
        "float" => match j {
            Json::Bool(_) => err("expected a number, got a boolean"),
            Json::Number(n) => Ok(Value::Float(n.as_f64().unwrap_or(0.0))),
            Json::String(s) => s
                .trim()
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|_| CoerceError("expected a number".into())),
            _ => err("expected a number"),
        },
        "bool" => match j {
            Json::Bool(b) => Ok(Value::Bool(*b)),
            Json::String(s) => match s.trim().to_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(Value::Bool(true)),
                "false" | "no" | "0" => Ok(Value::Bool(false)),
                _ => err("expected a boolean"),
            },
            _ => err("expected a boolean"),
        },
        "money" => match j {
            Json::Number(n) => Ok(Value::Money(Money::from_decimal(
                Decimal::from_str(&n.to_string()).unwrap_or_default(),
            ))),
            Json::String(s) => {
                let cleaned: String = s.trim().trim_start_matches('$').replace(',', "");
                Decimal::from_str(&cleaned)
                    .map(|d| {
                        Value::Money(Money {
                            amount: d,
                            text: cleaned.clone(),
                        })
                    })
                    .map_err(|_| CoerceError("expected a money amount".into()))
            }
            _ => err("expected a money amount"),
        },
        "duration" => match j {
            Json::Number(n) => Ok(Value::Duration(Duration::new(n.as_i64().unwrap_or(0), "s"))),
            Json::String(s) => {
                let t = s.trim();
                if ["ms", "s", "m", "h", "d", "w"]
                    .iter()
                    .any(|u| t.ends_with(u))
                {
                    Ok(Value::Duration(Duration::parse(t)))
                } else if let Ok(n) = t.parse::<i64>() {
                    Ok(Value::Duration(Duration::new(n, "s")))
                } else {
                    err("expected a duration")
                }
            }
            _ => err("expected a duration"),
        },
        "null" => {
            if j.is_null() {
                Ok(Value::Null)
            } else {
                err("expected null")
            }
        }
        _ => Ok(Value::from_json(j)),
    }
}

fn coerce_enum(j: &Json, e: &orchard_types::EnumType) -> Result<Value, CoerceError> {
    let names: Vec<&str> = e.variants.iter().map(|(n, _)| n.as_str()).collect();
    let (variant, payload): (String, Vec<Value>) = match j {
        Json::String(s) => {
            let v = s.rsplit('.').next().unwrap_or(s).to_string();
            (v, vec![])
        }
        Json::Object(o) if o.len() == 1 => {
            let (k, v) = o.iter().next().unwrap();
            let payload = match v {
                Json::Array(a) => a.iter().map(Value::from_json).collect(),
                other => vec![Value::from_json(other)],
            };
            (k.clone(), payload)
        }
        _ => return err(format!("expected one of {names:?}")),
    };
    if names.contains(&variant.as_str()) {
        Ok(Value::Enum {
            enum_name: e.name.clone(),
            variant,
            payload,
        })
    } else {
        err(format!(
            "'{variant}' is not a valid variant; expected one of {names:?}"
        ))
    }
}

fn coerce_record(j: &Json, r: &orchard_types::RecordType) -> Result<Value, CoerceError> {
    let obj = match j.as_object() {
        Some(o) => o,
        None => return err(format!("expected an object for {}", r.name)),
    };
    let mut fields = IndexMap::new();
    for (fname, fty, required) in &r.fields {
        match obj.get(fname) {
            Some(v) if !v.is_null() => {
                fields.insert(fname.clone(), coerce_value(v, fty, fname)?);
            }
            _ => {
                if *required {
                    return err(format!("missing required field '{fname}'"));
                }
                fields.insert(fname.clone(), Value::Null);
            }
        }
    }
    Ok(Value::Record {
        type_name: Some(r.name.clone()),
        fields,
    })
}

fn coerce_list(j: &Json, elem: &Type) -> Result<Value, CoerceError> {
    let arr = match j.as_array() {
        Some(a) => a,
        None => return err("expected an array"),
    };
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        out.push(coerce_value(item, elem, &format!("[{i}]"))?);
    }
    Ok(Value::List(out))
}

fn coerce_map(j: &Json, val: &Type) -> Result<Value, CoerceError> {
    let obj = match j.as_object() {
        Some(o) => o,
        None => return err("expected an object"),
    };
    let mut m = IndexMap::new();
    for (k, v) in obj {
        m.insert(k.clone(), coerce_value(v, val, k)?);
    }
    Ok(Value::Map(m))
}

// ---- decode-from-prose ----

/// `(parsed JSON if any, raw trimmed text)`.
fn decode_reply(text: &str) -> (Option<Json>, String) {
    let raw = strip_code_fence(text.trim());
    if let Ok(v) = serde_json::from_str::<Json>(&raw) {
        return (Some(v), raw);
    }
    if let Some(span) = first_bracket_span(&raw) {
        if let Ok(v) = serde_json::from_str::<Json>(&span) {
            return (Some(v), raw);
        }
    }
    (None, raw)
}

fn strip_code_fence(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // drop the ```lang line
        let after_nl = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
        let body = after_nl.trim_end();
        let body = body.strip_suffix("```").unwrap_or(body);
        return body.trim().to_string();
    }
    s.to_string()
}

/// Find the first balanced `{...}` or `[...]` span (respecting JSON strings).
fn first_bracket_span(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.iter().position(|&c| c == '{' || c == '[')?;
    let open = chars[start];
    let close = if open == '{' { '}' } else { ']' };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in chars.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(chars[start..=i].iter().collect());
                }
            }
            _ => {}
        }
    }
    None
}
