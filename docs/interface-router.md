# Transport-neutral Reticulum interface registry and router

Status: implemented portable seam with bounded exact-owner TX queues, bounded
stationary RX-buffer pools, generation-bound actor lifecycle handshakes, and
cancellation-safe actor/router waits. The
`NodeInterfaceSupervisor` owns the node, router, DATA and ordinary
coordinators, and per-actor permit servers. The ticket-aware radio dispatcher
owns only the TX half of one actor capability; the concrete LoRa task retains
the independent RX half. The first permanent E290 firmware graph now composes
those halves in separate node and LoRa tasks. Its build-only gates pass; powered
permanent-graph smoke now verifies boot, interface-online state, and ordinary
one-frame TX on both boards. Controlled RX/DATA, fairness/faults, and full
qualification remain open. The separate same-image E290 semantic HIL has passed
the functional radio/RNode/Rete path.

`reticulum-interface-router` is the smallest common boundary needed before a
second Reticulum interface is added. LoRa is the first and primary complete
transport vertical slice, but neither the registry nor the router contains
RNode framing, SX1262 state, CAD, frequency, region, RF airtime, USB, BLE or
Wi-Fi policy. No USB, BLE, or Wi-Fi Reticulum actor is being implemented during
the LoRa slice.

## Ownership and routing

Node-core remains the sole owner of Reticulum routing semantics. It resolves
`Only`, `All` and `AllExcept` against one synchronous enabled-interface
snapshot, selects exactly one hop, and emits a DATA or ordinary job whose
`job.interface()` is authoritative. The outbound router does not reinterpret
the target or parallelize fan-out. It:

1. looks up that selected `PacketInterfaceId` in the fixed registry;
2. rejects unknown, offline or over-MTU work without consuming its exact owner;
3. stamps the current queue-local lease and property snapshot;
4. moves the unchanged owner into only that queue's bounded actor handoff; and
5. returns an exact ticket-bound completion to node-core.

After node-core reconciles a completion, it alone may produce the next
serialized fan-out job. Host coverage proves an `All` DATA packet moves first
through interface 2, returns to node-core, and only then reaches interface 9 as
`Next`. A source-relative ordinary proof proves a native `Only(9)` action is
visible only to interface 9's actor.

Pressure never implies ownership loss. A full job queue returns the original
DATA or ordinary job. A full completion queue returns the exact completion.
Crossing one actor's completion through another actor capability is rejected
with the envelope intact.

The separately constructible `OrdinaryRouterCoordinator` in
`reticulum-tx-supervisor` now
exercises this contract for allocation-backed RNS actions. It copies an
atomically admitted envelope into its own fixed ordinary-packet pool, routes
one node-core-selected hop at a time, and retains the same slot and generation
until a ticketed completion either produces the next `All`/`AllExcept` hop or
returns the buffer to the pool. It derives the enabled-interface snapshot
fresh from `eligible_interfaces()` during each admission attempt; callers can
supply an envelope deadline, but cannot inject a stale enabled set or owner
clock sample. This coordinator is owned beside the DATA coordinator by the
transport-neutral `NodeInterfaceSupervisor`; concrete interface actors remain
outside that sole node-owner aggregate.

## Cancellation-safe actor and router waits

The immediate APIs remain available for bounded synchronous loops. The router
also exposes advisory async wake surfaces without transferring ownership early:

- `InterfaceTxActorHandoff::poll_receive_job()` and `receive_job()` wait for one
  exact DATA or ordinary owner on that actor's queue;
- `InterfaceTxActorHandoff::poll_completion_capacity()` and
  `wait_completion_capacity()` wake an actor that is retaining an exact
  completion behind a full return queue without moving or reserving it;
- `InterfaceIngressActorHandoff` acquires exact reusable RX buffers and submits
  sealed native packets; its buffer and send-capacity waits move no owner while
  pending;
- `OutboundRouter::poll_receive_ingress()` and `receive_ingress()` fairly wait
  across every completed-packet queue and validate the observed queue, static
  fabric origin, and current registry lease;
- `OutboundRouter::poll_route_capacity()` and `wait_route_capacity()` report a
  currently invalid registry route immediately and otherwise wait for the
  selected queue to have capacity; and
- `OutboundRouter::poll_receive_completion()` and `receive_completion()` wait
  across all actor completion queues in bounded round-robin order and perform
  the same lease validation as `try_receive_completion()`.

