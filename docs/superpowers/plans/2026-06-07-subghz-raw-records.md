# Sub-GHz Raw Records Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Sub-GHz raw level/duration capture and transmit records to flip-link.

**Architecture:** Reuse the existing control plane and generic daemon stream relay. Add a host-side `SubGhzSignal` record type and CLI commands, then add a firmware `subghz` instrument whose capture ISR streams 5-byte level/duration samples and whose TX callback yields Flipper `LevelDuration` values.

**Tech Stack:** Rust host crates; `flipperzero-sys` 0.16 Sub-GHz HAL on firmware; `minicbor` `Value` params; existing `STREAM_START`/`STREAM_DATA`/`STREAM_STOP` transport.

---

## File Structure

- Modify: `crates/flip-proto/src/messages.rs`
  - Add the Sub-GHz stream format constant.
- Create: `crates/flip-client/src/subghz.rs`
  - Own `SubGhzPreset`, `SubGhzEdge`, `SubGhzSignal`, file parse/write, stream decode, and request param builders.
- Modify: `crates/flip-client/src/lib.rs`
  - Export Sub-GHz types and add `subghz_capture` / `subghz_transmit`.
- Modify: `crates/flip-cli/src/main.rs`
  - Add `flip subghz capture` and `flip subghz transmit`.
- Create: `firmware/src/subghz_instrument.rs`
  - Own Sub-GHz parameter parsing, capture lifecycle, TX lifecycle, and radio teardown.
- Modify: `firmware/src/registry.rs`
  - Advertise `subghz.transmit` and streaming `subghz.capture`.
- Modify: `firmware/src/main.rs`
  - Route `subghz.capture`, drain Sub-GHz capture in the main loop, and stop it during `STREAM_STOP`/teardown.
- Modify: `README.md`
  - Document basic Sub-GHz usage and frequency responsibility.

## Task 1: Protocol Constant + Host Signal Model

**Files:**
- Modify: `crates/flip-proto/src/messages.rs`
- Create: `crates/flip-client/src/subghz.rs`
- Modify: `crates/flip-client/src/lib.rs`

- [ ] **Step 1: Add failing protocol test**

In `crates/flip-proto/src/messages.rs`, extend `stream_bodies_round_trip`:

```rust
assert_eq!(
    STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1,
    "subghz_level_duration_le_v1"
);
```

Run: `cargo test -p flip-proto --features alloc stream_bodies_round_trip`

Expected: FAIL to compile because `STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1` is not defined.

- [ ] **Step 2: Add stream format constant**

In `crates/flip-proto/src/messages.rs`, below `STREAM_FORMAT_RAW_I32_US`, add:

```rust
/// Raw Sub-GHz level/duration samples: 1 byte level + u32 little-endian duration_us.
pub const STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1: &str = "subghz_level_duration_le_v1";
```

Run: `cargo test -p flip-proto --features alloc stream_bodies_round_trip`

Expected: PASS.

- [ ] **Step 3: Add failing host signal tests**

Create `crates/flip-client/src/subghz.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_parse_and_display_round_trip() {
        for (text, preset) in [
            ("ook270", SubGhzPreset::Ook270),
            ("ook650", SubGhzPreset::Ook650),
            ("2fsk_dev238", SubGhzPreset::FskDev238),
            ("2fsk_dev476", SubGhzPreset::FskDev476),
            ("msk99_97", SubGhzPreset::Msk99_97),
            ("gfsk9_99", SubGhzPreset::Gfsk9_99),
        ] {
            assert_eq!(text.parse::<SubGhzPreset>().unwrap(), preset);
            assert_eq!(preset.to_string(), text);
        }
        assert!("bad".parse::<SubGhzPreset>().is_err());
    }

    #[test]
    fn raw_file_round_trips() {
        let signal = SubGhzSignal {
            frequency: 433_920_000,
            preset: SubGhzPreset::Ook650,
            edges: vec![
                SubGhzEdge { level: true, duration_us: 9000 },
                SubGhzEdge { level: false, duration_us: 4500 },
            ],
        };

        assert_eq!(SubGhzSignal::parse(&signal.to_file_string()).unwrap(), signal);
    }

    #[test]
    fn parse_rejects_missing_metadata_and_bad_edges() {
        assert!(SubGhzSignal::parse("1 100\n").is_err());
        assert!(SubGhzSignal::parse("# frequency=433920000\n1 100\n").is_err());
        assert!(SubGhzSignal::parse("# frequency=433920000\n# preset=ook650\n2 100\n").is_err());
        assert!(SubGhzSignal::parse("# frequency=433920000\n# preset=ook650\n1 0\n").is_err());
    }

    #[test]
    fn decodes_five_byte_stream_records() {
        let mut out = Vec::new();
        let payload = [1, 0x28, 0x23, 0, 0, 0, 0x94, 0x11, 0, 0];

        assert_eq!(decode_stream_data(&payload, &mut out), 2);
        assert_eq!(
            out,
            vec![
                SubGhzEdge { level: true, duration_us: 9000 },
                SubGhzEdge { level: false, duration_us: 4500 },
            ]
        );
    }

    #[test]
    fn transmit_params_include_frequency_preset_edges() {
        let signal = SubGhzSignal {
            frequency: 433_920_000,
            preset: SubGhzPreset::Ook650,
            edges: vec![SubGhzEdge { level: true, duration_us: 9000 }],
        };

        let params = signal.to_transmit_params(3);
        assert_eq!(params.get("frequency"), Some(&flip_proto::Value::U64(433_920_000)));
        assert_eq!(params.get("preset"), Some(&flip_proto::Value::Text("ook650".into())));
        assert_eq!(params.get("repeat"), Some(&flip_proto::Value::U64(3)));
    }
}
```

