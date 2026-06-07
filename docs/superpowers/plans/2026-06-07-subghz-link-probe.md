# Sub-GHz Link Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a diagnostic `subghz.link_probe` path that starts the SDK byte worker, writes a small payload, drains any received bytes, and reports stability-oriented counters.

**Architecture:** Keep this as a one-shot diagnostic opcode, not a file-transfer protocol. Host code builds and parses a small CBOR `Value` map, the CLI exposes `flip subghz link-probe`, and firmware owns the `SubGhzTxRxWorker` lifecycle inside `subghz_instrument.rs`. The daemon does not change because this slice uses bounded one-shot requests.

**Tech Stack:** Rust host crates, Clap, flip-link CBOR `Value`, `flipperzero-sys` 0.16 Sub-GHz device and `subghz_tx_rx_worker_*` APIs.

---

## File Structure

- Modify: `crates/flip-client/src/subghz.rs`
  - Add probe payload validation, hex parsing, request map construction, and response parsing.
- Modify: `crates/flip-client/src/lib.rs`
  - Export the result type and add `subghz_link_probe`.
- Modify: `crates/flip-cli/src/main.rs`
  - Add `flip subghz link-probe` and output formatting.
- Modify: `firmware/src/registry.rs`
  - Advertise and dispatch `subghz.link_probe`.
- Modify: `firmware/src/subghz_instrument.rs`
  - Implement the worker lifecycle and result map.
- Modify: `README.md`
  - Add a short diagnostic command example.

---

## Task 1: Host Probe Model

**Files:**
- Modify: `crates/flip-client/src/subghz.rs`

- [ ] **Step 1: Add failing host tests**

Append these tests to the existing `#[cfg(test)] mod tests` in `crates/flip-client/src/subghz.rs`:

```rust
#[test]
fn link_probe_params_validate_payload_and_timeout() {
    let params = link_probe_params(433_920_000, b"hello", Duration::from_millis(250)).unwrap();

    assert_eq!(
        params.get("frequency"),
        Some(&flip_proto::Value::U64(433_920_000))
    );
    assert_eq!(
        params.get("payload"),
        Some(&flip_proto::Value::Bytes(b"hello".to_vec()))
    );
    assert_eq!(params.get("timeout_ms"), Some(&flip_proto::Value::U64(250)));

    assert!(link_probe_params(433_920_000, b"", Duration::from_millis(250)).is_err());
    assert!(
        link_probe_params(
            433_920_000,
            &[0xaa; MAX_LINK_PROBE_BYTES + 1],
            Duration::from_millis(250)
        )
        .is_err()
    );
}

#[test]
fn parses_link_probe_result_map() {
    let value = flip_proto::Value::Map(vec![
        ("written".into(), flip_proto::Value::U64(5)),
        ("read".into(), flip_proto::Value::U64(2)),
        ("callbacks".into(), flip_proto::Value::U64(1)),
        ("rx_preview".into(), flip_proto::Value::Bytes(vec![0xab, 0xcd])),
    ]);

    assert_eq!(
        link_probe_result(&value).unwrap(),
        SubGhzLinkProbeResult {
            written: 5,
            read: 2,
            callbacks: 1,
            rx_preview: vec![0xab, 0xcd],
        }
    );
}

#[test]
fn parses_hex_probe_payload() {
    assert_eq!(parse_probe_hex("0x6865 6c6c6f").unwrap(), b"hello");
    assert!(parse_probe_hex("abc").is_err());
    assert!(parse_probe_hex("zz").is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```sh
cargo test -p flip-client subghz::tests::link_probe
```

Expected: FAIL to compile because `link_probe_params`, `MAX_LINK_PROBE_BYTES`, `SubGhzLinkProbeResult`, `link_probe_result`, and `parse_probe_hex` do not exist.

- [ ] **Step 3: Implement host probe helpers**

In `crates/flip-client/src/subghz.rs`, add `Duration` to the imports:

```rust
use std::time::Duration;
```

Add this code after `MAX_SUBGHZ_DURATION_US`:

```rust
pub const MAX_LINK_PROBE_BYTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGhzLinkProbeResult {
    pub written: u64,
    pub read: u64,
    pub callbacks: u64,
    pub rx_preview: Vec<u8>,
}
```

Add these helpers before `edge_to_value`:

```rust
pub fn parse_probe_hex(input: &str) -> Result<Vec<u8>> {
    let mut cleaned = String::new();
    for part in input.split_whitespace() {
        let part = part.strip_prefix("0x").unwrap_or(part);
        cleaned.push_str(part);
    }
    if cleaned.len() % 2 != 0 {
        return Err(anyhow!("hex payload must contain an even number of digits"));
    }

    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for index in (0..cleaned.len()).step_by(2) {
        let byte = u8::from_str_radix(&cleaned[index..index + 2], 16)
            .map_err(|_| anyhow!("invalid hex byte '{}'", &cleaned[index..index + 2]))?;
        out.push(byte);
    }
    validate_probe_payload(&out)?;
    Ok(out)
}

