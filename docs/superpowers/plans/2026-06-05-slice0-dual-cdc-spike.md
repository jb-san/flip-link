# flip-link Slice 0 — Dual-CDC Walking Skeleton Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the full flip-link skeleton (proto → core → daemon → cli) and a Rust FAP, then prove a `PING`/`PONG` round-trip over the dual-CDC USB link end-to-end via `flip status` — the go/no-go gate on `flipperzero-rs`.

**Architecture:** A `no_std` `flip-proto` crate owns the frame wire format (shared by host and firmware via a path dependency). `flip-core` (std) opens the Flipper's interface-1 CDC serial port and reads/writes frames. `flip-daemon` owns that link and relays frames to clients over a unix socket. `flip-cli` is a thin client that auto-spawns the daemon. The firmware FAP switches USB to dual-CDC on launch and echoes `PONG` to any `PING`. No CBOR and no instruments in this slice — payloads are raw bytes.

**Tech Stack:** Rust (stable for host crates, `nightly-2025-08-31` for firmware), `flipperzero-rs` 0.16.0 + `flipperzero-sys`, `flipperzero-tools` (`run-fap`/`storage`), `serialport` 4.x, `clap` 4.x. Reference implementation: the proven C prototype at `../mcp/firmware` and `../mcp/protocol`.

**Scope:** Slice 0 only. IR (Slice 1) and I²C/SPI/UART (Slice 2) are separate plans. See `docs/superpowers/specs/2026-06-05-flip-link-architecture-design.md`.

**Hardware note:** Steps tagged **[HW]** require a Flipper Zero connected over USB and the FAP launched on-device (Back exits it). They are gated behind `FLIPPER_HW=1`. The operator runs these; everything else runs with no hardware.

---

## File Structure

```
flip-link/
  Cargo.toml                       # host workspace (excludes firmware/)
  .gitignore
  crates/
    flip-proto/
      Cargo.toml                   # no_std, no deps yet
      src/lib.rs                   # re-exports
      src/crc16.rs                 # CRC-16/CCITT-FALSE
      src/frame.rs                 # MsgType, Frame, encode/decode
    flip-core/
      Cargo.toml
      src/lib.rs
      src/transport.rs             # Transport trait + FrameReader
      src/mock.rs                  # in-memory loopback transport (tests)
      src/serial.rs                # serialport-backed transport + port discovery
      src/device.rs                # DeviceLink: connect + ping over a Transport
      tests/hw_ping.rs             # [HW] gated integration test
    flip-daemon/
      Cargo.toml
      src/main.rs                  # arg parsing (start/stop/status), wiring
      src/server.rs                # UnixListener accept loop + frame relay
    flip-cli/
      Cargo.toml
      src/main.rs                  # clap commands: status, daemon {start,stop,status}
      src/client.rs                # connect to socket, auto-spawn daemon
  firmware/                        # STANDALONE package (NOT a workspace member)
    .cargo/config.toml
    rust-toolchain.toml
    Cargo.toml                     # depends on ../crates/flip-proto by path
    src/main.rs                    # dual-CDC switch + PING/PONG echo + GUI/exit
    icons/                         # FAP icon (from template)
  docs/superpowers/...
```

---

## Task 1: Host workspace skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `.gitignore`
- Create: `crates/flip-proto/Cargo.toml`
- Create: `crates/flip-proto/src/lib.rs`

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/flip-proto", "crates/flip-core", "crates/flip-daemon", "crates/flip-cli"]
exclude = ["firmware"]

[workspace.package]
edition = "2021"
license = "MIT"

