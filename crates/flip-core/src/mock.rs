use crate::transport::{FrameReader, Transport};
use anyhow::Result;
use flip_proto::messages::{
    from_payload, to_payload, Caps, Instrument, Req, Resp, PROTOCOL_VERSION,
};
use flip_proto::{encode, MsgType, Value};
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

/// In-memory transport answering HELLO->CAPS and REQ(sys.echo)->RESP(params).
pub struct ControlLoopback {
    out: VecDeque<u8>,
    reader: FrameReader,
}

impl ControlLoopback {
    pub fn new() -> Self {
        ControlLoopback {
            out: VecDeque::new(),
            reader: FrameReader::new(),
        }
    }
    fn queue(&mut self, typ: MsgType, seq: u16, body: &[u8]) {
        let mut enc = vec![0u8; flip_proto::HEADER_SIZE + body.len() + 2];
        let n = flip_proto::encode(typ, 0, seq, body, &mut enc).unwrap();
        self.out.extend(&enc[..n]);
    }
}

impl Default for ControlLoopback {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for ControlLoopback {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.reader.feed(buf);
        while let Some(f) = self.reader.next_frame() {
            match f.typ {
                MsgType::Hello => {
                    let caps = Caps {
                        protocol_version: PROTOCOL_VERSION,
                        instruments: vec![Instrument {
                            id: "sys".to_string(),
                            opcodes: vec!["version".to_string(), "echo".to_string()],
                        }],
                    };
                    let body = to_payload(&caps);
                    self.queue(MsgType::Caps, f.seq, &body);
                }
                MsgType::Req => {
                    let req: Req = from_payload(&f.payload).unwrap();
                    let result = if req.opcode == "echo" {
                        req.params
                    } else {
                        Value::Null
                    };
                    let body = to_payload(&Resp { ok: true, result });
                    self.queue(MsgType::Resp, f.seq, &body);
                }
                _ => {}
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
