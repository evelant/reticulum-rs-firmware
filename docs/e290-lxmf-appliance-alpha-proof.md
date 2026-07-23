# E290 LXMF host-appliance alpha proof

**Date:** 2026-07-22

**Status:** complete for one message submitted through the authenticated
loopback HTTP service, delivered by the permanent E290 node over LoRa, and
imported verbatim from the peer through its authenticated USB API. The same run
exercised process-capability bootstrap, schema-2 device binding, automatic
background inbox import and status reconciliation, and a deliberate reconnect
on the same USB enumeration.

This is a development proof of the host appliance boundary. It is not evidence
that the E290 serves the SPA, and it does not qualify Wi-Fi, BLE, a physical
unplug/replug, service or host restart, a second simultaneous appliance service,
browser rendering, multi-hop, direct/Resource/propagated LXMF, pressure, or
soak.

## Reused firmware and board binding

Both boards retained the permanent 128-entry E290 firmware used by the earlier
[bidirectional chat-alpha proof](e290-lxmf-chat-alpha-proof.md). This run did not
reflash or repeat that record's exact firmware readback, so it adds host-service
integration evidence rather than a new firmware-artifact qualification.

| Board | USB serial | Primary destination | LXMF delivery destination |
| --- | --- | --- | --- |
| A, service owner | `AC:A7:04:E1:3E:88` | `c99e8ff1ec8629e4e1290e14462ae8af` | `03869ee76b74d1e2a4626f0c02ae3248` |
| B, radio peer | `AC:A7:04:E1:3F:88` | `83a09ed807a0a7c631386deaa0448fb9` | `935caba93f7cd97c7c6658350ac02b45` |

Board A's service selected `/dev/cu.usbmodem1101` by exact USB descriptor
serial, authenticated with its Active credential, and transactionally bound a
fresh SQLite database to all three returned identity values. The ready snapshot
reported no pending outbox rows and four older inbox messages imported during
the initial full scan.

The first hardware attempt exposed a host-only discovery defect: macOS
enumerates one native CDC data interface as both `/dev/cu.*` and `/dev/tty.*`,
so counting matching metadata as distinct devices rejected an otherwise
unambiguous serial. The successful run used the corrected policy, which selects
the sole matching callout path while still rejecting genuinely ambiguous
matches.

## Authenticated HTTP-to-radio-to-peer path

The service bound an ephemeral loopback listener at port `56654`. Its printed
URL contained a process capability only in the fragment. The test exchanged
that capability through `POST /api/v1/session`, retained the resulting
`HttpOnly` API cookie, and read the authenticated snapshot. The capability
value is intentionally omitted from this record.

After adding Board B as a contact, `POST /api/v1/messages` committed this exact
material to Board A's host database:

| Field | Value |
| --- | --- |
| Local outbox row | `1` |
| Destination | `935caba93f7cd97c7c6658350ac02b45` |
| Timestamp ms | `1784762778000` |
| Title | `appliance-alpha` |
| Content | `sent through the bundled web service` |
| LXMF message ID | `d6aaf99d4d798603a4c8d7b8763ba3fbcc9c79aad31cf7b3c0370a83bd9a2b48` |
| Terminal status | `Delivered` |

An immediate snapshot after that HTTP response showed one pending row, as
intended: the request only durably queued local material. The actor then
submitted the exact retained timestamp and idempotency material to Board A and
projected the device status asynchronously. A later conversation read returned
the same row as `Delivered`, with the exact message identifier, title, and
content above.

Board B was then opened by the existing authenticated foreground CLI at
`/dev/cu.usbmodem101` and synchronized into its own fresh SQLite database. The
sync reported five inserted inbox messages because the device already retained
earlier proof traffic. Its timeline contained the exact new message identifier,
source/destination relationship, timestamp, title, and content above. This
closes the host HTTP -> durable SQLite outbox -> authenticated USB -> E290 LoRa
-> peer durable LXMF inbox -> authenticated USB -> peer SQLite path for one
basic opportunistic message.

Finally, `POST /api/v1/reconnect` discarded Board A's current session. The actor
rediscovered `/dev/cu.usbmodem1101`, reauthenticated on the unchanged USB
enumeration, revalidated the same schema-2 binding, and returned to `ready` with
zero pending rows and no error.

After the final actor-shutdown and serial-selection fixes, the complete host
tree was run against Board A again with a fresh database. Exact-serial discovery
again selected `/dev/cu.usbmodem1101`; the authenticated snapshot reached
`ready`, retained the same three binding values, imported the four existing
messages, and reported no pending work or error. An SSE client was deliberately
left connected while the process received Ctrl-C. It observed the final stopped
revision and then reached EOF; the process exited promptly, and the schema-2
database reopened with the exact binding above. This is a same-enumeration
shutdown smoke, not the still-deferred physical disconnect or process-restart
matrix.

## Evidence limits and retained local data

The successful service database was
`/private/tmp/reticulum-appliance-smoke-a.sqlite3`; the independent peer import
used `/private/tmp/reticulum-appliance-smoke-b.sqlite3`. These are local
development artifacts, not portable repository evidence. Active credential
files remain separate owner-only secrets and are not copied into this record.

Focused application-core, service-actor, HTTP security/routing, and serial
selection tests plus strict Clippy passed around this run. A release-grade
record still needs a committed host source and binary digest, automatic evidence
bundle, visual/browser matrix, process restart against the retained database,
physical cable and suspend/resume recovery, concurrent-access exclusion, and a
long-running two-service soak.
