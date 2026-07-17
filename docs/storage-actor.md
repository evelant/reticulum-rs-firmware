# Portable sole storage actor

**Status:** portable sole-owner aggregate and narrow node-observation/
acknowledgement surface implemented and host/ESP32-S3 target-checked; permanent
Embassy task, product `esp-storage` partition adapter, firmware/device-API
transport linkage, boot service orchestration, and integrated powered-fault
qualification remain open

## Ownership boundary

`reticulum-storage-actor` is the only portable component allowed to combine one
schema-1 NOR journal with its live semantic state. A
`StorageActor<F, SUBMISSIONS, PROJECTED>` owns:

- the `MultiwriteNorFlash` backend and last completely established
  `JournalState`;
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

The actor is synchronous and executor-independent. It is the portable ownership
core for a future permanent task, not that Embassy task itself. Its consuming
`into_flash` escape is intended for tests or an explicit shutdown/recovery
boundary; ordinary firmware must keep the mounted actor in one permanent owner.

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
narrow actor-owned operations. `begin_preparation`,
`observe_preparation_result`, `observe_frame`, `observe_terminal`,
`observe_recovered`, and `observe_quarantined` are all busy/fault-gated and
plan against the actor's live index. Any returned persistence request must flow
back through `persist_projector`. `pending_acknowledgements` exposes only exact
copyable actions unlocked by durable records, and `report_acknowledgement`
updates only the actor-owned correlation after the caller reports the matching
node/supervisor result. A compile-fail doctest guards against replacing or
extracting the sole projector through safe code.

These methods deliberately accept the existing node-core and TX-dispatch
observation types instead of inventing a second correlation vocabulary. They
still do not run the orchestration loop: the permanent task must drain a
follow-on projector persistence request before admitting unrelated work. A
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
assertion requires it to remain at or below 512 bytes. An acceptance retains one
complete opaque model plan; a projector mutation retains only its handle. This
is a ceiling for one serialized ambiguity cell, not the complete actor, index,
projector, task-stack, or firmware RAM budget.

## Fault behavior

Backend errors are retryable ambiguity and leave the exact mutation pending.
Capacity and idempotency outcomes are definitive typed results. Corrupt
manifests or records, semantic replay failures, conflicting logical content,
projector/request mismatches, impossible compaction outcomes, readback mismatch,
and model-application invariants latch a bounded `StorageFault`. Once faulted,
all mutation entry points fail closed with that same fault; the actor never
publishes a partial acceptance or acknowledgement.

The backend's concrete error type is intentionally retained only at this
portable boundary. `reticulum-device-api-adapter` translates it, actor `Busy`,
identifier exhaustion, and a latched fault into stable API errors without
leaking platform error types.

## Validation and remaining integration

Focused host tests cover mount-before-service, typed durable boot recovery and
lost-reply reconciliation, acceptance/replay/conflict and
index capacity, lost acceptance and projector replies followed by autonomous
reconciliation, sole-projector identity and replacement resistance, the full
preparation/frame/terminal/retry-and-complete acknowledgement path, recovered
owner acknowledgement, quarantine audit plus deferred finalization,
busy/fault-gated observations, pre-fault rejection before flash mutation,
compaction recovery after an ambiguous target erase, and permanent fault
latching. The 17-test suite, compile-fail ownership guard, strict host clippy,
and ESP32-S3 Xtensa check/clippy pass. The existing powered storage HIL
predates this actor and calls the journal directly, so it is not on-target
actor qualification.

Product integration still requires:

- one permanent Embassy task that owns the actor for the boot lifetime;
- a checked product `esp-storage` adapter constrained to the real `retlog`
  partition, rather than only the dedicated HIL adapter;
- gating USB/BLE/Wi-Fi device-API and RF/node service until mount, replay, and
  actor-owned conservative boot recovery are complete for every replayed
  submission; the portable operation exists, but the permanent task must drive
  it to a definitive result before opening services;
- connection of the implemented authenticated device-API adapter to framing,
  sessions and USB/BLE/Wi-Fi, plus a safe projector-slot retirement handshake;
- coordination with flash cache constraints, watchdog feeding, OTA, other
  stores, journal compaction, and radio deadlines;
- controlled power-cut/brownout tests, endurance and soak measurements, stack
  and boot-scan measurements, and an at-rest encryption/provisioning decision;
  and
- merger with the sole node owner and concrete radio owner for bidirectional
  Reticulum traffic.

The two antenna-equipped development boards are authorized for NA915 TX/RX.
Current product-candidate graphs remain TX-free because storage/node/radio
integration is not implemented, not because development transmission is
forbidden. A bounded integration image may transmit whenever that accelerates
the work, provided it retains one explicit regional/airtime policy and one
radio owner.
