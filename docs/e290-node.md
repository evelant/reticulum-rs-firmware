# Permanent Vision Master E290 node image

**Status:** the first permanent, LoRa-first image is implemented. Its release
record below captures the host-library, host-client, portable-target, ESP32-S3,
strict review, graph, image-size, and same-image readback gates. Its
third task now owns a USB Serial/JTAG pre-authentication initialization and live-
pairing bearer, one shared exact-next sequence space, debounced GPIO21 physical
presence, an interrupt-linearized reset-epoch guard, and an application-entry
USB boot quarantine. Powered work has now completed the full first outbound path
on the two E290s. MAC `ac:a7:04:e1:3e:88` retained its button-confirmed empty-
store initialization, durable Active generation 3, and host credential; MAC
`ac:a7:04:e1:3f:88` owns a separate durable node identity. Both ran the exact
same current image with matching address-zero readbacks. An authenticated API
1.1 session read the sender's public primary destination, durably accepted one
RNS DATA submission, and stayed open while polling status. The peer matched its
own destination, decrypted the packet, returned a valid Reticulum proof, and
the sender durably projected `Delivered`. A full sender USB re-enumeration and
fresh authenticated session returned the same terminal metadata. Earlier
controls returned
`initialization-required`, enforced GPIO21 for initialization and live Begin,
rejected stale sequence zero, and restored a fresh epoch after full host
re-enumeration. Both preceding boot-quarantined image readbacks matched exactly,
both boards served sequence zero again after the induced hard reset, and
120-second no-button workflows left both credential partitions erased. Exact
Pending and Abort readbacks, mutation ambiguity/fault cuts, and broader powered
lifecycle qualification remain open. Source `5f3f259` passed the earlier
bounded powered upgrade smoke on both `HT-RA62-HF` boards: exact same-image readback,
resident pairing-policy and erased-initialization eligibility, zero credential
mutation, journal/LoRa/interface startup, and ordinary one-frame TX. Full
powered product-graph, USB suspend/resume, power-cut behavior, and the ROM/
bootloader interval before the earliest Rust entrypoint remain open.
The permanent source graph now additionally composes the feature-free
authenticated session and handoff crates, a static depth-one request/reply
handoff, node-side current-authority dispatch, and the first deliberately
minimal authenticated USB session bearer. It accepts one handshake per USB
connection and one request at a time, and a session fault remains terminal
until reset or re-enumeration. Resumption, protocol retries, close records,
encryption, rate limiting/attempt policy, repeated handshake attempts, and
concurrent requests are deferred. Credential selection, admission handoff, and
node dispatch are bearer-neutral. The current qualification suite is USB
Serial/JTAG-only; BLE or Wi-Fi can later reuse the ownership boundary after its
binding/suite is added and qualified. The powered proof now qualifies
authenticated capabilities and identity reads, sequential request/response
flights in one session, durable submission, LoRa DATA delivery, peer decrypt/
proof, terminal projection, and status after USB re-enumeration. It does not
claim application-level message consumption, session resumption, or either
deferred wireless bearer binding.

This target is the first executable product composition, not another HIL
fixture. It starts a transport-mode Rete node, one E290 LoRa actor, receive and
transmit scheduling, routed DATA and ordinary-action ownership, periodic
protocol maintenance, and local announces. It now also owns a power-loss-safe
device identity and restart-safe announce-emission clock, validates and safely
first-provisions the exact node-journal partition, and strictly completes a
submission-runtime recovery gate before constructing node or radio service. It
then transfers the sole flash backend and mounted runtime into a resident
operation-scoped storage coordinator that the node task schedules throughout
the firmware lifetime. An optional journal mount/recovery failure occurs before
any durability-gated DATA owner can exist; it disables local durable submission
service while the LoRa node still starts in route-only mode. The exact
authorized-frame request/durable-echo handoff is source-composed and now passes
cross-layer host qualification. The one-entry accepted-history cap is exercised
by that harness solely as a composition profile and is not a product-capacity
commitment. Portable API framing, a featureless pre-authentication
initialization-control codec, immutable credential authority, the
qualification-session core, and the boot-lifetime job handoff are qualified;
semantic schema 2 persists exact authorization provenance. The dedicated
credential-partition contract and portable store are selected in ADR 0009; its
initial developer/HIL pairing-admission policy is now implemented as a separate
portable crate. The store is boot-mounted, deterministically recovered, and
retained by the resident coordinator. Lifecycle-specific Add/Activate/Abort
planners, opaque typed store commit/reconcile owners, mounted-store pending
selection, and a read-only four-way interrupted-initialization classifier now
pass their portable gates. E290 boot now consumes that classifier read-only and
maps only its canonical interrupted trajectory to an explicit disabled state;
it does not recover or initialize media automatically. The feature-free policy
is now a permanent-E290-only dependency, resident inside the coordinator's
`CredentialRuntime` with the exact boot binding, any mounted authority, any
admitted initialization permit, and every live-pairing proof/store owner. The
runtime implements bounded entropy, Begin/ProofStart/Activate/AbortCurrent,
cleanup-before-next-mutation, and ambiguous-result reconciliation. A compiled
bearer-neutral depth-one handoff preserves exact secret-bearing owners under
pressure and is split between the USB and node tasks. The node schedules live
requests against control events and journal mutation by their captured causal
frontier, retains exact request correlation through durable drive/retry, and
returns success only after the matching commit. A sole USB Serial/JTAG/GPIO task
terminates the six zero-session, zero-tag initialization/live-pairing request
kinds through one decoder and one sequence gate. The first SOF establishes
a boot-lifetime connection epoch; an 8 ms missed-SOF interval suspends endpoint
work without changing that epoch or its exact-next sequence; a later SOF resumes
it; and only a USB bus reset retires it so a subsequent SOF can allocate the next
epoch. The task also debounces active-low GPIO21 and exchanges scalar commands/
replies with the node through depth-one channels. An unexpected bus reset
increments an ISR-owned generation, blocks RX and TX, forces the USB pad off,
retires old response ownership, scrubs USB RAM, and only admits a replacement
epoch after an interrupt-linearized scrubbed reattach and clean reset. The opaque exclusivity
capability and the sole flash coordinator remain node-owned. The bootstrap
records remain distinct from both the authenticated session and a Reticulum
packet interface. Successful physical initialization and the minimal rebooted
authenticated capabilities exchange are now qualified on one powered board.
Separately, the USB task now drives the minimal authenticated
session described below, and the node receives exact authenticated request owners,
revalidates each grant against the currently publishable authority, and invokes
logical dispatch synchronously through a short-lived submission-port view that
cannot borrow credential records. Revoked or missing credentials return only
the generic authentication-required response with zero port I/O and no
unauthenticated fallback. Source-level external admission now reaches that lane
through the single-flight USB bearer, and the powered happy path above exercises
it after durable activation and reboot. Exact Pending/Abort readbacks,
activation ambiguity, failure cuts, and repeated-session behavior remain open. ADR
0005's active-owner policy is implemented: a
permanent fault
with an unresolved frame enters interface-local `ActiveOwnerFailStopped`, takes
the same LoRa lease offline without changing its generation, retains the exact
frame/completion/ticket, and permits no fresh LoRa work for the rest of the boot.
Device configuration, message storage, client delivery, LXMF/NomadNet, and
production-ready host-facing USB/BLE/Wi-Fi services remain visible product
blockers.

## Composition boundary

