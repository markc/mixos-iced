//! Mix's value rules over `serde_json::Value`: type names, truthiness,
//! numeric coercion, rendering and equality.

use serde_json::Value;

use crate::{Error, ErrorKind};

pub(crate) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "nil",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "map",
    }
}

/// nil, `false`, `0`, `""`, `"0"`, `[]` and `{}` are falsy; everything
/// else is truthy.
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// A number, a finite numeric string (sign, fraction and exponent only,
/// whitespace trimmed) or a bool (`true` is 1). Nothing else coerces.
pub(crate) fn to_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// The JSON number for an arithmetic result. A whole number that round
/// trips through i64 is stored as an integer, so `40 + 2` is `42` and not
/// `42.0`; a non-finite result has no JSON form and is a runtime error.
pub(crate) fn number(n: f64) -> Result<Value, Error> {
    if !n.is_finite() {
        return Err(Error::new(ErrorKind::Runtime, "number is not finite"));
    }
    let as_int = n as i64;
    if as_int as f64 == n {
        return Ok(Value::Number(as_int.into()));
    }
    serde_json::Number::from_f64(n)
        .map(Value::Number)
        .ok_or_else(|| Error::new(ErrorKind::Runtime, "number is not finite"))
}

/// Append the Mix text form of a value: numbers print as integers when
/// they are whole, nil prints as `nil`, strings are unquoted, lists are
/// `[a, b]` and maps are `{k: v}`.
pub(crate) fn render(value: &Value, out: &mut String) {
    use std::fmt::Write;
    match value {
        Value::Null => out.push_str("nil"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            let as_int = f as i64;
            if as_int as f64 == f {
                let _ = write!(out, "{as_int}");
            } else {
                let _ = write!(out, "{f}");
            }
        }
        Value::String(s) => out.push_str(s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(key);
                out.push_str(": ");
                render(item, out);
            }
            out.push('}');
        }
    }
}

pub(crate) fn rendered(value: &Value) -> String {
    let mut out = String::new();
    render(value, &mut out);
    out
}

/// `==`: same-type values compare by content; a number against a string
/// compares numerically when the string parses; a list or map against a
/// scalar is unequal. A collection on both sides is a type error.
pub(crate) fn equals(left: &Value, right: &Value, op: &str) -> Result<bool, Error> {
    Ok(match (left, right) {
        (Value::Array(_) | Value::Object(_), Value::Array(_) | Value::Object(_)) => {
            return Err(Error::new(
                ErrorKind::Runtime,
                format!(
                    "`{op}` is not defined for {} and {}; it would always answer {}, not compare them",
                    type_name(left),
                    type_name(right),
                    if op == "==" { "false" } else { "true" }
                ),
            ));
        }
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(n), Value::String(s)) | (Value::String(s), Value::Number(n)) => {
            s.parse::<f64>().is_ok_and(|parsed| Some(parsed) == n.as_f64())
        }
        _ => false,
    })
}

/// A list index, negative counting from the end; `None` when out of range.
pub(crate) fn signed_index(index: f64, len: usize) -> Option<usize> {
    let idx = index as i64;
    if idx >= 0 {
        let u = idx as usize;
        (u < len).then_some(u)
    } else {
        let back = idx.unsigned_abs() as usize;
        (back <= len).then(|| len - back)
    }
}
