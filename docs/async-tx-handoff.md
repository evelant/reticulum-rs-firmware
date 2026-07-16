# Owning async TX handoff

**Status:** portable route/permit/completion/recovery and owning Embassy
handoff storage implemented and target-checked; dispatcher actor next; no
firmware TX graph
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
feature as part of this slice.

## Ownership topology

Allocate zero-filled buffers in `.bss` with `ConstStaticCell`, then place each
unique reference into the available/completion channel before tasks start:

```text
available/completion channel
    -> node-local reservation and RNS preparation
    -> jobs channel
    -> one interface dispatcher across await
    -> available/completion channel
```

The node actor is the sole consumer of available/completed buffers and the
sole producer of jobs. One dispatcher owns every enabled packet interface and
has the inverse channel roles; it serializes each route and invokes the
selected interface internally. Independent per-interface actors would instead
require per-interface queues or a routing dispatcher in front of them, because
multiple consumers of one jobs queue could take work for the wrong interface.
Both owner-channel capacities equal the pool size. The separate permit-request
and permit-reply channels each have depth one, matching the single serialized
dispatcher. While one endpoint owns a buffer, its outbound owner channel can
contain at most every other buffer; a send is therefore capacity-infallible if
the endpoints remain encapsulated. An unexpected full result returns the exact
non-`Copy` value in `ChannelFull<T>` and becomes an invariant fault, never a
drop or duplication.

`TxHandoff::split(&'static mut self)` consumes the unique reference normally
obtained from `ConstStaticCell` and creates one `NodeHandoff` plus one
`DispatcherHandoff`. Their individual port capabilities are non-`Clone`,
`must_use`, and require `&mut self` for every send or receive operation. Raw
Embassy channels/senders/receivers, `clear()`, and owner-taking async sends are
not public. Only receiving may await; all four send directions use
non-awaiting `try_send` and return the unchanged value on pressure.

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
   | Recovered { buffer, record }
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

The node actor first removes one free unique buffer from the available channel,
then calls a portable API equivalent to:

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

Pool exhaustion is rejected before entropy or RNS mutation. If
`NodeHandoff::jobs.try_send` reports full, the caller still owns the `TxJob`
returned by `ChannelFull::into_inner()` and calls `rollback_queued(job, now)`.
Before its deadline and before any prior
authorization this cancels the exact receipt; at or after the deadline it
enters and finalizes exact-owner recovery, returning `Recovered`. It must never
use a cancellable `send(job).await` that can lose the owner future.

## Authorization boundary

Owning bytes is not authorization to transmit. Immediately before the first
irreversible hardware action, the future interface dispatcher must split its
`TxJob` into `PermitPendingTx` plus an opaque non-`Copy` scalar
`TxPermitRequest`. The node owner already linearizes:

- stale, terminal, cancelled, expired, recovery-required, wrong-interface or
  policy-denied work: deny without touching radio TX;
- active work with a valid route, deadline, regional profile, and airtime
  reservation: change `Routed -> Authorized`, set `may_have_transmitted`, and
  issue a generation-bound non-`Copy` permit.

Permit requests and replies use separate depth-one scalar channels; they
never enter either buffer-owning channel or affect its capacity proof. A
request binds owner, node instance, packet slot, dispatch generation,
interface, and hop generation. Permit issuance is one single-owner transition
and is irrevocable: after it succeeds, the dispatch is conservatively
classified as possibly transmitted even if the actor later reports a driver
error or misses the deadline. The handoff returns exact full-channel values for
requests, replies, and cooperative owner returns. The future dispatcher actor
must keep at most one permit exchange outstanding, retain full or mismatched
control values, and disable TX on a control-plane invariant instead of
dropping either side.

`PermitPendingTx::resolve(reply, now)` rejects a mismatched reply while retaining
both owners. A grant resolved at or after its deadline becomes
`ExpiredAuthorizedTx`: it exposes no bytes but remains possibly transmitted
because issuance already won the race. Before the deadline, only
`AuthorizedTx::frame(now)` can borrow the encoded packet. That accessor is
one-shot and also rejects the exact deadline (`now >= deadline`).

A terminal committed before authorization suppresses transmission. Once
authorization wins the race, RF may occur; later proof/timeout state remains
retained and terminal acknowledgement remains blocked until the unique buffer
returns.

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
node-core finalizes that record and returns `Recovered { buffer, record }`; the
buffer is then reusable. A same-lease metadata mismatch or reported recovery
fault instead returns an owning `TxQuarantine` and keeps the scalar record
fail-closed. A foreign or stale completion is rejected intact, not reclaimed.

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
- stable-address/no-copy, pressure, cancelled-receive, crossed-reply,
  stale-token, delayed-reply, terminal-race, cumulative-authorization, and
  late-recovery tests;
- generic RISC-V and ESP32-S3 compilation; and
- an exact handoff dependency contract plus dependency/feature guards that keep
  Tracker TX unavailable.

The next implementation slice is the sole dispatcher actor using the frozen
portable transitions and handoff capabilities. The handoff remains outside
every firmware graph; no actor or radio path consumes it yet.

The graph policy checks every current Tracker profile and the Cargo
`--all-features` closure for both `reticulum-node-core` and
`reticulum-tx-handoff`. Adding a feature-only transitive ownership path
therefore fails before a new firmware feature can bypass the reviewed list.

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