```text
transport-neutral node task
  NodeInterfaceSupervisor
    NodeCore in transport mode
    DATA and ordinary-action coordinators
    permit servers and shared authorization policy
    bounded ingress, completion, tick and announce lanes
  ProductStorageCoordinator
    resident sole flash backend
    CredentialRuntime
      retained boot binding + optional MountedCredentialStore
      feature-free PairingPolicy + private initialization permit
      forward-only erased/interrupted physical drive
      resident live-pairing proof + typed mutation/reconciliation owners
    SubmissionRuntime + operation-scoped BoundJournal views
    exact authorized-frame retain/re-offer + durable echo
  depth-one pre-auth control command/reply handoff
  depth-one bearer-neutral live-pairing command/reply handoff
  authenticated API node lane
    current-authority revalidation + synchronous logical dispatch
    disjoint short-lived SubmissionPort view
    retained reply pressure + terminal malformed-owner quarantine
  depth-one authenticated request/reply handoff
  captured-time causal frontier before shared flash mutation
             |
       InterfaceFabric slot 0
       ticketed jobs/completions
       exact reusable RX buffers
             |
permanent LoRa actor task
  InterfaceIngressActorHandoff
  TimedRnodeRx
  SoleRadioTxDispatcher
    post-byte-exposure DATA completion/router-ticket gate
  E290Radio / SoleRnodeRadio

pre-authentication USB/GPIO task
  sole USB Serial/JTAG RX/TX owner
  one COBS decoder and sequence gate for status/initialize plus
    Begin/ProofStart/Activate/AbortCurrent
  boot-lifetime connection epoch + exact-next sequence
  reset ISR generation + pad-off/RAM-scrub/clean-reattach guard
  active-low GPIO21 stable-time debounce
  minimal authenticated USB session bearer
    one handshake per connection; one request in flight
    fault terminal until USB reset/re-enumeration
    no resumption, retries, close records, encryption, rate/attempt policy,
      repeated attempts, or concurrency in this first profile
  no Reticulum interface capability
```

LoRa remains deliberately the primary and only concrete transport actor in
this first slice. The node
owner depends on interface descriptors, leases, queues and resource permits;
it does not know about SX1262 pins, LoRa framing or radio futures. A later
Reticulum transport is an adapter added by increasing the product slot profile,
registering another interface descriptor, and spawning an actor that owns that
slot. A composite authorization policy will dispatch resource accounting by
interface. USB/BLE/Wi-Fi client access is a separate device-API capability and
does not need to masquerade as a Reticulum packet interface.

The initial fixed capacities are:

| Resource | Capacity |
| --- | ---: |
| Rete paths | 16 |
| Pending local announces | 4 |
| Deduplication entries | 32 |
| Rete links | 4 |
| DATA buffers | 4 |
| Ordinary-action buffers | 8 |
| Interface slots | 1 |
| Jobs, completions and ingress buffers per slot | 2 |

These are named product-profile constants, not cross-crate architectural
limits.

## Scheduler and RF policy

The LoRa task gives an idle radio one bounded receive operation before checking
the TX queue. A partial RNode packet retains receive priority until completion
or the profile-derived fragment deadline. Completed bytes move into an exact
fabric-owned ingress buffer. If the ingress queue is full, the sealed packet is
retained unchanged; if no reusable buffer exists, the task skips RX and gives
TX one turn. Once a ticketed TX owner is dequeued, the dispatcher drives it
through backoff, CAD, resource permission, one logical one/two-frame transmit,
and exact completion return before resuming receive service.

The NA915 development profile currently uses a maximum of three CAD attempts
and a randomized 24--360 ms backoff interval, preserving the reference RNode
24 ms slot and complete 15-slot contention envelope. Busy exhaustion rejects
the attempt; it never force-transmits. The exact maximum 500-byte logical
packet airtime is 821,760 us. The 1,500,000 us whole-TX watchdog covers that
airtime plus named 50,000 us pre-RF, 25,000 us inter-frame, and 500,000 us
driver/scheduler allowances. CAD has a separate 500,000 us watchdog.

Dropped CAD, TX and RX futures enter the dispatcher's explicit cancellation
recovery. Ticketed completion is drained before terminal disablement, so a
packet owner is not stranded. Other terminal actor paths stop scheduling any
further radio operations; they do not claim that an independent hardware
shutdown occurred. Restart/reinitialization and actor-to-registry offline
signaling are later lifecycle work.

The node task rotates across queued ingress, supervisor/permit progression,
RNS maintenance, local announces, and one resident durable-runtime step. Each
storage step borrows a bound journal view for only that physical operation. A
backend or busy result receives bounded retry. A permanent runtime fault with
no active durability-gated DATA owner disables local durable service while the
LoRa lanes continue; with an unresolved active owner, the node enters
`ActiveOwnerFailStopped`, takes the same LoRa lease offline without changing its
generation, and retains the observation/completion/ticket while admitting no
later RF operation. The storage lane is normally idle until an authenticated
USB client submits work; the current bounded run completed this path through
peer proof and post-re-enumeration status. The task performs
at most 16 immediate passes, yields, and currently uses a temporary 1 ms idle
poll because the aggregate does not yet expose one combined readiness/deadline
wait.

A permanent DATA coordinator, ordinary coordinator, or permit-service fault is
logged as `FAIL-CLOSED-DRAIN`, not treated as anonymous progress. Fresh work is
denied while the task continues stepping coordinator, permit, and completion
lanes so owners already admitted to those machines can return. Terminal
ingress actions are quarantined locally, or left as explicit supervisor residue
if that slot is already occupied, only after any simultaneously backpressured
sealed RX buffer has returned to its actor pool. Pre-admission local retry and
supervisor-ingress envelopes are quarantined in place and are not re-offered.
If returning a sealed RX buffer fails for anything other than a full actor
queue, the task takes and retains that exact packet as terminal quarantine
rather than retrying an invariant failure forever.
The task then remains alive solely to drain already-admitted work; it does not
dequeue fresh ingress, tick, or announce.

## Memory and flash profile

The image autodetects ESP32-S3 PSRAM and refuses to continue unless the mapped
capacity is between the qualified 8 MiB floor and the board datasheet's 16 MiB
claim. Fixed channels, task storage, permit stores and ownership state remain
in internal static RAM. The allocator receives 64 KiB of reclaimed internal
RAM plus the detected PSRAM for growth-oriented protocol and future client
allocations. Future atomic or `Arc`-backed allocations must be audited before
placing their storage in external RAM.

The target requires a 16 MiB flash image/header and uses
[`partitions/heltec-vision-master-e290-node.csv`](../partitions/heltec-vision-master-e290-node.csv):

| Region | Offset | Size | Current use |
| --- | ---: | ---: | --- |
| NVS | `0x009000` | 24 KiB | ESP/NVS reserve |
| PHY init | `0x00f000` | 4 KiB | ESP PHY reserve |
| Factory app | `0x010000` | 6 MiB | Permanent node ELF |
| Node identity | `0x610000` | 8 KiB | Wired, mirrored plaintext private identity |
| Announce clock | `0x612000` | 8 KiB | Wired, mirrored boot-epoch append logs |
| API credentials | `0x614000` | 8 KiB | Wired boot mount/recovery; exact eFuse-derived binding; retained plaintext two-sector store; no automatic provisioning |
| Device config | `0x616000` | 104 KiB | Reserved, not wired |
| Node journal | `0x630000` | 1 MiB | Resident operation-scoped submission runtime; one-entry qualification cap; authenticated submission and post-re-enumeration terminal status powered-qualified |
| Message store | `0x730000` | 2 MiB | Reserved, not wired |
| Unallocated | `0x930000` | 6.8125 MiB | OTA/layout decision |

The workspace runner in `.cargo/config.toml` hardcodes an 8 MiB flash size and
must not be used for this target.

`node_identity`, `announce_clock`, and `api_credentials` use ESP-IDF's standard
`data,undefined` subtype. All three have application-owned formats; the
credential range is checked, boot-mounted/recovered, and retained. Explicit
initialization and ADR 0010 live pairing are routed through the resident owner;
minimal single-flight authenticated USB session/API serving is powered-qualified
through identity, durable submission, sequential status, peer proof, and a
post-re-enumeration terminal status read.
`device_config`
retains the standard NVS subtype while it is unwired; the application-owned
journal and unwired message store retain `data,undefined`. Their labels and
ranges remain distinct. Numeric custom subtypes are only valid with custom
partition types in the image tooling and are not used here.

### Durable identity, journal and announce ordering

After partition validation, `ProductFlashOwner` derives the credential binding
from the exact same eFuse-based physical-device ID used for the journal and
mounts/recovers `api_credentials` immediately after flash open. A mechanical
host regression requires that call to precede identity preflight, journal
provisioning, announce-clock reservation, identity load/provision, and journal
mount, so credential recovery is complete before any other product-store write.
Mount is read-only and never auto-provisions erased media. Boot attempts at most
one reported `RetirePredecessor` operation and then at most one
`CleanupInactive` operation, retaining any mounted owner in
`ProductStorageCoordinator`.

