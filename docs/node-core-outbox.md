# Bounded node-core external-buffer DATA dispatch

**Status:** portable route/permit/completion/recovery, owning handoff storage,
and firmware-excluded RF-inert persistent dispatcher, permit server, and node
DATA-owner machine with synchronous parked-owner preparation implemented;
firmware-excluded permanent RF-inert supervisor and async runner implemented;
portable durable model/projector and independent physical journal implemented;
no sole storage actor, firmware RF TX linkage, or radio driver
**Rete pin:** `f6f5fb0637d00691e09fa0105be4df902405fee4`

## Purpose and boundary

`reticulum-node-core` proves the portable state transitions on both sides of an
owning interface handoff. Firmware will eventually supply a complete 500-byte
`TxPacketBuffer`; node-core registers it once, prepares encrypted RNS DATA
directly into it, resolves the route, and returns a unique routed `TxJob`
without moving or copying the packet array. The same owner then moves through
permit-pending, authorized or unpermitted, completion, and recovery typestates.
Alongside the concrete RNS owner, node-core stores only fixed scalar dispatch
metadata for those external buffers and the fixed DATA attempt ledger. Proofs
and timeouts move the exact attempt into an in-place terminal tombstone before
Rete removes its receipt.

The crate privately owns `reticulum-rns-rete::EmbeddedNode`, but its public
surface uses project-owned identities, destination hashes, interface targets,
packet-slot IDs, deadlines, attempt tokens and handles, terminal outcomes,
errors and capacity snapshots. It has no dependency on the device API,
Embassy, radio traits, ESP crates, a board support package or durable storage.
The separate `reticulum-tx-handoff` edge crate depends on node-core and Embassy
Sync while keeping node-core itself synchronous and executor-free.
`reticulum-tx-dispatch` depends on both portable crates and owns their packet-
interface roles in persistent state. It has no direct device-API, executor,
clock, TX-capable driver/HAL, or firmware dependency; node-core's transitive
portable RX/framing edge supplies no TX capability. A future local-session
dispatcher is a different boundary: it will depend on both node-core and
device-api and map their types explicitly.

`reticulum-tx-supervisor` is a separate firmware-excluded edge. It owns one
exact node-core, the DATA machine, permit server, RF-inert dispatcher,
authorization policy, and monotonic clock contract in a permanent aggregate.
It provides both complete synchronous passes and a never-returning async run
loop, but has no firmware, radio/HAL, flash, or device-API dependency.

`TxJob`, permit requests/replies, completions, buffers, and recovery records do
not expose packet bytes. Only an exactly matched `AuthorizedTx` can borrow the
encoded frame, once, through `frame(now)` before its deadline. This API proves
authorization semantics; it does not perform RF transmission. The bounded
channel storage and an RF-inert dispatcher now exist, but the dispatcher's only
frame consumer is a private scalar inspector. It exposes no pluggable byte sink
and has no interface implementation, radio implementation, or RF-capable
firmware connection.

`NoRfTxDispatcher` keeps each unique job, permit-pending/authorized owner,
completion, pressured owner return, and unmatched control value in a compact
persistent enum. Every `step(now)` completes one synchronous transition, so an
exact `ChannelFull<T>` value is restored before control returns. Its short
`wait_for_input()` receives only while idle or waiting for a permit reply and
stores a ready channel value in persistent state in the same poll.
`TxPermitServer` does the same for node-side permit requests, invokes policy at
most once and only for a validated live candidate, and retains a reply under
pressure. `NodeTxDataMachine` consumes the sole node job/return roles, validates
exactly the registered fixed pool during boot, and parks available, recovered,
or quarantined owners by stable slot. It processes completions through
node-core, retains every `Next` continuation unchanged until the job channel
accepts it, and synchronously prepares fresh DATA from the lowest available
parked slot. Queued returns and retained transitions take priority. Queue
preflight leaves entropy and node state untouched; an ordinary preparation
rejection reparks the validated exact owner, while a fail-closed rejection
parks its owning quarantine; an unexpected authoritative enqueue failure
retains the definitely-unsent job for rollback with the next fresh clock sample.
Terminal, expired, recovery-required, or invalid requests bypass policy.
Cancellation while a short wait remains pending leaves the item in its Embassy
channel. The DATA machine stores a ready return in persistent state before its
wait completes and waits for `Next` capacity without putting the job into the
future. `TxSupervisor::run()` now selects those phase-compatible waits plus the
next absolute deadline; the permanent top-level task borrowing the aggregate
must itself never be cancelled.

