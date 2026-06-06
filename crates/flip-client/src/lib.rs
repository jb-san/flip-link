mod daemon;
pub mod signal;

use anyhow::{anyhow, Result};
use flip_proto::{Caps, MsgType, Resp, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use daemon::ping_through_daemon;
pub use signal::IrSignal;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceStatus {
    Connected { instruments: usize },
    Disconnected,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonStatus {
    pub daemon_running: bool,
    pub device: DeviceStatus,
    pub log_path: PathBuf,
}

pub fn status() -> DaemonStatus {
    daemon::status()
}

pub fn caps(timeout: Duration) -> Result<Caps> {
    daemon::caps(timeout)
}

pub fn invoke(instrument: &str, opcode: &str, params: Value, timeout: Duration) -> Result<Resp> {
    daemon::invoke(instrument, opcode, params, timeout)
}

pub fn ir_transmit(signal: &IrSignal, timeout: Duration) -> Result<u64> {
    let resp = invoke("ir", "transmit", signal.to_transmit_params(), timeout)?;
    sent_count(&resp.result)
}

pub fn ir_capture(auto_end: Option<Duration>, cancel: &dyn Fn() -> bool) -> Result<IrSignal> {
    let mut conn = daemon::open_stream("ir", "capture", Value::Null)?;
    let mut raw = Vec::new();
    let mut last_data = Instant::now();

    loop {
        if cancel() {
            break;
        }
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                let n = signal::decode_stream_data(&payload, &mut raw);
                if n > 0 {
                    last_data = Instant::now();
                }
            }
            Some((MsgType::StreamStop, _)) => break,
            Some((MsgType::Error, payload)) => return Err(decode_agent_error(&payload)),
            Some(_) | None => {}
        }

        if let Some(gap) = auto_end {
            if !raw.is_empty() && last_data.elapsed() >= gap {
                break;
            }
        }
    }

    conn.send(MsgType::StreamStop, &[])?;
    drain_capture_stop(&mut conn, &mut raw)?;

    let signal = IrSignal::from_capture(raw);
    if signal.timings.is_empty() {
        return Err(anyhow!("no IR signal captured"));
    }
    Ok(signal)
}

fn drain_capture_stop(conn: &mut daemon::StreamConn, raw: &mut Vec<u64>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                signal::decode_stream_data(&payload, raw);
            }
            Some((MsgType::StreamStop, payload)) => {
                if let Ok(stop) =
                    flip_proto::messages::from_payload::<flip_proto::StreamStop>(&payload)
                {
                    if stop.dropped > 0 {
                        eprintln!(
                            "warning: {} samples dropped (buffer overflow)",
                            stop.dropped
                        );
                    }
                }
                break;
            }
            Some((MsgType::Error, payload)) => return Err(decode_agent_error(&payload)),
            _ => {}
        }

        if Instant::now() >= deadline {
            break;
        }
    }
    Ok(())
}

fn decode_agent_error(payload: &[u8]) -> anyhow::Error {
    match flip_proto::messages::from_payload::<flip_proto::AgentError>(payload) {
        Ok(e) => anyhow!("device error {}: {}", e.code, e.message),
        Err(e) => anyhow!("decode ERROR: {e}"),
    }
}

fn sent_count(result: &Value) -> Result<u64> {
    match result {
        Value::U64(n) => Ok(*n),
        Value::Map(fields) => fields
            .iter()
            .find(|(key, _)| key == "sent")
            .and_then(|(_, value)| match value {
                Value::U64(n) => Some(*n),
                _ => None,
            })
            .ok_or_else(|| anyhow!("ir.transmit response missing numeric sent field")),
        _ => Err(anyhow!("ir.transmit response was not a sent count")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_count_accepts_current_firmware_map() {
        assert_eq!(
            sent_count(&Value::Map(vec![("sent".to_string(), Value::U64(4))])).unwrap(),
            4
        );
    }

    #[test]
    fn sent_count_accepts_future_plain_u64() {
        assert_eq!(sent_count(&Value::U64(4)).unwrap(), 4);
    }

    #[test]
    fn sent_count_rejects_wrong_shape() {
        assert!(sent_count(&Value::Null).is_err());
        assert!(sent_count(&Value::Map(vec![(
            "sent".to_string(),
            Value::Text("4".into())
        )]))
        .is_err());
    }
}
