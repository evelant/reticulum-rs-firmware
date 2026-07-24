# E290 same-Link reuse and direct-replay powered proof

**Date:** 2026-07-24

**Status:** complete for one bounded two-board run. A freshly booted sender
accepted two direct-only LXMF submissions before either terminal status was
queried. The submissions had different device idempotency keys but identical
LXMF material. Both reached durable `Delivered` with distinct Reticulum packet
hashes, while the receiver imported exactly one new LXMF message.

This run exercises the per-Link single-flight and receiver replay paths added
after the [stale-Link recovery proof](e290-stale-link-recovery-powered-proof.md).
The portable regressions provide the exact internal evidence: one retains the
same `LinkHandle` across two successful attempts and prevents the second from
preparing until the first terminal is durably acknowledged; another sends the
same LXMF wire twice over one continuously active real Rete Link and observes
`New` followed by `Replay` with no second store write.

The physical run used the macOS host: authenticated USB to sender A and the
existing CoreBluetooth appliance service for receiver B. `MetalbeardMobile`
was available again but was not required. USB and BLE were local device-API
bearers, not Reticulum packet interfaces; board-to-board traffic used NA915
LoRa.

## Source and artifact binding

The powered image was built from the uncommitted
`codex/lxmf-delayed-proof` checkout based on
`88a5ddc4d717ab6ce3a62ceb8fcd7c3c930b043f`. Its Rete dependencies were pinned
to `a443173b0829c2637ce23531a8cde15fdfec185e`. The exact artifact and
identity-qualified address-zero readback were:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| E290 ELF | 14,882,120 | `8197e8cc0ace9cf1f9c8120a480d81a44f8b740485c234f04ef5477f589d6535` |
| E290 merged firmware | 948,208 | `e98cf75f3e7cce395fdd1a7d7855c86cb0fe5b0392eae83920aa69e24924fce9` |
| Sender address-zero readback | 948,208 | `e98cf75f3e7cce395fdd1a7d7855c86cb0fe5b0392eae83920aa69e24924fce9` |

The flash helper verified sender A as ESP32-S3 eFuse MAC
`AC:A7:04:E1:3E:88`, 16 MiB flash, disabled secure boot and flash encryption,
and the confirmed `HT-RA62-HF` radio module. Receiver B remained on the
previously qualified current image and was not reflashed; the changed
single-flight policy is sender-side.

| Role | EUI-48 | Local bearer | LXMF delivery destination |
| --- | --- | --- | --- |
| Sender A | `AC:A7:04:E1:3E:88` | USB | `03869ee76b74d1e2a4626f0c02ae3248` |
| Receiver B | `AC:A7:04:E1:3F:88` | BLE | `935caba93f7cd97c7c6658350ac02b45` |

The ignored evidence directory is
`target/private-e290-proofs/same-link-replay-20260724`. It contains the exact
merged image, identity-bound flash records, and exact readback. It contains no
copied credential.

## Two submissions, one LXMF message

Both submissions used:

| Field | Value |
| --- | --- |
| Destination | `935caba93f7cd97c7c6658350ac02b45` |
| Timestamp ms | `1784940000000` |
| Title | `R` (one byte) |
| Content | 295 bytes of ASCII `S` |
| Complete normalized LXMF wire | 408 bytes |
| Destination-stripped carrier | 392 bytes |
| LXMF message ID | `9692c4fbe855b9439cf97d43819f41b5b54dacd36e73e52927dbe6e66620830c` |

The 392-byte carrier is one byte above the 391-byte opportunistic ceiling and
the 408-byte complete wire remains within the 431-byte Link MDU. Neither
submission could fall back to opportunistic destination DATA.

The host issued two `lxmf-send` operations back-to-back. Both returned durable
acceptance in a combined 4.6 seconds, before the later status queries:

| Submission | Device idempotency key | Message ID |
| ---: | --- | --- |
| `6` | `0102030405060708090a0b0c0d0e0f31` | `9692c4fbe855b9439cf97d43819f41b5b54dacd36e73e52927dbe6e66620830c` |
| `7` | `0102030405060708090a0b0c0d0e0f32` | `9692c4fbe855b9439cf97d43819f41b5b54dacd36e73e52927dbe6e66620830c` |

This distinction is essential. Reusing the first device idempotency key would
only replay sender-journal acceptance and would emit no RF. A different key
created a second durable sender submission, while the identical destination,
timestamp, title, content, source identity, and deterministic signature
produced the same LXMF wire and message ID.

## Sender and receiver results

Both sender submissions reached durable `Delivered`. Their encrypted
Reticulum packets were distinct:

| Submission | Terminal | Packet bytes | Encoded packet SHA-256 |
| ---: | --- | ---: | --- |
| `6` | `Delivered` | 483 | `31a6a04c2a0fe4d373858cb90767a3591300d7b75c57ce70df380fade4f5cf01` |
| `7` | `Delivered` | 483 | `8dcc4b3b1a4f4d955503bcd37fd58790412d9398e0f625f0f92440192ba8e7e1` |

Receiver B's continuously running authenticated appliance projection had 11
inbound rows with maximum device sequence 13 before the run. After both sender
terminals, it had 12 rows with maximum sequence 14. The one new row was:

| Receiver field | Value |
| --- | --- |
| Device sequence | `14` |
| Message ID | `9692c4fbe855b9439cf97d43819f41b5b54dacd36e73e52927dbe6e66620830c` |
| Timestamp ms | `1784940000000` |
| Title/content lengths | `1` / `295` |

The first packet therefore produced the durable message; the fresh second RF
delivery returned another valid proof to the sender without producing another
receiver message.

## Exact source qualification

The device API intentionally exposes submission status and packet digest, not
opaque Link handles or the receiver's internal commit-kind enum. The following
properties are consequently source-qualified and exercised by the powered
sequence rather than independently telemetered on the boards:

- `NodeCore::link_has_unacknowledged_attempt()` reports both `Active` and
  unacknowledged `Terminal` attempts for one exact Link.
- `SubmissionRuntime` parks a direct-required follower with
  `DirectLinkAttemptBackpressured`, does not open a second same-destination
  Link, and keeps the follower parked through terminal durability and exact
  acknowledgement.
- The integrated runtime regression then prepares the follower on the same
  exact `LinkHandle`; both real Link-DATA attempts reach `Delivered`.
- Eligible short LXMF can still proceed opportunistically, and work on another
  reusable Link remains schedulable while one Link is busy.
- A follower already waiting when its predecessor reaches
  `DeliveryTimeout` remains parked through durable retirement and then requests
  a fresh Link instead of selecting the retired handle.
- The durable-ingress regression sends one exact Python-compatible wire twice
  over one continuously active Rete Link. It observes `New` then `Replay`,
  preserves one store record and the original durable receipt, performs zero
  NOR writes for replay, and releases two distinct retained proofs covering two
  distinct Link-DATA packet hashes.

Combined with the exact source regressions, the powered facts—fresh sender
boot, two accepted direct-only submissions, identical LXMF IDs, two distinct
delivered packet hashes, and one receiver record—exercise those paths. They do
not add a new telemetry field merely for the proof.

## Qualification boundary

This record closes the bounded alpha gaps for successful outbound-initiator
same-Link reuse and direct receiver replay. It does not qualify sustained or
simultaneous multi-Link traffic, responder/backchannel reuse,
initiator/backchannel direct receive, Link-table exhaustion on powered
hardware, multi-hop routing, Resource transfer, propagated LXMF, electrical
power cuts, allocation pressure, or soak. The timeout-recovery proof remains
the separate record for retirement after peer reboot.
