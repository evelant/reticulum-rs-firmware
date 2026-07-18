# Phase 1 exploratory Tracker transmit HIL

**Status:** exact LoRa PHY/RNode framing passed in both directions; the
historical deterministic ANNOUNCE passed ordinary-RNode/Python validation; and
one identical Rust/Rete image completed a powered two-board announce, encrypted
DATA and delivery-proof round trip. Product identity, durability, multi-hop,
LXMF and production TX policy remain open<br>
**Target:** two Heltec Wireless Tracker V2.3 boards, ESP32-S3FN8, SX1262 and
KCT8103L<br>
**Region/profile:** user-authorized NA915 bench HIL with antennas attached

## Scope

This lane first answered a narrow hardware question: can the Rust Tracker BSP
configure the SX1262 and external FEM so another RNode-compatible radio decodes
the exact bytes it transmits? It now also supplies a bounded same-image product-
surface Rete exchange. It remains separate from the product firmware and from
the formal receive-only qualification lane.

Every HIL mode is factory-eFuse-MAC gated to the two known lab boards and shuts
the radio down after its bounded operation. An unknown board never constructs
the radio. The sentinel and historical semantic-announce modes transmit no more
than their original one-frame budget; `semantic-roundtrip-hil` permits exactly
two completed transmissions on each board. The normal profile is 915 MHz, SF7,
125 kHz, CR 4/5, 24 preamble symbols, explicit header, CRC, normal IQ and the
private LoRa sync word. The board requests a 14 dBm antenna-path estimate while
programming the SX1262 for 0 dBm behind the Tracker's external PA.

The default feature-free image retains the 18-byte sentinel exchange,
deliberately shorter than the minimum valid RNS packet. A default-mode pass
proves only PHY and RNode physical framing. The mutually exclusive
`semantic-announce-hil` mode disables the sentinel responder and permits only
the known E9 initiator to construct, validate and transmit one signed announce.
The separately explicit `semantic-roundtrip-hil` mode runs the product
`reticulum-rns-rete` surface on both boards and selects the initiator/responder
role from the exact authorized eFuse MAC.

## Result

The observed matrix is now:

| Transmitter | Receiver | Result |
| --- | --- | --- |
| Rust Tracker | Rust Tracker | Exact 19-byte physical frame passed |
| RNode 1.86 Tracker | Rust Tracker | Exact raw PONG physical frame passed |
| Rust Tracker | RNode 1.86 Tracker | Exact 19-byte physical frame passed after fixing the peer's host-delivery regression |
| Rust Tracker semantic fixture | RNode 1.86 Tracker + pinned Python RNS 1.3.8 | Exactly one 167-byte ordinary RNode packet delivered; signed first-hop ANNOUNCE validated |
| Same Rust/Rete image on both Trackers | Same Rust/Rete image on both Trackers | Two signed ANNOUNCEs established both paths; encrypted DATA was decrypted by E0, which returned a delivery proof; E9's exact current receipt reached `Delivered` |

The decisive Rust-to-RNode pass is preserved at
`artifacts/board-flashes/2026-07-16-e040-rnode-safe-peer-irq-promisc-delivery-fix-diagnostic/hil/e944-normal-14dbm-to-e040-fixed-promisc/attempt-2-coordinated`.
E9 emitted exactly one post-marker `DRIVER_TX_DONE`. E0 returned exactly one
raw frame, `90 || "RETICULUM-HIL-PING"`, plus packet RSSI/SNR. Raw SX1262 IRQ
diagnostics recorded `PreambleDetected | HeaderValid` in the DCD path and
`RxDone | HeaderValid` in the DIO1 path, with no header or CRC error.

The E9 normal image SHA-256 is
`ca0526bb641de83f58df727b7e1689d52d006220212d0705607448959183a58e`.
The exact received frame SHA-256 is
`3d610b820708efb561820c946d7ca2d1cea3a39225e9176fc8e8a3796f31c3de`.

## Same-image Rete semantic round trip

