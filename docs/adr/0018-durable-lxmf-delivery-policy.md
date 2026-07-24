# ADR 0018: Durable LXMF delivery policy and direct-Link support

- **Status:** accepted for the appliance alpha; automatic opportunistic
  delivery and the bounded fresh outbound-initiator one-packet direct-Link
  success path are powered-qualified
- **Date:** 2026-07-23
- **Revised:** 2026-07-24
- **Extends:** [ADR 0013](0013-bounded-lxmf-wire-boundary.md),
  [ADR 0014](0014-durable-lxmf-message-ownership.md),
  [ADR 0016](0016-bound-link-data-lxmf-ingress.md), and
  [ADR 0017](0017-reticulum-peer-discovery-and-proximity-bootstrap.md)
- **Supersedes in part:** [ADR 0008](0008-durable-authorization-provenance.md)
  for the journal schema, physical format, record-body ceiling, and geometry;
  ADR 0008's authorization-provenance decision remains in force

## Context

LXMF distinguishes delivery method from Reticulum routing. An opportunistic
LXMF message is one destination DATA packet and may still traverse a learned
Reticulum route and obtain an end-to-end packet proof. A direct LXMF message
uses an authenticated Reticulum Link, carrying either one Link DATA packet or
an RNS Resource. Propagated delivery is the separate store-and-forward mode for
peers that need not be reachable at the same time.

Python LXMF 1.0.1 chooses `DIRECT` when a caller supplies no desired method. An
explicitly requested `OPPORTUNISTIC` message remains opportunistic when it fits
the encrypted single-packet limit and falls back to direct delivery when it
does not. Its router requests and rediscovers paths while retrying
opportunistic messages; it does not universally replace a failed opportunistic
message with a Link.

That reference-implementation default does not require this LoRa-first
appliance to establish a new Link for every short chat message. For an isolated
packet, opportunistic delivery avoids Link-establishment airtime and latency.
A Link becomes the better mechanism when one is already active, multiple
exchanges can amortize its setup, the message does not fit opportunistic DATA,
an explicit policy requests session semantics, or Resource transfer is
required.

The accepted signed LXMF message must remain identical whichever delivery
method is selected. Choosing `DirectLxmf` as the durable intent name would bind
storage ownership to a later transport decision and make safe retry or
escalation unnecessarily difficult.

The `90570ca` Rete predecessor generated a receipt for destination DATA and
Channel traffic but not ordinary Link DATA. Its `2d07818` descendant
implements the distinct ordinary Link-DATA receipt and respects each receiving
destination's proof policy. Its `a443173` descendant additionally
reclaims responder `Handshake` state on Reticulum's
`360 + 6 * max(1, post-ingress hops)` second establishment timeout. Generic
native initiator expiry remains absent. The product wrapper closes that half
for its outbound initiator transaction with a transport-neutral timeout and
exact abort operation rather than coupling the lifecycle to LoRa or to the E290
radio actor. The current `354b875` descendant adds canonical request values and
prepared-versus-confirmed request dispatch ownership; those request primitives
do not change this LXMF delivery-policy decision or imply full NomadNet
support.

## Decision

### Persist the exact message independently of delivery method

Use a closed `SubmissionIntent` vocabulary:

- `ExperimentalRnsData` retains the existing destination and maximum 383-byte
  plaintext contract unchanged.
- `LxmfMessage` retains the exact complete signed LXMF wire, including its
  16-byte destination prefix, up to the current 431-byte inline boundary.

`LxmfMessage` does not mean opportunistic, direct, propagated, LoRa, or any
specific Reticulum interface. Bytes `16..` are the compatible opportunistic
carrier; the complete wire is used for direct Link DATA or Resource transfer.
No delivery path may recompose, resign, truncate, or otherwise reinterpret the
accepted bytes. Retries and any automatic escalation therefore retain the same
LXMF message identifier.

The current device API has one method-neutral `experimental.lxmf.basic_send`
operation. It durably stores the method-neutral message for the appliance's
implicit `Auto` policy. A later explicit preference surface may add
`Opportunistic`, `Direct`, or `Propagated`, but that preference must itself
become durable before the API acknowledges it. It must not be inferred from
transient UI or connection state after a reboot.

Principal-scoped idempotency includes the intent variant and all exact semantic
bytes. A retry of the same basic-send request therefore resolves to the same
submission and LXMF message identifier, while the same idempotency key with
different bytes or a different intent remains a conflict.

The journal moves to semantic schema 3 and physical format 2:

- canonical record body: 544 bytes;
- physical slot: 672 bytes;
- unchanged bank size: `0x7f000` bytes;
- 774 complete slots per bank, with a 64-byte unused tail; and
- 154 submissions at the existing five-record worst case.

