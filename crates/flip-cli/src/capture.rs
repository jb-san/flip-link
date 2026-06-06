//! `flip ir capture`: open a capture stream, read timings until Ctrl-C or a
//! silence gap, then write them in the `ir transmit --file` format.

use crate::client;
use crate::ir;
use anyhow::{anyhow, Result};
use flip_proto::MsgType;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run a capture. `auto_end_ms`: if `Some(ms)`, stop after that much silence;
/// otherwise run until Ctrl-C. `output`: file path, or `None` for stdout.
pub fn run(auto_end_ms: Option<u64>, output: Option<&str>) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    let mut conn = client::open_stream("ir", "capture", flip_proto::Value::Null)?;
    eprintln!("capturing… (Ctrl-C to stop)");

    let mut timings: Vec<u64> = Vec::new();
    let mut last_data = Instant::now();
    let auto_end = auto_end_ms.map(Duration::from_millis);

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                let n = ir::decode_stream_data(&payload, &mut timings);
                if n > 0 {
                    last_data = Instant::now();
                }
            }
            Some((MsgType::StreamStop, _)) => break,
            Some((MsgType::Error, payload)) => {
                let e: flip_proto::AgentError = flip_proto::messages::from_payload(&payload)
                    .map_err(|e| anyhow!("decode ERROR: {e}"))?;
                return Err(anyhow!("device error {}: {}", e.code, e.message));
            }
            Some(_) => {}
            None => {}
        }
        if let Some(gap) = auto_end {
            if !timings.is_empty() && last_data.elapsed() >= gap {
                break;
            }
        }
    }

    // Ask the device to stop and drain the final frames (incl. STREAM_STOP).
    conn.send(MsgType::StreamStop, &[])?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                ir::decode_stream_data(&payload, &mut timings);
            }
            Some((MsgType::StreamStop, payload)) => {
                if let Ok(s) = flip_proto::messages::from_payload::<flip_proto::StreamStop>(&payload)
                {
                    if s.dropped > 0 {
                        eprintln!("warning: {} samples dropped (buffer overflow)", s.dropped);
                    }
                }
                break;
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    if timings.is_empty() {
        return Err(anyhow!("no IR signal captured"));
    }
    let text = ir::format_timings(&timings);
    match output {
        Some(path) => {
            std::fs::write(path, &text)?;
            eprintln!("captured {} timings -> {path}", timings.len());
        }
        None => std::io::stdout().write_all(text.as_bytes())?,
    }
    Ok(())
}