Each complete supervisor pass takes a fresh checked monotonic sample before
lease maintenance, DATA processing, permit/policy processing, and dispatcher
processing. `NodeCore::next_tx_deadline()` supplies the earliest live owner
deadline; the supervisor combines it with the active permit-recovery grace and
waits for the exact earlier instant. Sustained progress yields after 16 passes.
A retained fault blocks fresh preparation and further policy calls while DATA
and dispatcher stepping continue to drain exact owners where possible.

## External ownership and registration

`TxPacketBuffer` owns the packet array and deliberately implements neither
`Copy` nor `Clone`. Firmware will allocate each buffer in static storage and
move its unique mutable reference through the owning handoff. Node-core never
contains that 500-byte array.

Before handoff, `register_packet_buffer()` binds each buffer exactly once to
the node identity, `NodeInstanceId` and a stable `PacketSlotId`. The matching
`PACKET_BUFFERS` entry changes from `Unregistered` to `Free`; a second
registration, full metadata table or zero-capacity profile is a typed
registration rejection. A foreign owner/incarnation is rejected later if that
buffer is supplied for preparation. Registration does not prepare a packet or
register a receipt.

The related states are:

```text
external buffer:   Unregistered -> Available -> Bound(unique TX owner)
                                                   |          |
                                                   v          v
                                               Available  TxQuarantine

dispatch metadata: Unregistered -> Free -> Reserved -> Routed -> Authorized
                                                         ^           |
                                                         |-- next ---|
                                                         |           |
                                                         v           v
                                                    RecoveryRequired -> Free

attempt ledger:                   Free -> Reserved -> Active -> Terminal -> Free
                                          \-------> Free

Reserved -> Free: native preparation failure before an attempt becomes active
Active -> Terminal: proof, timeout, or exact definitely-unsent cancellation
Terminal -> Free: durable projection and explicit acknowledgement after unique
                  buffer return
```

`Bound(unique TX owner)` is an ownership statement. Only `AuthorizedTx` means
the permit linearization point has passed, and even that means “may have
transmitted”, not proof that hardware started or completed RF.

## Preparation transaction

`prepare_data_into_slot()` accepts a registered available buffer, a
`PrepareDataRequest`, and entropy. The request keeps the RNS whole-second clock
separate from the packet-owner millisecond clock and includes the destination,
plaintext, `owner_now`, `TxLeaseDeadline`, and a synchronous snapshot of
enabled packet interfaces:

1. Reject `deadline <= owner_now` before reservation, entropy use, or RNS
   mutation.
2. Validate the buffer owner/incarnation and its free dispatch slot.
3. Reserve a free attempt-ledger slot and checked, non-repeating attempt,
   dispatch, and hop generations before consuming entropy or mutating RNS.
4. Mark dispatch and attempt metadata `Reserved`, then call
   `EmbeddedNode::prepare_data_into()` with the external buffer's exact
   500-byte array.
5. On any native preparation failure, restore both metadata reservations and
   return the same available buffer inside `PrepareFailure`.
6. On success, commit the full receipt hash to the active attempt and resolve
   Rete's `All`, `Only(interface)`, or `AllExcept(interface)` target against the
   supplied enabled-interface snapshot.
7. Bind the first deterministic hop, dispatch generation, target, and deadline
   into the buffer and return the only `TxJob` that owns its mutable reference.

Capacity, registration and generation failures are rejected before entropy or
RNS mutation. Native receipt-table saturation, duplicate receipt hashes,
unknown destinations, oversized payloads, cryptography and packet-build
failures unwind the reservations and return the exact same buffer. After
native success, committing the pre-reserved metadata cannot allocate or fail.
An empty resolved route cancels the exact receipt and returns the same
available buffer. If that cancellation unexpectedly fails, preparation returns
an owning `TxQuarantine` and leaves the scalar recovery record retained instead
of pretending the buffer is reusable.

