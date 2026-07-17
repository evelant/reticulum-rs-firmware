# Owning async TX handoff

**Status:** portable route/permit/completion/recovery and owning Embassy DATA
handoff storage implemented and target-checked. The production ordinary path
uses interface-router job/completion queues plus the permit-only handoff and
per-actor permit server; the obsolete combined ordinary job/completion/permit
handoff has been removed. Production DATA jobs and completions also use the
interface router. `NodeInterfaceSupervisor` owns both coordinators and every
permit server, and the build-verified E290 target composes that aggregate with
the ticket-aware dispatcher and board owner in separate node and LoRa tasks.
**RF status:** the two attached `HT-RA62-HF` boards are antenna-equipped and
authorized for NA915 development TX. The isolated same-image E290 semantic HIL
passed; the permanent owner graph remains unflashed and unqualified

## Legacy DATA handoff decision

The original RF-inert DATA-machine tests established the following ownership
rules with a pool-sized job/return handoff. This topology remains in
`reticulum-tx-dispatch` for focused tests; it is not the production per-actor
job/completion path. The production graph uses the interface router for DATA
and ordinary owners and retains only the family-specific permit handoffs from
this crate.

Move unique `&'static mut TxPacketBuffer` references through two bounded
Embassy channels. Only a pointer crosses tasks: the fixed 500-byte packet does
not move or copy, and the radio actor can retain the unique buffer across an
`await` without borrowing node-core.

Node-core remains portable and Embassy-free. It owns attempt/dispatch metadata
but receives an externally owned buffer for preparation. The implemented
`reticulum-tx-handoff` crate retains the legacy DATA job/return topology plus
the production DATA and ordinary permit-only pairs. Tracker firmware does not
depend on that crate and gains no TX-capable radio type or feature from it. The
separate `reticulum-tx-dispatch` crate owns the legacy handoff roles in
persistent state and remains excluded from firmware. It has no executor, timer,
device-API, TX-capable radio driver/HAL, or pluggable byte-sink dependency and
cannot transmit. Node-core's transitive portable RX/framing edge supplies no TX
capability.

Within that crate, `NodeTxDataMachine` consumes the sole node-side job sender
and owner-return receiver. It validates the complete registered buffer pool at
boot, parks owners by stable slot, reconciles completions through node-core,
retains serialized continuation jobs, and synchronously prepares fresh DATA
from parked owners without exposing raw owners.

The legacy `TxSupervisor` owns one exact node-core, DATA machine, permit server,
RF-inert dispatcher, authorization policy, and monotonic clock contract for
focused no-RF tests. Its `RfInertTxPolicy` denies every RF authorization. New
firmware instead uses the synchronous `NodeInterfaceSupervisor` production
aggregate described in [Portable node/interface supervisor](tx-supervisor.md).

## Legacy DATA ownership topology

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

The production ordinary edge does not duplicate router ownership channels.
`OrdinaryPermitHandoff<M>` contains only the depth-one permit request/reply
channels split into one node-side `OrdinaryNodePermitHandoff` and one
actor-side `OrdinaryDispatcherPermitHandoff`. Ticketed `OrdinaryTxJob` and
`OrdinaryTxCompletion` values travel through the interface router's per-actor
queue, while `OrdinaryRouterCoordinator` retains its fixed packet pool.
`OrdinaryPermitServer` owns the node-side permit capability, authorizes once
per request, and retains the exact request/reply across cancellation and
pressure.

There is deliberately no second ordinary job FIFO. Permanent composition uses
the interface-router queues plus the permit-only split, leaving one
authoritative ticketed ownership path for ordinary packets.

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
a lost unique `&'static mut` runtime reference. Legacy `TxSupervisor::run()`
provides that test aggregate's never-cancelled async loop. It executes at most 16
immediately productive complete passes before yielding, yields again after
every selected wake, and its quiescent `select` includes only waits compatible with the current DATA, permit, and
dispatcher phases plus the next absolute deadline. Losing waits retain their
values in channels or persistent state. The task borrowing the aggregate for
`'static` must itself never be cancelled. No production firmware spawns this
legacy runner; the E290 node task schedules `NodeInterfaceSupervisor` instead.

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
generation. Each request also binds an exact opaque interface resource ID and
nonzero actor-defined units. Node-core does not interpret those units as RF
airtime, stream credits, or any other link-specific resource. Stale or foreign
control messages cannot expose a reused buffer.