This remains above the E290 product profile of 128 resident submissions and
does not change the 1 MiB `node_journal` partition. Schema-2/physical-1 media
fails read-only. Developer migration erases and reprovisions only
`0x630000..0x730000`; firmware must not silently erase incompatible history.
The native development app opens a fresh
`reticulum-lxmf-chat-alpha-schema3.sqlite3` database with this migration so
stale local submission identifiers cannot poll or collide with a newly
provisioned node. Its separate appliance credential file is retained.

### Select delivery with an appliance-owned automatic policy

For the current source-free basic-send operation, `Auto` applies in this order:

1. reuse a compatible active Link or backchannel when one already exists and
   has no active or unacknowledged terminal DATA attempt;
2. otherwise send an eligible one-packet message opportunistically, including
   while a matching Link is busy;
3. select or establish a direct Link when the message cannot use the available
   opportunistic packet form, when a future bounded retry policy escalates it,
   or when an explicit future preference requires direct delivery;
4. use one Link DATA packet when the complete wire fits the Link MDU;
5. use an RNS Resource over the Link for larger messages once Resource-backed
   durable storage and recovery are implemented; and
6. use a configured propagation node only for an explicit propagated policy.

Path discovery is shared protocol machinery, not a delivery method. Both
opportunistic DATA and Link establishment use Rete's retained path and
interface selection. No step chooses LoRa directly, and a future Wi-Fi, BLE,
USB, Ethernet, or other Reticulum interface participates through the same
route and owner contracts.

The exact thresholds are capabilities rather than permanent product limits.
The current inline intent retains at most 431 wire bytes. Its destination-
stripped carrier can use the dedicated 391-byte Header-1 opportunistic path;
a routed Header-2 packet may impose a smaller MDU and cause `Auto` to select a
Link instead. Messages above 431 bytes remain future Resource work and must
never be truncated.

The current source implements the first bounded subset of this policy. It
reuses a compatible active product-initiated outbound Link from a registry
sized to the native product Link table (four entries on E290) before
considering opportunistic delivery, but only when that exact Link has no active
or unacknowledged terminal DATA attempt. Closed or unknown handles are pruned
during lookup and capacity checks. A `Stale` Link is retained because
authenticated traffic may revive it, but it is not selectable and continues
to occupy its registry slot. Without a ready compatible entry, the policy
retains the existing opportunistic-first behavior when the carrier fits. An
oversize carrier, or a smaller carrier that exceeds the selected routed packet
MDU, takes the direct path. If a destination-matching Link is busy, such
direct-required work remains durably `Preparing` under typed backpressure; the
runtime neither falls back to an incompatible carrier nor creates a second
Link to that destination. A new Link is not created until the destination has
an authenticated identity and usable retained path; the same tagged
path-discovery owner, dispatch acknowledgement, wait, and retry schedule
therefore gates Link establishment. A full registry is likewise bounded
backpressure: the exact message remains durably `Preparing` for a later retry,
not terminal failure. Responder/backchannel Link discovery and reuse are not
part of this first subset.

### Implement direct Link delivery as a reusable capability

When `Auto` or an explicit future preference selects a direct Link, use the
following transport-neutral lifecycle:

1. durably move `Queued -> Preparing`;
2. revalidate the remote identity and usable native path;
3. when absent, emit a tagged path request under the existing bounded
   discovery schedule;
4. reuse a compatible idle active outbound Link or initiate one; when a
   matching active Link still owns an active or unacknowledged terminal
   attempt, retain the exact submission under typed backpressure instead of
   initiating a second same-destination Link; attach a newly initiated Link's
   opaque handle to the exact generation-tagged offer, and retain the
   LINKREQUEST through ordinary-router pressure;
5. start one product-owned establishment deadline only after the ordinary
   router confirms the exact LINKREQUEST's first real interface dispatch; the
   offer snapshots a 30-second minimum, Reticulum's six-second first-hop and
   per-retained-hop allowances, one full-MTU serialization interval from the
   authoritative eligible-interface bitrate, and a two-second queue-to-radio
   guard, then abort the exact pending Link if that deadline expires;
6. after the authenticated Link becomes active, prepare ordinary context-None
   Link DATA from the exact durable wire bytes and register a Link-DATA
   receipt before exposing packet bytes;
7. route the packet only to the interface bound by the retained Link;
8. durably project authorized-frame evidence before acknowledging interface
   completion; and
9. map a valid Link proof to `Delivered`, or timeout, cancellation, Link close,
   or recovery to the existing durable failure vocabulary; a Link-DATA
   `DeliveryTimeout` also retires that exact reusable Link through the normal
   authenticated close path.

