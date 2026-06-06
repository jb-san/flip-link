//! Owns the serial link to the device: connect, HELLO->CAPS, read/route, reconnect.

use crate::router::Router;
use anyhow::{Context, Result};
use flip_core::serial::{pick_agent_port, SerialTransport};
use flip_core::transport::{FrameReader, Transport};
use flip_proto::MsgType;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// Run the device owner forever: (re)connect, then pump frames until an error,
/// then reconnect. `outbound` carries already-framed bytes from clients.
pub fn run(router: Arc<Router>, outbound: Receiver<Vec<u8>>) -> ! {
    loop {
        match session(&router, &outbound) {
            Ok(()) => {}
            Err(e) => eprintln!("device session ended: {e:#}; reconnecting in 500ms"),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn session(router: &Router, outbound: &Receiver<Vec<u8>>) -> Result<()> {
    let port = pick_agent_port().context("find Flipper agent port")?;
    let mut device = SerialTransport::open(&port).context("open device")?;
    eprintln!("flip-daemon connected to device on {port}");

    // HELLO -> CAPS handshake; cache the CAPS body for clients.
    cache_caps(router, &mut device)?;

    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 1024];
    loop {
        // Drain any queued outbound frames to the device (non-blocking).
        while let Ok(bytes) = outbound.try_recv() {
            device.write_all(&bytes)?;
        }
        // Read inbound and route whole frames.
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
