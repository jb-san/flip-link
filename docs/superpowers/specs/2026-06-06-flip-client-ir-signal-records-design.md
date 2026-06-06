# flip-client Library + IR Signal Records (Layer 1) — Design

**Date:** 2026-06-06
**Status:** Approved design, pending implementation plan
**Context:** Refines Slice 1c. The raw IR capture/transmit work but a captured file
(bare µs timings) isn't faithfully replayable, and the client logic that the future
MCP server will need is trapped in the `flip-cli` binary.

## 1. Problem

Two issues surfaced when replaying a captured IR signal:

1. **A captured signal isn't a complete record.** Replay needs the **carrier
   frequency** and **duty cycle**, and the first captured sample is the pre-signal
   idle gap (time since RX was armed), not signal — transmitting it as a long mark
   corrupts the whole mark/space alignment. The carrier frequency is **not
   measurable** from raw RX (the Flipper's IR receiver is a fixed ~38 kHz
   demodulator that strips the carrier and outputs only the mark/space envelope),
   so it must be a recorded, editable field — defaulted, not measured.

2. **Client logic lives in the CLI binary.** `connect`/`caps`/`invoke`/capture-stream/
   transmit and the IR signal handling are in `flip-cli`. The planned MCP server is
   another daemon client (it talks to the same socket), so leaving this in the CLI
   forces the MCP to duplicate it.

This design fixes both: a faithful **IR signal record** (Layer 1), implemented in a
new shared **`flip-client`** library that both the CLI and the future MCP consume.

> Layer 2 (arbitrary **byte** data over IR — a line code + framing on top of these
> signal records) is explicitly out of scope here; this is the foundation it builds on.

## 2. Crate structure

New `crates/flip-client/` (library), depending on `flip-proto` (with `alloc`) and
`flip-core`. It absorbs the daemon-client code currently in `flip-cli`.

```
crates/flip-client/
  src/lib.rs       # typed client API: status/caps/invoke/ir_capture/ir_transmit
  src/daemon.rs    # daemon socket: connect (+ auto-spawn + log redirect),
                   #   try_connect, StreamConn (framed read/write), status query
  src/signal.rs    # IrSignal type + file-format parse/write + leading-idle trim
crates/flip-cli/
  src/main.rs      # thin clap frontend -> flip-client; wires Ctrl-C -> capture cancel
  src/kv.rs        # stays: parse `k=v` clap args -> Value (CLI-specific)
```

`flip-core` keeps its role (the **device-link** driver the daemon uses);
`flip-client` is the **client-side** API (the CLI and MCP use it). Both rest on
`flip-proto`. The daemon is unchanged.

**Cancellation seam:** `ir_capture` takes `cancel: impl Fn() -> bool` rather than
owning signal handling, so the CLI wires SIGINT (`ctrlc`) and the future MCP wires
its own cancellation token — no signal handling in the library.

## 3. `flip-client` API

```rust
// daemon.rs — connection management (moved from cli/client.rs, unchanged behavior)
pub fn connect() -> Result<UnixStream>;        // auto-spawns daemon (stdio -> log file)
pub fn try_connect() -> Option<UnixStream>;     // no spawn (for status)
pub struct StreamConn { /* owns one socket; send()/next_frame() */ }
pub fn open_stream(instrument, opcode, params: Value) -> Result<StreamConn>;

// lib.rs — typed operations
pub fn status() -> DaemonStatus;                       // running? device connected?
pub fn caps(timeout) -> Result<Caps>;
pub fn invoke(instrument, opcode, params: Value, timeout) -> Result<Resp>;
pub fn ir_transmit(signal: &IrSignal, timeout) -> Result<u64>;   // -> edges sent
pub fn ir_capture(auto_end: Option<Duration>, cancel: &dyn Fn() -> bool)
        -> Result<IrSignal>;                           // streams, trims, returns signal
```

`ir_capture` runs the capture stream (the existing `StreamConn` + STREAM_DATA
decode), accumulates timings, ends on `cancel()` or an `auto_end` silence gap, sends
`STREAM_STOP`, drains the final frames, and returns a trimmed `IrSignal`.
`ir_transmit` builds the REQ params from the signal's `frequency`/`duty_permille`/
`timings` and invokes `ir.transmit`.

