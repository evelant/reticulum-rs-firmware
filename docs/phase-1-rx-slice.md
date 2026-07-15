# Phase 1 Tracker receive-only vertical slice

**Status:** implementation contract for the first radio-bearing lab image

**Target:** Heltec Wireless Tracker V2.3, ESP32-S3FN8, SX1262 and KCT8103L

**Driver baseline:** `lora-phy` 3.0.1 on the pinned workspace graph

## Purpose and safety boundary

This slice should prove the smallest useful path from a real RNode-compatible
LoRa transmission into the project-owned Rete ingress boundary:

```text
SX1262 RX -> depth-2 raw-frame channel -> TimedRnodeRx -> EmbeddedNode::ingest
```

It is deliberately not a bidirectional node. The image may receive and inspect
traffic, but no value produced by Rete may reach a radio transmission API. The
radio owner exposes no send method, the external FEM remains selected for RX,
and the target qualification trace must contain no SX1262 `SetTx` command.

The existing `safe-idle` image remains the default firmware feature and remains
RF-inert. It continues to hold SX1262 reset, KCT8103L power, CSD and CTX low and
must not initialize `lora-phy`. The receive slice is a separate, explicit lab
feature or binary. Adding the receive slice must not silently change what
`cargo build -p reticulum-heltec-tracker-v2` builds or boots.

## Non-goals

This milestone does not provide:

- LoRa transmission, announces, proofs, forwarding or transport-node service;
- production regional, airtime, duty-cycle, power or thermal policy;
- the Tracker FEM transmit gain table or a requested antenna-power API;
- USB packet transport, BLE, Wi-Fi, a device API, SPA or mobile application;
- LXMF, propagation service, NomadNet, Micron or GNSS;
- a reusable all-radio abstraction or an upstream replacement for
  `rete-iface-lora`;
- a production decision between the SX1262 LDO and DC-DC regulator modes;
- a production decision on SX1262 boosted RX with the external KCT8103L LNA;
- a guarantee that the provisional FEM delays are correct for every board
  sample, temperature or supply condition.

## Explicit lab receive profile

There is no default legal frequency for this board. `LabRxProfile` therefore
must not implement `Default`, and it must not contain `unwrap_or` fallbacks for
frequency or modulation. The RX image is built only after the operator supplies
an explicit profile. A missing or invalid lab profile is a build/configuration
failure; it must never fall through to 915 MHz, 868 MHz or another regional
guess.

The profile contains only receive and wire-compatibility fields:

```rust,ignore
struct LabRxProfile {
    frequency_hz: u32,
    spreading_factor: SpreadingFactor,
    bandwidth: Bandwidth,
    coding_rate: CodingRate,
    preamble_symbols: u16,
    explicit_header: bool,
    crc: bool,
    iq_inverted: bool,
}
```

The first interoperability profile is constructed explicitly with the same
frequency, spreading factor and bandwidth as the transmitting test RNode,
`CodingRate::_4_5`, an 18-symbol preamble, explicit headers, CRC enabled and
normal (non-inverted) IQ. These are explicit lab inputs, not board defaults.
There is intentionally no TX-power field.

All project-owned range and combination checks occur before SX1262 reset is
released. The driver-specific `LoRa::create_modulation_params()` check is
necessarily performed after the RX-only radio has been initialized, but while
VFEM power and CSD remain low. If it rejects the profile, the external path was
never energized and the image stops for watchdog recovery without entering RX.

## Tracker pin ownership and provisional power-up sequence

The receive image owns these pins exactly once:

| Function | GPIO | Initial level or mode |
| --- | ---: | --- |
| KCT8103L CSD | 4 | output low |
| KCT8103L CTX | 5 | output low (RX) |
| KCT8103L VFEM power | 7 | output low |
| SX1262 NSS | 8 | output high |
| SX1262 SCLK | 9 | SPI2 clock |
| SX1262 MOSI | 10 | SPI2 output |
| SX1262 MISO | 11 | SPI2 input |
| SX1262 reset | 12 | output low |
| SX1262 BUSY | 13 | input, pull-down |
| SX1262 DIO1 IRQ | 14 | input, pull-down |
| KCT8103L CPS electrical tie | 46 | input/high-impedance |

The schematic exposes the PA_CPS node at ESP32-S3 GPIO46 while SX1262 DIO2 is
the intended owner of that node. GPIO46 must remain input/high-impedance for the
entire image. It must never be configured as an output or used as a second CPS
driver.

