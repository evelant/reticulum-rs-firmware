# Owning async TX handoff

**Status:** portable route/permit/completion/recovery and owning Embassy
handoff storage implemented and target-checked; firmware-excluded RF-inert
persistent packet-interface state machine, node-side permit server, and fixed
per-slot DATA-owner machine with synchronous preparation implemented;
firmware-excluded permanent RF-inert supervisor and async runner implemented;
no firmware TX graph or radio driver
**RF status:** compile-disabled until antenna/load and regional authorization

## Decision

Move unique `&'static mut TxPacketBuffer` references through two bounded
Embassy channels. Only a pointer crosses tasks: the fixed 500-byte packet does
not move or copy, and the radio actor can retain the unique buffer across an
`await` without borrowing node-core.

Node-core remains portable and Embassy-free. It owns attempt/dispatch metadata
but receives an externally owned buffer for preparation. The implemented
`reticulum-tx-handoff` crate owns the Embassy channel topology. The Tracker
firmware does not depend on that crate and gains no TX-capable radio type or
feature as part of this slice. The separate `reticulum-tx-dispatch` crate owns
the handoff roles in persistent state, but is also excluded from every firmware
graph. It has no executor, timer, device-API, TX-capable radio driver/HAL, or
pluggable byte-sink dependency and cannot transmit. Node-core's transitive
portable RX/framing edge supplies no TX capability.

Within that crate, `NodeTxDataMachine` consumes the sole node-side job sender
and owner-return receiver. It validates the complete registered buffer pool at
boot, parks owners by stable slot, reconciles completions through node-core,
retains serialized continuation jobs, and synchronously prepares fresh DATA
from parked owners without exposing raw owners.

The separate firmware-excluded `reticulum-tx-supervisor` crate owns one exact
node-core, DATA machine, permit server, RF-inert dispatcher, authorization
policy, and monotonic clock contract in a permanent aggregate. It has no
firmware, radio/HAL, flash, or device-API dependency. Its initial
`RfInertTxPolicy` denies every RF authorization. The aggregate/run-loop
contract is detailed in [RF-inert permanent TX supervisor](tx-supervisor.md).

## Ownership topology

Allocate zero-filled buffers in `.bss` with `ConstStaticCell`, then place each
unique reference into the available/completion channel before tasks start:

```text
available/completion channel -> NodeTxDataMachine parked-owner table
    -> synchronous node-local reservation and RNS preparation
    -> jobs channel
    -> one persistent packet-interface dispatcher
    -> available/completion channel
```

The node actor is the sole consumer of available/completed buffers and the
sole producer of jobs; `NodeTxDataMachine` owns both capabilities. One
dispatcher owns every enabled packet interface and
has the inverse channel roles; it serializes each route and invokes the
selected interface internally. Independent per-interface actors would instead
require per-interface queues or a routing dispatcher in front of them, because
multiple consumers of one jobs queue could take work for the wrong interface.
Both owner-channel capacities equal the pool size. The separate permit-request
and permit-reply channels each have depth one, matching the single serialized
dispatcher. While one endpoint owns a buffer, its outbound owner channel can
contain at most every other buffer; a send is therefore capacity-infallible if
the endpoints remain encapsulated. An unexpected full result always returns
the exact non-`Copy` value in `ChannelFull<T>`, never a drop or duplication.
The node DATA machine specifically retains and retries a pressured `Next`
continuation because an earlier hop may already have been authorized.
Fresh preparation first stores any already queued owner return, then preflights
job capacity before selecting the lowest available slot. A preparation
rejection ordinarily validates and reparks the exact buffer; a fail-closed
route-cancellation fault instead parks the returned owning quarantine. If
authoritative enqueue unexpectedly rejects a newly prepared, definitely-unsent job, the machine
retains it and calls node-core rollback on the next synchronous step with a
fresh clock sample; it never retries that fresh job or reuses the stale request
timestamp. Exact-deadline rollback therefore enters ordinary recovered-owner
acknowledgement instead of silently making the buffer available.

