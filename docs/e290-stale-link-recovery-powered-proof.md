# E290 stale-Link recovery powered proof

**Date:** 2026-07-24

**Status:** complete for one bounded, sequential two-board recovery run. A
direct LXMF message first established the working baseline, the receiver alone
was reset while the sender retained its cached Link, the next direct message
reached durable `Failed(DeliveryTimeout)`, and a later direct message
established a usable session and reached durable `Delivered`. The receiver
imported the baseline and recovery messages, but not the timed-out message.

This is the first powered recovery record for the exact direct-Link timeout
retirement specified by
[ADR 0018](adr/0018-durable-lxmf-delivery-policy.md). The run used only the
macOS host: USB to the sending E290 and CoreBluetooth to the receiving E290.
`MetalbeardMobile` and an iOS client were not involved. USB and BLE were local
authenticated device-API bearers, not Reticulum packet interfaces; the
board-to-board path used NA915 LoRa.

## Source and artifact binding

The firmware changes were still uncommitted when this powered run was made.
The dirty `codex/lxmf-delayed-proof` checkout was based on
`a12e610c786bc8efce7c68578d28d9945d325e0f`, and its Rete dependency was pinned
to `a443173b0829c2637ce23531a8cde15fdfec185e`. This record therefore does not
invent or claim a source commit for the change. It is bound to the exact ELF,
merged image, Rete revision, and sender readback below:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| E290 ELF | 14,884,136 | `624993eec092358927c471a6e8dbdec5eaf008bebde855188903a1abf605a46b` |
| E290 merged firmware | 947,232 | `9e688e36b4b723c3fcb49603add0ee285c3b3a064d65c8e45180f84e422e3098` |
| Sender address-zero readback | 947,232 | `9e688e36b4b723c3fcb49603add0ee285c3b3a064d65c8e45180f84e422e3098` |

The identity-bound flash check verified sender A's ESP32-S3 eFuse MAC as
`AC:A7:04:E1:3E:88`, its 16 MiB flash, and its `HT-RA62-HF` radio module before
the exact readback. Receiver B remained the BLE-accessed board with EUI-48
`AC:A7:04:E1:3F:88`.

| Role | EUI-48 | Client bearer | LXMF delivery destination |
| --- | --- | --- | --- |
| Sender A | `AC:A7:04:E1:3E:88` | USB | `03869ee76b74d1e2a4626f0c02ae3248` |
| Receiver B | `AC:A7:04:E1:3F:88` | BLE | `935caba93f7cd97c7c6658350ac02b45` |

## Powered sequence

All three direct submissions were sent sequentially from A to B. The first
message, `recovery-0`, established the pre-reset baseline. Each message used a
10-byte title and the maximum 295-byte basic content. Even before MessagePack
container overhead, its destination, source, timestamp, signature, title, and
content exceed the 407-byte complete-wire Header-1 opportunistic ceiling, while
remaining within the one-packet Link limit. `Auto` therefore had to use direct
Link DATA rather than opportunistic DATA.

| Field | Value |
| --- | --- |
| Submission ID | `3` |
| LXMF message ID | `3dc9e8beace2c4b3801199ddd57227c34026992833a9db431bf22d96e0f54f51` |
| Sender terminal state | `Delivered` |
| Reticulum packet bytes | 499 |
| Packet SHA-256 | `d4fa8091b81e08c9a6a785ea7437378c7fe7cd08981c346b1d07785cc63e0ea6` |
| Receiver inbox sequence | `12` |

Receiver B was then reset once while sender A remained running. This removed
B's boot-volatile responder-side Link state while A still held the initiator
Link as reusable. The next direct message, `recovery-1`, exercised that stale
sender-side session:

| Field | Value |
| --- | --- |
| Submission ID | `4` |
| LXMF message ID | `54c8d2af2408f8e2556a9355fdd82be15d2addb8037413c951cbc90af5e6a0e9` |
| Sender terminal state | `Failed(DeliveryTimeout)` |
| Stored status kind | `6` |
| Stored failure kind | `1` |
| Receiver inbox result | absent |

The failed submission remained terminal. It was not silently replayed. A
separate later submission, `recovery-2`, then completed through a usable Link:

| Field | Value |
| --- | --- |
| Submission ID | `5` |
| LXMF message ID | `c515ad62c24e05018092a080a281c2cb15908d9a724a25eaf6aad0c7b1d81e40` |
| Sender terminal state | `Delivered` |
| Reticulum packet bytes | 499 |
| Packet SHA-256 | `16dd1e1734cae964b7b62e341436beecf7caf47a266da895edf4f3fbe96b2ed3` |
| Receiver inbox sequence | `13` |

The final receiver projection contained exactly the two messages expected from
this run at sequences `12` and `13`: `recovery-0` and `recovery-2`.
`recovery-1`, message
`54c8d2af2408f8e2556a9355fdd82be15d2addb8037413c951cbc90af5e6a0e9`,
was absent.

```text
recovery-0 -> direct delivery -> Delivered -> B sequence 12
reset B    -> B loses its boot-volatile half of the Link
recovery-1 -> stale direct Link -> Failed(DeliveryTimeout) -> absent on B
recovery-2 -> later fresh direct work -> Delivered -> B sequence 13
```

## Durability-first Link retirement

The implementation associates each direct terminal attempt with the exact
opaque Link handle that carried it. A Link-DATA `DeliveryTimeout` does not
immediately discard that handle. The submission runtime first persists the
final `Failed(DeliveryTimeout)` record and retains its exact persistence
correlation through both an ordinary commit acknowledgement and ambiguous-I/O
projector reconciliation.

Only after that final record is known durable does the runtime expose a
separate retirement control step. Firmware then evicts that exact reusable
Link entry and closes the corresponding native Link through its normal
authenticated close path before acknowledging the terminal owner. This
ordering prevents session cleanup from erasing or overtaking the durable reason
for the failure.

Retirement is recovery for later work, not an automatic retry policy.
Submission `4` remained `Failed(DeliveryTimeout)`; submission `5` was distinct
work and obtained the subsequent `Delivered` result. The powered sequence
qualifies the externally observable consequence of that contract: one stale
session produces one durable timeout, and the next sequential direct
submission can recover instead of repeatedly reusing the same dead session.

## Evidence and qualification boundary

The ignored development-machine evidence directory is
`target/private-e290-proofs/stale-link-recovery-20260724`. It contains the
identity-bound sender flash/readback records and final sender-outbox,
sender-service, and receiver-inbox projections. It contains no copied
credential.

This record qualifies:

- the exact sender image read back from E290 A;
- a working direct baseline before the receiver reset;
- receiver-only reboot while the sender stayed running;
- durable `Failed(DeliveryTimeout)` for the stale-Link attempt;
- no receiver import of that failed message;
- no automatic replay of the failed submission; and
- successful delivery and receiver import of a distinct later direct
  submission.

It does not independently expose the opaque Link handle or a wire-level
`LINKCLOSE` trace through the client API; those exact-correlation and ordering
properties remain source- and regression-qualified. It also does not qualify
multiple simultaneous direct attempts on one Link. Timeout retirement closes
the complete session, so a younger sibling attempt awaiting a proof could
conservatively time out when an older sibling retires their shared Link. Direct
sends must remain sequential for the current alpha until the runtime permits
only one in-flight direct attempt per Link or defers close until all sibling
receipts drain.

This bounded run also does not qualify responder/backchannel reuse,
multi-destination Link-table pressure, ambiguous flash I/O on powered hardware,
electrical power cuts, multi-hop routing, Resource transfer, additional
Reticulum packet interfaces, allocation pressure, sustained traffic, or soak.
