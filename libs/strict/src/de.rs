//! Serde `Deserializer` over a [`Value`] tree.
//!
//! Lets a typed config struct keep `#[derive(Deserialize)]` and its serde
//! attributes (`default`, `deny_unknown_fields`, `rename_all`, `untagged`)
//! instead of a hand-written `TryFrom<Value>`. The format has one number
//! type, `f64`, so integer fields hydrate through an exact conversion that
//! refuses a fraction, a non-finite value and anything beyond 2^53 rather
//! than truncating.

use std::path::Path;

use serde::de::{
    self, Deserialize, DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};

use crate::MAX_EXACT_INT;
use crate::error::{Error, ErrorKind, Result};
use crate::value::{Map, Value};

/// Deserialize a `T` from an already-parsed tree. `T` may borrow from it.
pub fn from_value<'de, T>(value: &'de Value) -> Result<T>
where
    T: Deserialize<'de>,
{
    T::deserialize(ValueDeserializer::new(value))
}

/// Parse `source` and deserialize a `T` from it: the entry point config
/// loaders use.
pub fn from_str<T>(source: &str) -> Result<T>
where
    T: de::DeserializeOwned,
{
    from_value(&crate::parse(source)?)
}

/// Read the file at `path`, parse it and deserialize a `T` from it.
pub fn from_file<T>(path: &Path) -> Result<T>
where
    T: de::DeserializeOwned,
{
    from_value(&crate::parse_file(path)?)
}

fn de_error(message: String) -> Error {
    Error::new(ErrorKind::Deserialize, message)
}

/// Deserializer wrapping a borrowed node.
pub struct ValueDeserializer<'de> {
    value: &'de Value,
}

impl<'de> ValueDeserializer<'de> {
    pub fn new(value: &'de Value) -> Self {
        Self { value }
    }

    fn type_error(&self, expected: &str) -> Error {
        de_error(format!("expected {expected}, found {}", self.value.type_name()))
    }

    fn number(&self) -> Result<f64> {
        match self.value {
            Value::Number(n) => Ok(*n),
            _ => Err(self.type_error("number")),
        }
    }

    /// Exact `f64 -> i64`. The concrete width (`i8`, `i16`, …) is checked
    /// afterwards by serde's own `visit_i64`.
    fn exact_i64(&self) -> Result<i64> {
        let n = self.number()?;
        if !n.is_finite() {
            return Err(de_error(format!("{n} is not a finite integer")));
        }
        if n.fract() != 0.0 {
            return Err(de_error(format!(
                "{n} is not an integer (a whole number is required here)"
            )));
        }
        if n.abs() > MAX_EXACT_INT {
            return Err(de_error(format!(
                "{n} exceeds the range integers can represent exactly in this format (2^53)"
            )));
        }
        Ok(n as i64)
    }

    /// Exact `f64 -> u64`, as [`Self::exact_i64`] plus a sign check.
    fn exact_u64(&self) -> Result<u64> {
        let n = self.number()?;
        if !n.is_finite() {
            return Err(de_error(format!("{n} is not a finite integer")));
        }
        if n.fract() != 0.0 {
            return Err(de_error(format!(
                "{n} is not an integer (a whole number is required here)"
            )));
        }
        if n < 0.0 {
            return Err(de_error(format!(
                "{n} is negative (an unsigned integer is required here)"
            )));
        }
        if n > MAX_EXACT_INT {
            return Err(de_error(format!(
                "{n} exceeds the range integers can represent exactly in this format (2^53)"
            )));
        }
        Ok(n as u64)
    }
}

macro_rules! deserialize_signed {
    ($method:ident) => {
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            visitor.visit_i64(self.exact_i64()?)
        }
    };
}

macro_rules! deserialize_unsigned {
    ($method:ident) => {
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            visitor.visit_u64(self.exact_u64()?)
        }
    };
}

