# Phase 1 exploratory Tracker transmit HIL

**Status:** exact LoRa PHY/RNode framing passed in both directions, and the
deterministic signed RNS ANNOUNCE passed a powered ordinary-RNode delivery plus
pinned Python-RNS 1.3.8 first-hop validation; product identity, live node
admission/routing, LXMF and production TX policy remain open<br>
**Target:** two Heltec Wireless Tracker V2.3 boards, ESP32-S3FN8, SX1262 and
KCT8103L<br>
**Region/profile:** user-authorized NA915 bench HIL with antennas attached

## Scope

This lane answers a narrow hardware question: can the Rust Tracker BSP configure
the SX1262 and external FEM so another RNode-compatible radio decodes the exact
bytes it transmits? It is separate from the product firmware and from the
formal receive-only qualification lane.

The HIL image is factory-eFuse-MAC gated to the two known lab boards, transmits
at most one frame per authorized initiator boot, and shuts the radio down after
its bounded operation. An unknown board never constructs the radio. The normal
profile is 915 MHz, SF7, 125 kHz, CR 4/5, 24 preamble symbols, explicit header,
CRC, normal IQ and the private LoRa sync word. The board requests a 14 dBm
antenna-path estimate while programming the SX1262 for 0 dBm behind the
Tracker's external PA.

The default feature-free image retains the 18-byte sentinel exchange,
deliberately shorter than the minimum valid RNS packet. A default-mode pass
proves only PHY and RNode physical framing. The mutually exclusive
`semantic-announce-hil` mode disables the sentinel responder and permits only
the known E9 initiator to construct, validate and transmit one signed announce.

## Result

The observed matrix is now:

| Transmitter | Receiver | Result |
| --- | --- | --- |
| Rust Tracker | Rust Tracker | Exact 19-byte physical frame passed |
| RNode 1.86 Tracker | Rust Tracker | Exact raw PONG physical frame passed |
| Rust Tracker | RNode 1.86 Tracker | Exact 19-byte physical frame passed after fixing the peer's host-delivery regression |
| Rust Tracker semantic fixture | RNode 1.86 Tracker + pinned Python RNS 1.3.8 | Exactly one 167-byte ordinary RNode packet delivered; signed first-hop ANNOUNCE validated |

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

## Deterministic semantic announce slice

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

### Powered semantic result

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

Build the semantic image explicitly with:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-tx-hil \
  --no-default-features --features semantic-announce-hil \
  --target xtensa-esp32s3-none-elf
```

This exact mode has now been linked, host-vector tested and accepted for the
narrow powered semantic evidence above. It remains a conformance HIL, not a
product firmware profile.

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

## Board state after testing

- E9:44 booted the semantic announce HIL for the preserved run and ended with
  the radio shut down in its permanent inert hold. This attempt contains no
  post-run flash readback or restoration record, so it makes no stronger claim
  about E9's current flash contents.
- E0:40 ran the safe-peer RNode application in ordinary receive mode; the
  listener queried firmware 1.86, board 69, platform 128 and MCU 129. The
  previously preserved application SHA-256 remains
  `7dcb75daa3c47afedbaab25c4b1e2f2bbf9ea1416ad5fadd1ba8ebe7f19688b9`,
  but this attempt did not perform a new post-run readback.

## Next bounded product slice and remaining gates

The immediate product-code slice remains RF-inert: build the sole permanent
storage actor around the implemented two-bank journal, serialize projector
plans and commit/readback acknowledgements through it, and expose device-API
acceptance only after the durable transition succeeds. This closes the current
persist-before-accept gap without confusing a successful RF fixture with a
durable product send.

1. Merge RX ingress, `NodeCore::ingest()`/tick actions, durable submission
   projection and exact acknowledgements under the eventual sole node owner.
2. Then run the first production-path RF slice: learn a peer from a live
   announce, prepare one bounded encrypted DATA packet in a registered external
   buffer, and carry it through the existing supervisor, permit typestates,
   real regional/airtime policy and sole radio owner. The Python peer must
   decrypt the DATA and return a proof; no conformance constructor or direct
   frame bypass belongs in that path.
3. Convert remaining allocation-backed RNS actions into caller-reservable
   packet ownership before enabling announce/proof/forwarding bursts, then test
   ordered traffic and queue pressure with raw monitoring as an observation tap.
4. Complete formal powered receive/electrical/fault/retention/soak
   qualification. This exploratory semantic pass does not substitute for those
   gates or production regional certification.

No upstream issue or pull request was opened.
