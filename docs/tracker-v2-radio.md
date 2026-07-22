# Tracker V2 bidirectional radio owner

**Status:** product-named RX/CAD/TX owner extracted and powered-regression
tested; real dispatcher integration remains open<br>
**Board:** Heltec Wireless Tracker V2.3, ESP32-S3FN8, SX1262 and KCT8103L<br>
**Initial configuration:** explicit NA915 development profile

## Capability boundary

Three crates intentionally describe different trust and capability surfaces:

- `reticulum-board-heltec-tracker-v2` remains the frozen receive-only board
  boundary. Its public owner has no TX or CAD method and its private SPI
  firewall rejects transmit opcodes. No feature, including `--all-features`,
  can turn that crate into a transmitter.
- `reticulum-board-heltec-tracker-v2-radio` is the product-capable sibling. It
  owns the proven Tracker configuration, SX1262 PA override, external-FEM and
  reset policy under product names. Board-neutral legacy bounded receive,
  persistent continuous receive, CAD, and atomic one/two-frame transmit
  mechanics live in
  `reticulum-radio-lora-phy` and are shared with independent board wrappers.
- `reticulum-board-heltec-tracker-v2-tx-hil` is now a one-dependency
  compatibility facade. It retains historical HIL aliases, log labels and the
  diagnostic near-field feature forward, but contains no SPI, radio or FEM
  implementation.

This split keeps the receive-only qualification reproducible while avoiding a
Cargo feature that would silently collapse its capability boundary.

## Fixed initial configuration

`TRACKER_NA915_DEV_CONFIGURATION` is an opaque board-validated value with no
public constructor. Firmware passes that exact value explicitly when it
constructs `TrackerRadio`; it cannot supply an arbitrary `LabRxProfile` or
numeric power and accidentally bypass the board range or PA override. The
calibrated value is invariant under every Cargo feature.

| Field | Value |
| --- | --- |
| Configuration identity | `Na915DevCalibratedMinimum` |
| Center frequency | 915,000,000 Hz |
| Modulation | SF7, 125 kHz, CR 4/5 |
| Preamble | 24 symbols |
| Packet mode | explicit header, CRC, normal IQ |
| Sync word | private network (`0x1424`) |
| Regulator / receive gain | LDO / unboosted |
| Legacy bounded-RX preamble-search timeout | 248 symbols |
| Whole RX operation watchdog | 1,500,000 us; expiry is fail-closed cancellation |
| Antenna-path target | characterized 14 dBm |
| SX1262 output | 0 dBm behind the external FEM |
| PA/OCP override | duty `0x04`, `hp_max=0x07`, OCP `0x28` |

The explicit `near-field-attenuation` feature additionally exposes
`TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION`, which uses the SX1262
minimum of -9 dBm and labels the estimated 5 dBm antenna path as diagnostic and
uncalibrated. Merely enabling the feature never selects or substitutes that
value: the historical HIL must pass it explicitly through its own
feature-selected `TRACKER_TX_HIL_CONFIGURATION`. It is not a second production
power profile. Graph policy locks the product crate's feature shape and the
historical HIL's exact feature forward.

## Ownership and cancellation

`TrackerRadio` owns every SPI, reset, DIO1, busy and external-FEM resource. Its
shared `Sx126xRnodeRadio` core takes `Option<Active>` at the beginning of every
async RX, CAD or atomic logical-packet TX operation. If a future is cancelled
or an operation fails, the local active owner drops through the private Tracker
interface: SX1262 reset is asserted, then CSD, CTX and FEM power are driven
low. The wrapper remains faulted instead of reusing uncertain hardware state.

TX retains the qualified two-stage one-shot arm:

1. the public owner arms packet preparation;
2. the private early hook consumes that arm and asserts CTX before packet/FIFO
   writes;
3. the final hook consumes the prepared state immediately before `SetTx`;
4. successful `TxDone` is followed by explicit standby so CTX returns low.

The low-level product surface is deliberately one physical frame at a time,
while the portable sole-radio trait owns a complete split-frame TX operation:

- `receive_frame()` performs one bounded SX1262 receive and returns RSSI, SNR
  and the monotonic tick sampled when the final DIO1 wait resumed;
- `SoleRnodeRadio::receive_bounded()` exposes the same caller-buffer operation
  with portable signal/final-IRQ metadata and a normal no-preamble result;
- `SoleRnodeRadio::receive_continuous_until()` starts one continuous-RX epoch,
  races only the cancellation-safe DIO wait against a caller scheduler future,
  and leaves RX armed after a frame, discarded invalid frame, or scheduler
  yield;
- `SoleRnodeRadio::invalidate_receive_session()` marks an abandoned RX epoch
  untrusted so the next receive or CAD/TX transition performs standby,
  IRQ-routing disable, pending-IRQ clear, and a fresh arm;
- `channel_activity_detected()` performs exactly one low-level CAD and returns
  `true` for busy, then explicitly restores standby because pinned
  `lora-phy` otherwise retains its CAD software mode;
- `transmit_frame()` accepts exactly 1 through 255 bytes and performs no
  framing, retry or policy decision;
- `shutdown()` consumes the active owner and leaves the path inert.

