# E290 outbound direct-Link powered proof

**Date:** 2026-07-24

**Status:** complete for one bounded, fresh outbound-initiator Reticulum Link
from the installed iOS client through BLE to one E290, one context-`NONE` Link
DATA packet over NA915 LoRa, and a durable LXMF commit on a second E290. The
receiver returned the delayed Link proof, the sender durably reached
`Delivered`, and both sides retained the exact result across normal board reset
and app-process relaunch.

This is the first powered direct-delivery record for
[ADR 0018](adr/0018-durable-lxmf-delivery-policy.md). Both firmware images used
Rete revision `2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` on
`codex/link-data-receipts`. It qualifies that revision's one-packet fresh-Link
success path, not every Link lifecycle, retry, pressure, or Resource path, and
does not qualify the later responder-Handshake reclamation pin.

## Exact artifacts and roles

Both antenna-equipped Heltec Vision Master E290 boards used the same
direct-Link implementation with feature-specific local client bearers. They
were not byte-identical images:

| Role | EUI-48 | Client bearer | Merged image bytes | SHA-256 |
| --- | --- | --- | ---: | --- |
| Sender B | `AC:A7:04:E1:3F:88` | BLE | 1,110,832 | `ca703a99f43eaa11edd53c325094e24935305056aa84ec5774b5072caaa60a31` |
| Receiver A | `AC:A7:04:E1:3E:88` | USB | 946,240 | `b783298bdaa9111b86fd062b02a34dd1ac59404999ad76c2b3ce14b54dfd6e24` |

The identity-bound flash helper verified each board's expected MAC, 16 MiB
flash, and `HT-RA62-HF` radio module. Exact address-zero readback matched each
image digest. The writes preserved the product data partitions, including both
board identities, the receiver's USB credential, and the sender credential
already held by the installed app on `MetalbeardMobile`.

| Role | Primary destination | LXMF delivery destination |
| --- | --- | --- |
| Sender B | `83a09ed807a0a7c631386deaa0448fb9` | `935caba93f7cd97c7c6658350ac02b45` |
| Receiver A | `c99e8ff1ec8629e4e1290e14462ae8af` | `03869ee76b74d1e2a4626f0c02ae3248` |

Booting the newly flashed images created an empty boot-volatile outbound-Link
registry. The installed Expo app authenticated to sender B over BLE and queued
one message to receiver A.

## Forced-direct message

The exact durable message had:

| Field | Value |
| --- | --- |
| Source | `935caba93f7cd97c7c6658350ac02b45` |
| Destination | `03869ee76b74d1e2a4626f0c02ae3248` |
| Timestamp ms | `1784913758761` |
| Title | `D` |
| Title bytes | 1 |
| Content bytes | 295 |
| Submission ID | `2` |
| LXMF message ID | `4e497b64ceca04d092b6fe0da2e85e51ef585f354ccab555aec2fdf0fd31b5e0` |
| Complete normalized LXMF wire | 408 bytes |

Removing the 16-byte destination prefix leaves a 392-byte opportunistic
carrier. The exact Header-1 opportunistic ceiling is 391 bytes, while the
active Link MDU is 431 bytes:

```text
408-byte complete wire
-16-byte destination prefix
=392-byte opportunistic carrier
>391-byte opportunistic maximum
<=431-byte Link MDU
```

`Auto` therefore could not select destination DATA. Because no cached Link
survived the fresh boot, the successful carrier necessarily established a new
authenticated Link and used one ordinary Link DATA packet. This proof uses
exact size exclusion; the current API does not separately expose a
delivery-method or Link-handle telemetry field.

## Durable receiver commit and returned proof

Receiver A's authenticated LXMF list advanced from nine records to ten and
reported the new message as handle `10`, with the source, destination, message
identifier, one-byte title, 295-byte content, and 408-byte normalized wire
above. An authenticated chunk read reconstructed the complete wire:

| Receiver artifact | Value |
| --- | --- |
| Handle | `10` |
| Bytes | 408 |
| SHA-256 | `14c164d54cd2c7c6e22c5bafe433e3ba07cb3891341ae5a1a20055233bb65d90` |