Pending polls register wakers only. They neither reserve queue capacity nor
remove an owner, so dropping a pending future leaves the job or completion
queued. Capacity readiness is advisory: the subsequent `try_route_data()` or
`try_route_ordinary()` call revalidates generation, online state, MTU, and
capacity while moving the exact owner.

The ordinary coordinator adds `poll_route_progress()` and
`wait_route_progress()` over all of its locally retained jobs. Its round-robin
route cursor and bounded `step()` continue past one pressured queue so a full
queue reserved for the primary LoRa actor cannot starve a ready packet
for another interface. The permanent node task fairly interleaves the
aggregate's bounded TX/permit pass with its bounded ingress pass. A later
combined wait-set can replace the first product loop's short polling without
changing ownership.

## Authoritative fixed registry

Every slot records:

- stable one-byte `PacketInterfaceId`;
- fixed `InterfaceQueueId`;
- non-repeating queue-local generation;
- online state;
- logical native-packet MTU;
- opaque actor configuration identity;
- optional nonzero advertised bitrate; and
- scalar relative cost.

Bitrate and cost are metadata only in this slice. Route selection remains with
RNS/node-core. `eligible_interfaces()` is the sole source for node-core's
current `InterfaceSet`; firmware must not maintain another enabled-ID list. It
checks every registration and fails explicitly for an ID above the current
compact 0-through-63 profile, even while that registration is offline.

Online-to-offline transition rejects new jobs but does not invalidate an owner
already accepted by the queue. Reconfiguration changes MTU/configuration and
advances the generation. A completion carrying the superseded lease becomes a
typed stale observation while retaining its exact node owner. The permanent
runtime must use `into_node_recovery()` and reconcile that completion through
the matching DATA or ordinary owner; stale interface attribution is not a
license to strand the packet buffer.

An actor must execute an accepted owner under the configuration identity in
that owner's stamped context. If it cannot preserve an old configuration after
a registry update, composition must quiesce/drain the queue before updating or
return the owner unpermitted/recovery-faulted; it must never transmit old bytes
under an unrelated new configuration.

## Generation-bound actor lifecycle

Every fabric slot also owns independent depth-one lifecycle request and
acknowledgement channels. The actor capability's three-way split returns a
non-cloneable `InterfaceLifecycleActorHandoff` beside the TX and RX
capabilities. A concrete actor reports `Ready` only after transport-specific
initialization succeeds and drives `Offline` to an acknowledged result before
entering terminal retention. A retained or rejected exchange is resumed and
retried without returning to transport operations. Each request carries the
exact registry lease; it cannot invent a new interface identity or generation.

`OutboundRouter::try_process_lifecycle()` polls actor slots fairly, treats the
observed channel as authority, validates the request's queue and current
generation, applies the online bit, and synchronously returns an exact typed
acknowledgement. Crossed queues and stale generations are acknowledged as
rejections without changing eligibility. If the actor's acknowledgement
channel is already occupied, the observed request remains queued and unapplied.
Ready and Offline are idempotent under one current lease.

The production aggregate registers every slot offline and exposes no direct
enable operation. Only the queue-bound lifecycle capability can make that
generation eligible; node-owned policy has a separate disable-only operation.
That also means a replacement generation begins offline if an old actor's
terminal report is rejected as stale.

`NodeInterfaceSupervisor::step()` treats lifecycle as a pre-routing gate. After
DATA-owner maintenance, it services a pending lifecycle report before entering
the round-robin completion, coordinator, and permit scan. Lifecycle is not one
more lane in that scan: fairness applies inside the router's lifecycle cursor,
which rotates among actor request queues. This ordering prevents an actor that
has reported Offline from receiving another fresh owner merely because the
supervisor's orchestration cursor was positioned at a routing lane.

Offline changes only admission of fresh routed work. Already accepted jobs,
completions, and completed ingress remain valid under the unchanged generation.
The actor lifecycle handoff enforces one exchange in flight. Cancelling an
acknowledgement wait preserves that pending exchange so the actor can resume
it; an attempted second request is rejected with both the pending and unsent
states still explicit.

That preservation is deliberately not automatic failover. In the graceful
case an actor may go Offline and still legitimately return an already accepted
owner; node-core can then continue serialized fan-out through another online
actor. A terminal E290 fail-stop instead retains any owner whose exposure or
outcome is ambiguous, so that same attempt cannot automatically advance. The
Offline transition excludes the failed actor only from fresh attempts. A
future terminal-drain/revocation protocol must distinguish safely returnable,
unstarted queued work from exposed or otherwise ambiguous ownership before the
former can be recovered without weakening the latter's retention rule.

