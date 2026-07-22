# ADR 0014: Durable LXMF message ownership

- **Status:** accepted for the portable store/event-owner tranche and first
  permanent-E290 opportunistic receive composition; one bounded A-to-B LoRa
  durable-delivery chain is power-qualified, while broader qualification remains
  deferred
- **Date:** 2026-07-21
- **Powered evidence updated:** 2026-07-21
- **Decision owners:** project maintainers
- **Extends:** [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0011](0011-durable-rns-inbox-qualification.md),
  [ADR 0012](0012-application-event-and-resource-ownership.md), and
  [ADR 0013](0013-bounded-lxmf-wire-boundary.md)

## Context

The bounded LXMF wire and application-ingress tranches can authenticate an
owned opportunistic message while borrowing its exact application-event
payload. They deliberately cannot acknowledge that event or make its bytes
survive reset. ADR 0011's one-entry raw-RNS record proved the physical
durability path, but its 383-byte payload ceiling, single-item policy, and
qualification-only schema are not an LXMF mailbox.

The next owner transition must be honest about both facts. A semantic record
without committed bytes is not durable, while copying every possible Resource
body into a fixed RAM array would make a temporary board profile part of the
protocol. The same logical LXMF message can also arrive through different RNS
carriers or with a different valid stamp, so exact wire equality alone is not
the message identity.

## Decision

### Separate semantic identity, physical storage, and lease integration

The first tranche uses three portable, featureless components:

- `reticulum-lxmf-model` owns dependency-free scalar types and borrowed
  normalized-wire segments. It embeds no fixed-capacity message buffer.
- `reticulum-lxmf-store` owns an append-only NOR format, exact operation-scoped
  device/range binding, mount/replay, and durable receipts.
- `reticulum-lxmf-durable-ingress` joins a project application-event lease to
  validation and storage. It is the only component in this tranche allowed to
  acknowledge the lease, and only a durable receipt authorizes that action.

The store does not depend on Rete, `node-core`, a board, radio, executor,
client bearer, or ADR 0011's raw inbox. The durable-ingress adapter does not
own flash directly; it borrows the sole coordinator's exact store view for one
operation.

### Retain exact normalized LXMF wire as the authority

The durable content is the complete normalized LXMF wire:

```text
destination || source || signature || exact MessagePack payload
```

For opportunistic destination DATA, the destination omitted by the RNS carrier
is written as a 16-byte prefix followed by the untouched borrowed carrier
payload. The two slices stream directly to storage without first creating a
contiguous message allocation. Title and content remain binary, the complete
fields map remains exact MessagePack, the timestamp remains its IEEE-754 bits,
and signature, stamp, unknown fields, and accepted noncanonical four-item
encodings remain byte-for-byte recoverable.

The immutable record metadata contains the protocol message ID, destination,
source, exact timestamp bits and bounded layout lengths. Stamp-admission and
carrier facts are durable validation/arrival evidence, not replacements for
the wire blob. Future read state, conversation indexes, Micron rendering,
attachments, search data, and client-specific projections must be rebuildable
from the authoritative record.

### Distinguish replay, alternate stamps, and collisions

The 32-byte LXMF message ID is the primary logical key. Admission also carries
a domain-separated SHA-256 fingerprint of the exact authenticated material
`destination || source || payload_without_stamp`, using the same Python raw
four-item and canonical stamped-payload rules as validation.

When a message ID already exists:

- the same authenticated-material fingerprint and matching redundant
  authenticated projections are a replay, including a different valid stamp
  or another carrier, and do not create a second logical message;
- the same material fingerprint with contradictory destination, source,
  timestamp bits, or decoded authenticated lengths is a fail-closed metadata
  conflict rather than a replay; this catches persisted corruption or parser
  drift even though honest identical material cannot produce the mismatch;
- a different fingerprint is a fail-closed collision and never replaces the
  first record; and
- exact-wire digests remain physical integrity evidence but do not define
  logical equality.

The stable logical handle is assigned once to a logical message identity, not
derived from a flash offset, so later compaction may relocate bytes without
changing client references. This first tranche retains the first committed
wire observation; durable multi-arrival history is deferred.

### Use variable erase-block extents, not a packet-sized slot

Physical format 1 is an append-only sequence of variable-length records over
an exact partition whose length is a multiple of 4096 bytes. A record reserves
the number of 4096-byte extents needed for repeated 512-byte extent headers,
normalized wire, and a 256-byte final footer containing its protected digests
and terminal marker. Before any dynamic header bytes, the writer programs and
reads back a claim-only page overlay containing a high-entropy marker. It then
programs and reads back the complete self-describing header. This ordering lets
mount distinguish a sparse torn claim/header write from arbitrary programmed
media and resynchronize at every erase boundary. Commit streams and reads back
the content and footer after all repeated headers, then programs the terminal
marker last. Only a complete final decode can issue a durable receipt.