pub(crate) fn link_probe_params(
    frequency: u32,
    payload: &[u8],
    timeout: Duration,
) -> Result<Value> {
    validate_probe_payload(payload)?;
    let timeout_ms = timeout.as_millis();
    if timeout_ms == 0 || timeout_ms > 5_000 {
        return Err(anyhow!("link probe timeout must be 1..=5000 ms"));
    }
    Ok(Value::Map(vec![
        ("frequency".into(), Value::U64(frequency as u64)),
        ("payload".into(), Value::Bytes(payload.to_vec())),
        ("timeout_ms".into(), Value::U64(timeout_ms as u64)),
    ]))
}

pub(crate) fn link_probe_result(value: &Value) -> Result<SubGhzLinkProbeResult> {
    Ok(SubGhzLinkProbeResult {
        written: required_u64(value, "written")?,
        read: required_u64(value, "read")?,
        callbacks: required_u64(value, "callbacks")?,
        rx_preview: match value.get("rx_preview") {
            Some(Value::Bytes(bytes)) => bytes.clone(),
            _ => return Err(anyhow!("link probe response missing rx_preview bytes")),
        },
    })
}

fn validate_probe_payload(payload: &[u8]) -> Result<()> {
    if payload.is_empty() {
        return Err(anyhow!("link probe payload is empty"));
    }
    if payload.len() > MAX_LINK_PROBE_BYTES {
        return Err(anyhow!(
            "link probe payload too large: {} bytes (max {})",
            payload.len(),
            MAX_LINK_PROBE_BYTES
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, key: &str) -> Result<u64> {
    match value.get(key) {
        Some(Value::U64(n)) => Ok(*n),
        _ => Err(anyhow!("link probe response missing numeric {key} field")),
    }
}
```

- [ ] **Step 4: Run host tests**

Run:

```sh
cargo test -p flip-client subghz::tests::link_probe
```

Expected: PASS.

- [ ] **Step 5: Commit host model**

Run:

```sh
git add crates/flip-client/src/subghz.rs
git commit -m "feat(client): add Sub-GHz link probe model"
```

---

## Task 2: Client API and CLI

**Files:**
- Modify: `crates/flip-client/src/lib.rs`
- Modify: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Add failing client API test**

Append this test to `#[cfg(test)] mod tests` in `crates/flip-client/src/lib.rs`:

```rust
#[test]
fn link_probe_result_rejects_wrong_shape() {
    assert!(subghz::link_probe_result(&Value::Null).is_err());
    assert!(subghz::link_probe_result(&Value::Map(vec![
        ("written".into(), Value::U64(1)),
        ("read".into(), Value::U64(0)),
        ("callbacks".into(), Value::U64(0)),
        ("rx_preview".into(), Value::Text("not bytes".into())),
    ]))
    .is_err());
}
```

- [ ] **Step 2: Add failing CLI tests**

Append these tests to `#[cfg(test)] mod tests` in `crates/flip-cli/src/main.rs`:

```rust
#[test]
fn subghz_link_probe_accepts_data_payload() {
    let cli = Cli::try_parse_from([
        "flip",
        "subghz",
        "link-probe",
        "--freq",
        "433920000",
        "--data",
        "hello",
        "--timeout",
        "250",
    ])
    .unwrap();

    let Cmd::SubGhz {
        cmd:
            SubGhzCmd::LinkProbe {
                freq,
                data,
                hex,
                timeout,
            },
    } = cli.cmd
    else {
        panic!("expected subghz link-probe");
    };

    assert_eq!(freq, 433_920_000);
    assert_eq!(data.as_deref(), Some("hello"));
    assert_eq!(hex, None);
    assert_eq!(timeout, 250);
}

#[test]
fn subghz_link_probe_requires_one_payload_source() {
    let missing = Cli::try_parse_from(["flip", "subghz", "link-probe", "--freq", "433920000"])
        .unwrap_err();
    assert_eq!(missing.kind(), clap::error::ErrorKind::MissingRequiredArgument);

    let conflict = Cli::try_parse_from([
        "flip",
        "subghz",
        "link-probe",
        "--freq",
        "433920000",
        "--data",
        "hello",
        "--hex",
        "6869",
    ])
    .unwrap_err();
    assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```sh
cargo test -p flip-client link_probe_result_rejects_wrong_shape
cargo test -p flip-cli subghz_link_probe
```

Expected: FAIL to compile because the client result helper is private to the test path and the CLI variant does not exist.

- [ ] **Step 4: Add client API**

In `crates/flip-client/src/lib.rs`, change the export line:

```rust
pub use subghz::{SubGhzEdge, SubGhzLinkProbeResult, SubGhzPreset, SubGhzSignal};
```

Add this function after `subghz_transmit`:

```rust
pub fn subghz_link_probe(
    frequency: u32,
    payload: &[u8],
    timeout: Duration,
) -> Result<SubGhzLinkProbeResult> {
    let params = subghz::link_probe_params(frequency, payload, timeout)?;
    let host_timeout = timeout
        .checked_add(Duration::from_secs(3))
        .unwrap_or(Duration::from_secs(8));
    let resp = invoke("subghz", "link_probe", params, host_timeout)?;
    subghz::link_probe_result(&resp.result)
}
```

- [ ] **Step 5: Add CLI command and output**

In `crates/flip-cli/src/main.rs`, change the import:

```rust
use flip_client::{DaemonStatus, DeviceStatus, IrSignal, SubGhzPreset, SubGhzSignal};
```

to:

```rust
use flip_client::{
    DaemonStatus, DeviceStatus, IrSignal, SubGhzPreset, SubGhzSignal, subghz::parse_probe_hex,
};
```

Add this variant to `SubGhzCmd`:

```rust
    /// Probe the SDK Sub-GHz byte worker with a small payload.
    LinkProbe {
        /// Frequency in Hz. Required; there is no default RF frequency.
        #[arg(long)]
        freq: u32,
        /// UTF-8 text payload to write.
        #[arg(long, conflicts_with = "hex", required_unless_present = "hex")]
        data: Option<String>,
        /// Hex payload to write, for example 0x68656c6c6f.
        #[arg(long, conflicts_with = "data")]
        hex: Option<String>,
        /// Firmware wait after writing, in ms.
        #[arg(long, default_value_t = 500)]
        timeout: u64,
    },
```

Add this match arm inside `Cmd::SubGhz { cmd } => match cmd`:

```rust
            SubGhzCmd::LinkProbe {
                freq,
                data,
                hex,
                timeout,
            } => {
                let payload = match (data, hex) {
                    (Some(data), None) => data.into_bytes(),
                    (None, Some(hex)) => parse_probe_hex(&hex)?,
                    _ => unreachable!("clap enforces exactly one payload source"),
                };
                let result =
                    flip_client::subghz_link_probe(freq, &payload, Duration::from_millis(timeout))?;
                println!(
                    "link probe wrote {} bytes; read {} bytes; callbacks {}",
                    result.written, result.read, result.callbacks
                );
                if !result.rx_preview.is_empty() {
                    println!("rx preview: {}", hex_preview(&result.rx_preview));
                }
                Ok(())
            }
```

Add this helper after `render_value`:

```rust
fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}
```

- [ ] **Step 6: Run tests**

Run:

```sh
cargo test -p flip-client link_probe_result_rejects_wrong_shape
cargo test -p flip-cli subghz_link_probe
```

Expected: PASS.

- [ ] **Step 7: Commit client and CLI**

Run:

```sh
git add crates/flip-client/src/lib.rs crates/flip-cli/src/main.rs
git commit -m "feat(cli): add Sub-GHz link probe command"
```

---

## Task 3: Firmware Registry and Stub

**Files:**
- Modify: `firmware/src/registry.rs`
- Modify: `firmware/src/subghz_instrument.rs`

- [ ] **Step 1: Add registry entry**

Replace `SUBGHZ_OPCODES` in `firmware/src/registry.rs` with:

```rust
static SUBGHZ_OPCODES: &[OpcodeEntry] = &[
    OpcodeEntry {
        opcode: "transmit",
        handler: crate::subghz_instrument::transmit,
    },
    OpcodeEntry {
        opcode: "link_probe",
        handler: crate::subghz_instrument::link_probe,
    },
];
```

- [ ] **Step 2: Add firmware stub**

In `firmware/src/subghz_instrument.rs`, add this function after `transmit`:

```rust
pub fn link_probe(_params: &Value) -> Result<Value, (u32, String)> {
    Err((ERR_INTERNAL, "subghz link probe not implemented".to_string()))
}
```

- [ ] **Step 3: Build firmware**

Run:

```sh
cd firmware && cargo build --release
```

Expected: PASS. This proves the registry dispatch shape compiles before implementing the worker.

- [ ] **Step 4: Commit registry stub**

Run:

```sh
git add firmware/src/registry.rs firmware/src/subghz_instrument.rs
git commit -m "feat(fw): advertise Sub-GHz link probe"
```

---

## Task 4: Firmware Link Probe Worker

**Files:**
- Modify: `firmware/src/subghz_instrument.rs`

- [ ] **Step 1: Add constants and callback counter**

Update the atomic import:

```rust
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU32, Ordering};
```

Add these constants near the existing Sub-GHz constants:

```rust
const MAX_LINK_PROBE_BYTES: usize = 64;
const MAX_LINK_PROBE_TIMEOUT_MS: u32 = 5_000;
const DEFAULT_LINK_PROBE_TIMEOUT_MS: u32 = 500;
```

Add this static near the TX atomics:

```rust
static LINK_PROBE_CALLBACKS: AtomicU32 = AtomicU32::new(0);
```

Add this callback after `rx_capture_isr`:

```rust
unsafe extern "C" fn link_probe_have_read(_context: *mut core::ffi::c_void) {
    LINK_PROBE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}
```

- [ ] **Step 2: Add payload and timeout parsers**

Add these helpers after `repeat_count`:

```rust
fn probe_payload(params: &Value) -> Result<Vec<u8>, (u32, String)> {
    let bytes = match params.get("payload") {
        Some(Value::Bytes(bytes)) => bytes,
        _ => return Err((ERR_BAD_PARAMS, "payload bytes required".to_string())),
    };
    if bytes.is_empty() {
        return Err((ERR_BAD_PARAMS, "payload empty".to_string()));
    }
    if bytes.len() > MAX_LINK_PROBE_BYTES {
        return Err((ERR_OVERSIZED, "payload too large".to_string()));
    }
    Ok(bytes.clone())
}

fn probe_timeout_ms(params: &Value) -> Result<u32, (u32, String)> {
    let timeout = params
        .get("timeout_ms")
        .and_then(as_u64)
        .unwrap_or(DEFAULT_LINK_PROBE_TIMEOUT_MS as u64);
    if timeout == 0 || timeout > MAX_LINK_PROBE_TIMEOUT_MS as u64 {
        return Err((ERR_BAD_PARAMS, "timeout_ms out of range".to_string()));
    }
    Ok(timeout as u32)
}
```

- [ ] **Step 3: Replace stub with worker implementation**

Replace the stub `link_probe` with:

```rust
pub fn link_probe(params: &Value) -> Result<Value, (u32, String)> {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) || !TX_PTR.load(Ordering::Acquire).is_null() {
        return Err((ERR_BUSY, "subghz busy".to_string()));
    }

    let frequency = required_frequency(params)?;
    let mut payload = probe_payload(params)?;
    let timeout_ms = probe_timeout_ms(params)?;
    let device = internal_device();
    if device.is_null() {
        return Err((
            ERR_INTERNAL,
            "subghz internal device unavailable".to_string(),
        ));
    }
    if unsafe { !sys::subghz_devices_is_frequency_valid(device, frequency) } {
        return Err((ERR_BAD_PARAMS, "invalid subghz frequency".to_string()));
    }
    if unsafe { !sys::subghz_devices_begin(device) } {
        return Err((ERR_BUSY, "subghz device unavailable".to_string()));
    }

    let worker = unsafe { sys::subghz_tx_rx_worker_alloc() };
    if worker.is_null() {
        unsafe { sys::subghz_devices_end(device) };
        return Err((ERR_INTERNAL, "subghz worker allocation failed".to_string()));
    }

    LINK_PROBE_CALLBACKS.store(0, Ordering::Release);
    unsafe {
        sys::subghz_tx_rx_worker_set_callback_have_read(
            worker,
            Some(link_probe_have_read),
            core::ptr::null_mut(),
        );
    }

    let started = unsafe { sys::subghz_tx_rx_worker_start(worker, device, frequency) };
    if !started {
        unsafe {
            sys::subghz_tx_rx_worker_free(worker);
            sys::subghz_devices_idle(device);
            sys::subghz_devices_sleep(device);
            sys::subghz_devices_end(device);
        }
        return Err((ERR_BUSY, "subghz link worker unavailable".to_string()));
    }

    let wrote =
        unsafe { sys::subghz_tx_rx_worker_write(worker, payload.as_mut_ptr(), payload.len()) };
    let mut read_total = 0u64;
    let mut rx_preview = Vec::new();
    let mut waited_ms = 0u32;

    while waited_ms < timeout_ms {
        let available = unsafe { sys::subghz_tx_rx_worker_available(worker) };
        if available > 0 {
            let mut buf = [0u8; MAX_LINK_PROBE_BYTES];
            let want = core::cmp::min(available, buf.len());
            let got = unsafe { sys::subghz_tx_rx_worker_read(worker, buf.as_mut_ptr(), want) };
            read_total = read_total.saturating_add(got as u64);
            let room = MAX_LINK_PROBE_BYTES.saturating_sub(rx_preview.len());
            if room > 0 {
                let take = core::cmp::min(room, got);
                rx_preview.extend_from_slice(&buf[..take]);
            }
        }
        unsafe { sys::furi_delay_ms(1) };
        waited_ms += 1;
    }

    unsafe {
        sys::subghz_tx_rx_worker_stop(worker);
        sys::subghz_tx_rx_worker_free(worker);
        sys::subghz_devices_idle(device);
        sys::subghz_devices_sleep(device);
        sys::subghz_devices_end(device);
    }

    if !wrote {
        return Err((ERR_INTERNAL, "subghz link worker write failed".to_string()));
    }

    Ok(Value::Map(alloc::vec![
        ("written".to_string(), Value::U64(payload.len() as u64)),
        ("read".to_string(), Value::U64(read_total)),
        (
            "callbacks".to_string(),
            Value::U64(LINK_PROBE_CALLBACKS.load(Ordering::Acquire) as u64),
        ),
        ("rx_preview".to_string(), Value::Bytes(rx_preview)),
    ]))
}
```

- [ ] **Step 4: Format and build firmware**

Run:

```sh
cd firmware && cargo fmt
cd firmware && cargo build --release
```

Expected: PASS. If the build fails because the worker callback signature differs, use the exact `SubGhzTxRxWorkerCallbackHaveRead` type from `flipperzero-sys` bindings and rerun the same build.

- [ ] **Step 5: Commit firmware worker**

Run:

```sh
git add firmware/src/subghz_instrument.rs
git commit -m "feat(fw): implement Sub-GHz link probe"
```

---

## Task 5: README and Verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document diagnostic usage**

In the README Sub-GHz section, add this paragraph after the raw capture/transmit example:

````markdown
For byte-worker diagnostics with one Flipper, use `link-probe`:

```sh
flip subghz link-probe --freq 433920000 --data hello
flip subghz link-probe --freq 433920000 --hex 0x68656c6c6f --timeout 250
```

This only proves the FAP can start the SDK Sub-GHz byte worker and write a small
payload without destabilizing the device. End-to-end byte transfer requires a
second Flipper running a receive command in a later slice.
````

- [ ] **Step 2: Run full verification**

Run:

```sh
cargo fmt
cargo test
cargo test -p flip-proto --features alloc
cd firmware && cargo build --release
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 3: Rebuild debug CLI and check help**

Run:

```sh
cargo build
./target/debug/flip subghz link-probe --help
```

Expected: help shows `--freq`, `--data`, `--hex`, and `--timeout`.

- [ ] **Step 4: Single-device hardware smoke**

Flash and run the FAP:

```sh
just fw-run
```

In another terminal, run:

```sh
./target/debug/flip caps
./target/debug/flip subghz link-probe --freq 433920000 --data hello
./target/debug/flip status
```

Expected:

- `caps` lists `subghz.link_probe`.
- `link-probe` prints `link probe wrote 5 bytes; read 0 bytes; callbacks 0` or the same shape with nonzero read/callback counts.
- `status` reports the device reachable immediately afterward.

- [ ] **Step 5: Commit docs and verified slice**

Run:

```sh
git add README.md
git commit -m "docs(subghz): document link probe diagnostic"
```

If any code changed during hardware debugging, include those files in the same commit only if they directly fix link-probe behavior:

```sh
git add README.md crates/flip-cli/src/main.rs crates/flip-client/src/lib.rs crates/flip-client/src/subghz.rs firmware/src/registry.rs firmware/src/subghz_instrument.rs
git commit -m "fix(subghz): stabilize link probe diagnostic"
```
