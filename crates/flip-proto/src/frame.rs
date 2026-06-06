//! Frame wire format (little-endian), identical to the proven C prototype.
//!
//! magic u16 0xF1A6 | version u8 | type u8 | flags u8 | seq u16 | length u32
//! | payload[length] | crc16 u16 over bytes [version .. end of payload].

use crate::crc16::crc16_ccitt_false;

pub const FRAME_MAGIC: u16 = 0xF1A6;
pub const FRAME_VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 11; // magic..length inclusive
pub const CRC_SIZE: usize = 2;
pub const MAX_PAYLOAD: u32 = 0xFFFF;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum MsgType {
    Hello = 1,
    Caps = 2,
    Req = 3,
    Resp = 4,
    StreamStart = 5,
    StreamData = 6,
    StreamStop = 7,
    Event = 8,
    Error = 9,
    Ping = 10,
    Pong = 11,
}

impl MsgType {
    pub fn from_u8(v: u8) -> Option<MsgType> {
        Some(match v {
            1 => MsgType::Hello,
            2 => MsgType::Caps,
            3 => MsgType::Req,
            4 => MsgType::Resp,
            5 => MsgType::StreamStart,
            6 => MsgType::StreamData,
            7 => MsgType::StreamStop,
            8 => MsgType::Event,
            9 => MsgType::Error,
            10 => MsgType::Ping,
            11 => MsgType::Pong,
            _ => return None,
        })
    }
}

/// A decoded frame. `payload` borrows from the input buffer.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub typ: MsgType,
    pub flags: u8,
    pub seq: u16,
    pub payload: &'a [u8],
}

#[derive(Debug)]
pub enum DecodeResult<'a> {
    /// A complete, CRC-valid frame and the number of bytes it consumed.
    Frame(Frame<'a>, usize),
    /// Not enough bytes yet.
    NeedMore,
    /// Bad magic or CRC at offset 0; caller should drop 1 byte and retry.
    Resync,
}

/// Encode a frame into `out`. Returns bytes written, or `None` if `out` is too small
/// or the payload exceeds `MAX_PAYLOAD`.
pub fn encode(typ: MsgType, flags: u8, seq: u16, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    if payload.len() as u32 > MAX_PAYLOAD {
        return None;
    }
    let total = HEADER_SIZE + payload.len() + CRC_SIZE;
    if out.len() < total {
        return None;
    }
    out[0..2].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
    out[2] = FRAME_VERSION;
    out[3] = typ as u8;
    out[4] = flags;
    out[5..7].copy_from_slice(&seq.to_le_bytes());
    out[7..11].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    out[HEADER_SIZE..HEADER_SIZE + payload.len()].copy_from_slice(payload);
    // CRC covers bytes [2 .. end of payload] (everything except magic).
    let crc = crc16_ccitt_false(&out[2..HEADER_SIZE + payload.len()]);
    let crc_at = HEADER_SIZE + payload.len();
    out[crc_at..crc_at + 2].copy_from_slice(&crc.to_le_bytes());
    Some(total)
}

/// Try to decode one frame from the front of `buf`.
pub fn decode(buf: &[u8]) -> DecodeResult<'_> {
    if buf.len() < 2 {
        return DecodeResult::NeedMore;
    }
    let magic = u16::from_le_bytes([buf[0], buf[1]]);
    if magic != FRAME_MAGIC {
        return DecodeResult::Resync;
    }
    if buf.len() < HEADER_SIZE {
        return DecodeResult::NeedMore;
    }
    let length = u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]);
    if length > MAX_PAYLOAD {
        return DecodeResult::Resync;
    }
    let length = length as usize;
    let total = HEADER_SIZE + length + CRC_SIZE;
    if buf.len() < total {
        return DecodeResult::NeedMore;
    }
    let crc_at = HEADER_SIZE + length;
    let want = u16::from_le_bytes([buf[crc_at], buf[crc_at + 1]]);
    let got = crc16_ccitt_false(&buf[2..crc_at]);
    if want != got {
        return DecodeResult::Resync;
    }
    let typ = match MsgType::from_u8(buf[3]) {
        Some(t) => t,
        None => return DecodeResult::Resync,
    };
    let frame = Frame {
        typ,
        flags: buf[4],
        seq: u16::from_le_bytes([buf[5], buf[6]]),
        payload: &buf[HEADER_SIZE..crc_at],
    };
    DecodeResult::Frame(frame, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_ping() {
        let mut buf = [0u8; 64];
        let n = encode(MsgType::Ping, 0, 0x1234, b"hi", &mut buf).unwrap();
        match decode(&buf[..n]) {
            DecodeResult::Frame(f, used) => {
                assert_eq!(used, n);
                assert_eq!(f.typ, MsgType::Ping);
                assert_eq!(f.seq, 0x1234);
                assert_eq!(f.payload, b"hi");
            }
            other => panic!("expected Frame, got {:?}", other),
        }
    }

    #[test]
    fn need_more_when_truncated() {
        let mut buf = [0u8; 64];
        let n = encode(MsgType::Pong, 0, 1, b"abc", &mut buf).unwrap();
        assert!(matches!(decode(&buf[..n - 1]), DecodeResult::NeedMore));
    }

    #[test]
    fn resync_on_bad_magic() {
        assert!(matches!(decode(&[0x00, 0x00, 0x00]), DecodeResult::Resync));
    }

    #[test]
    fn resync_on_corrupt_crc() {
        let mut buf = [0u8; 64];
        let n = encode(MsgType::Ping, 0, 1, b"xy", &mut buf).unwrap();
        buf[n - 1] ^= 0xFF; // corrupt last CRC byte
        assert!(matches!(decode(&buf[..n]), DecodeResult::Resync));
    }
}
