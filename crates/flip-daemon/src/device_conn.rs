//! Owns the serial link to the device: connect, HELLO->CAPS, read/route.
//!
//! Reconnect policy: after the device drops, retry the connect a bounded number
//! of times, then go IDLE (stop trying, stop logging) until a client command
//! pokes the `wake` channel — at which point we retry again. This avoids the
//! daemon spinning forever (and spamming logs) while the FAP is not running.

use crate::router::Router;
use anyhow::{Context, Result};
use flip_core::serial::{pick_agent_port, SerialTransport};
use flip_core::transport::{FrameReader, Transport};
use flip_proto::MsgType;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// Connect attempts before going idle (then we wait for a client poke).
const MAX_RETRIES: u32 = 5;
/// Match the Flipper CDC endpoint size and pace large frames so the FAP can
/// drain its small ISR-to-main-loop RX stream buffer.
const OUTBOUND_WRITE_CHUNK: usize = 64;
const OUTBOUND_WRITE_PAUSE: Duration = Duration::from_millis(2);

/// Run the device owner forever. `outbound` carries already-framed bytes from
/// clients; `wake` is pinged by a client thread whenever a command arrives.
pub fn run(router: Arc<Router>, outbound: Receiver<Vec<u8>>, wake: Receiver<()>) -> ! {
    loop {
        // Bounded connect attempts; a fresh count each reconnect cycle.
        let mut device = None;
        for attempt in 1..=MAX_RETRIES {
            match connect_and_cache(&router) {
                Ok(dev) => {
                    device = Some(dev);
                    break;
                }
                Err(e) => {
                    if attempt == MAX_RETRIES {
                        eprintln!(
                            "device unreachable after {MAX_RETRIES} attempts; idle until next command"
                        );
                    } else {
                        eprintln!("connect failed: {e:#}; retry {attempt}/{MAX_RETRIES} in 500ms");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        match device {
            Some(dev) => {
                if let Err(e) = pump(dev, &router, &outbound) {
                    eprintln!("device session ended: {e:#}");
                }
                // Loop back and reconnect immediately (fresh attempt count).
            }
            None => {
                // Idle: block until a client command arrives, then retry. Drain
                // any extra pokes so we attempt exactly once per idle wake.
                router.set_caps(Vec::new()); // CAPS is stale while disconnected
                let _ = wake.recv();
                while wake.try_recv().is_ok() {}
            }
        }
    }
}

/// Open the agent port and complete the HELLO->CAPS handshake (caching CAPS).
fn connect_and_cache(router: &Router) -> Result<SerialTransport> {
    let port = pick_agent_port().context("find Flipper agent port")?;
    let mut device = SerialTransport::open(&port).context("open device")?;
    cache_caps(router, &mut device)?;
    eprintln!("flip-daemon connected to device on {port}");
    Ok(device)
}

/// Pump frames until a serial error: drain queued outbound to the device, read
/// inbound, route whole frames to their client by seq.
fn pump(mut device: SerialTransport, router: &Router, outbound: &Receiver<Vec<u8>>) -> Result<()> {
    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 1024];
    loop {
        while let Ok(bytes) = outbound.try_recv() {
            write_outbound_frame(&mut device, &bytes, || {
                std::thread::sleep(OUTBOUND_WRITE_PAUSE)
            })?;
        }
        let got = device.read(&mut scratch)?;
        if got > 0 {
            reader.feed(&scratch[..got]);
            while let Some(frame) = reader.next_frame() {
                router.deliver(frame);
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn write_outbound_frame(
    device: &mut impl Transport,
    bytes: &[u8],
    mut pause_between_chunks: impl FnMut(),
) -> Result<()> {
    for (index, chunk) in bytes.chunks(OUTBOUND_WRITE_CHUNK).enumerate() {
        if index > 0 {
            pause_between_chunks();
        }
        device.write_all(chunk)?;
    }
    Ok(())
}

/// Send HELLO and wait for CAPS, caching its payload. Times out after 2s.
fn cache_caps(router: &Router, device: &mut SerialTransport) -> Result<()> {
    let hello = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + hello.len() + 2];
    let n = flip_proto::encode(MsgType::Hello, 0, 0, &hello, &mut buf).unwrap();
    device.write_all(&buf[..n])?;

    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 512];
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(f) = reader.next_frame() {
            if f.typ == MsgType::Caps {
                router.set_caps(f.payload);
                eprintln!("flip-daemon cached CAPS ({} bytes)", router.caps().len());
                return Ok(());
            }
            continue;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for CAPS");
        }
        let got = device.read(&mut scratch)?;
        if got > 0 {
            reader.feed(&scratch[..got]);
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flip_core::transport::Transport;

    #[derive(Default)]
    struct MockTransport {
        writes: Vec<Vec<u8>>,
    }

    impl Transport for MockTransport {
        fn write_all(&mut self, buf: &[u8]) -> Result<()> {
            self.writes.push(buf.to_vec());
            Ok(())
        }

        fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Ok(0)
        }
    }

    #[test]
    fn outbound_frames_are_written_in_cdc_sized_chunks() {
        let bytes: Vec<u8> = (0..150).map(|n| n as u8).collect();
        let mut transport = MockTransport::default();
        let mut pauses = 0;

        write_outbound_frame(&mut transport, &bytes, || pauses += 1).unwrap();

        assert_eq!(
            transport.writes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![64, 64, 22]
        );
        assert_eq!(transport.writes.concat(), bytes);
        assert_eq!(pauses, 2);
    }
}
