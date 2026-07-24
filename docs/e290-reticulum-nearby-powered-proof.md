# E290 Reticulum-native Nearby powered proof

**Date:** 2026-07-24

**Status:** complete for one bounded, foreground iOS demonstration in which an
authenticated BLE session to one E290 exposed a second E290 learned from its
signed `lxmf.delivery` announce, the Expo **Nearby** action opened that peer's
existing durable conversation without endpoint entry, and one short message in
each direction crossed LoRa and reached durable `Delivered`. Each receiver
imported the exact peer message. The app process also relaunched with
byte-identical contact, conversation, first message, and terminal evidence.

This is the powered contact-selection tranche of
[ADR 0017](adr/0017-reticulum-peer-discovery-and-proximity-bootstrap.md). It is
not the complete ADR acceptance record: the peer was already a durable contact,
both short messages used the current Header-1 opportunistic carrier rather
than an authenticated Link, and neither board was restarted after these exact
messages. The reusable product-owned direct-Link capability in
[ADR 0018](adr/0018-durable-lxmf-delivery-policy.md) was
[qualified separately](e290-direct-link-powered-proof.md) after this run.

## Exact artifacts and roles

Both antenna-equipped boards ran the same ordinary schema-3 firmware image
after exact flash readback:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| E290 merged firmware | 1,080,368 | `ec05810addcce6ea883c7e2451e6b6bce0b049dff8cd613937ac3deacaeadd62` |
| iOS `main.jsbundle` | 2,733,255 | `6fc40920dfdc63e35eb418bdbdd4aac2174dd827dd52e0c426a68bdff1ef363e` |
| iOS arm64 executable | 21,024,208 | `2f074c2eb236dae07c50703af1aa7226334dcf0928848b49f75c8b8f3fa1302e` |

The self-contained Release application used bundle identifier
`org.reticulum.appliance`, passed strict deep code-signature verification, and
was installed on the physical iOS device labelled `MetalbeardMobile`. It did
not require Metro.

| Role | EUI-48 | Primary destination | LXMF delivery destination |
| --- | --- | --- | --- |
| Phone-connected board B | `AC:A7:04:E1:3F:88` | `83a09ed807a0a7c631386deaa0448fb9` | `935caba93f7cd97c7c6658350ac02b45` |
| Nearby LoRa peer A | `AC:A7:04:E1:3E:88` | `c99e8ff1ec8629e4e1290e14462ae8af` | `03869ee76b74d1e2a4626f0c02ae3248` |

The development journal was migrated independently on each board while its
identity, credential, and configuration partitions were retained. The phone's
existing credential then selected and authenticated board B automatically
after the phone was unlocked. No credential bytes or secret digest are
included in this record.

## No-typing peer and conversation selection

Both boards remained powered while board B's authenticated, boot-scoped peer
projection learned board A's signed `lxmf.delivery` announce. Through its
credential-bound BLE session, the app selected that record under **Nearby**.
This opened the durable contact
`03869ee76b74d1e2a4626f0c02ae3248`, displayed as
`Peer b447 b650 2157`, without entering its 32-character destination.

The opened conversation contained seven previously imported messages; its
latest inbound title was `ios-release-proof-return`. A read-only phone database
snapshot independently contained one contact and those seven inbound records.
Because that contact predated this run, this result qualifies the **Open**
branch of the picker, not first-time **Add** persistence.

## B-to-A message and returned proof

The user queued this exact message from the selected conversation:

| Field | Value |
| --- | --- |
| Source | `935caba93f7cd97c7c6658350ac02b45` |
| Destination | `03869ee76b74d1e2a4626f0c02ae3248` |
| Timestamp ms | `1784902139983` |
| Title | `Nearby-no-type-proof` |
| Content | `Proximity ble discovery proof` |
| Submission ID | `1` |
| LXMF message ID | `a73d49c3b3bf399b85ef0c24405991c1de5eb2ef663611de883d2ff61e0c4532` |
| Sender terminal status | `Delivered` |
| Reticulum packet bytes / SHA-256 | 259 / `76a0b8c6fb14debffc0c74c6c8b8e0f054bad971c256f3c087606e4532d9400a` |

