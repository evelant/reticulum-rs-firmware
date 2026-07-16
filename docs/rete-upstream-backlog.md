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
`f6f5fb0637d00691e09fa0105be4df902405fee4` (`firmware-pin-f6f5fb0`). It adds
caller-owned DATA preparation, explicit receipt-capacity errors, fixed-capacity
terminal sinks, exact DATA/channel terminal candidates, allocation-atomic
proof/timeout delivery, full-hash receipt cancellation and core-aware LXMF
sibling-attempt cleanup. The legacy LXMF
event handler without mutable core access still leaves siblings to timeout.
This candidate remains on the project fork; it has not been submitted upstream.

## 1. Transactional Link admission

**Priority:** blocking relayed Links and production Link acceptance

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

Relay admission remains open. Both HEADER_2 and fall-through relay paths ignore
failure to insert the relay `link_table` entry, and HEADER_2 relay state is
inserted before route availability is known. The public API must distinguish
`Existing`, `Inserted`, and `Full` before forwarding and expose relay
count/lookup in a read-only capacity snapshot.

Remaining regression matrix:

1. Fill a relay table and verify a new relayed request is rejected explicitly.
2. Verify an existing relay Link ID is not misclassified as new capacity.
3. Verify a missing route cannot leave orphan relay state.

The local `EmbeddedNode` retains product-level owned-Link quota preflight and
rejects relayed LINKREQUESTs until the remaining seam exists.

## 2. LINKREQUEST validation, HEADER_2 dispatch and reverse admission

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

Remaining local admission does not consult registered-destination direction or
`accepts_links` policy. Locally terminating HEADER_2 requests also do not yet
reach the direct/local validator consistently. Relay forwarding is a distinct
operation: it should decide transport-ID ownership and capacity atomically,
without treating a relay as the final endpoint responsible for destination
shape policy.

A HEADER_2 request addressed to the local transport is treated as relayed even
when its final destination is local. Conversely, a HEADER_2 request addressed
to another transport can fall through into the nominal HEADER_1 path.

The same dispatch ordering affects other packet types. Non-announce HEADER_2
packets naming a different transport can fall through native local/forwarding
logic. A packet naming this transport enters the generic relay branch before
local DATA, owned-Link DATA, proof/receipt or announce dispatch, so locally
terminating traffic is dropped or misrouted. Endpoint-mode unmatched proofs
also fall through as forwarding actions even though transport is disabled.

Both targeted HEADER_2 relay and ordinary HEADER_1 DATA relay insert a reverse
entry while ignoring a full bounded-map result. The packet is still forwarded,
but its proof can no longer return. Expose lookup/capacity admission as one
transactional forwarding operation and return a typed `Full` outcome before
emitting the packet.

Add released-Python differential fixtures for valid 64/67-byte direct/local
requests and negative destination-policy and transport-ID boundaries. Add
H1/H2 reverse-table exhaustion tests, locally terminating H2 request/DATA/proof
cases and an endpoint unmatched-proof case. Dispatch should decide HEADER_2
ownership before any local or relay allocation, and locally terminating
requests should then pass through the same canonical-shape validator.

The local adapter mirrors Python's non-announce transport-ID filter, rejects
native H2 classes that cannot yet be dispatched safely, suppresses endpoint
forwarding actions, and preflights reverse capacity on the two admitted DATA
relay paths.

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

## 4. Link state events and outbound activity

**Priority:** application correctness and keepalive behavior

The responder currently emits `LinkEstablished` immediately after receiving a
LINKREQUEST, while its state is still `Handshake` and data sending returns
`LinkNotActive`. It emits a second `LinkEstablished` after LRRTT activates the
Link. Prefer a distinct pending/request event, or reserve `LinkEstablished`
for the transition to `Active`.

Generic `build_link_data_packet()` users also do not update `last_outbound`.
Best-effort Link data, identify, request and response traffic can therefore
trigger unnecessary keepalives. Move outbound timestamp maintenance into the
common successful packet builder or require `now` in the relevant APIs.

`Transport::tick()` also never expires `Pending` or `Handshake` Links because
`Link::check_stale()` handles only `Active` and `Stale`. An initiator that never
receives LRPROOF, or a responder that never receives LRRTT, can therefore hold
a bounded owned-Link slot indefinitely. Add explicit establishment deadlines,
timeout results/counters and release-then-fresh-retry coverage.

The local adapter suppresses premature establishment events and records the
timestamp after best-effort Link data, but the native behavior needs its own
tests and repair.

## 5. Transactional channel receipts

**Priority:** reliable delivery

The adaptive channel window can grow beyond the heapless channel-receipt table
capacity `L`. `send_channel_message()` mutates channel state and builds a
packet before ignoring receipt insertion failure. A valid proof for such a
packet is then unmatchable and the envelope remains pending.

Channel retransmission rebuilds the encrypted packet with fresh randomness but
does not replace/register the corresponding full-hash receipt. A proof for the
retransmission is therefore also unmatchable even when the original insertion
succeeded. Receipt replacement must be transactional with retransmit state and
must retain any still-valid sibling hashes until one proof wins or the attempt
set fails.

It also queues the envelope and advances the channel sequence before packet
construction. A payload larger than the negotiated Link MDU can therefore
return an error while leaving an unsent message pending. Validate the per-Link
payload limit before channel mutation, not only the fixed receipt capacity.