Critical output levels are established before the executor and heap are
started, as in `safe-idle`. The lab RX transition then uses this exact order:

1. Validate the explicit `LabRxProfile` while reset, VFEM power, CSD and CTX
   remain low and NSS remains high.
2. Configure SPI2 mode 0 at an explicit initial 1 MHz, plus BUSY and DIO1
   inputs, while SX1262 reset is still low.
3. Reassert CTX low and leave GPIO46 input/high-impedance.
4. With VFEM power and CSD still low, give reset, DIO1, BUSY and the SPI device
   to the private RX-only radio wrapper. `lora-phy` performs its reset sequence:
   10 ms delay, reset low for 20 ms, reset high, then another 10 ms delay.
5. Initialize the always-powered SX1262, including private sync-word, DIO3 TCXO
   and DIO2 RF-switch control, while the external FEM remains disabled. Create
   and validate the modulation and receive-packet parameters in this state.
6. After successful SX1262 initialization, reassert CTX GPIO5 low.
7. Set VFEM power GPIO7 high, then wait `FEM_POWER_SETTLE_MS = 5`.
8. Set CSD GPIO4 high, then wait `FEM_CSD_SETTLE_MS = 5`.
9. Verify CTX is still low and GPIO46 is still input/high-impedance, then call
   `prepare_for_rx(Continuous)`. The subsequent complete `rx()` call issues the
   SX1262 continuous-RX command.

The two 5 ms FEM delays are conservative Phase 1 placeholders, not values
characterized by Heltec or established by the working Tracker firmware. The
working C++ path initializes the SX1262 before enabling the KCT8103L branch and
contains no explicit delay between its VFEM, CSD and CTX writes. HIL must capture
power, CSD, CTX, reset, BUSY and current so the values can be replaced by
evidence before this sequence is treated as production policy.

The CTX output is held by an opaque `RxOnlyFem` owner. It exposes no setter, no
mutable pin reference and no `into_parts()` escape. The only state transition
available outside construction is fail-closed shutdown, which reasserts CTX
low before lowering CSD and VFEM power. CTX is not exposed as a generic TX
switch pin.

## Pinned `lora-phy` configuration

SPI is converted to async mode and wrapped as an async
`embedded_hal::spi::SpiDevice`, initially using
`embedded-hal-bus::spi::ExclusiveDevice`. `embedded-hal-bus` must be a direct,
exact dependency with its `async` feature rather than an accidental transitive
dependency.

The board module supplies a private `TrackerRxInterfaceVariant` rather than
using `GenericSx126xInterfaceVariant` directly. It receives:

- reset GPIO12 as an `OutputPin`;
- DIO1 GPIO14 as an async `Wait` input;
- BUSY GPIO13 as an async `Wait` input;
- no externally callable RF-switch pins.

It implements the same reset timing, DIO1-high wait and BUSY-low wait required
by `lora-phy`. Its receive-switch hook succeeds without changing CTX, which is
already held low by `RxOnlyFem`. Its transmit-switch hook unconditionally
returns `RadioError::InvalidConfiguration`; SX126x `do_tx()` calls that hook
before writing `SetTx`, so an accidental internal TX call fails before opcode
`0x83` while the separately owned CTX remains low. The driver must own the IRQ
path for the lifetime of the radio; polling DIO1 in another task would race the
receive state machine.

The first lab configuration is:

```rust,ignore
let sx1262 = Sx126x::new(
    spi_device,
    interface_variant,
    sx126x::Config {
        chip: Sx1262,
        tcxo_ctrl: Some(TcxoCtrlVoltage::Ctrl1V8),
        use_dcdc: false, // Phase 1 provisional choice, not production policy
        rx_boost: false, // measured below before any production decision
    },
);

let mut radio = LoRa::new(sx1262, false, Delay).await?;
```

The `false` argument selects the private LoRa network sync word `0x1424`, which
matches RNode. `Ctrl1V8` configures the DIO3-powered 1.8 V TCXO. The stock
`Sx1262` variant enables `SetDIO2AsRfSwitchCtrl(1)` during initialization, so
DIO2 owns CPS and is low outside TX. DIO2 and DIO3 are controlled by SX1262
commands, not driven as ESP32 outputs; the GPIO46 tie remains high-impedance.

