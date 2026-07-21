# Bounded node-core external-buffer packet dispatch

**Status:** portable DATA and ordinary route/permit/completion/recovery owners,
bounded interface fabric, both router coordinators and per-actor permit
services, ticket-aware real-radio dispatch, and the permanent E290 node/LoRa
composition are implemented and pass their host, portable-target and ESP32-S3
build gates. The durable model/projector, physical journal, sole storage actor,
and authenticated device-API adapter are implemented separately. Local
DATA/LXMF submission, client delivery, and full powered E290 qualification of
the product image remain open.
The permanent image's bounded source-`96e38aa` boot/interface/ordinary-TX smoke
has passed, without controlled peer RX or DATA.
**Rete pin:** `90570cafc812b3025011cb690ec74a27f287cb3f`
(designated durable tag `firmware-pin-90570ca`)

The preceding `14c7b4955a1ff6903e87cc40b42498f7869b6f4f` pin had host and
portable-target LRRTT validation and a build-only E290 package. Its 776,464-byte
merged image uses 710,928/6,291,456 application bytes (11.30%) and has SHA-256
`7b11c6f6a3c039d46ab0117fd362920aaa40145e7f27cbc6fa0a8a84a7ab3571`.
It has no flashed-image readback or powered proof. The current application-
event release needs a two-board powered lifecycle/RF run, but its default E290
release is now known: text/data/BSS 684,167/3,676/469,152 bytes (1,156,995 bytes
total by GNU size), a 789,504-byte merged image using
723,968/6,291,456 application bytes (11.51%), ELF SHA-256
`ebb34e7176a8e61b6969ebf99d7dac97c6e674ef5e583bbf931a34e8b6e970a2`, and
merged SHA-256
`1796f161c480d0348e3d47fd8f3cda5fda5b51aa38ad6024aaad04c8ba1751ce`.
The merged image matched an exact `3e:88` readback and served an authenticated
`identity-summary`; `3f:88` did not enumerate. The source-`96e38aa` result above
and later powered records
remain bound to the revisions they name.

At this pin, native ingress distinguishes exact path/reverse/Link forwarding
from genuine propagation. `PacketRouting::ExactInterface(id)` maps to the
project-owned `Only(id)` target, while `AllExceptSource` maps to
`AllExcept(source)`. An exact route is retained even when `id == source`, so a
shared-medium interface can relay to another peer on the same actor slot.
Reverse proofs are one-shot: only the recorded outbound interface can return
them to the recorded ingress interface, and a wrong-interface proof is dropped
while consuming that route. Link packets and non-LRPROOF Link proofs require
the stored direction and hop count. LRPROOF is stricter: it must arrive from
the responder-side interface at the stored remaining hops, the responder
identity must be known and reconstructable, and its signature must validate
before the Link entry is refreshed or the proof is forwarded. A targeted
HEADER_2 LRPROOF is normalized into that canonical validation instead of
bypassing it through generic Link transport; a valid proof retains exact relay
routing, while an invalid one does not refresh the Link.

The current native stack also normalizes owned HEADER_2 local traffic into
ordinary DATA, LINKREQUEST, Link and proof/receipt dispatch. Transported
HEADER_2 DATA/SINGLE and LINKREQUEST/SINGLE require an exact path and admit
reverse or relay-Link state transactionally before forwarding. Stack outcomes
carry typed owned/relay `LinkTableFull`, `ReverseTableFull`, and
`ReverseRouteConflict` rejections; the project adapter maps them without
reconstructing failure from counters. Foreign non-ANNOUNCE H2 packets are
filtered before state mutation, while H2 ANNOUNCE remains eligible for normal
announce validation. Relay-Link occupancy is exposed separately from owned
Links. The previously validated deterministic 235-check project conformance run
included 40 released-Python LRRTT MessagePack checks and a complete
three-node A--B--C Link handshake, channel DATA and proof flow over two exact
relay interfaces, pre-dedup wrong-hop LRPROOF rejection, 8 fresh-ciphertext
retry/receipt-replacement checks, plus 40 exact keepalive lifecycle checks. The
current schema-2 lifecycle/candidate runner passes 647 checks.

Locally owned Link output has a separate authenticated binding. A responder
binds to LINKREQUEST ingress. An initiator's learned path selects the initial
request target but does not bind the Link; only a valid LRPROOF does so. Active
application output and asynchronous close, keepalive, retransmit,
request/response and Resource output carry `BoundInterface`, which the adapter
maps to the exact physical interface. Only the initial LINKREQUEST may
broadcast, and only when its path has no recorded interface. Link DATA and
`RESOURCE_PRF` on another interface are rejected before dedup admission, so a
later authoritative-interface copy remains eligible.

