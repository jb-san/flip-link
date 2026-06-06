# flip-client IR Signal Records Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract reusable daemon-client behavior from `flip-cli` into a new `flip-client` library and add Layer 1 IR signal records that preserve carrier frequency, duty cycle, and mark-first timing alignment.

**Architecture:** Add `crates/flip-client` as the shared host-side client API. Move socket/framing/streaming code from `flip-cli/src/client.rs` into `flip-client/src/daemon.rs`, add `flip-client/src/signal.rs` for `IrSignal` parsing/writing/transmit params, and expose typed `caps`, `invoke`, `status`, `ir_capture`, and `ir_transmit` operations from `flip-client/src/lib.rs`. Then reduce `flip-cli` to Clap wiring plus `kv.rs`, with Ctrl-C handled only in the CLI.

**Tech Stack:** Rust workspace; `anyhow`; `flip-core::transport::FrameReader`; `flip-proto` with `alloc`; Unix domain sockets; existing daemon streaming protocol from Slice 1c.

**Scope:** Host crates only. Firmware and daemon protocol are unchanged. Hardware-free tests cover the signal record and formatting behavior; hardware acceptance remains the CLI capture/replay flow.

---

## File Structure

```
Cargo.toml                         # add crates/flip-client to workspace members
Cargo.lock                         # updated by cargo after adding the new crate
crates/flip-client/Cargo.toml      # new library crate
crates/flip-client/src/lib.rs      # public typed client API
crates/flip-client/src/daemon.rs   # moved daemon socket/framed transport logic
crates/flip-client/src/signal.rs   # IrSignal record, parse/write, capture trim
crates/flip-cli/Cargo.toml         # depend on flip-client; remove flip-core/flip-proto direct deps only if unused
crates/flip-cli/src/main.rs        # thin Clap frontend; local rendering and Ctrl-C flag
crates/flip-cli/src/kv.rs          # unchanged CLI-only k=v parser
crates/flip-cli/src/client.rs      # delete after migration
crates/flip-cli/src/ir.rs          # delete after migration
crates/flip-cli/src/capture.rs     # delete after migration
```

---

## Task 1: Create `flip-client` crate and move daemon plumbing

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/flip-client/Cargo.toml`
- Create: `crates/flip-client/src/lib.rs`
- Create: `crates/flip-client/src/daemon.rs`

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, replace:
```toml
members = ["crates/flip-proto", "crates/flip-core", "crates/flip-daemon", "crates/flip-cli"]
```
with:
```toml
members = [
    "crates/flip-proto",
    "crates/flip-core",
    "crates/flip-daemon",
    "crates/flip-client",
    "crates/flip-cli",
]
```

- [ ] **Step 2: Create `crates/flip-client/Cargo.toml`**

```toml
[package]
name = "flip-client"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
flip-core = { path = "../flip-core" }
flip-proto = { path = "../flip-proto", features = ["alloc"] }
anyhow = { workspace = true }
```

- [ ] **Step 3: Create the temporary library root**

Create `crates/flip-client/src/lib.rs`:
```rust
pub mod daemon;

