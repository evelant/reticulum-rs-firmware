# Portable sole storage actor

**Status:** portable backend-independent sole-owner aggregate, exact bound-
journal access, and narrow node-observation/acknowledgement surface implemented;
the separate transport-neutral submission runtime supplies portable boot/live
orchestration with the same operation-scoped access. The E290 now hosts both in
a resident sole-flash coordinator beside the transport-neutral node supervisor
and composes the exact authorized-frame request/durable-echo handoff. That
LoRa-first software composition and ADR 0005's interface-local active-owner
fail-stop now pass cross-layer host tests. Portable API framing, immutable
credential authority, the qualification-session core, and job handoff are
qualified; semantic schema 3 retains that authorization provenance and adds a
distinct exact method-neutral LXMF-message intent.
Resident credential initialization and live-pairing mutation are composed
through the pre-authentication USB records. The minimal authenticated USB
session/API lane is composed and passes one bounded powered credential/API/
DATA/peer-proof/status path. The permanent graph's empty-journal/ordinary-TX
powered smoke also passes; integrated powered-fault qualification remains open.

## Ownership boundary

`reticulum-storage-actor` is the only portable component allowed to combine one
physical-format-2, semantic-schema-3 NOR journal with its live semantic state. A
`StorageActor<SUBMISSIONS, PROJECTED>` owns:

- the exact `JournalBinding` established at mount and the last completely
  established `JournalState`;
- the `SubmissionIndex` produced by complete semantic replay;
- the sole `SubmissionProjector` and its volatile attempt correlation;
- one optional pending mutation retained across an ambiguous backend result;
  and
- one bounded fail-closed `StorageFault` latch.

The actor can be constructed only through `StorageActor::mount`. Mount scans
and validates the complete physical journal and consumes semantic replay before
returning a live value, so callers cannot query an index, plan projector work,
or accept a request from a partial replay. An erased but unformatted partition
and an unknown nonblank partition both fail closed. Formatting remains an
explicit provisioning operation outside this actor.

The physical backend remains outside the actor. Mount and every physical
mutation borrow a `BoundJournalAccess` for one synchronous operation. The actor
rejects a different device, absolute range, capacity, alignment, length, or
physical-layout version before journal I/O; this integration error does not
latch a fault in otherwise-valid durable state. A coordinator can therefore
lend mutually exclusive partition views without a boot-lifetime borrow of the
whole flash device.

The actor is synchronous and executor-independent. `reticulum-submission-runtime`
owns and advances it in the portable durability-first loop, but neither crate
is an Embassy task or product composition. See
[Transport-neutral durable submission runtime](submission-runtime.md).

## Persist-before-visible ordering

There are two serialized mutation sources:

1. `accept` asks the live index to preflight an authenticated
   `AcceptanceCandidate`, retains the exact opaque plan, appends it through the
   journal, then applies it to the live index. A new `SubmissionId` is returned
   only after commit or exact readback equivalence. Exact idempotent replay
   returns the original durable ID; conflicting content never mutates it.
2. `persist_projector` accepts only a request that can be resolved from the
   actor-owned projector. The actor retains the compact `PersistHandle`, resolves
   the exact request again before each physical attempt, and reports the
   persistence result to that same projector and live index. An equal request
   from a different projector cannot replace the owned request.

The public projector accessor is immutable. Mutation is exposed only through
narrow actor-owned operations. `ready_intent` returns an owned durable intent
only after its preparation barrier is committed and no attempt, fault, or
mutation is pending, so a caller need not retain an index borrow across a node
call.
`begin_preparation`, transport-neutral `observe_preparation`, `observe_frame`,
`observe_terminal`, `observe_recovered`, and `observe_quarantined` are all
busy/fault-gated and plan against the actor's live index. Dispatcher queue state
and interface-specific preparation results remain outside durable storage. Any
returned persistence request must flow back through `persist_projector`.
`pending_acknowledgements` exposes only exact copyable actions unlocked by
durable records, and `report_acknowledgement` updates only the actor-owned
correlation after the caller reports the matching node/supervisor result. A
compile-fail doctest guards against replacing or extracting the sole projector
through safe code.