Pending-Link expected hops are now retained explicitly. An initiator snapshots
the known path's hops when it creates the Link, or records the
`PATHFINDER_M = 128` wildcard if no path is known. A mismatched LRPROOF is
rejected before deduplication or Link-state mutation. The responder starts
without an expectation and records the post-ingress hop only from
authenticated, decrypted LRRTT. Pending-handshake payload parity is now
covered: Rete emits canonical MessagePack float64, accepts Python u-msgpack's
numeric scalar families and first-object/trailing-byte behavior, and uses the
greater local or peer RTT with Python ordering. The request anchor is immutable
and precise Link time uses microsecond `MonotonicInstant`/
`MonotonicDuration` values with binary64 RTT. An opaque, non-repeating
eight-byte token binds LINKREQUEST or LRPROOF output to the first successful
interface confirmation. The initiator uses the confirmed egress interval's
start; the responder uses its completion. This is the generic ordinary-router/
interface acceptance boundary, not physical LoRa RF `TxDone`.

Fresh authenticated LRRTT is valid in `Handshake`, `Active`, or `Stale`.
Initial activation emits establishment once; Active updates and Stale
reactivation emit `LinkRttUpdated` without duplicating establishment counters.
Exact raw replay is deduplicated. Authenticated malformed or nonnumeric LRRTT
tears down all three states, with `links_failed` incremented only for
Handshake. Zero RTT is retained with 5-second keepalive and 10-second stale
floors; otherwise stale grace is `4 * RTT + 5 seconds`. Rete intentionally
authenticates before liveness mutation, so a corrupt stale LRRTT cannot revive
the Link even though released Python performs its liveness update first.

The adapter passes precise `*_at` ingress/tick values and confirms at the
transport-neutral ordinary-router handoff. Rete's upstream Tokio and Embassy
runners remain coarse/unconfirmed. Rete also uses one pre-decrypt ingress
sample across one bounded synchronous handler, while Python's method samples
three times internally.

This native binding is an interface slot, not a shared-instance client
endpoint. Synchronous Tokio `Hub` output can retain the source client, but
asynchronous owned-Link output broadcasts to sibling clients on that slot until
Link state carries endpoint-aware client identity and reconnect generation.
Keepalives now use exact unencrypted 20-byte `0xff` initiator requests and
`0xfe` responder replies. The
initiator alone probes after both a full inbound-silence interval and a full
interval since its previous probe; deterministic valid repeats avoid dedup only
after bound-interface admission, lifecycle traffic emits no
application event, and automatic output preflights and retains the bound route
before committing its timer. Stale starts after two intervals and keeps a
transition-relative `4 * RTT + 5 seconds` revival window (five seconds when RTT
is zero); valid bound Link traffic revives it. Channel send preflights MDU,
pending-window and receipt capacity;
maintenance discovers immutable retry tokens; NodeCore preflights the bound
route; and a fresh-ciphertext retry atomically replaces the envelope's sole
receipt before retry/window/timestamp state commits. Obsolete proofs fail
closed, full-table replacement succeeds in place, and Link removal reclaims
channel receipts. Automatic timeout removal still emits no `LINKCLOSE` packet.
Receipt capacity below an adaptive channel window remains typed backpressure
and a product sizing/throughput decision.

Arbitrary remote HEADER_1 LINKREQUEST remains disabled until interface roles
distinguish it from local-origin injection. The temporary H1 DATA compatibility
path guards reverse exhaustion and truncated-key conflicts before native
ingress. Rete snapshot loading restores identities only. Saved path
observations and cached announces remain inactive until a stable interface
identity can be rebound explicitly; the node must relearn them after restart.

## Purpose and boundary

`reticulum-node-core` proves the portable state transitions on both sides of an
owning interface handoff. The permanent E290 composition supplies and
registers its fixed 500-byte `TxPacketBuffer` pool; node-core can prepare
encrypted RNS DATA directly into a buffer, resolve the route, and return a
unique routed `TxJob` without moving or copying the packet array. The same
owner then moves through
permit-pending, authorized or unpermitted, completion, and recovery typestates.
Alongside the concrete RNS owner, node-core stores only fixed scalar dispatch
metadata for those external buffers and the fixed DATA attempt ledger. Proofs
and timeouts move the exact attempt into an in-place terminal tombstone before
Rete removes its receipt.

The independent `OrdinaryActionOwner` registers a second class of caller-owned
500-byte buffers against the same opaque node/incarnation scope. It can only be
issued by the one-shot `NodeCore::take_ordinary_action_owner()` claim. Its
constructor is not public and dropping the issued pool does not release the
claim. This is an identity invariant, not just a convenience: slot IDs and
generations are pool-local, so two pools with the same `TxOwnerScope` could
otherwise both create slot-zero/generation-one jobs and accept each other's
owners.

Static buffer references live by value in `OrdinaryBufferPool`, indexed by
their registered stable slots. `register_and_park()` validates the destination
slot before changing either owner metadata or the supplied buffer;
`park_return()` retains the complete owning return on any collision or metadata
error. `admit_from_pool()` borrows the parking table only briefly and moves the
selected references into jobs with the buffers' original static lifetime. This
allows a permanent machine to park a terminal return and admit the same exact
pointer again. The older borrowed-slice `admit()` remains only for bounded
short-lived stack scopes; a static slice borrowed through that API cannot be a
reusable parking table.

