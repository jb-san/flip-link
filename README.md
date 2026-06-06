# flip-link

Low-level, all-Rust toolkit for driving Flipper Zero instruments from a host CLI.
See [`docs/superpowers/specs/`](docs/superpowers/specs/) for the architecture and
[`docs/superpowers/plans/`](docs/superpowers/plans/) for implementation plans.

- `crates/flip-proto` — the wire contract (frame codec; `no_std`, shared by host + firmware)
- `crates/flip-core`  — serial transport + device link
- `crates/flip-daemon`— owns the device link, relays frames over a unix socket
- `crates/flip-cli`   — the `flip` CLI (the **primary testing harness**)
- `firmware/`         — the on-device FAP (flipperzero-rs; a standalone package excluded
  from the host workspace because it builds for the `thumbv7em-none-eabihf` target)

## Status: Slice 0 — dual-CDC walking skeleton

The host stack and firmware are implemented and build; the on-device PING/PONG
round-trip is the remaining hardware acceptance step.

- Host: `cargo build` (produces `flip` + `flip-daemon`), `cargo test` (11 tests).
- Firmware: builds to `firmware/target/thumbv7em-none-eabihf/release/flip_link.fap`.

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

## Development

```sh
cargo test                 # host unit/integration tests (hardware-free)
FLIPPER_HW=1 cargo test -p flip-core --test hw_ping -- --nocapture   # [HW] go/no-go
```
