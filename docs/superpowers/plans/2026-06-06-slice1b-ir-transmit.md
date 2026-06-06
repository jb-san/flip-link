# flip-link Slice 1b — IR Transmit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the first real hardware instrument — IR transmit — as a request/response opcode (`ir.transmit`), and grow the firmware's receive accumulator to the heap so large timing arrays fit.

**Architecture:** Reuses the Slice 1a control plane unchanged. The firmware's fixed 1 KB stack receive-accumulator becomes a heap `Vec<u8>` (with an overflow guard), so a `REQ` carrying hundreds of IR timings fits. A new `ir` instrument's `transmit` handler parses `{frequency, duty_permille, timings:[…]}` from the request `Value`, drives `furi_hal_infrared_async_tx_*` via a get-data ISR callback (timings published through atomics), and blocks on `wait_termination` before replying `RESP ok`. The CLI gains `flip ir transmit --file <timings>`.

**Tech Stack:** Rust; `flipperzero-sys` 0.16 IR HAL (verified present in fw 1.4.3 API table); the recursive `Value` from Slice 1a carries the timings array. No streaming (that's Slice 1c / capture).

**Scope:** IR **transmit** only (request/response). IR **capture** + the streaming engine are Slice 1c. Builds on Slice 1a (verified on hardware).

**Hardware note:** Steps tagged **[HW]** need a Flipper running the FAP and an IR target (a device with an IR receiver) to observe transmission. Everything else is hardware-free.

---

## File Structure

```
firmware/src/main.rs            # receive accumulator: fixed [u8;1024] -> heap Vec<u8> + overflow guard
firmware/src/ir_instrument.rs   # NEW: ir.transmit handler + TX get-data ISR callback
firmware/src/registry.rs        # register the `ir` instrument (transmit opcode)
crates/flip-cli/src/main.rs     # `flip ir transmit` subcommand
crates/flip-cli/src/ir.rs       # NEW: parse a timings file -> Value params; invoke ir.transmit
```

---

## Task 1: firmware — heap receive accumulator

The current main loop accumulates incoming bytes in a fixed `acc: [u8; ACC_CAP=1024]` on the stack. An IR `transmit` REQ with a timings array easily exceeds 1 KB → the frame would be truncated, fail CRC, and wedge the resync loop. Move the accumulator to the heap (`Vec<u8>`) with an overflow guard. This also removes 1 KB from the FAP stack and fixes the deferred "accumulator wedge on oversized frame" finding.

**Files:**
- Modify: `firmware/src/main.rs`

- [ ] **Step 1: Read the current drain loop**

Read `firmware/src/main.rs`. Locate (inside `fn main`) the declarations:
```rust
    let mut acc = [0u8; ACC_CAP];
    let mut acc_len: usize = 0;
    let mut chunk = [0u8; CHUNK];
    let mut enc = [0u8; ENC_CAP];
```
and the receive+drain block that begins with `let got = unsafe { sys::furi_stream_buffer_receive(...) };` through the end of the inner `loop { ... }` that decodes frames.

- [ ] **Step 2: Add a MAX_FRAME const**

Near the other consts (after `const MAX_IDLE: u32 = 15_000;`), add:
```rust
/// Largest accepted inbound frame (header+payload+crc). Guards the heap
/// accumulator: anything bigger is treated as garbage and dropped.
const MAX_FRAME: usize = 16 * 1024;
```

- [ ] **Step 3: Replace the accumulator declarations**

Replace:
```rust
    let mut acc = [0u8; ACC_CAP];
    let mut acc_len: usize = 0;
    let mut chunk = [0u8; CHUNK];
    let mut enc = [0u8; ENC_CAP];
```
with:
```rust
    let mut acc: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut chunk = [0u8; CHUNK];
    let mut enc = [0u8; ENC_CAP];
```

- [ ] **Step 4: Replace the receive+drain block**

Replace the entire block that currently reads:
```rust
        if got == 0 {
            idle += 1;
            continue;
        }
        idle = 0;

        // Append to the accumulator (drop overflow defensively).
        let space = ACC_CAP - acc_len;
        let n = core::cmp::min(space, got);
        acc[acc_len..acc_len + n].copy_from_slice(&chunk[..n]);
        acc_len += n;

        // Drain whole frames, echoing PONG for each PING.
        loop {
            let used;
            let mut send_len = 0usize;
            match decode(&acc[..acc_len]) {
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
                DecodeResult::NeedMore => break,
                DecodeResult::Resync => {
                    if acc_len == 0 {
                        break;
                    }
                    acc.copy_within(1..acc_len, 0);
                    acc_len -= 1;
                    continue;
                }
            }
            if send_len > 0 {
                cdc_send_all(&enc[..send_len]);
            }
            acc.copy_within(used..acc_len, 0);
            acc_len -= used;
        }
```
with:
```rust
        if got == 0 {
            idle += 1;
            continue;
        }
        idle = 0;

        // Append to the heap accumulator; drop everything if it grows past a
        // sane max (garbage/oversized frame) so the resync loop can't wedge.
        acc.extend_from_slice(&chunk[..got]);
        if acc.len() > MAX_FRAME {
            acc.clear();
            continue;
        }

        // Drain whole frames, echoing PONG for each PING.
        loop {
            let used;
            let mut send_len = 0usize;
            match decode(&acc) {
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
                DecodeResult::NeedMore => break,
                DecodeResult::Resync => {
                    if acc.is_empty() {
                        break;
                    }
                    acc.drain(0..1);
                    continue;
                }
            }
            if send_len > 0 {
                cdc_send_all(&enc[..send_len]);
            }
            acc.drain(0..used);
        }
```
Note: `ACC_CAP` is now unused — remove the `const ACC_CAP: usize = 1024;` line (or leave it; if removed, ensure nothing else references it). Removing it avoids a dead-const warning.

- [ ] **Step 5: Build**

Run: `cd firmware && cargo build --release`
Expected: builds `flip_link.fap`. The borrow pattern is unchanged from the working version (decode borrows `acc`; the PING `enc` write and `handle_frame` run before `acc.drain`, so the borrow ends first).

- [ ] **Step 6: Commit**

```bash
git add firmware/src/main.rs
git commit -m "feat(fw): heap receive accumulator (fits large IR frames; fixes wedge)"
```

---

## Task 2: firmware — `ir.transmit` handler

**Files:**
- Create: `firmware/src/ir_instrument.rs`

- [ ] **Step 1: Write the transmit handler + get-data ISR callback**

`firmware/src/ir_instrument.rs`:
```rust
//! IR instrument — transmit only (Slice 1b). Capture is Slice 1c.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use flip_proto::messages::{ERR_BAD_PARAMS, ERR_BUSY, ERR_OVERSIZED};
use flip_proto::Value;
use flipperzero_sys as sys;

const DEFAULT_FREQ: u32 = 38_000;
const DEFAULT_DUTY_PERMILLE: u32 = 330;
const MAX_EDGES: usize = 1024;

/// Timings shared with the TX get-data ISR. Valid only between
/// async_tx_start and wait_termination; the owning Vec lives on the handler
/// stack for that whole window.
static TX_PTR: AtomicPtr<u32> = AtomicPtr::new(core::ptr::null_mut());
static TX_LEN: AtomicUsize = AtomicUsize::new(0);
static TX_POS: AtomicUsize = AtomicUsize::new(0);

/// IR TX get-data ISR: feed the next edge. Odd/even position = mark/space
/// (first timing is a mark, LED on).
unsafe extern "C" fn tx_get_data(
    _ctx: *mut core::ffi::c_void,
    duration: *mut u32,
    level: *mut bool,
) -> sys::FuriHalInfraredTxGetDataState {
    let ptr = TX_PTR.load(Ordering::Acquire);
    let len = TX_LEN.load(Ordering::Acquire);
    let pos = TX_POS.load(Ordering::Acquire);
    if ptr.is_null() || pos >= len {
        return sys::FuriHalInfraredTxGetDataStateLastDone;
    }
    unsafe {
        *duration = *ptr.add(pos);
        *level = (pos % 2) == 0;
    }
    TX_POS.store(pos + 1, Ordering::Release);
    if pos + 1 >= len {
        sys::FuriHalInfraredTxGetDataStateLastDone
    } else {
        sys::FuriHalInfraredTxGetDataStateOk
    }
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::U64(n) => Some(*n),
        Value::I64(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

/// `ir.transmit` — params { frequency?:u, duty_permille?:u, timings:[u,...] }.
pub fn transmit(params: &Value) -> Result<Value, (u32, String)> {
    let freq = params
        .get("frequency")
        .and_then(as_u64)
        .unwrap_or(DEFAULT_FREQ as u64) as u32;
    let duty_permille = params
        .get("duty_permille")
        .and_then(as_u64)
        .unwrap_or(DEFAULT_DUTY_PERMILLE as u64) as u32;

    let timings: Vec<u32> = match params.get("timings") {
        Some(Value::Array(a)) => a.iter().filter_map(as_u64).map(|v| v as u32).collect(),
        _ => return Err((ERR_BAD_PARAMS, "timings array required".to_string())),
    };
    if timings.is_empty() {
        return Err((ERR_BAD_PARAMS, "timings empty".to_string()));
    }
    if timings.len() > MAX_EDGES {
        return Err((ERR_OVERSIZED, "too many timings".to_string()));
    }
    if unsafe { sys::furi_hal_infrared_is_busy() } {
        return Err((ERR_BUSY, "ir busy".to_string()));
    }

    // Publish timings to the ISR, then drive the async transmission. `timings`
    // stays alive on this stack frame until wait_termination returns.
    TX_PTR.store(timings.as_ptr() as *mut u32, Ordering::Release);
    TX_LEN.store(timings.len(), Ordering::Release);
    TX_POS.store(0, Ordering::Release);

    let duty = duty_permille as f32 / 1000.0;
    unsafe {
        sys::furi_hal_infrared_async_tx_set_data_isr_callback(
            Some(tx_get_data),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_tx_start(freq, duty);
        sys::furi_hal_infrared_async_tx_wait_termination();
        sys::furi_hal_infrared_async_tx_stop();
    }
    TX_PTR.store(core::ptr::null_mut(), Ordering::Release);

    Ok(Value::Map(vec![(
        "sent".to_string(),
        Value::U64(timings.len() as u64),
    )]))
}
```

- [ ] **Step 2: Declare the module + build**

Add `mod ir_instrument;` to `firmware/src/main.rs` next to `mod registry;` / `mod sys_instrument;`. Then:
Run: `cd firmware && cargo build --release`
Expected: compiles (the handler is registered in Task 3; an unused warning until then is fine). If `FuriHalInfraredTxGetDataState`/state constants or the IR fns aren't found, confirm the exact sys paths (`sys::furi_hal_infrared_async_tx_*`, `sys::FuriHalInfraredTxGetDataStateOk/LastDone`) — they are verified present in flipperzero-sys 0.16.

- [ ] **Step 3: Commit**

```bash
git add firmware/src/ir_instrument.rs firmware/src/main.rs
git commit -m "feat(fw): ir.transmit handler (async TX via get-data ISR)"
```

---

## Task 3: firmware — register the `ir` instrument

**Files:**
- Modify: `firmware/src/registry.rs`

- [ ] **Step 1: Add the ir instrument to the table**

In `firmware/src/registry.rs`, add an opcode table and instrument entry. After the existing `static SYS_OPCODES: &[OpcodeEntry] = ...;` block, add:
```rust
static IR_OPCODES: &[OpcodeEntry] = &[OpcodeEntry {
    opcode: "transmit",
    handler: crate::ir_instrument::transmit,
}];
```
Then change the `INSTRUMENTS` table from:
```rust
static INSTRUMENTS: &[InstrumentEntry] = &[InstrumentEntry {
    id: "sys",
    opcodes: SYS_OPCODES,
}];
```
to:
```rust
static INSTRUMENTS: &[InstrumentEntry] = &[
    InstrumentEntry {
        id: "sys",
        opcodes: SYS_OPCODES,
    },
    InstrumentEntry {
        id: "ir",
        opcodes: IR_OPCODES,
    },
];
```
Note: `OpcodeEntry.handler` has type `sys_instrument::Handler = fn(&Value) -> Result<Value, (u32, String)>`. `crate::ir_instrument::transmit` has exactly that signature, so it fits the same table.

- [ ] **Step 2: Build**

Run: `cd firmware && cargo build --release`
Expected: builds `flip_link.fap` with no unused-warning for `ir_instrument` (now referenced).

- [ ] **Step 3: Commit**

```bash
git add firmware/src/registry.rs
git commit -m "feat(fw): register ir instrument (transmit)"
```

---

## Task 4: CLI — `flip ir transmit --file <timings>`

**Files:**
- Create: `crates/flip-cli/src/ir.rs`
- Modify: `crates/flip-cli/src/main.rs`

- [ ] **Step 1: Write the timings-file parser + params builder with tests**

`crates/flip-cli/src/ir.rs`:
```rust
//! `flip ir transmit` helpers: parse a timings file into REQ params.

use anyhow::{anyhow, Context, Result};
use flip_proto::Value;

/// Parse whitespace/newline-separated unsigned integers (microsecond timings).
/// Lines starting with `#` are comments. Returns the timings as a Value::Array.
pub fn parse_timings(text: &str) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for tok in text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.split_whitespace())
    {
        let v: u64 = tok
            .parse()
            .map_err(|_| anyhow!("invalid timing '{tok}' (expected unsigned integer µs)"))?;
        out.push(v);
    }
    if out.is_empty() {
        return Err(anyhow!("no timings found"));
    }
    Ok(out)
}