One admission call preflights every packet length and route, the complete free
capacity, selected registered owners, the inclusive deadline, and the required
checked generation range before changing any slot, pointer, or byte. Failure
returns the exact original `NodeActions` and leaves every parked reference in
place; success copies each native packet `Vec` once, in order, and retains the
events and unroutable count explicitly in
`OrdinaryActionBatch`. Each ordinary job retains its exact native target,
stable slot/generation, deadline, selected interface, and admission-time
remaining route. `OrdinaryTxJob` has no public byte accessor, RNode framing
state, or receipt record. `begin_permit(requirements)` moves it into a
separate ordinary-specific non-copy request/pending/reply family. The shared
policy sees the selected interface, packet length, exact opaque resource ID,
nonzero actor-defined units, deadline and cumulative `may_have_transmitted`
value. A covering same-resource reservation changes the authoritative phase
and cumulative history before the grant leaves the owner; mismatched or
under-sized reservations remain unpermitted. Only
`OrdinaryAuthorizedTx::frame(now)` exposes bytes, once and strictly before the
deadline. Ordinary and DATA owning typestates remain separate and share only
interface-neutral scalar policy vocabulary.

Typed ordinary completion validates its class against the routed or authorized
phase. Valid completion advances the same packet generation to the next
interface, returns the external buffer, or stops remaining fan-out through an
explicitly unpermitted or authorized cancellation. Pre-send cancellation
requires the caller to retain the exact unsent request; after sending the
request, pending ownership has no definitely-unsent cancellation shortcut.
Same-generation metadata/class faults and recovery faults retain the exact
unique owner in `OrdinaryTxQuarantine`, while foreign or stale completions are
returned unchanged as validation failures. A definitive final-hop return is
`RouteComplete` even at or after its deadline; `DeadlineExpired` means the
deadline actually suppressed at least one remaining fan-out hop.

The crate privately owns `reticulum-rns-rete::EmbeddedNode`, but its public
surface uses project-owned identities, destination hashes, interface targets,
packet-slot IDs, deadlines, attempt tokens and handles, terminal outcomes,
errors and capacity snapshots. The protocol-owner surface also forwards the
adapter's `IngressReport` and `NodeActions` envelopes without exposing mutable
Rete state. It has no dependency on the device API, Embassy, radio traits, ESP
crates, a board support package or durable storage.
The separate `reticulum-tx-handoff` edge crate depends on node-core and Embassy
Sync while keeping node-core itself synchronous and executor-free. It exposes
distinct DATA and ordinary-action channel families rather than erasing one
owner protocol into the other.
`reticulum-tx-dispatch` depends on both portable crates and owns their packet-
interface roles in persistent state. It has no direct device-API, executor,
clock, TX-capable driver/HAL, or firmware dependency; node-core's transitive
Rete closure contains no radio, RNode, LoRa or board crate. The separate
`reticulum-rns-rete-rx` vertical-slice adapter owns physical RNode receive and
reassembly. The implemented portable local-session core remains a different
boundary; a future credential-backed firmware dispatcher will depend on both
node-core and device-api and map their types explicitly.

`reticulum-tx-supervisor` is a separate portable edge. Its production
`NodeInterfaceSupervisor` owns one exact node-core, the authoritative interface
router, both coordinators, every per-actor permit server, the authorization
policy, and the monotonic-clock contract. It forwards the sole owner's
destination hash, explicit inbound-proof policy, bounded announce queue/flush,
registry-validated exact-owner RNS ingress, and timer maintenance. No public
supervisor method accepts a caller-selected raw interface ID. The older
RF-inert `TxSupervisor` and its async runner remain only a legacy DATA-machine
test aggregate; neither aggregate depends on firmware, radio/HAL, flash, or the
device API.

Inbound proof generation defaults to `Never` and must be explicitly changed to
`Always`. `queue_announce()` copies optional application data into Rete's
bounded pending queue and reports oversize or full-queue admission failures;
`flush_announces()` later returns the ready broadcast actions. Ingress and tick
likewise return all ordinary application events and outbound packets to the
caller. These native `NodeActions` remain allocation-backed until a caller
atomically admits the complete envelope into `OrdinaryActionOwner`. That owner
now connects to the interface router's ticketed per-actor job/completion queue
and a separate permit-only handoff/server. The implemented
`reticulum-radio-tx-dispatch` consumes the router's DATA/ordinary actor union
and retains LoRa-specific framing/access policy locally. The permanent E290
composition now joins it to `NodeInterfaceSupervisor`, timed RNode RX and the
sole E290 radio owner. No local DATA submission or client-delivery edge is
present yet, so that autonomous image does not originate a controlled DATA
exchange.

`TxJob`, permit requests/replies, completions, buffers, and recovery records do
not expose packet bytes. Only an exactly matched `AuthorizedTx` can borrow the
encoded frame, once, through `frame(now)` before its deadline. This API proves
authorization semantics; it does not itself perform RF transmission. The
legacy RF-inert dispatcher consumes authorized bytes only through a private
scalar inspector. The separate ticket-aware radio dispatcher is the sole byte
consumer in the permanent LoRa actor and owns the actual RNode/radio boundary.

