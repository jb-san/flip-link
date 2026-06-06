use crate::transport::{FrameReader, Transport};
use anyhow::{anyhow, Result};
use flip_proto::{encode, MsgType};
use std::time::{Duration, Instant};

/// A framed link to the device over any Transport. Owns sequence numbering.
pub struct DeviceLink<T: Transport> {
    transport: T,
    reader: FrameReader,
    next_seq: u16,
}

impl<T: Transport> DeviceLink<T> {
    pub fn new(transport: T) -> Self {
        DeviceLink {
            transport,
            reader: FrameReader::new(),
            next_seq: 1,
        }
    }

    fn alloc_seq(&mut self) -> u16 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        s
    }

    /// Send HELLO and await the CAPS body.
    pub fn hello(&mut self, timeout: Duration) -> Result<flip_proto::Caps> {
        let seq = self.alloc_seq();
        let body = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
        self.send(MsgType::Hello, seq, &body)?;
        let frame = self.await_seq(seq, timeout)?;
        match frame.typ {
            MsgType::Caps => {
                flip_proto::messages::from_payload(&frame.payload).map_err(|e| anyhow!("decode CAPS: {e}"))
            }
            other => Err(anyhow!("expected CAPS, got {:?}", other)),
        }
    }

    /// Send a REQ and await the RESP (Ok) or ERROR (Err with code+message).
    pub fn request(
        &mut self,
        instrument: &str,
        opcode: &str,
        params: flip_proto::Value,
        timeout: Duration,
    ) -> Result<flip_proto::Resp> {
        let seq = self.alloc_seq();
        let req = flip_proto::Req {
            instrument: instrument.into(),
            opcode: opcode.into(),
            params,
        };
        let body = flip_proto::messages::to_payload(&req);
        self.send(MsgType::Req, seq, &body)?;
        let frame = self.await_seq(seq, timeout)?;
        match frame.typ {
            MsgType::Resp => {
                flip_proto::messages::from_payload(&frame.payload).map_err(|e| anyhow!("decode RESP: {e}"))
            }
            MsgType::Error => {
                let e: flip_proto::AgentError = flip_proto::messages::from_payload(&frame.payload)
                    .map_err(|e| anyhow!("decode ERROR: {e}"))?;
                Err(anyhow!("device error {}: {}", e.code, e.message))
            }
            other => Err(anyhow!("expected RESP/ERROR, got {:?}", other)),
        }
    }

    /// Write a framed message body.
    fn send(&mut self, typ: MsgType, seq: u16, body: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; flip_proto::HEADER_SIZE + body.len() + 2];
        let n = encode(typ, 0, seq, body, &mut buf).ok_or_else(|| anyhow!("frame too large"))?;
        self.transport.write_all(&buf[..n])
    }

    /// Read frames until one matches `seq`, or time out.
    fn await_seq(&mut self, seq: u16, timeout: Duration) -> Result<crate::transport::OwnedFrame> {
        let deadline = Instant::now() + timeout;
        let mut scratch = [0u8; 512];
        loop {
            if let Some(f) = self.reader.next_frame() {
                if f.seq == seq {
                    return Ok(f);
                }
                continue;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("timed out waiting for seq {seq}"));
            }
            let got = self.transport.read(&mut scratch)?;
            if got > 0 {
                self.reader.feed(&scratch[..got]);
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Send a PING with `payload` and wait up to `timeout` for the matching PONG.
    /// Returns the echoed payload on success.
    pub fn ping(&mut self, payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        let seq = self.alloc_seq();
        let mut enc = [0u8; 1100];
        let n = encode(MsgType::Ping, 0, seq, payload, &mut enc)
            .ok_or_else(|| anyhow!("ping payload too large"))?;
        self.transport.write_all(&enc[..n])?;

        let deadline = Instant::now() + timeout;
        let mut scratch = [0u8; 512];
        loop {
            if let Some(f) = self.reader.next_frame() {
                if f.typ == MsgType::Pong && f.seq == seq {
                    return Ok(f.payload);
                }
                continue; // ignore unrelated frames
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("ping timed out waiting for pong seq {seq}"));
            }
            let got = self.transport.read(&mut scratch)?;
            if got > 0 {
                self.reader.feed(&scratch[..got]);
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::PongLoopback;

    #[test]
    fn ping_round_trips_via_mock() {
        let mut link = DeviceLink::new(PongLoopback::new());
        let echoed = link.ping(b"hello", Duration::from_millis(500)).unwrap();
        assert_eq!(echoed, b"hello");
    }

    #[test]
    fn ping_seq_increments() {
        let mut link = DeviceLink::new(PongLoopback::new());
        link.ping(b"a", Duration::from_millis(500)).unwrap();
        link.ping(b"b", Duration::from_millis(500)).unwrap();
        // second ping used seq 2; if correlation were broken the mock's seq-echo
        // would mismatch and time out, so reaching here proves correlation works.
    }

    #[test]
    fn hello_returns_caps_via_mock() {
        use crate::mock::ControlLoopback;
        let mut link = DeviceLink::new(ControlLoopback::new());
        let caps = link.hello(Duration::from_millis(500)).unwrap();
        assert_eq!(caps.protocol_version, 1);
        assert_eq!(caps.instruments[0].id, "sys");
    }

    #[test]
    fn request_echo_round_trips_params() {
        use crate::mock::ControlLoopback;
        use flip_proto::Value;
        let mut link = DeviceLink::new(ControlLoopback::new());
        let params = Value::Map(vec![("msg".to_string(), Value::Text("hi".to_string()))]);
        let resp = link
            .request("sys", "echo", params.clone(), Duration::from_millis(500))
            .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result, params);
    }
}
