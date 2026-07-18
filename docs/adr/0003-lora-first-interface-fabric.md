# ADR 0003: LoRa-first heterogeneous Reticulum interface fabric

- **Status:** accepted
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0002](0002-rete-provisional-foundation.md)

## Context

The first powered product target is the Vision Master E290 and its
HT-RA62/SX1262 LoRa path. LoRa is the fastest route to useful end-to-end mesh
behavior and deserves the first complete implementation, hardware
qualification, and operational hardening.

Reticulum is nevertheless a heterogeneous network. A permanent node may have
several interfaces online at once, learn a path on one interface, forward a
packet between two different interfaces, and fan an eligible packet out over
more than one interface. Future product profiles may combine LoRa with USB,
Wi-Fi, BLE, Ethernet, a second radio, or another stream or datagram link.

The device also has a separate local-control concern. USB, BLE, and Wi-Fi may
host the authenticated device API used by a SPA, mobile app, CLI, or desktop
client. A physical bearer may additionally host a Reticulum packet interface,
but the application API and Reticulum interface remain distinct logical
services with separate framing, authentication, flow control, and ownership.
The first USB milestone is that local client/control API, not a second packet
actor or an attempt at transport parity with LoRa.

A single global radio dispatcher would make later interfaces inherit RNode
framing, CAD, frequency, regional, airtime, and SX1262 assumptions. Conversely,
building speculative Wi-Fi and BLE packet actors before the LoRa path works
would delay the primary product milestone without validating useful hardware.

## Decision

### LoRa determines implementation order, not the core abstraction

The project will implement and qualify the E290 LoRa interface first. It will
receive the complete production treatment: RNode-compatible physical framing,
bounded reassembly, CAD and backoff, regional and power policy, exact airtime
accounting, radio deadlines, RX/TX fairness, and powered two-board Reticulum
interoperability.

The sole RNS/node owner remains independent of all of those details. It deals
in complete native Reticulum packets, stable packet-interface IDs, ingress
provenance, and Reticulum targets such as one interface, all eligible
interfaces, or all except the source interface.

### Use one authoritative bounded interface registry

Permanent composition will contain one fixed-capacity interface registry. A
record includes at least:

- a stable Reticulum packet-interface ID;
- a fixed actor queue/capability;
- an online state and non-repeating configuration generation;
- the logical native-packet MTU;
- an opaque actor-owned configuration identity; and
- transport-neutral metadata such as advertised bitrate and relative cost.

The registry is the source of the synchronous online-interface snapshot used
when node-core resolves an outbound Reticulum target. Firmware must not keep a
second hand-maintained enabled-interface set.

Each concrete actor receives only the bounded queue capability for its fixed
slot. Outbound routing moves the exact unique packet owner to the actor named
by the already-resolved interface ID. Completion returns through the same
generation-bound capability. Queue pressure, an offline or unknown interface,
MTU rejection, stale configuration, and crossed capabilities retain the exact
owner for explicit node reconciliation.

Ingress follows the inverse rule. A concrete actor removes its own link
framing, validates its configured MTU, and hands the sole node owner one
complete native Reticulum packet with provenance derived from its fixed
registry lease. Generic callers may not invent an arbitrary ingress interface
ID.

### Keep concrete link behavior inside its actor

The first actor is the LoRa/RNode actor. Only that branch may depend on RNode
fragmentation, LoRa modulation, CAD, radio configuration fingerprints,
regional frequency policy, RF airtime reservations, SX126x drivers, or a
Heltec board owner.

Later actors can have unrelated mechanics. For example, a USB stream actor may
use a bounded byte-stream frame, while a Wi-Fi actor may expose a configured
TCP, UDP, or local-discovery interface. They reuse the registry, native-packet
ownership, target resolution, and completion contract; they do not emulate a
LoRa modem.

There may be more than one actor of the same kind. “Sole radio” means sole
ownership within one LoRa actor, not a product-wide limit of one radio or one
LoRa interface.