The portable store can now distinguish exactly erased media, the one canonical
recoverably interrupted empty revision-1 trajectory, an already committed
empty revision 1, and ineligible media without mutation. This firmware boot
path invokes that classifier only after normal mount reports programmed
unformatted media. Only `RecoverableInterrupted` becomes
`InitializationInterrupted`; ineligible or logically contradictory results
remain corrupt, while classifier binding and backend failures retain their
distinct fail-closed phases. Classification never writes or erases, mounts no
authority, and confers no mutation eligibility. There is still no resident
automatic boot recovery; initialization remains an explicit request-time path.

The boot outcome is consumed into a resident `CredentialRuntime` inside
`ProductStorageCoordinator`. That runtime privately retains the exact credential
binding, any mounted authority, the feature-free pairing policy, and any
admitted initialization permit. Its physical drive freshly reclassifies media
and accepts only forward progress along the exact erased or recoverably
interrupted trajectory; binding mismatch, backward movement, noncanonical
completion, or stable media faults block further initialization for that boot,
while backend/readback ambiguity retains the permit for a same-boot retry.
The sole coordinator's cross-store gate defers initialization admission behind
retained journal actor/projector work, and defers journal physical drive or new
submission acceptance while credential initialization is in flight. The latter
is an explicit retry state, not runtime failure; projection, status, routing,
and LoRa remain available.

The seven product admission classes are `Ready`; `AuthOnly` (the Rust
`AuthenticationOnly` variant, logged as `AUTHENTICATION-ONLY`, with existing
authority publishable but mutation disabled); `Uninitialized` (the
`UninitializedErased` variant); `InitializationInterrupted`; `Blocked`;
`Corrupt`; and `Backend`.
Deterministic boot
retirement/cleanup failure quarantines only credential admission or mutation:
the owner and failure state remain resident, while journal policy and route-only
LoRa startup continue unchanged. No boot classification starts a session or
pairing flow by itself. A later USB hello may select an ordinary session only
from a currently publishable authority in `Ready` or `AuthOnly`; other classes
refuse opaquely. The narrow pre-authentication records expose only coarse status
and explicit empty-store initialization and cannot mint an authenticated grant.

`node_identity` is exactly two 4 KiB erase sectors. Each sector contains one
256-byte record with a fixed claim, versioned header, the exact 64-byte
Reticulum combined private key, reserved bytes, SHA-256 integrity, and a commit
marker programmed last; the rest of each sector must be erased. The key is
mirrored but **not encrypted**. The current developer image rejects enabled
ESP flash encryption, so a raw dump contains the private key twice and must be
handled as a secret.

Boot first performs a complete, mutation-free identity preflight. Blank or
recognizable torn-only media is `Vacant`; one or two matching committed copies
are `Committed`. Unknown programmed data without a valid copy, sole committed
corruption, or two valid but different keys fails closed before the announce
clock is touched. Preflight performs zero program and zero erase operations and
returns only non-secret coverage metadata.

The preflight result controls the clock policy. A vacant identity permits a
fresh clock, while a committed identity requires an existing clock high-water
record. The firmware reserves the next announce epoch in `announce_clock`
**before** it provisions or repairs the identity. Consequently, a power cut can
skip an epoch but cannot leave a persistent identity that was allowed to emit
with a reused or invented clock. A committed identity with blank or torn-only
clock media fails with `MissingHighWater` without mutating either partition.
Unknown or solely corrupt clock media also fails closed when no valid
high-water copy exists.

`announce_clock` is another two-sector 8 KiB format. Each sector is a 32-slot
append log of 128-byte commit-last SHA-256 records. A successful boot commits
the same next 20-bit epoch to both sectors before protocol service starts. A
volatile 20-bit ordinal forms the lower half of each local announce timestamp:
`(boot_epoch << 20) | per_boot_ordinal`. The ordinal advances only after RNS
accepts that signed announce into its owned queue. Exhaustion suppresses local
announces instead of wrapping. Rotation erases one sector only while the other
preserves the previous high-water value; retry after an ambiguous operation
rescans and advances past any record that may have committed.

On normal erased first provisioning, clock reservation performs four program
calls (prefix then commit in each sector), followed by six identity program
calls (claim, body, then commit in each mirror), with no erase. A normal reboot
does no identity writes or erases; it appends one clock record to each sector,
again four program calls and normally no erase. Clock sectors rotate after
their 32 slots are consumed or when a valid peer permits damage repair.
Identity repair writes only the non-authoritative peer and never erases the
sole valid copy. The product requires redundant identity coverage before
starting the node and remains inert if repair cannot establish it.

The same mutation-free identity preflight is independent authority for the
first journal format. While identity is `Vacant`, boot first calls
`provision_first(AllowFirstProvision)`, then reserves the announce epoch, all
before committing identity. This avoids consuming an epoch when the full
journal scan cannot establish the first format. The journal accepts only
completely erased media, an already-valid
empty generation-1 A bank, or a monotonic-compatible interruption of that
exact 160-byte manifest prefix/commit sequence; everything outside the first
manifest must be erased, and provisioning never erases. Thus every recognized
first-write cut can resume while identity remains vacant, while arbitrary or
nonempty media fails closed. Once identity is committed, provisioning is
skipped and only strict journal mount is permitted.

After identity reaches redundant coverage, `SubmissionRuntime` strictly mounts
the checked 1 MiB region and permits at most one accepted historical submission
before making any recovery mutation. That one-entry limit exists solely for
composition qualification and is not product capacity. It drives recovery
through `RecoveryStep::Complete`,
then moves into `ProductStorageCoordinator` with the sole physical flash owner.
The node task drives that resident runtime in its fifth fair lane; each physical
operation creates a short-lived `BoundJournal` over the exact partition and
releases the borrow afterward. This preserves one flash authority without
locking future configuration, message-store, and OTA work into a permanent
journal-only borrow.

Journal mount, unsupported history, or recovery failure is isolated because it
occurs during boot before a durability-gated DATA owner can exist: the
coordinator retains the flash backend with no runtime, local durable admission
remains closed, and the LoRa node/radio tasks still start in route-only mode.
The accepted-history cap is one for qualification. The minimal authenticated
USB edge now has powered initialize/pair/reboot/capabilities/identity evidence
plus one durable submission whose LoRa DATA/proof terminal state survived USB
re-enumeration. The
LoRa actor now hands the exact `AuthorizedFrameObservation` to the node/storage
owner while its dispatcher retains the completion and router ticket. The node
retains and re-offers that observation until the runtime returns `Durable`,
then echoes the identical scalar to release the dispatcher. The same transport-
neutral adapter contract applies to later packet interfaces. A permanent fault
before an unresolved owner exists selects `DisabledRouteOnly`; with an
unresolved owner it selects `ActiveOwnerFailStopped`. A request racing with the
route-only transition promotes to the latter state, while an already-durable
acknowledgement waiting for capacity remains releasable. Admission remains gated
only at the external API edge: the host harness now qualifies this composed
durability and failure behavior.

The resident `ProductStorageCoordinator` also implements the target-safe
device-API `SubmissionPort` for capability, principal-scoped status, and
`experimental-rns-data` acceptance semantics. That is only the product-side
semantic seam: the one-entry cap is not product capacity. Portable framing and
job handoff now enter the permanent graph through a static depth-one
authenticated request/reply channel. The node endpoint decodes the logical
request, revalidates its opaque grant against the resident current authority,
and calls the adapter synchronously through a credential-disjoint submission-
port view. Missing, revoked, replaced, or generation-mismatched credentials
produce a generic authentication-required response without port I/O or fallback.
Reply pressure retains the exact owner, while malformed logical CBOR is a
terminal retained fault rather than a redispatch candidate. The USB endpoint
now runs a fixed-capacity session manager with one handshake per connection and
one request in flight. It resets only at the connection boundary and fails
terminally until reset after a session fault. It intentionally has no
resumption, protocol retry, close record, encryption, rate/attempt policy,
repeated handshake, or concurrency yet. Its admission and request/reply
handoffs remain bearer-neutral for later BLE/Wi-Fi adapters, while each
non-USB binding still requires an explicitly enabled and qualified session
suite. The separate USB
bootstrap records remain pre-authentication and serve only initialization and
credential pairing.