`NoRfTxDispatcher` keeps each unique job, permit-pending/authorized owner,
completion, pressured owner return, and unmatched control value in a compact
persistent enum. Every `step(now)` completes one synchronous transition, so an
exact `ChannelFull<T>` value is restored before control returns. Its short
`wait_for_input()` receives only while idle or waiting for a permit reply and
stores a ready channel value in persistent state in the same poll.
`TxPermitServer` does the same for node-side permit requests, invokes policy at
most once and only for a validated live candidate, and retains a reply under
pressure. `NodeTxDataMachine` consumes the sole node job/return roles, validates
exactly the registered fixed pool during boot, and parks available, recovered,
or quarantined owners by stable slot. It processes completions through
node-core, retains every `Next` continuation unchanged until the job channel
accepts it, and synchronously prepares fresh DATA from the lowest available
parked slot. Queued returns and retained transitions take priority. Queue
preflight leaves entropy and node state untouched; an ordinary preparation
rejection reparks the validated exact owner, while a fail-closed rejection
parks its owning quarantine; an unexpected authoritative enqueue failure
retains the definitely-unsent job for rollback with the next fresh clock sample.
Terminal, expired, recovery-required, or invalid requests bypass policy.
Cancellation while a short wait remains pending leaves the item in its Embassy
channel. The DATA machine stores a ready return in persistent state before its
wait completes and waits for `Next` capacity without putting the job into the
future. Public `TxSupervisor::wait_for_work()` selects those phase-compatible
waits plus the next absolute deadline and may safely lose a race against an
independently owned RX or RNS-timer future. The permanent top-level task
borrowing the aggregate must itself never be cancelled.

Each complete supervisor pass takes a fresh checked monotonic sample before
lease maintenance, DATA processing, permit/policy processing, and dispatcher
processing. `NodeCore::next_tx_deadline()` supplies the earliest live owner
deadline; the supervisor combines it with the active permit-recovery grace and
waits for the exact earlier instant. Sustained progress yields after 16 passes.
A retained fault blocks fresh preparation and further policy calls while DATA
and dispatcher stepping continue to drain exact owners where possible.

## External ownership and registration

`TxPacketBuffer` owns the packet array and deliberately implements neither
`Copy` nor `Clone`. Firmware will allocate each buffer in static storage and
move its unique mutable reference through the owning handoff. Node-core never
contains that 500-byte array.

Before handoff, `register_packet_buffer()` binds each buffer exactly once to
the node identity, `NodeInstanceId` and a stable `PacketSlotId`. The matching
`PACKET_BUFFERS` entry changes from `Unregistered` to `Free`; a second
registration, full metadata table or zero-capacity profile is a typed
registration rejection. A foreign owner/incarnation is rejected later if that
buffer is supplied for preparation. Registration does not prepare a packet or
register a receipt.

The related states are:

```text
external buffer:   Unregistered -> Available -> Bound(unique TX owner)
                                                   |          |
                                                   v          v
                                               Available  TxQuarantine

dispatch metadata: Unregistered -> Free -> Reserved -> Routed -> Authorized
                                                         ^           |
                                                         |-- next ---|
                                                         |           |
                                                         v           v
                                                    RecoveryRequired -> Free

attempt ledger:                   Free -> Reserved -> Active -> Terminal -> Free
                                          \-------> Free

Reserved -> Free: native preparation failure before an attempt becomes active
Active -> Terminal: proof, timeout, or exact definitely-unsent cancellation
Terminal -> Free: durable projection and explicit acknowledgement after unique
                  buffer return
```

`Bound(unique TX owner)` is an ownership statement. Only `AuthorizedTx` means
the permit linearization point has passed, and even that means “may have
transmitted”, not proof that hardware started or completed RF.

## Preparation transaction

`prepare_data_into_slot()` accepts a registered available buffer, a
`PrepareDataRequest`, and entropy. The DATA/receipt path keeps its coarse RNS
deadline clock separate from both precise microsecond Link timing and the
packet-owner millisecond clock. The request includes the destination,
plaintext, `owner_now`, `TxLeaseDeadline`, and a synchronous snapshot of
enabled packet interfaces:

1. Reject `deadline <= owner_now` before reservation, entropy use, or RNS
   mutation.
2. Validate the buffer owner/incarnation and its free dispatch slot.
3. Reserve a free attempt-ledger slot and checked, non-repeating attempt,
   dispatch, and hop generations before consuming entropy or mutating RNS.
4. Mark dispatch and attempt metadata `Reserved`, then call
   `EmbeddedNode::prepare_data_into()` with the external buffer's exact
   500-byte array.
5. On any native preparation failure, restore both metadata reservations and
   return the same available buffer inside `PrepareFailure`.
6. On success, commit the full receipt hash to the active attempt and resolve
   Rete's `All`, `Only(interface)`, or `AllExcept(interface)` target against the
   supplied enabled-interface snapshot. Locally originated destination DATA is
   `Only` the interface on which the current path was learned; a peer registered
   without an observed interface retains the unknown/direct-path `All` fallback.