### Treat interface lifecycle and learned paths explicitly

Rete currently retains only the one-byte interface ID on a learned path, not
the project registry generation. A registry generation therefore prevents
stale queued jobs and completions from being misattributed, but it cannot by
itself invalidate Rete path state learned under an older configuration.

The first permanent composition will keep an interface ID and its wire
configuration immutable for the node-owner lifetime. Replacing or materially
reconfiguring an interface requires either purging every path learned on that
ID before the new generation becomes online or restarting the node owner. A
later Rete adapter may carry stronger path provenance, but firmware must not
silently reuse an ID and assume the registry generation repaired native path
state.

### Keep the node permit opaque and resource-generic

Node-core's one-shot byte-release contract binds an exact selected interface,
owner generation, opaque interface resource ID, and nonzero quantity of
actor-defined resource units. It validates that a reservation names the same
resource and covers the requested quantity, but it does not know whether the
units mean airtime, stream credits, queue capacity, or something else.

The LoRa branch maps its complete radio-configuration fingerprint to the
resource ID and aggregate RF microseconds to the unit quantity. Its policy must
reject unknown resource IDs, validate the selected interface lease, and
recompute the expected RNode frame shape and airtime from the native packet
length plus its authoritative profile rather than trusting an actor-supplied
quantity. After grant, the radio dispatcher rechecks the live fingerprint and
profile and applies the reserved units to its fresh CAD/access projection.

This is not a fictitious universal airtime model. A later stream actor supplies
its own resource ID, admission meaning, and policy without adding those
semantics to node-core or emulating a radio.

Cost and bitrate are recorded now for diagnostics and later policy. Initial
`All` fan-out stays deterministic and serialized; the first registry does not
invent a cost-based route algorithm that Reticulum has not requested.

## Consequences

- The separate E290 semantic-HIL fixture has now passed its controlled
  two-board ANNOUNCE, DATA, and proof exchange. The autonomous permanent image
  must separately prove boot, radio initialization,
  ordinary ANNOUNCE TX/RX, contention and reset behavior; it needs an external
  injector or the source-composed credential-backed USB API bearer to originate
  controlled DATA. The portable session core, target-safe submission port,
  cross-layer host path, and minimal USB bearer now exist; powered
  authentication and DATA injection remain open. No Wi-Fi or BLE implementation
  is required first.
- Rete, node-core, storage, LXMF, and application services cannot acquire a
  dependency on `lora-phy`, RNode framing, SX126x, or an E290/Tracker BSP.
- Node-core authorization contains no radio configuration, RNode frame-count,
  regional-policy, or RF-airtime vocabulary; those meanings stay in the LoRa
  actor's resource mapping and policy.
- The LoRa branch may be replaced, omitted, or instantiated more than once
  without changing node protocol ownership.
- USB, BLE, and Wi-Fi can be used for the device API without falsely becoming
  Reticulum interfaces, and can later expose separate Reticulum services when
  a product profile enables them.
- USB is the first planned local client/control bearer. An optional USB packet
  actor, and Wi-Fi or BLE packet actors, remain sequenced after the complete
  LoRa vertical slice.
- Dynamic interface removal, reconfiguration, completion recovery, and learned
  path invalidation must be tested as ownership/lifecycle behavior rather than
  treated as ordinary configuration updates.
- Constrained boards can compile out actors and services, while the full E290
  profile can run several interfaces simultaneously.

## Deferred decisions

This ADR does not select the first non-LoRa **Reticulum packet-interface** wire
framing, TCP/UDP discovery policy, BLE packet-service shape, USB composite
layout, interface cost algorithm, cross-radio regulatory coordinator, or a
Rete path-generation patch. ADR 0006 separately proposes the local device-API
framing; that does not create a packet actor. Packet-interface decisions are
made when the corresponding concrete interface begins, without changing the
LoRa-first interface-fabric boundary accepted here.
