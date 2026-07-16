# Bounded node-core external-buffer DATA dispatch

**Status:** portable route/permit/completion/recovery slice implemented; no
async or firmware RF TX linkage
**Rete pin:** `f6f5fb0637d00691e09fa0105be4df902405fee4`

## Purpose and boundary

`reticulum-node-core` proves the portable state transitions on both sides of a
future owning interface handoff. Firmware supplies a complete 500-byte
`TxPacketBuffer`; node-core registers it once, prepares encrypted RNS DATA
directly into it, resolves the route, and returns a unique routed `TxJob`
without moving or copying the packet array. The same owner then moves through
permit-pending, authorized or unpermitted, completion, and recovery typestates.
Alongside the concrete RNS owner, node-core stores only fixed scalar dispatch
metadata for those external buffers and the fixed DATA attempt ledger. Proofs
and timeouts move the exact attempt into an in-place terminal tombstone before
Rete removes its receipt.

The crate privately owns `reticulum-rns-rete::EmbeddedNode`, but its public
surface uses project-owned identities, destination hashes, interface targets,
packet-slot IDs, deadlines, attempt tokens and handles, terminal outcomes,
errors and capacity snapshots. It has no dependency on the device API,
Embassy, radio traits, ESP crates, a board support package or durable storage.
The future local-session dispatcher will depend on both node-core and
device-api and map their types explicitly.

`TxJob`, permit requests/replies, completions, buffers, and recovery records do
not expose packet bytes. Only an exactly matched `AuthorizedTx` can borrow the
encoded frame, once, through `frame(now)` before its deadline. This API proves
authorization semantics; it does not perform RF transmission. There is still
no async channel, interface actor, radio implementation, or RF-capable
firmware connection.

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
external buffer:   Unregistered -> Available -> Bound(unique TX owner)
                                                   |          |
                                                   v          v
                                               Available  TxQuarantine

dispatch metadata: Unregistered -> Free -> Reserved -> Routed -> Authorized
                                                         ^           |
                                                         |-- next ---|
                                                         |           |
                                                         v           v
                                                    RecoveryRequired -> Free

attempt ledger:                   Free -> Reserved -> Active -> Terminal -> Free
                                                         \-----------> Free