/// Build the `ir.transmit` params Value from timings + carrier settings.
pub fn transmit_params(timings: Vec<u64>, frequency: u64, duty_permille: u64) -> Value {
    Value::Map(vec![
        ("frequency".to_string(), Value::U64(frequency)),
        ("duty_permille".to_string(), Value::U64(duty_permille)),
        (
            "timings".to_string(),
            Value::Array(timings.into_iter().map(Value::U64).collect()),
        ),
    ])
}

/// Read + parse a timings file path.
pub fn load_timings_file(path: &str) -> Result<Vec<u64>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    parse_timings(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timings_with_comments_and_whitespace() {
        let text = "# an SOS\n9000 4500\n560 560\n560\n";
        assert_eq!(parse_timings(text).unwrap(), vec![9000, 4500, 560, 560, 560]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_timings("hello").is_err());
        assert!(parse_timings("# only a comment").is_err());
    }

    #[test]
    fn builds_params() {
        let p = transmit_params(vec![560, 560], 38000, 330);
        assert_eq!(p.get("frequency"), Some(&Value::U64(38000)));
        assert_eq!(
            p.get("timings"),
            Some(&Value::Array(vec![Value::U64(560), Value::U64(560)]))
        );
    }
}
```

- [ ] **Step 2: Add the `ir` subcommand to main.rs**

In `crates/flip-cli/src/main.rs`, add the module declaration near the others:
```rust
mod ir;
```
Add to the `Cmd` enum:
```rust
    /// IR instrument commands.
    Ir {
        #[command(subcommand)]
        cmd: IrCmd,
    },
