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
    expect_capture_stream_start(&mut conn)?;
    let mut raw = Vec::new();
    let mut last_data = Instant::now();
    let mut saw_final_stop = false;

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
            Some((MsgType::StreamStop, payload)) => {
                handle_capture_stop(&payload)?;
                saw_final_stop = true;
                break;
            }
            Some((MsgType::Error, payload)) => return Err(decode_agent_error(&payload)),
            Some((other, _)) => {
                return Err(anyhow!("unexpected frame during IR capture: {other:?}"))
            }
            None => {}
        }

        if let Some(gap) = auto_end {
            if !raw.is_empty() && last_data.elapsed() >= gap {
                break;
            }
        }
    }

    if !saw_final_stop {
        conn.send(MsgType::StreamStop, &[])?;
        drain_capture_stop(&mut conn, &mut raw)?;
    }

    let signal = IrSignal::from_capture(raw);
    if signal.timings.is_empty() {
        return Err(anyhow!("no IR signal captured"));
    }
    Ok(signal)
}

fn drain_capture_stop(conn: &mut daemon::StreamConn, raw: &mut Vec<u64>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                signal::decode_stream_data(&payload, raw);
            }
            Some((MsgType::StreamStop, payload)) => {
                handle_capture_stop(&payload)?;
                return Ok(());
            }
            Some((MsgType::Error, payload)) => return Err(decode_agent_error(&payload)),
            Some((other, _)) => {
                return Err(anyhow!(
                    "unexpected frame while stopping IR capture: {other:?}"
                ));
            }
            None => {}
        }

        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for IR capture stop"));
        }
    }
}

fn expect_capture_stream_start(conn: &mut daemon::StreamConn) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some((typ, payload)) = conn.next_frame(Duration::from_millis(50))? {
            return validate_capture_stream_start_frame(typ, &payload);
        }

        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for IR capture stream start"));
        }
    }
}

fn validate_capture_stream_start_frame(typ: MsgType, payload: &[u8]) -> Result<()> {
    match typ {
        MsgType::StreamStart => {
            let start: flip_proto::StreamStart = flip_proto::messages::from_payload(payload)
                .map_err(|e| anyhow!("decode STREAM_START: {e}"))?;
            if start.format != flip_proto::messages::STREAM_FORMAT_RAW_I32_US {
                return Err(anyhow!(
                    "unsupported IR capture stream format {} (expected {})",
                    start.format,
                    flip_proto::messages::STREAM_FORMAT_RAW_I32_US
                ));
            }
            Ok(())
        }
        MsgType::Error => Err(decode_agent_error(payload)),
        other => Err(anyhow!("expected STREAM_START, got {other:?}")),
    }
}

fn handle_capture_stop(payload: &[u8]) -> Result<()> {
    let stop: flip_proto::StreamStop = flip_proto::messages::from_payload(payload)
        .map_err(|e| anyhow!("decode STREAM_STOP: {e}"))?;
    if stop.dropped > 0 {
        return Err(anyhow!(
            "IR capture dropped {} samples (buffer overflow)",
            stop.dropped
        ));
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

    #[test]
    fn capture_stop_allows_clean_stop() {
        let payload = flip_proto::messages::to_payload(&flip_proto::StreamStop { dropped: 0 });

        assert!(handle_capture_stop(&payload).is_ok());
    }

    #[test]
    fn capture_stop_rejects_dropped_samples() {
        let payload = flip_proto::messages::to_payload(&flip_proto::StreamStop { dropped: 2 });
        let err = handle_capture_stop(&payload).unwrap_err();

        assert_eq!(
            err.to_string(),
            "IR capture dropped 2 samples (buffer overflow)"
        );
    }

    #[test]
    fn capture_stop_rejects_undecodable_payload() {
        let err = handle_capture_stop(&[0xff]).unwrap_err();

        assert!(err.to_string().starts_with("decode STREAM_STOP:"));
    }

    #[test]
    fn capture_stream_start_accepts_raw_i32_format() {
        let payload = flip_proto::messages::to_payload(&flip_proto::StreamStart {
            format: flip_proto::messages::STREAM_FORMAT_RAW_I32_US.to_string(),
        });

        assert!(validate_capture_stream_start_frame(MsgType::StreamStart, &payload).is_ok());
    }

    #[test]
    fn capture_stream_start_rejects_wrong_format() {
        let payload = flip_proto::messages::to_payload(&flip_proto::StreamStart {
            format: "raw_f32".to_string(),
        });
        let err = validate_capture_stream_start_frame(MsgType::StreamStart, &payload).unwrap_err();

        assert_eq!(
            err.to_string(),
            "unsupported IR capture stream format raw_f32 (expected raw_int32_le_us)"
        );
    }

    #[test]
    fn capture_stream_start_rejects_unexpected_frame() {
        let err = validate_capture_stream_start_frame(MsgType::Resp, &[]).unwrap_err();

        assert_eq!(err.to_string(), "expected STREAM_START, got Resp");
    }
}