The powered pass is preserved at
`artifacts/hil/tx-hil/20260716T230849Z-rust-rete-semantic-roundtrip/attempt-02-post-readback`.
This is a local, gitignored evidence bundle; this section is the committed
result record for fresh clones.
The exact same merged image ran on both boards. E9:44 selected the initiator
role and E0:40 selected the responder role; role selection did not change the
firmware bytes. Both instances used the product surface of
`reticulum-rns-rete`, not its conformance helpers. Graph policy verifies that
this mode includes the Rete core/stack/transport adapter and the board radio
owner, while excluding node-core, storage, device API and LXMF crates.

The bounded exchange was:

1. E9 sent a signed 167-byte RNS ANNOUNCE in one 168-byte physical frame.
2. E0 admitted it, learned E9's path and sent its own signed 167-byte ANNOUNCE
   in one 168-byte physical frame.
3. E9 admitted E0's announce, prepared encrypted DATA and sent 147 RNS bytes in
   one 148-byte physical frame. E0 decrypted the exact 36-byte payload:
   `RRH1` followed by the 16-byte initiator and responder destination hashes.
4. E0's configured inbound proof policy generated a 115-byte delivery proof in
   one 116-byte physical frame. E9 correlated it with the current prepared DATA
   receipt, observed `Delivered`, and ended with zero live receipts.

The strict dual-log validator cross-matched the packet hashes at every TX/RX
boundary. The DATA packet hash and receipt are
`4ca4ed5d856f45e1abb351762a3ccb8671c9c675a6bbfa082d73010746587a4d`;
the proof packet hash is
`9a46f631f80a129388408f2c9d90ec67c7345f18e1677fb2445227adbf4c42db`.
Each board reported exactly two driver TX completions, a terminal pass, and
radio shutdown. This is paired firmware serial and exact-readback evidence; the
same-image trial did not use an independent RF sniffer.

The application image is 360,208 bytes. Its complete 425,744-byte merged flash
image has SHA-256
`93ccac552d75a27f2cec571a9f00900210b4b862f157fca57c0cc50c9641fbc5`;
full-prefix readback from both E9 and E0 reproduced those bytes and that digest
exactly. The preserved ELF has SHA-256
`e85d88a8afbf89ea2392b42505abe637da946ca4448c0b5416a2e3c53925bd11`.
The decisive counted run followed those two full-prefix readbacks without any
intervening flash write, and its recorder performed only a normal-boot reset.

Both roles used a 64 KiB allocator and ADC-backed hardware TRNG. The observed
short-run heap peaks were 548 bytes on E9 and 764 bytes on E0. These figures are
useful regression checkpoints, not stack qualification, allocator-exhaustion
evidence, a sustained/soak memory result, or a full-product memory budget.
Likewise, the fixed public HIL identities are deliberate fixtures rather than
persisted production identities.

Earlier attempts under the pre-fix artifact root did not pass and remain
diagnostic only. The uncoordinated attempt missed the peer boot because host
tool delay exhausted E0's receive windows. The coordinated predecessor
exchanged both ANNOUNCEs and then exposed a time-domain implementation bug:
RNode fragment deadlines are expressed in high-resolution ticks while Rete
protocol time is expressed in seconds. The final image supplies those domains
separately, which is also the required permanent-runtime design.

Build this mode explicitly with:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-tx-hil \
  --no-default-features --features semantic-roundtrip-hil,tracker-radio \
  --target xtensa-esp32s3-none-elf