[workspace.dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
serialport = "4"
```

- [ ] **Step 2: Create `.gitignore`**

`.gitignore`:
```
/target
**/target
*.fap
.DS_Store
```

- [ ] **Step 3: Create the `flip-proto` crate manifest**

`crates/flip-proto/Cargo.toml`:
```toml
[package]
name = "flip-proto"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
# none yet — CRC and framing are hand-rolled; minicbor arrives in Slice 1.
```

- [ ] **Step 4: Create the `flip-proto` lib root**

`crates/flip-proto/src/lib.rs`:
```rust
//! flip-link wire contract. `no_std` so it compiles into the FAP and the host.
#![cfg_attr(not(test), no_std)]

pub mod crc16;
pub mod frame;

pub use crc16::crc16_ccitt_false;
pub use frame::{Frame, MsgType, DecodeResult, FRAME_MAGIC, HEADER_SIZE, MAX_PAYLOAD};
```

- [ ] **Step 5: Defer the build**

The `crc16` and `frame` modules referenced by `lib.rs` are created in Tasks 3–4, so the crate does not compile yet. Do not build here — the first green `cargo build -p flip-proto` lands at the end of Task 4. Commit the skeleton as-is.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml .gitignore crates/flip-proto
git commit -m "chore: host workspace + flip-proto skeleton"
```

---

## Task 2: flipperzero-rs toolchain bring-up — **feasibility checkpoint #1**

This proves the Rust→Flipper toolchain works *before* we write any of our firmware. We scaffold the stock "Hello, Rust!" app and run it on-device.

**Files:**
- Create: `firmware/.cargo/config.toml`
- Create: `firmware/rust-toolchain.toml`
- Create: `firmware/Cargo.toml`
- Create: `firmware/src/main.rs`
- Create: `firmware/icons/` (icon asset)

- [ ] **Step 1: Install the firmware toolchain prerequisites**

```bash
rustup toolchain install nightly-2025-08-31
rustup target add --toolchain nightly-2025-08-31 thumbv7em-none-eabihf
cargo install --locked flipperzero-tools
```
Expected: `run-fap` and `storage` binaries on `PATH` (`which run-fap`).

- [ ] **Step 2: Create the firmware toolchain pin**

`firmware/rust-toolchain.toml`:
```toml
[toolchain]
channel = "nightly-2025-08-31"
targets = ["thumbv7em-none-eabihf"]
```

- [ ] **Step 3: Create the cargo config (directory-scoped; only applies inside `firmware/`)**

`firmware/.cargo/config.toml`:
```toml
[target.thumbv7em-none-eabihf]
rustflags = [
    "-C", "target-cpu=cortex-m4",
    "-C", "panic=abort",
    "-C", "debuginfo=0",
    "-C", "opt-level=z",
    "-C", "embed-bitcode=yes",
    "-C", "lto=yes",
    "-C", "link-args=--script=flipperzero-rt.ld --Bstatic --relocatable --discard-all --strip-all --lto-O3 --lto-whole-program-visibility",
]

[build]
target = "thumbv7em-none-eabihf"
```

- [ ] **Step 4: Create the firmware manifest (standalone, depends on flip-proto by path)**

`firmware/Cargo.toml`:
```toml
cargo-features = ["different-binary-name"]

[package]
name = "flip-link-fw"
version = "0.1.0"
edition = "2024"
rust-version = "1.85.0"
autobins = false
autoexamples = false
autotests = false
autobenches = false

[[bin]]
name = "flip-link-fw"
filename = "flip_link.fap"
bench = false
test = false

[dependencies]
flipperzero = "0.16.0"
flipperzero-sys = "0.16.0"
flipperzero-rt = "0.16.0"
flipperzero-alloc = "0.16.0"
flip-proto = { path = "../crates/flip-proto" }
```

- [ ] **Step 5: Copy the template icon**

Copy an icon asset so `has_icon = true` works (the template ships `rustacean-10x10.icon`):
```bash
mkdir -p firmware/icons
curl -fsSL https://raw.githubusercontent.com/flipperzero-rs/flipperzero-template/main/rustacean-10x10.icon -o firmware/icons/rustacean-10x10.icon
```
If that path 404s, generate any 10x10 `.icon` per the flipperzero icon docs, or set `has_icon = false` in the manifest and drop the `icon =` line in Step 6.

- [ ] **Step 6: Create the stock hello app**

`firmware/src/main.rs`:
```rust
#![no_main]
#![no_std]

extern crate flipperzero_rt;
extern crate flipperzero_alloc;

use core::ffi::CStr;
use flipperzero::println;
use flipperzero_rt::{entry, manifest};

manifest!(
    name = "flip-link",
    app_version = 1,
    has_icon = true,
    icon = "icons/rustacean-10x10.icon",
);

entry!(main);

fn main(_args: Option<&CStr>) -> i32 {
    println!("flip-link toolchain OK");
    0
}
```

- [ ] **Step 7: Build the .fap**

Run (from inside `firmware/`):
```bash
cd firmware && cargo build --release
```
Expected: builds to `firmware/target/thumbv7em-none-eabihf/release/flip_link.fap`. If the linker errors on `flipperzero-rt.ld`, confirm `flipperzero-rt` 0.16 is the resolved dep (it ships the linker script) and that you are on the pinned nightly (`rustup show` inside `firmware/`).

- [ ] **Step 8 [HW]: Run on device**

With a Flipper connected:
```bash
cd firmware && run-fap target/thumbv7em-none-eabihf/release/flip_link.fap
```
Expected: the app launches on the Flipper and the host console shows `flip-link toolchain OK`. **This is checkpoint #1 — the Rust toolchain reaches the device.** If this fails, stop and resolve tooling before continuing.

- [ ] **Step 9: Commit**

```bash
git add firmware
git commit -m "feat(fw): flipperzero-rs toolchain bring-up (hello app)"
```

---

## Task 3: flip-proto — CRC-16/CCITT-FALSE (TDD)

**Files:**
- Modify: `crates/flip-proto/src/crc16.rs`

- [ ] **Step 1: Write the failing test**

`crates/flip-proto/src/crc16.rs`:
```rust
//! CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflection, xorout 0x0000.

/// Compute CRC-16/CCITT-FALSE over `data`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        // The standard CRC-16/CCITT-FALSE check value for "123456789" is 0x29B1.
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    }

    #[test]
    fn empty_is_init() {
        assert_eq!(crc16_ccitt_false(b""), 0xFFFF);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p flip-proto crc16`
Expected: PASS (`check_value`, `empty_is_init`). If `check_value` is not `0x29B1`, the polynomial loop is wrong — do not adjust the expected value, fix the implementation.

- [ ] **Step 3: Commit**

```bash
git add crates/flip-proto/src/crc16.rs
git commit -m "feat(proto): CRC-16/CCITT-FALSE with check vector"
```

---

## Task 4: flip-proto — frame encode/decode (TDD)

Implements the locked frame from the spec §3.1. Decode returns `NeedMore` (incomplete), `Frame` (one complete frame + bytes consumed), or `Resync` (bad magic/CRC at offset 0; caller drops 1 byte).

**Files:**
- Modify: `crates/flip-proto/src/frame.rs`

- [ ] **Step 1: Write the implementation + failing tests**

`crates/flip-proto/src/frame.rs`:
```rust
//! Frame wire format (little-endian), identical to the proven C prototype.
//!
//! magic u16 0xF1A6 | version u8 | type u8 | flags u8 | seq u16 | length u32
//! | payload[length] | crc16 u16 over bytes [version .. end of payload].

use crate::crc16::crc16_ccitt_false;

pub const FRAME_MAGIC: u16 = 0xF1A6;
pub const FRAME_VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 11; // magic..length inclusive
pub const CRC_SIZE: usize = 2;
pub const MAX_PAYLOAD: u32 = 0xFFFF;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum MsgType {
    Hello = 1,
    Caps = 2,
    Req = 3,
    Resp = 4,
    StreamStart = 5,
    StreamData = 6,
    StreamStop = 7,
    Event = 8,
    Error = 9,
    Ping = 10,
    Pong = 11,
}

impl MsgType {
    pub fn from_u8(v: u8) -> Option<MsgType> {
        Some(match v {
            1 => MsgType::Hello,
            2 => MsgType::Caps,
            3 => MsgType::Req,
            4 => MsgType::Resp,
            5 => MsgType::StreamStart,
            6 => MsgType::StreamData,
            7 => MsgType::StreamStop,
            8 => MsgType::Event,
            9 => MsgType::Error,
            10 => MsgType::Ping,
            11 => MsgType::Pong,
            _ => return None,
        })
    }
}

/// A decoded frame. `payload` borrows from the input buffer.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub typ: MsgType,
    pub flags: u8,
    pub seq: u16,
    pub payload: &'a [u8],
}

#[derive(Debug)]
pub enum DecodeResult<'a> {
    /// A complete, CRC-valid frame and the number of bytes it consumed.
    Frame(Frame<'a>, usize),
    /// Not enough bytes yet.
    NeedMore,
    /// Bad magic or CRC at offset 0; caller should drop 1 byte and retry.
    Resync,
}

/// Encode a frame into `out`. Returns bytes written, or `None` if `out` is too small
/// or the payload exceeds `MAX_PAYLOAD`.
pub fn encode(
    typ: MsgType,
    flags: u8,
    seq: u16,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    if payload.len() as u32 > MAX_PAYLOAD {
        return None;
    }
    let total = HEADER_SIZE + payload.len() + CRC_SIZE;
    if out.len() < total {
        return None;
    }
    out[0..2].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
    out[2] = FRAME_VERSION;
    out[3] = typ as u8;
    out[4] = flags;
    out[5..7].copy_from_slice(&seq.to_le_bytes());
    out[7..11].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    out[HEADER_SIZE..HEADER_SIZE + payload.len()].copy_from_slice(payload);
    // CRC covers bytes [2 .. end of payload] (everything except magic).
    let crc = crc16_ccitt_false(&out[2..HEADER_SIZE + payload.len()]);
    let crc_at = HEADER_SIZE + payload.len();
    out[crc_at..crc_at + 2].copy_from_slice(&crc.to_le_bytes());
    Some(total)
}

/// Try to decode one frame from the front of `buf`.
pub fn decode(buf: &[u8]) -> DecodeResult<'_> {
    if buf.len() < 2 {
        return DecodeResult::NeedMore;
    }
    let magic = u16::from_le_bytes([buf[0], buf[1]]);
    if magic != FRAME_MAGIC {
        return DecodeResult::Resync;
    }
    if buf.len() < HEADER_SIZE {
        return DecodeResult::NeedMore;
    }
    let length = u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]);
    if length > MAX_PAYLOAD {
        return DecodeResult::Resync;
    }
    let length = length as usize;
    let total = HEADER_SIZE + length + CRC_SIZE;
    if buf.len() < total {
        return DecodeResult::NeedMore;
    }
    let crc_at = HEADER_SIZE + length;
    let want = u16::from_le_bytes([buf[crc_at], buf[crc_at + 1]]);
    let got = crc16_ccitt_false(&buf[2..crc_at]);
    if want != got {
        return DecodeResult::Resync;
    }
    let typ = match MsgType::from_u8(buf[3]) {
        Some(t) => t,
        None => return DecodeResult::Resync,
    };
    let frame = Frame {
        typ,
        flags: buf[4],
        seq: u16::from_le_bytes([buf[5], buf[6]]),
        payload: &buf[HEADER_SIZE..crc_at],
    };
    DecodeResult::Frame(frame, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_ping() {
        let mut buf = [0u8; 64];
        let n = encode(MsgType::Ping, 0, 0x1234, b"hi", &mut buf).unwrap();
        match decode(&buf[..n]) {
            DecodeResult::Frame(f, used) => {
                assert_eq!(used, n);
                assert_eq!(f.typ, MsgType::Ping);
                assert_eq!(f.seq, 0x1234);
                assert_eq!(f.payload, b"hi");
            }
            other => panic!("expected Frame, got {:?}", other),
        }
    }

    #[test]
    fn need_more_when_truncated() {
        let mut buf = [0u8; 64];
        let n = encode(MsgType::Pong, 0, 1, b"abc", &mut buf).unwrap();
        assert!(matches!(decode(&buf[..n - 1]), DecodeResult::NeedMore));
    }

    #[test]
    fn resync_on_bad_magic() {
        assert!(matches!(decode(&[0x00, 0x00, 0x00]), DecodeResult::Resync));
    }

    #[test]
    fn resync_on_corrupt_crc() {
        let mut buf = [0u8; 64];
        let n = encode(MsgType::Ping, 0, 1, b"xy", &mut buf).unwrap();
        buf[n - 1] ^= 0xFF; // corrupt last CRC byte
        assert!(matches!(decode(&buf[..n]), DecodeResult::Resync));
    }
}
```

- [ ] **Step 2: Wire up the re-exports in lib.rs**

Update `crates/flip-proto/src/lib.rs` to export `encode`/`decode`:
```rust
//! flip-link wire contract. `no_std` so it compiles into the FAP and the host.
#![cfg_attr(not(test), no_std)]

pub mod crc16;
pub mod frame;

pub use crc16::crc16_ccitt_false;
pub use frame::{decode, encode, DecodeResult, Frame, MsgType, FRAME_MAGIC, HEADER_SIZE, MAX_PAYLOAD};
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p flip-proto`
Expected: PASS (all crc16 + frame tests). Confirms encode↔decode symmetry and resync behavior.

- [ ] **Step 4: Commit**

```bash
git add crates/flip-proto/src/frame.rs crates/flip-proto/src/lib.rs
git commit -m "feat(proto): frame encode/decode with resync semantics"
```

---

## Task 5: flip-core — Transport trait + FrameReader + mock loopback (TDD, no hardware)

**Files:**
- Create: `crates/flip-core/Cargo.toml`
- Create: `crates/flip-core/src/lib.rs`
- Create: `crates/flip-core/src/transport.rs`
- Create: `crates/flip-core/src/mock.rs`

- [ ] **Step 1: Create the crate manifest**

`crates/flip-core/Cargo.toml`:
```toml
[package]
name = "flip-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
flip-proto = { path = "../flip-proto" }
serialport = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 2: Create the lib root**

`crates/flip-core/src/lib.rs`:
```rust
pub mod transport;
pub mod mock;
pub mod serial;
pub mod device;

pub use transport::{FrameReader, Transport};
pub use device::DeviceLink;
```

- [ ] **Step 3: Write the transport module with a failing test**

`crates/flip-core/src/transport.rs`:
```rust
use anyhow::Result;
use flip_proto::{decode, DecodeResult, MsgType};

/// A byte-stream transport (serial port, socket, or in-memory mock).
pub trait Transport {
    /// Write all bytes.
    fn write_all(&mut self, buf: &[u8]) -> Result<()>;
    /// Read some bytes into `buf`, returning the count (0 = would-block/timeout).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
}

/// An owned, decoded frame (payload copied out so it doesn't borrow the buffer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedFrame {
    pub typ: MsgType,
    pub flags: u8,
    pub seq: u16,
    pub payload: Vec<u8>,
}