Run: `cargo test -p flip-client subghz`

Expected: FAIL to compile because the types/functions do not exist yet.

- [ ] **Step 4: Implement host signal model**

Replace `crates/flip-client/src/subghz.rs` with:

```rust
use anyhow::{anyhow, Context, Result};
use flip_proto::Value;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

pub const MAX_SUBGHZ_DURATION_US: u32 = 0x3fff_ffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubGhzPreset {
    Ook270,
    Ook650,
    FskDev238,
    FskDev476,
    Msk99_97,
    Gfsk9_99,
}

impl SubGhzPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ook270 => "ook270",
            Self::Ook650 => "ook650",
            Self::FskDev238 => "2fsk_dev238",
            Self::FskDev476 => "2fsk_dev476",
            Self::Msk99_97 => "msk99_97",
            Self::Gfsk9_99 => "gfsk9_99",
        }
    }
}

impl fmt::Display for SubGhzPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SubGhzPreset {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "ook270" => Ok(Self::Ook270),
            "ook650" => Ok(Self::Ook650),
            "2fsk_dev238" => Ok(Self::FskDev238),
            "2fsk_dev476" => Ok(Self::FskDev476),
            "msk99_97" => Ok(Self::Msk99_97),
            "gfsk9_99" => Ok(Self::Gfsk9_99),
            _ => Err(anyhow!("unknown Sub-GHz preset '{value}'")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGhzEdge {
    pub level: bool,
    pub duration_us: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGhzSignal {
    pub frequency: u32,
    pub preset: SubGhzPreset,
    pub edges: Vec<SubGhzEdge>,
}

impl SubGhzSignal {
    pub fn parse(text: &str) -> Result<Self> {
        let mut frequency = None;
        let mut preset = None;
        let mut edges = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(comment) = trimmed.strip_prefix('#') {
                let directive = comment.trim();
                if let Some(rest) = directive.strip_prefix("frequency=") {
                    frequency = Some(parse_u32("frequency", rest)?);
                } else if let Some(rest) = directive.strip_prefix("preset=") {
                    let token = rest.split_whitespace().next().ok_or_else(|| anyhow!("missing preset value"))?;
                    preset = Some(token.parse()?);
                }
                continue;
            }

            let fields = trimmed.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 {
                return Err(anyhow!("invalid Sub-GHz edge line '{trimmed}'"));
            }
            let level = match fields[0] {
                "0" => false,
                "1" => true,
                other => return Err(anyhow!("invalid Sub-GHz level '{other}'")),
            };
            let duration_us = fields[1]
                .parse::<u32>()
                .map_err(|_| anyhow!("invalid Sub-GHz duration '{}'", fields[1]))?;
            if duration_us == 0 || duration_us > MAX_SUBGHZ_DURATION_US {
                return Err(anyhow!("Sub-GHz duration out of range: {duration_us}"));
            }
            edges.push(SubGhzEdge { level, duration_us });
        }

        let frequency = frequency.ok_or_else(|| anyhow!("Sub-GHz frequency missing"))?;
        let preset = preset.ok_or_else(|| anyhow!("Sub-GHz preset missing"))?;
        if edges.is_empty() {
            return Err(anyhow!("no Sub-GHz edges found"));
        }
        Ok(Self { frequency, preset, edges })
    }

    pub fn to_file_string(&self) -> String {
        let mut out = format!(
            "# format=flip-subghz-raw-v1\n# frequency={}\n# preset={}\n",
            self.frequency, self.preset
        );
        for edge in &self.edges {
            out.push_str(if edge.level { "1 " } else { "0 " });
            out.push_str(&edge.duration_us.to_string());
            out.push('\n');
        }
        out
    }

    pub fn read_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn write_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_file_string()).with_context(|| format!("write {}", path.display()))
    }

    pub fn capture_params(frequency: u32, preset: SubGhzPreset) -> Value {
        Value::Map(vec![
            ("frequency".into(), Value::U64(frequency as u64)),
            ("preset".into(), Value::Text(preset.to_string())),
        ])
    }

    pub fn to_transmit_params(&self, repeat: u32) -> Value {
        Value::Map(vec![
            ("frequency".into(), Value::U64(self.frequency as u64)),
            ("preset".into(), Value::Text(self.preset.to_string())),
            ("repeat".into(), Value::U64(repeat as u64)),
            ("edges".into(), Value::Array(self.edges.iter().map(edge_to_value).collect())),
        ])
    }
}

fn edge_to_value(edge: &SubGhzEdge) -> Value {
    Value::Map(vec![
        ("level".into(), Value::Bool(edge.level)),
        ("duration_us".into(), Value::U64(edge.duration_us as u64)),
    ])
}

fn parse_u32(name: &str, rest: &str) -> Result<u32> {
    let token = rest.split_whitespace().next().ok_or_else(|| anyhow!("missing {name} value"))?;
    token.parse::<u32>().map_err(|_| anyhow!("invalid {name} value '{token}'"))
}

pub(crate) fn decode_stream_data(payload: &[u8], out: &mut Vec<SubGhzEdge>) -> usize {
    let mut count = 0;
    for chunk in payload.chunks_exact(5) {
        let duration_us = u32::from_le_bytes([chunk[1], chunk[2], chunk[3], chunk[4]]);
        if duration_us > 0 {
            out.push(SubGhzEdge { level: chunk[0] != 0, duration_us });
            count += 1;
        }
    }
    count
}
```

Keep the tests from Step 3 at the bottom of this file.

In `crates/flip-client/src/lib.rs`, add:

