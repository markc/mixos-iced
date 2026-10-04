//! The data tree a strict-data document parses to.

use std::fmt;

use indexmap::IndexMap;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::error::Result;

/// A strict-data map: string keys in insertion order.
pub type Map = IndexMap<String, Value>;

/// One node of a strict-data tree. The shape is JSON's with `nil` for
/// `null` and a single `f64` number type.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Nil,
    Bool(bool),
    Number(f64),
    String(String),
    List(Vec<Value>),
    Map(Map),
}

impl Value {
    /// A list value.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(items)
    }

    /// A map value.
    pub fn map(entries: Map) -> Value {
        Value::Map(entries)
    }

    /// The type's name as error messages spell it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
        }
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Map> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_map_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// The entry under `key` when this is a map; `None` otherwise.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map().and_then(|entries| entries.get(key))
    }

    /// The compact one-line strict-data text of this value. See
    /// [`crate::encode`].
    pub fn encode(&self) -> Result<String> {
        crate::encode::encode(self)
    }

    /// The indented strict-data text of this value. See
    /// [`crate::encode_pretty`].
    pub fn encode_pretty(&self) -> Result<String> {
        crate::encode::encode_pretty(self)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::List(items)
    }
}

impl From<Map> for Value {
    fn from(entries: Map) -> Self {
        Value::Map(entries)
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Value::Nil => serializer.serialize_unit(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Number(n) => serializer.serialize_f64(*n),
            Value::String(s) => serializer.serialize_str(s),
            Value::List(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Map(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a strict-data value")
    }

    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Nil)
    }

    fn visit_none<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Nil)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> std::result::Result<Value, D::Error> {
        Deserialize::deserialize(deserializer)
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(b))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(n as f64))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(n as f64))
    }

    fn visit_f64<E: de::Error>(self, n: f64) -> std::result::Result<Value, E> {
        Ok(Value::Number(n))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(s.to_owned()))
    }

    fn visit_string<E: de::Error>(self, s: String) -> std::result::Result<Value, E> {
        Ok(Value::String(s))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut entries = Map::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            entries.insert(key, value);
        }
        Ok(Value::Map(entries))
    }
}
