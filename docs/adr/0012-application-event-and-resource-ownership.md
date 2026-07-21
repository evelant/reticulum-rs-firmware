# ADR 0012: Application-event ownership and bounded RNS Resource admission

- **Status:** accepted and implemented for the first fixed-owner seam and LXMF
  parity corpus; RNS Resource ingress remains disabled
- **Date:** 2026-07-20
- **Decision owners:** project maintainers
- **Extends:** [ADR 0002](0002-rete-provisional-foundation.md),
  [ADR 0003](0003-lora-first-interface-fabric.md), and
  [ADR 0011](0011-durable-rns-inbox-qualification.md)

## Context

The pinned Rete node emits one `NodeEvent` stream for destination DATA,
announces, delivery receipts, Links, Channels, requests/responses, and RNS
Resources. The current integration preserves every event through Rete,
`node-core`, the ordinary-action owner, and `NodeInterfaceSupervisor`. The
permanent E290 task is the first destructive boundary: it converts
`DataReceived` into ADR 0011's one-entry raw-RNS qualification record and only
counts, then drops, every other event.

That behavior cannot support a standalone LXMF node. Opportunistic LXMF uses
destination DATA, direct delivery uses Link DATA or an RNS Resource, propagation
uses announces and requests, and delivery state uses proofs, receipt failures,
and Link lifecycle events. It would also couple future USB, BLE, and Wi-Fi
clients to whichever physical interface happened to receive a packet instead
of to the single device-owned Reticulum node.

Native Rete RNS Resource processing is not yet safe to enable merely by
selecting `AcceptNone` or `AcceptApp`. An advertisement constructs its
allocation-backed Resource before the configured strategy is consulted. One
offer can declare up to 1,048,575 parts and cause part, hash, and received-bit
vectors to be allocated. Completion can simultaneously retain ciphertext,
decryption output, decompressed or assembled plaintext, internal Resource
state, and event data. Request/response Resources bypass the ordinary
application strategy, output generation has a silent 256-packet ceiling, and
accept/reject can mutate or remove state even when no rejection packet was
successfully built. PSRAM increases the useful full-product budget but does
not make network-controlled, unchecked allocation or silent output loss a
valid policy.

The existing `TxPermitResourceId` vocabulary is unrelated to an RNS Resource;
it names an actor-defined authorization-capacity domain. Keeping both meanings
would make the application and transfer APIs ambiguous.

## Decision

### Own one transport-neutral application event vocabulary

`rns-rete` exhaustively consumes every pinned native `NodeEvent` into a
project-owned, non-`Clone` `ApplicationEvent`. Payload allocations move into
the new value exactly once; they are not cloned or exposed to physical
transport code. Payload-bearing `Debug` implementations reveal lengths and
protocol identifiers, not plaintext bytes.

The projection has no wildcard arm. A new upstream Rete variant must therefore
break compilation until the project explicitly chooses its ownership,
durability, and loss policy. Native `NodeEvent` is an adapter implementation
detail rather than the public event type carried by `NodeActions`.

The vocabulary preserves distinct correlation domains rather than flattening
them into a generic message identifier:

- application submission ID;
- per-attempt ID;
- 32-byte LXMF message ID;
- 32-byte RNS packet/receipt hash;
- 16-byte Link, request, path, destination, identity, and Resource hashes as
  their respective typed fields; and
- an owner-local application-event generation.

No event contains LoRa, SX1262, USB, BLE, Wi-Fi, or interface-slot identity.
Application consumers route by destination, Link, request, or Resource
ownership. Physical interface selection remains solely in the Reticulum
router and interface fabric.

### Put fixed outer ownership above the Rete adapter

`node-core` owns a caller-provided fixed array of application-event slots. An
atomic batch offer either admits the complete event portion of one
`NodeActions` envelope or returns that exact envelope unchanged. The outer
owner does not allocate. In this first migration, bounded packet-sized events
may still contain the allocation that Rete already created; admission moves
that allocation and adds no copy. RNS Resource bodies remain gated and will
eventually be represented by bounded blob handles, not by an assembled body in
an event slot.

Consumers receive FIFO, generation-checked leases. A lease can be completed
only by an explicit disposition such as committed/acknowledged, policy discard,
or quarantine. Dropping an unresolved lease retains and quarantines its exact
event instead of silently making the slot reusable. Stale or duplicate
dispositions fail without touching a newer slot generation. Diagnostics count
explicit dispositions and pressure; counters do not substitute for retained
ownership.

A consumer that intends to retry after backoff uses `quarantine_for_retry`.
That disposition returns an opaque, non-`Clone` structural token containing a
private backing-slot address plus the event generation, FIFO sequence, and
quarantine reason. `ApplicationEventId` remains diagnostic metadata and is not
accepted as retry authority. Exact reacquisition first checks that the backing
slot occurs at the same stable index in the presented owner, then checks every
captured incarnation field. A foreign owner with an equal scalar ID, a reused
slot generation, or a changed quarantine fails without mutating any event slot
and returns the unchanged token; only rejection diagnostics advance. The
address is never dereferenced, and a storage lifetime prevents address reuse
while a token remains live. It is represented privately as `usize` so a
static-storage token remains `Send` when carried across an Embassy task await;
the public type exposes no address or constructor.

