//! Deserialize a `toml::Value` while recording the dotted key path, so a
//! `Secret` can name the var it came from. `toml::Value`'s own deserializer
//! does not expose the current key.

use serde::de::{
    self, DeserializeOwned, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess,
    Visitor,
};
use serde::forward_to_deserialize_any;

use super::push_path;

pub fn from_value<T: DeserializeOwned>(value: &toml::Value) -> Result<T, String> {
    let _track = super::track_paths();
    T::deserialize(ValDe { v: value }).map_err(|e| e.to_string())
}

struct ValDe<'a> {
    v: &'a toml::Value,
}

impl<'de> Deserializer<'de> for ValDe<'de> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.v {
            toml::Value::String(s) => visitor.visit_str(s),
            toml::Value::Integer(i) => visitor.visit_i64(*i),
            toml::Value::Float(f) => visitor.visit_f64(*f),
            toml::Value::Boolean(b) => visitor.visit_bool(*b),
            toml::Value::Datetime(d) => visitor.visit_string(d.to_string()),
            toml::Value::Array(a) => visitor.visit_seq(SeqDe { a, i: 0 }),
            toml::Value::Table(t) => visitor.visit_map(MapDe {
                iter: t.iter(),
                key: None,
                value: None,
            }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_some(self)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_any(visitor)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct newtype_struct seq tuple
        tuple_struct map enum identifier ignored_any
    }
}

struct SeqDe<'a> {
    a: &'a [toml::Value],
    i: usize,
}

impl<'de> SeqAccess<'de> for SeqDe<'de> {
    type Error = de::value::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let Some(v) = self.a.get(self.i) else {
            return Ok(None);
        };
        let idx = self.i;
        self.i += 1;
        let _path = push_path(&idx.to_string());
        seed.deserialize(ValDe { v }).map(Some)
    }
}

struct MapDe<'a> {
    iter: toml::map::Iter<'a, String, toml::Value>,
    key: Option<String>,
    value: Option<&'a toml::Value>,
}

impl<'de> MapAccess<'de> for MapDe<'de> {
    type Error = de::value::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        match self.iter.next() {
            Some((k, v)) => {
                self.key = Some(k.clone());
                self.value = Some(v);
                seed.deserialize(k.as_str().into_deserializer()).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let key = self.key.take().unwrap_or_default();
        let v = self
            .value
            .take()
            .ok_or_else(|| de::Error::custom("secret path tracker: map value without a key"))?;
        let _path = push_path(&key);
        seed.deserialize(ValDe { v })
    }
}