/// Accumulates bytes from a transport and yields whole frames, resyncing on garbage.
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        FrameReader { buf: Vec::new() }
    }

    /// Feed raw bytes that arrived from the transport.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete frame, if one is buffered. Drops leading garbage.
    pub fn next_frame(&mut self) -> Option<OwnedFrame> {
        loop {
            match decode(&self.buf) {
                DecodeResult::Frame(f, used) => {
                    let owned = OwnedFrame {
                        typ: f.typ,
                        flags: f.flags,
                        seq: f.seq,
                        payload: f.payload.to_vec(),
                    };
                    self.buf.drain(0..used);
                    return Some(owned);
                }
                DecodeResult::NeedMore => return None,
                DecodeResult::Resync => {
                    if self.buf.is_empty() {
                        return None;
                    }
                    self.buf.drain(0..1);
                }
            }
        }
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flip_proto::encode;

    #[test]
    fn reader_yields_frame_across_chunks() {
        let mut out = [0u8; 64];
        let n = encode(MsgType::Ping, 0, 7, b"hi", &mut out).unwrap();
        let mut r = FrameReader::new();
        r.feed(&out[..3]); // partial header
        assert!(r.next_frame().is_none());
        r.feed(&out[3..n]); // rest
        let f = r.next_frame().unwrap();
        assert_eq!(f.typ, MsgType::Ping);
        assert_eq!(f.seq, 7);
        assert_eq!(f.payload, b"hi");
    }

    #[test]
    fn reader_resyncs_past_garbage() {
        let mut out = [0u8; 64];
        let n = encode(MsgType::Pong, 0, 1, b"x", &mut out).unwrap();
        let mut r = FrameReader::new();
        r.feed(&[0xDE, 0xAD, 0xBE]); // junk
        r.feed(&out[..n]);
        let f = r.next_frame().unwrap();
        assert_eq!(f.typ, MsgType::Pong);
    }
}
```

- [ ] **Step 4: Write the mock loopback transport**

`crates/flip-core/src/mock.rs`:
```rust
use crate::transport::{FrameReader, Transport};
use anyhow::Result;
use flip_proto::{encode, MsgType};
use std::collections::VecDeque;