`TxJob` exposes the stable slot, encoded length, preparation-time SHA-256 over
the complete encoded packet, distinct RNS proof-correlation token,
generation-scoped attempt handle, original interface target, selected interface,
and deadline. It has no byte accessor. Route selection is ascending by
`PacketInterfaceId` bit, and each subsequent hop receives a fresh checked
generation. Multi-interface fan-out is serialized through the same unique
buffer; no packet copy or repeated interface is introduced.

The copy-only `NodeTxQueuedHop` metadata reported by the DATA machine preserves
that `AttemptHandle` alongside the slot, full attempt token, interface, packet
length, full-packet digest, and deadline. `reticulum-submission-projector`
retains the handle, token, expected length, and expected digest in volatile
correlation state; the durable record contains no packet slot, generation,
deadline, or reference.

The adapter independently preflights the current 383-byte plaintext limit and
bounded receipt table before encryption. `AttemptToken` is the full Reticulum
hashable-part digest covered by a proof, not SHA-256 of every encoded interface
byte. Node-core computes the distinct complete-packet SHA-256 immediately after
successful preparation and retains it in authoritative queued metadata. The
RF-inert dispatch inspector independently rehashes the exact frame while it
holds the authorized byte borrow. The projector requires the preparation and
sink digests and lengths to agree before retaining an exact planned record;
only a storage backend's commit or exact readback result permits the live index
to apply it.

## Queue rejection and exact rollback

`NodeTxDataMachine` preflights job capacity before removing a parked owner, so
ordinary queue pressure consumes no buffer, entropy, or node state. If the
authoritative handoff still returns `ChannelFull<RoutedTxJob<'static>>`, the
machine retains that exact job as `FreshRollbackPending`. Its next
`step(owner, fresh_now)` calls `rollback_queued(job, fresh_now)`; it never reuses
the preparation request's older `owner_now`. Node-core synchronously proves that
the recovered routed job was never accepted. It validates the node incarnation,
stable slot, dispatch generation, receipt, attempt handle, target, hop, and
deadline before changing any state and returns the same
`TxCompletionDisposition` vocabulary as ordinary completion handling.

For a still-active attempt, rollback cancels the exact full-hash RNS receipt and
commits a retained `Terminal(Unsent(QueueRollback))` tombstone before freeing
the dispatch metadata. It restores the same external buffer to `Available` and
returns its original unique mutable reference, but the attempt ledger does not
become `Free` until that unsent final disposition is durably projected and the
exact terminal is explicitly acknowledged. If the receipt is unexpectedly
missing, or any binding is stale or inconsistent, `RollbackFailure` retains the
still-bound `TxJob`; the buffer is not silently reused. A rollback observed at
or after its deadline first enters the exact scalar recovery state, then
finalizes that matching late owner as `Recovered`; it cannot silently bypass
deadline accounting. Rollback is
forbidden once any earlier serialized hop was authorized, even if the current
hop is again in the routed state. The cumulative possibly-transmitted
classification cannot be erased.

A `TxCompletionDisposition::Next` is not a fresh rejection: an earlier route
may already have been authorized. `NodeTxDataMachine` therefore keeps that
exact continuation in persistent state, gives it priority, and retries
`try_send` without rollback until the sole job channel accepts it.

A proof or timeout may commit while the job remains bound. In that case Rete
has already removed the receipt, so rollback releases the dispatch and returns
the same buffer without cancelling a nonexistent receipt, while preserving the
terminal tombstone. `acknowledge_terminal()` reports `PacketStillBound` until
that release occurs. This ordering prevents an application from freeing the
tombstone while unique packet ownership is still outstanding.

Dropping any owning typestate does not reset its private buffer binding or
dispatch entry. A later preparation attempt with that buffer is rejected as
`PacketBufferBusy` before entropy or RNS mutation. Deadline maintenance can
move the authoritative scalar dispatch record into `RecoveryRequired`, but it
never fabricates or force-reuses the missing reference.

## Permit, byte access, and completion

