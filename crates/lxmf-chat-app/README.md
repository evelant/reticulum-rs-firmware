# Reticulum LXMF chat application services

`reticulum-lxmf-chat-app` is the reusable application layer used by the
long-running host appliance service and intended to replace the equivalent
ordering currently duplicated in the foreground CLI. It connects the
transport-neutral chat store to one authenticated sequential device session
without owning serial discovery, reconnect policy, HTTP, or UI state.

The engine commits outbound material before device I/O, replays exact retained
material after reconnect, projects device status monotonically, and scans the
device inbox one stable summary at a time. Inbox cursors remain session-local:
after reconnect the engine safely rescans from the beginning and skips
message IDs already present locally rather than persisting a bare handle that
could be reused after a device-store reset. At end-of-scan it acknowledges the
highest cursor only after every preceding message is durably local and retries
an ambiguous acknowledgement without moving the local durability boundary.

Each engine call performs at most one device operation plus its associated
durable local mutation. That stepwise contract lets a foreground client, host
service actor, or later native application supply its own scheduling and
reconnect policy without moving serial, HTTP, or executor dependencies into the
application core. Known inbox message IDs are detected before downloading the
complete normalized wire, so restarting a session can safely rescan summaries.
When API 1.14 supplies first-arrival evidence on a summary, the engine carries
the immutable interface and paired RSSI/SNR values into the same atomic inbound
commit. Older summaries remain valid with unknown ingress; Nearby announce
signal is never substituted for a message observation.

Outbound submission carries the optional `MessageLocation` already frozen into
the committed outbox material through API 1.17. Board-owned carrier retries
remain inside that durable submission and do not change the location or LXMF
message identity. A transitional explicit retry for a legacy or permanently
terminal row changes only the device-API request key to create a replacement
durable submission. On import,
the session recognizes Sideband `FIELD_TELEMETRY` location and commits it with
the message. Absent, malformed, or unsupported optional telemetry never causes
an otherwise authenticated title/content message to be discarded.

The session boundary also exposes the unreleased API 1.14 probe start/poll
operations without making them durable chat mutations. A probe exercises
Reticulum reachability only; it does not establish LXMF availability or
throughput.

The engine retains app-owned automatic-rearm wrappers only for schema migration
and store-conformance fixtures. Production schedulers must not call them:
startup, reconnect, sync, Nearby reads, and timers do not rearm terminal rows,
and current firmware owns unattended LXMF retry while status remains
`Preparing`.