The production preparation seam uses
`SubmissionPreparationObservation`, a transport-neutral projector vocabulary;
native `AuthorizedFrameObservation`, terminal, recovery, and quarantine types
retain exact node-core correlation. The actor itself does not choose or order
these calls. `SubmissionRuntime::drive_step` now supplies that portable loop and
drains a follow-on projector persistence request before unrelated work. A
quarantine audit, for example, can stage a second conservative final record and
has no upstream owner-release acknowledgement.

`finalize_boot_recovery(id, boot_sequence)` is the actor-owned boot mutation
edge. A fully replayed queued submission returns `ReplayQueued`, a durable final
submission returns `AlreadyFinal`, and replay-unsafe `Preparing` or
`AwaitingDelivery` work is changed to `InterruptedByReset` only after the exact
transition is committed or read back equivalent. Ambiguous writes retain the
plan, submission ID, and boot sequence in the same serialization cell;
`drive_pending()` can finish them without caller reconstruction, while a
mismatched retry receives `Busy`.

Only one mutation can occupy the actor's serialization cell. Unrelated work
receives `Busy` until the retained mutation reaches a definitive result.
Visibility follows this order:

```text
preflight exact record
  -> retain actor-owned pending identity
  -> append / compact / exact readback
  -> apply to live index and sole projector
  -> publish acceptance or release exact acknowledgement
```

If the backend reports an error after an operation may have reached flash, the
actor does not guess and does not discard the plan. Public `drive_pending()`
autonomously retries from actor-owned state. The journal then distinguishes a
new append from an already-equivalent committed record, including after a lost
program or erase reply. No original candidate, caller-owned projector, or
request copy is needed for reconciliation.

The retained cell is deliberately bounded. `PENDING_MUTATION_BYTES` measures
the actual `Option<PendingMutation>` layout in each build, and a compile-time
assertion requires it to remain at or below 544 bytes. An acceptance retains one
complete opaque model plan, including either the maximum 383-byte generic RNS
DATA payload or the maximum 431-byte exact LXMF wire. A projector
mutation retains only its handle. This
is a ceiling for one serialized ambiguity cell, not the complete actor, index,
projector, task-stack, or firmware RAM budget.

## Fault behavior

Backend errors are retryable ambiguity and leave the exact mutation pending.
Binding errors are non-latching coordinator mistakes detected before I/O; a
correctly bound access can immediately retry the retained work.
Capacity and idempotency outcomes are definitive typed results. Corrupt
manifests or records, semantic replay failures, conflicting logical content,
projector/request mismatches, impossible compaction outcomes, readback mismatch,
and model-application invariants latch a bounded `StorageFault`. Once faulted,
all mutation entry points fail closed with that same fault; the actor never
publishes a partial acceptance or acknowledgement.

The backend's concrete error type is intentionally retained at this portable
boundary. The E290 `ProductStorageCoordinator` maps runtime/backend results into
the bounded target-safe `SubmissionPortError` vocabulary; only then does
`reticulum-device-api-adapter` translate availability, busy, capacity and fault
outcomes into stable API responses. Platform error types and journal
capabilities do not cross the port.

The E290 product keeps actor faults separate from the node supervisor's mesh-
routing fault state. An ambiguous backend error retains the exact pending
mutation for unchanged retry. At boot, an unavailable journal mount, supported-
history profile, or recovery occurs before any durability-gated DATA owner can
exist and produces a resident coordinator with no submission runtime; local
durable service remains closed while route-only LoRa can continue. Flash-map,
identity, announce-clock, and identity-vacant fresh-provisioning failures remain
boot-fatal because they precede a valid core node identity and storage contract.

A permanent actor/runtime fault after an authorized DATA observation is active
does not release that owner merely because the node supervisor itself is not
draining. The E290 node retains the observation, while the portable dispatcher
retains the completion and router ticket awaiting an exact durable echo. The
implemented ADR 0005 product policy enters `ActiveOwnerFailStopped`, marks that
same LoRa lease offline without changing its generation, and stops fresh LoRa
ingress, protocol, announce, submission and radio work for the rest of the boot.
Only bounded fail-closed drainage continues; no timeout or automatic reboot
fabricates durability. A future independently owned packet actor can remain
healthy because this state is interface-local.

