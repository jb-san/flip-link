use anyhow::{anyhow, Context, Result};
use flip_core::transport::{FrameReader, Transport};
use flip_proto::{encode, MsgType};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("flip-link.sock");
    }
    PathBuf::from("/tmp/flip-link.sock")
}

/// A UnixStream wrapped as a Transport so we can reuse DeviceLink-style framing.
struct StreamTransport(UnixStream);

impl Transport for StreamTransport {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        Write::write_all(&mut self.0, buf)?;
        self.0.flush()?;
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.0.read(buf) {
            Ok(n) => Ok(n),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(0)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Connect to the daemon, spawning it if the socket is absent/dead.
pub fn connect() -> Result<UnixStream> {
    let path = socket_path();
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    // Spawn the daemon and wait for the socket to come up.
    spawn_daemon()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("daemon did not come up at {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_daemon() -> Result<()> {
    // Prefer a sibling `flip-daemon` next to this binary; fall back to PATH.
    let exe = std::env::current_exe().context("current exe")?;
    let candidate = exe.with_file_name("flip-daemon");
    let program = if candidate.exists() {
        candidate
    } else {
        PathBuf::from("flip-daemon")
    };
    Command::new(program)
        .arg("run")
        .spawn()
        .context("spawn flip-daemon")?;
    Ok(())
}

/// Round-trip a single framed control message through the daemon, returning the
/// reply frame (typ + payload). Used by caps()/invoke().
fn round_trip(typ: MsgType, payload: &[u8], timeout: Duration) -> Result<(MsgType, Vec<u8>)> {
    let stream = connect()?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut t = StreamTransport(stream);
    let mut reader = FrameReader::new();

    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + payload.len() + 2];
    let n = encode(typ, 0, 1, payload, &mut buf).ok_or_else(|| anyhow!("payload too big"))?;
    t.write_all(&buf[..n])?;

    let deadline = Instant::now() + timeout;
    let mut scratch = [0u8; 1024];
    loop {
        if let Some(f) = reader.next_frame() {
            return Ok((f.typ, f.payload));
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for reply via daemon"));
        }
        let got = t.read(&mut scratch)?;
        if got > 0 {
            reader.feed(&scratch[..got]);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Fetch capabilities (HELLO -> CAPS) via the daemon.
pub fn caps(timeout: Duration) -> Result<flip_proto::Caps> {
    let hello = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
    let (typ, payload) = round_trip(MsgType::Hello, &hello, timeout)?;
    match typ {
        MsgType::Caps => {
            flip_proto::messages::from_payload(&payload).map_err(|e| anyhow!("decode CAPS: {e}"))
        }
        other => Err(anyhow!("expected CAPS, got {:?}", other)),
    }
}

/// Ping the device through the daemon. Returns the echoed payload.
pub fn ping_through_daemon(payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let stream = connect()?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut t = StreamTransport(stream);
    let mut reader = FrameReader::new();

    let mut enc = [0u8; 1100];
    let n =
        encode(MsgType::Ping, 0, 1, payload, &mut enc).ok_or_else(|| anyhow!("payload too big"))?;
    t.write_all(&enc[..n])?;

    let deadline = Instant::now() + timeout;
    let mut scratch = [0u8; 512];
    loop {
        if let Some(f) = reader.next_frame() {
            if f.typ == MsgType::Pong && f.seq == 1 {
                return Ok(f.payload);
            }
            continue;
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for pong via daemon"));
        }
        let got = t.read(&mut scratch)?;
        if got > 0 {
            reader.feed(&scratch[..got]);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