```rust
pub mod subghz;
pub use subghz::{SubGhzEdge, SubGhzPreset, SubGhzSignal};
```

Run: `cargo test -p flip-client subghz`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-proto/src/messages.rs crates/flip-client/src/lib.rs crates/flip-client/src/subghz.rs
git commit -m "feat(client): add Sub-GHz raw signal records"
```

## Task 2: Host Client API + CLI

**Files:**
- Modify: `crates/flip-client/src/lib.rs`
- Modify: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Add failing client API tests**

In `crates/flip-client/src/lib.rs` tests, add:

```rust
#[test]
fn subghz_stream_start_accepts_subghz_format() {
    let payload = flip_proto::messages::to_payload(&flip_proto::StreamStart {
        format: flip_proto::messages::STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1.to_string(),
    });

    assert!(validate_subghz_stream_start_frame(MsgType::StreamStart, &payload).is_ok());
}

#[test]
fn subghz_stream_start_rejects_wrong_format() {
    let payload = flip_proto::messages::to_payload(&flip_proto::StreamStart {
        format: flip_proto::messages::STREAM_FORMAT_RAW_I32_US.to_string(),
    });
    let err = validate_subghz_stream_start_frame(MsgType::StreamStart, &payload).unwrap_err();

    assert_eq!(
        err.to_string(),
        "unsupported Sub-GHz capture stream format raw_int32_le_us (expected subghz_level_duration_le_v1)"
    );
}
```

Run: `cargo test -p flip-client subghz_stream_start`

Expected: FAIL to compile because `validate_subghz_stream_start_frame` does not exist.

- [ ] **Step 2: Add client API functions**

In `crates/flip-client/src/lib.rs`, add functions next to the IR operations:

```rust
pub fn subghz_transmit(signal: &SubGhzSignal, repeat: u32, timeout: Duration) -> Result<u64> {
    let resp = invoke("subghz", "transmit", signal.to_transmit_params(repeat), timeout)?;
    sent_count(&resp.result)
}

pub fn subghz_capture(
    frequency: u32,
    preset: SubGhzPreset,
    idle_gap: Option<Duration>,
    cancel: &dyn Fn() -> bool,
) -> Result<SubGhzSignal> {
    let mut conn = daemon::open_stream("subghz", "capture", SubGhzSignal::capture_params(frequency, preset))?;
    expect_subghz_stream_start(&mut conn)?;
    let mut edges = Vec::new();
    let mut last_data = Instant::now();
    let mut saw_final_stop = false;

    loop {
        if cancel() {
            break;
        }
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                let n = subghz::decode_stream_data(&payload, &mut edges);
                if n > 0 {
                    last_data = Instant::now();
                }
            }
            Some((MsgType::StreamStop, payload)) => {
                handle_stream_stop("Sub-GHz", &payload)?;
                saw_final_stop = true;
                break;
            }
            Some((MsgType::Error, payload)) => return Err(decode_agent_error(&payload)),
            Some((other, _)) => return Err(anyhow!("unexpected frame during Sub-GHz capture: {other:?}")),
            None => {}
        }

        if let Some(gap) = idle_gap {
            if !edges.is_empty() && last_data.elapsed() >= gap {
                break;
            }
        }
    }

    if !saw_final_stop {
        conn.send(MsgType::StreamStop, &[])?;
        drain_subghz_capture_stop(&mut conn, &mut edges)?;
    }

    if edges.is_empty() {
        return Err(anyhow!("no Sub-GHz signal captured"));
    }
    Ok(SubGhzSignal { frequency, preset, edges })
}
```

Also add:

```rust
fn expect_subghz_stream_start(conn: &mut daemon::StreamConn) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some((typ, payload)) = conn.next_frame(Duration::from_millis(50))? {
            return validate_subghz_stream_start_frame(typ, &payload);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for Sub-GHz capture stream start"));
        }
    }
}

fn validate_subghz_stream_start_frame(typ: MsgType, payload: &[u8]) -> Result<()> {
    match typ {
        MsgType::StreamStart => {
            let start: flip_proto::StreamStart = flip_proto::messages::from_payload(payload)
                .map_err(|e| anyhow!("decode STREAM_START: {e}"))?;
            if start.format != flip_proto::messages::STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1 {
                return Err(anyhow!(
                    "unsupported Sub-GHz capture stream format {} (expected {})",
                    start.format,
                    flip_proto::messages::STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1
                ));
            }
            Ok(())
        }
        MsgType::Error => Err(decode_agent_error(payload)),
        other => Err(anyhow!("expected STREAM_START, got {other:?}")),
    }
}

