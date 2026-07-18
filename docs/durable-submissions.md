# Durable submissions and persist-before-ack projection

Status: portable semantic model, projector, physical two-bank journal, sole
storage actor, transport-neutral submission runtime, native authorized-frame
seam, and exact E290 request/durable-echo handoff implemented; portable
authenticated device-API dispatch implemented; resident E290 operation-scoped
flash/runtime coordinator implemented; isolated powered journal clean-path/
software-reset HIL passed on board E9:44; the 106-test E290 host suite qualifies
the one-entry complete LoRa-first software composition and ADR 0005 active-owner
fail-stop. Portable API framing, the pre-authentication initialization-control
codec, immutable credential authority, the USB-
qualification session core, and the boot-lifetime job handoff are qualified;
semantic schema 2 now durably binds exact authorization provenance to every
acceptance. E290 now validates, boot-mounts, deterministically recovers, and
retains the credential store without auto-provisioning. Explicit initialization
and live pairing are routed through the resident owner and pre-authentication
USB records. The minimal authenticated USB session/API lane is source-composed;
live powered admission remains blocked by successful credential creation and
end-to-end handshake/request/reply proof. Source `96e38aa` adds bounded
powered evidence for exact image readback, erased credentials with zero
mutation, strict empty-journal mount, resident service, and ordinary TX on both
boards; it does not exercise durable DATA or interruption recovery.
Controlled power-cut durability, projector
retirement, journal retention, endurance/soak, and at-rest encryption remain
unqualified.

## Claim boundary

`reticulum-storage-model` owns the project vocabulary for accepted outbound
work. It is allocation-free and `no_std`; it defines canonical records,
principal-scoped idempotency, lifecycle validation, complete-replay sealing,
and a fixed-RAM index. `reticulum-submission-projector` correlates those records
with volatile node/TX observations and withholds exact terminal or recovered-
owner acknowledgement until the corresponding record is known durable.

Neither semantic crate writes flash. `reticulum-storage-journal` supplies
the physical format, complete replay, commit/readback, lifetime admission, and
two-bank compaction mechanisms described in
[Physical submission journal](storage-journal.md). A canonical CBOR record is
still not durable merely because it encodes successfully, and the in-RAM index
is not a free-space or wear-level estimate. `reticulum-storage-actor` now owns
the exact journal binding, replayed live index and sole projector, and connects
an exact plan to physical persistence before making an acceptance or
acknowledgement visible. The physical backend remains outside the actor and is
borrowed through one bound operation-scoped view. See
[Portable sole storage actor](storage-actor.md).

`reticulum-submission-runtime` owns that actor and implements the bounded
durability-first scheduling loop against a narrow `SubmissionNodePort`. Its
production port implementation uses `NodeInterfaceSupervisor` and contains no
selected interface in a preparation request: the authoritative node router may
choose LoRa now and other eligible Reticulum interfaces later without changing
the durable record vocabulary. The runtime is executor-, board-, radio-, RNode-,
and local-client-transport-independent. See
[Transport-neutral durable submission runtime](submission-runtime.md).

`reticulum-device-api-adapter` is the allocation-free authenticated dispatch
edge over a narrow semantic `SubmissionPort`. The coordinator retains the actor,
constructs an operation-scoped bound journal only inside durable acceptance,
and exposes neither capability to the adapter. The port returns the adapter's
bounded `SubmissionAcceptance` vocabulary, not storage-actor progress or a
backend type; `reticulum-storage-actor` and `embedded-storage` are test-only
dependencies of this crate. Default builds expose public capabilities and
principal-scoped status; missing and foreign IDs are indistinguishable, and
status returns `Internal` while the actor has ambiguous pending work or is
faulted rather than exposing a deliberately lagging index. Its
target-safe `experimental-rns-data` feature copies the experimental borrowed payload
into an owned candidate, maps only trusted dispatch provenance into the
storage-owned authorization snapshot, and returns an ID only after `accept`
reports durable success or exact replay. Adapter-local capability restriction prevents a
separately unified codec feature from advertising that operation. The feature
is checked on bare-metal targets. The adapter has no framing, session,
USB/BLE/Wi-Fi, node, or direct-radio ownership; its transport-neutral outbound
RNS DATA submission can nevertheless be routed by the product node over LoRa or
another eligible Reticulum interface after acceptance.