A mount scans the complete bound range. Committed records enter an opaque,
caller-backed, capacity-bounded RAM index slot slice. Recognized interrupted
records consume their reserved extents but are not visible. Unclaimed programmed media, duplicate
logical handles, conflicting logical records, invalid committed metadata,
programmed wire padding, digest mismatch, unsupported format, incompatible
geometry, or an undersized RAM index fails closed. Mount does not erase or
repair media.

The partition length is a product-layout choice, not a crate constant. The
E290 now assigns a separate 2 MiB `lxmf_store` range at
`0x930000..0xb30000`, preserving the existing 2 MiB raw-inbox qualification
range byte-for-byte. Format overhead leaves exactly 1,834,752 bytes for one
large normalized record and provides 512 one-extent slots for current
opportunistic messages. The released-Python maximum opportunistic carrier is
391 bytes, or 407 normalized bytes, and fits in one extent. There is no
imported 383-byte ceiling. Future Resource ingress may stream larger bodies
into the same kind of blob owner under an explicit board/product limit; it
must not require an equally large PSRAM allocation.

A format-1 record carries a `u16` extent count, so its one-record maximum is
capped at 65,535 extents even when the partition is larger. A prospective
1 MiB normalized record is representable, but no Resource product profile has
yet been selected or qualified.

Format 1 has no erase, deletion, tombstone, reclamation, compaction,
encryption-at-rest, or migration operation. Exhaustion is explicit. Adding
those operations requires a new reviewed state/ordering tranche and must
preserve stable logical handles and replay knowledge.

### Make ambiguous mutation ownership explicit

The store retains the fingerprint of one mutation whose backend result is
ambiguous. Until exact retry/reconciliation resolves it, every unrelated
mutation is rejected. A retry may present only the same logical candidate.
After reset, mount derives truth from media: incomplete extents remain retired,
while a fully committed record is visible exactly once.

This is the local-store application of ADR 0004's global rule. Target
composition must additionally serialize this owner with credential,
configuration, submission-journal, OTA, and future blob mutations through the
one physical flash coordinator.

### Acknowledge the application event only after durability

The durable-ingress operation consumes one `ApplicationEventLease` by value.
It borrows and validates the event, constructs borrowed normalized-wire
segments, commits them, ends every payload borrow, and acknowledges the lease
only after receiving `Committed` or `AlreadyDurable` from the store. Its
success result carries the application-event ID, stable message receipt, and an
optional ready delayed-proof ID.

Unrelated, deferred, rejected, capacity-limited, binding-failed,
backend-ambiguous, collision, and store-fault outcomes return the exact
unresolved lease. The caller must deliberately route it to another consumer,
retry it, discard it under a typed policy, or quarantine it. Dropping that
returned lease retains ADR 0012's existing fail-closed quarantine behavior.

Every call selects `Required` or `Optional` proof mode explicitly; the mode has
no default. Required mode rejects a proofless event after validation and full
candidate construction but before store I/O. Optional mode uses the ordinary
durable acknowledgement path for a proofless event. A retained proof in either
mode must form one structural delayed-proof transaction before store I/O. The
transaction combines the exact event lease with an already selected fixed-
capacity proof slot; scalar event IDs cannot authorize substitution.

Candidate evidence and durable metadata are copied only after complete wire,
signature, destination, and stamp validation. The candidate is fully
constructed before proof reservation. Because reservation consumes the event
lease, store work reacquires bytes through the combined transaction and a
private typed carrier rebind; this checks event kind, destination, payload
length, and candidate consistency without repeating signature or stamp work.
Rebind contradictions return `EventCarrierMismatch` rather than panicking.

On any store failure, `transaction.into_lease()` releases the still-empty proof
reservation and returns the exact proof-bearing event. Both `Committed` and
`AlreadyDurable` move the hidden proof infallibly into `Ready` while
acknowledging the event. Durable ingress does not drain or send that proof.

### Do not equate an RNS proof with durable LXMF delivery

Rete now supports `InboundProofPolicy::Retain`, and ADR 0012's application-event
owner privately binds the exact proof to its event. The durable-ingress
transaction implemented here controls when that proof becomes `Ready`. Host
tests using a real retained Rete DATA/proof pair and the released Python
`basic_binary` LXMF fixture prove new and replay commits, capacity-before-I/O,
and lost-terminal-write retry without duplicate ready proofs.

The permanent E290 node keeps its existing primary destination's immediate-proof
policy and disables local Links there. It validates and mounts the separate LXMF
partition into 512 caller-owned index slots explicitly allocated in PSRAM. The
sixteen delayed-proof slots and bounded retry/fault/proof-holder state are also
caller-owned, validated boot-lifetime PSRAM allocations; sixteen application-
event slots remain in internal static RAM. On mount success it registers the
derived `lxmf.delivery` destination with local Links disabled, admits signed
opportunistic DATA, selects per-destination `Retain`, and drains ready packet
proofs only through the ordinary transport-neutral supervisor after a new
commit or a fresh retransmission recognized as `AlreadyDurable`. Volatile proof
state is never reconstructed from the durable record after reboot.
Those event/proof counts are an E290 volatile-concurrency profile, not a protocol
or store ceiling.