`TxHandoff::split(&'static mut self)` consumes the unique reference normally
obtained from `ConstStaticCell` and creates one `NodeHandoff` plus one
`DispatcherHandoff`. Their individual port capabilities are non-`Clone`,
`must_use`, and require `&mut self` for every send or receive operation. Raw
Embassy channels/senders/receivers, `clear()`, and owner-taking async sends are
not public. Only receiving may await; all four send directions use
non-awaiting `try_send` and return the unchanged value on pressure.

Cancelling a receive future while it is still pending leaves its value in the
Embassy channel, including when it was woken but not polled again. Once a
receive returns an owning value, however, cancellation of a surrounding future
would logically abandon that owner unless it was first moved into persistent
machine state. `NoRfTxDispatcher` therefore keeps every `TxJob`, pending owner,
authorized owner, completion, and retained full-channel value in a compact
state enum allocated outside its short wait future. Each synchronous `step()`
performs one consuming transition and restores an exact value immediately if a
`try_send` is full. `wait_for_input()` and the permit server's
`wait_for_request()` return no owner; once an Embassy receive becomes ready,
they assign the value to persistent state in the same poll before reporting
readiness. `NodeTxDataMachine::wait_for_progress()` likewise assigns a ready
owner return before completing. When a `Next` job is pressured, it polls only
non-owning job-channel readiness while the exact job remains in persistent
machine state.

Those helpers make cancellation of a still-pending short wait safe. They do not
make cancellation of the top-level owner safe: a `StaticCell` cannot reacquire
a lost unique `&'static mut` runtime reference. `TxSupervisor::run()` now
provides the permanent aggregate-owning async loop. It executes at most 16
immediately productive complete passes before yielding, yields again after
every selected wake, and its quiescent `select` includes only waits compatible with the current DATA, permit, and
dispatcher phases plus the next absolute deadline. Losing waits retain their
values in channels or persistent state. The task borrowing the aggregate for
`'static` must itself never be cancelled; explicit reboot recovery remains a
future product boundary, and no firmware currently spawns this runner.

The return path carries more than the bare reference. Node-core already
provides non-`Copy` owning typestates whose payload remains one buffer pointer
plus scalar metadata:

```text
RoutedTxJob
  -> PermitPendingTx
  -> AuthorizedTx | ExpiredAuthorizedTx | UnpermittedTx
  -> TxCompletion

NodeCore::complete_tx(TxCompletion)
  -> Next(RoutedTxJob)
   | Available(buffer)
   | Recovered { buffer, observation }
   | Quarantined(TxQuarantine)
```

Each completion retains a bounded driver/retry code. The initial policy permits
no same-interface retry: an unpermitted completion either advances once to the
next deterministic route or cancels the exact receipt when no route remains
and no earlier route was authorized. Dispatch metadata retains a cumulative
`may_have_transmitted` bit; once set it never clears, and the receipt stays live
even if every later route is definitely unsent. Authorization sets that bit at
permit issuance, before the reply leaves node-core, even if the driver later
reports an error or the reply arrives after the deadline. A recovery fault or
same-lease invariant quarantines the returned unique owner. The node owner
validates incarnation, dispatch/hop generations, interface, attempt, and
completion class before advancing the serialized route plan.

Safe `ConstStaticCell` and Embassy channel APIs encapsulate the required unsafe
internals. Project crates retain `#![forbid(unsafe_code)]`; do not introduce
`static mut`, raw pointers, heap boxes, or forced mutex reclamation.

## Portable state and types

Node-core now provides the project-owned identifiers and target/set types
below, including the resolved `TxRoutePlan`:

```text
PacketSlotId(u16)
PacketInterfaceId(u8)
MonotonicMillis(u64)
TxLeaseDeadline(MonotonicMillis)

TxTarget = All | Only(interface) | AllExcept(interface)
InterfaceSet(u64)
TxRoutePlan { remaining interfaces }
```

`TxPacketBuffer` contains its 500-byte array and private generation-scoped
binding. A matching non-`Copy` owned job, opaque non-`Copy` permit request and
reply, and `AuthorizedTx` are required to expose bytes. Checks bind slot, owner
identity, `NodeInstanceId`, dispatch generation, selected interface, and hop
generation; stale or foreign control messages cannot expose a reused buffer.

