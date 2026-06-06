use anyhow::{anyhow, Context, Result};
use flip_core::transport::{FrameReader, Transport};
use flip_proto::{encode, MsgType};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("flip-link.sock");
    }
    PathBuf::from("/tmp/flip-link.sock")
}

/// Where an auto-spawned daemon's logs go (so they don't pollute the terminal).
pub fn log_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("flip-daemon.log");
    }
    PathBuf::from("/tmp/flip-daemon.log")
}

/// Connect to the daemon socket WITHOUT spawning one. `Ok(None)` = not running.
pub fn try_connect() -> Option<UnixStream> {
    UnixStream::connect(socket_path()).ok()
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
    // Redirect the daemon's output to a log file so its reconnect chatter never
    // lands in the user's terminal. `flip daemon status` (or tailing the log)
    // surfaces it on demand.
    let (out, err) = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        Ok(f) => match f.try_clone() {
            Ok(f2) => (Stdio::from(f), Stdio::from(f2)),
            Err(_) => (Stdio::from(f), Stdio::null()),
        },
        Err(_) => (Stdio::null(), Stdio::null()),
    };
    Command::new(program)
        .arg("run")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .context("spawn flip-daemon")?;
    Ok(())
}

/// Report daemon + device status WITHOUT spawning a daemon. Prints one block.
#[allow(dead_code)]
pub fn daemon_status() {
    let stream = match try_connect() {
        Some(s) => s,
        None => {
            println!("daemon: not running");
            println!("log:    {}", log_path().display());
            return;
        }
    };
    println!("daemon: running");
    // Ask for CAPS (HELLO). Connected device -> CAPS; otherwise the daemon
    // replies ERROR "device not connected".
    if stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .is_err()
    {
        println!("device: unknown (socket error)");
        return;
    }
    let mut t = StreamTransport(stream);
    let mut reader = FrameReader::new();
    let hello = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + hello.len() + 2];
    let n = encode(MsgType::Hello, 0, 1, &hello, &mut buf).unwrap();
    if t.write_all(&buf[..n]).is_err() {
        println!("device: unknown (write error)");
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut scratch = [0u8; 1024];
    loop {
        if let Some(f) = reader.next_frame() {
            match f.typ {
                MsgType::Caps => {
                    match flip_proto::messages::from_payload::<flip_proto::Caps>(&f.payload) {
                        Ok(caps) => {
                            println!("device: connected ({} instruments)", caps.instruments.len())
                        }
                        Err(_) => println!("device: connected (CAPS undecodable)"),
                    }
                }
                _ => println!("device: disconnected"),
            }
            println!("log:    {}", log_path().display());
            return;
        }
        if Instant::now() >= deadline {
            println!("device: unknown (no reply)");
            return;
        }
        match t.read(&mut scratch) {
            Ok(got) if got > 0 => reader.feed(&scratch[..got]),
            Ok(_) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                println!("device: unknown (read error)");
                return;
            }
        }
    }
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

/// Invoke an opcode (REQ -> RESP/ERROR) via the daemon.
pub fn invoke(
    instrument: &str,
    opcode: &str,
    params: flip_proto::Value,
    timeout: Duration,
) -> Result<flip_proto::Resp> {
    let req = flip_proto::Req {
        instrument: instrument.to_string(),
        opcode: opcode.to_string(),
        params,
    };
    let body = flip_proto::messages::to_payload(&req);
    let (typ, payload) = round_trip(MsgType::Req, &body, timeout)?;
    match typ {
        MsgType::Resp => {
            flip_proto::messages::from_payload(&payload).map_err(|e| anyhow!("decode RESP: {e}"))
        }
        MsgType::Error => {
            let e: flip_proto::AgentError = flip_proto::messages::from_payload(&payload)
                .map_err(|e| anyhow!("decode ERROR: {e}"))?;
            Err(anyhow!("device error {}: {}", e.code, e.message))
        }
        other => Err(anyhow!("expected RESP/ERROR, got {:?}", other)),
    }
}

/// A persistent daemon connection for streaming (capture). Owns one socket.
pub struct StreamConn {
    transport: StreamTransport,
    reader: FrameReader,
}

impl StreamConn {
    /// Send a framed control message (seq fixed at 1 for the single stream).
    pub fn send(&mut self, typ: MsgType, payload: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; flip_proto::HEADER_SIZE + payload.len() + 2];
        let n = encode(typ, 0, 1, payload, &mut buf).ok_or_else(|| anyhow!("payload too big"))?;
        self.transport.write_all(&buf[..n])
    }

    /// Read the next frame, waiting up to `timeout`. `Ok(None)` = nothing yet.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<(MsgType, Vec<u8>)>> {
        let deadline = Instant::now() + timeout;
        let mut scratch = [0u8; 1024];
        loop {
            if let Some(f) = self.reader.next_frame() {
                return Ok(Some((f.typ, f.payload)));
            }
            if Instant::now() >= deadline {
                return Ok(None);
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

/// Open a streaming connection by sending a REQ; returns the connection so the
/// caller can read STREAM_* frames. (The daemon switches the route to a stream
/// relay when the device replies STREAM_START.)
pub fn open_stream(
    instrument: &str,
    opcode: &str,
    params: flip_proto::Value,
) -> Result<StreamConn> {
    let stream = connect()?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut transport = StreamTransport(stream);
    let req = flip_proto::Req {
        instrument: instrument.to_string(),
        opcode: opcode.to_string(),
        params,
    };
    let body = flip_proto::messages::to_payload(&req);
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + body.len() + 2];
    let n =
        encode(MsgType::Req, 0, 1, &body, &mut buf).ok_or_else(|| anyhow!("payload too big"))?;
    transport.write_all(&buf[..n])?;
    Ok(StreamConn {
        transport,
        reader: FrameReader::new(),
    })
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