fn drain_subghz_capture_stop(conn: &mut daemon::StreamConn, edges: &mut Vec<SubGhzEdge>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match conn.next_frame(Duration::from_millis(50))? {
            Some((MsgType::StreamData, payload)) => {
                subghz::decode_stream_data(&payload, edges);
            }
            Some((MsgType::StreamStop, payload)) => {
                handle_stream_stop("Sub-GHz", &payload)?;
                return Ok(());
            }
            Some((MsgType::Error, payload)) => return Err(decode_agent_error(&payload)),
            Some((other, _)) => return Err(anyhow!("unexpected frame while stopping Sub-GHz capture: {other:?}")),
            None => {}
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for Sub-GHz capture stop"));
        }
    }
}
```

Refactor the existing IR stop handler into:

```rust
fn handle_stream_stop(label: &str, payload: &[u8]) -> Result<()> {
    let stop: flip_proto::StreamStop = flip_proto::messages::from_payload(payload)
        .map_err(|e| anyhow!("decode STREAM_STOP: {e}"))?;
    if stop.dropped > 0 {
        return Err(anyhow!("{label} capture dropped {} samples (buffer overflow)", stop.dropped));
    }
    Ok(())
}
```

Then replace existing `handle_capture_stop(&payload)?` calls with `handle_stream_stop("IR", &payload)?`.

Run: `cargo test -p flip-client`

Expected: PASS.

- [ ] **Step 3: Add failing CLI parser tests**

In `crates/flip-cli/src/main.rs` tests, add:

```rust
#[test]
fn subghz_capture_requires_freq_and_accepts_preset() {
    let cli = Cli::try_parse_from([
        "flip",
        "subghz",
        "capture",
        "--freq",
        "433920000",
        "--preset",
        "ook650",
        "--idle-gap",
        "500",
        "--output",
        "/tmp/button.subghz",
    ])
    .unwrap();

    let Cmd::SubGhz { cmd: SubGhzCmd::Capture { freq, preset, idle_gap, output, .. } } = cli.cmd else {
        panic!("expected subghz capture");
    };
    assert_eq!(freq, 433_920_000);
    assert_eq!(preset, "ook650");
    assert_eq!(idle_gap, Some(500));
    assert_eq!(output.as_deref(), Some("/tmp/button.subghz"));
}