The schematic has the SX1262 DC-DC support inductor populated, so DC-DC mode is
electrically available. The working Tracker C++ firmware nevertheless leaves
the chip in its default LDO mode. Phase 1 starts provisionally in LDO mode to
match that demonstrated bring-up and to avoid changing two variables at once;
this is not evidence that LDO is production-correct. HIL must compare LDO and
DC-DC receive current, stability, sensitivity and startup behavior before an
ADR or board policy selects one.

Likewise, the Rete ESP example uses `rx_boost: false`, while the working C++
driver writes the boosted RX-gain value. Phase 1 begins unboosted to establish a
lower-current baseline. It then measures both modes with the external LNA; no
production sensitivity or power claim is made in advance.

After construction, the only PHY setup calls are:

```rust,ignore
let modulation = radio.create_modulation_params(
    profile.spreading_factor,
    profile.bandwidth,
    profile.coding_rate,
    profile.frequency_hz,
)?;

let receive_packet = radio.create_rx_packet_params(
    profile.preamble_symbols,
    !profile.explicit_header,
    255,
    profile.crc,
    profile.iq_inverted,
    &modulation,
)?;

radio
    .prepare_for_rx(RxMode::Continuous, &modulation, &receive_packet)
    .await?;
```

The opaque wrapper exposes one async operation equivalent to
`LoRa::rx(&receive_packet, &mut [u8; 255])`. It does not expose the inner
`LoRa`, `Sx126x`, SPI device or interface variant and does not implement Rete's
bidirectional `ReteInterface` trait.

`LoRa::new()` programs PA configuration and SX1262 `SetTxParams` (`0x8e`) with
an initialization value even in an RX-only application. This command does not
start RF transmission and is allowed by the Phase 1 trace gate. The following
APIs are absent from the wrapper and forbidden in the RX image:

- `LoRa::prepare_for_tx()`;
- `LoRa::tx()`, which enables the TX switch and issues `SetTx` (`0x83`);
- `LoRa::continuous_wave()`, which enters continuous RF transmission;
- `rete-iface-lora::LoRaInterface::send()`;
- any generic node runner that can call a bidirectional interface's `send()`.

## Two-task ownership model

Exactly two long-lived tasks participate in this slice.

### 1. Radio RX task

The radio task exclusively owns `TrackerRxRadio` and DIO1/BUSY IRQ processing.
It performs the complete `LoRa::rx()` future without placing it inside a
cancellable `select`. `lora-phy` documents IRQ processing as unsafe to cancel
mid-flow because interruption during the SPI sequence can lock the radio.

After `prepare_for_rx(Continuous)` succeeds, the task repeatedly awaits one
complete `rx()` call. Control messages and diagnostics never cancel that
future. On a returned radio error, the task reasserts the fail-closed FEM state,
records a bounded error code and either performs a complete RX reinitialization
or stops for watchdog recovery; it does not improvise a partial TX/RX state
transition.

Every successful PHY receive is offered with `try_send` to a statically
allocated depth-2 raw-frame channel. The message contains `[u8; 255]`, a valid
length, `FrameSignal` and a monotonic receive timestamp. The radio task performs
no RNode parsing and owns no 508-byte assembly buffer. If the ingress task is
behind, it drops the new raw frame, increments `raw_frame_channel_full` and
immediately rearms RX instead of blocking with the radio out of receive mode.
Dropping either half of a split packet is permitted at this bounded handoff; the
ingress-side deadline ensures any unmatched half expires.

### 2. RNS ingress task

The ingress task exclusively owns `TimedRnodeRx`, its 508-byte caller-owned
scratch buffer, an endpoint-configured `EmbeddedNode`, its identity and
cryptographic RNG. It is the only task allowed to transform a physical frame
into an RNS packet.

When `TimedRnodeRx::next_deadline()` is `None`, the task awaits the next raw
frame. When a deadline exists, it selects between the raw-frame channel and an
Embassy monotonic timer for that absolute deadline. This select never contains
the PHY RX future. A timer wake calls `TimedRnodeRx::expire(now_ticks)` even in
complete radio silence, providing real wall-clock fragment expiry without
cancelling `lora-phy`.

On a channel item, the task records the receive timestamp for latency
diagnostics and calls `TimedRnodeRx::feed()` with the current monotonic time,
`FrameSignal` and the 508-byte scratch buffer. Using current time makes a frame
processed at or after the deadline unable to revive stale state even if the
channel and timer become ready together. `feed()` independently expires overdue
state before consuming bytes, so the race is fail-closed.

