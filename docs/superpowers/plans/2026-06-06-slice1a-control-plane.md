# flip-link Slice 1a — Control Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the CBOR control plane — capability discovery (`HELLO`/`CAPS`) and generic request/response (`REQ`/`RESP`/`ERROR`) — end-to-end, with a hardware-free `sys` test instrument, plus a daemon that reconnects across Flipper re-enumeration and serves multiple clients.

**Architecture:** `flip-proto` gains a recursive CBOR `Value` and minicbor-derived control envelopes (both firmware and host are Rust, so one codec). The firmware gains a `heap` (via `flipperzero-alloc`), an instrument registry, and a `sys` instrument (`version`, `echo`); it answers `HELLO`→`CAPS` and dispatches `REQ`→`RESP`/`ERROR` alongside the existing `PING`/`PONG`. The daemon is rewritten as a reconnecting, multi-client seq-rewriting proxy: a device-owner thread holds the serial link (reopening on broken pipe), answers `HELLO` from cached `CAPS`, and proxies `REQ`/`RESP` by rewriting the frame `seq`. The CLI gains `flip caps` and `flip invoke <instrument> <opcode> [k=v …]`.

**Tech Stack:** Rust; `minicbor` 2.2 (`derive` + `alloc`) on both sides; `flipperzero-alloc` 0.16 on the FAP; std threads + `mpsc` in the daemon. Builds on Slice 0 (verified on hardware). Reference: the C prototype registry at `../mcp/firmware/registry.c`.

**Scope:** Control plane only — **no IR, no streaming** (those are Plan 1b). `REQ`/`RESP` is strictly request/response here. Frame format, CRC, and `PING`/`PONG` are unchanged from Slice 0.

**Hardware note:** Steps tagged **[HW]** need a Flipper running the FAP; everything else is hardware-free. After flashing, restart the daemon (`just daemon-stop`) — Task 7 makes that automatic via reconnect, but during bring-up of earlier tasks do it manually.

---

## File Structure

```
crates/flip-proto/
  Cargo.toml                 # + minicbor dep, alloc feature
  src/lib.rs                 # + extern crate alloc (under feature); re-exports
  src/value.rs               # NEW: recursive CBOR Value + minicbor codec
  src/messages.rs            # NEW: Hello/Caps/Instrument/Req/Resp/AgentError + body helpers
crates/flip-core/
  src/device.rs              # + request()/hello() typed helpers over a Transport
crates/flip-daemon/
  src/server.rs              # REWRITE: reconnecting device-owner thread + socket accept
  src/device_conn.rs         # NEW: owns SerialTransport, connect()/reconnect()/HELLO→CAPS
  src/router.rs              # NEW: shared state — seq alloc, seq→client routing, cached CAPS
crates/flip-cli/
  src/client.rs              # + caps()/invoke() over the daemon socket
  src/main.rs                # + Caps, Invoke subcommands
  src/kv.rs                  # NEW: parse `k=v` args into a Value::Map
firmware/
  Cargo.toml                 # + flipperzero-alloc, flip-proto alloc feature
  src/main.rs                # + extern crate alloc; dispatch HELLO/REQ in the loop
  src/registry.rs            # NEW: instrument/opcode table + dispatch + build_caps
  src/sys_instrument.rs      # NEW: sys.version + sys.echo handlers
```

---

## Task 1: flip-proto — recursive CBOR `Value` (minicbor)

**Files:**
- Modify: `crates/flip-proto/Cargo.toml`
- Modify: `crates/flip-proto/src/lib.rs`
- Create: `crates/flip-proto/src/value.rs`

- [ ] **Step 1: Add minicbor + an `alloc` feature to the manifest**

`crates/flip-proto/Cargo.toml`:
```toml
[package]
name = "flip-proto"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
minicbor = { version = "2.2", default-features = false, features = ["derive"] }

[features]
default = []
alloc = ["minicbor/alloc"]
```

- [ ] **Step 2: Wire the alloc feature + new module into lib.rs**

`crates/flip-proto/src/lib.rs` (replace entire file):
```rust
//! flip-link wire contract. `no_std` so it compiles into the FAP and the host.
//! The frame codec is always `no_std`/alloc-free; the CBOR control messages
//! (the `value`/`messages` modules) require the `alloc` feature.
#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod crc16;
pub mod frame;

#[cfg(feature = "alloc")]
pub mod value;
#[cfg(feature = "alloc")]
pub mod messages;

pub use crc16::crc16_ccitt_false;
pub use frame::{decode, encode, DecodeResult, Frame, MsgType, FRAME_MAGIC, HEADER_SIZE, MAX_PAYLOAD};

#[cfg(feature = "alloc")]
pub use value::Value;
#[cfg(feature = "alloc")]
pub use messages::{AgentError, Caps, Hello, Instrument, Req, Resp};
```

- [ ] **Step 3: Write the `Value` type + codec with round-trip tests**

