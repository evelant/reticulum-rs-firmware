# ADR 0025: Durable packet-correlated radio tracing

- Status: accepted
- Date: 2026-08-01
- Extends: ADR 0003, ADR 0015, ADR 0018, ADR 0023, ADR 0024

## Context

Message status, current radio counters, the latest RX/TX observation, and a
volatile route table cannot explain a failed field-test attempt after later
traffic or a reboot. In particular, they do not establish which route and
interface were selected, whether the SX1262 completed either physical RNode
frame, or which returning proof ended the application attempt. Nearby announce
signal is evidence about a different packet and must not be attributed to the
message.

The diagnostic path must preserve exact attempt identity across firmware,
authenticated device API, app persistence, and export without retaining
message payloads. It must also remain transport-aware rather than implying that
LoRa RSSI is end-to-end Reticulum telemetry.

## Decision

The E290 firmware owns a 32-event, boot-scoped ring beside its radio
diagnostics. Each event has a strictly increasing sequence and a monotonic
microsecond timestamp. The ring records four kinds of immutable evidence:

- route selection for a durable destination-DATA submission, including the
  submission ID, destination, retained next hop when present, hop count,
  selected interface, resolution, packet length, complete encoded-packet
  SHA-256, and Reticulum proof-correlation token;
- terminal DATA dispatch, including the same packet/token identity, detailed
  dispatcher outcome, planned and completed physical-frame counts, byte-access
  authorization evidence, and each observed radio `TxDone` time;
- each complete logical LoRa packet accepted after physical reassembly,
  including packet digest, derivable hop-invariant packet hash, and
  receiver-local RSSI/SNR; and
- each application attempt terminal, including delivered, delivery-timeout, or
  unsent outcome and the accepted proof's ingress interface and optional
  receiver-local RSSI/SNR.

No packet or LXMF payload bytes enter this trace. The applied immutable LoRa
profile and its complete configuration fingerprint accompany every boot.
Requested output power is recorded as a chip-setting request, not measured
conducted power or EIRP.

API 1.16 adds authenticated read-only operation
`experimental.radio_trace.page` (`0xf014`). Its cursor contains both the boot
identifier and exclusive event sequence so a reboot cannot masquerade as a
completed old page. Responses contain at most three dense ascending events to
fit the frozen 448-byte logical body. Firmware reports overwritten or skipped
history explicitly.

While connected, the app runtime incrementally imports pages into additive
SQLite schema 7. Import is atomic and idempotent by boot and event sequence.
The route event's durable submission ID seeds the authoritative mapping from
the Reticulum token to the existing outbox attempt; later TX and terminal
events with that token are correlated and previously imported same-token
events are backfilled. RX correlation is retained only when unambiguous.
Existing schema-6 per-attempt phone-location observations are joined at query
time rather than copied into every trace row.

The app exposes the same durable newest-first query globally and per message,
and can export a complete paginated snapshot as JSON or RFC 4180 CSV. Exported
coordinates are sensitive local diagnostic data. Board monotonic timestamps,
app import wall time, and queue-time phone location remain distinct clocks;
the app must not present the location stamp as the exact RF emission point.

## Consequences

A later field analysis can distinguish route selection, local radio completion,
returning-proof delivery, and application timeout for one durable message
attempt. Repeated payloads remain distinct because correlation uses submission
and attempt identity rather than packet digest alone. Traces survive app and
board restarts once imported.

The board ring is intentionally bounded and volatile. A long disconnect can
overwrite evidence before import, which permanently marks history incomplete.
An outgoing trace cannot supply the remote receiver's RSSI; that observation
must be exported from the receiving appliance. A proof's signal is measured on
the sender's final return hop and may describe a relay. Ordinary transmissions,
remote clock synchronization, continuous background phone collection, a map,
and automated link-budget diagnosis remain separate work.