Only `TimedReceiveOutcome::Packet` authorizes a synchronous call to
`EmbeddedNode::ingest(packet, now, LORA_INTERFACE_ID, rng)`. Pending, expired,
framing-error and 501-through-508-byte results update bounded diagnostics and do
not reach Rete.

It consumes local events only for bounded counters and diagnostics. Every
packet in `IngressReport.actions.packets` is counted in
`rete_outbound_suppressed` and then dropped. No outbound channel exists, and the
ingress task has neither a radio handle nor a command capable of asking the
radio task to transmit. `unroutable_packets` is also counted. Endpoint role
reduces forwarding output but is not relied on as the RF interlock: suppression
applies to proofs, replies, forwarding, announce retransmission and every future
action variant.

## RNode and RNS length boundaries

The SX1262 delivers at most 255 bytes. Each received physical frame is processed
in this order:

1. Reject a reported PHY length over 255 before slicing the frame buffer.
2. Feed the entire physical frame, including its one-byte RNode header, to
   ingress-owned `TimedRnodeRx` with current monotonic time and `FrameSignal`.
3. Treat a pending first split frame as bounded state, not as an RNS packet.
4. Permit the RNode compatibility layer to assemble at most 508 bytes, matching
   RNode's physical `HW_MTU`.
5. Have `TimedRnodeRx` independently reject completed lengths 501 through 508
   at its base RNS 500-byte guard.
6. Pass only 0 through 500 bytes to `EmbeddedNode::ingest`.

The 508-byte physical limit must never be renamed to the RNS MTU. A raw 255-byte
LoRa frame must never bypass the RNode header/split layer and enter Rete
directly. Duplicate, mismatched, late and malformed split sequences follow
`TimedRnodeRx`'s explicit outcomes, errors and scheduled deadline policy.

## Bounded diagnostics

Diagnostics are a fixed-size snapshot of saturating counters and last-known
scalar values. They do not retain packet payloads, allocate a packet history or
emit one log line per noise IRQ. At minimum they include:

- PHY frames and bytes received;
- last RSSI and SNR;
- radio initialization and the IRQ, SPI, receive and timeout errors surfaced at
  the wrapper boundary;
- `TimedRnodeRx::diagnostics()` complete, pending, replaced, discarded,
  malformed and expired outcomes;
- completed RNode packets rejected above the 500-byte RNS limit;
- depth-2 raw-frame channel-full drops and handoff latency;
- Rete ingress dispositions and rejection classes;
- Rete events observed;
- outbound packets and unroutable actions suppressed;
- current regulator and RX-boost lab settings;
- a monotonically increasing radio-reinitialization count.

`lora-phy` 3.0.1 observes SX1262 CRC/header IRQ flags internally but does not
return a CRC-specific count through its public RX result. Phase 1 reports that
counter as unavailable rather than inferring one from missing packets; exposing
it would be a separate focused driver contribution.

Periodic summaries are rate-limited. Raw frames, RNS payloads, identity private
material and decrypted application content are not logged. Counter overflow
saturates instead of wrapping.

## Qualification gates

### Host gates

- `safe-idle` remains the default feature and its tests continue to prove no
  default frequency and TX disabled by default.
- `LabRxProfile` has no `Default`; missing frequency, invalid frequency and
  invalid modulation tests fail before the radio-power transition.
- A mock async `SpiDevice` captures the complete cold-start and RX command
  stream. It must contain private sync-word programming, DIO3 TCXO control
  `0x97` with voltage byte `0x02`, and DIO2 RF-switch control `0x9d 0x01`.
- The mock command stream may contain `SetTxParams` `0x8e`, because
  `lora-phy 3.0.1` performs that non-transmitting initialization. It must never
  contain `SetTx` `0x83` or a continuous-wave command.
- Wrapper API tests or compile-fail coverage prove there is no `send`, `tx`,
  `prepare_for_tx`, `continuous_wave`, inner-radio escape or CTX-high method.
- A crate-private fault-injection test attempts the lora-phy TX path and proves
  the Tracker interface's transmit-switch hook returns an error before SPI sees
  `SetTx`, while CTX remains low.
- `TimedRnodeRx` tests cover lengths 0, 1, 253, 254, 255, 256, 499, 500,
  501, 507, 508 and 509 plus duplicate, reordered, mismatched and expired
  fragments.
- Integration tests prove 501 through 508 can be valid at the RNode physical
  boundary but never reach Rete, while 500 can.
- A task-level paused-time test proves a pending fragment expires on its timer
  without another radio frame and without polling or cancelling the PHY task.
