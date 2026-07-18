# Portable node/interface supervisor

**Status:** `NodeInterfaceSupervisor` is the permanent portable protocol-owner
aggregate. It owns the sole `NodeCore`, authoritative interface router, DATA
and ordinary coordinators, one DATA and ordinary permit server per actor, and
the shared authorization policy. The first concrete consumer is the
LoRa-first, three-task node/LoRa/USB Heltec Vision Master E290 firmware graph. Its host,
portable-target, ESP32-S3 build and review gates pass. Source `96e38aa` passed a
bounded powered smoke on both `HT-RA62-HF` boards: exact image readback,
erased-credential/journal/LoRa startup, and two ordinary one-frame TXs each.
Controlled peer RX/DATA, fairness/faults, high-water, and full powered
qualification remain open; the separate semantic image supplies the controlled
radio/RNode/Rete HIL.

The older `TxSupervisor` is retained only as a legacy no-RF test aggregate for
the original DATA-machine ownership path. It is not the production composition
and does not define how future interfaces are added.

## Permanent ownership boundary

`reticulum_tx_supervisor::NodeInterfaceSupervisor` consumes and permanently
owns:

- one exact `NodeCore` and its node-instance ownership scope;
- one `InterfaceFabric`, including the authoritative registry and bounded
  job, completion, and ingress queues;
- one `DataRouterCoordinator` with its complete registered DATA buffer pool;
- one `OrdinaryRouterCoordinator` with its independent ordinary buffer pool;
- one DATA and one ordinary permit-only handoff for every interface actor slot;
  and
- one shared `TxAuthorizationPolicy` implementation.

Checked construction rejects zero actor slots, mismatched node ownership, or
invalid interface/permit composition without silently discarding a non-copy
owner. On success it returns the aggregate plus one `NodeInterfaceActorPorts`
capability for each actor slot. Each actor capability can then be split into
TX and ingress roles without creating a second registry authority.

The aggregate is intended to remain in static storage and be owned by one
never-cancelled node task for the boot lifetime. Dropping and reconstructing it
is not an owner-recovery mechanism.

## Sealed fixed-pool ingress

Ingress is queue-only. An interface actor obtains an
`AvailableIngressBuffer` from its own fabric queue, writes only the initialized
native RNS packet prefix, seals it as a `SealedIngressPacket`, and submits that
exact owner through its queue-bound ingress capability. There is no public
runtime path that wraps an arbitrary payload with a caller-selected interface
ID.

`step_ingress()` performs one bounded transition:

1. retry an exact buffer recycle retained behind a full return queue;
2. expose a non-retryable exact buffer owner until firmware takes it;
3. expose a retained terminal action owner until firmware takes it;
4. retry an action envelope retained because the ordinary coordinator was
   busy;
5. otherwise receive one sealed actor packet;
6. validate its queue origin, current interface lease, online state, initialized
   length, and registered logical MTU;
7. synchronously pass its bytes and registry-derived interface provenance to
   `NodeCore`; and
8. return the exact buffer to the same actor pool, retaining it for retry only
   if the return queue is full and otherwise exposing it as terminal residue.

The RX pool therefore remains stationary: queue depth fixes the number of
buffers assigned to each actor, and neither protocol processing nor action
backpressure creates replacement owners.

Actions returned by successful ingress are admitted to the owned ordinary
coordinator. `OrdinaryRouterOfferError::Busy` is the only retryable action
failure; the aggregate retains that exact envelope and retries it on a later
bounded ingress step. Aggregate faults, a disabled coordinator, and an
envelope larger than the configured ordinary pool are terminal. They retain an
exact `NodeInterfaceTerminalIngressActions` residue, report its typed
`NodeInterfaceIngressActionFault`, and require firmware to take the owner for
quarantine or fail-stop handling. Terminal faults are never mislabeled as
transient pressure.

Queue-origin rejection, node receipt-correlation rejection, action status, and
buffer-recycle pressure are all explicit `NodeInterfaceIngressStep` results.
Firmware must inspect both action and recycle status; processing a packet does
not imply that every resulting owner has already reached its next pool.
Only a full actor return queue is retryable. A crossed queue, slot or fabric
origin instead retains the exact sealed buffer as typed terminal residue for
firmware to take and quarantine while it drains already-admitted work.

## DATA and ordinary outbound ownership

The two packet families remain distinct even though they share the interface
router and product authorization policy.

`DataRouterCoordinator` owns the registered external DATA buffers used by
destination sends and receipt tracking. It prepares through the sole
`NodeCore`, retains each exact attempt owner, routes a ticketed DATA job to the
selected actor, and reconciles ticket-bound completion back into node-core.
Recovered owners remain unavailable until their exact durable disposition can
be acknowledged.

`OrdinaryRouterCoordinator` atomically moves complete allocation-backed
`NodeActions` envelopes into its registered fixed pool. It derives the enabled
interface set from the same authoritative router at admission time, serializes
`Only`, `All`, and `AllExcept` fan-out, and returns each exact ordinary owner to
the pool only after its ticketed completion is reconciled. Events, unroutable
counts, rejected envelopes, recoveries, and quarantines remain typed output;
they are not silently dropped.

