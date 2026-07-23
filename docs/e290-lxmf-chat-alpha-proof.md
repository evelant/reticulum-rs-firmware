# E290 LXMF chat-alpha powered proof

**Date:** 2026-07-22

**Status:** complete for one persistent USB-client exchange in each direction,
plus the 2026-07-23 host BLE-service composition addendum below. The USB
submissions reached durable Reticulum `Delivered`, each peer imported the same
LXMF message identifier and exact semantic content into SQLite, repeated inbox
synchronization was duplicate-free, and every separate CLI process
authenticated without a USB reset.

This is a bounded proof of the current 128-entry E290 image and the
`reticulum-lxmf-chat` command-line workflow, followed by a bounded
host-CoreBluetooth appliance-service proof. It is not a 128-message fill, soak,
multi-hop, electrical power-cut, propagation-node, NomadNet, installed native
Expo, or Wi-Fi qualification.

## Artifact and board binding

The ordinary Xtensa ELF was 13,998,484 bytes with SHA-256
`a3d7141f8eae09d2c98b8daedbf4dd868785fb113a902b783c4683fcbf823480`.
The runtime-measurement HIL ELF was 14,166,472 bytes with SHA-256
`7384fa7518e5b29f80a1b0a074923fbd8a2df30795d6fa71e40e6475e3fdef30`.
The explicit 16 MiB merged image was 893,408 bytes, used
827,872/6,291,456 application bytes, and had SHA-256
`c9f70e6d6dbacc52e77e6adf3ab56a6f5447e870915773ae1dce27ed4d0072c3`.

The identity-safe helper bound each write to the expected USB serial, MAC,
16 MiB flash, and fitted `HT-RA62-HF`. Exact 893,408-byte address-zero
readbacks matched the image digest on both boards before either application
was launched.

| Board | USB serial / MAC | Primary destination | LXMF delivery destination |
| --- | --- | --- | --- |
| A | `AC:A7:04:E1:3E:88` / `ac:a7:04:e1:3e:88` | `c99e8ff1ec8629e4e1290e14462ae8af` | `03869ee76b74d1e2a4626f0c02ae3248` |
| B | `AC:A7:04:E1:3F:88` / `ac:a7:04:e1:3f:88` | `83a09ed807a0a7c631386deaa0448fb9` | `935caba93f7cd97c7c6658350ac02b45` |

After launch, each board returned that same identity twice through two
consecutive authenticated CLI processes on one unchanged USB enumeration.
This directly exercises idle established-session replacement; no reset or
re-enumeration was used between chat commands.

## Retained-frame scheduler regression and recovery

The immediately preceding powered image transmitted Board B submission 3 and
Board A committed its exact LXMF message, but B remained durably `Preparing`.
Submission 4 then remained `Queued`. The sole radio dispatcher was retaining
the transmitted DATA completion until its frame observation became durable,
while an ordinary packet queued behind it; the node storage lane incorrectly
waited for that ordinary owner to become quiescent first.

Current source gives the exact retained DATA frame priority over scheduler
quiescence while preserving storage retry timing. A host regression routes an
ordinary announce behind a physically transmitted DATA frame and proves that
frame persistence, durability acknowledgement, and completion still advance.
On first boot of this corrected image, the old interrupted submission 3 became
explicit `Failed(Internal)`, and formerly blocked submission 4 advanced to
`Delivered`. No accepted owner was silently discarded or resubmitted.

## Fresh bidirectional chat exchange

Each client used a separate fresh SQLite database. `send` committed exact
timestamp, idempotency material, title, and content before device I/O; later
`reconcile` calls updated the same row from the authenticated device status.

| Direction | Device submission | Timestamp ms | Message ID | Packet bytes / SHA-256 | Terminal status |
| --- | ---: | ---: | --- | --- | --- |
| A to B | 4 | `1784755005750` | `7791e04917ffe09a1256861a8658ab675d9b537f57da75bb0ef26a09b439cf50` | 259 / `c7da77ce77328f171ca18204d6065acda61b6ecff2a269e0d52f1eb1ff61072e` | `Delivered` |
| B to A | 5 | `1784755069749` | `5009526f9b180c374226e8a8ec61bc4c1671ca5bfe5e42c1c09a82f0e3e8d69b` | 259 / `94a5cd38bf5ab6e91caee9034dd4b94a72d435f0f44edd162821a3289682b87d` | `Delivered` |

Board B imported A's message with source
`03869ee76b74d1e2a4626f0c02ae3248`, local destination
`935caba93f7cd97c7c6658350ac02b45`, title `scheduler-fix-a`, and content
`A to B after retained-frame scheduler fix`. Board A imported B's message with
the endpoints reversed, title `scheduler-fix-b`, and content
`B to A after retained-frame scheduler fix`. In each case the receiver's
timestamp and message ID exactly matched the sender's committed outbox row.