```

This proves direct endpoint path learning, encryption/decryption, proof
generation and receipt correlation across the real Rust radio path. It does
not prove production identity persistence or freshness policy, durable
submission/receipt state, reboot recovery, forwarding or multi-hop transport,
Links/Resources, LXMF, sustained memory behavior, formal RF qualification or
regional certification.

### Product-radio extraction regression

On 2026-07-16 EDT (2026-07-17 UTC) the proven SX1262/FEM implementation moved into
`reticulum-board-heltec-tracker-v2-radio`. The historical TX-HIL board crate
became a one-dependency compatibility facade; the frozen receive-only board
crate and its no-TX surface were unchanged. The product owner added an opaque,
explicitly selected configuration identity, low-level CAD with explicit
standby cleanup, and a DIO1-boundary RX timestamp without moving HIL role or
packet policy into the board layer. Its calibrated configuration remains
invariant under feature unification; the facade selects the separately exposed
near-field diagnostic value only when its own HIL feature is enabled.

Both boards were flashed with the same rebuilt semantic-roundtrip ELF and
started from coordinated USB-Serial/JTAG hard resets. The final run was piped
directly into the strict paired-log verifier, which cross-bound all four
semantic packets and returned `PASS`. E9 and E0 cross-matched DATA receipt
`63efcf518492d52597837ec59507c0d19db00e1309859627396c067a01185f00`;
the proof packet hash was
`8a5fb398b5f3d7bf003214bdc46312094ab4df14cc4d80c7f7805cb2654572de`.
Each role reported exactly two `DRIVER_TX_DONE` records, terminal `PASS`, and
`radio_active=false`. Short-run heap peaks remained 548 bytes on E9 and 764
bytes on E0. `espflash` reported a 361,728-byte application payload. The
6,399,116-byte ELF has SHA-256
`808ad808b0abf407f66399e1079f1fc11587ceaba2c15f038898631d39638392`.

This was a powered refactor regression, not a second readback-qualified
artifact bundle. It does not qualify CAD on air, a real CSMA dispatcher,
split-frame atomicity or the permanent firmware graph.

## Historical deterministic semantic announce slice

The explicit semantic build uses `reticulum-rns-rete`'s conformance-only
announce constructor with the committed Python-RNS 1.3.8 fixture: application
`testapp`, aspect `aspect1`, zero test entropy and Unix time `1700000000`. The
result is exactly 167 RNS bytes with destination
`2b7fa6842783252974dc5fcaff22b808` and full packet hash
`b63705cf3ed52d56e32e8e17fbd86f51f391b9ce86a1a38f0f3649c058e74cae`.
Before the radio sees any bytes, the firmware reparses the result, validates
its Ed25519 signature and destination derivation, and checks the committed
header, identity fields, destination and packet hash. Any drift returns without
transmitting. The RNode framer must then produce one 168-byte physical frame;
a split result is also a no-transmit failure.

### Powered historical result

The coordinated pass is preserved at
`artifacts/hil/tx-hil/20260716T183805Z-e944-rete-announce-to-e040-rnode/attempt-02-coordinated`.
After the listener was ready and E9 received its test reset, the E9 log records
one 167-byte RNS packet framed as one 168-byte physical frame with RNode header
`0x90`, exactly one `DRIVER_TX_DONE`, no retry, then `radio_active=false` and a
permanent inert hold. Its logged destination and packet hashes match the
committed fixture.

E0 ran RNode 1.86 in ordinary `rnode_packet` mode with promiscuous receive off.
Its peer-to-host `CMD_DATA` stream contained exactly one packet observation,
exactly one expected match and no mismatches. RNode had already removed the
physical header, so the delivered payload is exactly the 167 RNS bytes, with
SHA-256
`74dd63d749a9df03f2d315d3bf8ee5568d13a1ebbbd55f380392e3eff9b93080`.

The pinned Python RNS 1.3.8 `RNS.Packet`/`RNS.Identity` implementation parsed
that delivered packet as a zero-hop, HEADER_1, broadcast-transport, SINGLE
destination ANNOUNCE with context zero. It validated the Ed25519 signature,
public identity, destination/name-hash binding, destination
`2b7fa6842783252974dc5fcaff22b808` and full packet hash
`b63705cf3ed52d56e32e8e17fbd86f51f391b9ce86a1a38f0f3649c058e74cae`.
The manifest result is `valid_expected_first_hop_announce`.

The semantic application is 232,160 bytes; its complete merged flash image is
297,696 bytes with SHA-256
`8ba474d35527fd2ee4c906ee4b2fa26b2692e11342761371ac0676664df0f459`.
Before the run, an exact 297,696-byte merged-flash readback reproduced that
digest byte-for-byte. The preserved ELF is 4,102,372 bytes with SHA-256
`e6d48583d4eba42a4bd489c13b193ff85447a8f7665a05cdb416bc609e552bf7`.
The image booted and completed on the no-PSRAM Tracker, but this short fixture
did not measure heap or stack high-water marks and is not a sustained-load
memory qualification.

The fixed private key, zero RNG and old timestamp are public test material.
They are compiled only by `semantic-announce-hil` and are not a product
identity, entropy source or clock design. The safe default graph still excludes
Rete entirely. Graph policy separately checks that the semantic graph enables
only the RNS adapter's `conformance` surface and does not pull in node-core,
durable TX ownership or LXMF packages.

The Python validator did not start a full Reticulum instance. This result is
therefore not live Python Transport path admission, and the bytes did not enter
the product `NodeCore` receive path. It does not validate a persisted production
identity, announce scheduling/freshness, transport-role admission or
forwarding, multi-hop routing, proofs/Links/Resources, LXMF, durable submission
or a production regional/airtime policy.

Build the historical semantic-announce image explicitly with:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-tx-hil \
  --no-default-features --features semantic-announce-hil,tracker-radio \
  --target xtensa-esp32s3-none-elf
```