The physical backend's qualifying run is preserved at
`artifacts/storage-hil/20260716T211318Z-e944-7b47113` from source
`7b47113aeec6c7f0549cd5b264eceacef830fb4c`. Its strict two-boot serial check
covered A1 format, five appends, no-mutation retry/conflict, B2 compaction, a
software reset, and zero-write/zero-erase B2 replay. Independent raw-dump replay
confirmed one revision-4 `Delivered` submission across five committed records,
with the retired A manifest and unused B tail erased. This is isolated journal
clean-path evidence, not powered actor, powered-cut, API, or product-runtime
evidence.

```mermaid
flowchart LR
    Client["authenticated local client"] --> Bearer["minimal authenticated USB bearer (source-composed; powered proof open)"]
    Bearer --> Framing["portable framing (implemented)"]
    Framing --> Session["portable qualification session (composed single-flight)"]
    Session --> Handoff["portable job handoff (implemented)"]
    Handoff --> API["portable authenticated API adapter (implemented)"]
    Authority["immutable credential authority (implemented)"] --> Session
    Authority --> API
    CredentialStore["credential store boot-mounted/recovered; explicit initialization and pairing routed"] --> Authority
    API -->|"authenticated request; one in flight"| Runtime["resident portable submission runtime"]
    Coordinator["E290 sole-flash coordinator (resident)"] <--> Runtime
    Runtime <--> Store["portable sole storage actor (implemented)"]
    Coordinator --> Journal["operation-scoped schema-2 bound journal"]
    Journal --> Flash["validated raw NOR partition"]
    Store --> Model["actor-owned live replay index"]
    Store --> Projector["actor-owned sole projector"]
    Runtime <--> Supervisor["transport-neutral node supervisor"]
    Supervisor --> Fabric["Reticulum interface fabric"]
    Fabric --> LoRa["LoRa / SX1262 actor (first and primary)"]
    Fabric -. "later" .-> Other["Wi-Fi, BLE, USB, or other interfaces"]
    LoRa -->|"exact observation request; DATA owner retained"| FrameHandoff["bounded request / durable-echo handoff"]
    FrameHandoff --> Runtime
    Runtime -->|"Durable"| FrameHandoff
    FrameHandoff -->|"identical acknowledgement"| LoRa
```

The storage actor is the sole authority allowed to order physical commits and
mutate the live index. It also owns the one projector that carries bounded
volatile correlation. Callers receive only immutable projector inspection and
narrow actor-owned operations for the preparation barrier, node preparation
result, authorized frame, terminal, recovery, quarantine, and exact upstream
acknowledgement; they cannot obtain, replace, or extract the projector. Every
durable projector request returns through `persist_projector`, and every
mutating observation shares the actor's pending-write/fault gate. The node
supervisor remains the sole owner of native Rete state, packet buffers,
attempts, and TX typestates.

The submission runtime now supplies the portable orchestration that was
previously left to product code. It gates live operations until bounded boot
recovery completes, reconciles actor ambiguity and pending projection first,
withholds exact upstream acknowledgement until durable state unlocks it, then
drains node observations, prepares already-barriered work, and finally begins a
new `Queued -> Preparing` barrier. The permanent E290 node task now hosts this
runtime through the resident sole-flash coordinator beside node ingress,
timers, and `NodeInterfaceSupervisor`, with at most one runtime drive attempt
per outer loop. The concrete LoRa dispatcher remains a separate actor.

The current E290 profile permits one accepted-history entry solely for host
composition qualification; that is not product capacity, and no credential-
backed external API/session firmware lane or bearer is composed. Credential
boot is an earlier, independent coordinator step. Journal strict-mount,
supported-history, or recovery failure during
boot therefore occurs before a durability-gated DATA owner can exist; it leaves
the coordinator resident without a submission runtime, keeps local durable
service closed, and permits route-only LoRa to continue. Flash-map, identity,
announce-clock, and identity-authorized fresh-provisioning failures remain
boot-fatal. This product policy does not weaken the journal's fail-closed mount:
no invalid replay prefix is exposed or used. A permanent runtime/storage fault
after a DATA observation is active is different: ADR 0005 enters
`ActiveOwnerFailStopped`, retains the observation/completion/router ticket,
takes the same LoRa lease offline, and permits no later radio operation in that
boot.

