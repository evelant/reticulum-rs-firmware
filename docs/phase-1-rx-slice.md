# Phase 1 Tracker receive-only vertical slice

**Status:** implementation contract; steps 1–8 source/static work and clean-tree
artifact tooling are implemented; one clean paired bundle and one later
flash/readback smoke are preserved, while formal powered HIL remains pending

**Target:** Heltec Wireless Tracker V2.3, ESP32-S3FN8, SX1262 and KCT8103L

**Driver baseline:** `lora-phy` 3.0.1 on the pinned workspace graph

As of 2026-07-16, steps 1–8 below are present through source/static evidence and
clean-tree artifact tooling. A matching clean normal/pressure and closure pair
for commit `fdd6d9e` is preserved at
`artifacts/hil/phase1-rx/20260716T000006Z-fdd6d9e-normal-pressure-bundle`
and its `-closure-bundle` sibling. A later clean `bf23cc5` normal image was
flashed to E9:44, read back exactly, and observed through a bounded 125-second
supplemental smoke; that commit has no matching closure bundle, and neither run
is formal powered qualification. Host tests cover the validated profile,
fail-closed/cancellation sequencing, complete mock cold-start and receive
commands, the TX-hook interlock and the SPI opcode firewall, deterministic
depth-2 drop-new handoff behavior, profile-scaled fragment deadlines, exact
frame/deadline collisions, maintenance starvation, stale timer cancellation,
RNS MTU admission, endpoint Link policy and complete Rete action suppression,
typed compound radio faults, the reviewed public API surface and the firmware's
direct-dependency boundary. Every target mode in the current build-evidence
table links from the locked workspace, and the constrained Tracker capacity
profile has compiler-emitted stack-size evidence. The lab image now emits
allocator current/max-use telemetry and a startup-painted, guard-preserving
shared-stack high-water metric. Actual heap/stack stability still requires
powered HIL.
The separately named `lab-rx-backpressure` artifact stalls only the ingress
owner after the first split half, leaving the radio task free to produce an
exact depth-2 queue/drop result; the normal lab image contains no trigger path.
The lab image now runs the complete PHY receive future in a sole radio-owner
task and moves completed frames to a separate ingress owner without waiting;
that owner is connected through `TimedRnodeRx` to Rete. The later normal image
has been flashed for supplemental smoke, but it has not been treated as
electrically or formally RX/RF qualified.

The separately named returned-fault, retained-journal and four electrical
comparison modes are now compile-gated in source. The returned decorator and
electrical command matrix have focused host tests; all electrical/returned
selections and representative journal selectors have fresh target links and
ELF inspection. The clean-tree closure command can prepare and verify the exact
four electrical modes, both returned-fault policies and representative journal
slot 0/word 4 and write ordinal 9. The clean `fdd6d9e` closure bundle preserves
those exact artifacts, but none was flashed for the supplemental `bf23cc5`
normal-image smoke. Those software facts do not close their
retained-state, pin, current, sensitivity or RF gates. CI runs host/negative
tests, strict selector checks and the complete eight-build closure prepare and
verify pipeline for the GitHub merge commit. It intentionally does not preserve
that ephemeral bundle as qualification evidence.

## Purpose and safety boundary

This slice should prove the smallest useful path from a real RNode-compatible
LoRa transmission into the project-owned Rete ingress boundary:

