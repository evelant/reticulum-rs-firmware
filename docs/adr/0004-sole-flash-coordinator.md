# ADR 0004: Sole flash coordinator with operation-scoped store access

- **Status:** accepted
- **Date:** 2026-07-17
- **Decision owners:** project maintainers

## Context

The E290 product has one physical ESP flash device but several durable product
domains: immutable identity, announce clock, configuration, submission journal,
message/blob storage, and eventually OTA state. `esp-storage::FlashStorage` is a
synchronous, uniquely mutable NOR owner. `PartitionNorFlash` safely restricts a
borrow to one checked range, but that borrow still covers the complete backend.

The first journal-capable image used this fact only for a boot probe. The E290
image now transfers the sole flash owner and backend-independent
`SubmissionRuntime` into a resident product coordinator after first
provisioning or strict mount and complete boot recovery. Each runtime call
creates a fresh checked `node_journal` view and releases it before the task can
await. This avoids the permanent whole-flash borrow that the pre-decision
`StorageActor<F>` would have imposed while leaving configuration, message, and
OTA stores available to the same future coordinator.

Cloneable range handles behind a mutex do not solve the semantic problem. They
can serialize individual NOR calls, but do not prove that a complete journal
append, message commit, configuration generation, or recovery transaction owns
the flash until its durable conclusion. They also make overlapping partitions
and cross-store ordering easier to express incorrectly.

## Decision

### One product coordinator owns the physical backend

Permanent E290 composition has one resident storage coordinator containing the
only `FlashStorage`, the validated partition map, and the optional successfully
mounted durable submission runtime. It also retains the exact credential-store
binding, credential boot classification, and any successfully mounted
`MountedCredentialStore`. It is the ownership point for future enabled stores
as well. No node, radio, USB, BLE, Wi-Fi, UI, or client task
receives a raw flash owner or a generic read/write/erase capability.

The node supervisor and storage coordinator currently remain in one Embassy
task. That matches the durable runtime's direct mutable node-port contract and
avoids inventing an additional storage-command cross-task acknowledgement
protocol before measurements justify the split. The coordinator receives at
most one runtime drive attempt per outer node loop. The LoRa/SX1262 actor
remains separate and communicates only through bounded packet/observation
ownership handoffs.

### Portable durable state borrows storage per physical operation

`StorageActor` and `SubmissionRuntime` keep their semantic state, pending plans,
index, projector, and boot cursor without owning the NOR backend. Mount,
recovery, append, reconciliation, persistence, and compaction receive one
operation-scoped checked journal access from the product coordinator. Pure
projection and observation methods remain backend-free.

This follows the existing physical journal API, whose `mount`, `append`, and
`compact` operations already accept a caller-supplied mutable backend. It also
lets the coordinator create mutually exclusive short-lived regions for
configuration and message storage without unsafe shared mutation.

Removing owned `F` loses one useful guarantee: the same Rust type can describe
different devices or partitions. The refactored actor therefore stores a
`JournalBinding` established at mount and rejects a later access before I/O
unless its physical-device identity, absolute offset, length, and layout version
match. Only the product coordinator can mint a bound E290 access.

### Serialize durable transactions, not merely flash calls

The coordinator applies these global rules:

- one ambiguous store mutation is reconciled before an unrelated mutation;
- message/blob content commits and reads back before a journal reference commits,
  so reset may leave an orphan but never a reference to missing content;
- reclamation first commits that content is unreachable, then erases it;
- configuration commits a complete new generation before live node/radio state
  changes;
- OTA writes and verifies the inactive image before changing boot selection; and
- every cross-partition operation uses an explicit recoverable intent/commit
  protocol, because task order and mutexes are not durable across reset.

Public coordinator commands are typed product operations. Raw read, program,
and erase commands are not part of the task protocol.

“Ambiguous” here has a narrow ownership meaning. A same-boot
`PendingCredentialSuccessor` retained after an uncertain mutation result, or an
unresolved cross-store intent, blocks every unrelated durable mutation until
that exact owner is reconciled. A deterministic `RetirePredecessor` or
`CleanupInactive` state discovered by read-only boot mount is different: boot
attempts each reported step at most once, retains the mounted owner and failure
classification, and quarantines later credential admission/mutation for that
boot if recovery cannot finish. It does not globally block identity, announce-
clock, journal, or LoRa startup merely because the credential-domain recovery
attempt failed.