Actor liveness and product administrative policy currently meet at the same
online bit. The E290 actor emits Ready once at startup, so later durability
policy offlining cannot be undone by that actor. A future restartable actor or
operator-disable feature must represent administrative enablement separately
rather than treating a repeated Ready report as policy authority.

## Ingress provenance

Ingress uses the same capability boundary. Every fabric slot contains exactly
`QUEUE_DEPTH` stationary `PACKET_CAPACITY` buffers. An actor receives one
`AvailableIngressBuffer`, initializes a prefix, and consumes it into an
immutable `SealedIngressPacket`. It can bind a registry-issued descriptor only
when its fixed queue matches, and can submit a sealed owner only when the
authority, buffer queue, and static fabric origin all match its capability.
Callers cannot supply an arbitrary interface ID or replace the backing owner.

The router dequeues completed packets in bounded round-robin order, checks the
observed queue, permanent buffer origin, static fabric identity, and current
lease generation and logical MTU, then produces non-generic `ValidatedIngress`
that always owns a `SealedIngressPacket`. An
online-to-offline transition does not invalidate RX already completed under
the same generation; reconfiguration does. `NodeInterfaceSupervisor::step_ingress()`
is the production node entry point. It processes bytes synchronously, recycles
the exact sealed owner even on stale provenance or receipt-correlation failure,
and retains both action-admission pressure and unexpected recycle pressure
before dequeuing another packet. The generic direct-ingress shortcut is not a
public runtime API.

Future stream/datagram actors can use the same complete-native-packet pool and
provenance contract without acquiring LoRa framing, CAD, or RF policy.

## Protocol path-generation boundary

The lease closes asynchronous actor correlation, not RNS path provenance.
Rete currently retains only `received_on: u8` for a learned path; it does not
retain this registry's generation. Reusing the same `PacketInterfaceId` after a
disconnect or material reconfiguration can therefore make a newly prepared
DATA packet follow a path learned under the old interface incarnation.

Before activating a new generation under a reused ID, the permanent node owner
must invalidate every path learned on that ID, or reconstruct the node from a
policy-approved state that omits those paths. Until the RNS adapter exposes and
tests that invalidation operation, production composition must keep interface
identity/configuration immutable for one node lifetime (or restart the node on
change). Registry generation alone must not be presented as solving this
protocol-state problem.

## First concrete composition

The first permanent composition puts the E290 LoRa/RNode radio owner behind one
actor queue without moving LoRa requirements into this crate. It replaces the
former pair of product-global startup signals with the queue-bound Ready/
Offline request and acknowledgement exchange. The implemented
radio dispatcher accepts the queue's ticketed DATA/ordinary union and retains
each exact ticket across permit negotiation, RNode framing, CAD, transmit, and
completion return. It executes only under the job's stamped configuration
identity, and the LoRa task explicitly schedules RX or dequeues TX so continuous
egress cannot make mesh receive impossible. Its cancellation-safe
completion-capacity wait retains the exact completion.

The DATA and ordinary coordinators and their per-actor permit-only services are
implemented and owned by `NodeInterfaceSupervisor`. There is no second
ordinary job FIFO beside this router. The E290 target now places that sole
node-owner aggregate and the E290 LoRa actor in two permanent tasks. Remaining
work is powered qualification of their RX/TX fairness, watchdogs, and
interoperability. The physical modules are confirmed and the permanent image's
bounded two-board boot/ordinary-TX smoke passed, but it did not control peer RX
or DATA.

LoRa is the first complete transport target and the current implementation
priority. USB, BLE, and Wi-Fi remain future actors; there is no implementation
of any of them to compose today. The authenticated device API remains a
separate application boundary and does not become a Reticulum interface
implicitly.

## Validation

The focused interface-router suite contains 26 host tests. In addition to exact
actor selection, fan-out, queue pressure, lifecycle, and cancellation coverage,
it tests lifecycle fairness, acknowledgement pressure, crossed/stale lifecycle
rejection, native ingress round-trip/reuse, length bounds, two-slot fairness,
online-to-offline acceptance, stale-generation recycling, crossed authority,
queue and static-fabric rejection, and pressure/cancellation-safe ingress waits.

```sh
cargo test --locked -p reticulum-interface-router
cargo clippy --locked -p reticulum-interface-router --all-targets -- -D warnings
cargo check --locked -p reticulum-interface-router \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-interface-router \
  --target xtensa-esp32s3-none-elf
```