The message is within the current short-message boundary, so firmware selected
the dedicated Header-1 opportunistic carrier. Board B's sender row reached
status kind `5` (`Delivered`), and board A independently imported the exact
message. In the qualified implementation, that terminal state requires a
matching Reticulum proof, while the receiver's retained-proof policy releases
the proof only after a new durable LXMF inbox commit or a freshly received
retransmission recognized as already durable.

The macOS CoreBluetooth service then authenticated independently to board A
and imported the same message from its firmware inbox. Its SQLite row matched
the message identifier, source, local destination, timestamp, title, and
content above. This closes the receiver-side read in addition to the returned
proof.

## A-to-B return message and phone import

Keeping the phone connected to board B, the macOS service queued this exact
return through board A:

| Field | Value |
| --- | --- |
| Source | `03869ee76b74d1e2a4626f0c02ae3248` |
| Destination | `935caba93f7cd97c7c6658350ac02b45` |
| Timestamp ms | `1784902641000` |
| Title | `nearby-return-proof` |
| Content | `Hello back from E290 A to MetalbeardMobile through Reticulum LoRa.` |
| Submission ID | `1` |
| LXMF message ID | `c2f8126afa5525db98b72f05a589928f780b5e63d5cbbb035dfa74f8283b2f76` |
| Sender terminal status | `Delivered` |
| Reticulum packet bytes / SHA-256 | 291 / `7be365f2d48181259a8298ef9cce7e442843166541774bdbbacbca42d39c3466` |

Board A's outbox reached status kind `5` (`Delivered`), and board B
independently imported the exact message. In the qualified implementation that
combination requires B's durable commit followed by its matching proof. The
foreground phone app then showed `nearby-return-proof` immediately. A
read-only phone database snapshot contained the same message identifier,
endpoints, timestamp, title, and content as inbound sequence 9. The final phone
snapshot had SHA-256
`74fae9b2950956452a3010379dbf00834cb3ca50cad00e4a4c6bbfc660b90fcf`;
the board-A host SQLite snapshot had SHA-256
`2f166c8b572f0cd502862399afb0ce823a184e9c1cfa88c2faeb336671a819e0`.

Together the two messages close:

```text
Expo iOS -> BLE -> E290 B -> LoRa -> E290 A -> macOS CoreBluetooth service
macOS service -> BLE -> E290 A -> LoRa -> E290 B -> BLE -> Expo iOS
```

BLE remains the authenticated local API bearer at each edge, not a Reticulum
packet interface.

## App-process persistence

Before relaunch, the app-private
`reticulum-lxmf-chat-alpha-schema3.sqlite3` snapshot had SHA-256
`8be6c301b2db1c1d664e7f2f662673aa3656d1bf11fbcaa7e5783745c9b2061b`.
The Release process was terminated and relaunched in the foreground. A second
read-only snapshot had the same digest and retained:

- the one durable contact;
- all seven inbound messages;
- the new outbound message and exact message identifier;
- status kind `5`, which the Rust store decodes as `Delivered`; and
- the same 259-byte packet length and packet digest.

The private snapshots remain under the ignored
`target/private-e290-proofs/phone-nearby` development-evidence directory. They
contain no copied appliance credential.

## What remains

- Repeat with a fresh contact database to qualify first-time **Add**.
- Preserve the later bounded ADR 0018 fresh-Link, stale-Link recovery, and
  same-Link/direct-replay proofs; qualify the remaining fault/pressure behavior
  and bidirectional direct delivery.
- Qualify age-based peer expiry, Android hardware, background BLE restoration,
  multi-hop discovery, additional Reticulum interfaces, pressure, and soak.