The Link request, establishment event, prepared Link packet, receipt, interface
handoff, and durable submission remain correlated by bounded generation-safe
owners. A failed pre-I/O handoff cancels the exact receipt. Once any interface
owns the packet, only its completion plus proof/timeout lifecycle can release
the attempt. Pending Link establishment also has an explicit abort path so
failed peers cannot consume the fixed Link table indefinitely.

A direct terminal retains the exact Link handle that carried its packet. If
that receipt times out while native state still appears `Active`, the runtime
evicts the matching reusable entry and firmware routes the authenticated
`LINKCLOSE` action through the ordinary owner. The timed-out submission remains
terminal `Failed(DeliveryTimeout)`; it is not automatically replayed. A later
direct submission may establish a fresh Link.

The first implementation serializes Link establishment to one active product
transaction and serializes direct DATA attempts per exact Link while retaining
a fixed registry of reusable product-initiated outbound Links. Link occupancy
starts with the active packet attempt and extends through an unacknowledged
terminal: only the exact durable terminal acknowledgement releases that Link
for another direct attempt. This product policy does not narrow the lower node
core's independent bounded-attempt capability. The registry capacity matches
the product's native Link table; it is four on E290. The runtime separately
checks native owned-Link admission because inbound responder Links share that
table. Pressure marks only the affected direct-required submission, allowing
eligible short opportunistic work and work on other usable cached Links to
continue. It does not yet map an authenticated responder-side Link to its
remote `lxmf.delivery` destination, so reverse or backchannel reuse remains
deferred. This is a concurrency and ownership bound, not a message-size,
transport, or hardware feature restriction. Later responder/backchannel reuse
or multiple in-flight establishments may widen that owner without changing
the durable message or device API.

The establishment transaction, reusable-Link registry, path-discovery counters,
deadline, retry history, and Resource-wait marker are boot-volatile. The exact
LXMF wire remains in the journal, but the current storage model does **not**
resume pre-I/O `Auto` work after reset: boot recovery conservatively finalizes
both `Preparing` and `AwaitingDelivery` as `InterruptedByReset`. A future schema
or durable state distinction must identify work that provably never exposed a
frame before path/Link selection can resume safely. No Resource bytes are
emitted until durable Resource ownership and recovery are implemented.

Within one uninterrupted boot, Link-establishment expiry or loss clears the
volatile transaction and the firmware waits one second before trying the
submission, which is still `Preparing`, again with a fresh generation. These
retries are currently unbounded: there is no persisted retry budget and no
boot-local attempt ceiling. A bounded failure/escalation policy remains future
work.

### Extend Rete with an explicit ordinary Link-DATA receipt

Rete retains destination DATA, Channel, and ordinary Link-DATA receipts as
distinct kinds. A Link-DATA receipt records:

- complete covered packet hash;
- retained Link identifier;
- responder Ed25519 public key;
- creation time and delivery timeout; and
- an opaque cancellation token.

The receiver emits the canonical explicit 96-byte
`covered_hash || signature` proof for ordinary context-None Link DATA. The
initiator accepts it only when packet type, Link destination, context, covered
hash, retained Link association, and peer signature all agree. Terminal-sink
capacity is reserved before receipt, deduplication, or Link state mutates, so
downstream pressure is retryable rather than lossy. Closing a Link and periodic
maintenance also respect that reservation rule.

The product wrapper additionally binds a prepared direct-Link scalar to the
exact caller-owned LXMF wire bytes before asking Rete to encrypt or register a
receipt. This prevents same-length substitution between durable acceptance and
Link packet preparation. Link DATA uses the same authorized-frame durability
barrier as destination DATA: once bytes are exposed to an interface, the
dispatcher retains its completion and exact receipt owner until the complete
packet observation is durable. The remote endpoint's valid Link proof, released
only after its inbox commit, is what permits durable `Delivered`.

## Acceptance

The automatic short-message path is qualified when two E290s running the same
image learn each other's `lxmf.delivery` identity and path, accept a
method-neutral app-authored message, deliver it opportunistically to the
remote durable LXMF inbox, validate the resulting packet proof, and retain the
message and terminal timeline across board and app restart. The July 24 powered
record qualifies bidirectional existing-contact delivery and app-process
relaunch; fresh contact creation and board restart remain open.

Direct-Link capability is separately qualified when a forced-direct test or a
message that cannot use opportunistic DATA:

1. establishes or reuses an authenticated Reticulum Link over the routed
   interface;
2. commits the exact message to the receiver's durable LXMF inbox;
3. releases the receiver's Link proof only after that durable commit;
4. validates the proof and durably projects `Delivered` on the sender; and
5. preserves the message and terminal timeline across both board and app
   restarts.