Node-core preserves `PreparedData::target()` as project-owned `TxTarget`
metadata and resolves it synchronously against the request's snapshot of
enabled Reticulum packet interfaces. USB, BLE, and Wi-Fi device-API transports
are not automatically RNS packet interfaces. Multi-interface fan-out selects
ascending interface IDs, serially reuses the same unique buffer, and issues a
fresh generation-bound permit exchange for each hop. An empty resolved route
cancels the exact newly created receipt and returns the same available buffer;
an unexpected cancellation failure enters fail-closed recovery.

## Transactional preparation

`NodeTxDataMachine::try_prepare_and_submit_data` first gives a retained
transition or already queued owner return priority, then preflights job-channel
capacity. It selects the lowest validated `Available` owner from its internal
parked table and calls a portable API equivalent to:

```text
prepare_data_into_slot(buffer,
                       PrepareDataRequest {
                         destination, plaintext, rns_now,
                         owner_now, deadline, enabled_interfaces
                       },
                       rng)
```

The transaction must:

1. reject `deadline <= owner_now` before any reservation or mutation;
2. validate that the buffer ID maps to free dispatch metadata;
3. reserve attempt-ledger, dispatch, and hop generations before entropy/RNS
   mutation;
4. prepare directly into the external buffer;
5. restore metadata and leave the buffer free on failure;
6. resolve the target and bind length, receipt token, route, attempt, instance,
   and generations on success; and
7. enqueue the unique routed reference as a job.

Pool exhaustion is rejected before entropy or RNS mutation. An already full job
channel is also rejected before a buffer is removed, so it consumes neither
entropy nor node state. If the authoritative `try_send` nevertheless reports
full, the machine stores the returned exact `TxJob` as `FreshRollbackPending`.
Its next `step(owner, fresh_now)` invokes `rollback_queued` using that fresh
clock sample, never the preparation request's stale `owner_now`. Before the
deadline this cancels the exact receipt; at or after the deadline it enters and
finalizes exact-owner recovery, returning `Recovered`. A
`TxCompletionDisposition::Next` job after prior authorization must instead
remain in persistent node state and be retried; rollback deliberately rejects
it. Neither path may use a cancellable `send(job).await` that can lose the owner
future.

## Authorization boundary

Owning bytes is not authorization to transmit. The RF-inert dispatcher splits
its `TxJob` into `PermitPendingTx` plus an opaque non-`Copy` scalar
`TxPermitRequest` before its only byte inspection. A future driver integration
must preserve that same gate immediately before the first irreversible
hardware action. The node owner linearizes:

- stale, terminal, cancelled, expired, recovery-required, wrong-interface or
  policy-denied work: deny without touching radio TX;
- active work with a valid route, deadline, regional profile, and airtime
  reservation: change `Routed -> Authorized`, set `may_have_transmitted`, and
  issue a generation-bound non-`Copy` permit.

Permit requests and replies use separate depth-one scalar channels; they never
enter either buffer-owning channel or affect its capacity proof. A request
binds owner, node instance, packet slot, dispatch generation, interface, and
hop generation. Permit issuance is one single-owner transition and is
irrevocable: after it succeeds, the dispatch is conservatively classified as
possibly transmitted even if a later driver reports an error or misses the
deadline. `TxPermitServer` owns the node-side ports, invokes the synchronous
authorization policy at most once per request and only for a validated live
candidate, and retains the exact reply without reauthorizing while its channel
is full. Terminal, expired, recovery-required, or invalid requests bypass
policy. `NoRfTxDispatcher` keeps at most one exchange outstanding, retains
exact full or mismatched control values, returns the pending owner as a recovery
fault, and permanently disables itself on a control-plane invariant instead of
dropping either side.

`PermitPendingTx::resolve(reply, now)` rejects a mismatched reply while retaining
both owners. A grant resolved at or after its deadline becomes
`ExpiredAuthorizedTx`: it exposes no bytes but remains possibly transmitted
because issuance already won the race. Before the deadline, only
`AuthorizedTx::frame(now)` can borrow the encoded packet. That accessor is
one-shot and also rejects the exact deadline (`now >= deadline`).

A native in-RAM terminal tombstone established before authorization suppresses
transmission. Once authorization wins the race, RF may occur; later
proof/timeout state remains retained and terminal acknowledgement remains
blocked until the unique buffer returns and the separate durable disposition is
proved committed.

