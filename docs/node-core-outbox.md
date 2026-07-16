# Bounded node-core DATA outbox

**Status:** host-tested portable scaffold; not connected to firmware RF TX
**Rete pin:** `f6f5fb0637d00691e09fa0105be4df902405fee4`

## Purpose and boundary

`reticulum-node-core` proves the first state-changing boundary needed by the
standalone node: encrypted RNS DATA can move from the protocol owner into a
fixed interface handoff slot without a fallible allocation or queue insertion
after its delivery receipt is registered. Proofs and timeouts then move the
same attempt into a fixed in-place terminal tombstone before Rete removes the
receipt.

The crate privately owns the concrete `reticulum-rns-rete::EmbeddedNode`, but
its public surface uses project-owned identities, destination hashes, attempt
tokens and handles, terminal outcomes, errors, capacity snapshots and TX
leases. It has no dependency on the
device API, radio traits, Embassy, ESP crates, a board support package or
durable storage. The future local-session dispatcher depends on both node-core
and device-api and maps their types explicitly; node-core does not import a
frozen wire vocabulary.

This slice exposes raw RNS packet bytes only through a live synchronous lease.
The device API has no packet handle and cannot drain the outbox. The current
borrowed `TxFrame` is not yet an owning cross-task/async radio handle; the pool
ownership split must be resolved before an interface awaits TX completion.

## Preparation transaction

Each outbox element owns a 500-byte packet array and one private state:

```text
Free -> Reserved -> Ready -> Leased -> Free
                    ^          |
                    |----------|  definitely unsent transient return
```

Every DATA submission also reserves one `PATHS`-bounded attempt slot:

```text
Free -> Reserved -> Active -> Terminal -> Free
                                 |          ^
                                 +----------+  explicit acknowledgement only
```

`enqueue_data()` performs these operations synchronously:

1. Find a free outbox slot, a free attempt slot and the next non-repeating
   attempt generation without advancing any cursor.
2. If either bounded pool is full, return `OutboxFull` or `AttemptLedgerFull`
   before entropy or RNS state changes.
3. Mark both slots `Reserved` and invoke
   `EmbeddedNode::prepare_data_into()` with the outbox slot's exact 500-byte
   array.
4. On native failure, restore both slots to `Free` and return a project-owned
   typed error without advancing round-robin or generation state.
5. On success, commit the full receipt hash to `Active`, assign Copy
   packet/attempt metadata and change the packet `Reserved -> Ready`. No
   allocation, capacity check or fallible queue operation remains.

The adapter independently preflights the 383-byte plaintext limit and bounded
receipt table before encryption. The returned `AttemptToken` is the full RNS
hashable-part digest covered by a proof; it is not SHA-256 of every encoded
interface byte. Device-API packet diagnostics must compute their encoded-byte
digest separately.

## Interface leases

`lease_next()` changes one ready slot to `Leased` and returns an opaque owner
identity, node-instance ID, slot and globally increasing generation. A copied
stale lease cannot become valid again after a transient return, slot reuse or
node-owner reconstruction. `NodeInstanceId` must be unique for every owner
incarnation while old actor messages can exist; firmware will derive it from a
boot/session nonce or persisted boot epoch. Generation exhaustion is a typed
failure and leaves the packet ready.

The interface actor must report one of three meanings:

- `return_unsent`: an immediate transient rejection proved not to have reached
  a transmission path. The exact bytes and receipt return to `Ready`.
- `complete_tx`: transmission occurred or may have occurred. Packet storage is
  freed, but the receipt remains live for proof or timeout.
- `abort_unsent`: transmission definitely did not occur and will not be
  retried. The exact full-hash receipt is cancelled before packet storage is
  freed. If cancellation reports the receipt missing, the slot remains leased
  and an invariant error is returned.

A proof or timeout can become terminal while a packet is `Ready` or `Leased`.
Ready bytes are discarded before they can be leased. A live lease is retained
because an interface may already have started transmission; `leased_frame()`
then returns the exact `TerminalAttempt`, and terminal acknowledgement is
blocked until `complete_tx`, `return_unsent` or `abort_unsent` releases the
packet slot. Those release paths do not cancel an already-terminal receipt.

Rete starts the receipt timeout during packet preparation, not after RF
completion. The outbox is therefore a prompt interface handoff pool, not the
client intent queue or durable retry store. Higher-level requests must be
copied into separately bounded state before preparation, and local congestion
must not leave ready packets waiting indefinitely.

## Receipt terminal ledger

