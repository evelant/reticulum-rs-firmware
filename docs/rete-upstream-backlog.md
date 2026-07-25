# Rete upstream hardening backlog

**Status:** the first three focused issues and draft pull requests predate the
current local lifecycle work. No additional issue or pull request will be
opened without direct user approval.

This is the possible contribution queue discovered while integrating Rete
revision `9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743`. If the user directly approves
publication, each selected item should remain a focused test-first change.
Tracker pins, FEM sequencing, regional policy, product quotas and the device
API stay in this project; generic protocol and bounded-state corrections are
the candidates for upstream review.

The current firmware pin is
`dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08` on fork branch
`codex/responder-handshake-reclaim`. It descends from
`ba73ee426a3211951f5abb400c5728dd359272be`,
`354b8757bea63b9d1e27dec14f109fe6c7e03c5a`,
`338251b285a2447beb10d390d3e7f53694a1a916`, and
`a443173b0829c2637ce23531a8cde15fdfec185e`, then from
`2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` on
`codex/link-data-receipts`, which descends from
`90570cafc812b3025011cb690ec74a27f287cb3f`, whose tag is
`firmware-pin-90570ca`; the current revision has no designated durable tag. It
retains
caller-owned DATA preparation, explicit receipt-capacity errors, fixed-capacity
terminal sinks, exact DATA/channel terminal candidates, allocation-atomic
proof/timeout delivery, full-hash receipt cancellation and core-aware LXMF
sibling-attempt cleanup. It additionally carries exact-interface path,
reverse, and Link forwarding through the stack and runtime adapters; one-shot
reverse-proof interface validation; strict relayed LRPROOF direction, hop,
identity, signature, and canonical-header validation; and identities-only
snapshot restore while persisted path observations lack stable interface
identity. The final routing tranche adds transactional relay-Link and H2 reverse
admission, typed owned/relay Link-full and reverse full/conflict stack outcomes,
owned-H2 local dispatch, pre-mutation foreign-H2 filtering, and separately
observable relay-Link capacity. The current Link tranche additionally binds a
responder to LINKREQUEST ingress and an initiator only after valid LRPROOF,
routes established owned-Link output through `BoundInterface`, and rejects
wrong-interface Link DATA/`RESOURCE_PRF` before dedup admission. It snapshots a
known path's hops when an initiator creates a Link, uses the
`PATHFINDER_M = 128` wildcard for an unknown path, rejects LRPROOF hop mismatch
before deduplication/state mutation, and teaches a responder its post-ingress
hop only from authenticated, decrypted LRRTT. It also carries pending-handshake
MessagePack LRRTT parity, microsecond/binary64 dispatch-confirmed timing,
Handshake/Active/Stale refresh and authenticated-malformed teardown, plus the
exact unencrypted, role-specific, full-interval keepalive state machine,
deterministic-packet dedup exception, internal NodeCore lifecycle result, bound
automatic routing, and transition-relative stale revival described below. The
same pin makes initial Channel send and fresh-ciphertext retry receipt-atomic,
preflights the authoritative Link route before entropy or retry mutation,
replaces one envelope's sole proof target in place, rejects stale retry tokens
and obsolete proofs, and reclaims channel receipts on every Link removal path.
The `2d07818` descendant also registers ordinary Link-DATA receipts and applies
the receiving destination's proof policy instead of unconditionally proving
every context-`NONE` Link DATA packet. Its `a443173` descendant additionally
reclaims responder `Handshake` state at the Python-compatible establishment
deadline. The direct-request sequence adds bounded canonical MessagePack
values, including anonymous `nil`, at `338251b`; separate prepared/confirmed
authorities and first-dispatch timeout registration at `354b875`; lossless
inbound encoded values and their original timestamp at `ba73ee4`; and
phase-agnostic exact request-dispatch reclaim keyed by request and Link IDs
with prior native-phase reporting at `dfcaa36`. This is direct single-packet
request foundation, not full NomadNet or Resource support.
The legacy LXMF event handler without mutable core access still leaves siblings
to timeout.
This candidate remains on the project fork; no issue or pull request was opened
for the newer lifecycle or routing work, and publication still requires direct
user approval. The preceding pin has host/portable and build-only E290
evidence. The subsequent pre-PSRAM application-event release passed its root host/portable
validation, 647-check schema-2 conformance run, and default/HIL E290 build
gates. Its default package is a 789,504-byte merged image using
723,968/6,291,456 application bytes (11.51%); it matched an exact `3e:88`
readback and served an authenticated `identity-summary`. The runtime-
measurement HIL package is 800,480 bytes using 734,944/6,291,456 (11.68%) and
its target-scoped rebuild matched an exact `3e:88` readback. The authenticated
108,940-ms checkpoint retained 63,828 painted stack bytes (10,148 after the
maximum-frame deduction), recorded two confirmed transmissions, and observed
no unexpected error, failed allocation, watchdog timeout, or correlation
fault. The board was restored
to an exact-readback 789,504-byte default rebuild and served
`identity-summary`. Board `3f:88` did not enumerate. These are historical
pre-PSRAM artifact and checkpoint records. The current post-offload placement
evidence and still-open two-board LXMF qualification are recorded in the
[E290 runbook](e290-node.md#stage-5-psram-boot-checkpoint); older artifact
records remain bound to their recorded revisions.

## 1. Transactional Link admission — completed for owned and H2 relay paths

**Priority:** retain regression coverage and finish H1 interface roles

Owned-Link admission is adopted in integration-fork revision
`5ce8c4e437d3f2f07d302bc366ff06bacd6aff2d` and offered upstream in
[issue 8](https://github.com/s-retlaw/rete/issues/8) and
[draft PR 9](https://github.com/s-retlaw/rete/pull/9). It now:

- returns distinct outbound `LinkAlreadyExists` and `LinkTableFull` errors
  without releasing a request or mutating existing Link/path state;
- admits inbound responder state before signing LRPROOF and rolls it back if
  proof signing or LRPROOF packet serialization fails;
- returns typed owned-table capacity without a proof, success event or false
  success/error counters;
- retains Reticulum's packet-level dedup behavior after capacity rejection,
  while admitting a fresh request once capacity is available; and
- suppresses `NodeCore` proof/event output when responder state is full.

The current pin also makes H2 relay admission transactional. A
LINKREQUEST/SINGLE naming this transport requires a usable exact path before
state is inserted; the transport distinguishes existing, newly inserted, full,
and missing-route outcomes. Full admission returns typed
`LinkTableFull { table: Relay, .. }`, emits no forwarding packet and preserves
Link/raw/dedup state except for the intentional replay-filter record. Missing
routes leave no orphan Link entry. Relay-Link count is observable independently
from locally owned Links. Focused tests cover full, existing, missing-route,
fresh-retry and duplicate-retry behavior. The previous 235-check project runner
completed an A--B--C LINKREQUEST/LRPROOF/LRRTT/channel/proof flow, including 8
Channel retry/receipt-replacement checks, 40 released-Python LRRTT MessagePack
checks, and 40 exact keepalive lifecycle checks. The schema-2 lifecycle/
candidate suite does not yet have a final published count.

The remaining product gate is role classification, not relay capacity.
Arbitrary remote H1 LINKREQUEST stays disabled until an interface role can
distinguish remote ingress from the local-origin H1 compatibility path. The
local `EmbeddedNode` retains product-level owned-Link quota preflight for stable
quotas and diagnostics.

## 2. LINKREQUEST validation, HEADER_2 dispatch and reverse admission — H2
covered

**Priority:** protocol correctness and hostile-input handling

Direct/local HEADER_1 shape validation is adopted from integration-fork commit
`05de2c2b2eda71e9ba6fc64d1f4d7a6f5ec320de` and is offered upstream in
[issue 6](https://github.com/s-retlaw/rete/issues/6) and
[draft PR 7](https://github.com/s-retlaw/rete/pull/7). It accepts exactly the
legacy 64-byte and current 67-byte requests and rejects non-`Single`, nonzero
context and non-canonical lengths before responder Link construction/insertion
or LRPROOF output. The adapter retains the same preflight because Rete still
flattens this rejection to `Invalid`, while the product API reports a typed
reason.

The current pin closes the H2 ordering defect. Non-ANNOUNCE H2 traffic naming a
foreign transport drops before state, statistics, dedup or raw-byte mutation;
H2 ANNOUNCE deliberately enters ordinary validation regardless of transport
ownership. Traffic naming this transport is normalized before typed dispatch:
owned local DATA, LINKREQUEST, Link, proof and receipt traffic reaches the same
handlers as canonical H1 input, and locally terminating LINKREQUEST still
passes the adapter's destination direction, `accepts_links`, shape and capacity
policy. Only nonlocal DATA/SINGLE and LINKREQUEST/SINGLE enter the narrow relay
path, where exact route and state admission are one transaction. Endpoint-mode
unmatched forwarding remains suppressed by the product adapter.

The current pin removes interface-zero fallback from admitted forwarding.
Ordinary H1/H2 DATA requires a path with a recorded receiving interface and
emits that exact target, including the intentional same-interface case. A
reverse entry records both ingress and outbound interfaces; its proof is
one-shot, forwards only when received from that outbound interface, and is
consumed and dropped on the wrong interface. Link DATA and non-LRPROOF Link
proofs require the stored direction and exact hop count. LRPROOF can travel
only from the responder-side outbound interface at the stored remaining hops
to the initiator-side interface, after responder identity reconstruction and
signature validation. Invalid direction, hop, identity, or signature does not
refresh the Link entry. H2 LINKREQUEST now creates only its Link route, not a
redundant reverse entry. A HEADER_2 LRPROOF targeted at the local transport
identity is normalized into the canonical validation path rather than being
allowed to bypass it through generic H2 Link handling.

H2 capacity admission is now explicit. A new reverse route returns typed
`ReverseTableFull` or `ReverseRouteConflict` before forwarding when it cannot be
retained; idempotent insertion of the same exact pair succeeds without
refreshing its timestamp. Relay Link failure likewise returns typed
`LinkTableFull { table: Relay, .. }`. Each failure preserves existing state and
raw bytes, emits no packet and remains deduplicated on exact replay. The adapter
maps those native stack rejections directly and exposes separate relay-Link and
reverse occupancy.

Remaining work is narrower: add released-Python differential fixtures for all
64/67-byte direct/local destination-policy and transport-ID boundaries, define
explicit interface roles, then replace the H1 DATA guard and enable or reject
remote H1 LINKREQUEST by role rather than packet shape alone. Keep the H2
exhaustion, collision, owned-local, foreign-filter, endpoint suppression,
pending-Link expected-hop, and three-node A--B--C regressions as permanent
gates.

## 3. Announce retransmission role policy

**Priority:** released-Python compatibility and RF behavior

Endpoint announce admission is adopted in integration-fork revision
`5ce8c4e437d3f2f07d302bc366ff06bacd6aff2d` and offered upstream in
[issue 10](https://github.com/s-retlaw/rete/issues/10) and
[draft PR 11](https://github.com/s-retlaw/rete/pull/11). Rete previously queued
every valid received announce and `NodeCore` immediately returned it as a
broadcast, even when `enable_transport()` had never been called. The fix gates
the received-announce retransmission queue on transport mode while preserving
endpoint validation, deduplication, path and identity learning, cached bytes,
ratchet handling, counters and the `AnnounceReceived` event.

Released Python also admits announcements received from a local shared-instance
client and excludes `PATH_RESPONSE` from ordinary rebroadcast. Rete does not
yet model that local-client role at this seam, does not apply the path-response
exclusion, and has additional role-sensitive cached-announce and known-path
response surfaces. Audit and cover those independently instead of silently
folding them into the endpoint fix.

### Local secondary-destination discovery gaps

The permanent E290 source now uses Rete's destination-specific announce
primitive for both its primary destination and registered `lxmf.delivery`
destination. Two pinned-revision behaviors remain incomplete for responsive,
collision-resistant discovery:

- A path request matching a registered local destination enters the forwarding
  path instead of queuing an announce for that destination. Periodic service
  announces work, but a peer that misses one cannot promptly recover by
  requesting the path.
- Announce retransmission derives `jitter_ms` with `% 500`, then converts it to
  a whole-second value only when `jitter_ms >= 500`. That condition is
  unreachable, so the effective delay is always zero. Simultaneously booted
  devices can therefore retransmit in lockstep.

The old E290 product scheduler exposed the practical interaction between this
transport behavior and half-duplex LoRa. It queued primary and
`lxmf.delivery` back-to-back; B processed exactly three distinct A announces
across the powered bootstrap cycles but still returned `no-path` for A's LXMF
destination. Rete transport mode immediately relays the first accepted announce,
so B transmitted that relay while A sent the secondary and could not receive it.

Current product source mitigates that composition without changing the Rete
pin. It schedules at most one destination per event, separates primary, LXMF,
and Nomad destinations by eight seconds, runs two retry triples with an
identity-derived first phase, and then enters the 30-minute cadence. That does
not close responsive path-request handling or the generic retransmission-jitter
defect. Retain local regressions for both Rete behaviors before changing the
pin. No issue or pull request has been opened for either item.

## 4. Link state events and outbound activity

**Priority:** application correctness and timeout behavior

The current pin closes the physical-interface routing part of this item. A
responder binds to LINKREQUEST ingress; an initiator's initial learned path is
only a request target, and the Link binds after valid LRPROOF ingress. Once
bound, application calls and asynchronous close, keepalive, retransmit,
request/response and Resource output carry `BoundInterface`. Only the initial
LINKREQUEST may broadcast, and only when no learned path interface exists.
Link DATA and `RESOURCE_PRF` received on another interface fail before dedup, so
a later copy on the bound interface is not poisoned.

That binding is a runtime interface slot, not a hosted client endpoint. On a
Tokio shared `Hub`, synchronous output can retain the source client but
asynchronous owned-Link output broadcasts to the other clients on the bound
slot. Replace the scalar slot with endpoint-aware identity including reconnect
generation before claiming Python-style per-client Link isolation.

The current pin reserves `LinkEstablished` for the first transition to
`Active`. The project adapter retains a defensive suppression guard, but it is
now expected to observe zero premature native events.

Expected-hop admission and LRRTT payload/lifecycle interoperability are
resolved at this pin. Rete emits canonical MessagePack float64, accepts the
numeric scalar families and first-object/trailing-byte behavior returned by
Python's u-msgpack, and selects the greater local or peer RTT with Python
ordering, including non-finite values. It retains the immutable request anchor,
uses microsecond `MonotonicInstant`/`MonotonicDuration`, and stores RTT as
binary64. Opaque, non-repeating eight-byte tokens correlate LINKREQUEST and
LRPROOF output with only the first successful interface confirmation. The
initiator anchors at the egress interval start and responder at completion;
this means router/interface handoff, not physical RF `TxDone`.

Fresh authenticated LRRTT is processed in `Handshake`, `Active`, and `Stale`.
Repeats emit `LinkRttUpdated` without a second establishment event or statistic,
and exact raw replay remains deduplicated. Authenticated malformed LRRTT tears
down any of those states; only Handshake increments `links_failed`. Zero RTT
retains the 5-second keepalive/10-second stale floor, while dynamic stale grace
is `4 * RTT + 5 seconds`. Authentication deliberately precedes liveness
mutation, so corrupt stale LRRTT does not revive Rete even though released
Python updates liveness first.

The core accepts one precise pre-decrypt ingress sample for its bounded
synchronous handler, rather than reproducing Python's three internal samples.
The firmware adapter uses precise `*_at` paths and confirms ordinary-router
acceptance. The generic upstream Tokio/Embassy runners remain coarse and
unconfirmed, so adopting this dispatch contract there remains contribution
work.

Generic `build_link_data_packet()` users also do not update `last_outbound`.
Best-effort Link data, identify, request and response traffic can therefore
leave native activity telemetry stale. Move outbound timestamp maintenance
into the common successful packet builder or require `now` in the relevant
APIs; keepalive scheduling no longer depends on this timestamp.

The current pin closes the responder half of the establishment leak.
`Transport::tick()` reclaims a responder that remains in `Handshake` without
authenticated LRRTT at `360 + 6 * max(1, post-ingress hops)` seconds, using
confirmed LRPROOF completion as its origin when available and LINKREQUEST
admission otherwise. Exact-boundary and release-then-fresh-retry tests cover
the native four-slot table. Generic native initiators that never receive
LRPROOF still have no automatic `Pending`/`Handshake` deadline; the firmware
owns its initiator deadline and exact abort separately.

Timeout observability remains coarse. Reclamation changes aggregate
`closed_links`/`links_closed` and deliberately does not classify the timeout as
a malformed or cryptographic `links_failed` event. Add a reason-specific
maintenance result/counter if product diagnostics need to distinguish
establishment expiry from established-Link closure. Also decide whether local
LINKREQUEST admission should reject or cap post-ingress hop values: the current
`u8` input permits a Python-compatible timeout from 366 through 1,890 seconds.

The local adapter's old establishment suppression remains as an expected-zero
defensive invariant. It still records the timestamp after best-effort Link data.

Keepalive wire, role, timer and stale-revival behavior is resolved at the
current pin. Rete emits exact unencrypted 20-byte Link DATA frames: only the
initiator sends `0xff`, after a full inbound-silence interval and no more often
than a full interval since its previous probe; only the responder returns
`0xfe`. Valid role-specific deterministic repeats bypass dedup only after the
bound-interface gate. NodeCore consumes them without application events and
preflights/carries `BoundInterface` before automatic construction commits the
probe timestamp. Stale begins after two keepalive intervals and retains a
`4 * RTT + 5 seconds` revival window from the actual transition/final probe
(five seconds when RTT is zero); keepalives and other valid bound Link traffic
revive it. Strict malformed, wrong-role and
legacy encrypted forms do not refresh liveness.

Established-Link watchdog timeout remains a separate residual:
`Transport::tick()` removes the expired Active/Stale Link but does not build and
route Python's timeout `LINKCLOSE`. Add a bounded timeout-outbound result that
preserves the retained interface route until packet formation, without
weakening the completed transition-relative grace behavior. Responder
establishment timeout intentionally emits no `LINKCLOSE`.

## 5. Transactional channel receipts — completed for bounded send/retry lifecycle

**Priority:** retain regressions; size receipt capacity against product window
policy

The current pin closes the former send and retransmission correctness defects:

- initial send preflights negotiated MDU, live-sequence reuse, pending-window
  allocation, channel-receipt capacity, and output allocation before consuming
  entropy or mutating sequence, window, retry, or timestamp state;
- one pending envelope has exactly one live full-hash receipt/proof target;
- maintenance discovers a non-cloneable token bound to the Link session,
  generation, sequence, retry count, and timestamp, then NodeCore preflights
  the authoritative Link route before fresh encryption or retry mutation;
- a successful fresh-ciphertext retry atomically moves the sole proof target
  from H0 to H1 and only then commits retry/window/timestamp state. H0 is
  intentionally obsolete after H1 exists; this is not an LXMF-style sibling
  attempt set;
- exact-hash and truncated-key collisions reject without mutation, and
  capacity-neutral H0-to-H1 replacement succeeds when the receipt map is full;
- route failure consumes no entropy or retry, and generation/session checks
  make stale discovery and ABA-style token reuse fail closed;
- timestamp zero at boot, sequence wrap/reuse, out-of-order duplicate retry
  reception, proof after teardown discovery, every owned-Link removal path,
  and product receipt-sink backpressure have focused regressions; and
- only H1 can reserve and commit the product-owned Channel terminal after
  replacement; H0 leaves both the sink and the one retained receipt unchanged.

The remaining capacity question is policy rather than proof correlation.
`HeaplessStorage<..., L>` still sizes channel receipts with the Link table while
the adaptive channel window can grow beyond `L`; a larger window therefore
receives a correct typed backpressure response. Either size receipts
independently or cap product window policy to the configured receipt budget.

Released-Python parity also remains separate follow-up work: compare dynamic
retry timeout, whether the maximum-tries count includes the initial send,
slow-RTT initial-window selection, and per-envelope window shrink behavior.
Established-Link watchdog timeout `LINKCLOSE` remains item 4. Hosted fallible
allocation and fully sink-backed NodeCore outputs remain item 7; do not
duplicate CBC sizing math merely to reduce the current correctness-first retry
output reservation.

## 6. Explicit ingress dispositions

**Priority:** broaden the completed capacity subset to all recoverable failures

`NodeCore::handle_ingest()` now returns `IngestOutcome::rejection` alongside
events/actions for owned and relay `LinkTableFull`, `ReverseTableFull`, and
`ReverseRouteConflict`. These public Copy/Eq values preserve the affected
truncated hash where relevant, and runtime adapters carry them without
flattening. The project maps all four to stable product dispositions and
counters.

The broader item remains open: ordinary `Invalid` and `Duplicate` outcomes,
parse/crypto/unknown destination or Link, policy, route and output-backpressure
failures are not all typed at the stack boundary. The local adapter still uses
transport counter deltas for native `Invalid`/`Duplicate`, and some cases remain
`NoObservableOutcome`. The native `packets_sent` statistic is also incomplete:
it counts announce-queue output but not ordinary DATA, Link/channel, proof,
close or keepalive packets. Define whether counters represent packet creation,
interface enqueue or completed transmission, then count that boundary
consistently.

## 7. Bounded NodeCore outputs and Resources

**Priority:** full embedded profile

Heapless transport maps do not bound `NodeCore` events, output packets,
destinations, pending requests, resource lists, split queues or assembled
Resource data. Ingress also accepts hosted packets up to 300 KiB and allocates
for packets over the base MTU. The current Resource strategy is too late to be
an embedded admission boundary: advertisement handling constructs an
allocation-backed Resource before checking the 32-Resource table or consulting
`AcceptNone`/`AcceptApp`. One advertised part count can reach 1,048,575 and
immediately size part, hash and received-bit vectors. Request/response
Resources bypass the application strategy and auto-accept.

Introduce an embedded profile with caller-owned/fallible event and packet
sinks, explicit destination/request/resource quotas, and transactional output
backpressure. Resource receive must preflight concurrent transfers, advertised
bytes, parts, split count and aggregate split bytes, assembled/decompressed
output, transient copies, deadline, and retry count before allocation or
protocol mutation. The same limits apply to request/response Resources. The
long-term device path must stream through bounded flash-backed blob storage and
emit a stable object handle instead of assembling every representation in RAM.

Completion currently can retain concatenated ciphertext, a same-sized decrypt
buffer, plaintext/decompressed output, Resource state and final event data at
the same time. Accept/reject collapse missing state and packet-build failures
into an empty output and can remove or mark the Resource despite producing no
wire response. Internal Resource output silently truncates above 256 packets,
while an adaptive window may itself reach 75 packets and exceed the permanent
E290 ordinary-owner pool. Replace burst `Vec` output with a fallible cursor or
reservation bounded by the caller's available packet owners. Retry constants
must be enforced so a stalled receiver cannot retain one transfer forever.

The project-owned application-event seam moves existing allocation-backed
payloads exactly once into a fixed outer owner, but it does not make Rete's
internal creation allocation-atomic. Add a pre-mutation event reservation hook
equivalent to the current receipt-terminal reservation/commit contract. An RNS
Resource offer also needs an incarnation/generation-bound accept/reject token;
the resulting action must return through the ordinary router rather than a
raw mutable-`NodeCore` escape hatch.

The current `HeaplessStorage<..., L>` also cannot instantiate `L = 0` or
`L = 1`: its `heapless::IndexMap` capacity path requires greater than one. A
receive-only endpoint that rejects all Links therefore still reserves at least
two Link slots. Make a zero-Link storage profile representable (or separate
Link storage behind an optional capability) instead of forcing constrained
firmware to pay for unreachable state.

The local adapter caps ingress at 500 bytes, caps destination registration,
preflights receipt tables and rejects every Resource context until this work is
complete. That is a capability gate, not a reduction of the full product
requirements.

Path-request throttling also uses a private `P`-sized timestamp map whose
insertion result is ignored. Once it fills, new destination keys can bypass
the intended throttle. Include it in the capacity snapshot and transactional
admission/drop telemetry.

## 8. RNode receive outcomes and PHY metadata

**Priority:** radio robustness; independently useful

Rete's current split reassembler uses `None` for awaiting a continuation,
empty input and output-buffer failure. Direct oversized calls can panic,
`LoRaInterface::recv()` has no pending-fragment deadline, and PHY packet status
is discarded.

Contribute explicit receive outcomes/errors, length checks before copies, a
caller-driven deadline or timeout contract, and RSSI/SNR preservation. The
project-owned bounded `RnodeRxReassembler`/`TimedRnodeRx` implementation and
hostile tests in `crates/radio-interface` provide a differential
specification.

The generic Embassy runners also race interface `recv()` futures in `select`,
cancelling whichever future loses. `ReteInterface` does not state a
cancellation-safety contract, while a complete `lora-phy` receive operation
cannot safely be treated as cancellable. Either make cancellation an explicit
interface capability, split readiness from completion, or give each physical
interface a sole owner and feed a bounded central queue. The firmware uses the
last model and does not run the Rete Embassy LoRa loop.

The transmit side needs a separate focused audit before reuse. The current
LoRa adapter ignores its configured TX timeout, begins with a deterministic
split sequence, and transmits after CSMA attempts are exhausted instead of
returning a typed busy/deferred result. Generic typed outcomes and randomized
sequence state belong upstream; regional airtime and retry policy remain
product-owned.

## 9. Clock domains and restart-safe snapshots

**Priority:** request interoperability and durable-node correctness

The legacy `NodeCore::send_request()` convenience supplies one `now` value both
as the serialized request timestamp, which is wall-clock Unix time in released
Python, and as the monotonic timeout-registration time. One integer cannot
correctly represent both clocks on an offline device. The current pin closes
that boundary for direct single-packet requests: preparation accepts an exact
binary64 wall-clock value and returns cancelable prepared ownership without
starting a timeout; confirmation consumes that authority and records a
separate monotonic dispatch time only at exact first dispatch. It also accepts
exactly one bounded canonical MessagePack request value, including the
anonymous NomadNet `nil`.

This does not yet type the two scalar clocks, cover Resource-promoted requests,
or constitute a complete NomadNet client. Keep firmware on the new prepared
path, add boot-without-wall-time behavior, and extend the same transactional
ownership boundary before enabling Resource requests rather than silently
emitting uptime as Unix time.

Transport snapshots persist `learned_at`, `last_accessed`, and cached announce
observations without a stable interface identity, monotonic epoch, or wall-time
quality. The current pin takes the safe interim policy: `load_snapshot()`
restores identities only, does not activate saved paths or cached announces,
and therefore cannot advertise or forward on an unbound route after restart.
Add a versioned snapshot-age representation plus a stable interface
identity/generation and explicit rebind-or-drop policy before path restoration
returns. `SnapshotDetail::Full` should not claim broader recovery while it is
identical to `Standard`.

Rete provides useful snapshot and ratchet-store traits, but no checked-in
bounded flash implementation. The default ratchet store bounds only previous
local keys; peer and enforcement maps remain allocation-backed and private-key
arrays are not zeroized on replacement/drop. Generic bounds and key cleanup
belong upstream. The power-fail-safe flash schema, identity lifecycle and
qualified ESP entropy service remain product-owned.

## 10. Interface lifecycle, capacity and observability

**Priority:** multi-interface embedded operation

`ReteInterface` currently exposes only async send/receive. It cannot report
online state, effective MTU, queue capacity, interface role/mode, PHY metadata
or typed backpressure, and the bundled embedded runners have fixed one- and
two-interface shapes. This is enough for examples but not for a dynamic LoRa,
USB, BLE and Wi-Fi device fabric.

Add small orthogonal interfaces for capabilities/status and explicit bounded
enqueue outcomes instead of turning the core trait into a product API. Verify
that AP/Roaming interface mode actually reaches path learning and expiry.
Transport policy and metadata should be generic; discovery, authentication,
client sessions and device-API behavior remain in this project.

Ingress forwarding now carries the native decision directly as
`PacketRouting::ExactInterface` or `AllExceptSource`. Embassy dispatch preserves
an exact same-source slot, and Tokio sends a Direct slot normally or broadcasts
to the other clients of a same-source Hub while excluding the originating
client. Unknown exact indices are dropped. This closes the generic ingress
route-result gap without coupling transport to LoRa.

`NodeCore::prepare_data_packet_into()` still returns bytes and a receipt but not
the selected outbound interface for locally originated DATA. Python Reticulum
sends ordinary destination DATA on the receiving interface recorded by the
current path and broadcasts only when no such interface is known. The project
adapter snapshots Rete's `Path.received_on` and emits `Only(interface)`
accordingly, retaining `All` for a manually registered/unknown-interface path.
A generic outbound-preparation result should carry that decision directly so
sans-I/O callers do not need transport storage access. Native interface indices
also remain transient `u8` values without stable identity or incarnation, so
dynamic interface reuse still requires explicit purge or rebinding.

Owned-Link output now adds `PacketRouting::BoundInterface` and resolves the
scalar binding exactly on physical or Direct interfaces. A shared Tokio Hub is
the remaining exception: asynchronous output lacks the source-client endpoint
and broadcasts to that slot's siblings. The eventual interface identity must
cover both physical slot incarnation and multiplexed client endpoint
generation.

## 11. Transactional DATA receipts and terminal reclamation

**Priority:** blocking sustained outbound DATA and the production device API

At the reviewed upstream base, `NodeCore::build_data_packet()` encrypts and
constructs a DATA packet, touches the selected path, computes the packet hash
and calls `register_receipt()`. The
registration result is ignored. A caller can therefore receive apparent
packet-build success even when no proof can be correlated because the bounded
receipt map was full.

The opposite lifecycle boundary was also incomplete. Proof validation marks a
receipt `Delivered`, while `Transport::tick()` marks an expired receipt
`Failed`. Although the private `ReceiptTable` has a removal primitive,
`Transport` exposes only `receipt_count()`: neither `NodeCore` nor the
project-owned `EmbeddedNode` can inspect or reclaim terminal receipts. With the
current Tracker profile's `P = 16`, any sequence of sixteen delivered or timed-
out DATA packets permanently fills the table.

Make DATA preparation one transaction:

1. Return a packet/receipt token containing at least the packet hash needed for
   later correlation.
2. Propagate receipt insertion failure and avoid path mutation or externally
   observable packet success when admission fails.
3. Expose terminal receipt status/removal without leaking the mutable receipt
   map.
4. Surface newly failed receipts from maintenance and map validated proofs back
   to the corresponding product submission.
5. Test send/proof/reclaim and send/timeout/reclaim cycles for substantially
   more than `P` operations, plus full-table rejection with unchanged path,
   receipt and entropy state.

The generic fix is retained by current project pin
`dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08`, which descends through
`ba73ee426a3211951f5abb400c5728dd359272be`,
`354b8757bea63b9d1e27dec14f109fe6c7e03c5a`,
`338251b285a2447beb10d390d3e7f53694a1a916`, and
`a443173b0829c2637ce23531a8cde15fdfec185e`, then
`2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` from
`90570cafc812b3025011cb690ec74a27f287cb3f` (tag
`firmware-pin-90570ca`). It returns caller-owned packet metadata plus a full
receipt token, reports registration and output-allocation failures, and
atomically removes validated and timed-out receipts only after reserving their
exact kind/full-hash terminal candidate. Link-typed channel proofs are resolved
before ordinary DATA receipts so reservation and mutation agree even if a DATA
receipt's truncated key equals the Link ID. Channel candidates additionally
match the stored full outbound hash and destination Link ID, and relayed
HEADER_2 proofs bypass local terminal reservation. Direct Transport ingest and
maintenance results are `must_use`; sink-aware NodeCore paths avoid duplicate
receipt events; and the
hosted daemon consumes LXMF terminal output. Its descendants add ordinary
Link-DATA receipt correlation, destination proof-policy parity, and
responder-Handshake reclamation. The direct-request sequence adds exact
canonical values at `338251b`, prepared-versus-confirmed dispatch ownership at
`354b875`, inbound encoded values and their original timestamp at `ba73ee4`,
and phase-agnostic exact request-dispatch reclaim keyed by request and Link IDs
with prior native-phase reporting at `dfcaa36`. The
recorded selected
validation set for the `90570ca` predecessor passed 635 tests:
271 transport (174 library plus 97 integration), 137 stack (136
library plus one integration), 143 LXMF library, and 84 daemon library. The
four library targets total 537 tests. This is not a full nested-workspace test
count.
Those predecessor host suites passed on macOS, and the affected no-default
transport and stack crates compile for `riscv32imac-unknown-none-elf`. This newer
lifecycle work remains on the user's fork. No issue or pull request was opened,
and publication elsewhere still requires the user's direct approval.

The project adapter and `crates/node-core` now exercise both halves. Node-core
stores fixed dispatch metadata and the attempt ledger while each 500-byte
`TxPacketBuffer` remains externally owned and registered once. It reserves
dispatch and attempt slots before preparation, writes directly into the
supplied buffer, rejects `deadline <= owner_now` before mutation, and resolves
the RNS target deterministically against an enabled-interface snapshot. The
unique routed `TxJob` preserves the full receipt hash and original target but
exposes no bytes. Opaque non-`Copy` request/reply types bind each serialized hop
to an exact opaque interface-resource ID and nonzero actor-defined units.
Node-core does not interpret the resource as radio, stream, or datagram state.
A policy must supply a matching sufficient reservation; issuance irrevocably
records possible transmission and burns that reservation, and
only the matching `AuthorizedTx::frame(now)` exposes bytes once before the
deadline. A delayed grant becomes byte-inaccessible `ExpiredAuthorizedTx`.

Completion serializes fan-out through the same buffer. An exact receipt is
cancelled only when every hop was definitely unsent; any prior authorization
keeps it live and forbids rollback. Exact-deadline authorization, completion,
rollback, and maintenance retain a scalar recovery record scoped by
`NodeInstanceId`. A coherent late exact owner finalizes as `Recovered`; faults
and same-lease invariants retain an owning quarantine rather than inventing or
reusing the buffer. The exact candidate sink still turns the matching active
attempt into an in-place terminal tombstone before Rete removes the receipt,
and acknowledgement remains blocked while any typestate owns the buffer. A
layout guard prevents dispatch slots from regaining embedded packet arrays.

The firmware-excluded `reticulum-tx-dispatch` crate now supplies the RF-inert
persistent packet-interface machine, node-side permit server, and fixed
per-slot DATA-owner machine. It retains exact owning/control values under
pressure, reconciles completions, withholds recovered owners until exact
acknowledgement, retries `Next` unchanged, synchronously prepares fresh DATA
from the lowest available parked owner, uses cancellation-safe short waits, and
fails closed rather than guessing authorization when a recovery step at or
after its configured grace threshold observes no exact permit reply. The
firmware-excluded `reticulum-tx-supervisor` crate now owns node-core and those
three TX machines in one permanent aggregate. Its async runner takes a fresh
checked clock sample before maintenance/DATA/permit/dispatcher, waits for the
exact earlier live-owner deadline or permit grace, yields after 16 productive
passes and every selected wake, and selects only phase-compatible cancellation-
safe waits. The aggregate now also forwards explicit proof policy, bounded
announce operations, registry-validated exact-owner RNS ingress and RNS tick,
and exposes its short wait publicly for a permanent event loop. No public
supervisor method accepts a caller-selected raw interface ID.
`RfInertTxPolicy` denies RF, and retained faults stop new preparation and policy
while owner-draining transitions continue where possible.

The allocation-free storage model and submission projector now define canonical
intent/final-disposition records and gate terminal/recovery acknowledgement on
an exact persistence result. The independent physical journal now supplies
lifetime reservation, exact readback, complete integrity-validated replay, and
retention-only compaction. Its isolated clean-path/software-reset powered HIL
has passed; controlled power cuts and product-runtime integration remain open.
The portable sole storage actor now connects that journal to its live index and
projector with exact autonomous ambiguity recovery plus narrow node-observation
and acknowledgement methods. It also owns exact durable conservative boot
finalization. The remaining product blockers are a permanent firmware task/
product flash adapter and orchestration of mount/replay/recovery boot gates;
device-API/runtime linkage; safe projector-slot retirement;
permanent timed RX/RNode hosting and complete ordinary-action draining;
firmware/driver integration; product-runtime powered reboot recovery;
caller-reservable bounded construction for those ordinary outbound actions;
and higher-level LXMF persistence.
Until those slices and radio policy are connected, no device-facing/device-API
send operation or product firmware RF TX graph uses this path. That describes
the current implementation, not a prohibition on development TX: the two
attached antenna-equipped boards are cleared for NA915 transmission whenever a
bounded integration test benefits. Separately named RF HILs and the derived
RNode peer remain development artifacts, not product firmware dependencies.

## 12. LXMF retry and receipt-attempt correlation

**Priority:** blocking reliable opportunistic LXMF delivery

At the reviewed upstream base, Rete's hosted LXMF outbound queue stores only
one `packet_hash` per message. `process_outbound()` retries an opportunistic
message every ten seconds and overwrites that hash, while the underlying DATA
receipt timeout is thirty seconds. A valid late proof for an earlier attempt is
therefore ignored even though it proves the LXMF message was delivered. At the
same time, several obsolete DATA receipts remain live and consume the bounded
receipt table.

At the upstream base, admission failures are also folded into the same attempt
counter as a packet that was actually released to an interface. The local
receipt-lifecycle candidate corrects receipt-table-full and truncated-hash-
collision handling, with a five-message/four-slot saturation regression. It
also retains all five in-memory attempt hashes, accepts a delayed proof for any
one, retires timeouts individually, and emits exactly one final failure after
the last outstanding attempt fails. Applications using router-managed outbound
state are now explicitly required to dispatch every proof and failure through
the mutable router API.

The production LXMF state machine still needs a durable attempt ledger:

1. Persist every released attempt's receipt token until the message reaches a
   terminal state; preserve the candidate's ability to accept a valid proof
   for any still-associated attempt across reboot.
2. Drive retry timing from typed release/delivery state rather than a periodic
   resend that can overlap the receipt timeout unintentionally.
3. Preserve typed local admission/backpressure as `not released`, without
   spending a delivery attempt or losing retry state.
4. On delivery, cancellation or final failure, reclaim/cancel all associated
   RNS receipts and persist the LXMF terminal transition atomically.
5. Test delayed/reordered proofs, repeated local backpressure, reboot between
   every transition and attempt counts larger than the receipt-table bound.

The pinned hosted LXMF router now cancels sibling Transport receipts
synchronously when its core-aware event path observes delivery. Its retained
legacy event method cannot do so because it has no mutable core and therefore
leaves siblings to bounded timeout. The firmware must use the core-aware
equivalent and persist its own message/attempt transition; generic receipt
cleanup does not make the hosted in-memory ledger reboot-safe. Durable queue
and receipt state belongs in the project-owned bounded LXMF router unless a
suitably generic upstream API emerges. No issue or pull request has been opened
for this newer work.

## 13. Identity capability separation and secret-safe diagnostics

**Priority:** key safety and misuse resistance

Rete's `Identity` stores both private and public key material and derives
`Debug` across the complete value. An otherwise routine debug log can therefore
emit the X25519 and Ed25519 private bytes. The permanent firmware keeps its
durable identity wrapper opaque and non-`Debug`, never logs the imported Rete
identity, and zeroizes the temporary private-key material after import.

`Identity::from_public_key()` also constructs the same private-capable type with
zero-filled private fields. The resulting value continues to expose
`private_key()`, `sign()` and `decrypt()` even though it has no private-key
capability. This makes invalid private operations representable and can return
plausible-looking zero key bytes instead of rejecting the operation at the type
boundary.

Prefer separate public/verification and private/full-identity types, or an
explicit checked capability state whose private operations return a typed
unavailable error. Implement a redacted manual `Debug` for every secret-bearing
identity type and retain zeroization on replacement and drop. Add tests proving
that public-only identities verify and address correctly but cannot export a
private key, sign, decrypt or initiate private-key agreement. This is a generic
Rete improvement; the product-owned flash schema and identity provisioning
policy stay in this project. No issue or pull request has been opened for this
item.

## Submission order

1. Exact direct/local LINKREQUEST validation: submitted as upstream draft PR
   7; retain until merged or superseded.
2. Transactional owned Link admission: submitted as upstream draft PR 9;
   retain until merged or superseded.
3. Endpoint announce rebroadcast policy: submitted as upstream draft PR 11;
   retain until merged or superseded.
4. Relay-table admission/visibility and remaining HEADER_2 dispatch.
5. Link event/timestamp semantics; retain the completed Channel-receipt patch.
6. Transactional DATA receipt admission, terminal status and reclamation.
7. LXMF retry/receipt-attempt correlation.
8. Explicit ingress dispositions and full capacity snapshots.
9. Bounded output/Resource seams.
10. LoRa receive API improvements, independently if easier to review.
11. Clock-domain/request and restart-safe snapshot semantics.
12. Interface cancellation, capability and backpressure seams as individually
    reviewable changes.
13. Identity capability separation and redacted secret diagnostics.

Do not combine these into one project-specific fork commit. Keep every fix
small enough to review upstream, retain a regression here against the pinned
revision, and move all Rete workspace crates together when adopting an updated
commit.