Cancellation before authorization denies the permit and cancels the RNS
receipt after cooperative buffer return. Cancellation after authorization
cannot promise unsent status: stop later fan-out/retries and classify it as
possibly transmitted.

## Deadline and recovery

A deadline never frees a slot by itself. The unique reference may still be in
a channel, driver future, SPI transaction, or interface task. Exact-deadline
comparison is inclusive throughout the portable API: `now >= deadline` is
expired during preparation preflight, authorization, permit-reply resolution,
frame access, completion, rollback, and maintenance. Expiry changes scalar
metadata once to `RecoveryRequired` and publishes a bounded supervisor record
containing `NodeInstanceId`, slot, dispatch generation, selected interface,
prior phase, deadline, observation time, reason, and whether RF may have
started.

While the unique owner is away, the dispatch slot's scalar record is
authoritative. It does not own or fabricate the missing mutable reference.
Supervisor notifications are observational and may coalesce or drop under
pressure without losing the retained recovery state. When the exact late owner
returns with the conservative completion class implied by its prior phase,
node-core finalizes that record and returns `Recovered { buffer, observation }`;
the buffer binding is then reusable. The node DATA machine nevertheless parks
the buffer with its complete correlated observation until the storage actor
proves the exact audit committed and the projector unlocks the action; the
permanent supervisor then acknowledges that observation. A same-lease
metadata mismatch or reported recovery fault instead returns an owning
`TxQuarantine` and keeps the scalar record fail-closed. A foreign or stale
completion is rejected intact, not reclaimed.

- Before authorization: deny later permits and request a definitely-unsent
  cooperative return.
- After authorization: request radio cleanup and classify any return as
  possibly transmitted.
- On a coherent exact return: finalize metadata and reclaim the exact buffer
  as `Recovered`.
- On a fault or invariant mismatch: retain both the scalar recovery state and
  the externally owned `TxQuarantine`; do not reuse the buffer.
- No return by the recovery grace deadline: disable TX, retain the fault and
  request supervised hardware recovery. Never fabricate or force-reuse the
  missing reference.

The dispatcher separately configures a permit-exchange recovery grace interval
after the owner deadline. It continues to seek the exact reply because the node
may already have crossed the irrevocable authorization point. On the first
step whose clock sample is at or after the grace threshold, it checks the reply
queue first. Any reply observable by that step wins regardless of its enqueue
time and is resolved normally; a late grant still becomes byte-inaccessible
`ExpiredAuthorizedTx`. If no reply is observable, the dispatcher returns the
exact pending owner as a control-plane recovery-fault completion, permanently
disables itself, and leaves node-core to quarantine/reconcile that owner. It
never guesses whether authorization occurred. Permit-request pressure follows
the same fail-closed principle and retains an unsent request when its grace
expires.

The permanent supervisor samples its monotonic source separately before
`maintain_tx()`, the DATA machine, the permit server/policy, and the dispatcher;
no lane receives a stale sample borrowed from another transition. Node-core
exposes its exact earliest live owner deadline, and the supervisor combines
that with an active permit-exchange grace deadline for the next absolute wake.
A monotonic regression is retained as a permanent fault. Other permanent
faults stop fresh preparation and further policy calls while DATA and
dispatcher stepping continue to drain exact owners where their state machines
permit.

`lora-phy` 3.0.1 waits for DIO1 with SX1262 hardware TX timeout disabled and
warns against cancelling IRQ processing. It enables the RF switch and issues
`SetTx(0)` before that unbounded wait. MCU reset alone is therefore not an
accepted hung-radio recovery boundary. A future TX-capable BSP must first prove
an independently assertable SX1262 RESET plus CTX-safe sequence that remains
available to the supervisor; until then every hardware TX type and feature
stays absent. MCU reset recovers volatile software state only.

Reset discards volatile references and native receipts. Higher-level durable
LXMF/submission records must reconstruct fresh attempts under a new
`NodeInstanceId`; leases are never persisted. If reset follows airtime
authorization, charge the entire reservation conservatively so reboot cannot
reset regulatory accounting.