Only after that new durable inbox commit could receiver A release its Link
proof. Sender B validated the proof and projected the phone outbox row to
status kind `5` (`Delivered`):

| Sender field | Value |
| --- | --- |
| Local outbox row | `2` |
| Sequence | `10` |
| Submission ID | `2` |
| Terminal status | `Delivered` |
| Reticulum packet bytes | 483 |
| Packet SHA-256 | `01bb9e435742c6dc37832c0487de40c1d53b18543528ae3734cfa0d83110c1fd` |

Together, the forced carrier size, receiver commit, and sender terminal proof
close the powered fresh-Link delivery path:

```text
Expo iOS
  -> authenticated BLE device API
  -> E290 B outbound Link establishment
  -> NA915 LoRa Link DATA
  -> E290 A durable LXMF inbox commit
  -> delayed Link proof
  -> E290 B durable Delivered
```

BLE and USB were local authenticated client bearers in this run, not
Reticulum packet interfaces.

## Board and app restart persistence

After the terminal result, both boards received normal physical CPU resets.
Receiver A re-enumerated, authenticated again, retained count ten and handle
`10`, and served another 408-byte read. The pre-reset and post-reset wire files
were byte-identical and both had SHA-256
`14c164d54cd2c7c6e22c5bafe433e3ba07cb3891341ae5a1a20055233bb65d90`.

Sender B was checked independently rather than treating the phone's local
terminal row as device evidence. In a private sacrificial copy of the phone
database, row `2` was changed from terminal status kind `5` to accepted status
kind `1` and its packet length and digest were removed. The phone process was
stopped, a host service authenticated directly to reset board B over BLE, and
the normal nonterminal reconciliation queried device submission `2`. Board B
restored status kind `5`, packet length 483, and the exact packet SHA-256
`01bb9e435742c6dc37832c0487de40c1d53b18543528ae3734cfa0d83110c1fd`.
The short-lived private credential and sacrificial database were overwritten
and deleted after this read-only device-status check.

In a separate post-reset gate before the independent sender-status probe, the
iOS app was cold-launched with `--terminate-existing`. Its native BLE trace
recorded a new scan, connection to peripheral
`fd94a6a6-4009-9221-0eca-3ca9bb7d8c94`, discovery of appliance service
`f3c8a0b0-5e7a-4c51-a3b9-7d2160d20a01`, indication subscription, negotiated
write length, and repeated acknowledged GATT writes. This confirms that the
post-reset app process re-established a live BLE link rather than only opening
its local database.

Read-only app database snapshots before reset and after cold launch were
byte-identical, with SHA-256
`8a5500e91eafb959c52b6330f559441a838773f3a6069aa52c26cae2fa4a3797`.
The post-relaunch row retained the same destination, timestamp, title and
content lengths, submission ID, LXMF message ID, packet length and digest, and
terminal `Delivered` state.

The ignored local evidence directory is
`target/private-e290-proofs/direct-link/powered-20260724-1`. It includes both
identity-bound flash/readback records, receiver lists and reads before and
after reset, both exact wire files, the app database snapshots, cold-launch
JSON, the post-reset sender-status reconciliation result, and the bounded BLE
reconnect console trace. It contains no copied appliance credential and is a
development-machine record, not a portable repository artifact.

## Qualification boundary

This record qualifies:

- one fresh outbound Link establishment on the local two-board retained path;
- one complete LXMF message carried as context-`NONE` Link DATA;
- new receiver commit before delayed Link proof release;
- sender proof validation and durable `Delivered`; and
- exact terminal sender and receiver state after both board and app restart.

This historical record does not itself qualify active-Link reuse,
responder/backchannel reuse, `AlreadyDurable` replay, multiple simultaneous
establishments, Link-table or receipt pressure, timeout/fault cuts, the later
responder-`Handshake` reclamation behavior, pre-first-dispatch multi-transport
route churn, in-flight reset recovery, multi-hop routing, Resource transfer,
electrical power cuts, allocation pressure, sustained traffic, or soak. The
[later same-Link/replay record](e290-same-link-reuse-replay-powered-proof.md)
closes the bounded successful-reuse/replay outcome while explicitly retaining
the client-telemetry boundary.
