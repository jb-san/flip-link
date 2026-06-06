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
            device.write_all(&bytes)?;
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