`crates/flip-proto/src/value.rs`:
```rust
//! A dynamic CBOR value for generic instrument params/results. Requires `alloc`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use minicbor::data::Type;
use minicbor::decode::Error as DecodeError;
use minicbor::encode::{Error as EncodeError, Write};
use minicbor::{Decoder, Encoder};

/// A CBOR value restricted to the wire profile (no floats; map keys are text).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    U64(u64),
    I64(i64),
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    /// Convenience: look up a key in a `Map` value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl<C> minicbor::Encode<C> for Value {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        match self {
            Value::Null => {
                e.null()?;
            }
            Value::Bool(b) => {
                e.bool(*b)?;
            }
            Value::U64(n) => {
                e.u64(*n)?;
            }
            Value::I64(n) => {
                e.i64(*n)?;
            }
            Value::Text(s) => {
                e.str(s)?;
            }
            Value::Bytes(b) => {
                e.bytes(b)?;
            }
            Value::Array(a) => {
                e.array(a.len() as u64)?;
                for v in a {
                    v.encode(e, ctx)?;
                }
            }
            Value::Map(m) => {
                e.map(m.len() as u64)?;
                for (k, v) in m {
                    e.str(k)?;
                    v.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> minicbor::Decode<'b, C> for Value {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, DecodeError> {
        match d.datatype()? {
            Type::Null => {
                d.null()?;
                Ok(Value::Null)
            }
            Type::Bool => Ok(Value::Bool(d.bool()?)),
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => Ok(Value::U64(d.u64()?)),
            Type::I8 | Type::I16 | Type::I32 | Type::I64 => Ok(Value::I64(d.i64()?)),
            Type::String => Ok(Value::Text(d.str()?.to_string())),
            Type::Bytes => Ok(Value::Bytes(d.bytes()?.to_vec())),
            Type::Array | Type::ArrayIndef => {
                let len = d.array()?;
                let mut out = Vec::new();
                match len {
                    Some(n) => {
                        for _ in 0..n {
                            out.push(Value::decode(d, ctx)?);
                        }
                    }
                    None => {
                        while d.datatype()? != Type::Break {
                            out.push(Value::decode(d, ctx)?);
                        }
                        d.skip()?;
                    }
                }
                Ok(Value::Array(out))
            }
            Type::Map | Type::MapIndef => {
                let len = d.map()?;
                let mut out = Vec::new();
                match len {
                    Some(n) => {
                        for _ in 0..n {
                            let k = d.str()?.to_string();
                            out.push((k, Value::decode(d, ctx)?));
                        }
                    }
                    None => {
                        while d.datatype()? != Type::Break {
                            let k = d.str()?.to_string();
                            out.push((k, Value::decode(d, ctx)?));
                        }
                        d.skip()?;
                    }
                }
                Ok(Value::Map(out))
            }
            _ => Err(DecodeError::message("unsupported CBOR type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(v: &Value) -> Value {
        let mut buf = Vec::new();
        minicbor::encode(v, &mut buf).unwrap();
        minicbor::decode(&buf).unwrap()
    }

    #[test]
    fn scalars_round_trip() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::U64(42),
            Value::I64(-7),
            Value::Text("hi".to_string()),
            Value::Bytes(alloc::vec![1u8, 2, 3]),
        ] {
            assert_eq!(round_trip(&v), v);
        }
    }

    #[test]
    fn nested_map_and_array_round_trip() {
        let v = Value::Map(alloc::vec![
            ("freq".to_string(), Value::U64(38000)),
            (
                "timings".to_string(),
                Value::Array(alloc::vec![Value::U64(9000), Value::U64(4500)]),
            ),
        ]);
        assert_eq!(round_trip(&v), v);
    }

    #[test]
    fn get_finds_key() {
        let v = Value::Map(alloc::vec![("k".to_string(), Value::U64(1))]);
        assert_eq!(v.get("k"), Some(&Value::U64(1)));
        assert_eq!(v.get("nope"), None);
    }
}
```

- [ ] **Step 4: Run the tests (with the alloc feature)**

Run: `cargo test -p flip-proto --features alloc`
Expected: PASS — `scalars_round_trip`, `nested_map_and_array_round_trip`, `get_finds_key`, plus the existing crc16/frame tests. If a `minicbor` method name differs (e.g. `Decoder::array` returning a different shape), adjust to the installed 2.2 API — do not change the test assertions.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-proto/Cargo.toml crates/flip-proto/src/lib.rs crates/flip-proto/src/value.rs
git commit -m "feat(proto): recursive CBOR Value with minicbor codec"
```

---

## Task 2: flip-proto — control message envelopes

**Files:**
- Create: `crates/flip-proto/src/messages.rs`

- [ ] **Step 1: Write the envelopes + frame-body helpers with tests**

`crates/flip-proto/src/messages.rs`:
```rust
//! CBOR control-message bodies carried in frame payloads. Requires `alloc`.
//! Both firmware and host use these exact types (one codec, no drift).

use alloc::string::String;
use alloc::vec::Vec;

use crate::value::Value;

/// Protocol version reported in CAPS.
pub const PROTOCOL_VERSION: u32 = 1;

