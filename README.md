# flip-link

Low-level, all-Rust toolkit for driving Flipper Zero instruments from a host CLI.
See [`docs/superpowers/specs/`](docs/superpowers/specs/) for the architecture and
[`docs/superpowers/plans/`](docs/superpowers/plans/) for implementation plans.

- `crates/flip-proto` — the wire contract (frame codec; `no_std`, shared by host + firmware)
- `crates/flip-core`  — serial transport + device link
- `crates/flip-daemon`— owns the device link, relays frames over a unix socket
- `crates/flip-client` — reusable host API for daemon-backed capabilities, invoke, and IR helpers
- `crates/flip-cli`   — the `flip` CLI (the **primary testing harness**)
- `firmware/`         — the on-device FAP (flipperzero-rs; a standalone package excluded
  from the host workspace because it builds for the `thumbv7em-none-eabihf` target)

## Status: Slice 0 — dual-CDC walking skeleton ✅ (verified on hardware)

`flip status` round-trips PONG host → daemon → device → firmware on a Flipper Zero
(fw 1.4.3 / API 87.1). The FAP switches USB to dual-CDC, shows a status screen, and
exits on Back.

- Host: `cargo build` (produces `flip` + `flip-daemon`), `cargo test` (11 tests).
- Firmware: builds to `firmware/target/thumbv7em-none-eabihf/release/flip_link.fap`.

> Known limitation (Slice 1): the daemon opens the serial port once with no reconnect.
> After any Flipper reboot/relaunch the FAP re-enumerates onto a new port, so restart the
> daemon (`just daemon-stop`, or use `just reflash` / `just bench`).

## Tasks

Common workflows are wrapped in a [`justfile`](justfile) (`brew install just`); run
`just` to list them. Key recipes: `just build`, `just test`, `just check`,
`just fw-build`, `just fw-run` (build+flash+launch the FAP), `just status`,
`just hw-test`, and `just bench` (flash then status).

## Proving the link (Slice 0 acceptance)

Requires a Flipper Zero connected over USB.

1. Build the firmware: `just fw-build`
2. Flash + launch it on the device (it switches USB to dual-CDC and re-enumerates
   once — that brief port drop is expected): `just fw-run`
   (`run-fap` comes from `flipperzero-tools`: `cargo install --locked flipperzero-tools`.)
3. From the repo root, exercise the full CLI → daemon → device path: `just status`
   Expected:
   ```
   daemon: up
   device: reachable (PONG round-trip ok)
   echo:   "flip-status"
   ```
   Or do steps 2–3 in one go with `just bench`.

The spike firmware time-boxes itself (~5 minutes idle) and restores the USB config on
exit. If port auto-discovery is ambiguous, set `FLIP_PORT` to the agent port, e.g.
`FLIP_PORT=$(ls /dev/cu.usbmodemflip_* | tail -1) ./target/debug/flip status`.

## Capabilities & invoke (Slice 1a)

With the FAP running and the daemon up, the CBOR control plane gives capability
discovery and generic opcode invocation:

```sh
flip caps                          # list instruments/opcodes
flip invoke sys version            # -> {protocol: 1, fw: flip-link 0.1}
flip invoke sys echo msg=hi n=5    # -> {msg: hi, n: 5}
```

`sys` is a hardware-free test instrument. The daemon reconnects automatically when the
Flipper re-enumerates (reboot / relaunch), retrying a few times then idling until the next
command. It runs quietly — its logs go to a file, not your terminal. Use `flip daemon
status` (or `just daemon-status`) to check it, and `just daemon-log` to tail its log.

## IR capture & transmit

Capture a remote, then replay it — the round-trip:

```sh
flip caps                                            # `ir` lists `ir.transmit` + `ir.capture`
flip ir capture --auto-end 400 --output remote.txt   # press a remote button at the Flipper
flip ir transmit --file remote.txt                   # replay at the target device
```

`ir capture` streams raw timings until **Ctrl-C**, or `--auto-end <ms>` of silence (good for
one button press). The saved output is a full IR signal record, not just bare timings:
`# freq=<Hz>` and `# duty=<permille>` directive lines followed by µs timings.
`--output` writes a file (default: stdout), and `ir transmit --file` consumes the record so
capture→transmit round-trips. Old whitespace timing files with normal `#` comments still
work; without directive lines they default to 38000 Hz / 330 permille. The
`flip ir transmit --freq` and `--duty` flags are optional overrides for the file/header
values.

## Development

```sh
cargo test                 # host unit/integration tests (hardware-free)
FLIPPER_HW=1 cargo test -p flip-core --test hw_ping -- --nocapture   # [HW] go/no-go
```