pub use daemon::{
    caps, connect, invoke, log_path, open_stream, ping_through_daemon, try_connect, StreamConn,
};
```

- [ ] **Step 4: Move daemon code into `daemon.rs`**

Copy the contents of `crates/flip-cli/src/client.rs` into `crates/flip-client/src/daemon.rs`.

Then remove the CLI-only `render_value` function from `daemon.rs`. It starts with:
```rust
/// One-line rendering of a result Value for the CLI.
pub fn render_value(v: &flip_proto::Value) -> String {
```
and ends after its closing `}` before:
```rust
/// A persistent daemon connection for streaming (capture). Owns one socket.
pub struct StreamConn {
```

Keep these daemon-facing public functions in `daemon.rs` for now:
```rust
pub fn log_path() -> PathBuf
pub fn try_connect() -> Option<UnixStream>
pub fn connect() -> Result<UnixStream>
pub fn daemon_status()
pub fn caps(timeout: Duration) -> Result<flip_proto::Caps>
pub fn invoke(
    instrument: &str,
    opcode: &str,
    params: flip_proto::Value,
    timeout: Duration,
) -> Result<flip_proto::Resp>
pub struct StreamConn
pub fn open_stream(
    instrument: &str,
    opcode: &str,
    params: flip_proto::Value,
) -> Result<StreamConn>
pub fn ping_through_daemon(payload: &[u8], timeout: Duration) -> Result<Vec<u8>>
```

- [ ] **Step 5: Run the new crate check**

Run:
```bash
cargo check -p flip-client
```

Expected: PASS. The new library compiles and `Cargo.lock` is updated if Cargo needs to touch it.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/flip-client
git commit -m "feat(client): add flip-client crate with daemon transport"
```

---

## Task 2: Add `IrSignal` record with hardware-free tests

**Files:**
- Modify: `crates/flip-client/src/lib.rs`
- Create: `crates/flip-client/src/signal.rs`

- [ ] **Step 1: Expose the signal module**

In `crates/flip-client/src/lib.rs`, change:
```rust
pub mod daemon;
```
to:
```rust
pub mod daemon;
pub mod signal;
```

and add:
```rust
pub use signal::IrSignal;
```

- [ ] **Step 2: Write failing signal tests**

Create `crates/flip-client/src/signal.rs` with only the type, constants, and tests first:
```rust
use flip_proto::Value;

pub const DEFAULT_FREQUENCY: u32 = 38_000;
pub const DEFAULT_DUTY_PERMILLE: u32 = 330;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSignal {
    pub frequency: u32,
    pub duty_permille: u32,
    pub timings: Vec<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_existing_plain_timings_file_uses_defaults() {
        let signal = IrSignal::parse("# old file\n9000 4500\n560 560\n").unwrap();

        assert_eq!(signal.frequency, DEFAULT_FREQUENCY);
        assert_eq!(signal.duty_permille, DEFAULT_DUTY_PERMILLE);
        assert_eq!(signal.timings, vec![9000, 4500, 560, 560]);
    }

    #[test]
    fn parse_directives_with_trailing_comment_text() {
        let signal = IrSignal::parse(
            "# freq=40000      carrier in Hz\n# duty=250        duty in permille\n900 450\n",
        )
        .unwrap();

        assert_eq!(signal.frequency, 40_000);
        assert_eq!(signal.duty_permille, 250);
        assert_eq!(signal.timings, vec![900, 450]);
    }

    #[test]
    fn rejects_invalid_timing_or_directive_value() {
        assert!(IrSignal::parse("9000 hello").is_err());
        assert!(IrSignal::parse("# freq=nope\n9000").is_err());
        assert!(IrSignal::parse("# duty=nope\n9000").is_err());
        assert!(IrSignal::parse("# only comments\n").is_err());
    }

    #[test]
    fn write_emits_directives_and_twelve_timings_per_line() {
        let signal = IrSignal {
            frequency: 38_000,
            duty_permille: 330,
            timings: (1..=13).collect(),
        };

        assert_eq!(
            signal.to_file_string(),
            "# freq=38000\n# duty=330\n1 2 3 4 5 6 7 8 9 10 11 12\n13\n"
        );
    }

    #[test]
    fn file_format_round_trips() {
        let signal = IrSignal {
            frequency: 36_000,
            duty_permille: 400,
            timings: vec![9000, 4500, 560, 1690],
        };

        assert_eq!(IrSignal::parse(&signal.to_file_string()).unwrap(), signal);
    }

    #[test]
    fn from_capture_drops_leading_idle() {
        let signal = IrSignal::from_capture(vec![123_456, 9000, 4500, 560]);

        assert_eq!(signal.frequency, DEFAULT_FREQUENCY);
        assert_eq!(signal.duty_permille, DEFAULT_DUTY_PERMILLE);
        assert_eq!(signal.timings, vec![9000, 4500, 560]);
    }

    #[test]
    fn empty_capture_returns_empty_default_signal() {
        let signal = IrSignal::from_capture(Vec::new());

        assert_eq!(signal.frequency, DEFAULT_FREQUENCY);
        assert_eq!(signal.duty_permille, DEFAULT_DUTY_PERMILLE);
        assert!(signal.timings.is_empty());
    }

    #[test]
    fn transmit_params_include_frequency_duty_and_timings() {
        let params = IrSignal {
            frequency: 40_000,
            duty_permille: 250,
            timings: vec![560, 1690],
        }
        .to_transmit_params();

        assert_eq!(params.get("frequency"), Some(&Value::U64(40_000)));
        assert_eq!(params.get("duty_permille"), Some(&Value::U64(250)));
        assert_eq!(
            params.get("timings"),
            Some(&Value::Array(vec![Value::U64(560), Value::U64(1690)]))
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:
```bash
cargo test -p flip-client signal
```

Expected: FAIL because `IrSignal::parse`, `IrSignal::from_capture`, `IrSignal::to_file_string`, and `IrSignal::to_transmit_params` do not exist yet.

- [ ] **Step 4: Implement `IrSignal`**

Add these imports near the top of `crates/flip-client/src/signal.rs`:
```rust
use anyhow::{Context, Result};
use std::path::Path;
```

Add this `impl IrSignal` block after the struct:
```rust
impl IrSignal {
    pub fn from_capture(raw: Vec<u64>) -> Self {
        let timings = raw.into_iter().skip(1).collect();
        Self {
            frequency: DEFAULT_FREQUENCY,
            duty_permille: DEFAULT_DUTY_PERMILLE,
            timings,
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut frequency = DEFAULT_FREQUENCY;
        let mut duty_permille = DEFAULT_DUTY_PERMILLE;
        let mut timings = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(comment) = trimmed.strip_prefix('#') {
                let directive = comment.trim();
                if let Some(rest) = directive.strip_prefix("freq=") {
                    frequency = parse_directive_u32("freq", rest)?;
                } else if let Some(rest) = directive.strip_prefix("duty=") {
                    duty_permille = parse_directive_u32("duty", rest)?;
                }
                continue;
            }

            for token in trimmed.split_whitespace() {
                let timing = token.parse::<u64>().map_err(|_| {
                    anyhow::anyhow!("invalid timing '{token}' (expected unsigned integer us)")
                })?;
                timings.push(timing);
            }
        }

        if timings.is_empty() {
            return Err(anyhow::anyhow!("no timings found"));
        }

        Ok(Self {
            frequency,
            duty_permille,
            timings,
        })
    }