The target still supplies an outer monotonic deadline using the board-owned
`TRACKER_MAXIMUM_RECEIVE_OPERATION_US` value. Its 1.5-second bound covers the
248-symbol SF7/BW125 search, a maximum 255-byte frame, SPI/standby cleanup and
board-qualified scheduling margin. The SX1262 receive symbol timeout stops
after preamble detection, and the pinned TX path has no independent hardware
deadline. Cancelling an overdue operation is therefore a supervised radio
fault and reconstruction event, never a normal no-preamble timeout or ordinary
scheduler wake.

## Deliberately outside the board owner

The permanent dispatcher, not this crate, must own:

- RNode sequence allocation and one/two-frame packet representation;
- one CSMA/CAD contest for the whole logical packet, including both split
  frames;
- backoff, attempt exhaustion and the rule that exhaustion never forces TX;
- regional, airtime and reservation policy bound to the exact configuration
  identity;
- node-core permits, packet-owner deadlines and conservative completion;
- first-frame-success/second-frame-failure classification;
- RX/TX scheduling and radio reconstruction after cancellation or fault.

The HIL's MAC roles, fixed identities, sequences, packet budgets, startup
delays, semantic fixtures and terminal inert hold remain HIL policy and were
not moved into the product crate.

## Repeat the powered regression

Build the calibrated semantic image and verify its ELF identity:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-tx-hil \
  --no-default-features --features semantic-roundtrip-hil,tracker-radio \
  --target xtensa-esp32s3-none-elf
shasum -a 256 \
  target/xtensa-esp32s3-none-elf/release/reticulum-heltec-tracker-v2-tx-hil
```

Confirm the current port identities, then flash the same ELF without booting
either role early:

```sh
espflash board-info --port /dev/cu.usbmodem101 --non-interactive --skip-update-check
espflash board-info --port /dev/cu.usbmodem1101 --non-interactive --skip-update-check
espflash flash --after no-reset --port /dev/cu.usbmodem101 \
  --non-interactive --skip-update-check \
  target/xtensa-esp32s3-none-elf/release/reticulum-heltec-tracker-v2-tx-hil
espflash flash --after no-reset --port /dev/cu.usbmodem1101 \
  --non-interactive --skip-update-check \
  target/xtensa-esp32s3-none-elf/release/reticulum-heltec-tracker-v2-tx-hil
```

On the current fixtures, `/dev/cu.usbmodem101` must report E9:44 and
`/dev/cu.usbmodem1101` must report E0:40. The following `zsh` command starts
both post-reset streams concurrently and feeds the complete captures directly
to the fail-closed cross-log verifier:

```sh
python3.13 interop/python/verify_semantic_roundtrip_hil_logs.py \
  <(python3.13 interop/python/esp32s3_usb_serial_capture.py \
      --port /dev/cu.usbmodem101 --hard-reset-after-open \
      --pre-reset-drain-seconds 1 --duration-seconds 25 2>&1) \
  <(python3.13 interop/python/esp32s3_usb_serial_capture.py \
      --port /dev/cu.usbmodem1101 --hard-reset-after-open \
      --pre-reset-drain-seconds 1 --duration-seconds 25 2>&1)
```

This is the quick powered-refactor regression used below. It deliberately does
not claim preserved capture files, merged-image readback or a sealed evidence
bundle.

## Evidence and remaining limitations

Fourteen default-profile tests plus one diagnostic-profile test retain the
qualified command traces and cover the exact opaque configurations, PA
override, one-shot arm, FEM settling, fail-closed drop, RX-to-TX transition,
clear/busy CAD cleanup and a receive whose final IRQ timestamp overwrites its
preamble observation. Strict host and ESP32-S3 Xtensa Clippy pass with normal
and diagnostic features.

On 2026-07-16 EDT (2026-07-17 UTC), both attached Trackers were flashed with
the same final post-extraction `semantic-roundtrip-hil` image. E9 selected
initiator and E0 selected responder. The strict paired-log verifier cross-bound
both signed announces, encrypted DATA and its delivery proof and returned
`PASS`. The exact DATA receipt was
`63efcf518492d52597837ec59507c0d19db00e1309859627396c067a01185f00`;
the proof packet hash was
`8a5fb398b5f3d7bf003214bdc46312094ab4df14cc4d80c7f7805cb2654572de`.
Each role reported exactly two driver TX completions, ended in terminal `PASS`,
shut the radio down and remained inert. Short-run no-PSRAM heap peaks were 548
bytes on E9 and 764 bytes on E0. The 6,399,116-byte ELF has SHA-256
`808ad808b0abf407f66399e1079f1fc11587ceaba2c15f038898631d39638392`;
`espflash` reported a 361,728-byte application partition payload. This was a
powered regression run, not a new readback-qualified artifact bundle.

Open limitations are explicit:

- ordinary Rete actions are still allocation-backed and have no fixed packet
  ownership path;
- the existing dispatcher is RF-inert and no permanent firmware links this
  radio yet;
- cleanup pin failures from product shutdown/drop need a richer bounded fault
  record;
- only the initial NA915 development profile and calibrated product power are
  admitted; the separately selected near-field value is diagnostic only;
- no split-frame atomic dispatch, CSMA-pressure, sustained-load, multi-hop or
  formal regional-certification evidence exists yet.