Ordinary Rete action packets use a parallel owning family rather than the DATA
receipt typestates. `OrdinaryTxJob::begin_permit(requirements)` binds scope,
ordinary slot/generation, selected interface and the same exact opaque
interface-resource requirement vocabulary. A covering policy reservation is
the ordinary owner's
possibly-transmitted linearization point. Only
`OrdinaryAuthorizedTx::frame(now)` exposes the complete RNS packet once.
Ordinary typed completion preserves cumulative grant history across fan-out;
unpermitted and authorized cancellation are distinct phase-compatible paths,
and final `RouteComplete` wins when no interface remains. Pre-send cancellation
requires the exact retained request, while post-send ambiguity enters retained
quarantine. The interface router carries ordinary jobs and completions without
converting them into destination-DATA owners. `OrdinaryPermitHandoff` carries
only the matching request/reply pair. `NodeInterfaceSupervisor` owns the
ordinary coordinator and permit server, and the permanent E290 LoRa actor
consumes the corresponding actor roles.

Node-core preserves `PreparedData::target()` as project-owned `TxTarget`
metadata and resolves it synchronously against the request's snapshot of
enabled Reticulum packet interfaces. USB, BLE, and Wi-Fi device-API bearers
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

Owning bytes is not authorization to transmit. The RF-inert dispatcher first
proved the gate by splitting its `TxJob` into `PermitPendingTx` plus an opaque
non-`Copy` scalar `TxPermitRequest` before its only byte inspection. The
production `SoleRadioTxDispatcher` preserves that gate immediately before the
first irreversible hardware action. The node owner linearizes:

- stale, terminal, cancelled, expired, recovery-required, wrong-interface or
  policy-denied work: deny without touching radio TX;
- active work with a valid route, deadline, and covering interface-resource
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
candidate. The candidate includes the exact monotonic `now` sample used for
that authorization transition's deadline check, so a concrete actor policy can
make a time-based decision without owning or resampling the platform clock. It
also includes the selected interface, packet length, opaque resource ID, and
requested units. Authorization must return a same-resource reservation
covering those units; an unknown, mismatched, or under-sized reservation is
denied before the possibly-transmitted bit changes. An accepted
reservation is consumed at grant and is exposed through the grant and
`AuthorizedTx`; it is never refunded after a later ambiguous driver outcome.
The service retains the exact reply without reauthorizing
while its channel is full. Terminal, expired, recovery-required, or invalid
requests bypass policy. `NoRfTxDispatcher` keeps at most one exchange
outstanding, retains exact full or mismatched control values, returns the
pending owner as a recovery fault, and permanently disables itself on a
control-plane invariant instead of dropping either side.

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

Legacy `TxSupervisor` samples its monotonic source separately for its no-RF
DATA-machine lanes and combines that path's deadlines for its test runner. The
production `NodeInterfaceSupervisor` instead accepts a fresh monotonic sample
for each bounded pass, performs DATA maintenance, and fairly scans shared
completion, DATA, ordinary, and per-actor permit lanes. It exposes the earliest
node/DATA/ordinary owner deadline; the E290 node and LoRa tasks schedule that
portable work beside radio microsecond deadlines and permit-exchange recovery.

`lora-phy` 3.0.1 waits for DIO1 with SX1262 hardware TX timeout disabled and
warns against cancelling IRQ processing. It enables the RF switch and issues
`SetTx(0)` before that unbounded wait. MCU reset alone is therefore not an
accepted hung-radio recovery boundary. The E290 BSP now provides an
independently assertable SX1262 RESET, while the LoRa task bounds CAD, TX, RX and
BUSY waits and uses the dispatcher's cancellation recovery to return ticketed
owners. Generic actor failure still makes no claim that hardware was shut down;
MCU reset recovers volatile software state only.

Reset discards volatile references and native receipts. Higher-level durable
LXMF/submission records must reconstruct fresh attempts under a new
`NodeInstanceId`; leases are never persisted. If reset follows airtime
authorization, charge the entire reservation conservatively so reboot cannot
reset regulatory accounting.

## Implemented boundaries and current product composition

The legacy no-RF path remains useful and host/target-testable:

- external-buffer node-core ownership, deterministic serialized fan-out,
  opaque permit state, one-shot authorized byte access, completion, deadlines,
  and recovery diagnostics;
