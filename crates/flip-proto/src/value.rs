//! A dynamic CBOR value for generic instrument params/results. Requires `alloc`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use minicbor::data::Type;
use minicbor::decode::Error as DecodeError;
use minicbor::encode::{Error as EncodeError, Write};
use minicbor::{Decoder, Encoder};

/// A CBOR value restricted to the wire profile (no floats; map keys are text).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    U64(u64),
    I64(i64),
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    /// Convenience: look up a key in a `Map` value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl<C> minicbor::Encode<C> for Value {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        match self {
            Value::Null => {
                e.null()?;
            }
            Value::Bool(b) => {
                e.bool(*b)?;
            }
            Value::U64(n) => {
                e.u64(*n)?;
            }
            Value::I64(n) => {
                e.i64(*n)?;
            }
            Value::Text(s) => {
                e.str(s)?;
            }
            Value::Bytes(b) => {
                e.bytes(b)?;
            }
            Value::Array(a) => {
                e.array(a.len() as u64)?;
                for v in a {
                    v.encode(e, ctx)?;
                }
            }
            Value::Map(m) => {
                e.map(m.len() as u64)?;
                for (k, v) in m {
                    e.str(k)?;
                    v.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> minicbor::Decode<'b, C> for Value {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, DecodeError> {
        match d.datatype()? {
            Type::Null => {
                d.null()?;
                Ok(Value::Null)
            }
            Type::Bool => Ok(Value::Bool(d.bool()?)),
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => Ok(Value::U64(d.u64()?)),
            Type::I8 | Type::I16 | Type::I32 | Type::I64 => Ok(Value::I64(d.i64()?)),
            Type::String => Ok(Value::Text(d.str()?.to_string())),
            Type::Bytes => Ok(Value::Bytes(d.bytes()?.to_vec())),
            Type::Array | Type::ArrayIndef => {
                let len = d.array()?;
                let mut out = Vec::new();
                match len {
                    Some(n) => {
                        for _ in 0..n {
                            out.push(Value::decode(d, ctx)?);
                        }
                    }
                    None => {
                        while d.datatype()? != Type::Break {
                            out.push(Value::decode(d, ctx)?);
                        }
                        d.skip()?;
                    }
                }
                Ok(Value::Array(out))
            }
            Type::Map | Type::MapIndef => {
                let len = d.map()?;
                let mut out = Vec::new();
                match len {
                    Some(n) => {
                        for _ in 0..n {
                            let k = d.str()?.to_string();
                            out.push((k, Value::decode(d, ctx)?));
                        }
                    }
                    None => {
                        while d.datatype()? != Type::Break {
                            let k = d.str()?.to_string();
                            out.push((k, Value::decode(d, ctx)?));
                        }
                        d.skip()?;
                    }
                }
                Ok(Value::Map(out))
            }
            _ => Err(DecodeError::message("unsupported CBOR type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(v: &Value) -> Value {
        let mut buf = Vec::new();
        minicbor::encode(v, &mut buf).unwrap();
        minicbor::decode(&buf).unwrap()
    }

    #[test]
    fn scalars_round_trip() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::U64(42),
            Value::I64(-7),
            Value::Text("hi".to_string()),
            Value::Bytes(alloc::vec![1u8, 2, 3]),
        ] {
            assert_eq!(round_trip(&v), v);
        }
    }

    #[test]
    fn nested_map_and_array_round_trip() {
        let v = Value::Map(alloc::vec![
            ("freq".to_string(), Value::U64(38000)),
            (
                "timings".to_string(),
                Value::Array(alloc::vec![Value::U64(9000), Value::U64(4500)]),
            ),
        ]);
        assert_eq!(round_trip(&v), v);
    }

    #[test]
    fn get_finds_key() {
        let v = Value::Map(alloc::vec![("k".to_string(), Value::U64(1))]);
        assert_eq!(v.get("k"), Some(&Value::U64(1)));
        assert_eq!(v.get("nope"), None);
    }
}