The present singleton is bearer-neutral at the admission and node-dispatch
interfaces, not yet a concurrency namespace for several bearer actors. Before
BLE or Wi-Fi sessions run beside USB, use globally unique bearer-qualified
connection/session epochs, or give each bearer disjoint reply channels beneath
one global pairing-exclusivity coordinator. Independently starting every
bearer's epoch allocator at one and merging their replies is forbidden.

It also implements the sole-owner credential-initialization port. Each
request and drive freshly inspects `node_identity`; a physical drive then lends
one short-lived credential-partition view bound to the exact boot device/range/
layout before calling the resident runtime. The node task now invokes these
methods from a depth-one command/reply lane fed by the sole USB/GPIO task. That
task owns fixed COBS buffers, a stable-time active-low GPIO21 debouncer,
bus-reset-delimited nonzero connection epochs, exact-next request sequences,
stale-RX discard, and coarse response framing. A missed-SOF suspension retains
the epoch, sequence gate, and bytes already committed to the endpoint FIFO; it
does not represent disconnect. A powered macOS `USBDeviceReEnumerate` replaced
the service and restored sequence zero after firmware pull-off, USB-RAM scrub,
and reattachment. A non-seizing in-place `ResetDevice` returned success but
left the same BSD service silent; it is not an accepted recovery primitive.
The preceding boot-quarantined image reattached and served sequence zero on
both boards after the hard reset induced by exact flash readback. Suspend/resume
and the ROM/
bootloader interval before the application boot quarantine remain open. Live
Begin, ProofStart, Activate, and AbortCurrent are routed through this task and
durably driven by the resident credential owner.

Both operation-scoped views name the device with the domain-separated 16-byte
value `"e290-flash" || eFuse base MAC`. The credential view additionally fixes
absolute offset `0x614000`, length `0x2000`, and credential physical layout
version 1. The journal view fixes offset `0x630000`, length `0x100000`, and
journal physical layout version 1. Each store validates its exact values and
view capacity/alignment before I/O; every later borrowed operation must match
its retained binding exactly.

## Software composition and build gates

From the workspace root:

```sh
cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node --lib
cargo +stable clippy --locked \
  -p reticulum-heltec-vision-master-e290-node --lib -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked \
  -p reticulum-heltec-vision-master-e290-node --lib --no-deps
cargo +stable run --locked -p xtask -- graph-policy

source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --target xtensa-esp32s3-none-elf
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --target xtensa-esp32s3-none-elf -- -D warnings
```

The build script rejects an unreviewed `esp-rtos` main-stack implementation and
links `linkall.x`. Debug Xtensa builds are compile-time rejected.
The host library suite has passing policy/product/credential-boot/
credential-runtime/USB-control/live-routing tests, including the source-order
regressions, every canonical empty-initialization byte cut, adversarial media changes between
mount and classification, off-trajectory media, and classifier failure phases,
plus two real cross-layer composition tests. The happy path proves unauthenticated
and permission-denied requests cause zero NOR writes, exactly one authenticated
acceptance succeeds, and a second novel request reaches capacity without a
write. It then proves the durable `Preparing` barrier precedes node ownership,
drives the real `NodeInterfaceSupervisor`, exact E290 LoRa policy, and real
dispatcher through one DATA transmit, persists the exact authorized frame,
echoes it durably, and releases the completion. Delivery timeout, owner status,
foreign-principal `NotFound`, and remount of the durable final state complete the
path. The fault test injects a permanent wrong-binding error after frame
exposure with an ordinary announce queued behind it; the result is
`ActiveOwnerFailStopped`, no acknowledgement or completion, every owner retained,
and no later host-radio TX or RX. The focused tests include the exact
one-submission profile assertion and five focused durability-policy tests for
retry, route-only degradation, pending durable acknowledgement, sticky fail-stop,
and the request-after-disable race. Credential-runtime tests additionally
cover both initialization trajectories, fresh binding and identity checks,
forward-only media movement, ambiguous backend/readback retention, disconnect
ownership, policy completion, and fail-closed noncanonical states. Four
cross-store tests cover both retained journal owners, initialization before and
after physical I/O, stable credential states, and the distinct deferred versus
unavailable result. The USB-control additions cover stable-time active-low
debounce and held-low boot, SOF suspension and bus-reset-delimited epochs,
strict epoch and sequence exhaustion, duplicates and gaps, depth-one
command/reply pressure, capability-free handoff, coarse result mapping, and
mechanical third-task/sole-USB-owner/scheduling boundaries. They also freeze
bounded button/control arbitration, latch a stable High transition ahead of a
later Low, and treat a raw-sample gap of at least 20 ms as lost continuity: the
prior hold is cancelled and Low is suppressed until a fresh debounced High.
Each fresh connection resets both that publication latch and the debouncer to
Low, so release evidence retained for an older connection epoch cannot arm the
new epoch; the replacement epoch must observe a complete fresh High debounce.
The initialization-control host client has 12 focused tests for parsing,
default deadlines, single-open sequence progression, terminal result/exit
behavior, ambiguity guidance, and sequence exhaustion. The live-pairing client
adds 15 tests for its CLI contract, exact response-family correlation and
device-ID validation, pair/resume sequence headroom, owner-only reservation and
atomic Pending persistence, fixed 96-byte binary layout, and atomic complete
Pending-to-Active replacement that persists the HMAC-bound durable Active
generation while retaining owner-only permissions, and secret-free output. The
bounded authenticated client adds 15 tests for operation parsing, public-
identity formatting, sequential request IDs, version policy, polling terminal
semantics, coalesced-record preservation, and submission-input non-disclosure.
Together these 42 focused tests are part of the full 189-test xtask gate.

Once all response bytes enter the endpoint FIFO, firmware requests hardware
`WR_DONE` and releases the software response owner without waiting for a later
completion observation. The hardware then owns that response; any later response
remains losslessly backpressured at the FIFO until space is available. This avoids
deadlocking RX after the host has already received a frame without weakening
software ownership of a response that has not yet entered the FIFO.

Because USB Serial/JTAG hardware can retain those hardware-owned bytes across a
CPU/core reset, the image quarantines USB at the earliest application entry. It
forces the native pad off, power-cycles USB memory, keeps the pad detached
through product initialization, and installs the reset ISR before restoring the
canonical attached configuration. No traffic is admitted until the expected
enumeration reset names a clean epoch. Runtime bus reset applies the same
block/detach/scrub/reattach gate. The ROM and bootloader interval before the
earliest Rust entry remains a boot-chain residual, not a claimed erasure point.

The permanent image selects only `esp-println` features
`esp32s3,log-04,no-op` and does not initialize the logger. Application,
framework, and panic log text therefore cannot write the USB Serial/JTAG FIFO;
the pre-authentication COBS records have the sole application-owned byte stream.
This deliberately removes the earlier native-USB boot log as a validation
surface. Powered work must use the typed control responses, RF evidence, and
exact flash readback, or a separately designed diagnostic sink.

Separate ESP release builds made before the authenticated-node-foundation slice
with `-Z emit-stack-sizes` produced 1,025 fully symbolized records and identical
complete frame-size multisets for the default and journal-migration-permitted
variants. The largest frames are
`NodeCore::new` at 52,752 bytes, the Embassy main poll closure at 42,960 bytes,
`ProductFlashOwner::boot_credentials` at 27,488 bytes, and
`NodeInterfaceSupervisor::try_new` at 21,440 bytes. Disassembly establishes a
direct main-frame call to `NodeCore::new`, so that path has a 95,712-byte static
lower bound before deeper callees and interrupt context. The linked CPU0 stack
reservation is 176,268 bytes in the default image and 176,276 bytes in the
migration-permitted image. These compiler records are not runtime high-water
evidence, and the 52,752-byte maximum exceeds the Tracker-only 48 KiB frame
ceiling; an E290-specific static gate plus powered stack instrumentation remain
required.

