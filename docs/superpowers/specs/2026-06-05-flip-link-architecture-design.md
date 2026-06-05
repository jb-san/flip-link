# flip-link — Architecture Design

**Date:** 2026-06-05
**Status:** Approved design, pending implementation plan
**Supersedes:** the MCP-first prototype in `../mcp` (proven, but wrong starting point)

## 1. Vision

`flip-link` is a clean, all-Rust rewrite of the Flipper-Zero host-agent bridge.
It gives a host computer low-level, raw access to the Flipper's instruments —
starting with **Infrared** capture/transmit — through a CLI, and is built so that
new instruments (I²C / SPI / UART bus devices, and later raw GPIO digital/ADC)
are pure on-device additions that require no protocol, transport, or host changes.

The MCP server is explicitly **deferred** — it comes only after a functioning CLI,
and when it arrives it is a thin client on the same local socket the CLI already uses.

### North star (direction, not v1 scope)

The longer-term ambition is to turn the Flipper + flip-link into a **programmable
bus bridge that re-exposes attached hardware to the host OS as if it were natively
connected**:

- **WiFi-board-as-network-interface:** present the GPIO-attached ESP32 WiFi board
  to the laptop as an OS-level network interface (a `utun`/TUN device), so it can
  act as a secondary WiFi adapter. Realistic path: the daemon opens a host TUN
  device and pumps IP packets over the CBOR transport → FAP → UART → ESP32, where
  **cooperating ESP32 firmware** bridges packets onto its WiFi station and NATs out.
  Honest constraints: throughput is bounded by the Flipper GPIO-UART (low
  single-digit Mbps at best) plus USB-CDC overhead, and it requires custom ESP32
  firmware. Good for light use, not a fast link. Full USB-class gadget emulation
  (RNDIS/ECM presented *by the Flipper*) is out of reach; the in-reach version is a
  **host-side userspace virtual interface**.
- **General pattern — "exposers":** any instrument can be bridged to an OS-native
  surface — TUN for network, a PTY (virtual serial port) for a UART device, etc.

This north star is **not implemented in this spec**, but it justifies two design
decisions made now (see §3.3 bidirectional streaming and §6 the exposer seam) and a
future slice (§9, Slice 4).

## 2. Why this architecture

The host cannot touch the Flipper's hardware directly — the STM32WB owns the IR
hardware, the radios, the 1-Wire bus, and the GPIO. The host can only talk to what
the firmware exposes over USB, and the stock CLI/RPC surfaces do not expose raw
signal streams or arbitrary external buses. So the heart of the project is an
**on-device application** that drives the hardware and speaks one binary protocol,
with host-side tooling on top.

The prototype in `../mcp` proved this works end-to-end (dual-CDC USB, IR RX ISR
capture, IR TX) in C with a TypeScript host. This rewrite keeps the **proven wire
contract** and the **proven on-device structure**, and changes three things:

1. **All-Rust:** firmware via `flipperzero-rs`, shared protocol and host in Rust.
2. **Daemon-centric host:** a persistent daemon owns the device link; the CLI (and
   later MCP, and later exposers) are clients. This was chosen day-one because the
   device app stays running, re-doing the connect handshake per command is wasteful
   and fragile across the dual-CDC re-enumeration, and the eventual MCP server and
   the north-star exposers all need a long-lived connection owner anyway.
3. **One wire format end-to-end:** the same frame + CBOR contract runs over both the
   USB link (daemon ↔ device) and the unix socket (clients ↔ daemon).

## 3. The contract — `flip-proto`

A `no_std`, alloc-optional crate that is the single source of truth, compiled into
**both** the firmware and the host. Golden vectors are asserted by both a host Rust
test and an on-device test so the two sides can never silently drift.

### 3.1 Frame (unchanged from the proven prototype)

Little-endian:

```
offset  field    type   notes
0       magic    u16    0xF1A6  (sync marker; NOT covered by CRC)
2       version  u8     1
3       type     u8     MsgType
4       flags    u8     0 for now
5       seq      u16    request/response correlation
7       length   u32    payload byte count (≤ 0xFFFF; larger = framing error)
11      payload  []u8   `length` bytes
11+len  crc16    u16    CRC-16/CCITT-FALSE over bytes [2 .. 11+len)
```

CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, no reflection, xorout `0x0000`.
Header = 11 bytes, total frame = 13 + length. On bad magic/CRC the decoder drops one
byte and resyncs.

`MsgType`: `HELLO=1 CAPS=2 REQ=3 RESP=4 STREAM_START=5 STREAM_DATA=6 STREAM_STOP=7
EVENT=8 ERROR=9 PING=10 PONG=11`.

### 3.2 Message bodies

Control bodies use **CBOR** (see §3.4). `STREAM_DATA` payloads are **raw opaque
bytes** (e.g. IR = `int32` LE microsecond timings) — no CBOR overhead on the hot path;
`STREAM_START` declares the sample format.

```
HELLO  (client→device)  {} or {host_version:u}
CAPS   (device→client)  { protocol_version:u, instruments:[ {id:text, opcodes:[text]} ] }
REQ    (client→device)  { instrument:text, opcode:text, params:<map> }
RESP   (device→client)  { ok:bool, result:<map> }
ERROR  (device→client)  { code:u, message:text }
STREAM_START (device→client) { format:text }     e.g. "raw_int32_le_us"
STREAM_DATA  (raw payload, no CBOR)
STREAM_STOP  (client→device) {} ; (device→client) { dropped:u }

PROTOCOL_VERSION = 1
Error codes: 1=unknown_instrument 2=unknown_opcode 3=bad_params 4=busy
             5=oversized 6=internal
```

PING replies with PONG carrying the same seq and payload (the spike contract).

### 3.3 Streaming model — bidirectional (forward-looking)

Capture today is device→client only. To support the north-star bridges (a UART/
network bridge needs sustained client→device data), the contract reserves
**`STREAM_DATA` as legal in both directions** on an open session, correlated by the
session `seq`. This is **reserved now, implemented when the UART bridge is built** —
the IR slice uses only the device→client direction.

### 3.4 CBOR library

**`minicbor`** (`no_std`, no-alloc capable, `#[derive(Encode/Decode)]`) is the codec
on both sides. Note: the user's reference to "nanocbor" is a C library with no Rust
equivalent of that name; `minicbor` is the chosen Rust stand-in and gives us one
codec everywhere. CBOR profile on the wire: unsigned ints, text/byte strings,
booleans, arrays, maps with text-string keys. **No floats** (e.g. duty cycle travels
as integer permille).

## 4. Workspace layout

A single Cargo workspace. The firmware is a workspace member but builds on its own
embedded target (nightly, `thumbv7em-none-eabihf`, `build-std`, packaged to `.fap`
via the flipperzero-rs tooling), so a plain host `cargo build` does not build it.

```
flip-link/
  crates/
    flip-proto/    # no_std + alloc-optional. Frame codec + CBOR message types. THE contract.
    flip-core/     # std. Serial transport, device-link session/driver, reconnect. Reused by daemon (+ later MCP).
    flip-daemon/   # bin. Owns device link; unix-socket server; session manager; exposer seam.
    flip-cli/      # bin. `flip ...`; connects to daemon (auto-spawns it).
  firmware/        # flipperzero-rs FAP. Depends on flip-proto (no_std).
    instruments/   #   ir, then i2c / spi / uart — one module each.
  tests/golden/    # shared frame/CBOR golden vectors (asserted host + device)
  docs/superpowers/...
```

## 5. Firmware FAP (Rust)

Mirrors the proven C structure, in Rust:

- **Bootstrap / spike:** on launch switch to `usb_cdc_dual`, own interface 1, PING/PONG
  echo. Launched and exited on-device (Back exits, restores single-CDC); the daemon
  never launches or exits the app. **This is Slice 0 — nothing else is built until it
  round-trips against the host.**
- **Registry:** instrument-id + opcode dispatch table (the prototype's
  `AgentInstrument` / `AgentOpHandler` pattern); serializes the CAPS body.
- **Streaming engine:** app-owned capture lifecycle — ISR → stream-buffer → drain into
  `STREAM_DATA` frames, with backpressure/drop counting, ending on host `STREAM_STOP`
  or app exit.