```
Add a new subcommand enum (below the `Cmd` enum):
```rust
#[derive(Subcommand)]
enum IrCmd {
    /// Transmit IR timings from a file.
    Transmit {
        /// Path to a file of whitespace/newline-separated µs timings.
        #[arg(long)]
        file: String,
        /// Carrier frequency in Hz.
        #[arg(long, default_value_t = 38000)]
        freq: u64,
        /// Duty cycle in permille (e.g. 330 = 33%).
        #[arg(long, default_value_t = 330)]
        duty: u64,
    },
}
```
Add the match arm in `main`:
```rust
        Cmd::Ir { cmd } => match cmd {
            IrCmd::Transmit { file, freq, duty } => {
                let timings = ir::load_timings_file(&file)?;
                let count = timings.len();
                let params = ir::transmit_params(timings, freq, duty);
                let resp = client::invoke("ir", "transmit", params, Duration::from_secs(10))?;
                println!("transmitted {count} edges: {}", client::render_value(&resp.result));
                Ok(())
            }
        },
```
(`Subcommand` is already imported from clap in main.rs.)

- [ ] **Step 3: Build + test**

Run: `cargo build && cargo test`
Expected: whole workspace builds; `flip` binary builds; the new `ir::tests` (`parses_timings_with_comments_and_whitespace`, `rejects_garbage`, `builds_params`) pass.

- [ ] **Step 4: Commit**

```bash
git add crates/flip-cli/src/ir.rs crates/flip-cli/src/main.rs
git commit -m "feat(cli): flip ir transmit --file"
```

---

## Task 5: [HW] Transmit acceptance + sample file

**Files:**
- Create: `crates/flip-cli/examples/sos.txt`
- Modify: `README.md`

- [ ] **Step 1: Create a sample timings file**

`crates/flip-cli/examples/sos.txt`:
```
# A short on/off burst pattern (µs): mark, space, mark, space, ...
# Not a real protocol — a visible/measurable test pattern.
100000 100000 100000 100000 100000 100000
```

- [ ] **Step 2 [HW]: Transmit**

Flash + launch the FAP, then:
```bash
just reflash        # daemon-stop + build + upload
# launch flip-link on the Flipper, then:
./target/debug/flip caps          # ir now appears with transmit
./target/debug/flip ir transmit --file crates/flip-cli/examples/sos.txt
```
Expected:
```
# caps now shows:
ir
  ir.transmit
