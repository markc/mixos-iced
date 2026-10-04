//! Conversion to and from `serde_json::Value`, for consumers that work on
//! JSON trees.

use serde_json::Value as Json;

use crate::value::{Map, Value};

/// The JSON tree with the same shape. `nil` becomes `null`; every number
/// becomes a JSON float (`25` is `25.0`), and a non-finite number, which
/// JSON cannot spell, becomes `null`. Key order is preserved only as far as
/// `serde_json`'s map type keeps it.
pub fn to_json(value: &Value) -> Json {
    match value {
        Value::Nil => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => serde_json::Number::from_f64(*n).map_or(Json::Null, Json::Number),
        Value::String(s) => Json::String(s.clone()),
        Value::List(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Map(entries) => Json::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), to_json(v)))
                .collect(),
        ),
    }
}

/// The strict-data tree with the same shape. `null` becomes `nil`; every
/// number becomes an `f64`, so an integer beyond 2^53 loses precision.
pub fn from_json(value: &Json) -> Value {
    match value {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) => Value::String(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(from_json).collect()),
        Json::Object(entries) => {
            let mut map = Map::with_capacity(entries.len());
            for (k, v) in entries {
                map.insert(k.clone(), from_json(v));
            }
            Value::Map(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_through_json() {
        let value = crate::parse("name: \"alpha\"\nport: 25\nhalf: 0.5\ntags: [\"a\", nil, true]\n").unwrap();
        let json = to_json(&value);
        assert_eq!(
            json,
            json!({"name": "alpha", "port": 25.0, "half": 0.5, "tags": ["a", null, true]})
        );
        assert_eq!(from_json(&json), value);
    }

    #[test]
    fn non_finite_becomes_null() {
        assert_eq!(to_json(&Value::Number(f64::NAN)), Json::Null);
    }

    #[test]
    fn integers_arrive_as_f64() {
        assert_eq!(from_json(&json!(7)), Value::Number(7.0));
        assert_eq!(from_json(&json!(-7)), Value::Number(-7.0));
    }
}