`TxJob::begin_permit()` consumes routed ownership into `PermitPendingTx` and an
opaque non-`Copy` `TxPermitRequest`. `authorize_tx(request, now, policy)` first
validates node identity, `NodeInstanceId`, slot, dispatch generation, selected
interface, and per-hop generation. It denies terminal, expired, or retained-
recovery work without invoking policy. Only a fully valid candidate reaches
the synchronous regional/airtime policy.

A policy grant immediately and irrevocably changes authoritative dispatch
state to `Authorized` and sets the cumulative `may_have_transmitted` bit before
the non-`Copy` reply leaves node-core. This is the linearization point. A lost
or delayed grant can therefore never be reclassified as definitely unsent.
`PermitPendingTx::resolve(reply, now)` also checks the exact binding; a mismatch
retains both the unique pending owner and unchanged reply. A grant resolved at
or after the exact deadline produces `ExpiredAuthorizedTx`, which has no byte
accessor but remains possibly transmitted.

Only `AuthorizedTx::frame(now)` can expose packet bytes. It is one-shot, binds
the returned `TxFrame` to the exact permitted interface and attempt, and rejects
`now >= deadline`. The owner cannot be consumed into a completion while its
frame borrow is live. `ExpiredAuthorizedTx`, `AuthorizedTx`, and
`UnpermittedTx` produce owning completions with distinct conservative
classifications.

`complete_tx(completion, now)` reconciles that classification against the
authoritative scalar dispatch phase. An unpermitted hop advances to the next
deterministic route only while the attempt is nonterminal and its deadline is
still in the future. After the final definitely-unsent hop, the exact receipt
is cancelled only if no prior hop was authorized, and the attempt becomes a
retained unsent terminal rather than immediately free. Once any hop was
authorized, the receipt remains live through all later definitely-unsent
returns. A proof or timeout terminal stops later fan-out while preserving its
tombstone.

## Exact deadlines and retained recovery

Deadline comparison is consistently inclusive: `now >= deadline` is expired
in preparation preflight, authorization, permit-reply resolution, frame
access, completion, and `maintain_tx()`. Expiry never frees a slot. Maintenance
moves routed or authorized scalar dispatch metadata to `RecoveryRequired` and
publishes records in stable slot order. Each `TxRecoveryRecord` includes the
`NodeInstanceId`, packet slot, dispatch generation, selected interface,
deadline, observation time, prior phase, cumulative possibly-transmitted bit,
and bounded reason.

While the unique owner is missing, this scalar dispatch record is
authoritative. A coherent late completion for the exact owner/incarnation,
slot, generation, interface, and conservative phase returns
`TxCompletionDisposition::Recovered`; node-core finalizes the metadata and the
same buffer binding becomes reusable. `NodeTxDataMachine` parks the recovered
buffer with its complete generation-safe observation and does not expose it as
available until exact acknowledgement. The supervisor exposes both the
observation and acknowledgement facade. `reticulum-submission-projector`
withholds that action until the transport audit is known committed, although
no permanent runtime drives it. `reticulum-storage-journal` now supplies the
physical append/replay/compaction backend, but no sole storage actor connects
the projector plan to that backend yet.
An internally inconsistent same-lease return or an explicit recovery fault
returns an owning `TxQuarantine` and retains the fail-closed scalar record.
Before exposing its `TxRecoveryObservation`, quarantine canonicalizes the
private buffer binding from that authoritative dispatch record; inconsistent
buffer-side handle or token metadata cannot redefine durable correlation.
Wrong-owner or stale completions are rejected intact as `TxCompletionFailure`
and disable the node DATA machine without losing the completion. Recovery never
invents ownership, and notification loss cannot make a slot reusable.

The RF-inert dispatcher adds a configured permit recovery grace after the owner
deadline because the node may already have authorized a request whose reply is
delayed. On the first step sampling at or after that threshold it checks for a
reply first. Any reply observable by that step wins regardless of enqueue time;
a late grant is resolved as byte-inaccessible `ExpiredAuthorizedTx`. With no
observable reply, it returns the exact pending owner as a recovery-fault
completion, permanently disables itself, and requires node-core to
quarantine/reconcile the return. It never guesses whether authorization
occurred.

