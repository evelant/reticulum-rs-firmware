# ADR 0005: Interface-local fail-stop for an active DATA durability fault

- **Status:** accepted
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0003](0003-lora-first-interface-fabric.md), [ADR 0004](0004-sole-flash-coordinator.md)

## Context

The LoRa dispatcher retains a destination-DATA completion, its one-shot
interface-router ticket, and the exact authorized native-frame observation
after packet bytes have been exposed. The node/storage owner echoes that
observation only after the submission projector proves its corresponding
record durable. Until the exact echo arrives, the dispatcher cannot return the
completion or select another owner.

An ambiguous backend result does not establish permanent failure. The storage
actor retains its exact mutation, and a later identical retry can prove the
record appended or already equivalent. Busy projector/actor state likewise
requires unchanged retry. In contrast, a binding failure, unavailable runtime,
latched storage/projector fault, or non-retryable correlation failure cannot
produce the durable evidence needed by the active frame during the current
boot.

The current sole-radio dispatcher has one active family and one active
completion ticket. Continuing ordinary LoRa transmission would require either
returning the DATA completion without durability, abandoning the exact owner,
or moving the DATA completion and ticket into a second parked-owner machine.
The first two choices violate existing safety contracts; the third is a real
ownership redesign rather than a failure-policy shortcut.

## Decision

### Retry ambiguous work without a timeout

Backend ambiguity and recognized serialization pressure enter `Retrying`.
The runtime retains the exact operation and the node retains any exact frame.
Physical storage work observes the product retry backoff, while projection-only
frame offers may continue to report `Retain`. No retry count or elapsed timeout
fabricates durability or changes owner classification.

### Degrade only local durable service before an owner is active

A permanent durability failure with no unresolved authorized frame enters
`DisabledRouteOnly`. Local durable admission and runtime driving remain closed,
but the LoRa interface may continue route-only operation because no completion
is gated on the unavailable store.

An exact frame whose projection was already proven durable is different from
an unresolved frame. If its acknowledgement is merely waiting for channel
capacity, the node still sends that identical acknowledgement even if an
unrelated later storage failure disables service.

### Fail-stop the affected interface with an unresolved owner

A permanent failure with an unresolved authorized DATA frame enters
`ActiveOwnerFailStopped` for the remainder of the boot. The E290 node:

- retains the exact frame without acknowledging it;
- leaves the completion and router ticket in the dispatcher;
- marks the same LoRa registry lease offline without changing its generation;
- stops fresh ingress, protocol tick, local announce, and local submission
  admission work; and
- continues only bounded fail-closed drainage that does not require releasing
  the gated LoRa owner.

The policy also covers the scheduling race in which storage fails after the
node scans an empty frame-request channel but before the dispatcher queues its
request. A frame received while service is already `DisabledRouteOnly`
promotes the state to `ActiveOwnerFailStopped`.

There is no acknowledgement timeout and no automatic reboot. An external reset
or power failure is handled by the existing conservative boot-recovery model,
but reset is not used as a same-boot substitute for returning an exact ticket.

### Keep the failure interface-local

The decision concerns the actor that owns the gated completion. A future node
with independent USB, Wi-Fi, BLE, Ethernet, or second-radio packet actors may
continue those interfaces if their own ownership machines remain healthy.
The initial E290 profile has only one LoRa actor, so its mesh packet service is
effectively fail-stopped in this state.

## Consequences

- Persist-before-ack and every existing non-`Copy` owner invariant remain
  unchanged.
- Ordinary LoRa TX and RX do not resume in the same boot after an active-owner
  permanent fault. Permitting RX alone would not provide forwarding and would
  create additional work behind an actor that cannot transmit.
- Jobs already accepted by the actor remain retained. Marking the lease offline
  prevents new routing but deliberately does not stale or reclassify those
  owners.
- Route-only degradation remains available for boot-time optional-journal
  failure and permanent runtime failure before an active DATA owner exists.
- Operational health must distinguish `DisabledRouteOnly` from
  `ActiveOwnerFailStopped`; neither may be logged as successful durable service.
- A later same-boot recovery design must add a bounded separately parked
  completion/ticket owner plus durable incident or quarantine semantics. It
  cannot be introduced by weakening the current echo gate.

## Implementation and qualification

The E290 product exposes a host-testable four-state policy:

- `Ready`;
- `Retrying { retry_not_before_ms }`;
- `DisabledRouteOnly`; and
- `ActiveOwnerFailStopped`.

Host tests cover retry deadlines, no-owner degradation, an already-durable
pending acknowledgement, active-owner fail-stop, sticky terminal behavior, and
the request-after-disable race.

Two additional E290 cross-layer host composition tests exercise the real
authenticated adapter, submission runtime, `NodeInterfaceSupervisor`, exact
E290 LoRa policy, authorized-frame handoff, and radio dispatcher around fake NOR
and a scripted host radio. The happy path proves zero-write authorization
rejection, one durable acceptance and cap, the preparation barrier, exact frame
persistence/echo/completion, delivery timeout, principal-scoped status, and
durable remount. The permanent-fault path exposes a DATA frame, queues an
ordinary announce behind it, injects a wrong journal binding, and proves
`ActiveOwnerFailStopped` emits no acknowledgement, retains all owners, takes the
LoRa lease offline, and permits no later host-radio TX or RX.

The E290 library therefore has 53 passing tests: 51 focused policy/product/
credential-boot/credential-runtime tests
plus those two cross-layer composition tests. This closes software composition
qualification for the LoRa-first one-entry profile. Portable API framing,
immutable credential authority, the qualification-session core, and job handoff
are qualified, and semantic schema 2 persists exact authorization provenance;
ADR 0009's credential store is now boot-composed, while live external admission
remains blocked on physical-presence initialization/pairing, a firmware
API/session lane, and a bearer. Source `96e38aa` now supplies bounded powered
permanent-graph evidence for exact image readback, erased credential/journal
boot, LoRa/interface readiness, and ordinary TX on both boards. It does not
exercise the active-DATA durability owner or this ADR's fail-stop path; those
still require separate hardware evidence.