impl<'de> de::Deserializer<'de> for ValueDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::Nil => visitor.visit_unit(),
            Value::Bool(b) => visitor.visit_bool(*b),
            Value::Number(n) => visitor.visit_f64(*n),
            Value::String(s) => visitor.visit_borrowed_str(s),
            Value::List(items) => visitor.visit_seq(SeqDeserializer { iter: items.iter() }),
            Value::Map(entries) => visitor.visit_map(MapDeserializer::new(entries)),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::Bool(b) => visitor.visit_bool(*b),
            _ => Err(self.type_error("bool")),
        }
    }

    deserialize_signed!(deserialize_i8);
    deserialize_signed!(deserialize_i16);
    deserialize_signed!(deserialize_i32);
    deserialize_signed!(deserialize_i64);

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_i128(self.exact_i64()? as i128)
    }

    deserialize_unsigned!(deserialize_u8);
    deserialize_unsigned!(deserialize_u16);
    deserialize_unsigned!(deserialize_u32);
    deserialize_unsigned!(deserialize_u64);

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u128(self.exact_u64()? as u128)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f64(self.number()?)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f64(self.number()?)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::String(s) => {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => visitor.visit_char(c),
                    _ => Err(de_error(format!(
                        "expected a single character, found a string of length {}",
                        s.chars().count()
                    ))),
                }
            }
            _ => Err(self.type_error("char")),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::String(s) => visitor.visit_borrowed_str(s),
            _ => Err(self.type_error("string")),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(self.type_error("bytes, which strict data cannot carry"))
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::Nil => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::Nil => visitor.visit_unit(),
            _ => Err(self.type_error("nil")),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::List(items) => visitor.visit_seq(SeqDeserializer { iter: items.iter() }),
            _ => Err(self.type_error("list")),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.value {
            Value::Map(entries) => visitor.visit_map(MapDeserializer::new(entries)),
            _ => Err(self.type_error("map")),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_map(visitor)
    }

    /// A unit variant is a plain string; a variant with a payload is a
    /// single-key map `{ variant: payload }`.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        match self.value {
            Value::String(s) => visitor.visit_enum(s.as_str().into_deserializer()),
            Value::Map(entries) if entries.len() == 1 => {
                let (variant, value) = entries.iter().next().expect("len == 1");
                visitor.visit_enum(EnumDeserializer { variant, value })
            }
            Value::Map(entries) => Err(de_error(format!(
                "expected a single-key map for an enum variant, found a map with {} keys",
                entries.len()
            ))),
            _ => Err(self.type_error("a string or single-key map (for an enum)")),
        }
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }
}

struct SeqDeserializer<'de> {
    iter: std::slice::Iter<'de, Value>,
}

impl<'de> SeqAccess<'de> for SeqDeserializer<'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        match self.iter.next() {
            Some(v) => seed.deserialize(ValueDeserializer::new(v)).map(Some),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

/// Yields every key the map holds, so `deny_unknown_fields` sees a stale
/// key and turns it into an error instead of a silent default.
struct MapDeserializer<'de> {
    iter: indexmap::map::Iter<'de, String, Value>,
    value: Option<&'de Value>,
}

impl<'de> MapDeserializer<'de> {
    fn new(entries: &'de Map) -> Self {
        Self {
            iter: entries.iter(),
            value: None,
        }
    }
}

impl<'de> MapAccess<'de> for MapDeserializer<'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        match self.iter.next() {
            Some((k, v)) => {
                self.value = Some(v);
                seed.deserialize(KeyDeserializer { key: k }).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        let value = self
            .value
            .take()
            .expect("next_value_seed called before next_key_seed");
        seed.deserialize(ValueDeserializer::new(value))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

/// Keys are always strings, so every requested shape resolves to the
/// borrowed key text.
struct KeyDeserializer<'de> {
    key: &'de str,
}

impl<'de> de::Deserializer<'de> for KeyDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_borrowed_str(self.key)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

struct EnumDeserializer<'de> {
    variant: &'de str,
    value: &'de Value,
}

impl<'de> EnumAccess<'de> for EnumDeserializer<'de> {
    type Error = Error;
    type Variant = VariantDeserializer<'de>;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self::Variant)> {
        let variant = seed.deserialize(self.variant.into_deserializer())?;
        Ok((variant, VariantDeserializer { value: self.value }))
    }
}

struct VariantDeserializer<'de> {
    value: &'de Value,
}

impl<'de> VariantAccess<'de> for VariantDeserializer<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Deserialize::deserialize(ValueDeserializer::new(self.value))
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(ValueDeserializer::new(self.value))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_seq(ValueDeserializer::new(self.value), visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        de::Deserializer::deserialize_map(ValueDeserializer::new(self.value), visitor)
    }
}
