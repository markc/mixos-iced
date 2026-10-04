//! Serde `Serializer` that builds a [`Value`], plus [`to_string`] for
//! writing a typed config struct back to disk.
//!
//! Integers become `Value::Number(f64)`, so an integer whose magnitude
//! exceeds 2^53 is refused rather than rounded: the read side applies the
//! same gate, which keeps write-back symmetric with [`crate::de`].

use serde::ser::{self, Serialize};

use crate::MAX_EXACT_INT;
use crate::error::{Error, ErrorKind, Result};
use crate::value::{Map, Value};

/// Serialize `value` to a [`Value`] tree.
pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value> {
    value.serialize(ValueSerializer)
}

/// Serialize `value` to compact strict-data text: [`to_value`] then
/// [`crate::encode`].
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    crate::encode::encode(&to_value(value)?)
}

/// Serialize `value` to indented strict-data text: [`to_value`] then
/// [`crate::encode_pretty`].
pub fn to_string_pretty<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    crate::encode::encode_pretty(&to_value(value)?)
}

fn ser_error(message: String) -> Error {
    Error::new(ErrorKind::Serialize, message)
}

fn checked_i128(n: i128) -> Result<f64> {
    if n.unsigned_abs() > MAX_EXACT_INT as u128 {
        return Err(ser_error(format!(
            "integer {n} exceeds the range integers can represent exactly in this format (2^53)"
        )));
    }
    Ok(n as f64)
}

fn checked_u128(n: u128) -> Result<f64> {
    if n > MAX_EXACT_INT as u128 {
        return Err(ser_error(format!(
            "integer {n} exceeds the range integers can represent exactly in this format (2^53)"
        )));
    }
    Ok(n as f64)
}

