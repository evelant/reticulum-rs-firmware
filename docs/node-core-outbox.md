# Bounded node-core external-buffer DATA dispatch

**Status:** host-tested portable scaffold; no async or firmware RF TX linkage
**Rete pin:** `f6f5fb0637d00691e09fa0105be4df902405fee4`

## Purpose and boundary

`reticulum-node-core` proves the synchronous transaction immediately before an
owning interface handoff. Firmware supplies a complete 500-byte
`TxPacketBuffer`; node-core registers it once, prepares encrypted RNS DATA
directly into it, and returns a unique `TxJob` without moving or copying the
packet array. Alongside the concrete RNS owner, node-core stores only fixed
dispatch metadata for those external buffers and the fixed DATA attempt
ledger. Proofs and timeouts move the exact attempt into an in-place terminal
tombstone before Rete removes its receipt.

The crate privately owns `reticulum-rns-rete::EmbeddedNode`, but its public
surface uses project-owned identities, destination hashes, interface targets,
packet-slot IDs, deadlines, attempt tokens and handles, terminal outcomes,
errors and capacity snapshots. It has no dependency on the device API,
Embassy, radio traits, ESP crates, a board support package or durable storage.
The future local-session dispatcher will depend on both node-core and
device-api and map their types explicitly.

This slice does not expose packet bytes. `TxJob` exposes only scalar metadata;
a later interface-authorization state machine must issue a matching permit
before bytes become borrowable. There is currently no async channel, radio
actor, transmit-completion API, lease-expiry executor or RF-capable firmware
connection.

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
external buffer:   Unregistered -> Available -> Bound(TxJob) -> Available
dispatch metadata: Unregistered -> Free -> Reserved -> Queued -> Free
attempt ledger:                   Free -> Reserved -> Active -> Terminal -> Free
                                                         \-----------> Free

Active -> Free:   definitely-unsent exact rollback
Terminal -> Free: explicit acknowledgement after buffer return
```

`Bound(TxJob)` is an ownership statement, not transmit authorization. The
current API has no state that means radio TX started or completed.

## Preparation transaction

`prepare_data_into_slot()` accepts a registered available buffer, destination,
plaintext, RNS monotonic seconds, a millisecond `TxLeaseDeadline`, and entropy:

1. Validate the buffer owner/incarnation and its free dispatch slot.
2. Reserve a free attempt-ledger slot and checked, non-repeating attempt and
   dispatch generations before consuming entropy or mutating RNS.
3. Mark dispatch and attempt metadata `Reserved`, then call
   `EmbeddedNode::prepare_data_into()` with the external buffer's exact
   500-byte array.
4. On any native preparation failure, restore both metadata reservations and
   return the same available buffer inside `PrepareFailure`.
5. On success, commit the full receipt hash to the active attempt, preserve
   Rete's `All`, `Only(interface)` or `AllExcept(interface)` target and the
   caller's deadline, bind the dispatch generation into the buffer, and return
   the only `TxJob` that owns its mutable reference.

Capacity, registration and generation failures are rejected before entropy or
RNS mutation. Native receipt-table saturation, duplicate receipt hashes,
unknown destinations, oversized payloads, cryptography and packet-build
failures unwind the reservations and return the exact same buffer. After
native success, committing the pre-reserved metadata cannot allocate or fail.

`TxJob` exposes the stable slot, encoded length, complete RNS proof-correlation
hash, generation-scoped attempt handle, interface target and deadline. It has
no byte accessor. The target has not yet been resolved against an enabled
interface snapshot, and the deadline is retained metadata only; this slice
does not yet enforce expiry or recovery.

The adapter independently preflights the current 383-byte plaintext limit and
bounded receipt table before encryption. `AttemptToken` is the full Reticulum
hashable-part digest covered by a proof, not SHA-256 of every encoded interface
byte. Device-API diagnostics must compute their encoded-byte digest separately
after a future authorized byte-access boundary exists.

## Queue rejection and exact rollback

`rollback_queued()` is the only current release path for a `TxJob`. It models a
future owning-channel insertion that synchronously proves the job was never
accepted for transmission. It validates the node incarnation, stable slot,
dispatch generation, receipt, attempt handle, target and deadline before
changing any state.

For a still-active attempt, rollback cancels the exact full-hash RNS receipt
before freeing the attempt and dispatch metadata. It then restores the same
external buffer to `Available` and returns its original unique mutable
reference. If the receipt is unexpectedly missing, or any binding is stale or
inconsistent, `RollbackFailure` retains the still-bound `TxJob`; the buffer is
not silently reused.

A proof or timeout may commit while the job remains bound. In that case Rete
has already removed the receipt, so rollback releases the dispatch and returns
the same buffer without cancelling a nonexistent receipt, while preserving the
terminal tombstone. `acknowledge_terminal()` reports `PacketStillBound` until
that release occurs. This ordering prevents an application from freeing the
tombstone while unique packet ownership is still outstanding.

Dropping a `TxJob` does not reset its private buffer binding or dispatch entry.
A later preparation attempt with that buffer is rejected as
`PacketBufferBusy` before entropy or RNS mutation, leaving the original receipt
and attempt quarantined. This prevents silent reuse but is not recovery: the
future handoff must retain full-error ownership and escalate a genuinely lost
static reference to its supervisor.

There is intentionally no “possibly transmitted” completion path yet. Adding
one requires the async handoff, scalar authorization race and interface outcome
contract; `rollback_queued()` must not be used after authorization or possible
RF transmission.

## Receipt terminal ledger

`EmbeddedNode::ingest_with_receipt_sink()` and
`tick_with_receipt_sink()` present the exact receipt kind and complete packet
hash before changing Rete receipt or proof-deduplication state. Node-core's
private reservation matches only an active DATA slot with that full hash. The
same slot becomes `Terminal { Delivered | DeliveryTimeout }`, so terminal
commit cannot allocate or fill a second queue.

Unknown DATA hashes, already-terminal hashes and channel candidates are typed
`ReceiptCorrelationError` invariants, not retryable capacity failures. Rete
retains the affected receipt/proof state. Timer maintenance still returns its
ordinary actions alongside a fault so unrelated maintenance output is not
lost.

`terminal_attempts()` observes tombstones without removing them.
`acknowledge_terminal(handle)` frees one only after the dispatcher has durably
projected the result and no job remains bound. Node identity,
`NodeInstanceId`, ledger index and monotonic generation scope the opaque handle
against stale copies and ABA reuse. The ledger remains RAM-only and cannot
rehydrate active Rete receipts or terminal submissions after reset.

## Capacity and constrained hardware

`PACKET_BUFFERS` now bounds registered external buffers and node-owned dispatch
metadata; it no longer multiplies a 500-byte array inside `NodeCore`. Firmware
must still allocate the actual buffers, so product RAM budgeting includes at
least `PACKET_BUFFERS * 500` external packet bytes, each buffer's binding
metadata, node-core dispatch/attempt/Rete state and the async channel items
that will later carry their references.

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
```