7. Bind the first deterministic hop, dispatch generation, target, and deadline
   into the buffer and return the only `TxJob` that owns its mutable reference.

Capacity, registration and generation failures are rejected before entropy or
RNS mutation. Native receipt-table saturation, duplicate receipt hashes,
unknown destinations, oversized payloads, cryptography and packet-build
failures unwind the reservations and return the exact same buffer. After
native success, committing the pre-reserved metadata cannot allocate or fail.
An empty resolved route cancels the exact receipt and returns the same
available buffer. If that cancellation unexpectedly fails, preparation returns
an owning `TxQuarantine` and leaves the scalar recovery record retained instead
of pretending the buffer is reusable.

`TxJob` exposes the stable slot, encoded length, preparation-time SHA-256 over
the complete encoded packet, distinct RNS proof-correlation token,
generation-scoped attempt handle, original interface target, selected interface,
and deadline. It has no byte accessor. Route selection is ascending by
`PacketInterfaceId` bit, and each subsequent hop receives a fresh checked
generation. Multi-interface fan-out is serialized through the same unique
buffer; no packet copy or repeated interface is introduced.

The copy-only `NodeTxQueuedHop` metadata reported by the DATA machine preserves
that `AttemptHandle` alongside the slot, full attempt token, interface, packet
length, full-packet digest, and deadline. `reticulum-submission-projector`
retains the handle, token, expected length, and expected digest in volatile
correlation state; the durable record contains no packet slot, generation,
deadline, or reference.

The adapter independently preflights the current 383-byte plaintext limit and
bounded receipt table before encryption. `AttemptToken` is the full Reticulum
hashable-part digest covered by a proof, not SHA-256 of every encoded interface
byte. Node-core computes the distinct complete-packet SHA-256 immediately after
successful preparation and retains it in authoritative queued metadata. The
RF-inert dispatch inspector independently rehashes the exact frame while it
holds the authorized byte borrow. The projector requires the preparation and
sink digests and lengths to agree before retaining an exact planned record;
only a storage backend's commit or exact readback result permits the live index
to apply it.

## Queue rejection and exact rollback

`NodeTxDataMachine` preflights job capacity before removing a parked owner, so
ordinary queue pressure consumes no buffer, entropy, or node state. If the
authoritative handoff still returns `ChannelFull<RoutedTxJob<'static>>`, the
machine retains that exact job as `FreshRollbackPending`. Its next
`step(owner, fresh_now)` calls `rollback_queued(job, fresh_now)`; it never reuses
the preparation request's older `owner_now`. Node-core synchronously proves that
the recovered routed job was never accepted. It validates the node incarnation,
stable slot, dispatch generation, receipt, attempt handle, target, hop, and
deadline before changing any state and returns the same
`TxCompletionDisposition` vocabulary as ordinary completion handling.

For a still-active attempt, rollback cancels the exact full-hash RNS receipt and
commits a retained `Terminal(Unsent(QueueRollback))` tombstone before freeing
the dispatch metadata. It restores the same external buffer to `Available` and
returns its original unique mutable reference, but the attempt ledger does not
become `Free` until that unsent final disposition is durably projected and the
exact terminal is explicitly acknowledged. If the receipt is unexpectedly
missing, or any binding is stale or inconsistent, `RollbackFailure` retains the
still-bound `TxJob`; the buffer is not silently reused. A rollback observed at
or after its deadline first enters the exact scalar recovery state, then
finalizes that matching late owner as `Recovered`; it cannot silently bypass
deadline accounting. Rollback is
forbidden once any earlier serialized hop was authorized, even if the current
hop is again in the routed state. The cumulative possibly-transmitted
classification cannot be erased.

A `TxCompletionDisposition::Next` is not a fresh rejection: an earlier route
may already have been authorized. `NodeTxDataMachine` therefore keeps that
exact continuation in persistent state, gives it priority, and retries
`try_send` without rollback until the sole job channel accepts it.

A proof or timeout may commit while the job remains bound. In that case Rete
has already removed the receipt, so rollback releases the dispatch and returns
the same buffer without cancelling a nonexistent receipt, while preserving the
terminal tombstone. `acknowledge_terminal()` reports `PacketStillBound` until
that release occurs. This ordering prevents an application from freeing the
tombstone while unique packet ownership is still outstanding.

Dropping any owning typestate does not reset its private buffer binding or
dispatch entry. A later preparation attempt with that buffer is rejected as
`PacketBufferBusy` before entropy or RNS mutation. Deadline maintenance can
move the authoritative scalar dispatch record into `RecoveryRequired`, but it
never fabricates or force-reuses the missing reference.

## Permit, byte access, and completion