The earlier credential-runtime-composed release at source `5f3f259` is
659,035 bytes text, 11,464 bytes initialized data, and 461,364 bytes BSS/
reservations by GNU size; the packaged application is 670,608 of 6,291,456 bytes
(10.66% of the factory slot). The unpadded merged image is 736,144 bytes with
SHA-256
`f422a8003762f9579ee0f4faf8c85cf78961327f7bb2c6db8c8878bc071d389b`. CI
retains explicit growth headroom rather than treating this early image as the
full appliance ceiling.

The historical resident-live-pairing-before-USB-integration slice linked at 547,915 bytes text, 3,548
bytes initialized data, 469,280 bytes BSS/reservations, and 1,020,743 bytes
total by GNU size. Its packaged application is 590,960 of 6,291,456 bytes
(9.39% of the factory slot); the unpadded merged image is 656,496 bytes with
SHA-256
`9e788486c0621f0a2c32049b7df6522259b510fd2e080d26b83e5c5228ffc564`.
The depth-one pairing handoff contributes a 144-byte static owner and the USB
task pool is 2,016 bytes. These values remain below the unchanged CI ceilings of
720,896/16,384/475,136/1,180,000 bytes, leaving 5,856 bytes under the aggregate
BSS guard. They are host/link/package evidence, not runtime stack-high-water,
powered-memory, or flashed-hardware results.

The preceding boot-quarantined routed live-pairing release links at 594,219 bytes
text, 3,572 bytes initialized data, 469,256 bytes BSS/reservations, and
1,067,047 bytes total by GNU size. Its packaged application is 636,208 of
6,291,456 bytes (10.11%); the unpadded merged image is 701,744 bytes with
SHA-256
`14d9fd6dd482c47baa9afd2fda6a5ba1d69f46785bf23ae29f6b9fe561e4b212`.
Exact reads of that complete range matched on both powered boards. The values
remain under the unchanged CI caps; runtime stack high-water and heap pressure
are still not measured.

The last powered authenticated-node-foundation release links at 611,479 bytes text,
3,580 bytes initialized data, 469,248 bytes BSS/reservations, and 1,084,307
bytes total by GNU size. Its packaged application is 653,152 of 6,291,456 bytes
(10.38% of the factory slot); the unpadded merged image is 718,688 bytes with
SHA-256
`e20f6191cb2bfa78fbd7f3d588eb418913da3f1f89e3b80a4db0a28abaf414ea`.
Relative to the preceding flashed image, fixed `.bss` grew from 149,264 to
151,120 bytes while the linked stack reservation moved from 176,344 to 174,480
bytes; the GNU aggregate therefore fell by eight bytes. The static authenticated
handoff is 1,224 bytes under its 2,048-byte ceiling, and the node's single-state
request/reply/quarantine owner stays under its 1,024-byte ceiling. These are
linker bounds, not runtime high-water evidence.

That exact 718,688-byte image was flashed to both E290s and read back exactly
from address zero. Both boards returned sequence-zero `initialization-required`;
both 8 KiB credential partitions retained the exact all-`0xff` SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`;
and both recovered sequence-zero service after the credential readback reset.
This is bounded regression evidence for the existing pre-authentication USB
bootstrap only. The authenticated bearer endpoint was dormant in that image,
so no hello, proof, authenticated request, or authenticated reply ran on
hardware. It does not qualify the subsequently composed minimal bearer.

The preceding minimal-USB-bearer source linked at 640,587 bytes text, 3,596
bytes initialized data, 469,232 bytes BSS/reservations, and 1,113,415 bytes total
by GNU size. Its packaged application was 681,648 of 6,291,456 bytes (10.83% of
the factory slot). The unpadded merged image was 747,184 bytes with SHA-256
`5ccfeb7518ea3bfa856cb439b3e75d118ec3ec78254bc5f0ef9b33851740a8bd`.
That exact range matched address-zero readback on both MAC
`ac:a7:04:e1:3e:88` and `ac:a7:04:e1:3f:88`. Both boards then returned
sequence-zero `initialization-required`; exact 8 KiB credential reads remained
entirely `0xff` with SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.
That run is package/readback and pre-authentication bootstrap evidence, not a
powered authenticated handshake, request, or reply.

The current Active-generation-binding source links at 641,419 bytes text, 3,596
bytes initialized data, 469,232 bytes BSS/reservations, and 1,114,247 bytes total
by GNU size. Its packaged application is 682,480 of 6,291,456 bytes (10.85% of
the factory slot). The 748,016-byte merged image and its powered qualification
are recorded under the authenticated USB client below.

## Powered permanent-graph smokes

The first smoke was captured from source `96e38aa`. Both fully erased boards
received the same 729,504-byte merged image with SHA-256
`3b6c07d6c23265b5655901d0b9c62ce1dfafe92251372ef9f51aa11132371e5d`, and
post-boot reads of that complete range matched exactly on both boards.

Both monitored reboots reported 8,388,608 bytes of initialized PSRAM,
`UninitializedErased` credentials with `recovery_steps=0`, `writes=0`, and
`erases=0`, explicit initialization required, automatic provisioning disabled,
and API/session/bearer closed. Redundant identity, strict empty-journal mount,
resident storage, LoRa readiness, and interface 1/MTU 500 were present. Each
board completed two ordinary-family one-frame LoRa transmissions. Exact
post-boot reads of both 8 KiB credential partitions were entirely `0xff` and
shared SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.

Source `5f3f259` then passed a bounded upgrade smoke on 2026-07-18. Both known
eFuse MACs again reported ESP32-S3 revision 0.2, 16 MiB flash, and disabled
secure boot/flash encryption immediately before the write. Both exact
736,144-byte readbacks matched the merged-image SHA-256
`f422a8003762f9579ee0f4faf8c85cf78961327f7bb2c6db8c8878bc071d389b`. Counted
monitored reboots retained redundant identity and the clean journal, reported
8 MiB PSRAM, `credential_pairing_policy_resident=true`, and
`credential_initialization=Eligible { media: ExactlyErased }` while the local
API, session, and bearer remained closed. Both LoRa actors reached `READY` and
transmitted ordinary one-frame work. Post-boot reads of both complete 8 KiB
credential partitions retained the all-`0xff` SHA-256 above.

Those historical runs are boot and ordinary-TX smoke evidence, not controlled
peer reception or DATA delivery. Their statements that the bearer was closed
describe sources `96e38aa` and `5f3f259`; the newer pre-authentication USB
composition was subsequently flashed and exercised as described below. The
historical smokes do not qualify credential initialization, pairing,
authentication, the bootstrap bearer, an authenticated
local API bearer, interruption/power-cut recovery, runtime stack high-water,
heap pressure, soak behavior, or production security. The separate semantic
HIL remains the controlled cross-board ANNOUNCE/DATA/proof result.

## Pre-authentication USB control client

The host utility speaks only the zero-session, zero-tag initialization-control
records. `initialize` keeps one serial port open, queries status, advances the
exact-next sequence internally, asks for initialization, and continues polling
while the device reports physical presence or in-flight work. `status` defaults
to a 15-second overall deadline and `initialize` defaults to 120 seconds:

```sh
# Standalone status query from a fresh boot/USB epoch:
cargo +stable run --locked -p xtask -- e290-pairing-control \
  --port /dev/cu.usbmodemXXXX status
```

Or run the complete initialization workflow directly from a fresh epoch:

```sh
cargo +stable run --locked -p xtask -- e290-pairing-control \
  --port /dev/cu.usbmodemXXXX initialize
```

If the standalone status command printed `next_sequence=1` and no USB bus reset
occurred, continue that same epoch explicitly instead of reusing zero:

```sh
cargo +stable run --locked -p xtask -- e290-pairing-control \
  --port /dev/cu.usbmodemXXXX --sequence 1 initialize