## Implementation boundary before RF approval

Implemented and host/target-testable without RF:

- external-buffer node-core ownership, target resolution, deterministic
  serialized fan-out, opaque permit state, one-shot authorized byte access,
  completion, exact deadlines, and recovery diagnostics;
- `reticulum-tx-handoff` static one-time role splitting, pool-depth owner
  channels, depth-one permit channels, exclusive non-`Clone` capabilities, and
  exact `ChannelFull<T>` ownership returns;
- `reticulum-tx-dispatch` persistent RF-inert ownership phases, one-transition
  synchronous stepping, exact backpressure restoration, cancellation-safe
  short waits, permit-grace quarantine behavior, and a node-side permit server
  that authorizes once and retains a pressured reply;
- `NodeTxDataMachine` validated boot seeding into a fixed per-slot owner table,
  lowest-slot synchronous DATA preparation, return/continuation priority,
  queue preflight, exact preparation-rejection restoration, clocked fresh-job
  rollback, completion reconciliation, exact recovered-record acknowledgement,
  failure retention, and unchanged retry of pressured serialized `Next` jobs;
- `reticulum-tx-supervisor` permanent ownership of node-core and all three TX
  machines, fresh checked clock samples for every lane, exact deadline/grace
  wake selection, phase-gated cancellation-safe waits, bounded 16-pass yields,
  retained fault gating, exact terminal/recovery observation and acknowledgement
  facades, and the RF-denying `RfInertTxPolicy`;
- `reticulum-storage-model` canonical accepted/transition/audit records,
  principal-scoped idempotency, poisoned complete replay, fixed-RAM lifecycle
  indexing, and opaque preflight/apply plans;
- `reticulum-submission-projector` volatile attempt correlation, the durable
  pre-preparation barrier, complete-frame metadata projection, conservative
  terminal/recovery/quarantine mapping, exact retry/readback handling, and
  independent persist-before-ack actions;
- stable-address/no-copy, pressure, cancelled-receive, crossed-reply,
  stale-token, delayed-reply, terminal-race, cumulative-authorization, and
  late-recovery tests;
- a host-only, manually stepped no-RF integration harness covering authorized
  and denied hops, exact-deadline expiry/recovery, deterministic two-interface
  fan-out, and terminal-before-authorization suppression across the real
  handoff ports;
- generic RISC-V and ESP32-S3 compilation; and
- exact handoff/dispatcher/supervisor dependency contracts plus dependency/
  feature guards that keep Tracker TX unavailable.

The physical power-fail-safe journal now implements exact append/readback,
lifetime reservation, complete integrity-validated replay, and resumable
retention-only compaction. The next product slice is the sole permanent storage
actor that turns projector requests into those journal operations and publishes
their exact outcomes without weakening the ordering contract.
Ordinary RNS tick/actions, RX ingress, submission handling, projection, and
acknowledgement must then join this aggregate under the sole node owner. The
handoff, dispatcher, supervisor, and projector remain outside every firmware
graph, and no driver or radio path consumes them.

The graph policy checks every current Tracker profile and the Cargo
`--all-features` closure, enforces exact reviewed dependency sets for node-core,
handoff, dispatcher, supervisor, storage-model and submission-projector, and
keeps the dispatcher, supervisor and projector outside every firmware graph.
Adding a feature-only transitive ownership path therefore fails before a new
firmware feature can bypass the reviewed list.

Still requires explicit antenna/load and regional authorization:

- any TX-capable Tracker BSP surface or firmware feature;
- CTX/FEM transmit sequencing, `SetTx`, CAD or TX IRQ handling;
- power, frequency, access and airtime policy selection;
- flashing a TX-capable image; and
- over-the-air, thermal, harmonic or split-frame TX HIL.

## Remaining protocol blocker

The caller-owned path currently covers locally prepared DATA only. It is not
linked into firmware, and no portable completion code claims that RF occurred.
Rete `NodeActions` still contains allocation-backed proof, announce,
forwarding, Link and Resource packets. A full bounded node needs a
caller-reservable outbound-action sink so those bytes are built transactionally
into the same fixed pool; wrapping the resulting `Vec` after protocol mutation
is not an equivalent no-copy or backpressure guarantee.