- **Instruments:** `ir` first (RX ISR capture + TX), then `i2c` / `spi` / `uart`.
- Where `flipperzero-rs` lacks a safe wrapper (USB config, IR HAL, GPIO/bus HAL), go
  through `flipperzero-sys` unsafe bindings in a thin, isolated module per instrument.

## 6. `flip-daemon`

- **Device link (south):** owns the serial port via `flip-core`; HELLO/CAPS on connect;
  detects the single dual-CDC re-enumeration and reconnects; caches CAPS.
- **Client link (north):** unix domain socket (path under `$XDG_RUNTIME_DIR`, else a
  temp dir), same CBOR frames. Multiple clients; the daemon tags frames by client +
  `seq` and routes RESP/STREAM_* back to the originating client.
- **Session ownership:** a capture/bridge stream belongs to one client; a second
  capture REQ while busy returns `busy` (error code 4). STREAM_* routes only to the
  owning client.
- **Exposer seam (north-star hook):** an exposer is a daemon-internal component that
  owns a session and bridges it to an OS-native surface (TUN, PTY, …). Defined as a
  seam now; no exposers implemented in this spec.
- **Lifecycle:** the CLI **auto-spawns** the daemon on first use; `flip daemon
  {start,stop,status}` for explicit control. Single instance guarded by the socket.

## 7. `flip-cli`

```
flip status                          # daemon + device + caps summary
flip caps                            # instruments/opcodes (from cached CAPS)
flip ir capture [--auto-end] > f     # stream raw timings to stdout
flip ir transmit --file f
flip i2c scan                        # (Slice 2)
flip i2c read  --addr 0x.. --reg 0x.. --len N
flip i2c write --addr 0x.. --reg 0x.. --data ..
flip invoke <instrument> <opcode> [k=v ...]   # generic passthrough for any advertised cap
flip daemon {start,stop,status}
```

`invoke` makes a newly-advertised firmware instrument usable from the CLI immediately,
before it gets a bespoke subcommand.

## 8. Instrument model

Every instrument is `{ id, opcodes }`; every op is `REQ{instrument,opcode,params}` →
`RESP{ok,result}` or a stream. IR transmit and an I²C register write are the *same*
request shape — they differ only in params and which HAL the handler touches. That is
what lets I²C/SPI/UART (and later digital I/O and ADC) drop in as pure firmware
additions with no protocol, daemon, or transport changes. Bus ops are
request/response; IR capture (and a future UART monitor / network bridge) use the
streaming path.

## 9. Phasing

Each slice is independently shippable. **This spec's implementation plan covers
Slices 0–2;** Slices 3–4 are documented direction.

- **Slice 0 — Spike (go/no-go):** Rust FAP dual-CDC `PING`/`PONG` ↔ `flip-core` ↔ host
  test. Retires the `flipperzero-rs` feasibility risk before further investment.
- **Slice 1 — IR vertical:** registry + CAPS + IR capture/transmit + daemon +
  CLI. End-to-end parity with the `../mcp` prototype, in the new architecture.
- **Slice 2 — Buses:** I²C scan/transfer, then SPI, then UART bridge (UART bridge
  exercises bidirectional streaming, §3.3).
- **Slice 3 — MCP server (later):** new bin, thin client on the daemon socket.
- **Slice 4 — Exposers (north star):** TUN network interface fed by the UART bridge +
  cooperating ESP32 firmware; PTY exposer for serial devices.

## 10. Testing

- `flip-proto`: unit tests + golden vectors shared with the device.
- `flip-core` / daemon: mock transport (no hardware) covering session, multiplexing,
  ownership, and reconnect/re-enumeration logic.
- Hardware integration: gated behind an env flag (the prototype's `FLIPPER_HW=1`
  pattern); operator launches the FAP on-device.

## 11. Risks / open questions

- **`flipperzero-rs` depth** — the biggest risk (USB dual-CDC config, IR RX ISR
  callbacks, bus HAL). Slice 0 retires it before bulk investment.
- **Daemon ↔ re-enumeration races** — concentrated in `flip-core`, covered by
  mock-transport tests.
- **Firmware build inside a host workspace** — the nightly / `.fap` toolchain must be
  wired so host `cargo build`/test ignore the FAP target.
- **North-star throughput** — the GPIO-UART bottleneck caps the WiFi-bridge ambition;
  flagged early so expectations are set before Slice 4 is scoped.