`TxJob::begin_permit(requirements)` consumes routed ownership into
`PermitPendingTx` and an opaque non-`Copy` `TxPermitRequest`. Requirements bind
a `TxPermitResourceId` and nonzero actor-defined resource units; node-core does
not interpret either as a radio, stream, datagram, or other link mechanism.
`authorize_tx(request, now, policy)` first
validates node identity, `NodeInstanceId`, slot, dispatch generation, selected
interface, and per-hop generation. It denies terminal, expired, or retained-
recovery work without invoking policy. Only a fully valid candidate reaches
the synchronous interface-resource policy. The candidate includes the exact
requirements, packet length, and selected interface and carries the exact
`MonotonicMillis` sample supplied as `now`: the same sample used for the
immediately preceding deadline check, without a resample before policy runs.
It is an evaluation-time observation and does not imply that policy grants the
candidate.

A policy authorization must carry a `TxPermitReservation` naming the same
resource whose units cover the request. A resource mismatch or
under-reservation becomes `CapacityUnavailable` while the dispatch remains
unpermitted. A covering reservation immediately and irrevocably changes
authoritative dispatch state to `Authorized` and sets the cumulative
`may_have_transmitted` bit before the non-`Copy` reply leaves node-core. This is
the reservation-consumption and transmission-classification linearization
point; no later fault refunds it. A lost or delayed grant can therefore never
be reclassified as definitely unsent. The exact reservation is available from
the grant and resolved `AuthorizedTx` for the selected interface actor.
`PermitPendingTx::resolve(reply, now)` also checks the exact binding; a mismatch
retains both the unique pending owner and unchanged reply. A grant resolved at
or after the exact deadline produces `ExpiredAuthorizedTx`, which has no byte
accessor but remains possibly transmitted.

Only `AuthorizedTx::frame(now)` can expose packet bytes. It is one-shot, binds
the returned `TxFrame` to the exact permitted interface and attempt, and rejects
`now >= deadline`. The owner cannot be consumed into a completion while its
frame borrow is live. `ExpiredAuthorizedTx`, `AuthorizedTx`, and
`UnpermittedTx` produce owning completions with distinct conservative
classifications.

`complete_tx(completion, now)` reconciles that classification against the
authoritative scalar dispatch phase. An unpermitted hop advances to the next
deterministic route only while the attempt is nonterminal and its deadline is
still in the future. After the final definitely-unsent hop, the exact receipt
is cancelled only if no prior hop was authorized, and the attempt becomes a
retained unsent terminal rather than immediately free. Once any hop was
authorized, the receipt remains live through all later definitely-unsent
returns. A proof or timeout terminal stops later fan-out while preserving its
tombstone.

## Exact deadlines and retained recovery

Deadline comparison is consistently inclusive: `now >= deadline` is expired
in preparation preflight, authorization, permit-reply resolution, frame
access, completion, and `maintain_tx()`. Expiry never frees a slot. Maintenance
moves routed or authorized scalar dispatch metadata to `RecoveryRequired` and
publishes records in stable slot order. Each `TxRecoveryRecord` includes the
`NodeInstanceId`, packet slot, dispatch generation, selected interface,
deadline, observation time, prior phase, cumulative possibly-transmitted bit,
and bounded reason.

While the unique owner is missing, this scalar dispatch record is
authoritative. A coherent late completion for the exact owner/incarnation,
slot, generation, interface, and conservative phase returns
`TxCompletionDisposition::Recovered`; node-core finalizes the metadata and the
same buffer binding becomes reusable. `NodeTxDataMachine` parks the recovered
buffer with its complete generation-safe observation and does not expose it as
available until exact acknowledgement. The supervisor exposes both the
observation and acknowledgement facade. `reticulum-submission-projector`
withholds that action until the transport audit is known committed.
`reticulum-storage-journal` supplies the
physical append/replay/compaction backend, and `reticulum-storage-actor` now
owns that journal, the live replay index and the sole projector. It retains one
bounded exact pending mutation and can autonomously reconcile an ambiguous
backend result before exposing the acknowledgement. The E290 image now mounts
and recovers that runtime before service, but no resident firmware path yet
drives live observations or acknowledgements from this node/supervisor path.
An internally inconsistent same-lease return or an explicit recovery fault
returns an owning `TxQuarantine` and retains the fail-closed scalar record.
Before exposing its `TxRecoveryObservation`, quarantine canonicalizes the
private buffer binding from that authoritative dispatch record; inconsistent
buffer-side handle or token metadata cannot redefine durable correlation.
Wrong-owner or stale completions are rejected intact as `TxCompletionFailure`
and disable the node DATA machine without losing the completion. Recovery never
invents ownership, and notification loss cannot make a slot reusable.

The RF-inert dispatcher adds a configured permit recovery grace after the owner
deadline because the node may already have authorized a request whose reply is
delayed. On the first step sampling at or after that threshold it checks for a
reply first. Any reply observable by that step wins regardless of enqueue time;
a late grant is resolved as byte-inaccessible `ExpiredAuthorizedTx`. With no
observable reply, it returns the exact pending owner as a recovery-fault
completion, permanently disables itself, and requires node-core to
quarantine/reconcile the return. It never guesses whether authorization
occurred.

## Receipt terminal ledger

