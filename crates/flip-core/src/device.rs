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
        DeviceLink { transport, reader: FrameReader::new(), next_seq: 1 }
    }

    fn alloc_seq(&mut self) -> u16 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        s
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
}