    pub fn to_file_string(&self) -> String {
        let mut out = format!("# freq={}\n# duty={}\n", self.frequency, self.duty_permille);
        for (i, timing) in self.timings.iter().enumerate() {
            if i > 0 {
                out.push(if i % 12 == 0 { '\n' } else { ' ' });
            }
            out.push_str(&timing.to_string());
        }
        out.push('\n');
        out
    }

    pub fn read_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn write_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_file_string())
            .with_context(|| format!("write {}", path.display()))
    }

    pub fn to_transmit_params(&self) -> Value {
        Value::Map(vec![
            ("frequency".to_string(), Value::U64(self.frequency as u64)),
            (
                "duty_permille".to_string(),
                Value::U64(self.duty_permille as u64),
            ),
            (
                "timings".to_string(),
                Value::Array(self.timings.iter().copied().map(Value::U64).collect()),
            ),
        ])
    }
}

fn parse_directive_u32(name: &str, rest: &str) -> Result<u32> {
    let token = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing {name} value"))?;
    token
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid {name} value '{token}'"))
}
```

- [ ] **Step 5: Run signal tests**

Run:
```bash
cargo test -p flip-client signal
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/flip-client/src/lib.rs crates/flip-client/src/signal.rs
git commit -m "feat(client): add IR signal record format"
```

---

## Task 3: Add typed client operations

**Files:**
- Modify: `crates/flip-client/src/lib.rs`
- Modify: `crates/flip-client/src/daemon.rs`
- Modify: `crates/flip-client/src/signal.rs`

- [ ] **Step 1: Replace the temporary library root with typed API stubs and tests**

Replace `crates/flip-client/src/lib.rs` with:
```rust
pub mod daemon;
pub mod signal;