`EmbeddedNode::ingest_with_receipt_sink()` and
`tick_with_receipt_sink()` present the exact receipt kind and complete packet
hash before changing Rete receipt or proof-deduplication state. Node-core's
private reservation matches only an active DATA slot with that full hash. The
same slot becomes `Terminal { Delivered | DeliveryTimeout }`, so terminal
commit cannot allocate or fill a second queue. A proof or timeout can establish
that tombstone before permit resolution or frame observation; the projector
uses the preparation-bound digest and length for direct delivery and permits a
timeout to finalize without first writing `AwaitingDelivery`.

Unknown DATA hashes, already-terminal hashes and channel candidates are typed
`ReceiptCorrelationError` invariants, not retryable capacity failures. Rete
retains the affected receipt/proof state. Timer maintenance still returns its
ordinary actions alongside a fault so unrelated maintenance output is not
lost.

`terminal_attempts()` observes tombstones without removing them.
`acknowledge_terminal(handle)` frees one only after the storage backend has
proved the exact final record durable, the projector has unlocked that
acknowledgement, and no job remains bound. Node identity,
`NodeInstanceId`, ledger index and monotonic generation scope the opaque handle
against stale copies and ABA reuse. The ledger remains RAM-only and cannot
rehydrate active Rete receipts or terminal submissions after reset.

## Capacity and constrained hardware

`PACKET_BUFFERS` now bounds registered external buffers and node-owned dispatch
metadata; it no longer multiplies a 500-byte array inside `NodeCore`. Firmware
must still allocate the actual buffers, so product RAM budgeting includes at
least `PACKET_BUFFERS * 500` external packet bytes, each buffer's binding
metadata, node-core dispatch/attempt/Rete state and the handoff channel items
that carry their references. The node DATA machine additionally uses one fixed
per-slot enum containing a pointer plus bounded recovery metadata at most; a
layout guard keeps every packet array external. The current firmware allocates
none of this TX path.

`PATHS` bounds the current Rete path, reverse and destination-DATA receipt maps
and the node-core attempt ledger. `LINKS` independently bounds the owned and
relay Link maps. Rete's heapless maps require `PATHS` and `LINKS` to be powers
of two greater than one; announce and deduplication deques require
`ANNOUNCES > 0` and `DEDUPLICATION > 0`.
`capacity_profile_is_supported()` exposes that project-owned guard, and
`NodeCore::new()` has compile-time assertions for a monomorphized invalid
profile. Retained tombstones intentionally reduce new-attempt capacity until
acknowledged.

Node-core's `CapacitySnapshot` reports registered buffers,
queued/used/configured dispatch metadata, current/configured Rete receipts and
active/terminal/used/configured attempt slots. The enclosing `EmbeddedNode`
metrics separately report owned Links, relay Links and reverse occupancy. Both
surfaces expose only scalar counts, never a native Rete collection or packet
bytes.

The crate itself uses no allocator API and compiles for generic bare metal.
Its owned Rete node still has allocation-backed construction and ordinary
ingest/action paths, so this result applies only to external-buffer DATA
preparation, dispatch metadata and the attempt ledger. Before selecting a
no-PSRAM Tracker profile, record Xtensa static layout plus linked `.bss`, heap
and stack headroom for the complete firmware allocation.

## Current validation

```sh
cargo test --locked -p reticulum-node-core
cargo clippy --locked -p reticulum-node-core --all-targets -- -D warnings
cargo check --locked -p reticulum-node-core \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-node-core \
  --target xtensa-esp32s3-none-elf
cargo test --locked -p reticulum-tx-handoff
cargo clippy --locked -p reticulum-tx-handoff --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-handoff \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-handoff \
  --target xtensa-esp32s3-none-elf
cargo test --locked -p reticulum-tx-dispatch
cargo clippy --locked -p reticulum-tx-dispatch --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-dispatch \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-dispatch \
  --target xtensa-esp32s3-none-elf
cargo test --locked -p reticulum-tx-supervisor
cargo clippy --locked -p reticulum-tx-supervisor --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-supervisor \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
```

