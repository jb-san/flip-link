# Flipper Interface Roadmap + Sub-GHz Design

**Date:** 2026-06-07
**Status:** Draft design for Sub-GHz Slice 1
**Context:** IR raw capture/transmit is complete enough to pause. The next goal is
to work through the Flipper Zero interfaces one by one and decide whether
flip-link can capture arbitrary data, transmit arbitrary data, or both.

## 1. North star

flip-link should expose Flipper Zero interfaces as programmable transports, not
only as canned cloning tools. For each interface, track two layers:

- **Layer 1: raw signal/frame records.** Capture what the interface sees and
  transmit it back with enough metadata to be reproducible.
- **Layer 2: arbitrary byte link.** Define framing/line coding or use an existing
  packet API so flip-link can move caller-provided bytes between compatible peers.

Layer 1 proves the hardware lifecycle, backpressure, and file format. Layer 2 is
the data-transfer product.

## 2. Interface roadmap

| Interface | Layer 1 feasibility | Layer 2 feasibility | Priority | Notes |
| --- | --- | --- | --- | --- |
| Infrared | Complete for raw envelope timings. | Future IR byte codec on top of timings. | Done for now | File stores carrier/duty plus timings. Capture stop is CLI-driven via Ctrl-C, `--idle-gap`, or `--duration`. |
| Sub-GHz | High. Raw level/duration capture and replay map to HAL async RX/TX. | High, but needs either Flipper packet worker or our own PHY/framing. | Next | Must require explicit frequency and preset. TX can be refused by regional validation. |
| GPIO raw digital | High. Sample/generate pin transitions. | High. Can become a simple wired bitstream. | Very high | Useful as a known-good arbitrary link and test harness for line coding. |
| UART over GPIO | Medium for captures, because UART is already decoded bytes. | Very high. Native serial byte stream. | Very high | Best early Layer 2 transport after Sub-GHz raw. |
| SPI over GPIO | Medium. Bus transactions are structured, not a passive universal capture unless roles are defined. | High as a controlled master/slave transaction API. | High | Needs role, mode, speed, chip-select policy. |
| I2C over GPIO | Medium. Similar to SPI but open-drain bus rules matter. | High for master transactions, lower for arbitrary peer-to-peer. | High | Good for tool/control workflows, less natural as a generic link. |
| iButton / 1-Wire | Medium. Read/emulate keys and timed pulses. | Medium. Possible but narrow and slow. | Medium | More useful for protocol records than generic transfer. |
| NFC 13.56 MHz | Medium. Record/emulate supported tag/protocol frames. | Medium. APDU/NDEF style data is possible, raw RF is constrained. | Medium | Needs a separate protocol decision per NFC family. |
| 125 kHz RFID | Medium-low. Record/emulate supported IDs. | Low-medium. Generic byte transfer is possible only with custom modulation/protocol. | Low-medium | Mostly protocol cloning/emulation, not broad data transport. |
| BLE / 802.15.4 radio | Medium. App-level packets are possible. | Medium-high via GATT or custom app protocol. | Medium | Flipper firmware ownership and coexistence need investigation. |
| USB | Already core transport. | Already core transport. | Existing | flip-link itself uses USB CDC. |
| BadUSB / HID / U2F | Low as generic sensors. | Narrow, protocol-specific. | Low | Useful for scripted host interactions, not arbitrary bidirectional capture. |
| microSD | Storage only. | File transport only. | Support role | Useful for large records and offline exchange. |
| Display/buttons/buzzer/vibration/battery | Not capture/transmit transports. | Not data links. | Status/UI only | Expose status or local UX later if needed. |

Recommended order after IR:

1. Sub-GHz raw timing capture/transmit.
2. Sub-GHz byte link investigation and first link mode.
3. UART over GPIO.
4. Raw GPIO digital timing.
5. SPI/I2C controlled transactions.
6. iButton, NFC, 125 kHz RFID.
7. BLE/802.15.4 if the firmware ownership model is practical.

## 3. Sub-GHz scope

Sub-GHz has two useful products:

1. **Raw signal records.** Capture and replay unknown OOK/FSK-ish RF signals as
   level/duration samples on a chosen frequency/preset. This is the Sub-GHz
   equivalent of the IR Layer 1 work, except levels are explicit and the carrier
   is the configured radio frequency.
2. **Byte link.** Send and receive arbitrary bytes between compatible devices.
   This can use `subghz_tx_rx_worker_*` if it works well from a FAP, or a custom
   line code built on the raw timing layer.

Slice 1 implements raw records only. It deliberately leaves the byte link as
Slice 2, because TX/RX lifecycle, frequency validation, stream backpressure, and
file fidelity must be proven first.