## 4. IR signal record (Layer 1)

```rust
// signal.rs
pub struct IrSignal {
    pub frequency: u32,      // carrier Hz (default 38000 — assumed, not measured)
    pub duty_permille: u32,  // duty cycle in permille (default 330)
    pub timings: Vec<u64>,   // µs durations, mark first
}
```

- **File format** (flip-link's own; backward-compatible with the existing timings
  files): directive comments carry the carrier, the rest is whitespace/newline µs
  integers.
  ```
  # freq=38000      carrier in Hz (assumed; edit to match a known source/target)
  # duty=330        duty cycle in permille
  9000 4500 560 560 1690 …
  ```
  Parsing: a `#` line matching `freq=<u32>` or `duty=<u32>` sets that field; any
  other `#` line is a plain comment; remaining tokens are µs timings. Absent
  directives default to `frequency=38000`, `duty_permille=330`. Writing emits the
  two directive lines then 12 timings per line.

- **`IrSignal::from_capture(raw: Vec<u64>) -> IrSignal`** — drops `raw[0]` (the
  leading idle); `frequency=38000`, `duty_permille=330`. If `raw` is empty, the
  result has no timings (caller treats as "no signal captured").

- **`IrSignal::parse(&str) -> Result<IrSignal>`**, **`to_file_string(&self) -> String`**,
  and convenience **`read_file(path)`/`write_file(path)`**.

- **`IrSignal::to_transmit_params(&self) -> Value`** — `{frequency, duty_permille,
  timings:[…]}` for `ir.transmit`.

The carrier defaulting to 38000 documents the assumption and is the value the
Flipper's own receiver is tuned to; to reach a non-38 kHz target the user edits the
header or passes `--freq` (which overrides before `ir_transmit`).

## 5. CLI (thin frontend; UX unchanged)

- `flip ir capture [--auto-end <ms>] [--output <file>]`: install a Ctrl-C flag, call
  `flip_client::ir_capture(auto_end, &|| flag.load())`, then `signal.write_file` (or
  print `to_file_string` to stdout). Reports `captured N timings -> <file>` and any
  `dropped` warning.
- `flip ir transmit --file <f> [--freq <Hz>] [--duty <permille>]`: `IrSignal::read_file`,
  apply `--freq`/`--duty` overrides if present, `flip_client::ir_transmit`.
- `flip caps` / `flip invoke` / `flip status` / `flip daemon status`: delegate to the
  matching `flip-client` functions.

## 6. Migration

This is a move + thin, not a rewrite: `cli/client.rs` → `flip-client/daemon.rs` (+
the typed ops to `lib.rs`); `cli/ir.rs` capture/format/transmit-params logic →
`flip-client/signal.rs`; `cli/capture.rs` stream loop → `flip-client::ir_capture`.
`flip-cli` keeps `main.rs` (clap) + `kv.rs` and gains the Ctrl-C wiring. Behavior is
unchanged except the Layer-1 additions (freq/duty in the file + leading-idle trim).

## 7. Testing

- `flip-client` unit tests (hardware-free): `IrSignal` format round-trip
  (parse↔write), directive parsing (with/without `# freq=`/`# duty=`, defaults),
  `from_capture` leading-idle trim, `to_transmit_params`.
- Daemon-client ops (`caps`/`invoke`/`ir_capture`/`ir_transmit`) remain
  integration-tested via the CLI against hardware (the established `FLIPPER_HW`
  pattern); the capture round-trip is the acceptance: capture a signal, replay it,
  confirm faithful reproduction (and, for a 38 kHz source, that it triggers the
  original device).

## 8. Out of scope / future

- **Layer 2 — byte data link** (line code + framing for arbitrary bytes over IR,
  to any peer running the codec) builds on `IrSignal`/`ir_transmit`/`ir_capture`.
- **Protocol decoding** (NEC/RC5/…): deliberately not done — flip-link targets raw,
  arbitrary signals, not consumer-IR protocol cloning.
- The MCP server will depend on `flip-client` and expose its operations as tools.