The ordinary FIFO quarantine path remains an explicit manual-recovery escape
hatch. It can inspect the same unchanged event and therefore can produce more
than one retry token for that one incarnation. Such tokens serialize through
the owner's exclusive lease, authorize only that exact backing-slot
generation/sequence/reason, and all become stale once the slot is resolved and
reused. They are not globally unique job identifiers.

`NodeInterfaceSupervisor` drains complete non-packet action envelopes into a
passed application-event owner only when the whole batch fits. Capacity
pressure leaves the exact envelope at the existing ordinary boundary. Packet
dispatch, completion reconciliation, actor lifecycle, and ingress-buffer
recycling remain independently progressable. The public firmware surface no
longer destructively takes an untyped event vector.

ADR 0011's raw-RNS inbox becomes the first explicit consumer. It acknowledges
DATA only after it has either retained the candidate for its commit path or
recorded an explicit typed rejection under its qualification policy. Protocol
maintenance events may be deliberately coalesced or discarded only by a named
product policy. Events needed by a future LXMF engine remain distinguishable
and cannot fall through an `Other(_)` arm.

### Keep native RNS Resources fail-closed

The existing pre-ingest `ResourceIngressDisabled` rejection remains active in
every product profile. Defining application event variants for offers,
progress, completion, failure, and rejection does not claim that the current
Rete Resource implementation is enabled or bounded.

Before removing that gate, the pinned Rete fork must provide a pre-allocation
admission contract covering at least:

- concurrent inbound and outbound Resources;
- advertised transfer bytes and part count;
- split count and aggregate split bytes;
- assembled and decompressed byte ceilings;
- maximum transient copies or an equivalent scratch reservation;
- deadline and retry limits;
- request/response Resources under the same limits;
- fallible event/output sinks; and
- an output cursor/window bounded by available ordinary packet owners instead
  of a silently truncated `Vec` burst.

An accepted offer will carry a node-incarnation/generation-bound decision
token. `Accept` or `Reject` returns an owned protocol action through the
ordinary router; application code never sends a radio packet directly. An
accepted body streams into a bounded durable blob store and completion emits a
stable object handle. Reclamation, interrupted-transfer recovery, and a
failed-to-build reject packet must all have explicit states.

### Establish LXMF compatibility before selecting a runtime

The first LXMF tranche is a deterministic Python-LXMF-derived wire corpus, not
an embedded client. It covers binary title/content, heterogeneous known and
unknown MessagePack fields within ADR 0013's typed first-tranche exclusions,
exact hashes/signatures, 32-byte proof-of-work stamps, 16-byte tickets,
opportunistic/direct boundaries, and direct packet/Resource boundaries. Each
fixture records its generator, pinned Python/RNS/LXMF versions, source revision,
and exact bytes.

The corpus treats released Python LXMF as the compatibility authority.
Precursor's Python-generated vectors and the `no_std + alloc` LXMF-rs wire
model are useful references. Rete's current LXMF codec is a negative oracle for
known two-byte stamp/ticket and structured-field incompatibilities; its hosted
router is not linked into firmware. The eventual firmware LXMF state machine
owns durable submissions, attempts, sibling-receipt cancellation, message
state, and propagation policy behind narrow node operations rather than a raw
mutable `NodeCore` escape hatch.

### Remove the permit naming collision before Resource APIs expand

The authorization vocabulary will migrate from `TxPermitResourceId` to
`TxPermitDomainId`, from `resource()` to `domain()`, and from
`ResourceUnavailable` to `PermitDomainUnavailable`. In this repository,
unqualified “Resource” then consistently means the Reticulum transfer
primitive. This mechanical rename may land separately, but no new RNS Resource
API may adopt the ambiguous old name.

## Consequences

- LoRa remains the first qualified interface, but application processing no
  longer depends on LoRa or the HT-RA62/SX1262 driver.
- A full E290/PSRAM profile may allocate larger application and storage
  budgets than a reduced no-PSRAM profile. The reduced profile may disable
  LXMF, NomadNet, propagation, or Resources without constraining the full
  product design.
- The first owner stops implicit event destruction but does not by itself make
  Rete's internal event allocation atomic. The final pre-mutation reservation
  hook remains a required Rete hardening step.
- ADR 0011 remains a deliberately disposable qualification format. Client
  bearers will consume a durable semantic message store, not native events or
  the one-entry raw record.
- No upstream issue or pull request is authorized by this decision. A local
  Rete repair may be prepared and tested, but publishing it requires the
  user's direct approval.

## Acceptance evidence

The first implementation must prove:

1. exhaustive projection of every pinned native event, including pointer-
   stable move tests for owned payload allocations;
2. all-or-nothing mixed event-batch admission and exact return under pressure;
3. FIFO generation reuse, unresolved-lease quarantine, foreign equal-ID and
   stale structural-retry rejection, and safe serialization of same-event
   retry tokens;
4. survival of supervisor busy/retry and tick-produced events;
5. packet/completion/lifecycle progress while application output is pressured;
6. explicit E290 dispositions for DATA and every non-DATA event class, with no
   destructive `Other(_)` path;
7. a reproducibly generated Python LXMF corpus plus negative mutations; and
8. unchanged Resource-ingress rejection until every pre-allocation and
   streaming criterion above is independently qualified.