The 24-test host suite covers capacity/routing types, stable one-time
registration, zero-capacity rejection, pointer-stable no-copy preparation,
preserved target/deadline metadata, lost-job quarantine, a layout guard that
dispatch slots do not embed packet-sized arrays, pre-entropy
unregistered/foreign/capacity and generation failures, native failure
rollback, receipt-hash collision, exact queue rollback and buffer reuse,
missing-receipt retention, cross-incarnation rejection, proof/timeout terminal
races while a job remains bound, mismatched-terminal-token retention,
tombstone backpressure and exact acknowledgement reuse, invalid-proof retry,
repeated full hashes across tombstones, explicit receipt-correlation faults,
timer-action preservation and exact full-hash candidate selection. The generic
bare-metal and ESP32-S3 checks exercise the same Embassy-free public slice;
they do not link or exercise an async or radio path.

## Next boundary

The next slice is ownership handoff and authorization, not RF transmission:

1. Allocate fixed `TxPacketBuffer`s in static storage and move unique
   `&'static mut` references through bounded Embassy job and return channels.
2. Resolve each preserved `TxTarget` against an enabled interface snapshot and
   serialize multi-interface fan-out without copying the 500-byte packet.
3. Add a scalar, generation-bound permit state that is checked immediately
   before the first irreversible hardware action; only that state may expose
   packet bytes to the interface actor.
4. Define definitely-unsent, authorized-or-possibly-transmitted and recovery
   completion outcomes, plus deadline quarantine. A missing unique reference
   must never be fabricated or force-reused.
5. Persist accepted intent, active attempts and terminal projection policy
   before mapping tombstones into device API v1.
6. Convert allocation-backed ordinary RNS actions into caller-reservable packet
   ownership; the DATA path alone does not cover proofs, announces, forwarding,
   Links or Resources.
7. Only after those boundaries and explicit antenna/load/regional approval may
   a guarded radio implementation or RF HIL use this path.