/// Serializer producing a single [`Value`].
pub struct ValueSerializer;

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = Error;

    type SerializeSeq = SeqSerializer;
    type SerializeTuple = SeqSerializer;
    type SerializeTupleStruct = SeqSerializer;
    type SerializeTupleVariant = TupleVariantSerializer;
    type SerializeMap = MapSerializer;
    type SerializeStruct = MapSerializer;
    type SerializeStructVariant = StructVariantSerializer;

    fn serialize_bool(self, v: bool) -> Result<Value> {
        Ok(Value::Bool(v))
    }

    fn serialize_i8(self, v: i8) -> Result<Value> {
        self.serialize_i128(v as i128)
    }
    fn serialize_i16(self, v: i16) -> Result<Value> {
        self.serialize_i128(v as i128)
    }
    fn serialize_i32(self, v: i32) -> Result<Value> {
        self.serialize_i128(v as i128)
    }
    fn serialize_i64(self, v: i64) -> Result<Value> {
        self.serialize_i128(v as i128)
    }
    fn serialize_i128(self, v: i128) -> Result<Value> {
        Ok(Value::Number(checked_i128(v)?))
    }

    fn serialize_u8(self, v: u8) -> Result<Value> {
        self.serialize_u128(v as u128)
    }
    fn serialize_u16(self, v: u16) -> Result<Value> {
        self.serialize_u128(v as u128)
    }
    fn serialize_u32(self, v: u32) -> Result<Value> {
        self.serialize_u128(v as u128)
    }
    fn serialize_u64(self, v: u64) -> Result<Value> {
        self.serialize_u128(v as u128)
    }
    fn serialize_u128(self, v: u128) -> Result<Value> {
        Ok(Value::Number(checked_u128(v)?))
    }

    fn serialize_f32(self, v: f32) -> Result<Value> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_f64(self, v: f64) -> Result<Value> {
        Ok(Value::Number(v))
    }

    fn serialize_char(self, v: char) -> Result<Value> {
        Ok(Value::String(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Value> {
        Ok(Value::String(v.to_owned()))
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<Value> {
        Err(ser_error(
            "raw bytes have no strict-data representation; encode them as a string".into(),
        ))
    }

    fn serialize_none(self) -> Result<Value> {
        Ok(Value::Nil)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Value> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Value> {
        Ok(Value::Nil)
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Value> {
        Ok(Value::Nil)
    }

    /// A unit variant is its (renamed) name as a string, the inverse of the
    /// deserializer's string arm.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Value> {
        Ok(Value::String(variant.to_owned()))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Value> {
        value.serialize(self)
    }

    /// `{ variant: payload }`.
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value> {
        let mut m = Map::with_capacity(1);
        m.insert(variant.to_owned(), value.serialize(ValueSerializer)?);
        Ok(Value::Map(m))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSerializer> {
        Ok(SeqSerializer {
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<SeqSerializer> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SeqSerializer> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<TupleVariantSerializer> {
        Ok(TupleVariantSerializer {
            variant,
            items: Vec::with_capacity(len),
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<MapSerializer> {
        Ok(MapSerializer {
            entries: Map::with_capacity(len.unwrap_or(0)),
            next_key: None,
        })
    }
    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<MapSerializer> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<StructVariantSerializer> {
        Ok(StructVariantSerializer {
            variant,
            entries: Map::with_capacity(len),
        })
    }
}

pub struct SeqSerializer {
    items: Vec<Value>,
}

impl ser::SerializeSeq for SeqSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.items.push(value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::List(self.items))
    }
}

impl ser::SerializeTuple for SeqSerializer {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<Value> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for SeqSerializer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<Value> {
        ser::SerializeSeq::end(self)
    }
}

pub struct TupleVariantSerializer {
    variant: &'static str,
    items: Vec<Value>,
}

impl ser::SerializeTupleVariant for TupleVariantSerializer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.items.push(value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        let mut m = Map::with_capacity(1);
        m.insert(self.variant.to_owned(), Value::List(self.items));
        Ok(Value::Map(m))
    }
}

pub struct MapSerializer {
    entries: Map,
    next_key: Option<String>,
}

impl ser::SerializeMap for MapSerializer {
    type Ok = Value;
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<()> {
        self.next_key = Some(key.serialize(MapKeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        let key = self
            .next_key
            .take()
            .ok_or_else(|| ser_error("serialize_value called before serialize_key".into()))?;
        self.entries.insert(key, value.serialize(ValueSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<Value> {
        Ok(Value::Map(self.entries))
    }
}

impl ser::SerializeStruct for MapSerializer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<()> {
        self.entries.insert(key.to_owned(), value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::Map(self.entries))
    }
}

pub struct StructVariantSerializer {
    variant: &'static str,
    entries: Map,
}

impl ser::SerializeStructVariant for StructVariantSerializer {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<()> {
        self.entries.insert(key.to_owned(), value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        let mut m = Map::with_capacity(1);
        m.insert(self.variant.to_owned(), Value::Map(self.entries));
        Ok(Value::Map(m))
    }
}

/// Map keys are strings. An integer key is written in decimal (as
/// serde_json does); any other shape is refused rather than dropped.
struct MapKeySerializer;

fn key_error(kind: &str) -> Error {
    ser_error(format!("map key must be a string (or integer); found {kind}"))
}

impl ser::Serializer for MapKeySerializer {
    type Ok = String;
    type Error = Error;

    type SerializeSeq = ser::Impossible<String, Error>;
    type SerializeTuple = ser::Impossible<String, Error>;
    type SerializeTupleStruct = ser::Impossible<String, Error>;
    type SerializeTupleVariant = ser::Impossible<String, Error>;
    type SerializeMap = ser::Impossible<String, Error>;
    type SerializeStruct = ser::Impossible<String, Error>;
    type SerializeStructVariant = ser::Impossible<String, Error>;

    fn serialize_str(self, v: &str) -> Result<String> {
        Ok(v.to_owned())
    }
    fn serialize_char(self, v: char) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_i8(self, v: i8) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_i16(self, v: i16) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_i32(self, v: i32) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_i64(self, v: i64) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_u8(self, v: u8) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_u16(self, v: u16) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_u32(self, v: u32) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_u64(self, v: u64) -> Result<String> {
        Ok(v.to_string())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<String> {
        Ok(variant.to_owned())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_bool(self, _v: bool) -> Result<String> {
        Err(key_error("bool"))
    }
    fn serialize_i128(self, _v: i128) -> Result<String> {
        Err(key_error("i128"))
    }
    fn serialize_u128(self, _v: u128) -> Result<String> {
        Err(key_error("u128"))
    }
    fn serialize_f32(self, _v: f32) -> Result<String> {
        Err(key_error("f32"))
    }
    fn serialize_f64(self, _v: f64) -> Result<String> {
        Err(key_error("f64"))
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<String> {
        Err(key_error("bytes"))
    }
    fn serialize_none(self) -> Result<String> {
        Err(key_error("none"))
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<String> {
        Err(key_error("some"))
    }
    fn serialize_unit(self) -> Result<String> {
        Err(key_error("unit"))
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<String> {
        Err(key_error("unit struct"))
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String> {
        Err(key_error("newtype variant"))
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
        Err(key_error("sequence"))
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        Err(key_error("tuple"))
    }
    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeTupleStruct> {
        Err(key_error("tuple struct"))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(key_error("tuple variant"))
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        Err(key_error("map"))
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        Err(key_error("struct"))
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(key_error("struct variant"))
    }
}
