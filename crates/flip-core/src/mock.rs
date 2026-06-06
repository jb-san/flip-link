use crate::transport::{FrameReader, Transport};
use anyhow::Result;
use flip_proto::{encode, MsgType};
use std::collections::VecDeque;

/// In-memory transport: every PING written is answered with a PONG carrying the
/// same seq and payload, queued for the next read. Mimics the firmware echo.
pub struct PongLoopback {
    out: VecDeque<u8>,
    reader: FrameReader,
}

impl PongLoopback {
    pub fn new() -> Self {
        PongLoopback {
            out: VecDeque::new(),
            reader: FrameReader::new(),
        }
    }
}

impl Default for PongLoopback {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for PongLoopback {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.reader.feed(buf);
        while let Some(f) = self.reader.next_frame() {
            if f.typ == MsgType::Ping {
                let mut enc = [0u8; 1100];
                let n =
                    encode(MsgType::Pong, 0, f.seq, &f.payload, &mut enc).expect("pong encodes");
                self.out.extend(&enc[..n]);
            }
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match self.out.pop_front() {
                Some(b) => {
                    buf[n] = b;
                    n += 1;
                }
                None => break,
            }
        }
        Ok(n)
    }
}