/// In-memory transport: every PING written is answered with a PONG carrying the
/// same seq and payload, queued for the next read. Mimics the firmware echo.
pub struct PongLoopback {
    out: VecDeque<u8>,
    reader: FrameReader,
}

impl PongLoopback {
    pub fn new() -> Self {
        PongLoopback { out: VecDeque::new(), reader: FrameReader::new() }
    }
}

impl Default for PongLoopback {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for PongLoopback {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.reader.feed(buf);
        while let Some(f) = self.reader.next_frame() {
            if f.typ == MsgType::Ping {
                let mut enc = [0u8; 1100];
                let n = encode(MsgType::Pong, 0, f.seq, &f.payload, &mut enc)
                    .expect("pong encodes");
                self.out.extend(&enc[..n]);
            }
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match self.out.pop_front() {
                Some(b) => {
                    buf[n] = b;
                    n += 1;
                }
                None => break,
            }
        }
        Ok(n)
    }
}
```

- [ ] **Step 5: Run the transport tests**

Run: `cargo test -p flip-core transport`
Expected: PASS (`reader_yields_frame_across_chunks`, `reader_resyncs_past_garbage`).

- [ ] **Step 6: Commit**

```bash
git add crates/flip-core/Cargo.toml crates/flip-core/src/lib.rs crates/flip-core/src/transport.rs crates/flip-core/src/mock.rs
git commit -m "feat(core): Transport trait, FrameReader, pong loopback mock"
```

---

## Task 6: flip-core — DeviceLink::ping over a Transport (TDD)

**Files:**
- Create: `crates/flip-core/src/device.rs`

- [ ] **Step 1: Write DeviceLink with a failing test (uses the mock)**

`crates/flip-core/src/device.rs`:
```rust
use crate::transport::{FrameReader, Transport};
use anyhow::{anyhow, Result};
use flip_proto::{encode, MsgType};
use std::time::{Duration, Instant};

/// A framed link to the device over any Transport. Owns sequence numbering.
pub struct DeviceLink<T: Transport> {
    transport: T,
    reader: FrameReader,
    next_seq: u16,
}