After complete mount/replay, `finalize_boot_recovery` also owns the conservative
boot edge. It returns queued and already-final decisions without writing, or
retains and durably commits the exact `InterruptedByReset` transition before
reporting `Finalized`. An ambiguous backend reply preserves the ID, boot
sequence, and plan for exact autonomous retry.

## Durable identity and records

Acceptance is scoped by the authenticated `PrincipalId` plus a client-chosen
`IdempotencyKey`. The content comparison uses a domain-separated SHA-256 over
the semantic destination and payload, not over API CBOR. Repeating the same
principal, key, and content returns the original `SubmissionId`; changing the
content under that key is a conflict and never mutates the original record.

The current immutable journal vocabulary is:

- `Accepted`: submission ID, principal, idempotency key, exact credential ID/
  generation, authority revision, authorization-policy version, granted
  permission mask, and the complete bounded experimental RNS DATA intent;
- `StateTransition`: submission ID, exact next revision, and lifecycle state;
  a reboot-interrupted final transition carries its `BootRecoveryMarker`
  inside `InternalFailure::InterruptedByReset`;
- `Audit`: submission ID and exact next revision plus one transport-recovered
  or transport-quarantined observation containing the RNS attempt token,
  conservative transmission uncertainty, and an exact
  `TransportRecoveryReason`; its `CompletionFault` variant alone carries the
  unrestricted `u16` driver/control-plane completion code.

Boot recovery is not a separate audit event. The interruption evidence and
boot sequence are part of the immutable final lifecycle transition, so replay
cannot apply a free-standing boot marker before or after an unrelated state.

The complete encoded packet SHA-256 and the RNS attempt token are distinct
types. Node-core hashes every encoded packet byte immediately after successful
preparation and retains that digest with the queued attempt. The authorized
sink independently rehashes the exact borrowed frame, and the projector
requires both values to agree before planning prepared metadata. The RNS token
instead covers Reticulum's protocol-defined hashable bytes for proof
correlation. They are never interchangeable.

`AttemptHandle`, packet-buffer slot, dispatch generation, monotonic deadline,
native receipt object, and every reference remain volatile. Persisting any of
them would create a false promise that a reset node incarnation can rehydrate
an old lease or Rust owner.

Records use strict definite-map indexed CBOR with semantic schema version 2 and
a 512-byte ceiling. The domain-separated content SHA-256 remains available but
is derived from immutable intent rather than serialized or retained as a
second copy. Transport recovery records contain a semantic reason
discriminant; only `CompletionFault` carries a separate unrestricted `u16`
driver/control-plane completion code, so no such code can collide with
deadline, cancellation, identifier-exhaustion, or invariant reasons. Decoding
re-encodes and byte-compares the value, rejecting noncanonical integers,
trailing data,
malformed lengths, invalid authorization snapshots, and invalid state combinations.
Physical atomicity, integrity chaining, and torn-write detection are supplied
by `reticulum-storage-journal`. Its SHA-256 values are unkeyed corruption
detection, not tamper authentication or confidentiality; either property would
require a separate reviewed design.

## Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Queued: Accepted committed
    Queued --> Preparing: no-replay barrier committed
    Queued --> FinalFailed: validation failure
    Queued --> Cancelled: cancelled before preparation
    Preparing --> AwaitingDelivery: packet metadata committed
    Preparing --> Delivered: proof terminal before frame record
    Preparing --> FinalFailed: preparation/policy/timeout/internal failure
    AwaitingDelivery --> Delivered: proof or application acknowledgement
    AwaitingDelivery --> FinalFailed: timeout or internal failure