## 4. Sub-GHz Slice 1 behavior

### CLI

```
flip subghz capture --freq 433920000 --preset ook650 --idle-gap 500 --output /tmp/button.subghz
flip subghz transmit --file /tmp/button.subghz
```

Optional controls:

- `--duration <ms>` on capture ends by wall-clock time.
- `--repeat <n>` on transmit repeats the record, with a conservative default of
  `1`.
- `--freq` and `--preset` on transmit override the file metadata.

`--freq` is required for capture. There is no default frequency; accidental RF
transmit/receive behavior should be avoided.

### Raw record file

Use a flip-link owned text format, readable and diffable:

```
# format=flip-subghz-raw-v1
# frequency=433920000
# preset=ook650
1 9000
0 4500
1 560
0 560
```

Each data line is `<level> <duration_us>`.

- `level` is `0` or `1`.
- `duration_us` is an unsigned integer and must fit in the 30-bit
  `LevelDuration.duration` field used by Flipper Sub-GHz async TX.
- The first captured record may be an idle lead-in. The client should trim
  leading idle/duplicate idle once we can characterize the hardware behavior;
  the initial implementation should preserve levels exactly and only reject
  empty captures.

### Stream payload

Use a new stream format constant:

```
subghz_level_duration_le_v1
```

Each `STREAM_DATA` payload is a sequence of 5-byte records:

- byte 0: level, `0` or `1`
- bytes 1..4: little-endian `u32` duration in us

The daemon remains generic. It already relays `STREAM_START`, bidirectional
`STREAM_DATA`, and `STREAM_STOP`.

### Firmware

Add a `subghz` instrument:

- `subghz.capture` is streaming.
- `subghz.transmit` is a one-shot request.
- `subghz.validate` is optional but useful for quick CLI checks.

Firmware uses the local `flipperzero_sys` bindings. Preset loading and
frequency validation/setup go through the Sub-GHz device abstraction because the
enum preset loader is exposed there in SDK 0.16:

- Device lookup: `subghz_devices_get_by_name("cc1101_int")`.
- Device lifecycle: `subghz_devices_begin/end/reset/idle/sleep`.
- Preset loading: `subghz_devices_load_preset`.
- Frequency validation/setup: `subghz_devices_is_frequency_valid` and
  `subghz_devices_set_frequency`.
- Presets:
  - `FuriHalSubGhzPresetOok270Async`
  - `FuriHalSubGhzPresetOok650Async`
  - `FuriHalSubGhzPreset2FSKDev238Async`
  - `FuriHalSubGhzPreset2FSKDev476Async`
  - `FuriHalSubGhzPresetMSK99_97KbAsync`
  - `FuriHalSubGhzPresetGFSK9_99KbAsync`
- Raw timing capture: `furi_hal_subghz_start_async_rx(callback, context)`.
- Raw timing transmit: `furi_hal_subghz_start_async_tx(callback, context)`.

TX callback returns `LevelDuration`. The SDK defines level values as:

- `0`: reset/end
- `1`: low
- `2`: high
- `3`: wait

When the record is exhausted, return reset/end. Do not loop by default.

### Backpressure and safety

Follow the IR lessons:

- Device-to-host capture uses an ISR-to-main stream buffer and dropped-sample
  counter.
- Host-to-device transmit stays one-shot for Slice 1; the daemon already paces
  large writes to the device CDC endpoint.
- The firmware must stop RX/TX and return the radio to idle/sleep on normal stop,
  client disconnect, Back press, and errors.
- Invalid or region-refused frequencies return a clear device error. flip-link
  must not bypass Flipper firmware regional controls.

## 5. Sub-GHz Slice 2 byte link

After Slice 1 works on hardware:

1. Spike `subghz_tx_rx_worker_*` with two Flippers or one Flipper plus a known
   compatible receiver.
2. If the worker is practical, expose:
   ```
   flip subghz recv --freq 433920000 --output bytes.bin
   flip subghz send --freq 433920000 --file bytes.bin
   ```
3. If the worker is not practical from a FAP, define a small raw-timing line code
   on top of Slice 1:
   - preamble
   - length
   - payload
   - CRC
   - inter-frame gap

Slice 2 acceptance is not "can replay a captured remote." Acceptance is two
compatible peers transferring caller-provided bytes and validating the payload
CRC.

## 6. Out of scope

- Rolling-code bypass, key extraction, or protocol-specific security attacks.
- Region unlocks or transmit outside firmware-approved frequencies.
- Decoding vendor Sub-GHz protocols in Slice 1.
- Import/export compatibility with Flipper's `.sub` files, unless it becomes a
  deliberate later task.
