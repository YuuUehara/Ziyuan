//! Shared duplicate-key-safe JSON decoding with an optional container-depth bound.
use super::{Result, invalid};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;

/// Root containers count as layer 1; scalar values add no layer.
/// `None` preserves serde_json's production recursion/error boundary.
pub(crate) fn parse_strict_json(
    bytes: &[u8],
    label: &str,
    depth_limit: Option<usize>,
) -> Result<Value> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue {
        path: label.into(),
        depth: 0,
        depth_limit,
    }
    .deserialize(&mut deserializer)
    .map_err(|e| invalid(format!("{label} 格式不对：{e}")))?;
    deserializer
        .end()
        .map_err(|e| invalid(format!("{label} 格式不对：{e}")))?;
    Ok(value)
}

/// Deserialize a JSON value without losing duplicate keys (including tagged objects).
struct StrictValue {
    path: String,
    depth: usize,
    depth_limit: Option<usize>,
}

impl StrictValue {
    fn child(&self, path: String) -> Self {
        Self {
            path,
            depth: self.depth + 1,
            depth_limit: self.depth_limit,
        }
    }

    fn check_depth<E: de::Error>(&self) -> std::result::Result<(), E> {
        if let Some(limit) = self.depth_limit
            && self.depth >= limit
        {
            return Err(E::custom(format!("{} JSON 嵌套至多 {limit} 层", self.path)));
        }
        Ok(())
    }
}
impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for StrictValue {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON 值")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("无效数字"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        self.check_depth::<A::Error>()?;
        let mut values = Vec::new();
        while let Some(v) =
            seq.next_element_seed(self.child(format!("{}.{}", self.path, values.len())))?
        {
            values.push(v);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        self.check_depth::<A::Error>()?;
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let path = format!("{}.{key}", self.path);
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("{path} 字段重复")));
            }
            values.insert(key, map.next_value_seed(self.child(path))?);
        }
        Ok(Value::Object(values))
    }
}