```text
SX1262 RX -> depth-2 raw-frame channel -> ReceiveOnlyRete -> EmbeddedNode::ingest
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

An untrusted `LabRxProfileConfig` represents explicit numeric build input,
including `Option<u32>` for a genuinely missing frequency. Validation produces
an opaque `LabRxProfile` with private fields and copy-only getters. The profile
contains only receive and wire-compatibility fields:

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

For the Tracker V2.3 863–928 MHz RF path, the complete configured channel—not
only its center—must fit inside that hardware range. This is a receive hardware
qualification bound, not a legal-frequency or future TX authorization claim.
Numeric narrow-band inputs accept only documented RNode spellings and exact
`lora-modulation` values; there is no nearest-bandwidth rounding.

This RNode slice requires SF7–SF12, CR 4/5–4/8, a preamble of at least 18,
explicit header, CRC enabled and normal IQ. Pinned `lora-phy` 3.0.1 and the
working RNode SX1262 firmware select low-data-rate optimization differently for
several narrow-band SF/BW tuples. Validation rejects those tuples as
`UnverifiedRnodeLdroCombination` until an upstream fix or focused HIL evidence
removes that uncertainty; it does not silently call them interoperable.

All project-owned range and combination checks occur before SX1262 reset is
released. The driver-specific `LoRa::create_modulation_params()` check is
necessarily performed after the RX-only radio has been initialized, but while
VFEM power and CSD remain low. If it rejects the profile, the external path was
never energized. The typed initialization fault is recorded and logged before
an ESP ROM digital-core software reset, without
entering RX. A 30-second TIMG0 MWDT is enabled
before radio initialization as a backup for a stalled initialization or later
main/executor failure.

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
| SX1262 BUSY | 13 | input, no internal pull |
| SX1262 DIO1 IRQ | 14 | input, no internal pull |

The supplied V2.3 schematic's hidden netlist connects `PA_CPS` directly between
SX1262 DIO2 and KCT8103L CPS (`U12-12`, `U10-5`, with `C92-1` on the same net).
ESP32-S3 GPIO46 is a separate header signal (`U6-52` to `P3-17`) and the pin map
also shows it as an ordinary breakout. Earlier drafts incorrectly treated the
two nets as shared. GPIO46 is therefore not an RF interlock input and is not
claimed by this image. DIO2 remains the sole CPS driver. The audited schematic
SHA-256 is
`148672bdc7ca8646d9de5d3e9a9e58c647b1c46bd5b0b68616efa80dbd225ea7`;
the pin-map SHA-256 is
`81b2e47d94dd0d3a3749c9b89ba46f22f343a8eab5d979bff721454bf4a0a5a3`.

Critical output levels are established before the executor and heap are
started, as in `safe-idle`. The lab RX transition then uses this exact order:

1. Validate the explicit `LabRxProfile` while reset, VFEM power, CSD and CTX
   remain low and NSS remains high.
2. Configure SPI2 mode 0 at an explicit initial 1 MHz, plus BUSY and DIO1
   inputs, while SX1262 reset is still low.
3. Reassert CTX low; GPIO46 is unrelated to the RF path and remains unclaimed.
4. With VFEM power and CSD still low, give reset, DIO1, BUSY and the SPI device
   to the private RX-only radio wrapper. `lora-phy` performs its reset sequence:
   10 ms delay, reset low for 20 ms, reset high, then another 10 ms delay.
5. Initialize the always-powered SX1262, including private sync-word, DIO3 TCXO
   and DIO2 RF-switch control, while the external FEM remains disabled. Create
   and validate the modulation and receive-packet parameters in this state.
6. After successful SX1262 initialization, reassert CTX GPIO5 low.
7. Set VFEM power GPIO7 high, then wait `FEM_POWER_SETTLE_MS = 5`.
8. Set CSD GPIO4 high, then wait `FEM_CSD_SETTLE_MS = 5`.
9. Verify CTX is still low, then call `prepare_for_rx(Continuous)`. The
   subsequent complete `rx()` call issues the SX1262 continuous-RX command.

The two 5 ms FEM delays are conservative Phase 1 placeholders, not values
characterized by Heltec or established by the working Tracker firmware. The
working C++ path initializes the SX1262 before enabling the KCT8103L branch and
contains no explicit delay between its VFEM, CSD and CTX writes. HIL must capture
power, CSD, CTX, reset, BUSY and current so the values can be replaced by
evidence before this sequence is treated as production policy.

That is the demonstrated `microReticulum_Firmware/sx126x.cpp` sequence. The
separate retained `microReticulum` LoRa-interface example powers the Tracker
front end before chip initialization and inserts 1 ms delays. The references
therefore do not establish one universal ordering; FEM-off initialization is
the fail-closed Phase 1 choice and still requires HIL proof.

The CTX output is held by an opaque `RxOnlyFem` owner. It exposes no setter, no
mutable pin reference and no `into_parts()` escape. The only state transition
available outside construction is fail-closed shutdown, which reasserts CTX
low before lowering CSD and VFEM power. CTX is not exposed as a generic TX
switch pin.

## Pinned `lora-phy` configuration

SPI is configured in mode 0 at 1 MHz, converted to async mode and wrapped as an async
`embedded_hal_async::spi::SpiDevice`, initially using
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

A second private boundary, `RxOnlySpiDevice`, rejects `WriteBuffer` `0x0e`,
`SetTx` `0x83`, continuous-wave `0xd1` and continuous-preamble `0xd2` before
the physical SPI device. This defense is independent of the interface TX hook.
The initialization-only `SetTxParams` `0x8e` remains allowed.

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
commands, not driven as ESP32 outputs; no ESP32 GPIO is connected to PA_CPS.

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
future. On a returned radio error, the opaque wrapper removes its active owner,
attempts coordinated CTX/CSD/VFEM shutdown and best-effort reset assertion, then
returns a typed fault while remaining permanently faulted. The error preserves
its exact operation and initiating `lora-phy`/FEM code. Explicit prepare/receive
shutdown preserves an independent FEM cleanup failure, so that cleanup can no
longer overwrite the primary cause. Best-effort `Drop` and partially failed FEM
enable cleanup remain unobservable second-error paths. The task
publishes that bounded record, increments per-class counters and immediately
calls the ESP ROM digital-core software reset. The wrapper first attempts FEM
shutdown and SX1262 reset assertion, but neither a teardown result nor this
reset class proves that the RF path stays de-energized through ROM and boot;
powered capture must qualify that transition. This Phase-1 policy deliberately
chooses reboot instead of in-process reconstruction because the faulted wrapper
has consumed and dropped its private peripheral owners.

The 30-second TIMG0 MWDT is fed only after the main task completes one selected
wake, synchronous protocol maintenance and diagnostics pass. It catches a
panic, whole-executor stall or unexpectedly long synchronous ingress work. A
following `CoreMwdt0` boot transactionally counts that supervisor reset in the
same consecutive fault-reset streak as a returned radio fault, so a repeatable
watchdog crash loop cannot reconstruct and re-energize the radio indefinitely.
It cannot identify a radio task waiting indefinitely for DIO1 while the main
task continues normally; radio silence is itself a legitimate indefinite DIO1
wait.
BUSY is different: every target BUSY-low wait is bounded to 100 ms inside the
pin adapter. Timeout cancels only that GPIO wait, returns `RadioError::Busy`,
faults and tears down the opaque radio owner, and selects the reset policy; the
complete `LoRa::rx()` future is never externally cancelled.
Before logging or resetting, a returned radio fault is committed to a
dual-slot, CRC-protected journal in RTC fast persistent memory. The following
`CoreSw` boot acknowledges that pending record without double-counting it. A
`CoreMwdt0` boot with no pending returned-radio record commits one fault reset
before startup is authorized. Returned radio faults and supervisor-watchdog
resets therefore share one consecutive streak; reaching three enters RF-inert
quarantine before SPI, radio, executor timer or supervisor-watchdog
construction. Quarantine never times out; the intended operator recovery is a
verified cold power cycle. Ten continuous minutes after successful radio
activation clears a nonzero streak, with radio silence counting as healthy
operation. Torn, corrupt, degraded, ambiguous or unexpected-pending retained
state fails closed. ESP32-S3 reset reason `0x01` conflates a true power-on with
brownout/super-watchdog cases, and pinned `esp-hal` clears persistent RTC data
before `main` for that class. Powered HIL therefore cannot prove that cold power
removal is the *only* event that clears quarantine; this remains an explicit
hardware/HAL limitation alongside retention and inert-pin qualification.
If committing a returned-radio fault reports any write error, firmware does not
issue `CoreSw`: the radio owner is already inert, the supervisor watchdog is
disabled, and execution halts in the current powered session. This is necessary
because failed verification cannot prove that even the first poison store
reached retained memory.

Every successful PHY receive is offered with `try_send` to a statically
allocated depth-2 raw-frame channel. The message contains `[u8; 255]`, a valid
length, `FrameSignal` and a monotonic receive timestamp. The radio task performs
no RNode parsing and owns no 508-byte assembly buffer. If the ingress task is
behind, it drops the new raw frame, increments `channel_full_drops` and
immediately rearms RX instead of blocking with the radio out of receive mode.
Dropping either half of a split packet is permitted at this bounded handoff; the
ingress-side deadline ensures any unmatched half expires.

### 2. RNS ingress task

The ingress task exclusively owns a `ReceiveOnlyIngress` coordinator containing
`TimedRnodeRx`, its 508-byte caller-owned scratch buffer and an
endpoint-configured `EmbeddedNode` with the ephemeral identity. The task owns
the cryptographic RNG separately and lends it to each synchronous coordinator
wake. The constructor fixes the Rete role to endpoint and disables Link
admission on the primary destination; callers cannot select transport mode or
re-enable Links. It is the only task allowed to transform a physical frame into
an RNS packet.

The coordinator always reports one absolute monotonic wake: the earlier of a
pending fragment deadline and Rete's next five-second maintenance tick. The
target selects between that Embassy timer and the raw-frame channel. This
select never contains the PHY RX future. After either result it samples one
`Instant`, services expiry and due maintenance, and only then handles a frame.
Rebuilding both futures after each result cancels an obsolete timer when a
different-sequence first half replaces the pending fragment.

On a channel item, the task records the receive timestamp for latency
diagnostics. Coordinator expiry uses the current monotonic sample, while
`TimedRnodeRx::feed()` anchors a newly retained first half to the radio-capture
timestamp. The pre-age check guarantees that capture-based deadline is still
in the future; queue delay therefore cannot grant a fragment a second full
timeout.

When either wake expires a pending half, the coordinator retains the greatest
expired deadline as a watermark. Every queued frame captured at or before that
watermark is dropped, including both slots of the raw-frame channel, even when
the left-biased timer won one wake before the channel was drained. Future,
out-of-order and age-at-least-timeout timestamps are also rejected before RNode
parsing. This prevents an old same-sequence continuation from becoming a new
first half across timer/channel scheduling. The timeout is twice the configured
maximum 255-byte frame airtime plus a five-second scheduling guard, so
narrow-band profiles are not constrained by an arbitrary short constant.

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
applies to proofs, replies, forwarding, announce retransmission and every action
represented in the current project-owned `NodeActions`. The suppression helper
exhaustively destructures `NodeActions`; adding another field there therefore
fails compilation until this boundary is reviewed. Changes to upstream Rete
outcomes still require review at the owning adapter. The same helper destroys
output from both packet ingress and periodic Rete maintenance.

### Lab identity and entropy

The target-linked Phase 1 image creates a new ephemeral Reticulum identity on
each boot. It keeps ESP HAL's `TrngSource` active with the ESP32-S3 RNG and SAR
ADC1, obtains a `Trng`, fills the complete 64-byte Reticulum private-key form,
constructs the identity and explicitly zeroizes the caller-owned key buffer.
The source outlives every `Trng`, so ADC1 remains reserved and battery sensing
is disabled for this lab image.

For HIL only, the boot log emits the 64-byte **public** identity key, destination
hash and fixed destination name. A peer can therefore construct encrypted local
DATA and an expected-rejection LINKREQUEST for that boot. Private-key bytes are
never logged. This temporary discovery surface goes away when the authenticated
device API and durable identity service exist.

`Trng::try_new()` confirms that an entropy source is active but does not expose
a runtime entropy-health result. This is sufficient only for the non-persisted,
non-transmitting Phase 1 identity. It is not the production identity service,
does not replace first-boot atomic persistence or backup/recovery, and does not
settle the later ADC/entropy time-multiplexing and seeded-DRBG design.

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

Radio and ingress diagnostics are fixed-size snapshots of saturating counters
and last-known scalar values. The radio task records physical frames, bytes,
length, RSSI/SNR and receive time before queue admission, then publishes the
depth-2 handoff totals through a short critical-section-protected copy. The
ingress owner records raw-age/order/deadline rejection, complete RNode
outcomes, the independent 500-byte guard, Rete dispositions and rejection
classes, maintenance calls, observed events and every suppressed action.

Radio initialization and receive faults preserve an exhaustive scalar mapping
of the originating `lora-phy` error and an independent optional FEM cleanup
failure. The snapshot separates initialization and receive phases, classifies
SPI, IRQ, timeout, configuration and FEM failures, and records requested
target resets. The selected Phase 1 policy does not reconstruct the
radio in-process: a returned fatal radio error is logged once and requests a
digital-core software reset. Consequently `reinitialization_attempts` remains zero
in this image; a reboot starts a new volatile diagnostics snapshot while the
separate retained journal preserves the combined returned-radio and
supervisor-watchdog fault-reset streak and total.

The first and every 64th physical frame and channel-full drop are logged, as is
every RNS-admitted completed packet with its digest, plus a complete 60-second
heartbeat during silence. The
heartbeat repeats the public identity/destination, configuration-independent
radio and ingress counters, allocator total/current/free/maximum-use values,
and a second stack record containing reserved/usable/painted bytes, monotonic
high-water use, minimum remaining margin, guard offset/integrity and scanner
validity. These are live measurements, but qualification still requires their
capture under powered hostile traffic and soak.

`lora-phy` 3.0.1 observes SX1262 CRC/header IRQ flags internally but does not
return a CRC-specific count through its public RX result. Phase 1 reports that
counter as unavailable rather than inferring one from missing packets; exposing
it would be a separate focused driver contribution.

Periodic summaries are rate-limited. Raw frames, RNS payloads, identity private
material and decrypted application content are not logged. For HIL comparison,
only the SHA-256 of the exact completed RNS bytes is retained and printed.
Counter overflow saturates instead of wrapping.

## Current build evidence

On 2026-07-16, the preserved clean-tree `fdd6d9e` normal/pressure and exact
eight-artifact closure pair produced and verified the GNU `size` results below
with the pinned ESP toolchain. Each bundle's independent fresh-source,
fresh-target and fresh-Cargo-home canary rebuild matched its selected ELF and
merged image byte-for-byte, including under a hostile ambient environment.
The later clean `bf23cc5` normal image also passed independent reproducibility,
exact flash readback, and a 125-second supplemental smoke as recorded in
`artifacts/board-flashes/2026-07-16-e944-bf23cc5-rx-refresh/RESULTS.md`.
There is no matching `bf23cc5` closure bundle. These artifacts do not replace
the formal powered qualification matrix.

| Binary | text | data | bss/reserved | total |
| --- | ---: | ---: | ---: | ---: |
| default `safe-idle` | 57,587 | 4,044 | 468,780 | 530,411 |
| explicit normal `lab-rx` | 339,011 | 10,356 | 462,544 | 811,911 |
| `lab-rx-backpressure` | 341,071 | 10,364 | 462,536 | 813,971 |
| electrical LDO / unboosted | 339,107 | 10,356 | 462,544 | 812,007 |
| electrical LDO / boosted | 338,971 | 10,356 | 462,544 | 811,871 |
| electrical DC-DC / unboosted | 338,987 | 10,356 | 462,544 | 811,887 |
| electrical DC-DC / boosted | 338,971 | 10,356 | 462,544 | 811,871 |
| returned fault / `one-boot` | 341,615 | 10,396 | 462,504 | 814,515 |
| returned fault / `repeat-until-quarantine` | 340,039 | 10,388 | 462,512 | 812,939 |
| journal corrupt / slot 0, word 4 | 44,269 | 2,732 | 404,636 | 451,637 |
| journal torn / write ordinal 9 | 45,461 | 2,708 | 404,660 | 452,829 |

The safe image remains below its reviewed cap and its final defined-symbol
inventory retains no name matching a LoRa/SX126x radio operation. It also
retains the exact reviewed upstream `esp-rtos` runtime-source identity while
keeping all lab-only retained state absent.

Relative to the earlier 312,967/9,348/463,480/785,795 lab baseline, the current
receive-only façade, radio/fault/heap telemetry, raw-packet SHA-256, bounded
BUSY wait, retained reset quarantine, startup stack instrumentation, DIO1-wait
completion timestamp capture, corrected `esp-rtos` main-stack units and
compile-gated fixture surfaces produce a 26,116-byte aggregate increase in the
normal lab image. The current lab ELF has an exact 72-byte
`NOBITS` journal in `.rtc_fast.persistent`, an exact 32-byte `.noinit` stack
marker and a strong 74-byte leaf `__zero_bss` paint hook. The image remains
inside the recorded CI tripwires; these are link/static facts, not powered
retention or runtime-margin evidence.

The regular and instrumented builds used ESP Rust 1.95.0.0
(`rustc 1.95.0-nightly 95e5bda86`) and Xtensa GCC 15.2.0. A reproducible normal
lab command with compiler stack-size records is:

```sh
CARGO_TARGET_DIR=/tmp/reticulum-stack-20260715-current \
RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
RETICULUM_LAB_RX_FREQUENCY_HZ=915000000 \
RETICULUM_LAB_RX_SPREADING_FACTOR=7 \
RETICULUM_LAB_RX_BANDWIDTH_HZ=125000 \
RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR=5 \
RETICULUM_LAB_RX_PREAMBLE_SYMBOLS=18 \
RETICULUM_LAB_RX_EXPLICIT_HEADER=1 \
RETICULUM_LAB_RX_CRC=1 \
RETICULUM_LAB_RX_IQ_INVERTED=0 \
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --bin reticulum-heltec-tracker-v2-lab-rx \
  --no-default-features --features lab-rx \
  --target xtensa-esp32s3-none-elf
