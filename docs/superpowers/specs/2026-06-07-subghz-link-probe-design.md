# Sub-GHz Link Probe Design

**Date:** 2026-06-07
**Status:** Design for a single-device spike
**Context:** Raw Sub-GHz capture/transmit is committed. The next goal is
arbitrary byte transfer over Sub-GHz. Only one Flipper Zero is currently
available, so this slice must validate SDK integration and device stability
without claiming over-the-air byte delivery.

## 1. Goal

Prove that flip-link can start Flipper's byte-oriented Sub-GHz TX/RX worker from
the FAP, write bounded byte payloads to it, keep the device stable, and expose
diagnostics through the CLI. This is a prerequisite for later `send`/`recv`
commands that transfer caller-provided files between two Flippers.

## 2. Non-goals

- Do not claim RF byte transfer works end-to-end with one Flipper.
- Do not build a custom raw-timing modem in this slice.
- Do not bypass regional frequency validation.
- Do not add daemon host-to-device streaming yet.
- Do not promise reliability, retransmission, or file transfer semantics.

## 3. SDK finding

The SDK exports `subghz_tx_rx_worker_*` through both the local UFBT headers and
the Rust bindings:

- `subghz_tx_rx_worker_alloc`
- `subghz_tx_rx_worker_start(worker, device, frequency)`
- `subghz_tx_rx_worker_write(worker, data, size)`
- `subghz_tx_rx_worker_available`
- `subghz_tx_rx_worker_read`
- `subghz_tx_rx_worker_set_callback_have_read`
- `subghz_tx_rx_worker_stop`
- `subghz_tx_rx_worker_free`

The worker start function takes a radio device and frequency, but no explicit
preset. The spike should treat the worker as owning its packet-mode radio
configuration. The existing raw Sub-GHz path still owns explicit preset-based
timing capture/transmit.

## 4. CLI behavior

Add a diagnostic command:

```sh
flip subghz link-probe --freq 433920000 --data hello
```

Optional controls:

- `--hex <hex>` sends bytes specified as hex instead of UTF-8 text.
- `--timeout <ms>` bounds how long firmware waits after writing before stopping
  the worker.

Rules:

- Exactly one of `--data` or `--hex` is required.
- Payload length is capped to a conservative small value for the probe, initially
  64 bytes.
- Frequency is required and must pass firmware validation.

Expected success output:

```text
link probe wrote 5 bytes; read 0 bytes
```

If future hardware or loopback behavior produces received bytes, print the
received byte count and hex preview. With one Flipper, zero received bytes is
not a failure. The probe succeeds if the worker starts, accepts the write, stops
cleanly, and the device remains reachable.

## 5. Firmware behavior

Add a one-shot `subghz.link_probe` opcode.

Firmware flow:

1. Validate `frequency` and `payload`.
2. Refuse while raw Sub-GHz capture or transmit is active.
3. Initialize and begin the internal CC1101 device with
   `subghz_devices_get_by_name("cc1101_int")`.
4. Allocate `SubGhzTxRxWorker`.
5. Register a read callback that increments a counter. The main request path
   should still poll/read after write so it does not rely on callback timing.
6. Start the worker with the selected frequency.
7. Call `subghz_tx_rx_worker_write`.
8. Wait up to `timeout_ms`, polling `available` and draining any bytes into a
   bounded buffer.
9. Stop/free the worker and end the device on all paths.
10. Return a map:

```text
{
  written: u64,
  read: u64,
  callbacks: u64,
  rx_preview: bytes-or-hex-string,
}
```

The firmware must leave USB and the radio usable after success, bad params,
start failure, write failure, timeout, and client disconnect.

## 6. Host data model

Keep this slice diagnostic-only:

- Add host parsing for probe payload input.
- Add `subghz_link_probe(frequency, payload, timeout)` in `flip-client`.
- Add CLI output formatting.

Do not introduce a persistent `.subghz-link` file format yet. File transfer
will need packetization and integrity checks that are out of scope for this
single-device probe.

## 7. Later two-device design

When a second Flipper is available, build real transfer commands:

```sh
flip subghz recv --freq 433920000 --output out.bin
flip subghz send --freq 433920000 --file in.bin
```

The transfer protocol should sit above `subghz_tx_rx_worker_*` and include:

- magic/version
- stream id
- sequence number
- payload length
- CRC
- bounded chunk size
- pacing and retry policy

The daemon currently relays device-to-host streams and only forwards
`STREAM_STOP` from client to device during an active stream. Large host-to-device
file transfer should either use bounded one-shot chunks first or add explicit
bidirectional stream relay.

## 8. Acceptance criteria

Single-device acceptance:

- `flip caps` lists `subghz.link_probe`.
- `flip subghz link-probe --freq 433920000 --data hello` returns a structured
  success response.
- `flip status` succeeds immediately after the probe.
- Invalid frequency or oversized payload returns a clear error.
- Firmware release build passes.
- Existing host tests pass.

Two-device acceptance, deferred:

- One Flipper can receive a file sent by another Flipper.
- Sender and receiver hashes match.
- Interrupted transfer leaves both Flippers reachable.