DATA and ordinary ticketed jobs and completions use the shared interface
router. Each actor's DATA and ordinary permit exchange instead uses its own
depth-one permit-only pair. A permit server retains the exact request or reply
across pressure and calls the shared policy once for that request. Only a
matching grant permits the actor to expose packet bytes through the family's
one-shot typestate. A coordinator fault stops fresh authorization but preserves
the forced-denial path needed to return already-issued actor owners safely.

A stale registry generation does not discard a completion. The router converts
it into explicit node recovery while retaining the exact DATA or ordinary
owner, and the supervisor demultiplexes it back to the matching coordinator.
An unexpected coordinator rejection latches an aggregate fault with a
copy-only binding for the exact retained completion.

## Fair bounded progression

`step(now)` first performs DATA owner maintenance once, then scans the
following orchestration lanes with a persistent round-robin cursor:

- shared interface completion intake;
- DATA coordinator progression;
- ordinary coordinator progression;
- one DATA permit server per actor; and
- one ordinary permit server per actor.

At most one useful ownership transition is selected per pass. Idle, pressured,
or disabled lanes do not end the scan, and the cursor advances after both
progress and a completely idle pass. A full LoRa actor queue therefore cannot
indefinitely hide another ready lane, and a coordinator-local fault does not
prevent an already-issued permit request from reaching its forced-denial drain
path.

The aggregate exposes the earliest DATA, ordinary, or node-owner deadline plus
copy-only capacity, permit-phase, ingress-residue, and coordinator-fault
status. The permanent firmware task owns scheduling around this synchronous
surface, including protocol seconds, ingress polling, actor completion
capacity, radio microsecond deadlines, and executor yielding. The portable
aggregate does not own a radio or impose LoRa scheduling on a future non-radio
actor.

Local announce flush and protocol ticks also admit every returned packet action
through the owned ordinary coordinator. Their typed failure results retain the
complete exact action envelope, so the caller must retry, reject, quarantine,
or fail-stop explicitly.

## First concrete firmware graph

The Heltec Vision Master E290 node target is the first concrete permanent
composition. It deliberately contains two long-lived tasks:

- a transport-neutral node task owning `NodeInterfaceSupervisor`, node timers,
  action drain/retry, and interface online state; and
- one LoRa actor task owning timed RNode receive/reassembly, the ticket-aware
  `SoleRadioTxDispatcher`, CAD/backoff, exact airtime permit requirements, and
  the E290 HT-RA62/SX1262 radio.

The two tasks exchange only the actor capability returned by the shared
interface fabric, ticketed jobs/completions, sealed ingress buffers, and
permit-only messages. LoRa is the first and primary complete transport slice.
The node/router seam is transport-neutral so a later USB, Wi-Fi, BLE, Ethernet,
or other packet actor can receive its own registry slot and bounded pools, but
those actors are intentionally deferred. They will implement their native link
mechanics and will not emulate CAD, airtime reservations, RNode fragmentation,
or another radio abstraction.

The E290 image is not yet the full appliance. Its mirrored durable identity and
restart-safe announce epoch are boot-gated ahead of node/radio service; the pre-
authentication initialization/live-pairing edge and authenticated API/session
serving are composed. One powered permanent-graph submission now reaches LoRa
peer proof and durable terminal status after USB re-enumeration. Message storage,
local LXMF submission/delivery, optional clients, broader reset/power-cut, and
full LoRa qualification remain later work.

## Legacy no-RF aggregate

`TxSupervisor` and its async `run()`/`wait_for_work()` surface predate the
shared interface-router composition. They remain useful for focused tests of
the original external-buffer DATA machine, permit grace, fresh clock samples,
deadline recovery, and exact-owner behavior under an always-denying
`RfInertTxPolicy`.

That type does not own the production `OutboundRouter`, both coordinator
families, all per-actor permit servers, or a concrete actor. Its no-RF
dispatcher has no hardware or pluggable byte sink. New firmware must compose
`NodeInterfaceSupervisor`; documentation and code must not infer permanent
runtime behavior from the legacy runner.

## Durability and remaining integration

This crate defines volatile protocol ownership, not powered durability.
`reticulum-submission-projector`, `reticulum-storage-actor`, and the physical
journal own persist-before-accept and persist-before-ack semantics. The E290
node task still needs to host and boot-gate that storage path, project terminal,
recovery, and quarantine observations, and expose the authenticated device API.

Other remaining product work includes powered identity/announce-clock
qualification, local LXMF intent admission and event delivery,
Links/Resources hardening, regional
release policy, memory/stack/soak qualification, and additional transport
actors after the LoRa vertical slice is stable.

## Validation

Use the package's focused host and portable-target gates without relying on a
fixed test count:

```sh
cargo test --locked -p reticulum-tx-supervisor
cargo clippy --locked -p reticulum-tx-supervisor --all-targets -- -D warnings
cargo check --locked -p reticulum-tx-supervisor \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
```