use anyhow::{anyhow, Result};
use flip_proto::{Caps, MsgType, Resp, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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

pub fn invoke(
    instrument: &str,
    opcode: &str,
    params: Value,
    timeout: Duration,
) -> Result<Resp> {
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
                if let Ok(stop) = flip_proto::messages::from_payload::<flip_proto::StreamStop>(&payload)
                {
                    if stop.dropped > 0 {
                        eprintln!("warning: {} samples dropped (buffer overflow)", stop.dropped);
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
        assert!(sent_count(&Value::Map(vec![("sent".to_string(), Value::Text("4".into()))])).is_err());
    }
}
```

- [ ] **Step 2: Add stream decode to `signal.rs`**

After `parse_directive_u32`, add:
```rust
pub fn decode_stream_data(payload: &[u8], out: &mut Vec<u64>) -> usize {
    let mut count = 0;
    for chunk in payload.chunks_exact(4) {
        let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        out.push(value.max(0) as u64);
        count += 1;
    }
    count
}
```

Add this test to `signal.rs`:
```rust
    #[test]
    fn decodes_le_i32_stream_samples() {
        let mut out = Vec::new();
        let payload = [0x10, 0x27, 0, 0, 0x2c, 0x01, 0, 0];

        assert_eq!(decode_stream_data(&payload, &mut out), 2);
        assert_eq!(out, vec![10_000, 300]);
    }
```

- [ ] **Step 3: Replace printing status with typed status in `daemon.rs`**

At the top of `crates/flip-client/src/daemon.rs`, add:
```rust
use crate::{DaemonStatus, DeviceStatus};
```

Replace the whole `pub fn daemon_status()` function with:
```rust
pub fn status() -> DaemonStatus {
    let log_path = log_path();
    let stream = match try_connect() {
        Some(s) => s,
        None => {
            return DaemonStatus {
                daemon_running: false,
                device: DeviceStatus::Unknown("daemon not running".to_string()),
                log_path,
            };
        }
    };

    if stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .is_err()
    {
        return DaemonStatus {
            daemon_running: true,
            device: DeviceStatus::Unknown("socket error".to_string()),
            log_path,
        };
    }

    let mut transport = StreamTransport(stream);
    let mut reader = FrameReader::new();
    let hello = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + hello.len() + 2];
    let n = match encode(MsgType::Hello, 0, 1, &hello, &mut buf) {
        Some(n) => n,
        None => {
            return DaemonStatus {
                daemon_running: true,
                device: DeviceStatus::Unknown("HELLO payload too big".to_string()),
                log_path,
            };
        }
    };
    if transport.write_all(&buf[..n]).is_err() {
        return DaemonStatus {
            daemon_running: true,
            device: DeviceStatus::Unknown("write error".to_string()),
            log_path,
        };
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut scratch = [0u8; 1024];
    loop {
        if let Some(frame) = reader.next_frame() {
            let device = match frame.typ {
                MsgType::Caps => match flip_proto::messages::from_payload::<flip_proto::Caps>(&frame.payload) {
                    Ok(caps) => DeviceStatus::Connected {
                        instruments: caps.instruments.len(),
                    },
                    Err(_) => DeviceStatus::Unknown("CAPS undecodable".to_string()),
                },
                MsgType::Error => DeviceStatus::Disconnected,
                _ => DeviceStatus::Disconnected,
            };
            return DaemonStatus {
                daemon_running: true,
                device,
                log_path,
            };
        }

        if Instant::now() >= deadline {
            return DaemonStatus {
                daemon_running: true,
                device: DeviceStatus::Unknown("no reply".to_string()),
                log_path,
            };
        }

        match transport.read(&mut scratch) {
            Ok(got) if got > 0 => reader.feed(&scratch[..got]),
            Ok(_) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                return DaemonStatus {
                    daemon_running: true,
                    device: DeviceStatus::Unknown("read error".to_string()),
                    log_path,
                };
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run:
```bash
cargo test -p flip-client
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-client/src/lib.rs crates/flip-client/src/daemon.rs crates/flip-client/src/signal.rs
git commit -m "feat(client): expose typed daemon and IR operations"
```

---

## Task 4: Rewire `flip-cli` to use `flip-client`

**Files:**
- Modify: `crates/flip-cli/Cargo.toml`
- Modify: `crates/flip-cli/src/main.rs`
- Delete: `crates/flip-cli/src/client.rs`
- Delete: `crates/flip-cli/src/ir.rs`
- Delete: `crates/flip-cli/src/capture.rs`

- [ ] **Step 1: Update CLI dependencies**

In `crates/flip-cli/Cargo.toml`, replace:
```toml
[dependencies]
flip-core = { path = "../flip-core" }
flip-proto = { path = "../flip-proto", features = ["alloc"] }
anyhow = { workspace = true }
clap = { workspace = true }
ctrlc = "3"
```
with:
```toml
[dependencies]
flip-client = { path = "../flip-client" }
flip-proto = { path = "../flip-proto", features = ["alloc"] }
anyhow = { workspace = true }
clap = { workspace = true }
ctrlc = "3"
```

Keep `flip-proto` because `kv.rs` returns `flip_proto::Value`.

- [ ] **Step 2: Replace `main.rs` module declarations and imports**

At the top of `crates/flip-cli/src/main.rs`, replace:
```rust
mod capture;
mod client;
mod ir;
mod kv;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;
```
with:
```rust
mod kv;

use anyhow::Result;
use clap::{Parser, Subcommand};
use flip_client::{DaemonStatus, DeviceStatus, IrSignal};
use flip_proto::Value;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
```

- [ ] **Step 3: Change transmit CLI args to optional overrides**

In `IrCmd::Transmit`, replace:
```rust
        /// Carrier frequency in Hz.
        #[arg(long, default_value_t = 38000)]
        freq: u64,
        /// Duty cycle in permille (e.g. 330 = 33%).
        #[arg(long, default_value_t = 330)]
        duty: u64,
```
with:
```rust
        /// Override carrier frequency in Hz from the signal file.
        #[arg(long)]
        freq: Option<u32>,
        /// Override duty cycle in permille from the signal file.
        #[arg(long)]
        duty: Option<u32>,
```

- [ ] **Step 4: Replace command handling**

In `main`, replace the `Cmd::Status` arm with:
```rust
        Cmd::Status => {
            match flip_client::ping_through_daemon(b"flip-status", Duration::from_secs(3)) {
                Ok(echo) => {
                    println!("daemon: up");
                    println!("device: reachable (PONG round-trip ok)");
                    println!("echo:   {:?}", String::from_utf8_lossy(&echo));
                }
                Err(e) => {
                    println!("status: FAILED - {e:#}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
```

This keeps the existing `flip status` UX, including auto-spawn through the daemon client.

Replace the `Cmd::Caps` arm with:
```rust
        Cmd::Caps => {
            let caps = flip_client::caps(Duration::from_secs(3))?;
            println!("protocol v{}", caps.protocol_version);
            for inst in &caps.instruments {
                println!("{}", inst.id);
                for op in &inst.opcodes {
                    println!("  {}.{op}", inst.id);
                }
            }
            Ok(())
        }
```

Replace the `Cmd::Invoke` arm body with:
```rust
            let params = kv::parse_params(&params)?;
            let resp = flip_client::invoke(&instrument, &opcode, params, Duration::from_secs(3))?;
            println!("{}", render_value(&resp.result));
            Ok(())
```

Replace the `DaemonCmd::Status` arm with:
```rust
            DaemonCmd::Status => {
                print_daemon_status(flip_client::status());
                Ok(())
            }
```

Replace the `IrCmd::Transmit` arm with:
```rust
            IrCmd::Transmit { file, freq, duty } => {
                let mut signal = IrSignal::read_file(&file)?;
                if let Some(freq) = freq {
                    signal.frequency = freq;
                }
                if let Some(duty) = duty {
                    signal.duty_permille = duty;
                }
                let count = signal.timings.len();
                let sent = flip_client::ir_transmit(&signal, Duration::from_secs(10))?;
                println!("transmitted {count} edges: {sent}");
                Ok(())
            }
```

Replace the `IrCmd::Capture` arm with:
```rust
            IrCmd::Capture { output, auto_end } => run_capture(auto_end, output.as_deref()),
```

- [ ] **Step 5: Add CLI-local helpers**

Append these helpers to `crates/flip-cli/src/main.rs` after `main`:
```rust
fn print_daemon_status(status: DaemonStatus) {
    if !status.daemon_running {
        println!("daemon: not running");
        println!("log:    {}", status.log_path.display());
        return;
    }

    println!("daemon: running");
    match status.device {
        DeviceStatus::Connected { instruments } => {
            println!("device: connected ({instruments} instruments)");
        }
        DeviceStatus::Disconnected => println!("device: disconnected"),
        DeviceStatus::Unknown(reason) => println!("device: unknown ({reason})"),
    }
    println!("log:    {}", status.log_path.display());
}

fn run_capture(auto_end_ms: Option<u64>, output: Option<&str>) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    eprintln!("capturing... (Ctrl-C to stop)");
    let auto_end = auto_end_ms.map(Duration::from_millis);
    let signal = flip_client::ir_capture(auto_end, &|| stop.load(Ordering::SeqCst))?;

    match output {
        Some(path) => {
            signal.write_file(path)?;
            eprintln!("captured {} timings -> {path}", signal.timings.len());
        }
        None => std::io::stdout().write_all(signal.to_file_string().as_bytes())?,
    }
    Ok(())
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::U64(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Bytes(bytes) => format!(
            "0x{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
        Value::Array(items) => {
            let inner = items.iter().map(render_value).collect::<Vec<_>>().join(", ");
            format!("[{inner}]")
        }
        Value::Map(fields) => {
            let inner = fields
                .iter()
                .map(|(key, value)| format!("{key}: {}", render_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}
```

- [ ] **Step 6: Delete migrated modules**

Delete:
```bash
crates/flip-cli/src/client.rs
crates/flip-cli/src/ir.rs
crates/flip-cli/src/capture.rs
```

- [ ] **Step 7: Run CLI tests**

Run:
```bash
cargo test -p flip-cli
```

Expected: PASS. The `kv` tests still run; no CLI module references to deleted files remain.

- [ ] **Step 8: Commit**

```bash
git add Cargo.lock crates/flip-cli
git commit -m "refactor(cli): delegate daemon and IR behavior to flip-client"
```

---

## Task 5: Workspace verification and capture/replay acceptance

**Files:**
- No required source changes unless verification exposes a defect.

- [ ] **Step 1: Format**

Run:
```bash
cargo fmt --all
```

Expected: command exits 0 and formats all host crates.

- [ ] **Step 2: Run hardware-free tests**

Run:
```bash
cargo test
```

Expected: PASS for the full host workspace.

- [ ] **Step 3: Run lint check**

Run:
```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS with no warnings.

- [ ] **Step 4: Confirm old timing files remain readable**

Run:
```bash
cargo run -p flip-cli --bin flip -- ir transmit --file crates/flip-cli/examples/sos.txt --freq 38000 --duty 330
```

Expected without hardware/daemon: this may fail at daemon/device connection, but it must not fail while parsing `crates/flip-cli/examples/sos.txt`. If it fails before connecting with an `invalid timing` or `no timings found` error, fix `IrSignal::parse`.

- [ ] **Step 5: Hardware acceptance capture/replay [HW]**

With the FAP running on a connected Flipper, run:
```bash
just build
./target/debug/flip ir capture --auto-end 500 --output /tmp/test.ir
sed -n '1,4p' /tmp/test.ir
./target/debug/flip ir transmit --file /tmp/test.ir
```

Expected:
```text
# freq=38000
# duty=330
```

and the transmit count equals the number of timings after the leading idle was removed. Confirm replay triggers the same IR target for a normal 38 kHz remote.

- [ ] **Step 6: Commit final verification fixes**

If any verification step required source edits, commit them:
```bash
git add Cargo.lock Cargo.toml crates/flip-client crates/flip-cli
git commit -m "fix(client): polish flip-client migration"
```

If no edits were needed, do not create an empty commit.

---

## Implementation Notes

- `flip-client::ir_capture` owns the stream loop, leading-idle trim, and final `STREAM_STOP` drain. It does not install signal handlers.
- `flip-cli` owns Ctrl-C through `ctrlc::set_handler` and passes a cancellation closure to `flip_client::ir_capture`.
- `IrSignal::parse` intentionally supports old bare timing files by defaulting `frequency=38000` and `duty_permille=330`.
- `IrSignal::from_capture` intentionally drops `raw[0]`; it is the receiver's pre-signal idle gap.
- `flip_client::ir_transmit` accepts both the current firmware response shape (`{sent: N}`) and a future plain `U64(N)` response so the host API can still return the specified `u64`.

## Self-Review

**Spec coverage:** The plan creates `flip-client`, moves daemon client logic, adds `IrSignal` with `freq`/`duty` directives, trims leading idle, emits 12 timings per line, exposes `ir_capture`/`ir_transmit`, and rewires CLI commands with Ctrl-C cancellation in the CLI.

**Placeholder scan:** No task uses deferred-work markers. Code-changing steps include concrete snippets or explicit moved files/functions.

**Type consistency:** `IrSignal.frequency` and `duty_permille` are `u32`; transmit params convert them to `Value::U64`; CLI overrides are `Option<u32>`; `ir_transmit` returns `Result<u64>`.