`EmbeddedNode::ingest_with_receipt_sink()` and
`tick_with_receipt_sink()` present the exact receipt kind plus complete packet
hash before changing Rete receipt or proof-deduplication state. Node-core's
private reservation matches only an `Active` DATA slot with that full hash;
the same slot becomes `Terminal { Delivered | DeliveryTimeout }`, so terminal
commit cannot allocate or fill a second queue.

Unknown DATA hashes, already-terminal hashes and channel candidates are typed
`ReceiptCorrelationError` invariants, not retryable capacity failures. Rete
retains the affected receipt/proof state. Timer maintenance still returns its
ordinary actions alongside the fault so non-receipt work is not lost.

`terminal_attempts()` observes tombstones without removing them.
`acknowledge_terminal(handle)` frees one only after the dispatcher has durably
projected the result; a node-identity, `NodeInstanceId`, ledger index and
monotonic generation scope the opaque handle against stale copies and ABA
reuse. This scaffold stores the ledger in RAM only. It does not yet restore
active Rete receipts or terminal submissions after reset, so reboot recovery
requires a separate persisted intent/attempt design before production TX.

## Capacity and constrained hardware

`OUTBOX` is a compile-time packet-slot count. Each slot contributes exactly 500
packet bytes plus state metadata. `PATHS` bounds the current Rete path, reverse
and destination-DATA receipt maps and the node-core attempt ledger. Rete's
heapless maps require `PATHS` and `LINKS` to be powers of two greater than one;
its announce and deduplication deques require `ANNOUNCES > 0` and
`DEDUPLICATION > 0`. `capacity_profile_is_supported()` exposes the same
project-owned guard, and `NodeCore::new()` emits explicit compile-time
assertions for a monomorphized invalid profile. Retained tombstones
intentionally reduce new-send capacity until acknowledged. Profiles may reduce
the valid values on the no-PSRAM Tracker, while a larger board can increase
them without changing protocol behavior.

`CapacitySnapshot` reports ready, leased, used and configured outbox slots;
current/configured Rete receipt entries; and active, terminal, used and
configured attempt slots. It contains no native Rete collection or mutable
state.

The crate itself uses no allocator API and compiles for a generic bare-metal
target. Its owned Rete node still contains allocation-backed construction and
ordinary ingest/action paths, so this result applies specifically to DATA
preparation, packet-pool state and lease bookkeeping—not the entire future
node runtime.

Host `size_of` diagnostics are not an ESP32-S3 layout measurement, but they
confirm that attempt metadata is material in addition to each 500-byte packet
array. Before selecting the no-PSRAM Tracker profile, record Xtensa static
layout plus linked `.bss`/heap/stack headroom and add a regression budget for
the chosen `PATHS/LINKS/OUTBOX` tuple.

## Current validation

```sh
cargo test --locked -p reticulum-node-core
cargo clippy --locked -p reticulum-node-core --all-targets -- -D warnings
cargo check --locked -p reticulum-node-core \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked -p reticulum-node-core \
  --target xtensa-esp32s3-none-elf
```

The 30-test host suite covers profile constraints, parseable output and receipt
hashes, outbox and ledger rejection before entropy, unknown destinations,
oversized payloads,
receipt saturation and hash collision, definitely-unsent cancellation,
uncertain TX completion, byte-identical transient retry, rejected-transaction
ordering, stale copied and cross-incarnation leases/handles, generation
exhaustion, exact proof and timeout tombstones, invalid-proof retry, full-hash
selection, unknown/already-terminal/channel correlation faults, and every
Ready/Leased terminal ordering. Both the generic bare-metal target and the
ESP32-S3 Xtensa target compile successfully.

## Next boundary

The next slice is packet ownership and reboot/lost-owner recovery, not RF
transmission. It should:

1. Move fixed packet buffers behind a unique owning handle so an interface can
   hold one across `await` without copying 500 bytes or borrowing node-core.
2. Preserve `PreparedData::target()` as explicit multi-interface routing
   authorization; the current DATA lease exposes bytes and attempt identity but
   not its target.
3. Add a supervisor-visible lease deadline and cooperative quarantine/return
   path. A lost unique buffer must never be force-reused; escalation may require
   a retained-fault record and reset.
4. Persist accepted intent, active attempt and terminal projection state, and
   define reset policy or native receipt rehydration before mapping tombstones
   into device API v1 and acknowledging them.
5. Convert allocation-backed ordinary RNS actions into bounded packet ownership
   transactionally; the caller-owned DATA path alone does not solve proofs,
   announces, forwarding, Links or Resources.
6. Only then connect a guarded interface actor, airtime/regional policy and RF
   TX HIL. Antenna/load and the regional profile remain prerequisites for any
   over-the-air transmission.
