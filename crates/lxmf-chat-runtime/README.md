# Transport-neutral LXMF chat runtime

`reticulum-lxmf-chat-runtime` owns the long-running, single-writer application
actor shared by host and native clients. It opens the SQLite conversation
store, serializes contact and durable-outbox mutations, reconciles one
authenticated device session, polls the device inbox, and publishes immutable
snapshots.

The runtime does not discover serial ports, choose a mobile platform API, or
serve HTTP. Callers provide a `Connector`; successful connections carry
transport-neutral metadata and an optional opaque lease retained for the
session lifetime. Connection state names the actual bearer, endpoint, and
device label. Retryable, unavailable-in-this-build, and permanent connector
failures have distinct states, so reserved USB OTG, BLE, and Wi-Fi connectors
can remain honest stubs without retrying continuously.

The shared Serde and `ts-rs` request/response types are the semantic client
contract. Both the loopback HTTP service and the Expo native UniFFI facade use
the same validation, JSON-safe integer policy, contacts, timelines, and durable
send outcomes. The host service separately projects ready state into its
historical HTTP-v1 `port` and `usb_serial` names; that compatibility shape does
not leak back into this runtime.

New outbound requests may include one optional `MessageLocation`. It is stored
as part of the exact outbox material before submission and remains unchanged
across reconnects, board-owned carrier retries, and explicit terminal-row
replacement. Timeline projections expose the same
typed value on outbound and recognized inbound messages; it is not the
app-submission phone-location observation used by private RF tracing.

Complete connectors now include the host USB Serial/JTAG adapter in
`reticulum-lxmf-chat-service` and the Expo native BLE path backed by the shared
Rust appliance/session core. The BLE suite-3 binding has a bounded installed-iOS
powered proof; this does not yet qualify every mobile platform or lifecycle.
The opt-in native raw-TCP Wi-Fi proof connector and separately transcript-bound
suite-2 E290 SoftAP endpoint are implemented and host-qualified; powered field
qualification remains open.

Inbox synchronization acknowledges the appliance's durable mailbox watermark
only after every preceding message in a scan is already present in the local
durable store. The engine batches the highest safe cursor at end-of-scan and
retains it across an ambiguous session failure for idempotent retry. Replays
that are already in SQLite are safe to acknowledge and do not create another
message or notification activity event.

Nearby discovery is an explicit foreground projection, not a background
Reticulum scanner owned by this crate. The app polls it only while foregrounded
and the Nearby surface is visible. Reading a changed nearby projection does not
mutate or rearm the local outbox; post-acceptance delivery policy belongs to the
powered appliance.

Radio and route diagnostics use a separate on-demand actor command. Rust reads
one node snapshot and at most 32 retained routes, verifies that every
lexicographically paged response has the same route-table revision, and retries
the whole snapshot once if the table changes mid-read. Retained routes are not
presented as connected or reachable peers, and their local last-use age is not
presented as a last-heard observation. API-1.15 LoRa projection distinguishes
DATA from ordinary terminal work and retains the latest DATA packet's encoded
length/SHA-256/interface so it can be compared with message packet evidence.
That prepared identity also exists for pre-authorization failures and is not an
RF-transmission claim.

API-1.16 packet-correlated trace synchronization is independent of those
latest-value reads. While a session is usable, the actor drains boot-aware
three-event device pages into SQLite without starving inbox or status work.
The local query remains available after disconnect and returns the same
generated global or per-message view used by native and web clients. Board
monotonic observation time, app import wall time, and queue-time phone location
remain distinct fields.

Foreground SQLite and in-memory commands drain only the bounded local burst
already waiting in the actor channel. A queued device-session command is
deferred until after one background inbox, submission, or trace opportunity,
so diagnostics cannot monopolize the bearer and a scheduled reconnect can run
before a dependent read. The burst never waits for a caller to enqueue more
work, so fast UI polling cannot starve background turns. A newly accepted or
explicitly retried send also makes reconciliation urgent until that outbox work
has been submitted to the appliance, ahead of due inbox and diagnostic reads.
Urgency stays with that exact row through its first post-acceptance status read.
While it remains nonterminal, its status then alternates with at most one
ordinary round-robin outbox row; this keeps the newest send visibly current
without resetting or starving the independent fairness cursor. The device lane
is reserved only for the short submit-to-first-status gap, never across an API
backoff. Nonterminal board-owned delivery states poll at one second instead of
occupying the serialized BLE bearer at 10 Hz. Trace catch-up yields to initial
chat synchronization and becomes immediately due after terminal delivery.
Once RF trace synchronization reaches the current end, it polls every five
seconds; the faster operation-gap cadence is reserved for a reported backlog,
and submission or inbound-import progress wakes trace collection immediately.

API 1.18 distinguishes ordinary retained-flash ownership from a durable fault.
When submit, submission-status, inbox-read, or mailbox-acknowledgement returns
`RetryLater`, the actor keeps the authenticated session and exact durable work,
backs off that reconcile or inbox lane for 250 milliseconds, and lets the other
background lanes continue. A hot outbox row is re-armed after the delay without
reserving the serialized bearer while it waits. Structural
`CapacityExhausted` retains its separate 30-second policy; unavailable
capabilities, backend ambiguity, and internal faults do not enter the transient
busy path.

Timeline projections preserve optional receiver-local first-arrival interface
and signal evidence for inbound messages. When field location is available,
the inbox actor also commits the receiver phone's current fix atomically with a
message's first durable import. Activity queries project both the authenticated
sender-attached location and that import-time receiver fix from the canonical
message row without duplicating either in the journal. The actor also
serializes one-shot Reticulum probe start/poll calls through the same
authenticated session, while leaving the probe volatile and outside the
message/activity journal. Probe return signal is the final hop into this
appliance and may come from a relay; it is not the remote request RSSI.

After device acceptance, this runtime only reconciles the existing submission's
status. Startup, reconnect, explicit sync, timers, and Nearby reads never rearm
a terminal row or create another device submission. `Retry now` remains a
transitional explicit same-row user operation in this layer; unattended
eventual-delivery retry belongs to the appliance firmware so it continues while
every client is disconnected.

Focused checks:

```sh
cargo test --locked -p reticulum-lxmf-chat-runtime
cargo clippy --locked -p reticulum-lxmf-chat-runtime --all-targets -- -D warnings
```
