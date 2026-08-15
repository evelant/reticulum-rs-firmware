# ADR 0026: Sideband-compatible LXMF message location

- **Status:** accepted for the appliance alpha
- **Date:** 2026-08-02
- **Extends:** [ADR 0013](0013-bounded-lxmf-wire-boundary.md),
  [ADR 0014](0014-durable-lxmf-message-ownership.md),
  [ADR 0015](0015-universal-expo-client-and-generated-bindings.md),
  [ADR 0018](0018-durable-lxmf-delivery-policy.md), and
  [ADR 0025](0025-durable-packet-correlated-radio-tracing.md)

## Context

The app needs an optional sender location that travels with a message and is
useful to an ordinary LXMF recipient. Reticulum itself routes packets and does
not define a message-position header. LXMF instead owns an extensible
MessagePack fields map, and Sideband already uses LXMF field
`FIELD_TELEMETRY` (`0x02`) for a binary sensor map containing time and location
sensors.

This recipient-visible value is distinct from two existing phone-location
uses. RMAP location is public interface-discovery metadata. Field-test location
is a private app observation joined to one local transport attempt. Neither is
part of the signed LXMF message, and silently substituting either would give it
the wrong lifetime and meaning.

The solution must preserve ADR 0013's raw-wire compatibility boundary, ADR
0014's commit-before-ack ownership, and ADR 0018's rule that delivery selection
and retries never recompose an accepted message. It must also avoid opening an
unbounded caller-supplied fields API before arbitrary MessagePack values,
attachments, and Resources have their own storage and validation policy.

## Decision

API 1.17 adds optional key `5` to `experimental.lxmf.basic_send`. The key is a
typed seven-value location map: latitude and longitude in decimal microdegrees,
altitude in centimetres, speed in centimetres per second, bearing in
centidegrees, horizontal accuracy in centimetres, and source-fix update time in
whole Unix seconds. Latitude and longitude are validated against world bounds.
The API accepts no raw fields bytes. A client that supplies location requires an
observed device minor version of at least 17 rather than allowing an older
decoder to ignore the optional key.

The device converts that semantic value to the Sideband-compatible LXMF shape:

- outer LXMF fields key `0x02` contains a MessagePack binary value;
- its sensor map contains time sensor `0x01` and location sensor `0x02`; and
- location is the seven-element Sideband fixed-point array, including its update
  time.

Without location, basic send retains the existing empty fields map. With
location, the resulting fields bytes are part of the LXMF payload, signature,
and message ID. Reticulum routing and interface selection remain unchanged.
The current complete fields map is at most 52 bytes rather than the one-byte
empty map, so it can consume up to 51 additional bytes of the existing
one-packet title/content budget.

The app requests a fresh high-accuracy foreground phone fix only when the
per-message composer switch is enabled. A durable phone-local preference sets
the initial state of new composers, and each draft can override it. Requested
location failure leaves the message visibly unqueued. On success, the app
commits the location with the exact outbox material before device I/O. Automatic
and explicit retries may change their device-API idempotency key, but retain the
original location, timestamp, content, LXMF message ID, and signed wire.

SQLite schema 8 stores the optional all-or-none location projection on inbound
and outbox rows. The inbox decoder recognizes Sideband location while keeping
unknown fields opaque at the LXMF wire boundary. Missing or malformed optional
telemetry yields no location projection and does not discard an otherwise
authenticated message. Timeline and details views show sent and received
locations, with an OpenStreetMap action. UI copy identifies the value as
sender-attached phone location, not board GNSS, routing state, relay position,
receiver signal, or exact RF-emission position.

## Consequences

Sideband-compatible clients can consume a useful location without a private
Reticulum extension, and this appliance can render the same field on receipt.
The sender's chosen coordinate is authenticated with the message and remains
stable through lossy-network retries.

Location reduces the text available inside the current inline one-packet
ceilings and may therefore cause a message near a delivery-method boundary to
require a different eligible path or be rejected as too large. The composer UI
and protocol errors must keep that limit visible rather than truncating data.

This decision does not define arbitrary LXMF field editing, attachments,
telemetry history, Resources, propagation delivery, board GNSS collection, or a
field-test map. Those features may reuse the bounded raw-wire and typed semantic
boundaries, but cannot reinterpret the location field or mutate it between
attempts.