A second `sync` on each board reported `inbox_inserted=0` and
`inbox_duplicates=4`, confirming stable message-ID deduplication across
separate authenticated client sessions.

## BLE-bearer composition proof

**Date:** 2026-07-23

**Status:** complete for the long-running macOS host service in both
directions. This does not qualify an installed iOS/Android application.

The host appliance service's transport-neutral BLE connector reused the same
durable chat actor and `DeviceClientSession` as USB. It derived each exact E290
advertisement from that board's activated credential, opened the bounded
CoreBluetooth stream, authenticated BLE suite 3, and published ordinary
`BluetoothLowEnergy` connection metadata. The device-ID and advertised-name
derivation now have one shared `no_std` contract used by firmware, the host
connector, and native Expo credential import.

Before starting the services, the one-shot direct adapter independently
selected and authenticated both boards:

| Board | CoreBluetooth peripheral | Local name | Suite-3 authentication / total |
| --- | --- | --- | ---: |
| A / `3E:88` | `5ce4da26-f2aa-5474-0a6d-e8d4f1539d38` | `reticulum-e290-e13e88` | 8,080 / 8,620 ms |
| B / `3F:88` | `44f4ce46-9403-c317-c0aa-8185e33bb605` | `reticulum-e290-e13f88` | 8,584 / 9,124 ms |

Two service processes then owned the boards concurrently over BLE and each
settled in `ready` with its credential-bound public device ID and the expected
primary and LXMF destinations. Fresh SQLite databases retained the proof:

| Database | SHA-256 |
| --- | --- |
| `board-a.sqlite3` | `779e32689b3907de5b5832d5d0c36590dc0603ef4e3cfa70577b8d739fbc914c` |
| `board-b.sqlite3` | `e8ada2aada69ca482d5161c58863dfcc5595e42b226aeb032146ec8c3b82684e` |

The databases are local evidence under
`/private/tmp/reticulum-host-ble-service-proof-20260723.OBbh8F`. The
ephemeral HTTP capability cookies were deleted after both services shut down
cleanly.

Starting one send on each board at effectively the same instant produced one
successful B-to-A exchange and one explicit A-to-B
`failed_delivery_timeout`. Both terminal outcomes remained durable. A
half-duplex collision is a plausible explanation for the asymmetric result,
but this run did not instrument RF timing closely enough to prove that cause.
The failed row was retained and was not rewritten as success.

A later sequential A-to-B send passed. The final bidirectional evidence is:

| Direction | Timestamp ms | Message ID | Title | Terminal / peer result |
| --- | ---: | --- | --- | --- |
| B to A | `1784842341893` | `dc292c0257fba994a434498dc7e7bb2270415a6dfa0e4ff2e63ab31802d42386` | `host-ble-b-to-a` | sender `Delivered`; exact inbound peer import |
| A to B, sequential | `1784842429517` | `a4334ad7e4aa488b602a9a57bef760eb11c8e216ea205b8ee83fcd6db49c77f1` | `host-ble-a-to-b-sequential` | sender `Delivered`; exact inbound peer import |

Each peer retained the identical timestamp, message ID, UTF-8 title, and UTF-8
content shown by its sender. The B-to-A content was
`Board B BLE service to Board A over LoRa`; the sequential A-to-B content was
`Board A BLE service to Board B over LoRa after collision`.

After the connector added its final profile-key/credential EUI cross-check,
the rebuilt source again held both boards concurrently in `ready`. Their
compatibility projections now reported `ACA704E13E88` and `ACA704E13F88`
instead of an opaque credential-ID label, both retained the same authenticated
device and Reticulum destinations, both reported zero error, and neither
re-imported an already-known message. This last rerun performed no new RF send;
the exact bidirectional rows above remain the bearer-composition evidence.

This proves the combined host BLE to authenticated device API to E290 LoRa to
peer durable LXMF inbox to host BLE path. It does not turn the native bridge
constituent tests into a powered Expo phone result, and it does not qualify
simultaneous bidirectional scheduling, background BLE, reconnect under phone
lifecycle changes, multi-hop, propagated delivery, pressure, or soak.

## Static and host gates

The qualified source passes 176 serial E290 host-library tests with the
documented 16 MiB fixture stack, strict host Clippy, default and
runtime-measurement-HIL Xtensa release builds, and the linked stack inspector.
The measured default mount/append/compact chains are
53,072/52,816/52,704 bytes; the HIL chains are
53,248/53,040/52,928 bytes. Every chain also fits the 4,096-byte policy
reserve within its linked CPU stack.

Local flash/readback records and the two SQLite proof databases are under
`/private/tmp/reticulum-lxmf-chat-alpha-deadlock-fix`. They contain no active
credential file, but the path is a local evidence reference rather than a
portable repository artifact.