Exactly one fresh A-to-B powered trial now proves that stronger property for one
bounded opportunistic message. A 206-byte carrier became an exact 307-byte RNS
packet with SHA-256
`060037041c91eb5999f89bf84845c19e65bf7fa680827cce9c51e8ecc5dbe0a6`
and reached `Delivered` on its first attempt. Receiver B advanced durable-new,
proof-ready, proof-released, and ordinary-handoff by one, with zero already-
durable and ordering-violation events. The retained proof tag
`0x3dc4588d3a205429` matched sender A's delivered tag, and B recorded exactly one
confirmed proof-TX delta. Receiver `RPTE` generated-proof count/tag stays zero
by design: this retained LXMF proof is intercepted before ordinary RNS ingress
metadata, so the valid correlation is `LXTE` release tag plus confirmed TX plus
the sender's delivered tag.

The exact 2 MiB receiver store readback has SHA-256
`c75ab2a01b3266fda1e07e0271c70bb29c06e32636d70d8a70d977b9e8b0e21e`
and contains exactly one record for message
`abdeec2e498f09c96a6fd56ec3558ca86c2598aaeacac81969b645de3b549dc3`.
Its full-wire digest
`1c1839991401e01e15e3a3146cd3177a4fb7e5dbd52008fd119beaf091d377ba`
matches the independent generator. Baseline and terminal checkpoints report
zero allocation failures, unexpected runtime errors, RX/CAD/TX watchdog
expiries, and correlation faults. This is narrow evidence for the exact
new-commit/proof chain and persistent continuous RX across the split packet; it
does not qualify reverse direction, replay/remount, ambiguous writes, pressure,
range, or soak. The existing raw-inbox evidence remains a separate slice and
must not be relabelled as LXMF evidence.

## Consequences

- LoRa is the first carrier without appearing in the model or store APIs.
  Future Wi-Fi, BLE, USB, Link, Resource, and propagation paths converge on the
  same logical record owner.
- Full-product and reduced profiles choose explicit wire, record, index, and
  total-storage limits. A reduced board may disable the LXMF store without
  narrowing the full-feature protocol design.
- The fixed, caller-backed RAM owners are profile-owned. The E290 allocates its
  full 512-entry index, delayed-proof slice, and retry/fault/proof-holder state
  through `ExternalMemory`, validates each initialized allocation inside the
  detected PSRAM mapping, and retains them for the boot. The single powered HIL
  run reports no failed allocation, but sustained/pressure high-water remains a
  separate qualification step; the portable APIs do not mandate PSRAM or turn a
  convenient small stack allocation into a product capacity ceiling.
- The first format trades space for simple power-loss isolation: one interrupted
  append retires extents until later compaction. That is bounded and observable,
  but not yet an endurance-ready mailbox.
- Exact bytes remain available to future LXMF, NomadNet, Micron, and API layers;
  lossy UTF-8 or JSON projections cannot become the durable authority.
- Resource reception, propagation, outgoing encode/send/retry, tickets and
  ratchets, read/delete/tombstone state, client APIs, and broader powered target
  qualification remain explicit later tranches. The first opportunistic target
  destination/ingress/proof composition and one bounded powered commit/proof
  chain are now present.
- A pre-pending clean fault can disable only LXMF admission. Once a mutation is
  pending, an ambiguous store fault retains its exact owner and blocks all other
  flash mutations until reset/remount; routing and nonmutating consumers may
  continue. This preserves sole-flash authority rather than pretending LXMF
  isolation can resolve uncertain media state.

## Acceptance evidence

The first portable implementation must prove:

1. dependency-free, allocation-free `reticulum-lxmf-model` compilation;
2. `no_std` store and durable-ingress compilation for generic RISC-V and
   ESP32-S3 targets with no board/radio/executor dependency;
3. exact commit/remount/readback for basic, rich-fields, stamped, and 391-byte
   released-Python messages;
4. multiple records, explicit range/index exhaustion, and no 383-byte limit;
5. replay without a second record, alternate-stamp replay, and fail-closed
   same-ID/different-authenticated-material collision;
6. wrong-binding rejection before I/O, terminal readback before receipt, and
   recognized power-cut/lost-success behavior at every write stage;
7. the exact application-event lease returned on every non-durable outcome and
   acknowledgement only after a durable receipt;
8. real retained-proof preclassification and capacity before store I/O, one
   ready proof for each newly committed event and each fresh retransmission
   recognized as `AlreadyDurable`, exactly one ready proof after a
   lost-terminal-write retry, and a fresh proof for a fresh post-reset
   retransmission after remount replay;
9. manifest and resolved-closure policies excluding the raw inbox, submission
   store, platform, firmware, radio, device API, supervisor, and executor
   graphs from the new portable components; and
10. on the product target, exact package readback, durable record readback,
    release-tag/confirmed-TX/sender-terminal correlation, and zero runtime fault
    counters for at least one retained-proof opportunistic delivery.

Criteria 1 through 10 now have bounded evidence for the first tranche. Criterion
10 is satisfied only by the single A-to-B case described above; its explicit
limits remain part of the acceptance record.
