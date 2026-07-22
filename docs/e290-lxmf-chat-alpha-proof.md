# E290 LXMF chat-alpha powered proof

**Date:** 2026-07-22

**Status:** complete for one persistent USB-client exchange in each direction.
Both submissions reached durable Reticulum `Delivered`, each peer imported the
same LXMF message identifier and exact semantic content into SQLite, repeated
inbox synchronization was duplicate-free, and every separate CLI process
authenticated without a USB reset.

This is a bounded proof of the current 128-entry E290 image and the
`reticulum-lxmf-chat` command-line workflow. It is not a 128-message fill,
soak, multi-hop, electrical power-cut, propagation-node, NomadNet, BLE, or
Wi-Fi qualification.

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

## Static and host gates

The qualified source passes 163 serial E290 host-library tests with the
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