```

Repeating `-nostartfiles` is required because setting `RUSTFLAGS` replaces the
target rustflags in `.cargo/config.toml`.

The Tracker lab image deliberately instantiates the opaque façade
`ReceiveOnlyRete<16, 4, 32, 2>` around a private ingress owner: 16 paths, four
pending announces, 32 entries in each deduplication window and two Link slots.
Link acceptance remains
disabled. Two is nevertheless the minimum representable Link capacity in the
pinned Rete `HeaplessStorage` because its `heapless::IndexMap` cannot instantiate
capacity zero or one. This is an upstream storage limitation, not a decision to
remove Links or reduce the full product profile. Larger-board profiles retain
the generic capacity parameters.

The façade owner is initialized in place in a `StaticCell`; it does not become
a large temporary in the Embassy main future. The depth-2 channel and both task
pools are likewise static, the allocator backing remains 65,536 bytes, and the
full-stack modes retain the exact 72-byte RTC journal and 32-byte stack marker
described above. Pre-refactor owner-level symbol sizes have deliberately been
removed from this current section; the clean qualification bundle must record a
fresh complete owner/section inventory instead of inheriting stale values.

Fresh `inspect-elf` runs reported these largest compiler-emitted individual
frames:

| Mode | Maximum frame bytes |
| --- | ---: |
| normal `lab-rx` | 47,792 |
| backpressure | 47,872 |
| each of four electrical variants | 47,792 |
| returned fault / `one-boot` | 47,648 |
| returned fault / `repeat-until-quarantine` | 47,792 |
| RF-inert journal corruption/torn images | N/A; recorded as 0 because no executor or `.stack_sizes` section is linked |

CI rejects every radio-bearing image above the 49,152-byte per-frame drift
ceiling. These records are per-function and non-transitive: callees, interrupts,
executor/runtime frames and fault paths can be simultaneously live. The runtime
scanner supplies the needed high-water mechanism; powered hostile-traffic and
soak values remain the acceptance evidence. The journal images instead require
the runtime's default BSS hook, no stack marker, and no owned SPI, radio,
executor or watchdog definition.

CI now performs strict target Clippy, release link and `inspect-elf` coverage
for safe, normal, pressure, all four electrical configurations, both returned-
fault policies and representative journal selectors (corrupt slot 0/word 4 and
torn write 9). It rejects missing/invalid fixture environments, requires
distinct configured hashes, inventories non-absolute TX definitions and locks
the mode-appropriate retained/stack contracts. The caps are
65,536/6,144/475,136/550,000 bytes for safe-idle,
360,448/12,288/475,136/840,000 bytes for normal and electrical images,
364,544/12,288/475,136/845,000 bytes for pressure and returned-fault images,
and 65,536/6,144/419,840/500,000 bytes for journal images
(`text`/`data`/`bss`/aggregate decimal total). They are regression tripwires
with measured headroom, not proof of live heap, execution-stack safety or
powered retention.

The lab ELF's defined-symbol inventory found no retained named definition for
`LoRa::tx`, `do_tx`, `prepare_for_tx`, continuous-wave,
continuous-preamble, `WriteBuffer` or Rete interface `send`. It does contain
`set_tx_power_and_ramp_time`,
corresponding to the allowed non-transmitting `SetTxParams` initialization
described above. Rete's `flush_announces` code is linked through periodic core
maintenance, but its returned packets terminate at the exhaustively checked
suppression boundary and no radio-side consumer exists. Release LTO can inline
or merge a reachable function until its original name disappears, so this
inventory is only a regression clue. The SPI opcode firewall and exact mock
command traces are behavior-level software guards. Neither the target link nor
the mock evidence substitutes for the required logic-analyzer and on-air HIL
gates.

## Qualification gates

### Host gates

- `safe-idle` remains the default feature and its tests continue to prove no
  default frequency and TX disabled by default.
- `LabRxProfile` has no `Default`; missing frequency, invalid frequency and
  invalid modulation tests fail before the radio-power transition. Full-channel
  hardware bounds, RNode header/CRC/IQ requirements and known LDRO mismatches
  are covered explicitly.
- A mock async `SpiDevice` captures and exactly checks the ordered normalized
  cold-start and RX command stream for pinned `lora-phy` 3.0.1. Normalization
  omits only read/MISO response buffers; every deterministic outbound byte is
  retained. The trace contains private sync-word programming, DIO3 TCXO control
  `0x97` with voltage byte `0x02`, and DIO2 RF-switch control `0x9d 0x01`.
- The mock command stream may contain `SetTxParams` `0x8e`, because
  `lora-phy 3.0.1` performs that non-transmitting initialization. It must never
  contain `SetTx` `0x83` or a continuous-wave command.
- A private SPI firewall independently rejects `WriteBuffer`, `SetTx`,
  continuous-wave and continuous-preamble operations without forwarding the
  transaction to the underlying device.
- The reviewed wrapper surface contains no `send`, `tx`, `prepare_for_tx`,
  `continuous_wave`, inner-radio escape or CTX-high method, and the target
  symbol inventory finds none retained by name in this image. Firmware directly
  depends on the opaque `rns-rete-rx` façade, the board crate and the radio
  interface, not full Rete or `lora-phy`. The façade crate owns the RNode
  reassembly-plus-Rete receive composition; `reticulum-rns-rete` and
  `reticulum-node-core` remain independent of radio-interface and LoRa crates.
  `xtask graph-policy` enforces that
  manifest boundary across all three real feature sets and every target-specific
  dependency graph. A committed all-source AST snapshot records source-level
  public items, modules and re-exports plus inherent and explicit trait impls
  for project-declared public types across separate files and private inline
  modules, including an accidental `Deref` or `pub extern crate` escape. Local
  item-macro source and non-documentation attributes on recorded items are also
  retained, but the scanner does not expand macros. A separately compiled
  external-consumer contract proves approved façade construction still works while
  full-Rete/`lora-phy` imports and Rete send/inner plus radio TX/inner operations
  remain unavailable. Rete is pinned, but a future pin update must still review
  macro expansion and the resolved API of deliberately re-exported scalar
  types; project source syntax alone cannot see new inherent members added
  inside an upstream crate.
- A crate-private fault-injection test attempts the lora-phy TX path and proves
  the Tracker interface's transmit-switch hook returns an error before SPI sees
  `SetTx`, while CTX remains low.
- RNode planning/reassembly tests cover lengths 0, 1, 253, 254, 255, 256, 499,
  500, 501, 507, 508 and the impossible 509 boundary, plus duplicate,
  reordered, mismatched and expired fragments. `TimedRnodeRx` separately
  checks every completed length 501 through 508 at the RNS guard.
- The committed schema-3 RNode 1.86 HIL corpus records exact source/payload
  hashes for ordinary and promiscuous peer modes. Its 19 scenarios cover the
  released-Python announce and duplicate, header-only/1/253/254-byte single
  frames, 255/256/499/500-byte split packets, orphan/replacement/discard
  behavior, repeated and reordered same-sequence halves, and every 501–508-byte
  completion plus the feature-bound four-frame backpressure, one-boot returned
  fault and three-activation repeat-until-quarantine stimuli. Python tests
  verify KISS fragmentation, escaping, explicit peer
  configuration and the no-default RF authorization boundary; a Rust
  integration test replays every physical frame, wait, unstalled reference
  delta, packet digest and Rete disposition through the owning receive-only
  ingress. The pressure scenario separately records the instrumented target
  queue/expiry expectations so those intentionally different schedules cannot
  be confused.
- A separate host tool derives deterministic non-secret encrypted DATA from
  the public identity and destination hash printed by one exact Tracker boot.
  It rejects a mismatched destination before writing and its tests decrypt the
  resulting packet through a target holding the corresponding private key.
  This is a same-Rete-stack action-suppression fixture, not an independent RNS
  oracle.
- Integration tests prove 501 through 508 can be valid at the RNode physical
  boundary but never reach Rete. An exactly 500-byte split packet crosses the
  complete coordinator and increments the Rete ingress-call counter before a
  deliberately invalid GROUP LINKREQUEST is rejected semantically; its exact
  raw SHA-256 is checked independently.
- Coordinator tests prove silent expiry, exact frame/timer ties, continuously
  ready channel traffic, replacement of an obsolete deadline, distinct tick
  and transport-second domains, and one bounded maintenance call after delay.
- A normal Rete proof fixture proves packet actions are counted and destroyed
  by the same exhaustive helper used for receive-only ingress and maintenance.
  The actual target owner disables Links and uses `ProveNone`, so HIL validates
  local DATA event suppression and LINKREQUEST rejection rather than weakening
  those defense-in-depth policies merely to manufacture a packet action.
- Depth-2 raw-frame channel saturation has a deterministic drop-new result and
  cannot grow memory or block RX rearming; a dropped continuation leaves only
  bounded state that the ingress timer expires.
- The separately named backpressure artifact has a build-validated, one-shot
  async ingress stall. Its four-frame corpus is bound to that artifact mode and
  should yield exact target deltas `offered=3`, `queued=2`, `dropped=1` after
  servicing the pending fragment's original expiry. Software gates prove the
  mode separation; the deltas remain a powered HIL gate.
- Exhaustive radio-error mapping, simultaneous primary-plus-cleanup failure and
  saturating diagnostic tests prove both causes and all counters remain bounded.

### Target build gates

- Both the unchanged default `safe-idle` image and the explicit lab RX image
  build from one locked workspace for `xtensa-esp32s3-none-elf`.
- Every qualifying `esp-rtos`-based Tracker ELF contains the exact identity
  marker `esp-rtos-upstream-b50efcb-stack-words-v1`, proving it was built with
  the reviewed upstream source containing the CPU0 and CPU1 main-stack unit
  corrections. This applies to `safe-idle`, normal,
  pressure, electrical and returned-fault images. The RF-inert reset-journal
  HIL binaries use `esp_hal::main` instead and must not be rejected for lacking
  that marker.
- The safe-idle image still contains no radio initialization and retains its
  existing RF-inert boot behavior; size/map drift is explained.
- The lab RX image's ELF/map, static sections, reclaimed heap reservation,
  static task pools and compiler-emitted frames are recorded. The lab feature
  emits allocator total/current/free/maximum-use values at activation and every
  heartbeat. Its startup hook paints the shared CPU0/executor stack below the
  initial SP without touching the runtime guard; the portable scanner reports
  a monotonic high-water mark and sticky guard/scan validity each heartbeat.
  Actual values must still be captured on target.
- The lab-only dual-slot RTC-fast journal is linked at an exact guarded address
  and size. A returned radio fault is committed before the `CoreSw` reset, and
  a `CoreMwdt0` supervisor reset is counted transactionally at the next boot.
  The third combined fault reset quarantines before radio/SPI/watchdog
  construction, and a ten-minute healthy lease clears the streak. Host
  corruption/torn-write tests and link guards pass; powered retention and
  pin-state evidence remain.
- The ordinary lab image contains no backpressure trigger/completion strings.
  The separately linked `lab-rx-backpressure` image requires an explicit stall
  duration strictly beyond the active fragment timeout and no more than 25
  seconds, retains five seconds of watchdog service margin, and has independent
  size, link and TX-symbol gates. It is never the default or normal lab image.
- A 30-second TIMG0 main watchdog is enabled before radio initialization and is
  fed only after a completed main-owner wake. It resets whole-main/executor
  stalls; the following `CoreMwdt0` boot counts toward the shared quarantine
  streak. An indefinitely quiet DIO1 wait is deliberately not a fault while the
  main owner continues to run.
- Every BUSY-low wait is independently bounded to 100 ms inside the target pin
  adapter. A stuck BUSY line therefore becomes the typed radio fault/reset path
  without cancelling an in-progress DIO1/SPI receive sequence from outside.
- The linked RX owner has no retained non-absolute definition for `LoRa::tx`,
  `continuous_wave` or a bidirectional `ReteInterface::send`. Release
  dead-code elimination plus the structural API barriers make this a useful
  conservative audit, not a formal call-graph proof. A raw byte search for
  `0x83` is not sufficient because unrelated data can contain that byte; the
  symbol inventory is paired with the SPI command capture below.
- Existing `clippy::mem_forget` and large-stack protections remain enabled.

### Hardware-in-the-loop gates

- A logic-analyzer capture verifies the exact provisional power-up ordering,
  CTX never rises and SX1262 DIO2 directly owns the actual PA_CPS net. An
  optional unpowered continuity check may corroborate that header GPIO46 is a
  separate net, but it is not a firmware interlock gate.
- SPI capture from reset through initialization, packet reception, malformed
  traffic and idle soak contains no `SetTx` opcode `0x83`. `SetTxParams` `0x8e`
  is expected and explicitly allowed. DIO3 `0x97`/`0x02` and DIO2 `0x9d`/`0x01`
  must be visible.
- A known RNode or Python-controlled RNode transmitter sends single and split
  frames using the explicit matching lab profile. The Tracker reports correct
  completed bytes, RSSI/SNR and Rete ingress disposition through the 500-byte
  boundary.
- On-air monitoring finds no Tracker-originated packet during boot, successful
  RX, malformed traffic, local DATA, rejected LINKREQUEST, channel saturation,
  receive-error teardown/fault halt or a 24-hour receive soak.
- Power, CSD, CTX, reset and BUSY timing is captured on more than one Tracker
  sample. The provisional 5 ms delays are retained or changed based on the
  trace, not assumption.
- LDO and DC-DC modes are compared for initialization reliability, idle/RX
  current and receive behavior. Unboosted and boosted RX are compared for
  current and sensitivity with the KCT8103L path. Results are recorded before
  either setting becomes board policy.
- Allocator current/maximum use and the shared main/executor stack high-water
  mark remain stable during the soak and hostile-input run; both static async
  task pools remain intact. The pinned allocator does not expose a largest-free-
  block metric, so that value is explicitly unavailable rather than inferred.

## Closure-artifact implementation status

The powered fault and electrical comparisons must use separately named,
compile-gated artifacts. They must not be created by temporarily editing the
normal lab source, and none may weaken the receive-only SPI firewall.

### Returned receive fault

The implemented `lab-rx-returned-fault-hil` feature and binary require
`RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER=get-irq-status-after-set-rx` and
`RETICULUM_LAB_RX_RETURNED_FAULT_POLICY=one-boot|repeat-until-quarantine`, with
no defaults, and are mutually exclusive with the backpressure and electrical
variants. A target-only `SpiDevice` decorator wraps
the real `ExclusiveDevice` below the board-owned `RxOnlySpiDevice` firewall. It
forwards all operations, arms only after successfully forwarding the allowed
SX1262 `SetRx` transaction, and rejects the first following `GetIrqStatus`
before physical SPI. Because `GetIrqStatus` follows the real DIO1 event, one
benign peer frame drives the actual `TrackerRxRadio::receive()` error path,
interlock shutdown, owner drop, retained fault commit and `CoreSw` reset. The
expected primary failure is `Receive / Radio(Spi)` with no forced cleanup
failure. A one-byte sticky state distinguishes an injected failure from an
unrelated SPI error.

Host tests prove the wrapper's forwarding/fail point, the board integration
path and the still-outer opcode firewall. Build-time checks reject missing or
invalid trigger/policy configuration. `one-boot` arms only on a pristine
power-on history and holds RF-inert after its correlated reset;
`repeat-until-quarantine` re-arms on authorized boots until the ordinary third
fault quarantine prevents a fourth radio construction. The closure verifier
binds each policy's artifact identity, environment, ELF and merged image,
rejects cross-mode relabeling and inspects the final ELF. The preserved
`fdd6d9e` closure bundle supplies source/artifact provenance for that revision;
powered sticky-fired, reset and containment evidence remain open, as does a
closure bundle matching the later `bf23cc5` normal-image smoke.
Physical pin shorts are not an acceptable substitute.

### Retained-journal failure

The implemented RF-inert `lab-rx-reset-journal-corrupt-hil` and
`lab-rx-reset-journal-torn-hil` artifacts establish all fail-safe pin owners but
construct no SPI, radio, executor timer or watchdog. Corruption requires an
exact slot `0|1` and word `0..=8`; torn-write injection requires ordinal
`1..=9`. After a true-power baseline the selected image either flips that word
or runs the normal journal transaction through a private decorator that resets
after the selected aligned write. The next retained boot must enter the
ordinary fail-closed `CorruptOrTornJournal` quarantine before peripheral
construction. Mutation remains private to these features; the product API does
not gain a general retained-state corruption escape. Representative target
links and ELF inspection exist, and the preserved `fdd6d9e` closure bundle
contains exact slot 0/word 4 and write ordinal 9 artifacts. Their two-boot
powered captures are still required to prove RTC retention and quarantine
ordering; other selectors are outside that bundle.

### Regulator and receive gain

The implemented `lab-rx-electrical-hil` feature requires both
`RETICULUM_LAB_RX_REGULATOR=ldo|dcdc` and
`RETICULUM_LAB_RX_GAIN=unboosted|boosted`, with no defaults. A private-field
`TrackerRxConfiguration` is passed into `TrackerRxRadio::new`, producing four
distinct artifact identities from one code shape. Mock traces prove that DC-DC
adds only `0x96 0x01` after standby and before DIO2 control, LDO uses the reset
default without that command, and the gain write ends in `0x94` or `0x96`; all
other commands remain identical and TX-free. Each final variant still needs
its own common profile, interlock, stack, memory and no-TX checks. The preserved
`fdd6d9e` closure bundle contains and verifies a distinct ELF/image for every
variant, but powered measurements have not yet been collected.
These artifacts enable measurements; they do not select production policy.
Current, reliability and sensitivity comparisons still require a calibrated
fixture and more than one board.

## Implementation sequence

1. **Implemented:** add the separate lab RX build selection and non-default
   `LabRxProfile` validation without changing the safe-idle default.
2. **Implemented:** add the Tracker-private pin owner and `RxOnlyFem`, including
   corrected direct DIO2/CPS ownership, provisional timing constants and
   fail-closed shutdown/cancellation tests.
3. **Implemented and target-linked, not HIL-qualified:** add the exact async SPI
   device dependency and an opaque `TrackerRxRadio` around `lora-phy` 3.0.1.
   Prove its mock command stream and both independent TX barriers before
   hardware use.
4. **Implemented and target-linked, not HIL-qualified:** add the sole,
   non-cancellable radio task and its depth-2 `try_send` handoff of raw
   255-byte frames, signal metadata and monotonic receive time. Host tests
   prove the project-owned saturation policy preserves queued FIFO contents
   and drops the new frame with saturating diagnostics.
5. **Implemented and target-linked, not HIL-qualified:** add the coordinated
   ingress owner with `TimedRnodeRx`, the 508-byte scratch buffer, one
   channel-versus-deadline select, endpoint-only Rete ingestion, five-second
   maintenance and exhaustive outbound-action suppression. Host tests cover
   the deadline races, raw-frame age, clock domains and action boundary.
6. **Implemented and target-linked, not HIL-qualified:** add bounded radio-side
   frame/byte/signal/configuration diagnostics, exact compound fault reports,
   digital-core software-reset policy, bounded BUSY waits, main-owner MWDT,
   allocator telemetry,
   direct-dependency enforcement and the reviewed receive-only API snapshot.
   The target-linked dual-slot reset-storm quarantine and CPU0/executor stack
   watermark add the previously missing hardening mechanisms; their actual
   retention, inert-state and high-water values remain powered HIL gates.
7. **Target build and static inspection implemented; supplemental normal-image
   flash/readback smoke recorded; formal powered capture pending:**
   retain the safe-idle and normal lab images plus the separately named
   backpressure HIL artifact. Capture the first powered radio initialization
   with the antenna/load and lab instrumentation appropriate for the board,
   then restore the normal lab image after the one pressure scenario.
8. **Source, focused software/static evidence and clean-tree tooling
   implemented; one clean paired bundle preserved and powered capture pending:**
   add the protected
   returned-fault, retained-journal and four electrical comparison artifacts
   above. Host tests cover the returned-fault path and electrical trace matrix;
   all electrical/returned selections link and pass inspection, as do
   representative journal selectors. The preserved `fdd6d9e` closure bundle
   contains those exact eight artifacts. A matching `bf23cc5` closure and all
   corresponding powered evidence remain open.
9. Run single-frame, split-frame, malformed-input, backpressure and 24-hour RX
   HIL scenarios against an established RNode/Python peer.
10. Record FEM timing, regulator and RX-boost evidence. Update board policy only
   where those measurements support a conclusion.

## Exit criterion

Phase 1 RX is complete when the unchanged default image still boots RF-inert,
an explicitly configured lab image repeatedly receives and reassembles real
RNode traffic into the project-owned 500-byte Rete ingress boundary, all Rete
outbound actions are observably suppressed, memory and task stacks remain
bounded through the soak, CTX never leaves RX, and host plus HIL command traces
show no SX1262 `SetTx` `0x83` in any tested path while correctly allowing the
non-transmitting `SetTxParams` `0x8e` initialization command.

Completion permits the next engineering phase: firmware/RF integration and
qualification of the already-designed guarded transmit slice. It does not
authorize RF transmission or establish production FEM, regulator, RX-boost,
airtime, or regional policy.