```

For erased eligible media, leave GPIO21 released after the connection is
established, then hold it continuously for at least two seconds when prompted.
The 60-second policy window begins at the hold threshold. A standalone `status`
prints `next_sequence`; another command in the same USB epoch must use that
value. Closing and reopening the TTY does not start a new epoch, and DTR does not
delimit one; only a USB bus reset followed by a new SOF permits sequence
zero again. `initialize` avoids the ordinary operator hazard by retaining the
port for the complete workflow. The client asserts DTR because macOS otherwise
accepts writes without delivering them to this ESP32-S3 native USB serial
endpoint, and it keeps RTS deasserted. Port names are ephemeral.

Any post-send I/O failure or request timeout makes the last sent sequence
consumed-or-ambiguous: the
device may have accepted it before the reply was lost, so neither blind reuse
nor blind increment is safe. Confirm a USB bus reset before restarting
at zero. The firmware refuses `u64::MAX` and exhausts that epoch; the host rejects
an explicit maximum and reports no usable successor after `u64::MAX - 1`.

## Live-pairing USB client

The companion client keeps that same port and exact sequence space open across
Begin, ProofStart, and Activate. A `physical-presence-required` Begin is the
only result it retries: it advances the sequence, prints the GPIO21 instruction,
and waits within the overall deadline. Every other coarse Begin failure is
terminal. Before Begin, the client creates without overwrite, synchronizes, and
read-verifies an owner-only 96-byte Reserved marker. After a durable offer it
writes and verifies a complete Pending record in an owner-only same-directory
staging file, atomically renames that file over the reservation, synchronizes
the parent, and only then sends ProofStart and Activate. It validates the exact
device/credential continuation and full activation-confirmation MAC before
changing the state byte to Active. Serial and state scratch are zeroized;
device and credential IDs may be printed, but the PSK and proofs never are.
Secure `pair` and `resume` persistence is currently Unix-only.

```sh
cargo +stable run --locked -p xtask -- e290-pairing-live \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  pair
```

A Reserved state file is secret-free and contains only the canonical marker. A
Pending or Active file additionally contains device ID, credential ID,
generation, and the 32-byte PSK. It is plaintext developer/HIL key material and
must be protected like a password. Pair requires a starting sequence no greater
than `u64::MAX - 3`; resume requires no greater than `u64::MAX - 2`.

A known Pending file before Activate can retry ProofStart with a fresh nonce in
a newly confirmed physical-presence window:

```sh
cargo +stable run --locked -p xtask -- e290-pairing-live \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  resume
```

A lost Begin offer leaves only a Reserved marker while the device may own an
unrecoverable Pending PSK. Assess that state, then use the physically confirmed
identifier-free recovery command:

```sh
cargo +stable run --locked -p xtask -- e290-pairing-live \
  --port /dev/cu.usbmodemXXXX \
  abort-current
```

`abort-current` also retries only physical-presence-required and reports the
exact next sequence. It never reads or deletes a host state file. A successful
abort is a durable tombstone; removing any corresponding Pending host file is a
separate operator action.

After an ambiguous Activate, retain the complete file. The current utility
cannot distinguish Pending from Active because it does not yet implement
authenticated activation-state reconciliation. Do not guess Active, blindly
resume, or invoke `abort-current`; `resume` is a proof retry, not an
activation-state oracle.

## Minimal authenticated USB client

After `pair` has durably activated the credential and marked the owner-only
state file Active, force a real USB bus reset/re-enumeration before opening an
ordinary session. Pairing exclusivity intentionally keeps the old connection
closed to ordinary sessions, and merely closing or reopening the TTY is not a
reset boundary. The bounded host client performs one hello/proof handshake and
supports these authenticated logical operations:

- `system-capabilities` (the default command);
- `identity-summary`;
- `submission-status` with `--submission-id`;
- `submit-rns-data` with destination, payload, and idempotency key; and
- `submit-and-wait`, which submits once and polls every 500 ms over the same
  authenticated session until `Delivered` or a terminal failure.

Read the public primary destination:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  identity-summary
```

Submit DATA and wait up to the default 45-second overall deadline:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  --destination-hash <32-lowercase-hex> \
  --payload-hex <0-to-766-hex> \
  --idempotency-key <32-hex> \
  submit-and-wait
