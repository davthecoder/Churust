//! A `serde` deserializer over captured path parameters.
//!
//! Supports the three shapes a route needs:
//!
//! - `Path<u64>` — a single parameter, by position
//! - `Path<(u64, String)>` — several, positionally, in capture order
//! - `Path<Info>` — several, by name, into a struct
//!
//! Deliberately narrow: path parameters are always strings, so everything
//! bottoms out in `parse()`. Nested structures are not representable in a URL
//! path and are rejected rather than half-supported.

use crate::call::Params;
use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};
use std::fmt;

/// What went wrong turning parameters into `T`.
#[derive(Debug)]
pub struct PathError(String);

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathError {}

impl de::Error for PathError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        PathError(msg.to_string())
    }
}

/// Deserialize captured parameters into `T`.
pub(crate) fn from_params<T: serde::de::DeserializeOwned>(params: &Params) -> Result<T, PathError> {
    T::deserialize(ParamsDeserializer { params, index: 0 })
}

/// Deserializes one captured value.
///
/// A path parameter is always a string, so serde's own `StrDeserializer` is not
/// enough: it only ever calls `visit_str`, and a `u64` target rejects that.
/// Numeric and boolean targets parse instead.
#[derive(Clone, Copy)]
struct ValueDeserializer<'a>(&'a str);

macro_rules! parse_into {
    ($($m:ident => $visit:ident : $t:ty),* $(,)?) => {
        $(
            fn $m<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
                let v = self.0.parse::<$t>().map_err(|_| {
                    PathError(format!(
                        "`{}` is not a valid {}",
                        self.0,
                        std::any::type_name::<$t>()
                    ))
                })?;
                visitor.$visit(v)
            }
        )*
    };
}

impl<'de, 'a> Deserializer<'de> for ValueDeserializer<'a> {
    type Error = PathError;

    parse_into! {
        deserialize_bool => visit_bool: bool,
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64,
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
        deserialize_u64 => visit_u64: u64,
        deserialize_f32 => visit_f32: f32,
        deserialize_f64 => visit_f64: f64,
        deserialize_char => visit_char: char,
    }

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_str(self.0)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_str(self.0)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_string(self.0.to_string())
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_bytes(self.0.as_bytes())
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_byte_buf(self.0.as_bytes().to_vec())
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_some(self)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_str(self.0)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_unit()
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _n: &'static str,
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _n: &'static str,
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _n: &'static str,
        _v: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_enum(self.0.into_deserializer())
    }

    // A URL path segment is flat: these shapes cannot appear in one.
    fn deserialize_seq<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, PathError> {
        Err(PathError("a path parameter cannot be a sequence".into()))
    }
    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, PathError> {
        Err(PathError("a path parameter cannot be a tuple".into()))
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _n: &'static str,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, PathError> {
        Err(PathError(
            "a path parameter cannot be a tuple struct".into(),
        ))
    }
    fn deserialize_map<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, PathError> {
        Err(PathError("a path parameter cannot be a map".into()))
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _n: &'static str,
        _f: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, PathError> {
        Err(PathError("a path parameter cannot be a struct".into()))
    }
}

struct ParamsDeserializer<'a> {
    params: &'a Params,
    index: usize,
}

macro_rules! forward_to_str {
    ($($m:ident)*) => {
        $(
            fn $m<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
                // A single parameter: take the first, which is what a
                // one-parameter route means by `Path<u64>`.
                let raw = self.params.nth(0).ok_or_else(|| {
                    PathError("route captured no path parameter".into())
                })?;
                ValueDeserializer(raw).$m(visitor)
            }
        )*
    };
}

impl<'de, 'a> Deserializer<'de> for ParamsDeserializer<'a> {
    type Error = PathError;

    forward_to_str! {
        deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_f32 deserialize_f64 deserialize_char deserialize_str
        deserialize_string deserialize_bytes deserialize_byte_buf
        deserialize_identifier deserialize_any
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        match self.params.nth(0) {
            Some(_) => visitor.visit_some(self),
            None => visitor.visit_none(),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_seq(self)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, PathError> {
        if self.params.len() < len {
            return Err(PathError(format!(
                "route captured {} parameter(s) but {len} were requested",
                self.params.len()
            )));
        }
        visitor.visit_seq(self)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, PathError> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_map(self)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_map(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, PathError> {
        visitor.visit_enum(self)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathError> {
        visitor.visit_unit()
    }
}

impl<'de, 'a> SeqAccess<'de> for ParamsDeserializer<'a> {
    type Error = PathError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, PathError> {
        let Some(raw) = self.params.nth(self.index) else {
            return Ok(None);
        };
        self.index += 1;
        seed.deserialize(ValueDeserializer(raw)).map(Some)
    }
}

impl<'de, 'a> MapAccess<'de> for ParamsDeserializer<'a> {
    type Error = PathError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, PathError> {
        match self.params.iter().nth(self.index) {
            Some((k, _)) => seed.deserialize(ValueDeserializer(k)).map(Some),
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, PathError> {
        let (_, v) = self
            .params
            .iter()
            .nth(self.index)
            .ok_or_else(|| PathError("value without a key".into()))?;
        self.index += 1;
        seed.deserialize(ValueDeserializer(v))
    }
}

impl<'de, 'a> EnumAccess<'de> for ParamsDeserializer<'a> {
    type Error = PathError;
    type Variant = UnitVariant;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, UnitVariant), PathError> {
        let raw = self
            .params
            .nth(0)
            .ok_or_else(|| PathError("route captured no path parameter".into()))?;
        Ok((seed.deserialize(ValueDeserializer(raw))?, UnitVariant))
    }
}

/// Only unit variants are reachable: a path segment is a bare string, so there
/// is nothing for a variant to carry.
pub struct UnitVariant;

impl<'de> VariantAccess<'de> for UnitVariant {
    type Error = PathError;

    fn unit_variant(self) -> Result<(), PathError> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        _seed: T,
    ) -> Result<T::Value, PathError> {
        Err(PathError(
            "a path parameter cannot be a newtype variant".into(),
        ))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, PathError> {
        Err(PathError(
            "a path parameter cannot be a tuple variant".into(),
        ))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, PathError> {
        Err(PathError(
            "a path parameter cannot be a struct variant".into(),
        ))
    }
}
