# flip-link Slice 1c — IR Capture + Streaming Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add raw-signal **streaming** (`STREAM_START`/`STREAM_DATA`/`STREAM_STOP`) and the first streaming instrument — **IR capture** — closing the capture→transmit loop: `flip ir capture` records a remote into a timings file that `flip ir transmit --file` replays.

**Architecture:** The firmware gains a *capture lifecycle*: `ir.capture` arms `furi_hal_infrared_async_rx_*`; the RX ISR pushes `int32` µs durations into a capture stream buffer (drops counted); the main loop drains it into `STREAM_DATA` frames (raw bytes, no CBOR) while still serving normal frames; a client `STREAM_STOP` (or Back) stops RX, flushes, and sends the final `STREAM_STOP {dropped}`. The daemon learns streaming **generically**: when a forwarded REQ's first reply is `STREAM_START` (not `RESP`/`ERROR`), it keeps that seq's route alive and relays bidirectionally — device `STREAM_DATA`/`STREAM_STOP` → client, client `STREAM_STOP` → device — until the final stop. The CLI's `flip ir capture` reads the stream, ends on **Ctrl-C** or **`--auto-end <ms>`** (silence gap), and writes timings in the `ir transmit --file` format.

**Tech Stack:** Rust; `flipperzero-sys` 0.16 IR RX HAL (verified present); `minicbor` for the two new stream bodies; `ctrlc` crate for the CLI SIGINT handler. Builds on Slice 1b. Reference: the C prototype's `stream.c` + capture lifecycle in `../mcp/firmware/`.

**Scope:** IR **capture** + the generic streaming path. One capture at a time. Frame format and all of Slices 1a/1b are unchanged.

**Hardware note:** **[HW]** steps need a Flipper running the FAP and an IR remote to point at it. Everything else is hardware-free.

---

## File Structure

```
crates/flip-proto/src/messages.rs   # + StreamStart{format}, StreamStop{dropped}
firmware/src/ir_instrument.rs        # + capture: RX ISR, start/drain/stop, capture globals
firmware/src/registry.rs             # advertise ir.capture (streaming op) in CAPS
firmware/src/main.rs                 # main loop drains capture; handle_frame routes capture + STREAM_STOP; teardown finishes capture
crates/flip-daemon/src/router.rs     # deliver() no longer auto-removes; explicit unregister
crates/flip-daemon/src/server.rs     # serve_client: stream relay when first reply is STREAM_START
crates/flip-cli/Cargo.toml           # + ctrlc
crates/flip-cli/src/ir.rs            # + decode STREAM_DATA, format timings for output
crates/flip-cli/src/capture.rs       # NEW: stream read loop, Ctrl-C / auto-end, write output
crates/flip-cli/src/main.rs          # `flip ir capture` subcommand
```

---

## Task 1: flip-proto — stream message bodies

**Files:**
- Modify: `crates/flip-proto/src/messages.rs`

- [ ] **Step 1: Add the two stream bodies + a test**

In `crates/flip-proto/src/messages.rs`, after the `AgentError` struct, add:
```rust
/// STREAM_START body (device→client): declares the stream's sample format.
#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct StreamStart {
    #[n(0)]
    pub format: String,
}

/// STREAM_STOP body (device→client final frame): how many samples were dropped.
#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct StreamStop {
    #[n(0)]
    pub dropped: u32,
}

/// The raw-sample format IR capture uses: little-endian i32 microsecond durations.
pub const STREAM_FORMAT_RAW_I32_US: &str = "raw_int32_le_us";
```
Add to the `tests` module:
```rust
    #[test]
    fn stream_bodies_round_trip() {
        let s = StreamStart { format: STREAM_FORMAT_RAW_I32_US.to_string() };
        assert_eq!(from_payload::<StreamStart>(&to_payload(&s)).unwrap(), s);
        let p = StreamStop { dropped: 3 };
        assert_eq!(from_payload::<StreamStop>(&to_payload(&p)).unwrap(), p);
    }
```

