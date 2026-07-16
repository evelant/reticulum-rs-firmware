# RF-inert permanent TX supervisor

**Status:** permanent aggregate and async run loop implemented, target-checked,
and outside every firmware graph; portable durable projection exists beside
it and the independent physical journal is implemented, but there is no sole
storage actor, device API, or complete node-owner integration
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
quiescent, its internal `wait_for_work()` races only waits compatible with the
current machine phases:

- node DATA return or retained-`Next` capacity progress;
- a permit request while the permit server is idle;
- dispatcher job/reply input while that machine can receive it; and
- the exact absolute maintenance or permit-grace deadline.

The async-clock wait is required to be cancellation-safe. Each channel wait is
cancellation-safe because a ready owner/control value is
stored in persistent machine state before the short future completes; pending
losers remain in their Embassy channels. Dispatcher input is polled ahead of
the deadline future, preserving the rule that an already-observable exact
permit reply wins a grace-deadline tie. This safety applies to the short selected
waits, not cancellation of the permanent aggregate-owning task.

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
retains an opaque write plan and the caller's live `SubmissionIndex` remains
authoritative. `reticulum-storage-journal` can now append that canonical record,
enforce the physical lifetime reservation, replay, and compact, but no
implemented actor connects the plan to the journal or drives the projector and
supervisor from one permanent task. See
[Durable submissions](durable-submissions.md) and
[Physical submission journal](storage-journal.md).

## Work still outside the aggregate

The supervisor does not yet:

- own ordinary RNS `tick` output or allocation-backed actions;
- merge receive ingress with all node-core mutation under one sole node task;
- accept device-API/LXMF intents through a physical persist-before-accept edge;
- own the flash journal, reservations, replay, compaction, and boot recovery;
- drive the existing projector observations and acknowledgements from the sole
  node task;
- safely retire completed volatile projector correlations after every source
  has been drained;
- map projected dispositions into device API v1; or
- provide a driver, packet-interface implementation, radio reset contract, RF
  policy, or firmware dependency edge.

The next product boundary is the sole permanent storage actor around the
implemented power-fail-safe journal, followed by merging RX, RNS tick/actions,
submission projection, and acknowledgement into the eventual sole node owner.
Firmware TX integration remains later and separately gated.

## Validation

The focused supervisor suite currently contains 12 host tests covering
separate and deadline-crossing clock samples, a complete RF-denied owner
lifecycle, exact-deadline recovery retention and acknowledgement, terminal
acknowledgement before and after owner return, permit-grace reply priority and
fault draining, monotonic regression, cancellation of the combined wait,
deadline conversion, common-origin/full-seed construction, and static storage.

```sh
cargo test --locked -p reticulum-tx-supervisor
cargo clippy --locked -p reticulum-tx-supervisor --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-supervisor \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
```