## Validation and remaining integration

Focused host tests cover exact mount binding and no-I/O rejection for wrong
device/range/layout/capacity, mount-before-service, typed durable boot recovery and
lost-reply reconciliation, acceptance/replay/conflict and
index capacity, lost acceptance and projector replies followed by autonomous
reconciliation, sole-projector identity and replacement resistance, the full
preparation/frame/terminal/retry-and-complete acknowledgement path, recovered
owner acknowledgement, quarantine audit plus deferred finalization,
busy/fault-gated observations, pre-fault rejection before flash mutation,
compaction recovery after an ambiguous target erase, and permanent fault
latching. The focused suite, compile-fail ownership guard, strict host clippy,
and ESP32-S3 Xtensa check/clippy pass. The existing powered storage HIL
predates this actor and calls the journal directly, so it is not on-target
actor qualification.

The E290 product library adds two cross-layer host tests over this real actor/
runtime boundary. One proves zero-write authorization rejection, one durable
acceptance and cap, the pre-node preparation barrier, exact LoRa frame
persistence/echo/completion, timeout, principal isolation, and remount. The
other injects a wrong bound-journal access after frame exposure and proves
`ActiveOwnerFailStopped` retains every owner with an ordinary action queued and
permits no later host-radio operation. At this milestone, the 125-test E290 suite covered those
two paths plus the policy/product, credential boot/runtime, live-pairing, USB/
reset, and causal-frontier surfaces. It closes software composition
qualification without claiming powered flash or RF behavior.

Remaining product work includes:

- full powered qualification of the resident E290 coordinator, which owns the sole
  flash backend and lends one short-lived bound journal view to at most one
  runtime step per outer node loop. Its software composition and bounded
  empty-journal/ordinary-TX powered smoke now pass;
- preservation of the checked `node_journal` partition boundary and
  identity-vacant first-provision authority. `provision_first()` now repairs
  only the canonical empty A1 programming trajectory and never erases; an
  existing identity uses strict mount only;
- broader powered fault/cut/soak qualification of the authenticated device-API
  adapter's target-safe `SubmissionPort` path. `ProductStorageCoordinator`
  implements the semantic port under the current 128-entry resident profile;
  active-credential USB and BLE lanes have completed bounded
  DATA/LXMF/proof/status paths, while Wi-Fi awaits field qualification;
- an exact node-owner quiescence proof before projector-slot retirement, a
  quarantine release/suppression design, and an explicit response to the
  schema-3 journal's permanent retention, 154-submission lifetime limit, and
  lack of eviction/garbage collection;
- coordination with flash cache constraints, watchdog feeding, OTA, other
  stores, journal compaction, and radio deadlines;
- controlled power-cut/brownout tests, endurance and soak measurements, stack
  and boot-scan measurements, and an at-rest encryption/provisioning decision.

The two development boards are attached with antennas, physically confirmed as
`HT-RA62-HF`, and authorized for NA915. Their isolated semantic TX/RX HIL
passed; the permanent graph's source-`96e38aa` smoke also passed exact image
readback, erased credential classification, empty-journal mount, resident
storage, and ordinary TX. The current permanent image additionally passes one
durable DATA/peer-proof terminal path and post-re-enumeration status read. Power
cuts, high-water, and full storage/product-graph qualification remain open.
The current E290 product graph has the LoRa node/radio owner plus the resident
operation-scoped durable runtime driver and a 128-entry PSRAM submission
profile. Authenticated USB and BLE lanes originate accepted local durable work
in the powered E290 graph and have completed exact end-to-end proof paths. The
same runtime
preparation contract remains transport-
neutral: LoRa is the first primary route, while later eligible interfaces can
join the node fabric without changing actor or journal semantics. No speculative
second transport is required to complete or qualify the LoRa-first slice.
