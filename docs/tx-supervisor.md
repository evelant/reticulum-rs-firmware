# RF-inert permanent TX supervisor

**Status:** permanent portable protocol-owner aggregate and async TX-machine
run loop implemented, target-checked, and outside every firmware graph; RNS
ingress, timer maintenance, proof policy, and bounded announce admission are
exposed through the sole owner, but ordinary protocol actions remain
allocation-backed and there is no permanent firmware, storage, device-API, or
radio edge
**RF status:** both attached boards have antennas and are authorized for NA915
development TX; this crate remains RF-inert because its concrete driver/radio
owner and regional/airtime policy adapter are not implemented

## Boundary

`reticulum-tx-supervisor::TxSupervisor` owns one exact `NodeCore`, its
`NodeTxDataMachine`, the node-side `TxPermitServer`, the
`NoRfTxDispatcher`, and one authorization policy. The aggregate is intended to
live in static storage and be borrowed by one never-cancelled task for the
remainder of the boot. Dropping and reconstructing it is not an ownership
recovery mechanism.

The aggregate is now the portable sole RNS owner surface. It exposes the local
destination hash, explicit inbound delivery-proof policy, bounded signed-
announce queueing and flushing, complete-packet RNS ingress, and RNS timer
maintenance without exposing its mutable `NodeCore`. Automatic proofs default
to `Never`; opting into `Always` only makes proof packets appear in the returned
actions. Announce admission reports oversize application data or a full bounded
queue before the caller flushes ready announcements.

`flush_announces()`, `ingest_rns()`, and `tick_rns()` return their ordinary RNS
action/report envelopes to the caller. Those envelopes still contain
allocation-backed packet vectors. The supervisor neither stages those packets
into the fixed `TxPacketBuffer` pool nor retains them under downstream
backpressure, so a runtime must not discard them or treat this surface as a
radio dispatcher.

Construction also preserves channel identity and boot ownership. A
`PairedTxHandoff` can be created only from one channel store, all registered
buffer owners must be queued through it, and `NoRfTxMachineSet::try_new()`
rejects an incomplete seed set while returning the unchanged paired roles and
already queued owners. The supervisor therefore cannot combine disconnected
handoff halves or silently consume the last capability needed to finish
seeding.

The crate has no firmware, board, radio, HAL, device-API, flash, or executor
dependency. Its dispatcher still has only the fixed scalar no-RF inspector and
no pluggable byte sink. `RfInertTxPolicy` denies every otherwise valid candidate
with `RegionalProfileUnavailable`, so this aggregate cannot authorize RF.

## Clocked synchronous pass

`run_one_pass()` drives one bounded pass in this order:

1. sample the monotonic clock and call `NodeCore::maintain_tx()`;
2. sample again and step the node DATA machine;
3. when no fault has stopped new authorization, sample again and step the
   permit server and policy; and
4. sample again and step the RF-inert dispatcher.

Samples are never shared between lanes. A regressing sample is retained as a
permanent `TxClockRegression` and stops the remainder of that pass and all later
clocked transitions. `try_prepare_and_submit_data()` also replaces the
request's caller-supplied owner time with a fresh checked supervisor sample.
Every clock instance and owner deadline used with an aggregate must share one
epoch and millisecond scale.

`NodeCore::next_tx_deadline()` exposes the earliest live routed or authorized
owner deadline. `TxSupervisor::next_wake()` combines it with the dispatcher's
active permit-exchange grace deadline and returns the earlier absolute wake.
The async runner therefore does not depend on an arbitrary polling interval for
lease maintenance or permit recovery.

## Permanent async run

`run()` repeatedly executes complete synchronous passes. Sustained progress is
limited to `MAX_IMMEDIATE_PASSES`, currently 16, before an explicit executor
yield. The runner also yields after every selected wake, so a conforming clock
that wakes early cannot create a quiescent busy loop. When a pass is
quiescent, `wait_for_work()` races only waits compatible with the current
machine phases:

- node DATA return or retained-`Next` capacity progress;
- a permit request while the permit server is idle;
- dispatcher job/reply input while that machine can receive it; and
- the exact absolute maintenance or permit-grace deadline.