This exact historical mode was linked, host-vector tested and accepted for the
narrow powered semantic evidence above. It remains a conformance HIL, not a
product firmware profile; the later same-image round trip is the current
product-surface semantic result.

## Why the first Rust-to-RNode trials looked asymmetric

Frequency sweeps, reduced Rust TX power, RNode RX-gain changes, TCXO-only and
FEM timing experiments all produced a correctly timed DCD burst but no host
frame. Query-only SX1262 IRQ instrumentation then showed a complete,
CRC-clean decode.

The actual defect was in the external reference peer. RNode commit
`1a2e42d93ce9cea42c47fa1c4282abf00db96b33` migrated ESP32/nRF52 receive
delivery from a shared `packet_ready` buffer to `modem_packet_queue`, but left
promiscuous receive setting the old flag. No code consumed it. The local
reference patch now queues both ordinary and promiscuous packets through one
helper and removes the dead flag. This is a peer receive/host-delivery fix, not
a workaround in the Rust radio path.

The project product architecture already avoids this class of defect: the sole
radio owner moves a complete owned `RawReceivedFrame` into a bounded channel,
and raw monitoring should tap that same stream instead of creating a second
completion flag.

## Current board state

- E9:44 and E0:40 now contain the same final explicit-configuration
  `semantic-roundtrip-hil` image used for the 2026-07-16 EDT powered regression.
  The earlier readback-qualified 425,744-byte image and its SHA-256 remain
  preserved in the historical artifact bundle described above; the current
  image was flash-verified by `espflash` but not independently read back.
- E9 selected the initiator role and E0 selected the responder role from their
  exact eFuse MACs. In the current regression, each finished after two TX
  completions with the radio shut down. Neither board is running the earlier
  RNode peer image.

## Historical next slice and current remaining gates

When this exploratory Tracker HIL concluded, its immediate next slice was fixed
ordinary-action ownership plus a real CAD/RNode dispatcher, followed by
permanent node composition. That software work is now complete in the E290
target: `NodeInterfaceSupervisor` owns the router, DATA and ordinary
coordinators and per-actor permit services, while a separate LoRa task owns the
ticket-aware dispatcher and E290 radio. The permanent image passes its
software gates and now has a bounded two-board powered smoke for exact image
readback, boot, erased credentials, journal/LoRa/interface readiness, and
ordinary one-frame TX. Both physical E290 modules are confirmed `HT-RA62-HF`,
and the separate semantic image passed its powered functional HIL. Both attached antenna-equipped Tracker boards remain cleared for
NA915 development TX/RX and remain the regression fixture.

1. **Complete in the E290 source graph:** give the permanent node task sole
   ownership of `EmbeddedNode`, keep RNode microseconds separate from Rete
   seconds, and connect timed RX plus ordinary ingress/tick actions through the
   sealed interface fabric.
2. **Partially complete:** the resident storage actor/runtime now owns the
   product journal; connect the API adapter's external lane so a send is durable before Rete preparation, while proof
   and timeout outcomes become durable before terminal acknowledgement.
3. **Composed with bounded powered smoke:** the registered-buffer,
   supervisor/permit, regional/airtime, and sole-radio ownership path booted and
   emitted ordinary frames on both boards. Add a local durable DATA/LXMF
   submission surface, then reproduce the controlled peer exchange through the
   permanent E290 graph as its full powered qualification.
4. **Partially complete:** returned ordinary actions now enter a fixed pool and
   ticketed router path without loss under downstream pressure. Caller-reservable
   construction before Rete allocates and mutates remains open.
5. **Open:** complete formal powered receive/electrical/fault/retention/soak
   qualification. This exploratory semantic pass does not substitute for those
   gates or production regional certification.

No upstream issue or pull request was opened.