The 69-test node-core host suite covers bounded announce admission/flush,
explicit inbound-proof policy, stable one-time registration, pointer-stable no-
copy preparation, deadline-before-mutation rejection, empty and deterministic
multi-interface routes, per-hop generations, exact queue rollback, cumulative
prior authorization, opaque permit matching, policy and terminal races, exact-
deadline authorization/reply/frame/completion/maintenance behavior, serialized
fan-out, coherent late recovery, fault and invariant quarantine, cross-
incarnation/stale returns, completion-metadata tamper quarantine, receipt
terminal races, tombstone backpressure, and exact acknowledgement reuse. A
layout guard keeps packet-sized arrays out of
dispatch slots. Eleven handoff unit tests cover production-mutex static
construction, static-reference identity, FIFO ordering, owner/control pressure,
exact `ChannelFull<T>` returns, mismatched permit replies, ordinary common-
origin splitting without a seed, ordinary receive cancellation, and crossed
ordinary permit replies with exact full-channel cancellation. Five host-only
integration tests manually step the real job/request/reply/return ports through
authorized no-RF frame inspection, policy denial, exact-deadline grant
expiry/recovery, serialized two-interface fan-out with the same owner, and
terminal-before-authorization suppression. Generic bare-metal and ESP32-S3
checks compile node-core, the Embassy edge, the RF-inert dispatcher, and the
permanent supervisor. The 33-test TX-dispatch suite comprises fifteen
dispatcher/permit tests covering persistent serialized fan-out, exact-deadline
late-grant recovery, cancellation of short waits, terminal suppression, absent
and mismatched reply fail-closed behavior, one-shot policy invocation under
reply pressure, exact owner restoration under return pressure, inclusive grace
threshold observation semantics, authorization/recovery orderings, idle
orphan-reply wakeup, and production-mutex static layout. Eighteen node
DATA-machine tests cover validated fixed-pool seeding, exact buffer identity,
lowest-slot synchronous preparation, rejection restoration without entropy,
return and `Next` priority, queue preflight, before/deadline rollback, rollback
failure retention, final and recovered parking, generation-scoped recovery
acknowledgement, quarantine, exact owner binding, completion-failure retention,
cancelled return waits, `Next` pressure/readiness cancellation, and compact
production-mutex layout.
The 13 legacy `TxSupervisor` tests cover separate and deadline-crossing fresh
clock samples, the complete RF-denied lifecycle, exact-deadline recovery retention,
terminal/recovery acknowledgement facades, permit-grace reply priority and
fault drain, monotonic regression, public combined-wait cancellation, the
permanent protocol-owner forwarding surface, deadline conversion, common-
origin/full-seed construction, and static storage. Twenty-one focused ordinary-
action tests cover atomic full-pool failure with exact envelope return, packet
order and every target shape, event/unroutable retention, registration and
foreign/busy/invariant rejection, inclusive deadline return, stale and
exhausted generations, the one-shot same-scope pool claim, final-hop deadline
classification, serialized fan-out, exact requirements/reservation matching,
one-shot authorized bytes, cumulative transmission history, delayed grants,
typed cancellation, quarantine, the minimum active deadline, oversize
validation, owning park failures, exact static-pointer recycling, and unchanged
DATA capacity. The focused node-core and RF-inert dispatch suites contain 70
and 33 tests respectively. The production aggregate, ticket-aware dispatcher
and E290 radio owner are now linked in the permanent LoRa-first firmware graph;
the RF-inert machines remain focused regression fixtures.

## Next boundary

The portable owning storage layer, RF-inert dispatcher, permit and node DATA-
owner machines, and permanent production supervisor aggregate now exist.
`TxHandoff::split_paired()` consumes one unique static handoff; every registered
owner must seed that inseparable common-origin role set before
`NoRfTxMachineSet::try_new()` can bind it into the supervisor. Incomplete
construction returns the paired roles and queued owners unchanged. Pool-sized
channels carry jobs and owner returns, depth-one channels isolate permit
requests/replies, and every send is a non-awaiting `try_send` that returns the
unchanged value on pressure. Separately, `OrdinaryPermitHandoff::split_paired()`
creates the common-origin node/actor roles for only the depth-one ordinary
permit request/reply channels. Ticketed ordinary jobs and completions use the
interface router's per-actor queues; the fixed `OrdinaryBufferPool` remains in
the coordinator. The remaining product work at this boundary is:

1. Host the implemented portable storage actor in one permanent Embassy task.
   Connect the checked product `esp-storage` partition adapter, gate all service
   on complete mount/replay, and coordinate flash with watchdogs, OTA, other
   stores and radio timing. The isolated journal clean-path/software-reset HIL
   has passed; actor-on-target, controlled power-fail, endurance/soak, and
   integrated-runtime coverage remain open.
2. Preserve and qualify the implemented mirrored identity and announce-clock
   boot gate. It preflights identity without mutation, reserves the next logical
   announce epoch before provisioning/loading, requires redundant key mirrors,
   and keeps protocol-monotonic deadlines separate from the 40-bit local
   announce-emission order. Powered reset and power-cut evidence remains open.
3. Preserve the implemented ADR 0009 credential persistence/pairing and the
   connected immutable authority, authenticated device-API adapter, USB
   framing, minimal qualification-session bearer, and permanent actor task;
   qualify the powered credential and authenticated request path; drive the model's conservative boot
   recovery and node observations through the actor's narrow projector methods;
   and add a proved safe retirement condition for bounded volatile projector
   slots without attempting to persist leases or mutable references.
4. Add bounded local DATA/LXMF submission and durable client delivery without
   changing the completed router/permit/radio ownership path or adding RNode
   state to node-core. Native action allocation and pre-mutation backpressure
   remain separate Rete hardening work.
5. With both attached modules confirmed `HT-RA62-HF` and the isolated semantic
   HIL passed, qualify the permanent pair's boot, radio initialization,
   ordinary ANNOUNCE
   TX/RX, contention and reset behavior. Use an external injector or the
   separate semantic-HIL fixture for controlled DATA/proof until the local
   submission edge exists. Separately named RF HILs and the derived RNode peer
   remain development artifacts, not product dependencies.