The public wait lets a future permanent node task race TX-machine work against
its independently owned RX-frame and RNS-timer waits. The async-clock wait is
required to be cancellation-safe. Each channel wait is cancellation-safe
because a ready owner/control value is stored in persistent machine state
before the short future completes; pending losers remain in their Embassy
channels. Dispatcher input is polled ahead of the deadline future, preserving
the rule that an already-observable exact permit reply wins a grace-deadline
tie. Cancelling `wait_for_work()` is safe; cancelling the aggregate-owning task
or reconstructing the aggregate remains outside the contract.

## Fault behavior

The supervisor retains clock, DATA-machine, permit-service, and dispatcher
faults as a copy-only snapshot. Once any permanent fault exists:

- fresh DATA preparation is rejected;
- the permit lane and authorization policy are no longer called; and
- DATA and dispatcher steps continue where possible so exact owners already in
  motion can return to the parked table or a retained fail-closed state.

A clock regression is stricter because no further clocked transition is safe;
the permanent runner waits forever for supervised reboot/recovery. No fault
path fabricates or force-reuses a missing packet-buffer owner.

## Correlation and durable projection

`NodeTxQueuedHop` now includes the generation-scoped `AttemptHandle` alongside
slot, complete attempt token, interface, packet length, preparation-time full-
packet digest, and deadline. The metadata returned for a queued or retained hop
can therefore be correlated with node-core terminal tombstones without relying
on a slot alone.

The supervisor now exposes copy-only iterators for terminal attempts, recovered
owners, and quarantines, plus exact `acknowledge_terminal()` and
`acknowledge_recovered()` facades. `reticulum-submission-projector` correlates
those observations to the project-owned durable lifecycle and unlocks an exact
acknowledgement only after the corresponding transition or transport audit is
known committed. Recovered owners remain parked and unavailable until that
acknowledgement succeeds; terminal acknowledgement can remain retryable while
the packet owner is still bound.

Ordinary parked recoveries and quarantines are consumed through their distinct
`recovered_observations()` and `quarantine_observations()` iterator paths. A
recovery-correlated owner trapped as residue inside a permanently disabled DATA
machine is not acknowledgeable and is therefore exposed only through
`data_fault_quarantine_observation()`; the caller must project it as quarantine
even when `data_fault_residue_kind()` says `RecoveredBuffer`. The residue kind
and retained supervisor DATA fault preserve the original and secondary
diagnostics without misclassifying that fail-closed owner as releasable.

This is a portable semantic boundary, not powered durability. The projector
retains an opaque write plan. `reticulum-storage-actor` now owns that projector,
the live `SubmissionIndex`, and `reticulum-storage-journal`; it can append the
canonical record, enforce physical lifetime reservation, replay, compact, and
apply the index only after durability. No permanent firmware task yet drives
that actor and this supervisor together. See
[Durable submissions](durable-submissions.md) and
[Physical submission journal](storage-journal.md).

## Work still outside the aggregate

The supervisor does not yet:

- retain or convert allocation-backed announce, proof, forwarding, and other
  `NodeActions` into fixed packet owners accepted by a bounded dispatcher;
- host timed RNode reassembly, RX and protocol-second scheduling around the
  exposed sole-owner ingress/tick surface in a permanent firmware task;
- accept device-API/LXMF intents through a physical persist-before-accept edge;
- own the flash journal, reservations, replay, compaction, and boot recovery;
- drive the existing projector observations and acknowledgements from the sole
  node task;
- safely retire completed volatile projector correlations after every source
  has been drained;
- connect projected dispositions through the implemented device-API adapter in
  the permanent firmware task; or
- provide a driver, packet-interface implementation, radio reset contract, RF
  policy, or firmware dependency edge.

The next product boundary is a permanent Embassy runtime around this sole node
owner and the portable storage actor. It must add product flash adaptation and
boot gating, race cancellation-safe TX work with timed RX/RNS work, and convert
every returned ordinary protocol action into bounded owned storage before a
real policy/radio dispatcher can accept it. No permanent firmware dependency
edge exists yet.

## Validation

The focused supervisor suite currently contains 13 host tests covering
separate and deadline-crossing clock samples, a complete RF-denied owner
lifecycle, exact-deadline recovery retention and acknowledgement, terminal
acknowledgement before and after owner return, permit-grace reply priority and
fault draining, monotonic regression, cancellation of the public combined wait,
the permanent protocol-owner surface, deadline conversion, common-origin/full-
seed construction, and static storage.

```sh
cargo test --locked -p reticulum-tx-supervisor
cargo clippy --locked -p reticulum-tx-supervisor --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-supervisor \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
```
