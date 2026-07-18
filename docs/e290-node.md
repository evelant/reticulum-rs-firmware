# Permanent Vision Master E290 node image

**Status:** the first permanent, LoRa-first image is implemented and passes its
57-test host composition suite, portable-target, ESP32-S3 build,
review, and merged-image packaging gates. Source `5f3f259` passed a bounded
powered upgrade smoke on both `HT-RA62-HF` boards: exact same-image readback,
resident pairing-policy and erased-initialization eligibility, zero credential
mutation, journal/LoRa/interface startup, and ordinary one-frame TX. Full
powered product-graph and initialization qualification remain open.

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
`CredentialRuntime` with the exact boot binding, any mounted authority, and any
admitted initialization permit. The coordinator also compiles the sole-owner
physical initialization port, but no bearer, GPIO debounce, external request
lane, or powered run invokes it. The codec is not yet a firmware lane. Live
external admission is blocked by that invocation path, live
Begin/Proof/Activate/Abort composition, the external API/session lane, and a
bearer. ADR 0005's active-owner policy is implemented: a
permanent fault
with an unresolved frame enters interface-local `ActiveOwnerFailStopped`, takes
the same LoRa lease offline without changing its generation, retains the exact
frame/completion/ticket, and permits no fresh LoRa work for the rest of the boot.
Device configuration, message storage, client delivery, LXMF/NomadNet, and
host-facing USB/BLE/Wi-Fi services remain visible product blockers.

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
    SubmissionRuntime + operation-scoped BoundJournal views
    exact authorized-frame retain/re-offer + durable echo
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
later RF operation. The storage lane is normally idle because the product has
no external admission lane. The task performs
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
| Node journal | `0x630000` | 1 MiB | Resident operation-scoped submission runtime; one-entry qualification cap, no external admission lane |
| Message store | `0x730000` | 2 MiB | Reserved, not wired |
| Unallocated | `0x930000` | 6.8125 MiB | OTA/layout decision |

The workspace runner in `.cargo/config.toml` hardcodes an 8 MiB flash size and
must not be used for this target.

`node_identity`, `announce_clock`, and `api_credentials` use ESP-IDF's standard
`data,undefined` subtype. All three have application-owned formats; the
credential range is checked, boot-mounted/recovered, and retained while ADR
0009 live provisioning/pairing serving remains absent. `device_config`
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
LoRa startup continue unchanged. No state starts a session, bearer, external
API, pairing flow, or live authentication in this image.

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
The accepted-history cap is one for qualification, and no external admission
edge is composed. The
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
job handoff exist, but the image has no composed session, external API lane, or
USB/BLE/Wi-Fi bearer.

It also implements a compiled sole-owner credential-initialization port. Each
request and drive freshly inspects `node_identity`; a physical drive then lends
one short-lived credential-partition view bound to the exact boot device/range/
layout before calling the resident runtime. No firmware task, USB/BLE/Wi-Fi
bearer, connection-epoch source, or debounced GPIO21 sampler invokes these
methods yet, so this is software composition rather than powered initialization
evidence. Live Begin, Proof, Activate, and Abort mutations remain uncomposed.

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
The host library suite has 57 passing tests: 55 focused policy/product/
credential-boot/credential-runtime tests, including the source-order regression,
every canonical empty-initialization byte cut, adversarial media changes between
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
and no later host-radio TX or RX. The 55 focused tests include the exact
one-submission profile assertion and five focused durability-policy tests for
retry, route-only degradation, pending durable acknowledgement, sticky fail-stop,
and the request-after-disable race. Eleven credential-runtime tests additionally
cover both initialization trajectories, fresh binding and identity checks,
forward-only media movement, ambiguous backend/readback retention, disconnect
ownership, policy completion, and fail-closed noncanonical states. Four
cross-store tests cover both retained journal owners, initialization before and
after physical I/O, stable credential states, and the distinct deferred versus
unavailable result.

Separate ESP release builds with `-Z emit-stack-sizes` produced 1,025 fully
symbolized records and identical complete frame-size multisets for the default
and journal-migration-permitted variants. The largest frames are
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

The current credential-runtime-composed release baseline at source `5f3f259` is
659,035 bytes text, 11,464 bytes initialized data, and 461,364 bytes BSS/
reservations by GNU size; the packaged application is 670,608 of 6,291,456 bytes
(10.66% of the factory slot). The unpadded merged image is 736,144 bytes with
SHA-256
`f422a8003762f9579ee0f4faf8c85cf78961327f7bb2c6db8c8878bc071d389b`. CI
retains explicit growth headroom rather than treating this early image as the
full appliance ceiling.

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

This is boot and ordinary-TX smoke evidence, not controlled peer reception or
DATA delivery. It does not qualify credential initialization, pairing,
authentication, a local API bearer, interruption/power-cut recovery, runtime
stack high-water, heap pressure, soak behavior, or production security. The
separate semantic HIL remains the controlled cross-board ANNOUNCE/DATA/proof
result.

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

4. Flash the one-shot feature image and boot it once. Require the serial log to
   show `journal-reprovision-policy` with `explicit=true`,
   `erased_media_only=true`, and `automatic_erase=false`, followed by a passing
   `node-journal-provision` and schema-2 mount. The firmware scans the complete
   partition before provisioning and rejects any schema-1, corrupt, torn, or
   otherwise programmed byte without a write or erase. If this one-shot boot is
   interrupted during provision, erase and verify the same journal range again;
   it does not repair programmed migration media.
5. Reflash the ordinary image without the feature, preserving the complete
   application-data range. Its next boot must log the migration policy as
   disabled and strictly mount the existing schema-2 journal with zero
   provisioning mutation.

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
  forward-only initialization drive, cross-store mutation gate, and compiled
  sole-owner port. Preserve the featureless framing-only pre-authentication
  codec, then connect it to debounced physical presence, the USB byte owner, and
  a bounded command/reply handoff. After explicit initialization works, compose
  live Begin/Proof/Activate/Abort mutation ownership with the immutable
  authority and bounded COBS framing,
  qualification-session core, and boot-lifetime job/reply handoff with the
  first USB bearer. Persistent-state composition, firmware composition, and the
  physical bearer are the remaining edges for live external admission; the
  one-entry composition cap and ADR 0005 host behavior
  already pass. A later product-capacity policy must not weaken the same
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
- Compose the independently vector-tested ADR 0006 authentication model with
  ADR 0009 pairing and the first USB bearer. Add Wi-Fi as a Reticulum transport only when that
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
  current powered success establishes boot and ordinary-TX smoke only; the
  passed separate semantic HIL establishes the controlled E290
  radio/RNode/Rete functional baseline.
- Keep display and GNSS/location integration stubbed until the network,
  persistence and client ownership paths are stable.