- pool-depth DATA job/return channels plus cancellation-safe permit pairs;
- `reticulum-tx-dispatch`, `NodeTxDataMachine`, and legacy `TxSupervisor`
  ownership retention across pressure, cancellation, late replies, and exact
  deadline recovery; and
- a manually stepped no-RF harness covering authorization, denial, recovery,
  two-interface fan-out, and terminal races.

The production path now uses different composition edges:

- `InterfaceFabric` supplies one ticketed DATA/ordinary job and completion queue
  plus a stationary sealed-ingress pool for each actor slot;
- `DataRouterCoordinator` and `OrdinaryRouterCoordinator` retain their distinct
  exact owners and share only the authoritative router;
- `DataPermitHandoff` and `OrdinaryPermitHandoff` contain the permit-only pairs
  consumed by one server and one actor each;
- `NodeInterfaceSupervisor` permanently owns node-core, the router, both
  coordinators, every permit server, and the shared authorization policy;
- `reticulum-radio-tx-dispatch` retains the selected actor's exact ticket across
  permit negotiation, CAD, RNode transmit, cancellation recovery, and
  completion return over one `SoleRnodeRadio`; and
- the E290 target composes that portable node aggregate and one concrete LoRa
  actor as separate permanent tasks.

The graph policy admits that reviewed E290 product edge while continuing to
keep the RF-inert legacy dispatcher out of firmware and preserving the
Tracker's TX-free default/receive-only profiles. Adding an unreviewed platform,
driver, board, storage, API, or ownership dependency still fails the exact
graph checks.

The attached E290 boards have antennas, confirmed `HT-RA62-HF` modules, and
explicit NA915 development authorization. The isolated semantic image passed
its functional two-board HIL, but the permanent image remains unflashed.
Permanent-graph interoperability, electrical/RF behavior, fairness, watchdog,
and soak evidence therefore remain open. Durable identity plus a resident operation-scoped
storage coordinator are now composed. The coordinator owns the sole flash
backend, drives the mounted submission runtime from the node task, and borrows
an exact bound journal view only for each physical operation. The LoRa
dispatcher also composes a bounded authorized-frame request/durable-echo
handoff that retains every post-byte-exposure DATA completion and router ticket
until the runtime has durably projected the exact observation. If the journal
cannot mount or recover at boot, before such an owner exists, local durable
submission is disabled while route-only LoRa continues. A permanent storage
failure with an unresolved gated DATA owner instead enters ADR 0005's
interface-local `ActiveOwnerFailStopped`: the exact frame/completion/ticket stay
retained, the same LoRa lease goes offline without a generation change, and no
fresh LoRa work runs for the rest of the boot. Dispatcher coverage proves that
an acknowledgement-gated DATA owner excludes a queued ordinary job, RX and
completion, while request pressure, mismatch and cancellation keep exact owners.
Local submission, client delivery, and an external authenticated device-API job
lane/bearer are not yet composed into this first LoRa image.

## Remaining admission and qualification blocker

The permanent firmware now routes locally generated announces, ingress actions,
and other ordinary RNS packets through fixed owners to the LoRa actor. The
ordinary boundary still starts after Rete has created an allocation-backed
`NodeActions` envelope and mutated protocol state. Its fixed owner and bounded
router therefore provide exact downstream ownership and pressure but do not yet
provide caller-reservable construction or upstream backpressure before that
mutation. Local DATA/LXMF intent admission remains a separate product blocker;
the first E290 image has no local DATA submission surface.

The durable profile permits one accepted-history entry solely for composition
qualification while exposing no external admission lane; that cap is not a
product-capacity commitment. At the terminal radio-operation boundary, the LoRa
actor retains the exact
`AuthorizedFrameObservation`, completion and router ticket. The bounded,
transport-neutral request/reply handoff moves a copy to the resident submission
runtime, which retains and re-offers it, persists the observation, and returns
the identical durable acknowledgement; only then may the actor retire the
completion. Cancellation recovery offers the same retained observation before
completion is drained. A permanent persistence failure cannot produce that
echo: ADR 0005 instead fail-stops only the affected LoRa actor for the boot.
LoRa is the first and primary producer of this contract; later USB, Wi-Fi, BLE,
or radio Reticulum interfaces add their own actors behind the same seam rather
than inheriting LoRa-specific mechanics.