- Rete fixtures that produce events and packet actions prove all outbound
  actions are counted and destroyed without a radio-side effect.
- Depth-2 raw-frame channel saturation has a deterministic drop-new result and
  cannot grow memory or block RX rearming; a dropped continuation leaves only
  bounded state that the ingress timer expires.

### Target build gates

- Both the unchanged default `safe-idle` image and the explicit lab RX image
  build from one locked workspace for `xtensa-esp32s3-none-elf`.
- The safe-idle image still contains no radio initialization and retains its
  existing RF-inert boot behavior; size/map drift is explained.
- The lab RX image's ELF/map, static sections, reclaimed heap reservation and
  task stacks are recorded. Boot free heap and stack high-water measurements
  are captured on target.
- The linked RX owner has no reachable call path to `LoRa::tx`,
  `continuous_wave` or a bidirectional `ReteInterface::send`. A raw byte search
  for `0x83` is not sufficient because unrelated data can contain that byte;
  the static call audit is paired with the SPI command capture below.
- Existing `clippy::mem_forget` and large-stack protections remain enabled.

### Hardware-in-the-loop gates

- A logic-analyzer capture verifies the exact provisional power-up ordering,
  GPIO46 remains input/high-impedance, CTX never rises and DIO2 owns CPS.
- SPI capture from reset through initialization, packet reception, malformed
  traffic and idle soak contains no `SetTx` opcode `0x83`. `SetTxParams` `0x8e`
  is expected and explicitly allowed. DIO3 `0x97`/`0x02` and DIO2 `0x9d`/`0x01`
  must be visible.
- A known RNode or Python-controlled RNode transmitter sends single and split
  frames using the explicit matching lab profile. The Tracker reports correct
  completed bytes, RSSI/SNR and Rete ingress disposition through the 500-byte
  boundary.
- On-air monitoring finds no Tracker-originated packet during boot, successful
  RX, malformed traffic, Rete proof-producing input, channel saturation,
  radio-error recovery or a 24-hour receive soak.
- Power, CSD, CTX, reset and BUSY timing is captured on more than one Tracker
  sample. The provisional 5 ms delays are retained or changed based on the
  trace, not assumption.
- LDO and DC-DC modes are compared for initialization reliability, idle/RX
  current and receive behavior. Unboosted and boosted RX are compared for
  current and sensitivity with the KCT8103L path. Results are recorded before
  either setting becomes board policy.
- Boot free heap, minimum free heap, largest free block and both task stack
  high-water marks remain stable during the soak and hostile-input run.

## Implementation sequence

1. Add the separate lab RX build selection and non-default `LabRxProfile`
   validation without changing the safe-idle default.
2. Add the Tracker-private pin owner and `RxOnlyFem`, including GPIO46
   high-impedance enforcement, provisional timing constants and fail-closed
   shutdown tests.
3. Add the exact async SPI device dependency and an opaque `TrackerRxRadio`
   around `lora-phy` 3.0.1. Prove its mock command stream before hardware use.
4. Implement the non-cancellable radio task and its depth-2 `try_send` handoff
   of raw 255-byte frames, signal metadata and monotonic receive time.
5. Implement the ingress task with `TimedRnodeRx`, the 508-byte scratch buffer,
   channel-versus-deadline selection, endpoint Rete ingestion and unconditional
   outbound-action suppression. Add bounded diagnostics only after ownership is
   fixed.
6. Build and inspect both target images, then capture the first powered radio
   initialization with the antenna/load and lab instrumentation appropriate for
   the board.
7. Run single-frame, split-frame, malformed-input, backpressure and 24-hour RX
   HIL scenarios against an established RNode/Python peer.
8. Record FEM timing, regulator and RX-boost evidence. Update board policy only
   where those measurements support a conclusion.

## Exit criterion

Phase 1 RX is complete when the unchanged default image still boots RF-inert,
an explicitly configured lab image repeatedly receives and reassembles real
RNode traffic into the project-owned 500-byte Rete ingress boundary, all Rete
outbound actions are observably suppressed, memory and task stacks remain
bounded through the soak, CTX never leaves RX, and host plus HIL command traces
show no SX1262 `SetTx` `0x83` in any tested path while correctly allowing the
non-transmitting `SetTxParams` `0x8e` initialization command.

Completion authorizes design of the guarded transmit slice; it does not itself
authorize RF transmission or establish production FEM, regulator, RX-boost or
regional policy.
