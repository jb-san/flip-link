//! CBOR control-message bodies carried in frame payloads. Requires `alloc`.
//! Both firmware and host use these exact types (one codec, no drift).

use alloc::string::String;
use alloc::vec::Vec;

use crate::value::Value;

/// Protocol version reported in CAPS.
pub const PROTOCOL_VERSION: u32 = 1;

/// Error codes (mirror the spec). 1..=6 reserved as in the C prototype.
pub const ERR_UNKNOWN_INSTRUMENT: u32 = 1;
pub const ERR_UNKNOWN_OPCODE: u32 = 2;
pub const ERR_BAD_PARAMS: u32 = 3;
pub const ERR_BUSY: u32 = 4;
pub const ERR_OVERSIZED: u32 = 5;
pub const ERR_INTERNAL: u32 = 6;

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Hello {
    #[n(0)]
    pub host_version: u32,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Instrument {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub opcodes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Caps {
    #[n(0)]
    pub protocol_version: u32,
    #[n(1)]
    pub instruments: Vec<Instrument>,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Req {
    #[n(0)]
    pub instrument: String,
    #[n(1)]
    pub opcode: String,
    #[n(2)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Resp {
    #[n(0)]
    pub ok: bool,
    #[n(1)]
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct AgentError {
    #[n(0)]
    pub code: u32,
    #[n(1)]
    pub message: String,
}

/// STREAM_START body (device→client): declares the stream's sample format.
#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct StreamStart {
    #[n(0)]
    pub format: String,
}

/// STREAM_STOP body (device→client final frame): how many samples were dropped.
#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct StreamStop {
    #[n(0)]
    pub dropped: u32,
}

/// The raw-sample format IR capture uses: little-endian i32 microsecond durations.
pub const STREAM_FORMAT_RAW_I32_US: &str = "raw_int32_le_us";

/// Raw Sub-GHz level/duration samples: 1 byte level + u32 little-endian duration_us.
pub const STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1: &str = "subghz_level_duration_le_v1";

/// Encode any control body to a `Vec<u8>` (the frame payload).
pub fn to_payload<T: minicbor::Encode<()>>(msg: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    minicbor::encode(msg, &mut buf).expect("encode to Vec is infallible");
    buf
}

/// Decode a control body from a frame payload.
pub fn from_payload<'b, T: minicbor::Decode<'b, ()>>(
    bytes: &'b [u8],
) -> Result<T, minicbor::decode::Error> {
    minicbor::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn req_round_trip() {
        let req = Req {
            instrument: "sys".to_string(),
            opcode: "echo".to_string(),
            params: Value::Map(alloc::vec![(
                "msg".to_string(),
                Value::Text("hi".to_string())
            )]),
        };
        let payload = to_payload(&req);
        let back: Req = from_payload(&payload).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn stream_bodies_round_trip() {
        let s = StreamStart {
            format: STREAM_FORMAT_RAW_I32_US.to_string(),
        };
        assert_eq!(from_payload::<StreamStart>(&to_payload(&s)).unwrap(), s);
        let p = StreamStop { dropped: 3 };
        assert_eq!(from_payload::<StreamStop>(&to_payload(&p)).unwrap(), p);
        assert_eq!(
            STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1,
            "subghz_level_duration_le_v1"
        );
    }

    #[test]
    fn caps_round_trip() {
        let caps = Caps {
            protocol_version: PROTOCOL_VERSION,
            instruments: alloc::vec![Instrument {
                id: "sys".to_string(),
                opcodes: alloc::vec!["version".to_string(), "echo".to_string()],
            }],
        };
        let payload = to_payload(&caps);
        let back: Caps = from_payload(&payload).unwrap();
        assert_eq!(back, caps);
    }
}