## Receipt terminal ledger

`EmbeddedNode::ingest_with_receipt_sink()` and
`tick_with_receipt_sink()` present the exact receipt kind and complete packet
hash before changing Rete receipt or proof-deduplication state. Node-core's
private reservation matches only an active DATA slot with that full hash. The
same slot becomes `Terminal { Delivered | DeliveryTimeout }`, so terminal
commit cannot allocate or fill a second queue. A proof or timeout can establish
that tombstone before permit resolution or frame observation; the projector
uses the preparation-bound digest and length for direct delivery and permits a
timeout to finalize without first writing `AwaitingDelivery`.

Unknown DATA hashes, already-terminal hashes and channel candidates are typed
`ReceiptCorrelationError` invariants, not retryable capacity failures. Rete
retains the affected receipt/proof state. Timer maintenance still returns its
ordinary actions alongside a fault so unrelated maintenance output is not
lost.

`terminal_attempts()` observes tombstones without removing them.
`acknowledge_terminal(handle)` frees one only after the storage backend has
proved the exact final record durable, the projector has unlocked that
acknowledgement, and no job remains bound. Node identity,
`NodeInstanceId`, ledger index and monotonic generation scope the opaque handle
against stale copies and ABA reuse. The ledger remains RAM-only and cannot
rehydrate active Rete receipts or terminal submissions after reset.

## Capacity and constrained hardware

`PACKET_BUFFERS` now bounds registered external buffers and node-owned dispatch
metadata; it no longer multiplies a 500-byte array inside `NodeCore`. Firmware
must still allocate the actual buffers, so product RAM budgeting includes at
least `PACKET_BUFFERS * 500` external packet bytes, each buffer's binding
metadata, node-core dispatch/attempt/Rete state and the handoff channel items
that carry their references. The node DATA machine additionally uses one fixed
per-slot enum containing a pointer plus bounded recovery metadata at most; a
layout guard keeps every packet array external. The current firmware allocates
none of this TX path.

`PATHS` bounds the current Rete path, reverse and destination-DATA receipt maps
and the node-core attempt ledger. Rete's heapless maps require `PATHS` and
`LINKS` to be powers of two greater than one; announce and deduplication deques
require `ANNOUNCES > 0` and `DEDUPLICATION > 0`.
`capacity_profile_is_supported()` exposes that project-owned guard, and
`NodeCore::new()` has compile-time assertions for a monomorphized invalid
profile. Retained tombstones intentionally reduce new-attempt capacity until
acknowledged.

`CapacitySnapshot` reports registered buffers, queued/used/configured dispatch
metadata, current/configured Rete receipts and active/terminal/used/configured
attempt slots. It exposes no native Rete collection or packet bytes.

The crate itself uses no allocator API and compiles for generic bare metal.
Its owned Rete node still has allocation-backed construction and ordinary
ingest/action paths, so this result applies only to external-buffer DATA
preparation, dispatch metadata and the attempt ledger. Before selecting a
no-PSRAM Tracker profile, record Xtensa static layout plus linked `.bss`, heap
and stack headroom for the complete firmware allocation.

## Current validation

```sh
cargo test --locked -p reticulum-node-core
cargo clippy --locked -p reticulum-node-core --all-targets -- -D warnings
cargo check --locked -p reticulum-node-core \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-node-core \
  --target xtensa-esp32s3-none-elf
cargo test --locked -p reticulum-tx-handoff
cargo clippy --locked -p reticulum-tx-handoff --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-handoff \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-handoff \
  --target xtensa-esp32s3-none-elf
cargo test --locked -p reticulum-tx-dispatch
cargo clippy --locked -p reticulum-tx-dispatch --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-dispatch \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-dispatch \
  --target xtensa-esp32s3-none-elf
cargo test --locked -p reticulum-tx-supervisor
cargo clippy --locked -p reticulum-tx-supervisor --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-supervisor \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
```