```

The command accepts only the canonical Active state-file format, verifies the
device ID and credential generation during the handshake, preserves coalesced
records, accepts any response minor within API major 1, uses one absolute
deadline, and prints only non-secret scalar results. `submit-and-wait` uses
strictly increasing request IDs and treats only status `Internal` as a transient
poll result; `Failed`, `Cancelled`, any other API error, or deadline expiry is a
failure. It has no resumption, close, encryption, rate-policy, repeated-
handshake, or concurrent-request behavior. Each one-shot command drops its
restored client session after one response while firmware remains established,
so reset/re-enumerate USB before a later invocation. `submit-and-wait` is the
exception: it deliberately retains that one session for sequential status
requests. Do not infer whether a credential identifier exists from failure
text.

The current one-entry accepted-history cap is a qualification profile, and the
successful sender now contains its one committed record. A later novel
submission requires a product-capacity policy change or deliberate journal
reprovisioning; it is not evidence that the intended product capacity is one.

Earlier on 2026-07-18, the then-current 682,480-byte application was packaged as a
748,016-byte merged flash image (SHA-256
`4864180ab1d51081758ec3bec53068d6c75316209a2ccc269a0aad48c210fe2c`) and
installed on board MAC `ac:a7:04:e1:3e:88`. Exact address-zero readback matched
the image. The credential partition first read entirely `0xff` with SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.
One physical-presence hold completed initialization at request sequence 61;
pairing completed in that USB epoch at sequence 64 and atomically replaced the
owner-only host state file with Active generation 3. After a hard reset, an
exact credential-partition readback had SHA-256
`ce7c4937b0e72c3a8a332a040267b0c408a8946ea75f22041688cd7f5bd99170`.
The partition contains plaintext secret material, so only its digest is
recorded here. A fresh USB epoch then completed an authenticated
`system.capabilities` request: API 1.0, packet output disabled, direct radio TX
disabled, experimental RNS DATA submission enabled, 512-byte messages,
448-byte bodies, and 383-byte submission payloads. This qualifies durable
initialization, pairing, activation, reboot recovery, and the minimal
authenticated USB request/response path on hardware. It does not qualify
session resumption, encryption, rate limiting, BLE, or Wi-Fi.

Later on 2026-07-18, API 1.1 added `identity.summary` and the multi-request
`submit-and-wait` host path. The release application is 686,176 bytes; GNU size
reports 645,159 bytes text, 3,596 bytes initialized data, 469,232 bytes BSS/
reservations, and 1,117,987 bytes total. Its 751,712-byte merged image has
SHA-256
`4285fcaa9df6a6f0314ed4735377ea986b0efcafafc2710ad7594489a49b4795`.
The same image was installed on both boards and exact address-zero readbacks
matched. The sender's primary destination is
`c99e8ff1ec8629e4e1290e14462ae8af`; the separately provisioned receiver's is
`83a09ed807a0a7c631386deaa0448fb9`. The sender durably accepted submission 1,
prepared a 131-byte packet with full encoded-byte SHA-256
`df937860f5225deb9d2350c6f3a46f33bd659ccbcb6b47267add47c9a287a4fe`,
and reached `Delivered` in about 2.6 seconds after the receiver decrypted the
matching DATA and returned a valid Reticulum proof. Full sender USB re-
enumeration followed by a fresh authenticated `submission-status` request
returned the identical terminal state, length, and digest. This is end-to-end
proof through the permanent owner graph and physical LoRa link. `Delivered`
does not yet prove that an onboard LXMF/NomadNet mailbox or external client
consumed the plaintext; inbound application events are currently drained.

The first powered end-to-end attempt usefully failed closed during post-reboot
authentication with an expected generation 2 versus observed generation 3
mismatch: the durable store assigned generation 3 to the committed Active
record after the Pending record at generation 2, while the host retained the
Pending generation. Pairing proof suite 2 fixes that boundary by authenticating
the actual committed Active generation in the activation confirmation and
atomically persisting that exact generation on the host. The successful
rebooted proof above exercises that fix; it does not assume that Active is
Pending plus one.

On 2026-07-18, the historical control-only 652,992-byte merged image (SHA-256
`1727a14b58a076d65ea12feb61b564d5dfc66d6c6f0b9a8ddd39fc773332705c`) was
flashed with the explicit 16 MiB E290 partition table to both boards. Both MAC
`ac:a7:04:e1:3e:88` and `ac:a7:04:e1:3f:88` returned sequence-zero
`status=initialization-required`, code 1, and both returned
`physical-presence-required` when the single-open initialization workflow ran.
No-button five-second workflows on both boards advanced cleanly through
requests 0--47 before their overall deadlines, providing powered multi-request
liveness evidence. They did not open the physical-presence window or write
credentials: subsequent 8 KiB reads of both credential partitions were entirely
`0xff` with SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.
That historical run did not qualify a successful hold, credential write, or
exact post-write readback. It is powered status and
`physical-presence-required` evidence, not successful initialization evidence.
An initial flash using espflash's default partition table correctly failed
closed before provisioning; all subsequent powered claims use the repository's
explicit `partitions/heltec-vision-master-e290-node.csv` image.

An intermediate routed live-pairing/reset-guard image was then installed from
fully erased media on both boards. Both boards returned
`initialization-required` at sequence zero, returned physical-presence-required
through sequence 24 during a 2.5-second no-button initialization workflow,
dropped a deliberately stale sequence-zero request, and returned to a fresh
sequence-zero epoch after a full macOS USB re-enumeration. Live Begin shares that
same gate and sequence space: before initialization, both no-button clients
received only physical-presence-required through sequence 24 and created no
host PSK file. This intentionally does not reveal deeper credential state before
physical presence.

The preceding boot-quarantined image was then installed on both boards. Its
exact 701,744-byte address-zero readback from each board matched SHA-256
`14d9fd6dd482c47baa9afd2fda6a5ba1d69f46785bf23ae29f6b9fe561e4b212`.
After the hard reset induced by each readback, both boards reappeared and again
served sequence-zero `initialization-required`. Simultaneous 120-second
no-button initialization workflows remained responsive through sequence 1102
on MAC `ac:a7:04:e1:3e:88` and sequence 1100 on MAC
`ac:a7:04:e1:3f:88`; neither observed physical presence. Exact post-workflow
reads of both 8 KiB credential partitions were entirely `0xff` and shared
SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.
This demonstrates hard-reset service recovery through the application-entry
detach/scrub/reattach path, long no-button request liveness, and zero credential
mutation. Because no secret response was in flight, it does not independently
prove USB FIFO/RAM secret erasure or non-replay. That preceding image did not
qualify a successful GPIO21 hold, initialization, pairing, activation, or exact
post-write readback; the later current-image proof above closes only that happy
path. ROM and bootloader execution before the earliest Rust entrypoint also
remains outside the application boot-quarantine claim.

The current USB policy treats an 8 ms missed-SOF interval as suspension while
retaining the epoch and exact-next sequence, then resumes that same epoch on a
later SOF. Only bus reset disconnects. Full `USBDeviceReEnumerate` passes and
replaces the macOS service/session. Non-seizing in-place `ResetDevice` returns
success but leaves the same endpoint stale and is not a recovery mechanism.
That preceding image has bounded whole-chip hard-reset recovery evidence after
flash readback, while suspend/resume remains a separate powered qualification
case. A hard reset does not by itself qualify the ROM/bootloader interval before
the earliest Rust entrypoint.

## Connected-board identity and future flash procedure

The read-only 2026-07-17 discovery snapshot was:

| Ephemeral port | eFuse MAC | Chip | Flash | Security |
| --- | --- | --- | --- | --- |
| `/dev/cu.usbmodem101` | `ac:a7:04:e1:3e:88` | ESP32-S3 rev 0.2 | 16 MiB | secure boot and flash encryption disabled |
| `/dev/cu.usbmodem1101` | `ac:a7:04:e1:3f:88` | ESP32-S3 rev 0.2 | 16 MiB | secure boot and flash encryption disabled |

Ports are not identities and can change after reset or reconnection. Before a
future write:

1. Record the already-established `HT-RA62-HF` module identity for each board
   and keep a 915 MHz antenna attached.
2. Re-run `espflash board-info --chip esp32s3` immediately before each write and
   require the intended eFuse MAC, 16 MiB flash, disabled secure boot and
   disabled flash encryption.
3. Before creating any dump or evidence file, set `umask 077` and choose a
   directory on restricted, encrypted storage. A full dump from a provisioned
   node contains the plaintext Reticulum private key. File permissions are not
   encryption: do not place the dump in an unencrypted sync folder, attach it
   to an issue, or include it in ordinary build artifacts. Preserve a fresh
   16 MiB full-flash backup plus the exact ELF, partition table, `Cargo.lock`,
   tool versions and hashes:

   ```sh
   umask 077
   BACKUP_DIR="e290-private-backup-$(date -u +%Y%m%dT%H%M%SZ)"
   mkdir -m 700 "$BACKUP_DIR"
   # BACKUP_DIR must reside on encrypted storage.
   espflash read-flash --skip-update-check \
     --port "$PORT" --chip esp32s3 \
     --before default-reset --after no-reset --non-interactive \
     0 0x1000000 "$BACKUP_DIR/flash-before.bin"
   test "$(wc -c < "$BACKUP_DIR/flash-before.bin" | tr -d ' ')" = 16777216
   chmod 600 "$BACKUP_DIR/flash-before.bin"
   shasum -a 256 "$BACKUP_DIR/flash-before.bin" \
     > "$BACKUP_DIR/flash-before.sha256"
   ```

   Keep the board in the serial loader after this backup. Any later copy or
   archive of the dump must retain equivalent access control and encryption.
4. Create the explicit 16 MiB merged image rather than invoking the 8 MiB
   workspace runner:

   ```sh
   ELF=target/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node
   espflash save-image --skip-update-check \
     --chip esp32s3 --merge --skip-padding \
     --flash-mode dio --flash-freq 80mhz --flash-size 16mb \
     --xtal-freq 40mhz \
     --partition-table partitions/heltec-vision-master-e290-node.csv \
     --target-app-partition factory "$ELF" e290-node.bin
   IMAGE_BYTES="$(wc -c < e290-node.bin | tr -d ' ')"
   test "$IMAGE_BYTES" -le $((0x610000))
   ```

5. Before the **first product provisioning boot**, after the backup, erase the
   durability range. The unpadded merged image contains the bootloader,
   partition table and application; it does not initialize
   `0x610000..0x730000`. Flashing it over arbitrary old bytes therefore does
   not create blank identity, clock, credential, configuration, or journal
   media, and the firmware will correctly fail closed. Choose one destructive
   preparation:

   - erase the entire chip:

     ```sh
     espflash erase-flash --skip-update-check \
       --port "$PORT" --chip esp32s3 \
       --before default-reset --after no-reset --non-interactive
     ```

   - or preserve all other ranges and erase exactly the contiguous first-boot
     durability/configuration region:

     ```sh
     espflash erase-region --skip-update-check \
       --port "$PORT" --chip esp32s3 \
       --before default-reset --after no-reset --non-interactive \
       0x610000 0x120000
     ```

   In either case, verify the entire exclusive range before writing the image:

   ```sh
   espflash read-flash --skip-update-check \
     --port "$PORT" --chip esp32s3 \
     --before default-reset --after no-reset --non-interactive \
     0x610000 0x120000 "$BACKUP_DIR/durability-erased.bin"
   test "$(wc -c < "$BACKUP_DIR/durability-erased.bin" | tr -d ' ')" = 1179648
   test "$(LC_ALL=C tr -d '\377' < "$BACKUP_DIR/durability-erased.bin" \
     | wc -c | tr -d ' ')" = 0
   ```

   Do not allow an intermediate normal boot between erase verification and the
   merged-image write.
6. Write and read back the exact merged image while leaving the board in the
   loader:

   ```sh
   espflash write-bin --skip-update-check \
     --port "$PORT" --chip esp32s3 \
     --before default-reset --after no-reset --non-interactive \
     0 e290-node.bin
   espflash read-flash --skip-update-check \
     --port "$PORT" --chip esp32s3 \
     --before default-reset --after no-reset --non-interactive \
     0 "$IMAGE_BYTES" "$BACKUP_DIR/e290-node-readback.bin"
   cmp e290-node.bin "$BACKUP_DIR/e290-node-readback.bin"
   ```

7. On every **subsequent upgrade**, preserve a new secret full-flash backup but
   do not erase `node_identity`, `announce_clock`, `api_credentials`,
   `node_journal`, or any newer product store. The unpadded merged-image write
   must stop at or below
   `0x610000`. For an upgrade-layout check, read the complete application-data
   region `0x610000..0x930000` before the write, leave the board in the loader,
   read it again immediately afterward and require exact equality before the
   first upgraded boot. A future partition-map, identity, journal, or message
   format change requires an explicit migration procedure; it is not a normal
   upgrade.

### Explicit schema-1 development-journal migration

Semantic schema 1 did not persist authorization provenance and cannot be
truthfully upgraded. An ordinary schema-2 image therefore reports
`UnsupportedSemanticVersion(1)`, performs no journal mutation, closes local
submission service, and continues route-only LoRa. Development boards may use
this explicit journal-only procedure; it preserves `node_identity`,
`announce_clock`, `api_credentials`, `device_config`, and every unrelated flash
range.

1. Take and protect the full-flash backup described above, then leave the board
   in the serial loader.
2. Build and package a one-shot image with the non-default migration feature:

   ```sh
   cargo +esp build --locked --release \
     -p reticulum-heltec-vision-master-e290-node \
     --features journal-schema2-dev-reprovision \
     --target xtensa-esp32s3-none-elf
   ```

   Package it with the same explicit 16 MiB `espflash save-image` arguments
   above. Do not distribute or retain this exceptional build as the normal
   product image.
3. Erase exactly the 1 MiB journal partition and verify every byte is erased:

   ```sh
   espflash erase-region --skip-update-check \
     --port "$PORT" --chip esp32s3 \
     --before default-reset --after no-reset --non-interactive \
     0x630000 0x100000
   espflash read-flash --skip-update-check \
     --port "$PORT" --chip esp32s3 \
     --before default-reset --after no-reset --non-interactive \
     0x630000 0x100000 "$BACKUP_DIR/node-journal-erased.bin"
   test "$(wc -c < "$BACKUP_DIR/node-journal-erased.bin" | tr -d ' ')" = 1048576
   test "$(LC_ALL=C tr -d '\377' < "$BACKUP_DIR/node-journal-erased.bin" \
     | wc -c | tr -d ' ')" = 0
   ```

4. Flash the one-shot feature image and boot it once. The permanent image now
   uses no-op logging to reserve USB Serial/JTAG for framed control, so the old
   serial-log proof (`journal-reprovision-policy`, `node-journal-provision`, and
   schema-2 mount lines) is no longer available. Do not count this migration as
   verified without an independent exact raw-journal readback/parser or a
   separately reviewed diagnostic build/sink. The firmware still scans the
   complete partition before provisioning and rejects any schema-1, corrupt,
   torn, or otherwise programmed byte without a write or erase. If this one-
   shot boot is interrupted during provision, erase and verify the same journal
   range again; it does not repair programmed migration media.
5. Reflash the ordinary image without the feature, preserving the complete
   application-data range. Prove with the same independent readback/parser or
   separately reviewed diagnostic sink that it strictly mounts the existing
   schema-2 journal with migration disabled and zero provisioning mutation;
   the ordinary no-op-logging image emits no native USB log for this proof.

No firmware path erases the journal automatically, and the feature never
authorizes writes outside `node_journal`.

The first permanent-image write and powered smoke above verified boot, radio/
interface readiness, and autonomous ordinary TX on both boards. It did not
control or verify peer RX, DATA, contention, reset recovery, or interoperability;
the separate semantic HIL supplies the bounded controlled DATA/proof result.
Autonomous images with
`app_data=None` do not originate a controlled fragmented or transit packet.
Fragment reassembly, forwarding, DATA and proof testing therefore need an
external Reticulum peer/test injector, the semantic-HIL fixture, or the next
local submission/device-API slice. The separate semantic-HIL image has passed
as the bounded qualification fixture for the deterministic DATA/proof exchange.

## Product blockers after this slice

- Preserve ADR 0009's boot-mounted credential store, permanent-E290-only
  feature-free pairing-policy edge, and resident `CredentialRuntime`.
  Preserve the implemented lifecycle-specific credential planners, opaque
  typed store commit/reconcile path, mounted-store pending selection, and
  interrupted-initialization classifier and explicit read-only E290 boot
  state. Preserve the private exact permit/binding/mounted-authority ownership,
  forward-only initialization drive, cross-store mutation gate, and sole-owner
  port. Preserve the now-composed featureless pre-authentication codec,
  debounced GPIO21 physical presence, single USB byte owner, boot-lifetime
  connection epochs, exact-next sequence checks, and depth-one command/reply
  handoff. Preserve the now-qualified one-board button-confirmed initialization,
  durable activation, exact Active readback, reboot, and authenticated
  capabilities result; next qualify exact Pending/Abort readbacks,
  suspend/resume, controlled power cuts, mutation ambiguity, and the
  pre-application boot-chain residual. Preserve the bounded preceding-image
  hard-reset reattachment evidence already captured on both boards.
  Preserve the now-connected resident Begin/Proof/Activate/Abort owner,
  generalized cross-store exclusion, shared USB decoder/sequence owner,
  causal node frontier, secret handoff, and boot quarantine. Preserve the
  now-composed feature-free session/handoff dependencies, static depth-one API
  handoff, current-authority node dispatch, disjoint submission-port view, and
  minimal single-flight USB session bearer. Preserve the now-qualified API 1.1
  identity, authenticated RNS DATA, durable runtime, real LoRa peer proof,
  sequential status, and fresh post-re-enumeration session path.
  Resumption, retries, close records, encryption, rate/attempt policy, repeated
  attempts, and concurrency remain later hardening work; the transport-neutral
  admission boundary must remain reusable by BLE and Wi-Fi, whose session
  bindings/suites still require explicit implementation and qualification. The narrow
  pre-authentication bearer, one-entry composition cap, and ADR 0005 host
  behavior already pass. A later product-capacity policy must not weaken the same
  durability contract, and future interface actors fail-stop only their
  affected actor.
- Extend the resident sole-flash coordinator to host device configuration and
  message storage with explicit power-loss, wear, migration, and cross-store
  ordering behavior.
- Define and qualify the production key backup/recovery and at-rest protection
  policy. The current developer image deliberately requires flash encryption
  disabled and stores its mirrored private identity in plaintext.
- Deliver non-packet node output to a durable/client owner. This milestone logs
  and drains it so transport progress cannot deadlock.
- Add LXMF propagation/storage and local LXMF/NomadNet client services.
- Preserve the composed independently vector-tested ADR 0006 authentication
  model, ADR 0009 pairing, and first USB bearer. Add Wi-Fi as a Reticulum transport only when that
  separate link behavior is specified; packet transports remain deferred
  behind the primary LoRa slice.
- Replace the single-LoRa airtime policy with a composite per-resource policy
  when a second packet interface is introduced; add durable regional airtime
  accounting where required.
- Add task restart, radio reinitialization, registry offline transitions and a
  whole-node fault supervisor. A future in-process node-task restart must retain
  the live per-boot announce ordinal or reserve a new durable epoch before it
  emits; recreating ordinal zero under the same epoch is forbidden.
- Define the end-of-life policy for the 20-bit boot-epoch namespace. The current
  image fails inert at `EpochExhausted`; production may instead require an
  explicit identity rotation/reprovisioning workflow, but must never wrap.
- Replace the 1 ms node poll with a combined readiness/deadline wait.
- Run controlled two-board interoperability through the permanent image. Its
  current powered evidence establishes boot, ordinary-TX smoke, and bounded
  USB initialization-required/physical-presence-required behavior only; the
  passed separate semantic HIL establishes the controlled E290
  radio/RNode/Rete functional baseline.
- Keep display and GNSS/location integration stubbed until the network,
  persistence and client ownership paths are stable.