Either size receipt storage independently for the configured channel window,
or preflight/rollback the complete send transaction. A full receipt table must
return a typed error before sequence/window state changes. Test the boundary,
proof removal, reuse after proof, retransmission and wraparound.

## 6. Explicit ingress dispositions

**Priority:** diagnostics and recoverable failure

`NodeCore::handle_ingest()` maps native `LinkTableFull`, `Invalid` and
`Duplicate` to the same empty outcome, and several decrypt/unknown-state
failures are also empty without incrementing a unique counter. Add a
disposition or typed drop reason alongside events/actions. It should cover
parse, dedup, crypto, unknown destination/Link, capacity, policy, route and
output-backpressure failures.

The local adapter can recover `Invalid` and `Duplicate` only when transport
counter deltas identify them; everything else remains `NoObservableOutcome`.
The native `packets_sent` statistic is also incomplete at the pinned revision:
it counts announce-queue output but not ordinary DATA, Link/channel, proof,
close or keepalive packets. Define whether counters represent packet creation,
interface enqueue or completed transmission, then count that boundary
consistently.

## 7. Bounded NodeCore outputs and Resources

**Priority:** full embedded profile

Heapless transport maps do not bound `NodeCore` events, output packets,
destinations, pending requests, resource lists, split queues or assembled
Resource data. Ingress also accepts hosted packets up to 300 KiB and allocates
for packets over the base MTU.

Introduce an embedded profile with caller-owned/fallible event and packet
sinks, explicit destination/request/resource quotas, and transactional output
backpressure. Resource receive must cap concurrent transfers, bytes, parts,
decompressed output and transient copies; the long-term device path should be
able to stream through flash-backed storage instead of assembling every
representation in RAM.

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

`NodeCore::send_request()` supplies one `now` value both as the serialized
request timestamp, which is wall-clock Unix time in released Python, and as the
monotonic timeout-registration time. One integer cannot correctly represent
both clocks on an offline device. Split the API into explicit wall and
monotonic time types and test boot-without-wall-time behavior rather than
silently emitting uptime as a Unix timestamp.

Transport snapshots likewise persist `learned_at` and `last_accessed` values
without recording the monotonic epoch or wall-time quality. Restoring raw
pre-reboot ticks into a fresh monotonic epoch can extend or prematurely expire
paths. Add a versioned snapshot-age representation and explicit rebase/drop
policy. `SnapshotDetail::Full` should not claim broader recovery while it is
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

The generic fix is now the project pin
`f6f5fb0637d00691e09fa0105be4df902405fee4`, reachable at fork tag
`firmware-pin-f6f5fb0`. It returns caller-owned packet metadata plus a full
receipt token, reports registration and output-allocation failures, and
atomically removes validated and timed-out receipts only after reserving their
exact kind/full-hash terminal candidate. Link-typed channel proofs are resolved
before ordinary DATA receipts so reservation and mutation agree even if a DATA
receipt's truncated key equals the Link ID. Channel candidates additionally
match the stored full outbound hash and destination Link ID, and relayed
HEADER_2 proofs bypass local terminal reservation. Direct Transport ingest and
maintenance results are `must_use`; sink-aware NodeCore paths avoid duplicate
receipt events; and the
hosted daemon consumes LXMF terminal output. Focused transport, stack, LXMF and
daemon library suites pass, all workspace targets check on the macOS host, and
the affected no-default crates compile for `thumbv6m-none-eabi`. This newer
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
to an exact permit; issuance irrevocably records possible transmission, and
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
safe waits.
`RfInertTxPolicy` denies RF, and retained faults stop new preparation and policy
while owner-draining transitions continue where possible.

The allocation-free storage model and submission projector now define canonical
intent/final-disposition records and gate terminal/recovery acknowledgement on
an exact persistence result. The independent physical journal now supplies
lifetime reservation, exact readback, complete integrity-validated replay, and
retention-only compaction. The remaining product blockers are a sole permanent
actor connecting that journal to projector plans; a device-API adapter; safe
projector-slot retirement;
eventual sole-owner integration of RX plus ordinary RNS tick/actions;
firmware/driver integration; powered reboot recovery; caller-reservable bounded
construction for those ordinary outbound actions; and higher-level LXMF
persistence.
Until those slices and radio policy are connected, no device-facing/device-API
host send operation or firmware RF TX graph uses this path. Every project
firmware graph remains TX-free, and all project radio-bearing firmware
artifacts remain RX-only. The separately derived RNode peer remains an external
guarded development artifact, not a project firmware dependency.

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

## Submission order

1. Exact direct/local LINKREQUEST validation: submitted as upstream draft PR
   7; retain until merged or superseded.
2. Transactional owned Link admission: submitted as upstream draft PR 9;
   retain until merged or superseded.
3. Endpoint announce rebroadcast policy: submitted as upstream draft PR 11;
   retain until merged or superseded.
4. Relay-table admission/visibility and HEADER_2 dispatch.
5. Link event/timestamp semantics and channel receipts.
6. Transactional DATA receipt admission, terminal status and reclamation.
7. LXMF retry/receipt-attempt correlation.
8. Explicit ingress dispositions and full capacity snapshots.
9. Bounded output/Resource seams.
10. LoRa receive API improvements, independently if easier to review.
11. Clock-domain/request and restart-safe snapshot semantics.
12. Interface cancellation, capability and backpressure seams as individually
    reviewable changes.

Do not combine these into one project-specific fork commit. Keep every fix
small enough to review upstream, retain a regression here against the pinned
revision, and move all Rete workspace crates together when adopting an updated
commit.