#[test]
fn subghz_transmit_accepts_repeat() {
    let cli = Cli::try_parse_from([
        "flip",
        "subghz",
        "transmit",
        "--file",
        "/tmp/button.subghz",
        "--repeat",
        "2",
    ])
    .unwrap();

    let Cmd::SubGhz { cmd: SubGhzCmd::Transmit { file, repeat, .. } } = cli.cmd else {
        panic!("expected subghz transmit");
    };
    assert_eq!(file, "/tmp/button.subghz");
    assert_eq!(repeat, 2);
}
```

Run: `cargo test -p flip-cli subghz`

Expected: FAIL to compile because `SubGhz`/`SubGhzCmd` do not exist.

- [ ] **Step 4: Implement CLI commands**

In `crates/flip-cli/src/main.rs`, update imports:

```rust
use flip_client::{DaemonStatus, DeviceStatus, IrSignal, SubGhzPreset, SubGhzSignal};
```

Add to `Cmd`:

```rust
/// Sub-GHz radio commands.
SubGhz {
    #[command(subcommand)]
    cmd: SubGhzCmd,
},
```

Add:

```rust
#[derive(Subcommand)]
enum SubGhzCmd {
    /// Capture a raw Sub-GHz level/duration record.
    Capture {
        /// Frequency in Hz. Required; there is no default RF frequency.
        #[arg(long)]
        freq: u32,
        /// Radio preset: ook270, ook650, 2fsk_dev238, 2fsk_dev476, msk99_97, gfsk9_99.
        #[arg(long)]
        preset: String,
        /// Write signal record here (default: stdout).
        #[arg(long)]
        output: Option<String>,
        /// Stop after this many ms of silence once Sub-GHz data has been seen.
        #[arg(long)]
        idle_gap: Option<u64>,
        /// Stop after this many ms of wall-clock capture time.
        #[arg(long)]
        duration: Option<u64>,
    },
    /// Transmit a raw Sub-GHz level/duration record from a file.
    Transmit {
        /// Path to a Sub-GHz signal record file.
        #[arg(long)]
        file: String,
        /// Override frequency in Hz from the signal file.
        #[arg(long)]
        freq: Option<u32>,
        /// Override preset from the signal file.
        #[arg(long)]
        preset: Option<String>,
        /// Repeat count.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
}
```

Add a `Cmd::SubGhz` match arm:

```rust
Cmd::SubGhz { cmd } => match cmd {
    SubGhzCmd::Capture { freq, preset, output, idle_gap, duration } => {
        run_subghz_capture(freq, &preset, idle_gap, duration, output.as_deref())
    }
    SubGhzCmd::Transmit { file, freq, preset, repeat } => {
        let mut signal = SubGhzSignal::read_file(&file)?;
        if let Some(freq) = freq {
            signal.frequency = freq;
        }
        if let Some(preset) = preset {
            signal.preset = preset.parse::<SubGhzPreset>()?;
        }
        let count = signal.edges.len();
        let sent = flip_client::subghz_transmit(&signal, repeat, Duration::from_secs(30))?;
        println!("transmitted {count} Sub-GHz edges: {sent}");
        Ok(())
    }
},
```

Add:

```rust
fn run_subghz_capture(
    freq: u32,
    preset: &str,
    idle_gap_ms: Option<u64>,
    duration_ms: Option<u64>,
    output: Option<&str>,
) -> Result<()> {
    let preset = preset.parse::<SubGhzPreset>()?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    eprintln!("capturing Sub-GHz... (Ctrl-C to stop)");
    let idle_gap = idle_gap_ms.map(Duration::from_millis);
    let duration = duration_ms.map(Duration::from_millis);
    let started = Instant::now();
    let signal = flip_client::subghz_capture(freq, preset, idle_gap, &|| {
        stop.load(Ordering::SeqCst)
            || duration
                .map(|duration| started.elapsed() >= duration)
                .unwrap_or(false)
    })?;

    match output {
        Some(path) => {
            signal.write_file(path)?;
            eprintln!("captured {} Sub-GHz edges -> {path}", signal.edges.len());
        }
        None => std::io::stdout().write_all(signal.to_file_string().as_bytes())?,
    }
    Ok(())
}
```

Run: `cargo test -p flip-cli subghz`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-client/src/lib.rs crates/flip-cli/src/main.rs
git commit -m "feat(cli): add Sub-GHz raw capture and transmit commands"
```

## Task 3: Firmware Registry + Routing

**Files:**
- Create: `firmware/src/subghz_instrument.rs`
- Modify: `firmware/src/registry.rs`
- Modify: `firmware/src/main.rs`

- [ ] **Step 1: Add a compiling firmware stub**

Create `firmware/src/subghz_instrument.rs`:

```rust
use alloc::string::{String, ToString};
use flip_proto::Value;
use flip_proto::messages::ERR_BAD_PARAMS;

pub fn transmit(_params: &Value) -> Result<Value, (u32, String)> {
    Err((ERR_BAD_PARAMS, "subghz transmit not implemented".to_string()))
}

pub fn capture_active() -> bool {
    false
}

pub fn start_capture(
    seq: u16,
    _params: &Value,
    _send_start: impl FnOnce(u16, &str),
    send_error: impl FnOnce(u16, u32, &str),
) {
    send_error(seq, ERR_BAD_PARAMS, "subghz capture not implemented");
}

pub fn drain_capture(_send_data: impl FnOnce(u16, &[u8])) {}

pub fn stop_capture(_send_data: impl Fn(u16, &[u8]), _send_stop: impl FnOnce(u16, u32)) {}
```

In `firmware/src/main.rs`, add:

```rust
mod subghz_instrument;
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 2: Register Sub-GHz instrument**

In `firmware/src/registry.rs`, after `IR_OPCODES`, add:

```rust
static SUBGHZ_OPCODES: &[OpcodeEntry] = &[OpcodeEntry {
    opcode: "transmit",
    handler: crate::subghz_instrument::transmit,
}];
```

Add this entry to `INSTRUMENTS`:

```rust
InstrumentEntry {
    id: "subghz",
    opcodes: SUBGHZ_OPCODES,
    streaming_opcodes: &["capture"],
},
```

Add:

```rust
pub fn is_streaming(instrument: &str, opcode: &str) -> bool {
    INSTRUMENTS
        .iter()
        .find(|i| i.id == instrument)
        .map(|i| i.streaming_opcodes.iter().any(|s| *s == opcode))
        .unwrap_or(false)
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 3: Route streaming captures generically**

In `firmware/src/main.rs`, replace the special `if req.instrument == "ir" && req.opcode == "capture"` block with:

```rust
if registry::is_streaming(&req.instrument, &req.opcode) {
    match (req.instrument.as_str(), req.opcode.as_str()) {
        ("ir", "capture") => ir_instrument::start_capture(
            seq,
            send_stream_start,
            |s: u16, c: u32, m: &str| send_error(s, c, m),
        ),
        ("subghz", "capture") => subghz_instrument::start_capture(
            seq,
            &req.params,
            send_stream_start,
            |s: u16, c: u32, m: &str| send_error(s, c, m),
        ),
        _ => send_error(seq, flip_proto::messages::ERR_UNKNOWN_OPCODE, "unknown streaming opcode"),
    }
} else {
    match registry::dispatch(&req.instrument, &req.opcode, &req.params) {
        Ok(result) => send_msg(MsgType::Resp, seq, &Resp { ok: true, result }),
        Err((code, message)) => send_error(seq, code, &message),
    }
}
```

In the main loop, change capture draining to:

```rust
let ir_capturing = ir_instrument::capture_active();
let subghz_capturing = subghz_instrument::capture_active();
if ir_capturing {
    ir_instrument::drain_capture(send_stream_data);
}
if subghz_capturing {
    subghz_instrument::drain_capture(send_stream_data);
}
let capturing = ir_capturing || subghz_capturing;
```

In `MsgType::StreamStop`, stop both:

```rust
ir_instrument::stop_capture(send_stream_data, send_stream_stop);
subghz_instrument::stop_capture(send_stream_data, send_stream_stop);
```

At teardown, also stop both:

```rust
ir_instrument::stop_capture(send_stream_data, send_stream_stop);
subghz_instrument::stop_capture(send_stream_data, send_stream_stop);
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add firmware/src/main.rs firmware/src/registry.rs firmware/src/subghz_instrument.rs
git commit -m "feat(fw): advertise Sub-GHz instrument"
```

## Task 4: Firmware Sub-GHz Capture

**Files:**
- Modify: `firmware/src/subghz_instrument.rs`

- [ ] **Step 1: Implement preset/param helpers**

In `firmware/src/subghz_instrument.rs`, replace the stub imports and add:

```rust
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU32, Ordering};

use flip_proto::Value;
use flip_proto::messages::{ERR_BAD_PARAMS, ERR_BUSY, ERR_INTERNAL, ERR_OVERSIZED};
use flipperzero_sys as sys;

const MAX_EDGES: usize = 4096;
const CAPTURE_CAP: usize = 8192;
const EDGE_RECORD_SIZE: usize = 5;

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::U64(n) => Some(*n),
        Value::I64(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn required_frequency(params: &Value) -> Result<u32, (u32, String)> {
    params
        .get("frequency")
        .and_then(as_u64)
        .map(|n| n as u32)
        .ok_or_else(|| (ERR_BAD_PARAMS, "frequency required".to_string()))
}

fn required_preset(params: &Value) -> Result<sys::FuriHalSubGhzPreset, (u32, String)> {
    match params.get("preset") {
        Some(Value::Text(name)) => preset_by_name(name),
        _ => Err((ERR_BAD_PARAMS, "preset required".to_string())),
    }
}

fn preset_by_name(name: &str) -> Result<sys::FuriHalSubGhzPreset, (u32, String)> {
    match name {
        "ook270" => Ok(sys::FuriHalSubGhzPresetOok270Async),
        "ook650" => Ok(sys::FuriHalSubGhzPresetOok650Async),
        "2fsk_dev238" => Ok(sys::FuriHalSubGhzPreset2FSKDev238Async),
        "2fsk_dev476" => Ok(sys::FuriHalSubGhzPreset2FSKDev476Async),
        "msk99_97" => Ok(sys::FuriHalSubGhzPresetMSK99_97KbAsync),
        "gfsk9_99" => Ok(sys::FuriHalSubGhzPresetGFSK9_99KbAsync),
        _ => Err((ERR_BAD_PARAMS, "unknown subghz preset".to_string())),
    }
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS with unused warnings acceptable until later steps consume helpers.

- [ ] **Step 2: Add capture globals and ISR**

In `firmware/src/subghz_instrument.rs`, add:

```rust
static CAPTURE_STREAM: AtomicPtr<sys::FuriStreamBuffer> = AtomicPtr::new(core::ptr::null_mut());
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE_SEQ: AtomicU16 = AtomicU16::new(0);
static CAPTURE_DROPPED: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn rx_capture_isr(
    _level: bool,
    duration: u32,
    _context: *mut core::ffi::c_void,
) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() || duration == 0 {
        return;
    }
    let mut bytes = [0u8; EDGE_RECORD_SIZE];
    bytes[0] = if _level { 1 } else { 0 };
    bytes[1..5].copy_from_slice(&duration.to_le_bytes());
    let sent = unsafe {
        sys::furi_stream_buffer_send(
            sb,
            bytes.as_ptr() as *const core::ffi::c_void,
            EDGE_RECORD_SIZE,
            0,
        )
    };
    if sent != EDGE_RECORD_SIZE {
        CAPTURE_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

fn ensure_capture_buffer() -> *mut sys::FuriStreamBuffer {
    let existing = CAPTURE_STREAM.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let sb = unsafe { sys::furi_stream_buffer_alloc(CAPTURE_CAP, EDGE_RECORD_SIZE) };
    CAPTURE_STREAM.store(sb, Ordering::Release);
    sb
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 3: Implement start/drain/stop capture**

Replace the stub `capture_active`, `start_capture`, `drain_capture`, and `stop_capture` with:

```rust
pub fn capture_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Acquire)
}

pub fn start_capture(
    seq: u16,
    params: &Value,
    send_start: impl FnOnce(u16, &str),
    send_error: impl FnOnce(u16, u32, &str),
) {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) {
        send_error(seq, ERR_BUSY, "subghz busy");
        return;
    }
    let frequency = match required_frequency(params) {
        Ok(frequency) => frequency,
        Err((code, message)) => {
            send_error(seq, code, &message);
            return;
        }
    };
    let preset = match required_preset(params) {
        Ok(preset) => preset,
        Err((code, message)) => {
            send_error(seq, code, &message);
            return;
        }
    };
    let device = match internal_device() {
        Ok(device) => device,
        Err((code, message)) => {
            send_error(seq, code, &message);
            return;
        }
    };
    if unsafe { !sys::subghz_devices_is_frequency_valid(device, frequency) } {
        send_error(seq, ERR_BAD_PARAMS, "invalid subghz frequency");
        return;
    }
    if unsafe { !sys::subghz_devices_begin(device) } {
        send_error(seq, ERR_BUSY, "subghz device unavailable");
        return;
    }
    let sb = ensure_capture_buffer();
    if sb.is_null() {
        unsafe { sys::subghz_devices_end(device) };
        send_error(seq, ERR_INTERNAL, "no subghz capture buffer");
        return;
    }
    let mut scratch = [0u8; 128];
    while unsafe {
        sys::furi_stream_buffer_receive(sb, scratch.as_mut_ptr() as *mut core::ffi::c_void, 128, 0)
    } > 0
    {}
    CAPTURE_DROPPED.store(0, Ordering::Release);
    CAPTURE_SEQ.store(seq, Ordering::Release);
    CAPTURE_ACTIVE.store(true, Ordering::Release);

    unsafe {
        sys::subghz_devices_reset(device);
        sys::subghz_devices_load_preset(device, preset, core::ptr::null_mut());
        sys::subghz_devices_set_frequency(device, frequency);
        sys::subghz_devices_set_rx(device);
        sys::subghz_devices_flush_rx(device);
        sys::furi_hal_subghz_start_async_rx(Some(rx_capture_isr), core::ptr::null_mut());
    }
    send_start(seq, flip_proto::messages::STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1);
}

pub fn drain_capture(send_data: impl FnOnce(u16, &[u8])) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() || !CAPTURE_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let mut batch = [0u8; 250];
    let got = unsafe {
        sys::furi_stream_buffer_receive(sb, batch.as_mut_ptr() as *mut core::ffi::c_void, 250, 0)
    };
    let whole = got - (got % EDGE_RECORD_SIZE);
    if whole >= EDGE_RECORD_SIZE {
        send_data(CAPTURE_SEQ.load(Ordering::Acquire), &batch[..whole]);
    }
}

pub fn stop_capture(send_data: impl Fn(u16, &[u8]), send_stop: impl FnOnce(u16, u32)) {
    if !CAPTURE_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    unsafe {
        sys::furi_hal_subghz_stop_async_rx();
        if let Ok(device) = internal_device() {
            sys::subghz_devices_idle(device);
            sys::subghz_devices_sleep(device);
            sys::subghz_devices_end(device);
        }
    }
    let seq = CAPTURE_SEQ.load(Ordering::Acquire);
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if !sb.is_null() {
        loop {
            let mut batch = [0u8; 250];
            let got = unsafe {
                sys::furi_stream_buffer_receive(
                    sb,
                    batch.as_mut_ptr() as *mut core::ffi::c_void,
                    250,
                    0,
                )
            };
            let whole = got - (got % EDGE_RECORD_SIZE);
            if whole < EDGE_RECORD_SIZE {
                break;
            }
            send_data(seq, &batch[..whole]);
        }
    }
    send_stop(seq, CAPTURE_DROPPED.load(Ordering::Acquire));
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS. If a symbol name differs, grep the installed bindings in `~/.cargo/registry/src/.../flipperzero-sys-0.16.0/src/bindings.rs` and update only the symbol spelling, not the behavior.

- [ ] **Step 4: Commit**

```bash
git add firmware/src/subghz_instrument.rs
git commit -m "feat(fw): add Sub-GHz raw capture"
```

## Task 5: Firmware Sub-GHz Transmit

**Files:**
- Modify: `firmware/src/subghz_instrument.rs`

- [ ] **Step 1: Add TX edge parser and LevelDuration helpers**

In `firmware/src/subghz_instrument.rs`, add:

```rust
#[derive(Clone, Copy)]
struct TxEdge {
    level: bool,
    duration_us: u32,
}

fn parse_edges(params: &Value) -> Result<Vec<TxEdge>, (u32, String)> {
    let array = match params.get("edges") {
        Some(Value::Array(array)) => array,
        _ => return Err((ERR_BAD_PARAMS, "edges array required".to_string())),
    };
    if array.is_empty() {
        return Err((ERR_BAD_PARAMS, "edges empty".to_string()));
    }
    if array.len() > MAX_EDGES {
        return Err((ERR_OVERSIZED, "too many subghz edges".to_string()));
    }
    let mut out = Vec::new();
    for item in array {
        let level = match item.get("level") {
            Some(Value::Bool(level)) => *level,
            _ => return Err((ERR_BAD_PARAMS, "edge level required".to_string())),
        };
        let duration_us = match item.get("duration_us").and_then(as_u64) {
            Some(duration_us) if duration_us > 0 && duration_us <= 0x3fff_ffff => duration_us as u32,
            _ => return Err((ERR_BAD_PARAMS, "edge duration_us out of range".to_string())),
        };
        out.push(TxEdge { level, duration_us });
    }
    Ok(out)
}

fn repeat_count(params: &Value) -> Result<u32, (u32, String)> {
    let repeat = params.get("repeat").and_then(as_u64).unwrap_or(1);
    if repeat == 0 || repeat > 100 {
        return Err((ERR_BAD_PARAMS, "repeat out of range".to_string()));
    }
    Ok(repeat as u32)
}

fn level_duration(level: bool, duration_us: u32) -> sys::LevelDuration {
    sys::LevelDuration {
        _bitfield_align_1: [],
        _bitfield_1: sys::LevelDuration::new_bitfield_1(duration_us, if level { 2 } else { 1 }),
    }
}

fn level_duration_reset() -> sys::LevelDuration {
    sys::LevelDuration {
        _bitfield_align_1: [],
        _bitfield_1: sys::LevelDuration::new_bitfield_1(0, 0),
    }
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 2: Add TX callback globals**

In `firmware/src/subghz_instrument.rs`, add:

```rust
static TX_PTR: AtomicPtr<TxEdge> = AtomicPtr::new(core::ptr::null_mut());
static TX_LEN: AtomicU32 = AtomicU32::new(0);
static TX_POS: AtomicU32 = AtomicU32::new(0);
static TX_REPEAT: AtomicU32 = AtomicU32::new(1);
static TX_REPEAT_POS: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn tx_yield(_context: *mut core::ffi::c_void) -> sys::LevelDuration {
    let ptr = TX_PTR.load(Ordering::Acquire);
    let len = TX_LEN.load(Ordering::Acquire);
    if ptr.is_null() || len == 0 {
        return level_duration_reset();
    }
    let pos = TX_POS.load(Ordering::Acquire);
    let repeat_pos = TX_REPEAT_POS.load(Ordering::Acquire);
    let repeat = TX_REPEAT.load(Ordering::Acquire);
    if repeat_pos >= repeat {
        return level_duration_reset();
    }

    let edge = unsafe { *ptr.add(pos as usize) };
    let mut next_pos = pos + 1;
    let mut next_repeat_pos = repeat_pos;
    if next_pos >= len {
        next_pos = 0;
        next_repeat_pos += 1;
    }
    TX_POS.store(next_pos, Ordering::Release);
    TX_REPEAT_POS.store(next_repeat_pos, Ordering::Release);
    level_duration(edge.level, edge.duration_us)
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 3: Implement transmit**

Replace the stub `transmit` with:

```rust
pub fn transmit(params: &Value) -> Result<Value, (u32, String)> {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) || !TX_PTR.load(Ordering::Acquire).is_null() {
        return Err((ERR_BUSY, "subghz busy".to_string()));
    }
    let frequency = required_frequency(params)?;
    let preset = required_preset(params)?;
    let edges = parse_edges(params)?;
    let repeat = repeat_count(params)?;
    let device = internal_device()?;
    if unsafe { !sys::subghz_devices_is_frequency_valid(device, frequency) } {
        return Err((ERR_BAD_PARAMS, "invalid subghz frequency".to_string()));
    }
    if unsafe { !sys::subghz_devices_begin(device) } {
        return Err((ERR_BUSY, "subghz device unavailable".to_string()));
    }

    TX_PTR.store(edges.as_ptr() as *mut TxEdge, Ordering::Release);
    TX_LEN.store(edges.len() as u32, Ordering::Release);
    TX_POS.store(0, Ordering::Release);
    TX_REPEAT.store(repeat, Ordering::Release);
    TX_REPEAT_POS.store(0, Ordering::Release);

    let allowed = unsafe {
        sys::subghz_devices_reset(device);
        sys::subghz_devices_load_preset(device, preset, core::ptr::null_mut());
        sys::subghz_devices_set_frequency(device, frequency);
        sys::subghz_devices_flush_tx(device);
        sys::furi_hal_subghz_start_async_tx(Some(tx_yield), core::ptr::null_mut())
    };
    if !allowed {
        TX_PTR.store(core::ptr::null_mut(), Ordering::Release);
        unsafe {
            sys::subghz_devices_idle(device);
            sys::subghz_devices_sleep(device);
            sys::subghz_devices_end(device);
        }
        return Err((ERR_BAD_PARAMS, "subghz transmit not allowed on this frequency".to_string()));
    }

    let total_us = edges
        .iter()
        .fold(0u64, |acc, edge| acc.saturating_add(edge.duration_us as u64))
        .saturating_mul(repeat as u64);
    let timeout_ms = (total_us / 1000).saturating_add(1000).min(60_000) as u32;
    let mut waited_ms = 0u32;
    while unsafe { !sys::furi_hal_subghz_is_async_tx_complete() } && waited_ms < timeout_ms {
        unsafe { sys::furi_delay_ms(1) };
        waited_ms += 1;
    }

    unsafe {
        sys::furi_hal_subghz_stop_async_tx();
        sys::subghz_devices_idle(device);
        sys::subghz_devices_sleep(device);
        sys::subghz_devices_end(device);
    }
    TX_PTR.store(core::ptr::null_mut(), Ordering::Release);
    if waited_ms >= timeout_ms {
        return Err((ERR_INTERNAL, "subghz transmit timeout".to_string()));
    }

    Ok(Value::Map(alloc::vec![(
        "sent".to_string(),
        Value::U64((edges.len() as u64).saturating_mul(repeat as u64)),
    )]))
}
```

Run: `cd firmware && cargo build --release`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add firmware/src/subghz_instrument.rs
git commit -m "feat(fw): add Sub-GHz raw transmit"
```

## Task 6: Docs + Verification

**Files:**
- Modify: `README.md`
- Use hardware for acceptance.

- [ ] **Step 1: Document Sub-GHz usage**

In `README.md`, add a Sub-GHz section after the IR section:

````markdown
### Sub-GHz raw records

Sub-GHz requires an explicit frequency and preset:

```sh
flip subghz capture --freq 433920000 --preset ook650 --idle-gap 500 --output remote.subghz
flip subghz transmit --file remote.subghz
```

`--freq` is in Hz. The firmware validates frequencies through the Flipper Sub-GHz
HAL and refuses transmit when the device/region does not allow it. There is no
default RF frequency.

Raw `.subghz` records store `frequency`, `preset`, and level/duration samples.
This is Layer 1 raw replay, not the later arbitrary byte link.
````

Run: `cargo test`

Expected: PASS.

- [ ] **Step 2: Run firmware build**

Run: `cd firmware && cargo build --release`

Expected: builds `firmware/target/thumbv7em-none-eabihf/release/flip_link.fap`.

- [ ] **Step 3: Run host verification**

Run:

```bash
cargo test
cargo test -p flip-proto --features alloc
cargo build
```

Expected: all pass.

- [ ] **Step 4 [HW]: Launch firmware and inspect caps**

Operator:

```bash
cd firmware && run-fap target/thumbv7em-none-eabihf/release/flip_link.fap
```

From repo root:

```bash
./target/debug/flip caps
```

Expected: `subghz.capture` and `subghz.transmit` appear under `subghz`.

- [ ] **Step 5 [HW]: Capture a known Sub-GHz signal**

Use a legal local test frequency/preset.

```bash
./target/debug/flip subghz capture --freq 433920000 --preset ook650 --idle-gap 500 --output /tmp/test.subghz
```

Expected: command exits after post-data silence and writes a file with header:

```text
# format=flip-subghz-raw-v1
# frequency=433920000
# preset=ook650
```

If no source is available, run a timed noise capture instead:

```bash
./target/debug/flip subghz capture --freq 433920000 --preset ook650 --duration 1000 --output /tmp/noise.subghz
```

Expected: either a non-empty record or a clear `no Sub-GHz signal captured` error, with `flip status` still succeeding afterward.

- [ ] **Step 6 [HW]: Transmit captured record**

```bash
./target/debug/flip subghz transmit --file /tmp/test.subghz
./target/debug/flip status
```

Expected: transmit reports `transmitted N Sub-GHz edges: N`; status still reports device reachable. If transmit is refused for the frequency, the error must say it was invalid or not allowed, and the device must not crash.

- [ ] **Step 7: Commit**

```bash
git add README.md
git commit -m "docs: add Sub-GHz raw record usage"
```

## Self-Review Checklist

- Spec coverage:
  - Interface roadmap documented in `docs/superpowers/specs/2026-06-07-flipper-interface-roadmap-subghz-design.md`.
  - Slice 1 raw Sub-GHz capture/transmit covered by Tasks 1-6.
  - Slice 2 byte link explicitly deferred until raw lifecycle is proven.
- Placeholder scan:
  - No `TBD`, `TODO`, or "fill in later" steps.
  - Hardware-dependent steps have explicit commands and expected outcomes.
- Type consistency:
  - Host file format uses `frequency`, `preset`, `edges`.
  - Firmware request parser expects `frequency`, `preset`, `edges`, `repeat`.
  - Stream format is `STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1`.
  - Stream payload record is exactly 5 bytes: level + little-endian `u32` duration.