Active -> Free:   exact cancellation only when every hop is definitely unsent
Terminal -> Free: explicit acknowledgement after unique buffer return
```

`Bound(unique TX owner)` is an ownership statement. Only `AuthorizedTx` means
the permit linearization point has passed, and even that means “may have
transmitted”, not proof that hardware started or completed RF.

## Preparation transaction

`prepare_data_into_slot()` accepts a registered available buffer, a
`PrepareDataRequest`, and entropy. The request keeps the RNS whole-second clock
separate from the packet-owner millisecond clock and includes the destination,
plaintext, `owner_now`, `TxLeaseDeadline`, and a synchronous snapshot of
enabled packet interfaces:

1. Reject `deadline <= owner_now` before reservation, entropy use, or RNS
   mutation.
2. Validate the buffer owner/incarnation and its free dispatch slot.
3. Reserve a free attempt-ledger slot and checked, non-repeating attempt,
   dispatch, and hop generations before consuming entropy or mutating RNS.
4. Mark dispatch and attempt metadata `Reserved`, then call
   `EmbeddedNode::prepare_data_into()` with the external buffer's exact
   500-byte array.
5. On any native preparation failure, restore both metadata reservations and
   return the same available buffer inside `PrepareFailure`.
6. On success, commit the full receipt hash to the active attempt and resolve
   Rete's `All`, `Only(interface)`, or `AllExcept(interface)` target against the
   supplied enabled-interface snapshot.
7. Bind the first deterministic hop, dispatch generation, target, and deadline
   into the buffer and return the only `TxJob` that owns its mutable reference.

Capacity, registration and generation failures are rejected before entropy or
RNS mutation. Native receipt-table saturation, duplicate receipt hashes,
unknown destinations, oversized payloads, cryptography and packet-build
failures unwind the reservations and return the exact same buffer. After
native success, committing the pre-reserved metadata cannot allocate or fail.
An empty resolved route cancels the exact receipt and returns the same
available buffer. If that cancellation unexpectedly fails, preparation returns
an owning `TxQuarantine` and leaves the scalar recovery record retained instead
of pretending the buffer is reusable.

`TxJob` exposes the stable slot, encoded length, complete RNS proof-correlation
hash, generation-scoped attempt handle, original interface target, selected
interface, and deadline. It has no byte accessor. Route selection is ascending
by `PacketInterfaceId` bit, and each subsequent hop receives a fresh checked
generation. Multi-interface fan-out is serialized through the same unique
buffer; no packet copy or repeated interface is introduced.

The adapter independently preflights the current 383-byte plaintext limit and
bounded receipt table before encryption. `AttemptToken` is the full Reticulum
hashable-part digest covered by a proof, not SHA-256 of every encoded interface
byte. Device-API diagnostics must compute their encoded-byte digest separately
after the authorized byte-access boundary.

## Queue rejection and exact rollback

`rollback_queued(job, now)` models a future owning-channel insertion that
synchronously proves a routed job was never accepted. It validates the node
incarnation, stable slot, dispatch generation, receipt, attempt handle, target,
hop, and deadline before changing any state. It returns the same
`TxCompletionDisposition` vocabulary as ordinary completion handling.

For a still-active attempt, rollback cancels the exact full-hash RNS receipt
before freeing the attempt and dispatch metadata. It then restores the same
external buffer to `Available` and returns its original unique mutable
reference. If the receipt is unexpectedly missing, or any binding is stale or
inconsistent, `RollbackFailure` retains the still-bound `TxJob`; the buffer is
not silently reused. A rollback observed at or after its deadline first enters
the exact scalar recovery state, then finalizes that matching late owner as
`Recovered`; it cannot silently bypass deadline accounting. Rollback is
forbidden once any earlier serialized hop was authorized, even if the current
hop is again in the routed state. The cumulative possibly-transmitted
classification cannot be erased.

A proof or timeout may commit while the job remains bound. In that case Rete
has already removed the receipt, so rollback releases the dispatch and returns
the same buffer without cancelling a nonexistent receipt, while preserving the
terminal tombstone. `acknowledge_terminal()` reports `PacketStillBound` until
that release occurs. This ordering prevents an application from freeing the
tombstone while unique packet ownership is still outstanding.

Dropping any owning typestate does not reset its private buffer binding or
dispatch entry. A later preparation attempt with that buffer is rejected as
`PacketBufferBusy` before entropy or RNS mutation. Deadline maintenance can
move the authoritative scalar dispatch record into `RecoveryRequired`, but it
never fabricates or force-reuses the missing reference.

## Permit, byte access, and completion

`TxJob::begin_permit()` consumes routed ownership into `PermitPendingTx` and an
opaque non-`Copy` `TxPermitRequest`. `authorize_tx(request, now, policy)` first
validates node identity, `NodeInstanceId`, slot, dispatch generation, selected
interface, and per-hop generation. It denies terminal, expired, or retained-
recovery work without invoking policy. Only a fully valid candidate reaches
the synchronous regional/airtime policy.

A policy grant immediately and irrevocably changes authoritative dispatch
state to `Authorized` and sets the cumulative `may_have_transmitted` bit before
the non-`Copy` reply leaves node-core. This is the linearization point. A lost
or delayed grant can therefore never be reclassified as definitely unsent.
`PermitPendingTx::resolve(reply, now)` also checks the exact binding; a mismatch
retains both the unique pending owner and unchanged reply. A grant resolved at
or after the exact deadline produces `ExpiredAuthorizedTx`, which has no byte
accessor but remains possibly transmitted.

Only `AuthorizedTx::frame(now)` can expose packet bytes. It is one-shot, binds
the returned `TxFrame` to the exact permitted interface and attempt, and rejects
`now >= deadline`. The owner cannot be consumed into a completion while its
frame borrow is live. `ExpiredAuthorizedTx`, `AuthorizedTx`, and
`UnpermittedTx` produce owning completions with distinct conservative
classifications.

`complete_tx(completion, now)` reconciles that classification against the
authoritative scalar dispatch phase. An unpermitted hop advances to the next
deterministic route only while the attempt is nonterminal and its deadline is
still in the future. After the final definitely-unsent hop, the exact receipt
is cancelled only if no prior hop was authorized. Once any hop was authorized,
the receipt remains live through all later definitely-unsent returns. A proof
or timeout terminal stops later fan-out while preserving its tombstone.

## Exact deadlines and retained recovery

Deadline comparison is consistently inclusive: `now >= deadline` is expired
in preparation preflight, authorization, permit-reply resolution, frame
access, completion, and `maintain_tx()`. Expiry never frees a slot. Maintenance
moves routed or authorized scalar dispatch metadata to `RecoveryRequired` and
publishes records in stable slot order. Each `TxRecoveryRecord` includes the
`NodeInstanceId`, packet slot, dispatch generation, selected interface,
deadline, observation time, prior phase, cumulative possibly-transmitted bit,
and bounded reason.

While the unique owner is missing, this scalar dispatch record is
authoritative. A coherent late completion for the exact owner/incarnation,
slot, generation, interface, and conservative phase returns
`TxCompletionDisposition::Recovered`; node-core finalizes the metadata and the
same buffer becomes reusable. An internally inconsistent same-lease return or
an explicit recovery fault returns an owning `TxQuarantine` and retains the
fail-closed scalar record. Wrong-owner or stale completions are rejected intact
as `TxCompletionFailure`. Recovery never invents ownership, and notification
loss cannot make a slot reusable.

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

The focused host suite covers stable one-time registration, pointer-stable
no-copy preparation, deadline-before-mutation rejection, empty and deterministic
multi-interface routes, per-hop generations, exact queue rollback, cumulative
prior authorization, opaque permit matching, policy and terminal races, exact-
deadline authorization/reply/frame/completion/maintenance behavior, serialized
fan-out, coherent late recovery, fault and invariant quarantine, cross-
incarnation/stale returns, receipt terminal races, tombstone backpressure, and
exact acknowledgement reuse. A layout guard keeps packet-sized arrays out of
dispatch slots. The generic bare-metal and ESP32-S3 checks exercise the same
Embassy-free public slice; they do not link or exercise an async or radio path.

## Next boundary

The next slice is the owning Embassy handoff, not RF transmission:

1. Allocate fixed `TxPacketBuffer`s in static storage and move unique
   `&'static mut` references through bounded Embassy job and return channels.
2. Carry routed jobs and owning completions without cancellation loss; every
   full `try_send` path must return the unchanged owner to its caller.
3. Keep scalar permit traffic separate from the buffer-owning channels and
   bind at most one outstanding exchange to each unique owner.
4. Drive `maintain_tx()` and recovery observation from the node actor while the
   dispatcher cooperatively returns an exact late owner.
5. Persist accepted intent, active attempts, and terminal projection policy
   before mapping tombstones into device API v1.
6. Convert allocation-backed ordinary RNS actions into caller-reservable packet
   ownership; the DATA path alone does not cover proofs, announces, forwarding,
   Links or Resources.
7. Keep every firmware dependency graph TX-free and the radio-bearing lab
   image RX-only. Only after these boundaries and explicit antenna/load and
   regional approval may a guarded radio implementation or RF HIL use this
   path.
