# Rete upstream hardening backlog

**Status:** contribution sequence active; the first three focused issues and
draft pull requests are open upstream

This is the contribution queue discovered while integrating Rete revision
`9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743`. Each item should be submitted as
a focused test-first change. Tracker pins, FEM sequencing, regional policy,
product quotas and the device API stay in this project; generic protocol and
bounded-state corrections belong upstream.

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

## Submission order

1. Exact direct/local LINKREQUEST validation: submitted as upstream draft PR
   7; retain until merged or superseded.
2. Transactional owned Link admission: submitted as upstream draft PR 9;
   retain until merged or superseded.
3. Endpoint announce rebroadcast policy: submitted as upstream draft PR 11;
   retain until merged or superseded.
4. Relay-table admission/visibility and HEADER_2 dispatch.
5. Link event/timestamp semantics and channel receipts.
6. Explicit ingress dispositions and full capacity snapshots.
7. Bounded output/Resource seams.
8. LoRa receive API improvements, independently if easier to review.

Do not combine these into one project-specific fork commit. Keep every fix
small enough to review upstream, retain a regression here against the pinned
revision, and move all Rete workspace crates together when adopting an updated
commit.