impl<T: Transport> DeviceLink<T> {
    pub fn new(transport: T) -> Self {
        DeviceLink { transport, reader: FrameReader::new(), next_seq: 1 }
    }

    fn alloc_seq(&mut self) -> u16 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        s
    }

    /// Send a PING with `payload` and wait up to `timeout` for the matching PONG.
    /// Returns the echoed payload on success.
    pub fn ping(&mut self, payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        let seq = self.alloc_seq();
        let mut enc = [0u8; 1100];
        let n = encode(MsgType::Ping, 0, seq, payload, &mut enc)
            .ok_or_else(|| anyhow!("ping payload too large"))?;
        self.transport.write_all(&enc[..n])?;

        let deadline = Instant::now() + timeout;
        let mut scratch = [0u8; 512];
        loop {
            if let Some(f) = self.reader.next_frame() {
                if f.typ == MsgType::Pong && f.seq == seq {
                    return Ok(f.payload);
                }
                continue; // ignore unrelated frames
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("ping timed out waiting for pong seq {seq}"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::PongLoopback;

    #[test]
    fn ping_round_trips_via_mock() {
        let mut link = DeviceLink::new(PongLoopback::new());
        let echoed = link.ping(b"hello", Duration::from_millis(500)).unwrap();
        assert_eq!(echoed, b"hello");
    }

    #[test]
    fn ping_seq_increments() {
        let mut link = DeviceLink::new(PongLoopback::new());
        link.ping(b"a", Duration::from_millis(500)).unwrap();
        link.ping(b"b", Duration::from_millis(500)).unwrap();
        // second ping used seq 2; if correlation were broken the mock's seq-echo
        // would mismatch and time out, so reaching here proves correlation works.
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p flip-core device`
Expected: PASS (`ping_round_trips_via_mock`, `ping_seq_increments`).

- [ ] **Step 3: Commit**

```bash
git add crates/flip-core/src/device.rs
git commit -m "feat(core): DeviceLink::ping with seq correlation (mock-tested)"
```

---

## Task 7: flip-core — serial transport + Flipper port discovery

The host must find the Flipper's **second** CDC port (interface 1) that appears after the dual-CDC switch. On macOS these enumerate as `/dev/cu.usbmodemflip_*`; the app's interface-1 port is the higher-numbered of the two `flip_` ports.

**Files:**
- Create: `crates/flip-core/src/serial.rs`

- [ ] **Step 1: Implement port discovery + serial transport**

`crates/flip-core/src/serial.rs`:
```rust
use crate::transport::Transport;
use anyhow::{anyhow, Result};
use serialport::SerialPort;
use std::io::{ErrorKind, Read, Write};
use std::time::Duration;

/// List candidate Flipper CDC ports (name contains "flip", or USB VID 0x0483).
pub fn list_flipper_ports() -> Result<Vec<String>> {
    let mut names: Vec<String> = serialport::available_ports()?
        .into_iter()
        .filter(|p| {
            let is_flip_name = p.port_name.to_lowercase().contains("flip");
            let is_flip_vid = matches!(
                &p.port_type,
                serialport::SerialPortType::UsbPort(info) if info.vid == 0x0483
            );
            is_flip_name || is_flip_vid
        })
        .map(|p| p.port_name)
        .collect();
    names.sort();
    Ok(names)
}

/// Pick the agent (interface-1) port. After the dual-CDC switch two flipper
/// ports exist; interface 1 is the higher-numbered one. Override with FLIP_PORT.
pub fn pick_agent_port() -> Result<String> {
    if let Ok(p) = std::env::var("FLIP_PORT") {
        return Ok(p);
    }
    let ports = list_flipper_ports()?;
    match ports.len() {
        0 => Err(anyhow!("no Flipper serial ports found (is it connected and the FAP running?)")),
        1 => Ok(ports.into_iter().next().unwrap()),
        _ => Ok(ports.into_iter().last().unwrap()), // sorted; highest = interface 1
    }
}

pub struct SerialTransport {
    port: Box<dyn SerialPort>,
}

impl SerialTransport {
    pub fn open(path: &str) -> Result<Self> {
        let port = serialport::new(path, 115_200)
            .timeout(Duration::from_millis(50))
            .open()?;
        Ok(SerialTransport { port })
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.port.write_all(buf)?;
        self.port.flush()?;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
}
```

- [ ] **Step 2: Build (no unit test — this layer is hardware-bound; covered by Task 9)**

Run: `cargo build -p flip-core`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/flip-core/src/serial.rs
git commit -m "feat(core): serial transport + Flipper port discovery"
```

---

## Task 8: Firmware — dual-CDC switch + PING/PONG echo (**the spike**)

Translates the proven C prototype (`../mcp/firmware/flipper_agent_app.c`) to Rust via `flipperzero-sys`. The app: switches USB to dual-CDC, owns interface 1, reads bytes into a `FrameReader`, echoes `PONG` for each `PING`, shows a minimal GUI, and exits + restores USB config on Back.

> The exact `flipperzero-sys` symbol paths must be confirmed against the installed 0.16 bindings (Step 2). The C names from the prototype are: `furi_hal_usb_set_config`, `usb_cdc_dual`, `usb_cdc_single` (saved/restored), `furi_hal_cdc_send(if_num, buf, len)`, `furi_hal_cdc_receive(if_num, buf, max)`, `furi_hal_cdc_set_callbacks`. In `flipperzero-sys` these are typically reachable as `flipperzero_sys::furi_hal_*` free functions and `flipperzero_sys::{UsbInterface, ...}` items.

**Files:**
- Modify: `firmware/src/main.rs`

- [ ] **Step 1: Confirm the USB/CDC symbols exist in the installed bindings**

Run:
```bash
cd firmware && cargo doc -p flipperzero-sys --no-deps 2>/dev/null; \
grep -RinE "furi_hal_cdc_(send|receive)|furi_hal_usb_set_config|usb_cdc_dual" \
  ~/.cargo/registry/src/*/flipperzero-sys-0.16.0/ | head -40
```
Expected: matches that name the exact exported symbols/types. **Record the exact paths** (e.g. `furi_hal_cdc_send`, the dual-CDC interface global) — the code below uses the prototype's C names; adjust to the bindings' actual paths.

- [ ] **Step 2: Implement the dual-CDC echo app**

`firmware/src/main.rs`:
```rust
#![no_main]
#![no_std]

extern crate flipperzero_rt;
extern crate flipperzero_alloc;

use core::ffi::CStr;
use core::time::Duration;

use flipperzero::furi::message_queue::MessageQueue;
use flipperzero_rt::{entry, manifest};
use flipperzero_sys as sys;
use flip_proto::{decode, encode, DecodeResult, MsgType};

manifest!(
    name = "flip-link",
    app_version = 1,
    has_icon = true,
    icon = "icons/rustacean-10x10.icon",
);

entry!(main);

const AGENT_IF: u8 = 1; // interface 1 = our binary channel; 0 stays the CLI
const RX_CHUNK: usize = 256;

/// Send all bytes on the agent CDC interface in endpoint-sized chunks.
fn cdc_send_all(bytes: &[u8]) {
    // CDC_DATA_SZ on the Flipper is 64; chunk to be safe.
    let mut off = 0;
    while off < bytes.len() {
        let end = core::cmp::min(off + 64, bytes.len());
        unsafe {
            sys::furi_hal_cdc_send(AGENT_IF, bytes[off..].as_ptr(), (end - off) as u16);
        }
        off = end;
    }
}

fn main(_args: Option<&CStr>) -> i32 {
    // 1. Switch USB to dual-CDC so interface 1 is ours; save current to restore.
    //    (Adjust the type/global name to the actual flipperzero-sys binding.)
    let prev = unsafe { sys::furi_hal_usb_get_config() };
    unsafe {
        sys::furi_hal_usb_set_config(
            &raw const sys::usb_cdc_dual as *mut _,
            core::ptr::null_mut(),
        );
    }

    // 2. Minimal viewport so the user sees the app is running and can press Back.
    //    For the spike a simple loop polling the input queue is enough.
    let input_queue: MessageQueue<sys::InputEvent> = MessageQueue::new(8);

    // 3. RX loop: read CDC bytes on interface 1, decode frames, echo PONG.
    let mut rx = [0u8; RX_CHUNK];
    let mut acc: heapless_or_vec = Vec::new(); // see note below
    let mut running = true;
    while running {
        // Poll input for Back.
        if let Ok(ev) = input_queue.get(Duration::from_millis(0)) {
            if ev.key == sys::InputKey_InputKeyBack && ev.type_ == sys::InputType_InputTypeShort {
                running = false;
            }
        }

        let got = unsafe { sys::furi_hal_cdc_receive(AGENT_IF, rx.as_mut_ptr(), RX_CHUNK as u16) };
        if got > 0 {
            acc.extend_from_slice(&rx[..got as usize]);
            loop {
                match decode(&acc) {
                    DecodeResult::Frame(f, used) => {
                        if f.typ == MsgType::Ping {
                            let mut enc = [0u8; 1100];
                            if let Some(n) = encode(MsgType::Pong, 0, f.seq, f.payload, &mut enc) {
                                cdc_send_all(&enc[..n]);
                            }
                        }
                        acc.drain(0..used);
                    }
                    DecodeResult::NeedMore => break,
                    DecodeResult::Resync => {
                        if acc.is_empty() { break; }
                        acc.drain(0..1);
                    }
                }
            }
        } else {
            unsafe { sys::furi_delay_ms(2) };
        }
    }

    // 4. Restore USB config on exit.
    unsafe { sys::furi_hal_usb_set_config(prev, core::ptr::null_mut()); }
    0
}
```

> **Implementation notes for this step (resolve while coding, not placeholders):**
> - `acc` needs a growable byte buffer. With `flipperzero-alloc` linked, use `alloc::vec::Vec<u8>` (add `extern crate alloc;` and `use alloc::vec::Vec;`). Replace the `heapless_or_vec` pseudo-type accordingly.
> - The exact `sys` paths (`furi_hal_usb_get_config`/`set_config`, the `usb_cdc_dual` global, `furi_hal_cdc_send/receive`, `InputEvent`/`InputKey_*`) come from Step 1's grep — wire them to the real names. The C prototype in `../mcp/firmware/flipper_agent_app.c` is the authoritative behavior reference (flow control via tx semaphore, chunked send, dual-CDC switch).
> - For the spike, the GUI can be omitted entirely if reading the input queue requires extra setup — the operator can exit by closing the app from the device menu. Keep it minimal; the goal is the PING/PONG round-trip.

- [ ] **Step 3: Build the .fap**

Run: `cd firmware && cargo build --release`
Expected: builds `flip_link.fap`. Resolve any sys-symbol or borrow-checker errors here (this is the spike's real work).

- [ ] **Step 4: Commit**

```bash
git add firmware/src/main.rs
git commit -m "feat(fw): dual-CDC switch + PING/PONG echo on interface 1"
```

---

## Task 9: Hardware integration test — flip-core ↔ firmware PING **[HW]** — **GO/NO-GO**

This is the explicit feasibility gate. The operator launches the FAP; the test opens interface 1 and round-trips a PING.

**Files:**
- Create: `crates/flip-core/tests/hw_ping.rs`

- [ ] **Step 1: Write the gated integration test**

`crates/flip-core/tests/hw_ping.rs`:
```rust
//! Hardware round-trip: requires a Flipper with the flip-link FAP running.
//! Run with: FLIPPER_HW=1 cargo test -p flip-core --test hw_ping -- --nocapture

use flip_core::device::DeviceLink;
use flip_core::serial::{pick_agent_port, SerialTransport};
use std::time::Duration;

#[test]
fn ping_pong_over_usb() {
    if std::env::var("FLIPPER_HW").ok().as_deref() != Some("1") {
        eprintln!("skipping: set FLIPPER_HW=1 with the FAP running on-device");
        return;
    }
    let port = pick_agent_port().expect("agent port");
    eprintln!("using agent port: {port}");
    let transport = SerialTransport::open(&port).expect("open port");
    let mut link = DeviceLink::new(transport);

    let payload = b"flip-link-spike";
    let echoed = link
        .ping(payload, Duration::from_secs(2))
        .expect("pong round-trip");
    assert_eq!(&echoed, payload);
}
```
Make `device` and `serial` reachable from the integration test by ensuring they are `pub` in `lib.rs` (they are, via `pub mod`).

- [ ] **Step 2: Build the test (no hardware)**

Run: `cargo test -p flip-core --test hw_ping`
Expected: compiles; prints the skip message and passes (because `FLIPPER_HW` is unset).

- [ ] **Step 3 [HW]: Run against hardware**

Operator: connect the Flipper, then `cd firmware && run-fap target/thumbv7em-none-eabihf/release/flip_link.fap` to launch the FAP (it switches to dual-CDC and re-enumerates — expect a brief port drop). Then:
```bash
FLIPPER_HW=1 cargo test -p flip-core --test hw_ping -- --nocapture
```
Expected: PASS — `pong round-trip` succeeds, echoed payload equals sent. **This is the GO/NO-GO. If PONG round-trips, `flipperzero-rs` is validated and we proceed to the daemon/CLI. If not, debug the firmware (Task 8) or port selection (Task 7) before building further.**

- [ ] **Step 4: Commit**

```bash
git add crates/flip-core/tests/hw_ping.rs
git commit -m "test(core): [HW] dual-CDC PING/PONG go/no-go integration test"
```

---

## Task 10: flip-daemon — device link owner + unix socket relay

The daemon opens the device once and relays frames between a single client connection and the device. (Multi-client fan-out and session ownership arrive in Slice 1; Slice 0 needs one client doing ping.)

**Files:**
- Create: `crates/flip-daemon/Cargo.toml`
- Create: `crates/flip-daemon/src/server.rs`
- Create: `crates/flip-daemon/src/main.rs`

- [ ] **Step 1: Create the manifest**

`crates/flip-daemon/Cargo.toml`:
```toml
[package]
name = "flip-daemon"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
flip-core = { path = "../flip-core" }
flip-proto = { path = "../flip-proto" }
anyhow = { workspace = true }
clap = { workspace = true }
```

- [ ] **Step 2: Implement the socket path helper + relay server**

`crates/flip-daemon/src/server.rs`:
```rust
use anyhow::{Context, Result};
use flip_core::serial::{pick_agent_port, SerialTransport};
use flip_core::transport::{FrameReader, Transport};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

/// Daemon socket path: $XDG_RUNTIME_DIR/flip-link.sock, else /tmp/flip-link.sock.
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("flip-link.sock");
    }
    PathBuf::from("/tmp/flip-link.sock")
}

/// Run the daemon: open the device, accept clients, relay frames both ways.
/// Slice 0: one client at a time, byte-for-byte relay (client owns the protocol).
pub fn run() -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // clear stale socket
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind {}", path.display()))?;
    eprintln!("flip-daemon listening on {}", path.display());

    let port = pick_agent_port().context("find Flipper agent port")?;
    let mut device = SerialTransport::open(&port).context("open device")?;
    eprintln!("flip-daemon connected to device on {port}");

    for stream in listener.incoming() {
        let mut client = stream?;
        if let Err(e) = relay_one(&mut client, &mut device) {
            eprintln!("client session ended: {e:#}");
        }
    }
    Ok(())
}

/// Relay: read frames from the client, forward raw bytes to the device, read
/// device frames back, forward to the client. Ends when the client disconnects.
fn relay_one(client: &mut UnixStream, device: &mut SerialTransport) -> Result<()> {
    client.set_read_timeout(Some(Duration::from_millis(20)))?;
    let mut from_client = [0u8; 1024];
    let mut from_device = [0u8; 1024];
    let mut dev_reader = FrameReader::new();

    loop {
        // Client -> device (forward raw bytes; client already framed them).
        match client.read(&mut from_client) {
            Ok(0) => return Ok(()), // client closed
            Ok(n) => device.write_all(&from_client[..n])?,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }

        // Device -> client (reframe to forward only whole frames).
        let got = device.read(&mut from_device)?;
        if got > 0 {
            dev_reader.feed(&from_device[..got]);
            while let Some(f) = dev_reader.next_frame() {
                let mut enc = [0u8; 1100];
                let n = flip_proto::encode(f.typ, f.flags, f.seq, &f.payload, &mut enc)
                    .expect("reframe");
                client.write_all(&enc[..n])?;
            }
        }
    }
}
```

- [ ] **Step 3: Implement the daemon CLI entry**

`crates/flip-daemon/src/main.rs`:
```rust
mod server;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "flip-daemon", about = "flip-link device-link daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (default).
    Run,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => server::run(),
    }
}
```

- [ ] **Step 4: Build**

Run: `cargo build -p flip-daemon`
Expected: compiles clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-daemon
git commit -m "feat(daemon): device-link owner + unix socket frame relay"
```

---

## Task 11: flip-cli — `flip status` with daemon auto-spawn

**Files:**
- Create: `crates/flip-cli/Cargo.toml`
- Create: `crates/flip-cli/src/client.rs`
- Create: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Create the manifest**

`crates/flip-cli/Cargo.toml`:
```toml
[package]
name = "flip-cli"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[[bin]]
name = "flip"
path = "src/main.rs"

[dependencies]
flip-core = { path = "../flip-core" }
flip-proto = { path = "../flip-proto" }
anyhow = { workspace = true }
clap = { workspace = true }
```

- [ ] **Step 2: Implement the client (connect + auto-spawn + ping)**

`crates/flip-cli/src/client.rs`:
```rust
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
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
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
    let program = if candidate.exists() { candidate } else { PathBuf::from("flip-daemon") };
    Command::new(program)
        .arg("run")
        .spawn()
        .context("spawn flip-daemon")?;
    Ok(())
}

/// Ping the device through the daemon. Returns the echoed payload.
pub fn ping_through_daemon(payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let stream = connect()?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut t = StreamTransport(stream);
    let mut reader = FrameReader::new();

    let mut enc = [0u8; 1100];
    let n = encode(MsgType::Ping, 0, 1, payload, &mut enc).ok_or_else(|| anyhow!("payload too big"))?;
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
```

- [ ] **Step 3: Implement the CLI commands**

`crates/flip-cli/src/main.rs`:
```rust
mod client;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "flip", about = "flip-link CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show daemon + device status (does a PING round-trip).
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Status => {
            match client::ping_through_daemon(b"flip-status", Duration::from_secs(3)) {
                Ok(echo) => {
                    println!("daemon: up");
                    println!("device: reachable (PONG round-trip ok)");
                    println!("echo:   {:?}", String::from_utf8_lossy(&echo));
                }
                Err(e) => {
                    println!("daemon: up");
                    println!("device: UNREACHABLE — {e:#}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 4: Build the whole workspace**

Run: `cargo build`
Expected: all four host crates compile; `flip` and `flip-daemon` binaries produced under `target/debug/`.

- [ ] **Step 5: Run the full host test suite**

Run: `cargo test`
Expected: PASS — all flip-proto and flip-core unit tests; the `hw_ping` test prints its skip message and passes.

- [ ] **Step 6: Commit**

```bash
git add crates/flip-cli
git commit -m "feat(cli): flip status with daemon auto-spawn"
```

---

## Task 12: Slice 0 acceptance — `flip status` end-to-end **[HW]**

**Files:**
- Create: `README.md`

- [ ] **Step 1 [HW]: Run the end-to-end acceptance**

Operator: connect the Flipper, launch the FAP (`cd firmware && run-fap target/thumbv7em-none-eabihf/release/flip_link.fap`), wait for re-enumeration, then from the repo root:
```bash
cargo build
FLIP_PORT=$(ls /dev/cu.usbmodemflip_* | tail -1) ./target/debug/flip status
```
(`FLIP_PORT` override avoids ambiguity; omit it to exercise auto-discovery.)
Expected output:
```
daemon: up
device: reachable (PONG round-trip ok)
echo:   "flip-status"
```
**This is the Slice 0 acceptance: the CLI is the primary testing harness and it proves the full CLI → daemon → device → firmware path.**

- [ ] **Step 2: Write the README documenting the run**

`README.md`:
```markdown
# flip-link

Low-level Rust toolkit for driving Flipper Zero instruments from a host CLI.
See `docs/superpowers/specs/` for the architecture and `docs/superpowers/plans/`
for implementation plans.

- `crates/flip-proto` — the wire contract (frames; shared host + firmware)
- `crates/flip-core`  — serial transport + device link
- `crates/flip-daemon`— owns the device link, relays frames over a unix socket
- `crates/flip-cli`   — the `flip` CLI (primary testing harness)
- `firmware/`         — the on-device FAP (flipperzero-rs; standalone package)

## Slice 0: prove the link

1. Build the firmware: `cd firmware && cargo build --release`
2. Launch on device: `run-fap target/thumbv7em-none-eabihf/release/flip_link.fap`
   (the app switches USB to dual-CDC and re-enumerates once — expected)
3. From the repo root: `cargo build && ./target/debug/flip status`

Expected: `device: reachable (PONG round-trip ok)`. Press Back on the Flipper to exit.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: README + Slice 0 acceptance run"
```

---

## Self-Review Notes (for the implementer)

- **`flip-proto` stays dependency-free** in Slice 0. `minicbor` is added in the Slice 1 (IR) plan when HELLO/CAPS/REQ bodies arrive.
- **Firmware is NOT a workspace member** — it's a standalone package under `firmware/` so the directory-scoped `.cargo/config.toml` (which forces the `thumbv7em` target) never touches the host build. It depends on `flip-proto` by relative path.
- **Two feasibility checkpoints:** Task 2 (toolchain reaches device with the stock app) and Task 9 (our dual-CDC firmware round-trips PING/PONG). Task 9 is the hard go/no-go on `flipperzero-rs`.
- **The riskiest code is Task 8** (sys symbol resolution + dual-CDC). Step 1 of that task grep-confirms symbol names against the installed bindings before writing the unsafe calls; the C prototype at `../mcp/firmware/flipper_agent_app.c` is the behavioral reference.
- **Type consistency:** `Transport` (`write_all`/`read`), `FrameReader` (`feed`/`next_frame`), `OwnedFrame` (`typ`/`flags`/`seq`/`payload`), `DeviceLink::ping`, `MsgType::{Ping,Pong}`, and `encode`/`decode`/`DecodeResult` are used identically across core, daemon, and cli.
- **Daemon relay is intentionally minimal** for Slice 0 (single client, raw forward + device-side reframing). Multi-client routing, session ownership, and reconnect-across-re-enumeration are Slice 1 work.
```
