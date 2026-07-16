# Owning async TX handoff

**Status:** design accepted for the next portable slice; no firmware TX graph
**RF status:** compile-disabled until antenna/load and regional authorization

## Decision

Move unique `&'static mut TxPacketBuffer` references through two bounded
Embassy channels. Only a pointer crosses tasks: the fixed 500-byte packet does
not move or copy, and the radio actor can retain the unique buffer across an
`await` without borrowing node-core.

Node-core remains portable and Embassy-free. It owns attempt/dispatch metadata
but receives an externally owned buffer for preparation. A small handoff crate
owns the Embassy channel topology. The Tracker firmware will not gain a
TX-capable radio type or feature as part of this slice.

## Ownership topology

Allocate zero-filled buffers in `.bss` with `ConstStaticCell`, then place each
unique reference into the available/completion channel before tasks start:

```text
available/completion channel
    -> node-local reservation and RNS preparation
    -> jobs channel
    -> one interface actor across await
    -> available/completion channel
```

The node actor is the sole consumer of available/completed buffers and the
sole producer of jobs. The interface actor has the inverse roles. Both channel
capacities equal the pool size. While one endpoint owns a buffer, its outbound
channel can contain at most every other buffer; a send is therefore
capacity-infallible if the endpoints remain encapsulated. An unexpected full
result must return the unique reference to the caller and become an invariant
fault, never drop or duplicate ownership.

The return path carries more than the bare reference. It uses a non-`Copy`
owned completion whose payload is still only one buffer pointer plus scalar
metadata:

```text
TxCompletion {
    buffer,
    dispatch_generation,
    interface,
    outcome: DefinitelyUnsent | AuthorizedOrPossiblyTransmitted | RecoveryFault,
}
```

Each outcome also retains the bounded driver/retry reason needed by policy.
`DefinitelyUnsent` permits a same-interface retry, route advance or exact
receipt cancellation; `AuthorizedOrPossiblyTransmitted` keeps proof
correlation live even when the driver reports an error; `RecoveryFault`
quarantines the slot and enters supervisor recovery. The node owner validates
the completion's generation and interface before advancing the serialized
route plan. A mismatched completion is an invariant fault and cannot silently
free or reuse its returned buffer.

Safe `ConstStaticCell` and Embassy channel APIs encapsulate the required unsafe
internals. Project crates retain `#![forbid(unsafe_code)]`; do not introduce
`static mut`, raw pointers, heap boxes, or forced mutex reclamation.

## Portable state and types

Node-core will add project-owned identifiers and routing types:

```text
PacketSlotId(u16)
PacketInterfaceId(u8)
MonotonicMillis(u64)
TxLeaseDeadline(MonotonicMillis)

TxTarget = All | Only(interface) | AllExcept(interface)
InterfaceSet(u64)
TxRoutePlan { remaining interfaces }
```

`TxPacketBuffer` contains its stable slot ID, 500-byte array, encoded length
and generation-scoped binding. A matching non-Copy owned job plus scalar
`TxPermit` is required to expose the bytes. Checks bind slot, owner identity,
`NodeInstanceId`, dispatch generation, selected interface and permit
generation; stale scalar copies cannot expose a reused buffer.

The current node-core drops `PreparedData::target()`. The refactor must retain
it and resolve it synchronously against a snapshot of enabled Reticulum packet
interfaces. USB, BLE and Wi-Fi device-API transports are not automatically RNS
packet interfaces. Multi-interface fan-out serially reuses the same unique
buffer, issuing a new interface-bound permit for each hop.

## Transactional preparation

The node actor first removes one free unique buffer from the available channel,
then calls a portable API equivalent to:

```text
prepare_data_into_slot(buffer, destination, plaintext,
                       rns_seconds, lease_deadline, rng)
```

The transaction must:

1. validate that the buffer ID maps to free dispatch metadata;
2. reserve attempt-ledger and dispatch slots before entropy/RNS mutation;
3. prepare directly into the external buffer;
4. restore metadata and leave the buffer free on failure;
5. bind length, receipt token, target, attempt, instance and generation on
   success; and
6. enqueue the unique reference as a job.

Pool exhaustion is rejected before entropy or RNS mutation. If the jobs
channel invariant fails, node-core must cancel the exact new receipt, roll
back attempt/dispatch metadata and return the still-owned buffer.

## Authorization boundary

Owning bytes is not authorization to transmit. Immediately before the first
irreversible hardware action, the interface actor requests a scalar permit
from the node owner. The node linearizes:

- stale, terminal, cancelled, expired, recovery-required, wrong-interface or
  policy-denied work: deny without touching radio TX;
- active work with a valid route, deadline, regional profile and airtime
  reservation: change `Queued -> Authorized` and issue a generation-bound
  permit.

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
a channel, driver future, SPI transaction or interface task. Expiry changes
metadata once to `RecoveryRequired` and publishes a bounded supervisor record
containing the lease, interface, prior phase, deadline and whether RF may have
started.

- Before authorization: deny later permits and request a definitely-unsent
  cooperative return.
- After authorization: request radio cleanup/reset and classify any return as
  possibly transmitted.
- On return: finalize metadata and reclaim the exact buffer normally.
- No return by the recovery grace deadline: disable TX, retain the fault and
  reset the MCU. Never fabricate or force-reuse the missing reference.

`lora-phy` 3.0.1 waits for DIO1 with SX1262 hardware TX timeout disabled and
warns against cancelling IRQ processing. The initial actor must await the
complete operation; the supervisor watchdog/reset is the recovery boundary
until a TX wrapper proves bounded hardware timeout and cleanup.

Reset discards volatile references and native receipts. Higher-level durable
LXMF/submission records must reconstruct fresh attempts under a new
`NodeInstanceId`; leases are never persisted. If reset follows airtime
authorization, charge the entire reservation conservatively so reboot cannot
reset regulatory accounting.

## Implementation boundary before RF approval

Safe to implement and host/target-test now:

- external-buffer node-core refactor and target preservation;
- unique-reference Embassy handoff;
- fake-interface authorization/permit state machine;
- lease deadlines, recovery diagnostics and supervisor simulation;
- route resolution and serialized fan-out;
- stable-address/no-copy, pressure, stale-token and race tests;
- generic RISC-V and ESP32-S3 compilation; and
- dependency/feature guards that keep Tracker TX unavailable.

Still requires explicit antenna/load and regional authorization:

- any TX-capable Tracker BSP surface or firmware feature;
- CTX/FEM transmit sequencing, `SetTx`, CAD or TX IRQ handling;
- power, frequency, access and airtime policy selection;
- flashing a TX-capable image; and
- over-the-air, thermal, harmonic or split-frame TX HIL.

## Remaining protocol blocker

The caller-owned path currently covers locally prepared DATA only. Rete
`NodeActions` still contains allocation-backed proof, announce, forwarding,
Link and Resource packets. A full bounded node needs a caller-reservable
outbound-action sink so those bytes are built transactionally into the same
fixed pool; wrapping the resulting `Vec` after protocol mutation is not an
equivalent no-copy or backpressure guarantee.