```

`Queued` is the only replay-safe state. The `Queued -> Preparing` record must be
durable before node preparation starts. A reboot that finds `Preparing` or
`AwaitingDelivery` finalizes the submission as an internal
`InterruptedByReset` failure rather than risk duplicate RF. A reboot may replay
`Queued` work only after the backend has integrity-validated the complete
journal through its known end-of-log and the replay has been sealed; the replay
builder structurally exposes no planning or boot decision before that point.

Transport recovery and quarantine are revisioned audits, not lifecycle states.
They are illegal before the preparation barrier, retain the durable RNS attempt
token, and contribute to an accumulated `may_have_transmitted` value using
monotonic OR. An exact transport observation with `false` is valid even when an
earlier lifecycle transition already made the aggregate `true`; applying it
does not weaken the retained uncertainty. A contradictory token or second
transport audit is rejected during preflight and replay.

Crossing into `AwaitingDelivery`, direct delivery from `Preparing`, a delivery
timeout after the preparation barrier, or reset interruption after that barrier
sets the accumulated transmission uncertainty. These transitions may follow
authorization even when a frame observation was not yet durably projected, so
replay conservatively retains possible transmission.

## Two-phase mutation protocol

Every new record follows the same ordering:

1. Read the complete live index and construct the exact next transition or
   audit.
2. Ask the model to preflight it without mutation. A successful result is an
   opaque `PlannedMutation` containing the exact immutable record.
3. Ask the backend to reserve and commit that exact record under an idempotent
   logical key.
4. After commit or readback-equivalence is proven, apply the same opaque plan
   to the live index.
5. Only then publish acceptance, update client-visible state, or unlock the
   exact node/supervisor acknowledgement.

Retryable or ambiguous storage failure retains the identical plan and never
acknowledges upstream state. Contradictory readback, a different reply handle,
or an index mutation that violates sole-actor serialization latches a fault and
does not acknowledge. Planning catches revision, lifecycle, token, and index
errors before bytes are allowed to enter the journal, so a rejected mutation
cannot poison every later replay.

The implemented actor holds one optional pending mutation. Acceptance retains
the complete opaque plan; projector persistence retains only a compact handle
whose exact request remains in the actor-owned projector. Public
`drive_pending()` can resume either form after an ambiguous backend result
without receiving the original candidate, request, or another projector. The
actual `Option<PendingMutation>` layout is exposed as `PENDING_MUTATION_BYTES`
and compile-time constrained to at most 512 bytes. Unrelated requests receive
`Busy` until the retained mutation resolves, and invariant violations latch a
fail-closed bounded fault.

## Volatile correlation and acknowledgements

After the preparation barrier is durable, the projector binds one
`SubmissionId` to the generation-checked `AttemptHandle` and complete
`AttemptToken`. It records neither value until that barrier is committed, and
it never serializes the handle.

Node-core's queued metadata supplies the preparation-time packet length and
SHA-256 of all encoded bytes. `TxFrame::observation()` independently rehashes
the exact native DATA bytes exposed by the authorized frame and produces
`AuthorizedFrameObservation`: interface, attempt handle, RNS token, packet
length, and complete-frame SHA-256. This happens before RNode or radio
fragmentation. The runtime converts the observation to transport-neutral
projector state, intentionally dropping only the selected interface.

The portable radio dispatcher now gates every post-byte-exposure DATA outcome,
including TX cancellation and terminal fault paths. It retains the owning
`TxCompletion`, exact router ticket, expected observation, and any unsent
request until a bounded handoff delivers the observation to the E290 node task.
The node retains and re-offers that same value while
`offer_authorized_frame()` returns `Retain`; after projector persistence makes
the offer return `Durable`, it echoes the identical observation. Only an exact
echo advances the dispatcher to completion return. Full request or
acknowledgement channels and cancellation of capacity/acknowledgement waits do
not move or forget these owners. An unexpected or non-matching echo disables
the dispatcher and retains the completion, router ticket, expected observation,
and actual acknowledgement. `DispatchReport` is a copy-only diagnostic; taking
or logging it cannot release ownership.

The runtime maps correct transient projector pressure to `Retain`: an actor-
owned physical mutation, a proof/timeout terminal record planned before the
frame, or a pending exact recovery acknowledgement. The node keeps the same
observation while ordinary runtime steps resolve that work. Correlation or
lifecycle contradictions and fail-closed storage/projector faults remain typed
errors and never cause an acknowledgement.

The projector cross-checks the preparation and authorized-byte digests and
lengths; repeated fan-out observations are idempotent only when all durable
packet metadata is identical. The E290 composition implements this ownership
path under a host-qualified one-entry cap. The minimal authenticated USB lane
now surrounds the portable authority/session core in source; powered successful
credential creation and end-to-end reachability remain open.

Terminal outcomes map as follows:

| Node outcome | Durable disposition |
| --- | --- |
| valid proof/application acknowledgement, including before a frame record | `Delivered` with exact preparation metadata |
| native receipt timeout after preparation, including before a frame record | `Failed(DeliveryTimeout)` |
| policy denial or final definitely-unpermitted hop | `Failed(Rejected)` |
| queue rollback, retained recovery, or invariant/control failure | `Failed(Internal)` |

Unknown destination or an empty eligible route maps to `NoPath`. Ledger/receipt
pressure and a generated receipt collision remain same-boot retry conditions
while the durable state stays `Preparing`; a reboot still terminates that
replay-unsafe state conservatively.

A real proof can establish `Delivered` while durable state is still
`Preparing`; the projector constructs the exact packet details from the
node-core preparation binding and commits a direct final transition. A timeout
can likewise become final before the frame record. A later exact frame
observation is idempotent against either final state and cannot reopen it.

The projector can expose two independent exact acknowledgements:

- terminal tombstone acknowledgement after the final disposition is durable;
- recovered-buffer acknowledgement after its transport audit is durable.

`PacketStillBound` is retryable ordering, not permission to discard the action.
Terminal and recovery acknowledgements may arrive in either order and neither
may erase the other's correlation. Quarantine has no release acknowledgement;
its unique owner remains fail-closed. A recovery-correlated owner trapped inside
a permanently disabled DATA-machine residue is exposed by the supervisor only
as a quarantine observation and follows that no-release path; its residue kind
and supervisor fault remain separate diagnostics.

## Crash and retry expectations

| Interruption | Required result after recovery |
| --- | --- |
| before `Accepted` commit | no submission ID was published |
| after `Accepted` commit but before reply | readback/dedup returns the same ID |
| before `Preparing` commit | submission remains replay-safe `Queued` |
| after `Preparing` commit | reboot finalizes internal; it never blindly resends |
| after a later record commit but before storage reply | retry/readback proves the exact logical record; no duplicate semantic transition |
| after final/audit commit but before node acknowledgement | the exact acknowledgement remains withheld/retried; the owner or tombstone is not reused |
| while a record prefix/marker is torn | full scan ignores the uncommitted hole, consumes its physical position, and never reports it committed |
| while a committed record is corrupt or contradictory | mount fails closed and exposes no valid-looking replay prefix |

Host tests inject lost replies, repeated observations, conflicting metadata,
wrong generation handles, retryable acknowledgement ordering, and terminal /
recovery arrival in both orders. The physical journal's fake-NOR tests also
inject partial and lost-reply writes/erases across append and compaction phases.
The powered clean-path/software-reset HIL has passed, but powered flash fault
injection is still a separate acceptance gate.

## Backend and actor contract

The physical journal implements the record-level portions below. The portable
sole storage actor now preserves them while adding one serialization cell,
projector ownership, exact autonomous retry, live-index ordering, and a bounded
fault latch. The transport-neutral submission runtime now implements the
storage/node lifecycle ordering. Device-API transport serving, executor
scheduling, checked partition ownership, watchdogs, OTA, other flash users, and
radio timing still belong to the permanent product task.

1. **Complete replay before action.** Scan and integrity-validate records to a
   known end-of-log, then consume `SubmissionReplay` into the live index. No queued
   work, boot recovery, or device response is allowed during a partial scan.
   The first semantic record rejection poisons the replay builder, and replay
   completion returns that error rather than exposing a valid-looking prefix.
2. **Idempotent logical records.** Key every revisioned record by submission
   and revision, with the acceptance's principal/idempotency key indexed
   separately. A transition and audit at the same revision conflict rather than
   forming two keys. After an ambiguous write result, read before rewriting;
   identical bytes are equivalent and different bytes are a fault. Blind
   duplicate append is insufficient because it consumes physical space that the
   semantic index cannot see.
3. **Real admission reservation.** Before publishing `Accepted`, reserve
   physical space for the complete worst-case lifecycle and a possible
   transport audit. Semantic schema 2 permits at most five
   committed semantic records per submission: one `Accepted`, at most three
   state transitions, and at most one transport audit. The physical journal
   contains 812 slots and admits at most 162 acceptances, reserving 810
   lifetime records and leaving two slots. Torn holes trigger packing compaction
   rather than consuming semantic reservation permanently.
4. **Power-fail integrity and order.** Use a cryptographic digest (or another
   explicitly justified corruption-detection code), explicit commit markers,
   monotonically ordered record identity, and scan
   rules for erased, torn, corrupt, duplicate, and stale records.
5. **Permanent retention and compaction.** Semantic schema 2 compaction copies every
   committed record, including principal/idempotency history, and provides no
   eviction or garbage collection. The manifest and future schema-migration
   rules must preserve that guarantee. The submission journal is not the
   separate long-term message/blob archive.
6. **Serialized non-cancellable writes.** The portable actor owns the semantic
   mutation and borrows one exact bound backend view for each synchronous
   attempt, retaining an exact pending mutation whenever the result is
   ambiguous. The resident product coordinator owns flash, and the portable
   runtime drives that ambiguity before other lifecycle work. That coordinator
   must additionally order OTA/GC/watchdogs and radio timing and must not expose
   cancellation across an actor call.

## Selected physical-format-1, semantic-schema-2 design

Semantic schema 2 uses the implemented project-owned fixed-slot, two-bank NOR journal in
a dedicated 1 MiB `retlog` partition. The partition reserves two 4 KiB manifest
sectors and divides the remaining erase-aligned space into two 127-sector
banks. Each bank has 812 640-byte physical slots and a required 512-byte erased
tail. A slot contains a 64-byte versioned header, maximum 512-byte canonical
semantic body, 32-byte SHA-256 chain value, and 32-byte commit marker. See
[Physical submission journal](storage-journal.md) for exact offsets and fields.

The physical version stays at 1 while the semantic schema advances. A valid,
trajectory-consistent schema-1 authority returns typed
`UnsupportedSemanticVersion(1)` before record replay and without a write or
erase; its acceptance records cannot be upgraded truthfully because they never
contained credential/policy evidence. Development migration therefore erases
and explicitly reprovisions only `node_journal`, as specified by
[ADR 0008](adr/0008-durable-authorization-provenance.md).

For each append, the journal writes the header, canonical body, and integrity
fields, reads those pre-commit bytes back exactly, and writes the commit marker
last. A record is visible to replay only after that sequence. Boot validates the
selected bank as a whole against its superblock/manifest and then feeds every
committed record through the semantic replay builder. A corrupt or
contradictory committed record fails the bank; replay never salvages a
valid-looking prefix and starts node/API work from it.

Compaction is record-bank-preserving and manifest-proved. It first commits a
handoff inside the selected source manifest, then erases and streams every
retained record into the inactive target, reads each commit back, and commits
the target manifest last. That seal makes the consecutive newer generation
authoritative. Append remains blocked until a third erase retires only the old
manifest sector; the old record bank stays intact. A torn or committed handoff
blocks append and makes the copy resumable, while a power loss during manifest
retirement resumes that one erase without creating another generation. Once
retired, the old bank cannot be selected as fallback, so corruption of the sole
active manifest fails closed and a later suffix cannot disappear through
rollback. Schema 2 retains every accepted submission and revision permanently
and exposes no eviction or garbage-collection policy. Admission fails when the
fixed index or 162-submission lifetime reservation cannot support another
submission.

The portable implementation uses raw NOR semantics through `embedded-storage`.
The Tracker HIL adds a checked partition-relative `esp-storage` adapter; it does
not use generic byte-oriented `Storage::write` or the pinned ESP-IDF
`FlashRegion`. The current format requires exact 4-byte read/program alignment,
4 KiB erase alignment, and `MultiwriteNorFlash`. Formatting is explicit and
only accepts a completely erased partition; it never erases or reformats an
unknown nonblank partition.

`sequential-storage 8` remains useful research and differential-test material,
but it is not an open contender for the physical-format-1 journal implementation.

## Capacity and ESP32-S3 constraints

The current intent owns up to 383 payload bytes; a maximum schema-2 `Accepted`
record uses 508 of the 512 canonical bytes, and each in-RAM indexed submission retains that
intent plus lifecycle metadata. Submission counts therefore need explicit
static layout, stack, heap, boot-scan-time, and linked-image measurements on
the no-PSRAM Tracker profile. The codec currently uses a 512-byte canonical
scratch value during strict decode, so the eventual storage task must use a
deliberately sized stack or caller-owned static scratch rather than rely on a
large incidental async frame.

The current projector is also intentionally correctness-first rather than a
finished Tracker RAM profile: the current ESP32-S3 layout measures about 1,288
bytes for `SubmissionSlot` and about 640 bytes for its retained
`PendingRecord`. The actor now serializes physical writes through one global
pending-mutation cell rather than adding another complete plan per submission;
its actual optional cell is compile-time capped at 512 bytes. This does not cap
the projector slots, live index, complete actor, or future task stack. Measure
that full static layout and stack before selecting a Tracker capacity. Completed
correlations also cannot be retired automatically until product integration
proves that every terminal and transport observation source has been drained.

Profiles on larger boards may enable a larger index and richer clients. The
semantic feature set remains portable; constrained boards may disable local
LXMF/NomadNet/UI services without redefining the durable protocol.

## Remaining implementation gates

1. Extend the completed clean-path/software-reset Heltec qualification with
   controlled powered cuts at the relevant program/erase boundaries, preserving
   each image, readback, continuous serial capture, and raw-partition result as
   a separate evidence set.
2. Extend the implemented E290 first-provision composition with powered cuts.
   While identity is independently `Vacant`, `provision_first()` now resumes
   every monotonic-compatible canonical A1 prefix/commit cut without erasing;
   existing identities skip provisioning and use strict mount only. A future
   in-field migration needs its own durable config intent.
3. Preserve the two passing E290 cross-layer host tests: the authenticated
   zero-write/one-acceptance/barrier/LoRa/durable-echo/timeout/remount path and
   the wrong-binding post-frame `ActiveOwnerFailStopped` path with queued
   ordinary work and no later host-radio operation. The one-entry cap is a
   qualified composition profile, not product capacity.
4. Preserve the connected explicit initialization and live Begin/Proof/
   Activate/Abort ownership plus the composed authority, framing,
   qualification-session core, boot-lifetime job handoff, and minimal
   authenticated USB API bearer. Complete the powered credential lifecycle and
   one authenticated request/reply. Those are the missing proofs for live
   authenticated admission.
   Keep the local client API distinct from the node's Reticulum interface
   selection. No second interface is required; later Reticulum transports use
   the same transport-neutral runtime and router contract.
5. Add an exact quiescence proof from the sole node owner before reusing a
   completed projector slot. A final record plus terminal acknowledgement is
   insufficient because valid recovery may arrive later; permanent quarantine
   also needs an explicit release or durable suppression mechanism.
6. Choose an explicit journal-retention/export/migration policy. Schema 2 keeps
   every record and principal/idempotency history, admits at most 162
   submissions for the partition lifetime, and implements no eviction or
   garbage collection.
7. Measure static layout, journal scan/compaction time, stack, erase endurance,
   soak behavior, flash/watchdog contention, and radio-deadline impact on
   ESP32-S3 and larger profiles. The two attached boards have antennas,
   confirmed `HT-RA62-HF` modules, and a passed isolated NA915 semantic HIL;
   preserve an explicit regional/airtime profile and sole radio owner during
   the still-unqualified permanent storage/radio integration.
