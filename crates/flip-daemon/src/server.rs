use anyhow::{Context, Result};
use flip_core::serial::{pick_agent_port, SerialTransport};
use flip_core::transport::{FrameReader, Transport};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

/// Daemon socket path: $XDG_RUNTIME_DIR/flip-link.sock, else /tmp/flip-link.sock.
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("flip-link.sock");
    }
    PathBuf::from("/tmp/flip-link.sock")
}

/// Run the daemon: open the device, accept clients, relay frames both ways.
/// Slice 0: one client at a time, byte-for-byte relay (client owns the protocol).
pub fn run() -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // clear stale socket
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    eprintln!("flip-daemon listening on {}", path.display());

    let port = pick_agent_port().context("find Flipper agent port")?;
    let mut device = SerialTransport::open(&port).context("open device")?;
    eprintln!("flip-daemon connected to device on {port}");

    for stream in listener.incoming() {
        let mut client = stream?;
        if let Err(e) = relay_one(&mut client, &mut device) {
            eprintln!("client session ended: {e:#}");
        }
    }
    Ok(())
}

/// Relay: read frames from the client, forward raw bytes to the device, read
/// device frames back, forward to the client. Ends when the client disconnects.
fn relay_one(client: &mut UnixStream, device: &mut SerialTransport) -> Result<()> {
    client.set_read_timeout(Some(Duration::from_millis(20)))?;
    let mut from_client = [0u8; 1024];
    let mut from_device = [0u8; 1024];
    let mut dev_reader = FrameReader::new();

    loop {
        // Client -> device (forward raw bytes; client already framed them).
        match client.read(&mut from_client) {
            Ok(0) => return Ok(()), // client closed
            Ok(n) => device.write_all(&from_client[..n])?,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }

        // Device -> client (reframe to forward only whole frames).
        let got = device.read(&mut from_device)?;
        if got > 0 {
            dev_reader.feed(&from_device[..got]);
            while let Some(f) = dev_reader.next_frame() {
                let mut enc = [0u8; 1100];
                let n = flip_proto::encode(f.typ, f.flags, f.seq, &f.payload, &mut enc)
                    .expect("reframe");
                client.write_all(&enc[..n])?;
            }
        }
    }
}