The 43-test node-core host suite covers stable one-time registration, pointer-stable
no-copy preparation, deadline-before-mutation rejection, empty and deterministic
multi-interface routes, per-hop generations, exact queue rollback, cumulative
prior authorization, opaque permit matching, policy and terminal races, exact-
deadline authorization/reply/frame/completion/maintenance behavior, serialized
fan-out, coherent late recovery, fault and invariant quarantine, cross-
incarnation/stale returns, completion-metadata tamper quarantine, receipt
terminal races, tombstone backpressure, and exact acknowledgement reuse. A
layout guard keeps packet-sized arrays out of
dispatch slots. Five handoff unit tests cover production-mutex static
construction, static-reference identity, FIFO ordering, owner/control pressure,
exact `ChannelFull<T>` returns and mismatched permit replies. Five host-only
integration tests manually step the real job/request/reply/return ports through
authorized no-RF frame inspection, policy denial, exact-deadline grant
expiry/recovery, serialized two-interface fan-out with the same owner, and
terminal-before-authorization suppression. Generic bare-metal and ESP32-S3
checks compile node-core, the Embassy edge, the RF-inert dispatcher, and the
permanent supervisor. The 33-test TX-dispatch suite comprises fifteen
dispatcher/permit tests covering persistent serialized fan-out, exact-deadline
late-grant recovery, cancellation of short waits, terminal suppression, absent
and mismatched reply fail-closed behavior, one-shot policy invocation under
reply pressure, exact owner restoration under return pressure, inclusive grace
threshold observation semantics, authorization/recovery orderings, idle
orphan-reply wakeup, and production-mutex static layout. Eighteen node
DATA-machine tests cover validated fixed-pool seeding, exact buffer identity,
lowest-slot synchronous preparation, rejection restoration without entropy,
return and `Next` priority, queue preflight, before/deadline rollback, rollback
failure retention, final and recovered parking, generation-scoped recovery
acknowledgement, quarantine, exact owner binding, completion-failure retention,
cancelled return waits, `Next` pressure/readiness cancellation, and compact
production-mutex layout.
The 12-test supervisor suite covers separate and deadline-crossing fresh clock
samples, the complete RF-denied lifecycle, exact-deadline recovery retention,
terminal/recovery acknowledgement facades, permit-grace reply priority and
fault drain, monotonic regression, combined-wait cancellation, deadline
conversion, common-origin/full-seed construction, and static storage. The
focused node-core and dispatch suites contain 43 and 33 tests respectively.
None of these crates is linked into firmware, and there is still no driver,
radio, or RF path.

## Next boundary

The owning storage/capability layer, RF-inert dispatcher, permit and node
DATA-owner machines, and permanent supervisor now exist.
`TxHandoff::split_paired()` consumes one unique static handoff; every registered
owner must seed that inseparable common-origin role set before
`NoRfTxMachineSet::try_new()` can bind it into the supervisor. Incomplete
construction returns the paired roles and queued owners unchanged. Pool-sized
channels carry jobs and owner returns, depth-one channels isolate permit
requests/replies, and every send is a non-awaiting `try_send` that returns the
unchanged value on pressure. The remaining orchestration work is:

1. Wrap the implemented physical journal in the sole permanent storage actor,
   preserving exact append/readback, lifetime reservation, complete integrity-
   validated replay, and resumable compaction while adding serialized
   projector/API ordering. Complete RF-inert powered and controlled power-fail
   testing. The portable model and projector do not make their own durability
   claim.
2. Merge RX ingress, ordinary RNS tick/actions, durable submission projection,
   and exact acknowledgement into
   the eventual sole node owner. The current aggregate drives only TX lease
   maintenance and the three TX machines.
3. Map persist-before-accept intents and projected dispositions into device API
   v1, drive the model's conservative boot recovery, and add a proved safe
   retirement condition for bounded volatile projector slots without
   attempting to persist leases or mutable references.
4. Convert allocation-backed ordinary RNS actions into caller-reservable packet
   ownership; the DATA path alone does not cover proofs, announces, forwarding,
   Links or Resources.
5. Keep every project firmware graph TX-free and all project radio-bearing
   firmware artifacts RX-only. The separately derived RNode peer is an external
   guarded development artifact, not a project dependency. Only after these
   boundaries and explicit antenna/load and regional approval may a guarded
   driver/radio implementation or RF HIL use this path.