## Consequences

- The resident coordinator and exact authorized-frame request/durable-echo
  handoff are implemented. The E290 profile has a one-entry accepted-history cap
  used only for composition qualification; it is not product capacity. The
  software ownership path now passes cross-layer host tests. Portable API
  framing, immutable credential authority, the qualification-session core, and
  job handoff are qualified, and schema 2 persists exact authorization
  provenance. ADR 0009's credential partition and store are now validated,
  boot-mounted/recovered immediately after flash open, and retained in this
  coordinator without automatic provisioning. Explicit initialization and live
  pairing are routed through the pre-authentication USB records. The first
  authenticated API/session firmware lane is composed as a minimal single-
  flight USB bearer; one bounded powered handshake/identity/submission/peer-
  proof/status path passes.
- A journal strict-mount, supported-history, or recovery failure during boot
  occurs before any durability-gated DATA owner can exist and disables only
  local durable submission service. The sole flash owner remains resident and
  route-only LoRa can continue without touching the unavailable runtime. Flash-
  map validation, identity preflight/load, announce-clock reservation, and
  identity-authorized fresh journal provisioning remain boot-fatal.
- Once an authorized DATA observation is active, permanent storage failure does
  not permit the dispatcher to invent completion. The node retains the
  observation and the dispatcher retains its completion and router ticket.
  ADR 0005 enters interface-local `ActiveOwnerFailStopped`, takes the same LoRa
  lease offline, and permits no later radio operation in that boot. The E290
  host composition fault test proves this with a wrong binding after frame
  exposure and an ordinary announce queued behind the owner.
- Live external LoRa DATA now waits for explicit credential initialization/
  pairing, firmware composition of the portable
  authority/session edge, and a bearer, not another
  storage ownership, durability-policy, cap, or frame-
  handoff qualification. This is the complete primary LoRa software slice; a
  speculative second Reticulum transport is neither required nor composed.
- Device configuration and message storage can be added without changing the
  Reticulum interface fabric or handing out shared flash handles.
- Synchronous ROM flash calls still block cooperative execution. Journal scans,
  writes, sector/block erases, and compaction must be measured against radio
  deadlines; compaction may require an incremental state machine with bounded
  erase/copy steps.
- Critical-section protection around a ROM call is not transaction ownership.
  Multicore flash parking, Wi-Fi/BLE interaction, watchdog behavior, and PSRAM
  availability while caches are disabled remain powered qualification gates.
- Flash scratch and state required during ROM operations stay in internal RAM
  until PSRAM/cache-disabled behavior is explicitly qualified.

## Staged implementation

The backend-independent actor/runtime, resident E290 operation-scoped
coordinator, exact authorized-frame request/durable-echo handoff, one-entry cap,
and ADR 0005 fault behavior are implemented and pass cross-layer host
composition tests. The minimal external USB API edge now passes its first
bounded powered live-storage proof.

1. Preserve ADR 0009 credential boot ownership, the routed explicit
   initialization/live-pairing lifecycle, and the implemented authority,
   framing, session, handoff, and first authenticated local USB API bearer.
   Preserve its powered happy path while qualifying zero-write authorization
   rejection and broader failure/lifecycle cases.
2. With both physical `HT-RA62-HF` markings now confirmed, qualify E290 first provisioning,
   strict mount, boot recovery, resident ownership, authorized-frame handoff,
   ADR 0005 failure isolation, and pre-owner route-only degradation on both
   boards, including controlled powered cuts. The source-`96e38aa` erased-media
   smoke now supplies hardware evidence only for boot, zero-mutation credential
   classification, empty-journal mount, LoRa readiness, and ordinary TX.
3. Select a product-capacity policy beyond the host-qualified one-entry
   composition cap without weakening durability or principal isolation.
4. Add checked `message_store` partition validation, then typed configuration
   and message operations with explicit cross-store ordering.
5. Split storage into another Embassy task only if measurement warrants it; if
   split, exchange high-level durable commands and exact ownership tokens.
