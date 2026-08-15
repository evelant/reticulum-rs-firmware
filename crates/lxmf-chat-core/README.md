# LXMF chat core

`reticulum-lxmf-chat-core` is the persistence-facing domain core for the host
appliance and future desktop or mobile clients. It owns contacts, inbound LXMF
deduplication, commit-before-send outbox records, device acceptance identifiers, durable
status projection, conversation timelines, and restart reconciliation.
Current firmware owns unattended LXMF delivery inside one durable `Preparing`
submission. For a legacy or permanently terminal row, an explicit user action
can atomically create a replacement durable device submission on the same
outbox row with a fresh request key while preserving signed LXMF material,
message identity, timestamp, optional message location, and timeline ordering.
The old three-rearm app-owned budget and mutation APIs remain only to migrate
historical databases and exercise store-conformance fixtures; production
clients must not schedule from them.
Each successful message mutation also appends one privacy-bounded immutable
activity event. Bounded newest-first queries support an all-message log and
per-message details without retaining content, credentials, or request keys.
Inbound timeline rows also retain the appliance's optional immutable
first-arrival interface and paired RSSI/SNR observation. This is
receiver-local final-hop evidence; it may measure a relay, and outbound rows
have no equivalent receiver observation.

`MessageLocation` is a distinct optional semantic value carried inside the
authenticated LXMF payload. Outbound material commits it before device I/O and
retains it unchanged across board-owned attempts and any explicit replacement.
Inbound rows retain the recognized
Sideband-compatible location projection when present; this is separate from the
receiver-local ingress observation and from app-submission phone-location
field-test stamps. Those schema-named stamps are created per app submission;
board-owned carrier retries reuse the original stamp.

Packet-correlated RF trace is a separate immutable stream. A boot/profile and
event sequence identify raw route, DATA TX, logical RX, and attempt-terminal
observations. A route's durable submission ID seeds an unambiguous Reticulum
attempt-token association with the existing app-created outbox submission and
its queue-time location; later board attempts reuse that location stamp, and
same-token rows are backfilled without rewriting their raw evidence. Queries
are bounded, newest first, and either global or scoped to one timeline sequence.

The crate deliberately has no device transport, UI, async runtime, or
Reticulum implementation dependency. `ChatStore` is the database-neutral
adapter boundary. `MemoryChatStore` is the executable reference implementation
and can export and reopen an opaque `MemoryImage` for restart tests.

The default `sqlite` feature provides `SqliteChatStore` using exactly
`rusqlite 0.40.1` and bundled SQLite. It creates and versions its own schema,
and every mutation is a database transaction. Disable default features to use
only the domain and in-memory layers. The root workspace manifest and lockfile
pin the adapter and bundled SQLite dependency for reproducible client builds.

SQLite schema 2 added a singleton authenticated device binding: device ID,
primary Reticulum destination, and local `lxmf.delivery` destination. Schema 3
adds the historical app-owned automatic-rearm count. Schema 4 adds the
one-based app-created device-submission number and a bounded durable activity
stream. Schema 5 adds nullable inbound
first-arrival interface, RSSI, and SNR columns with an all-or-none signal
constraint. Existing inbound rows migrate with unknown ingress; migration does
not infer signal from Nearby announces or current interface state. An exact
duplicate may fill a currently missing ingress observation once, but never
replaces an observation already stored for that message. Existing schema-1
through schema-3 databases also cannot reconstruct historical mutation
times or manual retry boundaries. Migration therefore initializes each current
app-submission number to the known lower bound of
`automatic_retry_count + 1` and marks the activity history incomplete instead
of fabricating events. Historical manual replacements can make the lifetime
app-submission number higher than that reconstructable lower bound.
Schema 6 adds the closed per-app-submission phone-location observation. Schema 7 adds
`rf_trace_boots` and `rf_trace_events` without rebuilding the existing message
or activity tables. Trace import is atomic and idempotent by boot plus event
sequence; profile reuse, conflicting replay, or ambiguous token correlation is
rejected. A reported firmware overwrite or a missing sequence marks trace
history incomplete.
Schema 8 adds the seven nullable Sideband-location columns to inbound messages
and outbox material. They are all absent or all present; migration does not
invent location for older rows.
Schema 9 adds an optional first-import receiver-phone location to inbound
messages and preserves optional altitude and vertical accuracy in phone-location
observations. Existing rows migrate without an inferred receiver fix, and an
exact duplicate never replaces or backfills the observation captured with the
first durable import.
Schema-1 databases migrate as **unbound** because their existing rows contain
no authoritative device identity; schema-1 and schema-2 rows begin with zero
historical automatic rearms consumed.
Binding and migrations are transactional, and a different observed device
identity is rejected without mutation. The service adapter performs binding
after authentication; callers that use the store directly must enforce that
step themselves.

The opaque in-memory restart image is schema 8. Unlike SQLite, it has no
migration path: `MemoryChatStore::open` accepts only the exact current schema.