Fault tests cover path absence, opportunistic retry and escalation, Link
establishment timeout, Link-table and receipt-table pressure, output
substitution, interface handoff pressure, invalid proofs, dropped-proof
timeout, reboot before and after authorized frame ownership, and incompatible
journal media.

Current source/host qualification covers outbound-initiator active-Link reuse,
closed/unknown registry pruning, non-selectable `Stale` retention, full-registry
backpressure, path-gated establishment, first-dispatch deadline start and exact
pending-Link abort, complete-wire Link-DATA preparation, typed receipt
ownership, the authorized-frame durability barrier, and exact reusable-Link
retirement after a Link-DATA timeout. One integrated regression holds a second
direct-only submission while the first exact Link attempt is active and while
its terminal remains unacknowledged, proves that no second establishment is
created, then reuses the exact same `LinkHandle` after acknowledgement and
delivers both submissions. The timeout regression separately proves that a
waiting follower remains parked until the failed leader is durably finalized
and its Link is retired, after which the follower requests a fresh Link. The
complete fault/pressure matrix remains unqualified.

The [July 24 direct-Link powered record](../e290-direct-link-powered-proof.md)
separately closes the bounded fresh-Link success path. A 408-byte complete LXMF
wire produced a 392-byte destination-stripped carrier, one byte beyond the
391-byte Header-1 opportunistic ceiling but within the 431-byte Link MDU.
Starting with an empty boot-volatile Link registry, the sender therefore
established a new Link, the receiver durably committed the exact wire before
releasing its proof, and the sender projected `Delivered`. Normal resets of
both boards plus a cold app-process relaunch retained the byte-identical
receiver wire and exact terminal sender row.

The
[current-image stale-Link recovery record](../e290-stale-link-recovery-powered-proof.md)
separately starts with a successful direct baseline, reboots only the receiver,
observes a durable sender `DeliveryTimeout` with no receiver commit, and then
delivers the next sequential submission over a fresh Link. This qualifies the
narrow timeout-retirement consequence, not automatic retry of the failed
submission.

The
[same-Link reuse and replay record](../e290-same-link-reuse-replay-powered-proof.md)
starts sender A from a fresh boot and submits two distinct durable operations,
IDs `6` and `7`, with different idempotency keys but an identical direct-only
408-byte LXMF wire and message ID beginning `9692c4`. Both reach `Delivered`
with distinct hashes for their 483-byte Reticulum packets, while the receiver host
projection advances by exactly one row, from 11 rows/sequence 13 to 12
rows/sequence 14. The portable regressions prove exact-handle reuse and the
receiver's `AlreadyDurable` classification; the powered run physically
exercises that composed path but does not independently expose either the
opaque `LinkHandle` or replay kind through the client API.

## Consequences

- Short one-shot LoRa messages normally avoid Link-establishment overhead.
- A Link remains the stronger reusable session and large-message mechanism,
  without becoming a prerequisite for ordinary chat.
- The E290 alpha caches at most four active outbound Links. If all four remain
  active, a fifth direct-required destination stays durably `Preparing` under
  a repeating one-second backoff, while a short eligible message can still fall
  back to opportunistic DATA. Generic capacity-driven close or LRU eviction is
  future work; exact Link-DATA timeout retirement is implemented. Maintenance
  is not otherwise assumed to free a slot. A non-selectable `Stale` entry also
  retains its slot so it can revive, until it becomes `Closed` or disappears.
- Direct DATA is single-flight per exact Link from active attempt through
  durable terminal acknowledgement. A later direct-required submission for a
  busy matching Link stays durably `Preparing` under the firmware's bounded
  retry backoff, so timeout retirement has no younger same-Link sibling to
  invalidate. This does not block eligible opportunistic delivery or direct
  work on another usable Link.
- This appliance's `Auto` default intentionally differs from Python LXMF's
  implicit `DIRECT` default while remaining wire-compatible with both LXMF
  delivery methods.
- The accepted signed LXMF bytes and message identifier remain stable across
  reboot, retry, escalation, and OTA changes, but current reset recovery
  finalizes in-flight work as `InterruptedByReset` instead of resuming it.
- Generic RNS DATA remains a narrow independent operation.
- End-to-end `Delivered` requires the remote endpoint's proof after durable
  acceptance, whether the selected carrier is opportunistic DATA or Link DATA.
- The lifecycle can route over any retained Reticulum interface; LoRa is the
  first bounded powered-qualified direct-Link path.
- The alpha incurs one explicit development journal migration and a larger
  fixed resident intent/index footprint, which fits the E290 PSRAM profile.
- Messages above 319 bytes of Python LXMF `content_size` still require a
  separately implemented and qualified Resource path.
