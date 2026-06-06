use anyhow::Result;
use flip_proto::{decode, DecodeResult, MsgType};

/// A byte-stream transport (serial port, socket, or in-memory mock).
pub trait Transport {
    /// Write all bytes.
    fn write_all(&mut self, buf: &[u8]) -> Result<()>;
    /// Read some bytes into `buf`, returning the count (0 = would-block/timeout).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
}

/// An owned, decoded frame (payload copied out so it doesn't borrow the buffer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedFrame {
    pub typ: MsgType,
    pub flags: u8,
    pub seq: u16,
    pub payload: Vec<u8>,
}

/// Accumulates bytes from a transport and yields whole frames, resyncing on garbage.
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        FrameReader { buf: Vec::new() }
    }

    /// Feed raw bytes that arrived from the transport.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete frame, if one is buffered. Drops leading garbage.
    pub fn next_frame(&mut self) -> Option<OwnedFrame> {
        loop {
            match decode(&self.buf) {
                DecodeResult::Frame(f, used) => {
                    let owned = OwnedFrame {
                        typ: f.typ,
                        flags: f.flags,
                        seq: f.seq,
                        payload: f.payload.to_vec(),
                    };
                    self.buf.drain(0..used);
                    return Some(owned);
                }
                DecodeResult::NeedMore => return None,
                DecodeResult::Resync => {
                    if self.buf.is_empty() {
                        return None;
                    }
                    self.buf.drain(0..1);
                }
            }
        }
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flip_proto::encode;

    #[test]
    fn reader_yields_frame_across_chunks() {
        let mut out = [0u8; 64];
        let n = encode(MsgType::Ping, 0, 7, b"hi", &mut out).unwrap();
        let mut r = FrameReader::new();
        r.feed(&out[..3]); // partial header
        assert!(r.next_frame().is_none());
        r.feed(&out[3..n]); // rest
        let f = r.next_frame().unwrap();
        assert_eq!(f.typ, MsgType::Ping);
        assert_eq!(f.seq, 7);
        assert_eq!(f.payload, b"hi");
    }

    #[test]
    fn reader_resyncs_past_garbage() {
        let mut out = [0u8; 64];
        let n = encode(MsgType::Pong, 0, 1, b"x", &mut out).unwrap();
        let mut r = FrameReader::new();
        r.feed(&[0xDE, 0xAD, 0xBE]); // junk
        r.feed(&out[..n]);
        let f = r.next_frame().unwrap();
        assert_eq!(f.typ, MsgType::Pong);
    }
}