/// Error codes (mirror the spec). 1..=6 reserved as in the C prototype.
pub const ERR_UNKNOWN_INSTRUMENT: u32 = 1;
pub const ERR_UNKNOWN_OPCODE: u32 = 2;
pub const ERR_BAD_PARAMS: u32 = 3;
pub const ERR_BUSY: u32 = 4;
pub const ERR_OVERSIZED: u32 = 5;
pub const ERR_INTERNAL: u32 = 6;

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Hello {
    #[n(0)]
    pub host_version: u32,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Instrument {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub opcodes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Caps {
    #[n(0)]
    pub protocol_version: u32,
    #[n(1)]
    pub instruments: Vec<Instrument>,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Req {
    #[n(0)]
    pub instrument: String,
    #[n(1)]
    pub opcode: String,
    #[n(2)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Resp {
    #[n(0)]
    pub ok: bool,
    #[n(1)]
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct AgentError {
    #[n(0)]
    pub code: u32,
    #[n(1)]
    pub message: String,
}

/// Encode any control body to a `Vec<u8>` (the frame payload).
pub fn to_payload<T: minicbor::Encode<()>>(msg: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    minicbor::encode(msg, &mut buf).expect("encode to Vec is infallible");
    buf
}

/// Decode a control body from a frame payload.
pub fn from_payload<'b, T: minicbor::Decode<'b, ()>>(
    bytes: &'b [u8],
) -> Result<T, minicbor::decode::Error> {
    minicbor::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn req_round_trip() {
        let req = Req {
            instrument: "sys".to_string(),
            opcode: "echo".to_string(),
            params: Value::Map(alloc::vec![("msg".to_string(), Value::Text("hi".to_string()))]),
        };
        let payload = to_payload(&req);
        let back: Req = from_payload(&payload).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn caps_round_trip() {
        let caps = Caps {
            protocol_version: PROTOCOL_VERSION,
            instruments: alloc::vec![Instrument {
                id: "sys".to_string(),
                opcodes: alloc::vec!["version".to_string(), "echo".to_string()],
            }],
        };
        let payload = to_payload(&caps);
        let back: Caps = from_payload(&payload).unwrap();
        assert_eq!(back, caps);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p flip-proto --features alloc`
Expected: PASS including `req_round_trip` and `caps_round_trip`.

- [ ] **Step 3: Commit**

```bash
git add crates/flip-proto/src/messages.rs
git commit -m "feat(proto): HELLO/CAPS/REQ/RESP/ERROR control envelopes"
```

---

## Task 3: firmware — enable the heap (alloc)

The control messages decode into `String`/`Vec`, so the FAP needs a global allocator. `flipperzero-alloc` provides one backed by the Flipper's `furi` allocator (the C prototype used `malloc`, so the heap is proven).

**Files:**
- Modify: `firmware/Cargo.toml`
- Modify: `firmware/src/main.rs`

- [ ] **Step 1: Add the allocator + the proto alloc feature**

`firmware/Cargo.toml` — update the `[dependencies]` section to:
```toml
[dependencies]
flipperzero = "0.16.0"
flipperzero-sys = "0.16.0"
flipperzero-rt = "0.16.0"
flipperzero-alloc = "0.16.0"
flip-proto = { path = "../crates/flip-proto", features = ["alloc"] }
```

- [ ] **Step 2: Link the allocator + alloc crate in main.rs**

At the top of `firmware/src/main.rs`, directly after `extern crate flipperzero_rt;`, add:
```rust
extern crate alloc;
extern crate flipperzero_alloc;
```
(Keep everything else as-is for now. `flipperzero_alloc` registers the `#[global_allocator]`; `alloc` makes `String`/`Vec` available.)

- [ ] **Step 3: Build the firmware**

Run: `cd firmware && cargo build --release`
Expected: builds `flip_link.fap`. If the linker complains about a missing/duplicate allocator, confirm only `flipperzero-alloc` provides `#[global_allocator]`.

- [ ] **Step 4: Commit**

```bash
git add firmware/Cargo.toml firmware/src/main.rs
git commit -m "feat(fw): enable heap (flipperzero-alloc) for CBOR control messages"
```

---

## Task 4: firmware — instrument registry + `sys` instrument

Ports the C `registry.c` pattern (static table → dispatch) and adds a hardware-free `sys` instrument so the control plane is testable without IR.

**Files:**
- Create: `firmware/src/sys_instrument.rs`
- Create: `firmware/src/registry.rs`

- [ ] **Step 1: Write the `sys` instrument handlers**

`firmware/src/sys_instrument.rs`:
```rust
//! Hardware-free test instrument: proves the REQ/RESP control plane.

use alloc::string::{String, ToString};
use alloc::vec;
use flip_proto::Value;

/// A handler maps decoded params to a result Value, or an (error_code, message).
pub type Handler = fn(params: &Value) -> Result<Value, (u32, String)>;

/// `sys.version` — ignores params, returns protocol + firmware identity.
pub fn version(_params: &Value) -> Result<Value, (u32, String)> {
    Ok(Value::Map(vec![
        (
            "protocol".to_string(),
            Value::U64(flip_proto::messages::PROTOCOL_VERSION as u64),
        ),
        ("fw".to_string(), Value::Text("flip-link 0.1".to_string())),
    ]))
}

/// `sys.echo` — returns its params unchanged (proves params round-trip).
pub fn echo(params: &Value) -> Result<Value, (u32, String)> {
    Ok(params.clone())
}
```

- [ ] **Step 2: Write the registry (table + dispatch + CAPS builder)**

`firmware/src/registry.rs`:
```rust
//! Static instrument/opcode table, dispatch, and CAPS construction.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use flip_proto::messages::{Caps, Instrument, PROTOCOL_VERSION};
use flip_proto::Value;

use crate::sys_instrument;

struct OpcodeEntry {
    opcode: &'static str,
    handler: sys_instrument::Handler,
}

struct InstrumentEntry {
    id: &'static str,
    opcodes: &'static [OpcodeEntry],
}

static SYS_OPCODES: &[OpcodeEntry] = &[
    OpcodeEntry {
        opcode: "version",
        handler: sys_instrument::version,
    },
    OpcodeEntry {
        opcode: "echo",
        handler: sys_instrument::echo,
    },
];

static INSTRUMENTS: &[InstrumentEntry] = &[InstrumentEntry {
    id: "sys",
    opcodes: SYS_OPCODES,
}];

/// Find a handler by instrument id + opcode. Returns None if either is unknown.
pub fn find(instrument: &str, opcode: &str) -> Option<sys_instrument::Handler> {
    let inst = INSTRUMENTS.iter().find(|i| i.id == instrument)?;
    inst.opcodes
        .iter()
        .find(|o| o.opcode == opcode)
        .map(|o| o.handler)
}

/// True if the instrument id exists (to distinguish unknown-instrument from
/// unknown-opcode errors).
pub fn has_instrument(instrument: &str) -> bool {
    INSTRUMENTS.iter().any(|i| i.id == instrument)
}

/// Build the CAPS body from the static table.
pub fn build_caps() -> Caps {
    let instruments = INSTRUMENTS
        .iter()
        .map(|i| Instrument {
            id: i.id.to_string(),
            opcodes: i.opcodes.iter().map(|o| o.opcode.to_string()).collect::<Vec<String>>(),
        })
        .collect();
    Caps {
        protocol_version: PROTOCOL_VERSION,
        instruments,
    }
}

/// Dispatch a decoded request to its handler. Returns the result Value or an
/// (error_code, message). Unknown instrument/opcode produce the mirrored codes.
pub fn dispatch(instrument: &str, opcode: &str, params: &Value) -> Result<Value, (u32, String)> {
    match find(instrument, opcode) {
        Some(handler) => handler(params),
        None if !has_instrument(instrument) => Err((
            flip_proto::messages::ERR_UNKNOWN_INSTRUMENT,
            "unknown instrument".to_string(),
        )),
        None => Err((
            flip_proto::messages::ERR_UNKNOWN_OPCODE,
            "unknown opcode".to_string(),
        )),
    }
}
```

- [ ] **Step 3: Declare the modules (build check happens in Task 5)**

Add to the top of `firmware/src/main.rs` (after the `extern crate` lines, before `manifest!`):
```rust
mod registry;
mod sys_instrument;
```

- [ ] **Step 4: Build**

Run: `cd firmware && cargo build --release`
Expected: compiles (modules unused-warning is fine until Task 5 wires them in).

- [ ] **Step 5: Commit**

```bash
git add firmware/src/registry.rs firmware/src/sys_instrument.rs firmware/src/main.rs
git commit -m "feat(fw): instrument registry + sys instrument (version, echo)"
```

---

## Task 5: firmware — dispatch HELLO/REQ in the main loop

Extends the Slice 0 frame handling: in addition to `PING`→`PONG`, answer `HELLO`→`CAPS` and `REQ`→`RESP`/`ERROR`. The frame decode/drain loop already exists; this adds branches by `MsgType`.

**Files:**
- Modify: `firmware/src/main.rs`

- [ ] **Step 1: Add a frame-send helper and a request handler**

In `firmware/src/main.rs`, add these functions (above `fn main`):
```rust
/// Encode a control body and send it as a frame of `typ` with `seq`.
fn send_msg<T: minicbor::Encode<()>>(typ: MsgType, seq: u16, body: &T) {
    let payload = flip_proto::messages::to_payload(body);
    let mut frame = [0u8; ENC_CAP];
    if let Some(n) = encode(typ, 0, seq, &payload, &mut frame) {
        cdc_send_all(&frame[..n]);
    }
}

/// Handle one decoded control frame (HELLO/REQ); PING is handled inline in the
/// drain loop. Unknown/other types are ignored.
fn handle_frame(typ: MsgType, seq: u16, payload: &[u8]) {
    use flip_proto::messages::{from_payload, AgentError, Req, Resp};
    match typ {
        MsgType::Hello => {
            let caps = registry::build_caps();
            send_msg(MsgType::Caps, seq, &caps);
        }
        MsgType::Req => match from_payload::<Req>(payload) {
            Ok(req) => match registry::dispatch(&req.instrument, &req.opcode, &req.params) {
                Ok(result) => send_msg(MsgType::Resp, seq, &Resp { ok: true, result }),
                Err((code, message)) => {
                    send_msg(MsgType::Error, seq, &AgentError { code, message })
                }
            },
            Err(_) => send_msg(
                MsgType::Error,
                seq,
                &AgentError {
                    code: flip_proto::messages::ERR_BAD_PARAMS,
                    message: alloc::string::String::from("bad REQ body"),
                },
            ),
        },
        _ => {}
    }
}
```

- [ ] **Step 2: Call it from the drain loop**

In the drain loop inside `fn main`, find the `DecodeResult::Frame(f, consumed)` arm. Replace its body (currently only the `MsgType::Ping` echo) with:
```rust
                DecodeResult::Frame(f, consumed) => {
                    match f.typ {
                        MsgType::Ping => {
                            if let Some(en) = encode(MsgType::Pong, 0, f.seq, f.payload, &mut enc) {
                                send_len = en;
                            }
                        }
                        _ => handle_frame(f.typ, f.seq, f.payload),
                    }
                    used = consumed;
                }
```
(`send_len` stays the PING/PONG fast path written via `cdc_send_all(&enc[..send_len])`; `handle_frame` sends its own frames directly. `used` is still set to `consumed`.)

- [ ] **Step 3: Build**

Run: `cd firmware && cargo build --release`
Expected: compiles to `flip_link.fap`. Resolve any borrow issue by ensuring `handle_frame` is called with `f.payload` before `acc` is mutated (it is — it runs inside the match arm, same as the PING path).

- [ ] **Step 4: Commit**

```bash
git add firmware/src/main.rs
git commit -m "feat(fw): answer HELLO->CAPS and REQ->RESP/ERROR in the main loop"
```

---

## Task 6: flip-core — typed request/hello helpers over a Transport

The daemon (to the device) and tests need to send a `REQ` and await the `RESP`/`ERROR`, and to do the `HELLO`→`CAPS` handshake. Add these to `DeviceLink`, reusing its seq allocation and `FrameReader`.

**Files:**
- Modify: `crates/flip-core/Cargo.toml`
- Modify: `crates/flip-core/src/device.rs`

- [ ] **Step 1: Enable the proto alloc feature for flip-core**

`crates/flip-core/Cargo.toml` — change the `flip-proto` dependency line to:
```toml
flip-proto = { path = "../flip-proto", features = ["alloc"] }
```

- [ ] **Step 2: Add `hello` and `request` to DeviceLink with a mock test**

Append to `crates/flip-core/src/device.rs` (inside `impl<T: Transport> DeviceLink<T>`, before the closing brace of the impl):
```rust
    /// Send HELLO and await the CAPS body.
    pub fn hello(&mut self, timeout: Duration) -> Result<flip_proto::Caps> {
        let seq = self.alloc_seq();
        let body = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
        self.send(MsgType::Hello, seq, &body)?;
        let frame = self.await_seq(seq, timeout)?;
        match frame.typ {
            MsgType::Caps => Ok(flip_proto::messages::from_payload(&frame.payload)
                .map_err(|e| anyhow!("decode CAPS: {e}"))?),
            other => Err(anyhow!("expected CAPS, got {:?}", other)),
        }
    }

    /// Send a REQ and await the RESP (Ok) or ERROR (Err with code+message).
    pub fn request(
        &mut self,
        instrument: &str,
        opcode: &str,
        params: flip_proto::Value,
        timeout: Duration,
    ) -> Result<flip_proto::Resp> {
        let seq = self.alloc_seq();
        let req = flip_proto::Req {
            instrument: instrument.into(),
            opcode: opcode.into(),
            params,
        };
        let body = flip_proto::messages::to_payload(&req);
        self.send(MsgType::Req, seq, &body)?;
        let frame = self.await_seq(seq, timeout)?;
        match frame.typ {
            MsgType::Resp => flip_proto::messages::from_payload(&frame.payload)
                .map_err(|e| anyhow!("decode RESP: {e}")),
            MsgType::Error => {
                let e: flip_proto::AgentError = flip_proto::messages::from_payload(&frame.payload)
                    .map_err(|e| anyhow!("decode ERROR: {e}"))?;
                Err(anyhow!("device error {}: {}", e.code, e.message))
            }
            other => Err(anyhow!("expected RESP/ERROR, got {:?}", other)),
        }
    }

    /// Write a framed message body.
    fn send(&mut self, typ: MsgType, seq: u16, body: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; flip_proto::HEADER_SIZE + body.len() + 2];
        let n = encode(typ, 0, seq, body, &mut buf).ok_or_else(|| anyhow!("frame too large"))?;
        self.transport.write_all(&buf[..n])
    }

    /// Read frames until one matches `seq`, or time out.
    fn await_seq(&mut self, seq: u16, timeout: Duration) -> Result<crate::transport::OwnedFrame> {
        let deadline = Instant::now() + timeout;
        let mut scratch = [0u8; 512];
        loop {
            if let Some(f) = self.reader.next_frame() {
                if f.seq == seq {
                    return Ok(f);
                }
                continue;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("timed out waiting for seq {seq}"));
            }
            let got = self.transport.read(&mut scratch)?;
            if got > 0 {
                self.reader.feed(&scratch[..got]);
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
```
flip-core is `std`, so `Vec`/`vec!`/`String`/`into()` are in the prelude; `flip_proto::*` types are reached by full path. No new `use` lines are needed beyond the existing `use flip_proto::{encode, MsgType};` and the `Duration`/`Instant`/`anyhow!` already imported in `device.rs`.

- [ ] **Step 3: Extend the mock to answer HELLO/REQ**

The existing `PongLoopback` only answers PING. Add a second mock for control messages. Append to `crates/flip-core/src/mock.rs`:
```rust
use flip_proto::messages::{from_payload, to_payload, Caps, Instrument, Req, Resp, PROTOCOL_VERSION};
use flip_proto::Value;

/// In-memory transport answering HELLO->CAPS and REQ(sys.echo)->RESP(params).
pub struct ControlLoopback {
    out: VecDeque<u8>,
    reader: FrameReader,
}

impl ControlLoopback {
    pub fn new() -> Self {
        ControlLoopback {
            out: VecDeque::new(),
            reader: FrameReader::new(),
        }
    }
    fn queue(&mut self, typ: MsgType, seq: u16, body: &[u8]) {
        let mut enc = vec![0u8; flip_proto::HEADER_SIZE + body.len() + 2];
        let n = flip_proto::encode(typ, 0, seq, body, &mut enc).unwrap();
        self.out.extend(&enc[..n]);
    }
}

impl Default for ControlLoopback {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for ControlLoopback {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.reader.feed(buf);
        while let Some(f) = self.reader.next_frame() {
            match f.typ {
                MsgType::Hello => {
                    let caps = Caps {
                        protocol_version: PROTOCOL_VERSION,
                        instruments: vec![Instrument {
                            id: "sys".to_string(),
                            opcodes: vec!["version".to_string(), "echo".to_string()],
                        }],
                    };
                    let body = to_payload(&caps);
                    self.queue(MsgType::Caps, f.seq, &body);
                }
                MsgType::Req => {
                    let req: Req = from_payload(&f.payload).unwrap();
                    let result = if req.opcode == "echo" { req.params } else { Value::Null };
                    let body = to_payload(&Resp { ok: true, result });
                    self.queue(MsgType::Resp, f.seq, &body);
                }
                _ => {}
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
Add `use std::string::String;`? No — std prelude. Ensure `use flip_proto::MsgType;` is in scope (the file already uses `MsgType` for the pong mock).

- [ ] **Step 4: Add device tests using the control mock**

Append to the `tests` module in `crates/flip-core/src/device.rs`:
```rust
    #[test]
    fn hello_returns_caps_via_mock() {
        use crate::mock::ControlLoopback;
        let mut link = DeviceLink::new(ControlLoopback::new());
        let caps = link.hello(Duration::from_millis(500)).unwrap();
        assert_eq!(caps.protocol_version, 1);
        assert_eq!(caps.instruments[0].id, "sys");
    }

    #[test]
    fn request_echo_round_trips_params() {
        use crate::mock::ControlLoopback;
        use flip_proto::Value;
        let mut link = DeviceLink::new(ControlLoopback::new());
        let params = Value::Map(vec![("msg".to_string(), Value::Text("hi".to_string()))]);
        let resp = link
            .request("sys", "echo", params.clone(), Duration::from_millis(500))
            .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result, params);
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p flip-core`
Expected: PASS — existing 4 tests + `hello_returns_caps_via_mock` + `request_echo_round_trips_params`.

- [ ] **Step 6: Commit**

```bash
git add crates/flip-core/Cargo.toml crates/flip-core/src/device.rs crates/flip-core/src/mock.rs
git commit -m "feat(core): DeviceLink hello()/request() + control mock"
```

---

## Task 7: flip-daemon — reconnecting device-owner + shared router

Rewrite the daemon. A device-owner thread holds the serial link, performs `HELLO`→`CAPS` on connect, reconnects on any I/O error, reads inbound frames and routes `RESP`/`ERROR`/`CAPS` to clients by `seq`, and writes outbound frames from a channel. Shared state lives in `router.rs`.

**Files:**
- Create: `crates/flip-daemon/src/router.rs`
- Create: `crates/flip-daemon/src/device_conn.rs`

- [ ] **Step 1: Write the shared router state**

`crates/flip-daemon/src/router.rs`:
```rust
//! Shared daemon state: device-seq allocation, seq->client routing, cached CAPS.

use flip_core::transport::OwnedFrame;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    next_seq: u16,
    routes: HashMap<u16, Sender<OwnedFrame>>,
    caps_payload: Vec<u8>, // cached CAPS body (empty until first connect)
}

#[derive(Default)]
pub struct Router {
    inner: Mutex<Inner>,
}

impl Router {
    pub fn new() -> Self {
        Router {
            inner: Mutex::new(Inner {
                next_seq: 1,
                ..Default::default()
            }),
        }
    }

    /// Allocate a unique device-side seq and register where its reply should go.
    pub fn register(&self, reply_to: Sender<OwnedFrame>) -> u16 {
        let mut g = self.inner.lock().unwrap();
        let seq = g.next_seq;
        g.next_seq = g.next_seq.wrapping_add(1).max(1);
        g.routes.insert(seq, reply_to);
        seq
    }

    /// Deliver an inbound device frame to the client that owns its seq (if any).
    pub fn deliver(&self, frame: OwnedFrame) {
        let sender = {
            let mut g = self.inner.lock().unwrap();
            g.routes.remove(&frame.seq)
        };
        if let Some(tx) = sender {
            let _ = tx.send(frame);
        }
    }

    pub fn set_caps(&self, payload: Vec<u8>) {
        self.inner.lock().unwrap().caps_payload = payload;
    }

    pub fn caps(&self) -> Vec<u8> {
        self.inner.lock().unwrap().caps_payload.clone()
    }
}
```

- [ ] **Step 2: Write the device-owner connection/thread**

`crates/flip-daemon/src/device_conn.rs`:
```rust
//! Owns the serial link to the device: connect, HELLO->CAPS, read/route, reconnect.

use crate::router::Router;
use anyhow::{Context, Result};
use flip_core::serial::{pick_agent_port, SerialTransport};
use flip_core::transport::{FrameReader, Transport};
use flip_proto::MsgType;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// Run the device owner forever: (re)connect, then pump frames until an error,
/// then reconnect. `outbound` carries already-framed bytes from clients.
pub fn run(router: Arc<Router>, outbound: Receiver<Vec<u8>>) -> ! {
    loop {
        match session(&router, &outbound) {
            Ok(()) => {}
            Err(e) => eprintln!("device session ended: {e:#}; reconnecting in 500ms"),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn session(router: &Router, outbound: &Receiver<Vec<u8>>) -> Result<()> {
    let port = pick_agent_port().context("find Flipper agent port")?;
    let mut device = SerialTransport::open(&port).context("open device")?;
    eprintln!("flip-daemon connected to device on {port}");

    // HELLO -> CAPS handshake; cache the CAPS body for clients.
    cache_caps(router, &mut device)?;

    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 1024];
    loop {
        // Drain any queued outbound frames to the device (non-blocking).
        while let Ok(bytes) = outbound.try_recv() {
            device.write_all(&bytes)?;
        }
        // Read inbound and route whole frames.
        let got = device.read(&mut scratch)?;
        if got > 0 {
            reader.feed(&scratch[..got]);
            while let Some(frame) = reader.next_frame() {
                router.deliver(frame);
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Send HELLO and wait for CAPS, caching its payload. Times out after 2s.
fn cache_caps(router: &Router, device: &mut SerialTransport) -> Result<()> {
    let hello = flip_proto::messages::to_payload(&flip_proto::Hello { host_version: 0 });
    let mut buf = vec![0u8; flip_proto::HEADER_SIZE + hello.len() + 2];
    let n = flip_proto::encode(MsgType::Hello, 0, 0, &hello, &mut buf).unwrap();
    device.write_all(&buf[..n])?;

    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 512];
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(f) = reader.next_frame() {
            if f.typ == MsgType::Caps {
                router.set_caps(f.payload);
                eprintln!("flip-daemon cached CAPS ({} bytes)", router.caps().len());
                return Ok(());
            }
            continue;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for CAPS");
        }
        let got = device.read(&mut scratch)?;
        if got > 0 {
            reader.feed(&scratch[..got]);
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
```

- [ ] **Step 3: Build (server.rs still old; wired in Task 8)**

Run: `cargo build -p flip-daemon`
Expected: compiles with `unused` warnings for the new modules (they're wired in Task 8). If `OwnedFrame.payload` move/borrow errors appear in `router.deliver`, note that `OwnedFrame` is owned (`payload: Vec<u8>`), so moving it into the channel is fine.

- [ ] **Step 4: Commit**

```bash
git add crates/flip-daemon/src/router.rs crates/flip-daemon/src/device_conn.rs
git commit -m "feat(daemon): reconnecting device-owner thread + seq router"
```

---

## Task 8: flip-daemon — multi-client server (HELLO from cache, REQ proxy)

Replace `server.rs`. Spawn the device-owner thread, then accept clients; each client gets a thread that answers `HELLO` from cached CAPS and proxies other frames through the router (rewriting `seq` so replies route back).

**Files:**
- Modify: `crates/flip-daemon/src/main.rs`
- Modify (replace): `crates/flip-daemon/src/server.rs`

- [ ] **Step 1: Declare the new modules**

`crates/flip-daemon/src/main.rs` — add module declarations at the top (keep the existing clap `Cli`/`Cmd`/`main`):
```rust
mod device_conn;
mod router;
mod server;
```
(The rest of `main.rs` is unchanged: `Cmd::Run => server::run()`.)

- [ ] **Step 2: Replace server.rs with the multi-client proxy**

`crates/flip-daemon/src/server.rs` (replace entire file):
```rust
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
    {
        let router = router.clone();
        std::thread::spawn(move || device_conn::run(router, outbound_rx));
    }

    for stream in listener.incoming() {
        let stream = stream?;
        let router = router.clone();
        let outbound = outbound_tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_client(stream, router, outbound) {
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
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut reader = FrameReader::new();
    let mut scratch = [0u8; 1024];
    // Per-client channel for routed device replies.
    let (reply_tx, reply_rx) = channel::<OwnedFrame>();

    loop {
        match stream.read(&mut scratch) {
            Ok(0) => return Ok(()), // client closed
            Ok(n) => reader.feed(&scratch[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }

        while let Some(frame) = reader.next_frame() {
            if frame.typ == MsgType::Hello {
                // Answer from cache without touching the device.
                let caps = router.caps();
                write_frame(&mut stream, MsgType::Caps, frame.seq, &caps)?;
                continue;
            }
            // Proxy: rewrite seq, forward to device, await routed reply.
            let client_seq = frame.seq;
            let dev_seq = router.register(reply_tx.clone());
            let mut buf = vec![0u8; flip_proto::HEADER_SIZE + frame.payload.len() + 2];
            let n =
                flip_proto::encode(frame.typ, frame.flags, dev_seq, &frame.payload, &mut buf)
                    .expect("reframe");
            outbound.send(buf[..n].to_vec()).ok();

            match reply_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(reply) => {
                    write_frame(&mut stream, reply.typ, client_seq, &reply.payload)?;
                }
                Err(_) => {
                    // Timed out (e.g. device reconnecting): tell the client.
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
```

- [ ] **Step 3: Build the daemon**

Run: `cargo build -p flip-daemon`
Expected: compiles clean. (No unit tests here; the router/device logic is exercised end-to-end in Task 11. `Router` could be unit-tested, but its value is in integration.)

- [ ] **Step 4: Commit**

```bash
git add crates/flip-daemon/src/main.rs crates/flip-daemon/src/server.rs
git commit -m "feat(daemon): multi-client proxy — HELLO from cache, REQ seq-rewrite"
```

---

## Task 9: flip-cli — `flip caps`

**Files:**
- Modify: `crates/flip-cli/Cargo.toml`
- Modify: `crates/flip-cli/src/client.rs`
- Modify: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Enable the proto alloc feature for the CLI**

`crates/flip-cli/Cargo.toml` — change the `flip-proto` dependency to:
```toml
flip-proto = { path = "../flip-proto", features = ["alloc"] }
```

- [ ] **Step 2: Add a `caps()` helper to the client**

Append to `crates/flip-cli/src/client.rs` (after `ping_through_daemon`):
```rust
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
        MsgType::Caps => flip_proto::messages::from_payload(&payload)
            .map_err(|e| anyhow!("decode CAPS: {e}")),
        other => Err(anyhow!("expected CAPS, got {:?}", other)),
    }
}
```
Ensure the imports at the top of `client.rs` include `use flip_proto::{encode, MsgType};` (already present) and `use flip_core::transport::FrameReader;` (already present from `ping_through_daemon`).

- [ ] **Step 3: Add the `caps` subcommand**

In `crates/flip-cli/src/main.rs`, add to the `Cmd` enum:
```rust
    /// List instruments and opcodes the device advertises.
    Caps,
```
And add the match arm in `main`:
```rust
        Cmd::Caps => {
            let caps = client::caps(Duration::from_secs(3))?;
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

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: whole workspace compiles; `flip` binary builds.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-cli/Cargo.toml crates/flip-cli/src/client.rs crates/flip-cli/src/main.rs
git commit -m "feat(cli): flip caps"
```

---

## Task 10: flip-cli — `flip invoke <instrument> <opcode> [k=v …]`

**Files:**
- Create: `crates/flip-cli/src/kv.rs`
- Modify: `crates/flip-cli/src/client.rs`
- Modify: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Write the `k=v` → Value parser with tests**

`crates/flip-cli/src/kv.rs`:
```rust
//! Parse `key=value` CLI args into a `Value::Map`. Values are typed by shape:
//! integers -> U64/I64, true/false -> Bool, everything else -> Text.

use anyhow::{anyhow, Result};
use flip_proto::Value;

pub fn parse_params(args: &[String]) -> Result<Value> {
    let mut pairs = Vec::new();
    for arg in args {
        let (k, v) = arg
            .split_once('=')
            .ok_or_else(|| anyhow!("param '{arg}' is not key=value"))?;
        pairs.push((k.to_string(), parse_value(v)));
    }
    Ok(Value::Map(pairs))
}

fn parse_value(s: &str) -> Value {
    if s == "true" {
        return Value::Bool(true);
    }
    if s == "false" {
        return Value::Bool(false);
    }
    if let Ok(u) = s.parse::<u64>() {
        return Value::U64(u);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::I64(i);
    }
    Value::Text(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_pairs() {
        let v = parse_params(&[
            "msg=hi".to_string(),
            "n=5".to_string(),
            "neg=-3".to_string(),
            "flag=true".to_string(),
        ])
        .unwrap();
        assert_eq!(
            v,
            Value::Map(vec![
                ("msg".to_string(), Value::Text("hi".to_string())),
                ("n".to_string(), Value::U64(5)),
                ("neg".to_string(), Value::I64(-3)),
                ("flag".to_string(), Value::Bool(true)),
            ])
        );
    }

    #[test]
    fn rejects_bare_arg() {
        assert!(parse_params(&["nope".to_string()]).is_err());
    }
}
```

- [ ] **Step 2: Add `invoke()` to the client + a Value pretty-printer**

Append to `crates/flip-cli/src/client.rs`:
```rust
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

/// One-line rendering of a result Value for the CLI.
pub fn render_value(v: &flip_proto::Value) -> String {
    use flip_proto::Value::*;
    match v {
        Null => "null".to_string(),
        Bool(b) => b.to_string(),
        U64(n) => n.to_string(),
        I64(n) => n.to_string(),
        Text(s) => s.clone(),
        Bytes(b) => format!("0x{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
        Array(a) => {
            let inner = a.iter().map(render_value).collect::<Vec<_>>().join(", ");
            format!("[{inner}]")
        }
        Map(m) => {
            let inner = m
                .iter()
                .map(|(k, v)| format!("{k}: {}", render_value(v)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}
```

- [ ] **Step 3: Add the `invoke` subcommand**

In `crates/flip-cli/src/main.rs`, declare the module at the top:
```rust
mod kv;
```
Add to the `Cmd` enum:
```rust
    /// Invoke an instrument opcode with optional key=value params.
    Invoke {
        instrument: String,
        opcode: String,
        /// Zero or more key=value params.
        params: Vec<String>,
    },
```
Add the match arm in `main`:
```rust
        Cmd::Invoke {
            instrument,
            opcode,
            params,
        } => {
            let params = kv::parse_params(&params)?;
            let resp = client::invoke(&instrument, &opcode, params, Duration::from_secs(3))?;
            println!("{}", client::render_value(&resp.result));
            Ok(())
        }
```

- [ ] **Step 4: Build + run host tests**

Run: `cargo build && cargo test`
Expected: whole workspace builds; all tests pass (proto with `--features alloc` is exercised via flip-core which enables it; the `kv` tests pass). Note: `cargo test` at the root tests flip-proto WITHOUT the alloc feature, so its value/messages tests won't run there — run `cargo test -p flip-proto --features alloc` to exercise those. Confirm both.

- [ ] **Step 5: Commit**

```bash
git add crates/flip-cli/src/kv.rs crates/flip-cli/src/client.rs crates/flip-cli/src/main.rs
git commit -m "feat(cli): flip invoke with key=value params"
```

---

## Task 11: [HW] End-to-end acceptance + reconnect

**Files:**
- Modify: `README.md`

- [ ] **Step 1 [HW]: Flash the new firmware**

```bash
just reflash            # daemon-stop + build + upload
```
On the Flipper: **Apps → Tools → flip-link** (status screen shows; interface 1 ready).

- [ ] **Step 2 [HW]: Capabilities**

Run: `just status` (confirms PONG still works), then:
```bash
./target/debug/flip caps
```
Expected:
```
protocol v1
sys
  sys.version
  sys.echo
```

- [ ] **Step 3 [HW]: Invoke**

```bash
./target/debug/flip invoke sys version
./target/debug/flip invoke sys echo msg=hi n=5
```
Expected (order of map keys as sent):
```
{protocol: 1, fw: flip-link 0.1}
{msg: hi, n: 5}
```

- [ ] **Step 4 [HW]: Reconnect across re-enumeration**

With the daemon running and the app running, reboot the Flipper (`←`+`BACK`), relaunch the app on-device, wait ~3s, then **without** restarting the daemon:
```bash
./target/debug/flip caps
```
Expected: succeeds again (the device-owner thread reconnected and re-cached CAPS). The first call right after reboot may return a `device timeout` ERROR if it lands mid-reconnect; a retry succeeds. This validates Task 7's reconnect — no more manual `just daemon-stop` after a reboot.

- [ ] **Step 5: Document the control plane in the README**

Add this section to `README.md` after the "Proving the link" section:
```markdown
## Capabilities & invoke (Slice 1a)

With the FAP running and the daemon up:

```sh
flip caps                          # list instruments/opcodes
flip invoke sys version            # -> {protocol: 1, fw: flip-link 0.1}
flip invoke sys echo msg=hi n=5    # -> {msg: hi, n: 5}
```

The daemon reconnects automatically when the Flipper re-enumerates (reboot /
relaunch), so it no longer needs a manual restart after flashing.
```

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs: Slice 1a control plane (caps/invoke) + reconnect"
```

---

## Self-Review Notes (for the implementer)

- **`flip-proto` is split:** the frame codec stays `no_std`/alloc-free (unchanged); `value`/`messages` require `--features alloc`. Firmware and the host crates all enable `alloc`. Root `cargo test` does NOT run the proto value/messages tests (no feature) — run `cargo test -p flip-proto --features alloc`.
- **Firmware now uses the heap** (`flipperzero-alloc`). The 4 KB stack from Slice 0 is unchanged; CBOR temporaries (`Vec<u8>` payloads, decoded `String`s) live on the heap.
- **Daemon is now a real proxy:** device-owner thread (reconnects on any serial error), per-client threads, `Router` maps a daemon-allocated device seq → the originating client's reply channel. HELLO is answered from cached CAPS without a device round-trip. This is the reconnect fix promised for Slice 1.
- **Type consistency:** `Value`, `Req{instrument,opcode,params}`, `Resp{ok,result}`, `Caps{protocol_version,instruments}`, `Instrument{id,opcodes}`, `AgentError{code,message}`, `Hello{host_version}`, `to_payload`/`from_payload`, `DeviceLink::{hello,request}`, `registry::{find,has_instrument,build_caps,dispatch}`, `client::{caps,invoke,round_trip,render_value}`, `kv::parse_params` are used identically across crates.
- **Not in scope (Plan 1b):** streaming (STREAM_*), the IR instrument, richer error mapping. `REQ`/`RESP` here is strictly one-shot request/response.
- **Known minicbor risk:** the exact 2.2 method names (`Decoder::array`/`map` return shapes, `data::Type` variant names) are verified against the docs but confirm at compile time in Task 1; adjust the codec (not the tests) if a name differs.