- [ ] **Step 2: Re-export from lib.rs**

In `crates/flip-proto/src/lib.rs`, extend the messages re-export line to include the new types:
```rust
#[cfg(feature = "alloc")]
pub use messages::{AgentError, Caps, Hello, Instrument, Req, Resp, StreamStart, StreamStop};
```

- [ ] **Step 3: Test**

Run: `cargo test -p flip-proto --features alloc`
Expected: PASS incl. `stream_bodies_round_trip`.

- [ ] **Step 4: Commit**

```bash
git add crates/flip-proto/src/messages.rs crates/flip-proto/src/lib.rs
git commit -m "feat(proto): STREAM_START/STREAM_STOP bodies"
```

---

## Task 2: firmware — IR capture lifecycle

Adds capture to `ir_instrument.rs`: a capture stream buffer (ISR→main), an RX capture ISR pushing `i32` µs durations, and `start_capture`/`drain_capture`/`stop_capture`. Mirrors the proven C prototype (`../mcp/firmware/instruments/infrared.c` + the app's capture engine).

**Files:**
- Modify: `firmware/src/ir_instrument.rs`

- [ ] **Step 1: Add capture state + ISR + lifecycle functions**

Append to `firmware/src/ir_instrument.rs`:
```rust
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32};

/// Silence (µs) after which the RX hardware reports a timeout (burst ended).
const RX_TIMEOUT_US: u32 = 150_000;
/// Capture stream buffer capacity in bytes (i32 samples).
const CAPTURE_CAP: usize = 4096;

static CAPTURE_STREAM: AtomicPtr<sys::FuriStreamBuffer> = AtomicPtr::new(core::ptr::null_mut());
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE_SEQ: AtomicU16 = AtomicU16::new(0);
static CAPTURE_DROPPED: AtomicU32 = AtomicU32::new(0);

/// RX capture ISR: push each edge duration as a little-endian i32 (µs) into the
/// capture buffer. `level` is implicit in ordering (host replays durations in
/// order). On buffer-full, count a drop; never block the ISR.
unsafe extern "C" fn rx_capture_isr(_ctx: *mut core::ffi::c_void, _level: bool, duration: u32) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() {
        return;
    }
    let d = duration as i32;
    let bytes = d.to_le_bytes();
    let sent = unsafe {
        sys::furi_stream_buffer_send(sb, bytes.as_ptr() as *const core::ffi::c_void, 4, 0)
    };
    if sent != 4 {
        CAPTURE_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// RX silence-timeout ISR: the host decides when to stop; nothing to do here.
unsafe extern "C" fn rx_timeout_isr(_ctx: *mut core::ffi::c_void) {}

/// True if a capture is currently streaming.
pub fn capture_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Acquire)
}

/// Allocate the capture buffer once, lazily.
fn ensure_capture_buffer() -> *mut sys::FuriStreamBuffer {
    let existing = CAPTURE_STREAM.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let sb = unsafe { sys::furi_stream_buffer_alloc(CAPTURE_CAP, 4) };
    CAPTURE_STREAM.store(sb, Ordering::Release);
    sb
}

/// Start an IR capture for request `seq`: send STREAM_START, arm RX. The caller
/// (main loop) drains via `drain_capture` and ends via `stop_capture`. On busy,
/// sends an ERROR and does not start. `send_start`/`send_error` are provided by
/// main.rs (see Task 3) to avoid a circular module dependency.
pub fn start_capture(
    seq: u16,
    send_start: impl FnOnce(u16, &str),
    send_error: impl FnOnce(u16, u32, &str),
) {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) || unsafe { sys::furi_hal_infrared_is_busy() } {
        send_error(seq, ERR_BUSY, "ir busy");
        return;
    }
    let sb = ensure_capture_buffer();
    if sb.is_null() {
        send_error(seq, flip_proto::messages::ERR_INTERNAL, "no capture buffer");
        return;
    }
    // Drain any stale bytes from a previous capture.
    let mut scratch = [0u8; 64];
    while unsafe {
        sys::furi_stream_buffer_receive(sb, scratch.as_mut_ptr() as *mut core::ffi::c_void, 64, 0)
    } > 0
    {}
    CAPTURE_DROPPED.store(0, Ordering::Release);
    CAPTURE_SEQ.store(seq, Ordering::Release);
    CAPTURE_ACTIVE.store(true, Ordering::Release);

    send_start(seq, flip_proto::messages::STREAM_FORMAT_RAW_I32_US);

    unsafe {
        sys::furi_hal_infrared_async_rx_set_capture_isr_callback(
            Some(rx_capture_isr),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_rx_set_timeout_isr_callback(
            Some(rx_timeout_isr),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_rx_set_timeout(RX_TIMEOUT_US);
        sys::furi_hal_infrared_async_rx_start();
    }
}

/// Drain available whole i32 samples into one STREAM_DATA frame (non-blocking).
/// Call every main-loop iteration while `capture_active`.
pub fn drain_capture(send_data: impl FnOnce(u16, &[u8])) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() || !CAPTURE_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let mut batch = [0u8; 256];
    let got = unsafe {
        sys::furi_stream_buffer_receive(sb, batch.as_mut_ptr() as *mut core::ffi::c_void, 256, 0)
    };
    let whole = got - (got % 4);
    if whole >= 4 {
        send_data(CAPTURE_SEQ.load(Ordering::Acquire), &batch[..whole]);
    }
}

/// Stop the active capture: stop RX, flush remaining samples, send final
/// STREAM_STOP{dropped}. Safe to call when no capture is active (no-op).
pub fn stop_capture(send_data: impl Fn(u16, &[u8]), send_stop: impl FnOnce(u16, u32)) {
    if !CAPTURE_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    unsafe {
        sys::furi_hal_infrared_async_rx_stop();
    }
    let seq = CAPTURE_SEQ.load(Ordering::Acquire);
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if !sb.is_null() {
        loop {
            let mut batch = [0u8; 256];
            let got = unsafe {
                sys::furi_stream_buffer_receive(
                    sb,
                    batch.as_mut_ptr() as *mut core::ffi::c_void,
                    256,
                    0,
                )
            };
            let whole = got - (got % 4);
            if whole < 4 {
                break;
            }
            send_data(seq, &batch[..whole]);
        }
    }
    send_stop(seq, CAPTURE_DROPPED.load(Ordering::Acquire));
}
```

- [ ] **Step 2: Build**

Run: `cd firmware && cargo build --release`
Expected: compiles (capture fns unused until Task 3 wires them; warnings OK). Confirm the RX sys symbols resolve (`furi_hal_infrared_async_rx_set_capture_isr_callback`, `_set_timeout_isr_callback`, `_set_timeout`, `_start`, `_stop`) — verified present in flipperzero-sys 0.16.

- [ ] **Step 3: Commit**

```bash
git add firmware/src/ir_instrument.rs
git commit -m "feat(fw): IR capture lifecycle (RX ISR -> stream buffer -> STREAM_DATA)"
```

---

## Task 3: firmware — wire capture into the main loop

`handle_frame` routes `ir.capture` REQ to `start_capture` and `STREAM_STOP` to `stop_capture`; the main loop drains while active; teardown finishes any active capture. Frame-send helpers are passed as closures so `ir_instrument` stays decoupled.

**Files:**
- Modify: `firmware/src/main.rs`

- [ ] **Step 1: Add stream frame-send helpers**

In `firmware/src/main.rs`, after `send_msg`, add:
```rust
/// Send a STREAM_START frame with a `{format}` body.
fn send_stream_start(seq: u16, format: &str) {
    send_msg(
        MsgType::StreamStart,
        seq,
        &flip_proto::StreamStart { format: alloc::string::String::from(format) },
    );
}

/// Send a STREAM_DATA frame with a raw payload (no CBOR).
fn send_stream_data(seq: u16, raw: &[u8]) {
    let mut frame = alloc::vec![0u8; flip_proto::HEADER_SIZE + raw.len() + 2];
    if let Some(n) = encode(MsgType::StreamData, 0, seq, raw, &mut frame) {
        cdc_send_all(&frame[..n]);
    }
}

/// Send the final STREAM_STOP frame with a `{dropped}` body.
fn send_stream_stop(seq: u16, dropped: u32) {
    send_msg(MsgType::StreamStop, seq, &flip_proto::StreamStop { dropped });
}

/// Send an ERROR frame with a `{code,message}` body.
fn send_error(seq: u16, code: u32, message: &str) {
    send_msg(
        MsgType::Error,
        seq,
        &flip_proto::AgentError { code, message: alloc::string::String::from(message) },
    );
}
```

- [ ] **Step 2: Route capture + STREAM_STOP in handle_frame**

Replace the `handle_frame` function body's `match typ { ... }` so it handles capture and stream-stop. The new `handle_frame`:
```rust
/// Handle one decoded control frame (HELLO/REQ/STREAM_STOP); PING is handled
/// inline in the drain loop. Unknown/other types are ignored.
fn handle_frame(typ: MsgType, seq: u16, payload: &[u8]) {
    use flip_proto::messages::{from_payload, Req, Resp};
    match typ {
        MsgType::Hello => {
            let caps = registry::build_caps();
            send_msg(MsgType::Caps, seq, &caps);
        }
        MsgType::Req => match from_payload::<Req>(payload) {
            Ok(req) => {
                // ir.capture is a streaming op — it starts a STREAM, not a RESP.
                if req.instrument == "ir" && req.opcode == "capture" {
                    ir_instrument::start_capture(seq, send_stream_start, |s, c, m| {
                        send_error(s, c, m)
                    });
                } else {
                    match registry::dispatch(&req.instrument, &req.opcode, &req.params) {
                        Ok(result) => send_msg(MsgType::Resp, seq, &Resp { ok: true, result }),
                        Err((code, message)) => send_error(seq, code, &message),
                    }
                }
            }
            Err(_) => send_error(seq, flip_proto::messages::ERR_BAD_PARAMS, "bad REQ body"),
        },
        // Client asks to end the active capture.
        MsgType::StreamStop => {
            ir_instrument::stop_capture(send_stream_data, send_stream_stop);
        }
        _ => {}
    }
}
```
Note: `start_capture`'s `send_error` param is `impl FnOnce(u16, u32, &str)`; pass `|s, c, m| send_error(s, c, m)`. Its `send_start` param is `impl FnOnce(u16, &str)`; pass `send_stream_start`.

- [ ] **Step 3: Drain capture each main-loop iteration**

In `fn main`, inside the `while RUNNING.load(...) && idle < MAX_IDLE` loop, immediately after the `if got == 0 { idle += 1; continue; }` / `idle = 0;` block's frame-drain `loop { ... }` ends (i.e. after the inner frame-drain loop, still inside the while), add a capture drain. Find the end of the inner `loop { ... }` that drains frames; right after it (before the closing brace of the `while`), add:
```rust
        // Stream captured IR samples out while a capture is active.
        if ir_instrument::capture_active() {
            ir_instrument::drain_capture(send_stream_data);
        }
```
Also, so the loop runs promptly during a capture (don't park on a 20ms receive when samples are flowing), this is acceptable as-is: the 20ms `furi_stream_buffer_receive` timeout bounds latency to ~20ms per STREAM_DATA batch, which is fine for IR.

- [ ] **Step 4: Finish capture on exit**

In `fn main`, just before the final `usb_teardown(prev, rx_stream);` (the Back-exit / idle-timeout teardown path), add:
```rust
    // If the user pressed Back mid-capture, finish the stream cleanly.
    ir_instrument::stop_capture(send_stream_data, send_stream_stop);
```

- [ ] **Step 5: Build**

Run: `cd firmware && cargo build --release`
Expected: builds `flip_link.fap`. If a closure type-inference error appears on `start_capture`/`stop_capture` generics, annotate the closure args (`|s: u16, c: u32, m: &str|`). Resolve any borrow issue by confirming the send helpers are free functions (they are).

- [ ] **Step 6: Commit**

```bash
git add firmware/src/main.rs
git commit -m "feat(fw): wire IR capture into main loop (drain + STREAM_STOP + exit)"
```

---

## Task 4: firmware — advertise `ir.capture` in CAPS

`ir.capture` is handled specially (not via the dispatch table), but it must appear in CAPS so `flip caps` lists it. Add a `streaming_opcodes` field to the instrument table that `build_caps` includes.

**Files:**
- Modify: `firmware/src/registry.rs`

- [ ] **Step 1: Add streaming_opcodes to the table + CAPS**

In `firmware/src/registry.rs`:
1. Add a field to `InstrumentEntry`:
```rust
struct InstrumentEntry {
    id: &'static str,
    opcodes: &'static [OpcodeEntry],
    streaming_opcodes: &'static [&'static str],
}
```
2. Update both `INSTRUMENTS` entries to set the new field — `sys` gets `streaming_opcodes: &[]`, `ir` gets `streaming_opcodes: &["capture"]`:
```rust
static INSTRUMENTS: &[InstrumentEntry] = &[
    InstrumentEntry {
        id: "sys",
        opcodes: SYS_OPCODES,
        streaming_opcodes: &[],
    },
    InstrumentEntry {
        id: "ir",
        opcodes: IR_OPCODES,
        streaming_opcodes: &["capture"],
    },
];
```
3. In `build_caps`, include streaming opcodes in each instrument's advertised opcode list:
```rust
pub fn build_caps() -> Caps {
    let instruments = INSTRUMENTS
        .iter()
        .map(|i| {
            let mut opcodes: Vec<String> =
                i.opcodes.iter().map(|o| o.opcode.to_string()).collect();
            opcodes.extend(i.streaming_opcodes.iter().map(|s| s.to_string()));
            Instrument {
                id: i.id.to_string(),
                opcodes,
            }
        })
        .collect();
    Caps {
        protocol_version: PROTOCOL_VERSION,
        instruments,
    }
}
```

- [ ] **Step 2: Build**

Run: `cd firmware && cargo build --release`
Expected: builds; CAPS now advertises `ir.capture`.

- [ ] **Step 3: Commit**

```bash
git add firmware/src/registry.rs
git commit -m "feat(fw): advertise ir.capture (streaming) in CAPS"
```

---

## Task 5: daemon — generic stream relay

The daemon learns streaming generically: a forwarded REQ whose first reply is `STREAM_START` becomes a persistent bidirectional relay until the final `STREAM_STOP`. `deliver` no longer auto-removes routes; `serve_client` owns route lifetime.

**Files:**
- Modify: `crates/flip-daemon/src/router.rs`
- Modify: `crates/flip-daemon/src/server.rs`

- [ ] **Step 1: router — deliver without removing**

In `crates/flip-daemon/src/router.rs`, change `deliver` so it does NOT remove the route (the client decides when a request/stream is done), and keep `unregister` for explicit cleanup. Replace `deliver` with:
```rust
    /// Deliver an inbound device frame to the client that owns its seq (if any).
    /// The route is NOT removed here — the client unregisters when the request
    /// (one reply) or stream (final STREAM_STOP) completes.
    pub fn deliver(&self, frame: OwnedFrame) {
        let sender = {
            let g = self.inner.lock().unwrap();
            g.routes.get(&frame.seq).cloned()
        };
        if let Some(tx) = sender {
            let _ = tx.send(frame);
        }
    }
```

- [ ] **Step 2: server — unregister after one-shot replies + stream relay**

In `crates/flip-daemon/src/server.rs`, replace the proxy section of `serve_client` (the `else`/proxy branch after the HELLO handling — the block that does `let dev_seq = router.register(...)` through the `recv_timeout` match) with a version that unregisters one-shot routes and enters a relay loop on `STREAM_START`:
```rust
            // Proxy: rewrite seq, forward to device, await the first reply.
            let client_seq = frame.seq;
            let dev_seq = router.register(reply_tx.clone());
            forward(&outbound, frame.typ, frame.flags, dev_seq, &frame.payload);

            match reply_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(reply) if reply.typ == MsgType::StreamStart => {
                    // Streaming: relay until the final STREAM_STOP.
                    write_frame(&mut stream, reply.typ, client_seq, &reply.payload)?;
                    relay_stream(&mut stream, &reply_rx, &router, &outbound, client_seq, dev_seq)?;
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
```

- [ ] **Step 3: server — add `forward` + `relay_stream` helpers**

Add these functions to `crates/flip-daemon/src/server.rs` (after `write_frame`):
```rust
/// Frame `payload` with a (rewritten) seq and queue it to the device.
fn forward(outbound: &Sender<Vec<u8>>, typ: MsgType, flags: u8, seq: u16, payload: &[u8]) {
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + payload.len() + 2];
    let n = flip_proto::encode(typ, flags, seq, payload, &mut buf).expect("reframe");
    let _ = outbound.send(buf[..n].to_vec());
}

/// Bidirectional relay for an active stream: device STREAM_DATA/STOP -> client,
/// client STREAM_STOP -> device. Ends when the device sends the final
/// STREAM_STOP. The client socket is already non-blocking (50ms read timeout).
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
```
Ensure `use std::sync::mpsc::Sender;` is present (it is) and that `Router`, `FrameReader`, `OwnedFrame`, `MsgType` are in scope (they are). Remove the now-unused inline `buf`/`encode` reframe in the old proxy block (replaced by `forward`).

- [ ] **Step 4: Build**

Run: `cargo build -p flip-daemon`
Expected: compiles clean. (No unit tests; exercised on hardware in Task 8. The existing one-shot path now unregisters explicitly — same external behavior.)

- [ ] **Step 5: Commit**

```bash
git add crates/flip-daemon/src/router.rs crates/flip-daemon/src/server.rs
git commit -m "feat(daemon): generic stream relay (STREAM_START -> bidirectional until STOP)"
```

---

## Task 6: CLI — stream decode helpers

**Files:**
- Modify: `crates/flip-cli/src/ir.rs`

- [ ] **Step 1: Add STREAM_DATA decode + timings formatting with tests**

Append to `crates/flip-cli/src/ir.rs`:
```rust
/// Decode a STREAM_DATA payload (little-endian i32 µs) into timings, appending
/// to `out`. Returns the number of samples decoded.
pub fn decode_stream_data(payload: &[u8], out: &mut Vec<u64>) -> usize {
    let mut count = 0;
    for chunk in payload.chunks_exact(4) {
        let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        out.push(v.max(0) as u64);
        count += 1;
    }
    count
}

/// Render captured timings as the whitespace file format `ir transmit` reads:
/// 12 per line.
pub fn format_timings(timings: &[u64]) -> String {
    let mut s = String::new();
    for (i, t) in timings.iter().enumerate() {
        if i > 0 {
            s.push(if i % 12 == 0 { '\n' } else { ' ' });
        }
        s.push_str(&t.to_string());
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn decodes_le_i32_samples() {
        let mut out = Vec::new();
        let payload = [0x10, 0x27, 0, 0, 0x2c, 0x01, 0, 0]; // 10000, 300
        assert_eq!(decode_stream_data(&payload, &mut out), 2);
        assert_eq!(out, vec![10000, 300]);
    }

    #[test]
    fn format_round_trips_through_parse() {
        let timings = vec![9000u64, 4500, 560, 560, 1690];
        let text = format_timings(&timings);
        assert_eq!(parse_timings(&text).unwrap(), timings);
    }
}
```

- [ ] **Step 2: Test**

Run: `cargo test -p flip-cli`
Expected: PASS incl. `decodes_le_i32_samples`, `format_round_trips_through_parse`.

- [ ] **Step 3: Commit**

```bash
git add crates/flip-cli/src/ir.rs
git commit -m "feat(cli): STREAM_DATA decode + timings file formatting"
```

---

## Task 7: CLI — `flip ir capture`

**Files:**
- Modify: `crates/flip-cli/Cargo.toml`
- Create: `crates/flip-cli/src/capture.rs`
- Modify: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Add the ctrlc dependency**

In `crates/flip-cli/Cargo.toml`, add to `[dependencies]`:
```toml
ctrlc = "3"
```

- [ ] **Step 2: Write the capture stream client**

`crates/flip-cli/src/capture.rs`:
```rust
//! `flip ir capture`: open a capture stream, read timings until Ctrl-C or a
//! silence gap, then write them in the `ir transmit --file` format.

use crate::client;
use crate::ir;
use anyhow::{anyhow, Result};
use flip_proto::{encode, MsgType};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run a capture. `auto_end_ms`: if `Some(ms)`, stop after that much silence;
/// otherwise run until Ctrl-C. `output`: file path, or `None` for stdout.
pub fn run(auto_end_ms: Option<u64>, output: Option<&str>) -> Result<()> {
    // Ctrl-C flag.
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
            Some((MsgType::StreamStop, _)) => break, // device ended (unexpected here)
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
```

- [ ] **Step 3: Add `open_stream` to the client**

Append to `crates/flip-cli/src/client.rs` a small streaming connection helper that owns a daemon socket for the life of a capture:
```rust
/// A persistent daemon connection for streaming (capture). Owns one socket.
pub struct StreamConn {
    transport: StreamTransport,
    reader: FrameReader,
}

impl StreamConn {
    /// Send a framed control message (seq is fixed at 1 for the single stream).
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
pub fn open_stream(instrument: &str, opcode: &str, params: flip_proto::Value) -> Result<StreamConn> {
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
    let n = encode(MsgType::Req, 0, 1, &body, &mut buf).ok_or_else(|| anyhow!("payload too big"))?;
    transport.write_all(&buf[..n])?;
    Ok(StreamConn {
        transport,
        reader: FrameReader::new(),
    })
}
```

- [ ] **Step 4: Add the `capture` subcommand**

In `crates/flip-cli/src/main.rs`, add `mod capture;` with the other module declarations, and add a `Capture` variant to the `IrCmd` enum:
```rust
    /// Capture IR timings to a file (or stdout). Stops on Ctrl-C, or after a
    /// silence gap with --auto-end.
    Capture {
        /// Write timings here (default: stdout).
        #[arg(long)]
        output: Option<String>,
        /// Auto-stop after this many ms of silence (default: run until Ctrl-C).
        #[arg(long)]
        auto_end: Option<u64>,
    },
```
And in the `Cmd::Ir { cmd } => match cmd { ... }` arm, add:
```rust
            IrCmd::Capture { output, auto_end } => {
                capture::run(auto_end, output.as_deref())
            }
```

- [ ] **Step 5: Build + test**

Run: `cargo build && cargo test`
Expected: builds; all tests pass; `./target/debug/flip ir capture --help` shows `--output` and `--auto-end`.

- [ ] **Step 6: Commit**

```bash
git add crates/flip-cli/Cargo.toml crates/flip-cli/src/capture.rs crates/flip-cli/src/client.rs crates/flip-cli/src/main.rs
git commit -m "feat(cli): flip ir capture (Ctrl-C / --auto-end -> timings file)"
```

---

## Task 8: [HW] Capture acceptance + the round-trip

**Files:**
- Modify: `README.md`

- [ ] **Step 1 [HW]: Capture a remote**

```bash
just reflash        # daemon-stop + build + upload
# launch flip-link on the device, then:
./target/debug/flip caps          # ir now lists transmit AND capture
./target/debug/flip ir capture --auto-end 400 --output /tmp/mybutton.txt
# point a remote at the Flipper's IR receiver and press a button
```
Expected: `capturing…` then `captured N timings -> /tmp/mybutton.txt` (N is dozens for a typical remote code). Inspect `/tmp/mybutton.txt` — whitespace-separated µs integers.

- [ ] **Step 2 [HW]: Replay it (close the loop)**

```bash
./target/debug/flip ir transmit --file /tmp/mybutton.txt
```
Expected: `transmitted N edges`. Point the Flipper's IR LED at the original device — it should respond as if the remote button was pressed. This is the capture→transmit round-trip.

- [ ] **Step 3 [HW]: Ctrl-C path**

```bash
./target/debug/flip ir capture --output /tmp/hold.txt
# press a few remote buttons, then Ctrl-C
```
Expected: capture stops cleanly on Ctrl-C and writes the file (no daemon/device hang; a follow-up `flip caps` still works).

- [ ] **Step 4: Document capture in the README**

Update the "IR transmit (Slice 1b)" section heading to "IR capture & transmit" and add:
```markdown
## IR capture & transmit

Capture a remote, then replay it:

```sh
flip ir capture --auto-end 400 --output remote.txt   # press a remote button
flip ir transmit --file remote.txt                   # replay at the target device
```

`capture` streams raw timings until Ctrl-C, or `--auto-end <ms>` of silence. The file is
the whitespace/newline µs format `transmit` consumes — so capture→transmit round-trips.
```

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: IR capture & transmit round-trip (Slice 1c)"
```

---

## Self-Review Notes (for the implementer)

- **Streaming is generic in the daemon:** it doesn't know about IR. A REQ whose first reply is `STREAM_START` becomes a bidirectional relay until the device's final `STREAM_STOP`. Any future streaming instrument reuses this path. `router.deliver` no longer auto-removes routes; `serve_client` unregisters on one-shot completion, stream end, or client disconnect.
- **Firmware capture is ISR-driven** (mirrors the proven C): the RX capture ISR pushes `i32` LE µs into a 4 KB stream buffer (drops counted); the main loop drains whole samples into `STREAM_DATA`; `STREAM_STOP` (client) or Back stops RX, flushes, sends final `STREAM_STOP{dropped}`. Send helpers are passed as closures so `ir_instrument` doesn't depend on `main`.
- **`ir.capture` is special-cased** in `handle_frame` (it starts a stream, not a RESP) and advertised via `streaming_opcodes` in CAPS; the dispatch table only holds request/response handlers (`transmit`).
- **Capture-end is CLI-side:** Ctrl-C (SIGINT flag) or `--auto-end <ms>` silence both result in the CLI sending `STREAM_STOP`; the daemon/firmware just relay it. The device's `RX_TIMEOUT_US` only bounds per-edge silence detection, not the capture session.
- **Type consistency:** `StreamStart{format}`, `StreamStop{dropped}`, `STREAM_FORMAT_RAW_I32_US`; firmware `ir_instrument::{start_capture,drain_capture,stop_capture,capture_active}` + `send_stream_*` closures; daemon `forward`/`relay_stream`/`unregister`; CLI `StreamConn`/`open_stream`, `ir::{decode_stream_data,format_timings}`, `capture::run`.
- **Carryover (defer):** the `send_msg` >1087 B silent-drop still applies to *control* frames; `STREAM_DATA` uses `send_stream_data` (heap, sized to payload) so it's not affected. One capture at a time (no concurrency guard needed — single main loop).
- **Known hardware risk to watch:** the RX capture ISR + `furi_hal_infrared_async_rx_*` lifecycle is new (like TX in 1b). Watch the first capture for a clean start/stop and that `flip caps` still works afterward (RX properly stopped). If the device faults, suspect the ISR or a missing `_rx_stop` on teardown.
