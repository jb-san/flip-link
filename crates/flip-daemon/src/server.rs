use crate::device_conn;
use crate::router::Router;
use anyhow::{Context, Result};
use flip_core::transport::{FrameReader, OwnedFrame};
use flip_proto::MsgType;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Daemon socket path: $XDG_RUNTIME_DIR/flip-link.sock, else /tmp/flip-link.sock.
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("flip-link.sock");
    }
    PathBuf::from("/tmp/flip-link.sock")
}

/// Start the device owner, then accept clients. One thread per client.
pub fn run() -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    eprintln!("flip-daemon listening on {}", path.display());

    let router = Arc::new(Router::new());
    let (outbound_tx, outbound_rx) = channel::<Vec<u8>>();
    // `wake` lets a client thread poke the (possibly idle) device owner so it
    // retries connecting when a command arrives.
    let (wake_tx, wake_rx) = channel::<()>();
    {
        let router = router.clone();
        std::thread::spawn(move || device_conn::run(router, outbound_rx, wake_rx));
    }

    for stream in listener.incoming() {
        let stream = stream?;
        let router = router.clone();
        let outbound = outbound_tx.clone();
        let wake = wake_tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_client(stream, router, outbound, wake) {
                eprintln!("client session ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// Serve one client: HELLO answered from cached CAPS; other frames proxied to
/// the device with a rewritten seq so the reply routes back to this client.
fn serve_client(
    mut stream: UnixStream,
    router: Arc<Router>,
    outbound: Sender<Vec<u8>>,
    wake: Sender<()>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 1024];
    // Per-client channel for routed device replies.
    let (reply_tx, reply_rx) = channel::<OwnedFrame>();

    loop {
        match stream.read(&mut scratch) {
            Ok(0) => return Ok(()), // client closed
            Ok(n) => {
                reader.feed(&scratch[..n]);
                // A command arrived — wake the device owner if it's idle so it
                // retries connecting.
                let _ = wake.send(());
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }

        while let Some(frame) = reader.next_frame() {
            if frame.typ == MsgType::Hello {
                // Answer from cache without touching the device. If the device
                // hasn't connected/handshaked yet, CAPS is empty — report that
                // honestly instead of sending an undecodable empty body.
                let caps = router.caps();
                if caps.is_empty() {
                    let body = flip_proto::messages::to_payload(&flip_proto::AgentError {
                        code: flip_proto::messages::ERR_INTERNAL,
                        message: "device not connected".into(),
                    });
                    write_frame(&mut stream, MsgType::Error, frame.seq, &body)?;
                } else {
                    write_frame(&mut stream, MsgType::Caps, frame.seq, &caps)?;
                }
                continue;
            }
            // Proxy: rewrite seq, forward to device, await the first reply.
            let client_seq = frame.seq;
            let dev_seq = router.register(reply_tx.clone());
            forward(&outbound, frame.typ, frame.flags, dev_seq, &frame.payload);

            match reply_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(reply) if reply.typ == MsgType::StreamStart => {
                    // Streaming: relay until the final STREAM_STOP.
                    write_frame(&mut stream, reply.typ, client_seq, &reply.payload)?;
                    relay_stream(
                        &mut stream,
                        &reply_rx,
                        &router,
                        &outbound,
                        client_seq,
                        dev_seq,
                    )?;
                }
                Ok(reply) => {
                    // One-shot RESP/ERROR.
                    router.unregister(dev_seq);
                    write_frame(&mut stream, reply.typ, client_seq, &reply.payload)?;
                }
                Err(_) => {
                    router.unregister(dev_seq);
                    let body = flip_proto::messages::to_payload(&flip_proto::AgentError {
                        code: flip_proto::messages::ERR_INTERNAL,
                        message: "device timeout".into(),
                    });
                    write_frame(&mut stream, MsgType::Error, client_seq, &body)?;
                }
            }
        }
    }
}

fn write_frame(stream: &mut UnixStream, typ: MsgType, seq: u16, payload: &[u8]) -> Result<()> {
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + payload.len() + 2];
    let n = flip_proto::encode(typ, 0, seq, payload, &mut buf).expect("frame");
    stream.write_all(&buf[..n])?;
    Ok(())
}

/// Frame `payload` with a (rewritten) seq and queue it to the device.
fn forward(outbound: &Sender<Vec<u8>>, typ: MsgType, flags: u8, seq: u16, payload: &[u8]) {
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + payload.len() + 2];
    let n = flip_proto::encode(typ, flags, seq, payload, &mut buf).expect("reframe");
    let _ = outbound.send(buf[..n].to_vec());
}

/// Bidirectional relay for an active stream: device STREAM_DATA/STOP -> client,
/// client STREAM_STOP -> device. Ends when the device sends the final
/// STREAM_STOP. The client socket already has a 50ms read timeout.
fn relay_stream(
    stream: &mut UnixStream,
    reply_rx: &std::sync::mpsc::Receiver<OwnedFrame>,
    router: &Router,
    outbound: &Sender<Vec<u8>>,
    client_seq: u16,
    dev_seq: u16,
) -> Result<()> {
    let mut scratch = [0u8; 1024];
    let mut reader = FrameReader::new();
    loop {
        // Client -> device: forward a STREAM_STOP (rewriting seq) to end capture.
        match stream.read(&mut scratch) {
            Ok(0) => {
                // Client hung up mid-stream: ask the device to stop, then exit.
                forward(outbound, MsgType::StreamStop, 0, dev_seq, &[]);
                router.unregister(dev_seq);
                return Ok(());
            }
            Ok(n) => {
                reader.feed(&scratch[..n]);
                while let Some(f) = reader.next_frame() {
                    if f.typ == MsgType::StreamStop {
                        forward(outbound, MsgType::StreamStop, 0, dev_seq, &[]);
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                router.unregister(dev_seq);
                return Err(e.into());
            }
        }

        // Device -> client: relay stream frames; the final STREAM_STOP ends it.
        match reply_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(f) => {
                write_frame(stream, f.typ, client_seq, &f.payload)?;
                if f.typ == MsgType::StreamStop {
                    router.unregister(dev_seq);
                    return Ok(());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                router.unregister(dev_seq);
                return Ok(());
            }
        }
    }
}