# transmit:
transmitted 6 edges: {sent: 6}
```
To verify real emission, point the Flipper's IR LED at a phone camera (the LED pulses are visible on many camera sensors) during the transmit, or transmit a real captured remote code (available once Slice 1c capture lands) at the target device. The `RESP {sent: N}` confirms the firmware completed the async TX without error.

- [ ] **Step 3: Document IR transmit in the README**

Add to `README.md` after the "Capabilities & invoke" section:
```markdown
## IR transmit (Slice 1b)

```sh
flip caps                                              # `ir` now lists `ir.transmit`
flip ir transmit --file crates/flip-cli/examples/sos.txt
flip ir transmit --file remote.txt --freq 38000 --duty 330
```

The timings file is whitespace/newline-separated microsecond durations (mark, space,
mark, …); `#` lines are comments. IR capture (which produces these files) is Slice 1c.
```

- [ ] **Step 4: Commit**

```bash
git add crates/flip-cli/examples/sos.txt README.md
git commit -m "docs: IR transmit (Slice 1b) + sample timings file"
```

---

## Self-Review Notes (for the implementer)

- **Firmware stack discipline (learned in 1a bring-up):** the receive accumulator is now a heap `Vec<u8>`, not a stack array; `send_msg` already heap-allocates; the stack is 8 KB. Keep large buffers off the FAP stack.
- **The IR TX timings are shared with an ISR** via `TX_PTR`/`TX_LEN`/`TX_POS` atomics. The owning `Vec<u32>` lives on the `transmit` handler's stack across `async_tx_start`→`wait_termination` (which blocks until the signal finishes), so the pointer stays valid; `TX_PTR` is nulled before the Vec drops. Single transmission at a time (the main loop is synchronous; no concurrent capture in 1b).
- **`duty_cycle` is `f32` only at the HAL boundary** (`duty_permille as f32 / 1000.0`); the wire stays integer permille per the spec. The STM32WB55 has an FPU.
- **`ir.transmit` reuses the registry `Handler` type** (`fn(&Value) -> Result<Value,(u32,String)>`) — no new dispatch machinery; it's just another opcode beside `sys.*`.
- **Type consistency:** `ir_instrument::transmit`, `registry` IR_OPCODES/INSTRUMENTS, `ir::{parse_timings,transmit_params,load_timings_file}`, `client::invoke`/`render_value` are used consistently.
- **Not in scope (Slice 1c):** IR capture, the streaming engine (STREAM_*), daemon stream routing. `ir.transmit` is strictly request/response.
- **Carryover still deferred to 1c:** the `send_msg` >1087 B silent-drop (a transmit RESP is tiny, so not hit here); revisit when capture STREAM_DATA framing lands.
