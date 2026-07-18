# Reticulum Rust firmware

Bare-metal Rust firmware for a standalone Reticulum node. LoRa is the first and
primary complete transport vertical slice, using the ESP32-S3R8 and
HT-RA62/SX1262 combination on the Heltec Vision Master E290-HF. The node, Rete
core, registry, and router remain interface-neutral so a later second radio,
USB, Wi-Fi, or BLE link can join as an independent Reticulum interface without
pretending to be LoRa. None of those later Reticulum packet actors is being
implemented in parallel with the first LoRa path. USB's first product role is
the authenticated local client/control API; a Wi-Fi SPA and BLE client can
follow on that same separate API boundary. Those bearers become Reticulum
packet interfaces only through optional actors added after the LoRa slice. The
already-qualified Heltec Wireless Tracker V2.3 pair remains a constrained LoRa
regression target.

The hardware-independent
`reticulum-board-heltec-vision-master-e290` crate is the compiled source of
truth for the supplied schematic's internal GPIO ownership, the fitted
HT-RA62-HF 863--928 MHz range, and reset-low/NSS-high inert boot state. It
deliberately exposes no flash-capacity claim, qualified PSRAM capacity, default
frequency, or transmit-power selection; its 8 MiB PSRAM value is named only as
the ESP32-S3R8 design floor. Powered qualification has now established 16 MiB
flash and 8 MiB mapped octal PSRAM on each of the two supplied boards.

The independent `reticulum-board-heltec-vision-master-e290-radio` owner now
wraps a board-neutral `reticulum-radio-lora-phy` state machine. Its opaque
NA915 development profile uses the stock high-power SX1262 path at requested
14 dBm, with the current Semtech optimal PA/raw-command mapping, DC-DC,
internal DIO2 switching, DIO3 1.8 V TCXO control and reset-only
fail-closed containment. Host command-log tests and generic/Xtensa checks pass;
the isolated same-image E290 HIL has now passed the physical two-board
CAD/RX/TX/RNode/Rete path. This does not qualify the different permanent node
image.

The powered E290 semantic run flashed and read back one 421,296-byte image
(SHA-256 `4584abdff80ab4b3151bf5168a364dc30016e29230f51f06195661b455a01085`)
on both `HT-RA62-HF` boards. Signed ANNOUNCEs, encrypted DATA, and its delivery
proof crossed the NA915 link; receipt
`fc143c17784f784a8c68ff33e7d1bcf897f6bd2bfd4d1cc8a7ce68335baf0aa4`
ended `Delivered`, both roles completed exactly two TX operations, and both
radios shut down. The closed-capture verifier result is documented in
[the E290 semantic HIL record](docs/e290-semantic-hil.md).

LoRa-specific RNode framing, SX1262 ownership, CAD, regional configuration,
airtime reservation, and radio deadlines stay inside one LoRa interface actor.
Portable packet routing uses stable interface IDs and bounded per-interface
ingress/egress; it must not make Wi-Fi, BLE, or USB pretend to be radios or
force ordinary path-selected DATA onto every enabled link.

`reticulum-interface-router` now implements that portable seam: one fixed
authoritative registry derives node-core's online `InterfaceSet`, hands exact
DATA/ordinary owners only to the selected actor queue, validates
generation-bound actor-derived ingress provenance, and returns stale or
pressured owners for explicit reconciliation. Cancellation-safe capacity and
completion waits let a permanent task sleep without reserving an owner.
`reticulum-tx-supervisor::NodeInterfaceSupervisor` now owns the sole node,
router, DATA and ordinary coordinators, and paired permit services. It admits
complete Rete action envelopes into static buffers, routes ticket-bound jobs
fairly, preserves exact fan-out/completion ownership, and distinguishes
retryable pressure from terminal quarantine and fail-closed drain. The first
permanent E290 target composes that portable aggregate with one ticket-aware
LoRa dispatcher and the E290 radio owner; it is software-composition-qualified,
build-verified, and now powered-smoke-qualified on both development boards for
boot, erased credential handling, and ordinary one-frame TX.
Bitrate and cost are recorded but do not replace Reticulum routing. Until Rete
paths carry an interface generation, an ID/configuration stays immutable for
one node-owner lifetime or its learned paths must be purged before reuse.

The historical Tracker **Phase 1 receive vertical slice and bounded semantic
transmit integration** remains the regression/HIL lane described below. Its
default firmware binary is deliberately RF-disabled. A separate, explicitly
configured lab binary owns the Tracker SX1262 through an
opaque RX-only wrapper; it has no transmit API and hands complete PHY frames
from a sole radio task to a
separate ingress owner through a depth-2, non-blocking drop-new queue. Timed
RNode reassembly, endpoint-only Rete admission, periodic protocol maintenance
and unconditional action suppression are target-linked behind that owner. A
clean, matching normal/pressure and eight-artifact closure pair is preserved at
`artifacts/hil/phase1-rx/20260716T000006Z-fdd6d9e-*-bundle`. A later clean
`bf23cc5` normal image was flashed to board E9:44, read back byte-for-byte, and
ran a 125-second supplemental smoke recorded in
`artifacts/board-flashes/2026-07-16-e944-bf23cc5-rx-refresh/RESULTS.md`, but it
has no matching `bf23cc5` closure bundle and is not formal powered
qualification. Powered heap/stack,
electrical, RX/RF, fault, retention, and soak evidence therefore remain open.
Lab-only startup stack watermarking and retained reset-storm quarantine are now
linked. A separately named,
compile-gated lab artifact provides a one-shot deterministic depth-2 queue-
pressure stimulus without changing the normal lab image. Deterministic RNode
1.86 peer/malformed/backpressure/returned-fault stimuli are checked in as a
19-scenario corpus and replayed through the Rust ingress in CI; a separate
host-tested generator creates encrypted local DATA for one exact ephemeral
Tracker boot.

An independently named, eFuse-MAC-gated TX HIL proves exact LoRa PHY and RNode
framing on the two Tracker V2.3 boards. Rust-to-Rust, RNode-to-Rust and
Rust-to-RNode sentinel exchanges all pass. The newer
`semantic-roundtrip-hil` mode has now run one identical Rust/Rete image on both
boards: E9 and E0 exchanged signed ANNOUNCEs, learned each other's direct path,
then carried encrypted DATA and its delivery proof across the real radio path.
The exact earlier readback-qualified DATA receipt
`4ca4ed5d856f45e1abb351762a3ccb8671c9c675a6bbfa082d73010746587a4d`
ended `Delivered`, the receipt table ended empty, both roles reported exactly
two TX completions and both shut their radios down. The strict cross-log pass is
preserved at
`artifacts/hil/tx-hil/20260716T230849Z-rust-rete-semantic-roundtrip/attempt-02-post-readback`.

The qualified Tracker board policy has since moved out of the HIL-only BSP into
the product-named `reticulum-board-heltec-tracker-v2-radio` crate, while common
bounded RX, CAD and atomic one/two-frame TX mechanics now live in
`reticulum-radio-lora-phy`. The
frozen receive-only crate remains incapable of TX/CAD under every feature set;
the historical TX-HIL crate is now a one-edge compatibility facade. The new
owner requires an explicitly selected opaque NA915 configuration, keeps its
calibrated product value feature-invariant, and can additionally expose a
diagnostic near-field value without silently selecting it. It provides
physical-frame RX, low-level CAD, atomic logical-packet TX and fail-closed
shutdown. A 2026-07-16 EDT
(2026-07-17 UTC) powered regression of the extracted owner repeated the same
four-packet exchange on E9/E0 and ended with exact receipt delivery, two TX
completions per board and both radios inactive. See
[the product radio boundary](docs/tracker-v2-radio.md).

The earlier pre-extraction semantic-roundtrip qualification flashed and read
back the same 425,744-byte merged image from both boards, SHA-256
`93ccac552d75a27f2cec571a9f00900210b4b862f157fca57c0cc50c9641fbc5`.
The mode uses the product `reticulum-rns-rete` surface, ADC-backed TRNG and a
64 KiB heap, but fixed public HIL identities. Its short-run heap peaks of 548
bytes on E9 and 764 bytes on E0 are not stack, soak or full-product memory
qualification. That earlier run alone establishes neither durable production
identity/state nor powered operation of the permanent node-core/radio/storage/
API ownership graph; it also does not establish forwarding, multi-hop, LXMF,
or production TX policy.

The earlier deterministic one-way ANNOUNCE-to-RNode/Python result remains
preserved separately at
`artifacts/hil/tx-hil/20260716T183805Z-e944-rete-announce-to-e040-rnode/attempt-02-coordinated`.
That older conformance fixture used a fixed key, zero entropy and old timestamp;
it should not be confused with the later same-image product-surface round trip.
See
[the Phase-1 TX HIL record](docs/phase-1-tx-hil.md).

Separately from the target-linked receive-only image, the portable node-core
now registers caller-owned 500-byte packet buffers, retains fixed dispatch
metadata for them, and prepares outbound DATA directly into one supplied
buffer. `PrepareDataRequest` rejects an owner deadline at or before its current
monotonic sample before any reservation, entropy use, or RNS mutation. Success
resolves the preserved RNS target against an enabled-interface snapshot and
returns a unique routed `TxJob`; multi-interface fan-out is deterministic,
serialized, and reuses that same buffer.

An independent fixed owner now atomically stages every packet from one ordinary
Rete `NodeActions` envelope into caller-owned 500-byte buffers. It preserves
packet order, exact `All`/`Only`/`AllExcept` targets, serialized routes, events,
and unroutable counts without consuming DATA receipt capacity. Admission
failure returns the exact original envelope unchanged. These ordinary jobs
expose no packet bytes directly. A separate ordinary permit lifecycle binds
the same opaque interface resource and actor-defined unit requirements used by
DATA, marks cumulative possible transmission at a covering grant, and exposes
bytes once only through `OrdinaryAuthorizedTx::frame(now)`. Typed
completion then advances serialized fan-out, applies explicit cancellation,
returns the buffer, or retains a same-generation quarantine. This lifecycle
uses the interface router's ticketed job/completion queues; only its scalar
permit request/reply crosses a distinct bounded Embassy handoff. It owns no
RNode framing, executor, or concrete radio state.

The portable typestate now also covers opaque non-`Copy` permit requests and
replies, deadline-aware authorization, one-shot byte access through
`AuthorizedTx::frame(now)`, completion, and retained recovery. Permit issuance
requires an exact opaque interface resource ID and nonzero actor-defined
resource units. Node-core does not interpret those units. The LoRa actor maps
them to its configuration fingerprint and aggregate airtime, while retaining
RNode frame count, CAD, region, and radio policy locally. The policy must return
a matching, sufficient reservation; unknown resources, mismatches, and
under-reservation are denied before authorization. An accepted reservation is
consumed at the conservative linearization point and
irrevocably records that transmission may have started, even if the reply arrives too
late to expose bytes. Exact proofs
or timeouts remain fixed in-place terminal tombstones until explicit
acknowledgement, and a missing owner is never fabricated or force-reused.

For the permanent graph, `reticulum-tx-handoff` provides paired DATA and
ordinary permit request/reply stores without exposing raw channel handles or
an owner-taking async send. Ticketed DATA/ordinary jobs and completions remain
in the interface router. The portable RF-inert `reticulum-tx-dispatch` crate
owns the node-side DATA machines used by the supervisor in a persistent state
machine, provides the node-side permit server, and owns a node DATA machine
that validates boot seeds into a fixed per-slot owner table. The DATA machine
reconciles completions through node-core, parks recovered owners until exact
generation-scoped acknowledgement, and retains/retries serialized `Next` jobs
unchanged under pressure. It also synchronously selects the lowest available
parked owner, prepares DATA into that exact buffer, and either queues the fresh
job or restores/retains its exact owner on rejection or handoff failure. Known
returns and continuations take priority, and queue preflight avoids consuming
entropy or mutating node state under pressure. Synchronous steps retain every
owning value, while short waits store a ready return before completing or wait
for `Next` capacity without moving the job into a future. Its only byte consumer
is an internal scalar inspector: it has no executor, clock, TX-capable
driver/HAL, device-API, or pluggable byte-sink dependency and cannot transmit.
Node-core and its `reticulum-rns-rete` dependency have no radio, RNode, LoRa or
board dependency. The vertical-slice `reticulum-rns-rete-rx` adapter owns the
physical RNode receive/reassembly composition outside that interface-neutral
closure.

`reticulum-tx-supervisor::NodeInterfaceSupervisor` is the permanent portable
aggregate over node-core, the authoritative router, both coordinator families,
paired permit services, and one shared authorization policy. The E290 node task
supplies its monotonic scheduling, bounded fair passes, RNS ticks and announces;
retained faults stop fresh work while forced denials and completions continue
to drain exact owners. The older async `TxSupervisor` and `RfInertTxPolicy`
remain a no-RF legacy test aggregate rather than the product owner.

The permanent image now boot-gates that graph on its checked raw-NOR stores.
`reticulum-device-identity-store` preflights and then loads or provisions
one exact 64-byte Reticulum private identity across two commit-last mirrors.
`reticulum-announce-clock` reserves a mirrored 20-bit boot epoch before identity
mutation, while a 20-bit volatile ordinal gives each accepted local announce a
strictly increasing 40-bit emission value. The platform keeps a single checked
`esp-storage` owner and exposes each store through `reticulum-nor-flash-region`;
an existing identity with missing clock state, unknown bytes, conflicting keys,
or incomplete required redundancy fails closed before node or radio service.
While that independent identity preflight remains vacant, the product can also
resume only the canonical empty first `node_journal` format without erasing;
committed identities skip provisioning and use strict journal mount only.
The same boot guard now requires the exact plaintext `api_credentials`
partition at `0x614000..0x616000`. Immediately after the sole flash owner opens,
the product mounts that portable credential store through an exact binding
derived from the same eFuse-MAC device ID, before identity preflight, journal
provisioning, announce-clock reservation, identity load/provision, or journal
mount. Boot never provisions erased credential media: it performs at most one
reported predecessor retirement followed by at most one inactive-sector
cleanup, classifies the result as `Ready`, authentication-only,
uninitialized-erased, blocked, corrupt, or backend-failed, and transfers any
mounted owner into `ProductStorageCoordinator`. These developer partitions are
deliberately plaintext, so a provisioned full flash dump is secret material.

This first permanent composition is not yet the full product node. The portable
`reticulum-storage-model` defines strict canonical submission records,
principal-scoped idempotency, fail-closed complete replay, lifecycle validation,
and opaque preflighted mutations. `reticulum-submission-projector` now enforces
the durable `Queued -> Preparing` barrier and withholds exact terminal and
recovered-owner acknowledgements until their corresponding transition or audit
is known committed. Neither crate writes flash. The project-owned two-bank
journal implements the physical format, replay, append and compaction.
`reticulum-storage-actor` now joins those pieces as one portable sole owner: it
retains the exact physical journal binding, journal state, live replay index,
sole projector, one bounded pending mutation and a fail-closed fault latch,
while the NOR backend remains outside the actor. Construction borrows one bound
journal view and completes
the full physical mount and semantic replay before exposing service. Acceptance
and the currently delegated projector preparation barrier become visible only
after append commit or exact readback equivalence. `drive_pending()` can
reconcile an ambiguous backend result from actor-owned state without a caller
reproducing the request. Every later physical operation borrows a view whose
device, absolute range, capacity, alignment and layout version must match the
mount binding before I/O. The actual
optional pending cell is compile-time capped at 512 bytes. Its focused host
tests and ESP32-S3 Xtensa checks pass.

`reticulum-submission-runtime` now supplies the transport-neutral durable
ordering loop over that actor and the permanent `NodeInterfaceSupervisor`. It
boot-gates live service through conservative replay recovery, commits the
`Queued -> Preparing` barrier before native packet preparation, drains durable
projection before releasing exact node owners, and has no LoRa, RNode, radio,
board, executor, or local-client-transport dependency. The E290 image now keeps
that runtime in a resident `ProductStorageCoordinator` beside the permanent
node owner. Boot strictly mounts and recovers through an eFuse-MAC-bound journal
view, then every scheduled runtime operation borrows a fresh matching view from
the coordinator's sole flash backend. If the optional journal service cannot
mount or recover at boot, before a durability-gated DATA owner can exist, the
coordinator retains exclusive flash authority while local durable submission
stays disabled and route-only LoRa continues. The current product profile allows
at most one accepted-history entry solely for composition qualification, not as
a product-capacity commitment. No external admission lane exists, so production
cannot originate work through this path even though the host composition
harness now exercises it directly. Node-core emits an
`AuthorizedFrameObservation` from the
exact authorized native DATA bytes before interface framing. The portable radio
dispatcher now retains every post-byte-exposure DATA completion and router
ticket while a bounded transport-neutral request/acknowledgement handoff carries
that exact observation to the node task. The node retains and re-offers it until
the runtime reports `Durable`, then echoes the identical observation; only that
exact echo releases the dispatcher gate. Request pressure and cancelled waits
leave all owners in place, and an unexpected or mismatched acknowledgement
fails closed while retaining the expected and actual observations. The copy-
only `DispatchReport` is diagnostic evidence, not this ownership path.

A permanent storage/runtime failure with an unresolved durability-gated DATA
owner now enters [ADR 0005](docs/adr/0005-active-data-durability-fail-stop.md)'s
interface-local `ActiveOwnerFailStopped` state for the remainder of the boot.
The node retains the observation without an echo,
the dispatcher retains its completion and router ticket, and the same LoRa
lease is marked offline without a generation change. Fresh ingress, tick,
announce, submission and radio work stop; only bounded fail-closed drainage
continues. A permanent fault before an owner exists remains
`DisabledRouteOnly`, and an already-durable echo waiting for capacity is still
sent. LoRa/SX1262 remains the first and primary product interface. Later Wi-Fi,
BLE, USB, or radio packet links join through independent interface actors and
reuse the same routing and durability contracts, so a future healthy actor need
not inherit a LoRa-local fail-stop. No speculative second packet adapter is part
of this slice.

`reticulum-device-api-adapter` now supplies portable authenticated dispatch over
the target-safe `SubmissionPort` semantic boundary. With the
`experimental-rns-data` feature, the indexed-CBOR request and borrowed payload
are converted into one owned acceptance candidate, and an ID is returned only
after the port reports durable acceptance or exact replay. The resident E290
`ProductStorageCoordinator` implements that port with short-lived bound journal
views and stable mappings for replay, conflict, capacity, ambiguity and faults.
The product accepted-history cap is one solely for composition qualification,
not product capacity. Allocation-free COBS framing, the bounded
USB-qualification session server, and a depth-one boot-lifetime authenticated-
job handoff now define the portable edge, including mutual PSK proofs,
directional tags, exact sequences, partial-TX ownership, reconnect epochs and
stale-reply handling. Independent Python vectors freeze the transcript and wire
records. A separate allocation-free immutable credential authority now owns the
shared ID/generation types, validates fixed `Pending`/`Active`/PSK-free
`Revoked` records, selects zeroizing handshake material, and revalidates grants
through a borrowing dispatch lease. Semantic journal schema 2 now persists the
exact credential ID/generation, authority revision, policy version, and granted
permission mask with every accepted submission; the redundant serialized and
in-RAM content digest is derived from the immutable intent, so the unchanged
383-byte request still fits the 512-byte journal body. The canonical authority
image, dedicated credential-partition contract, two-sector commit/retire
format, and initial bounded developer/HIL pairing policy are now selected. The
portable store is boot-mounted/recovered and retained by the firmware
coordinator, but no provisioning/pairing manager, firmware API/session job lane,
or USB/BLE/Wi-Fi bearer invokes the adapter yet. The accepted
authentication, authority, provenance, and USB ownership contracts are
recorded in [ADR 0006](docs/adr/0006-authenticated-local-api-bearer.md) and
[ADR 0007](docs/adr/0007-device-api-credential-authority.md), with the durable
schema transition in
[ADR 0008](docs/adr/0008-durable-authorization-provenance.md) and the physical
store/pairing decision in
[ADR 0009](docs/adr/0009-device-api-credential-store-and-pairing.md). Default and
experimental host tests/clippy plus the corresponding ESP32-S3 Xtensa checks
pass.

The E290 library now has 37 passing host tests: 35 focused
policy/product/credential-boot tests, including a mechanical source-order
regression, plus two real cross-layer composition tests. The happy path rejects
unauthenticated and unauthorized requests without a NOR write, durably accepts
exactly one request and rejects a second novel request at the qualification cap,
commits `Preparing` before touching the real `NodeInterfaceSupervisor`, then
drives that supervisor, `ExactLoRaAirtimePolicy`, the real dispatcher, and a
host radio through exact frame persistence, durable echo, and completion. It
then projects a delivery timeout, enforces principal-scoped status, and remounts
the same durable final state. The fault path injects a wrong journal binding
after frame exposure with an ordinary announce queued behind the DATA owner;
`ActiveOwnerFailStopped` retains every owner and acknowledgement gate, takes the
LoRa lease offline, and permits no later host-radio TX or RX. This qualifies the
software composition, not ESP32-S3 execution or RF hardware.

The journal's isolated powered storage HIL passed on
board E9:44 from clean source `7b47113`: one counted capture spans raw-flash
format, five appends, mutation-free retry/conflict checks, A1-to-B2 compaction,
a software reset and zero-write/zero-erase B2 replay. An independent raw dump
check confirms generation 2, all five committed records, the revision-4
`Delivered` state, the erased A manifest and the erased B tail. Evidence is at
`artifacts/storage-hil/20260716T211318Z-e944-7b47113`.

That powered run qualifies only the journal's isolated clean path and software-
reset replay. The permanent E290 graph now has separate first powered-smoke
evidence from source `96e38aa`: both erased boards received and exactly read
back the same 729,504-byte image (SHA-256
`3b6c07d6c23265b5655901d0b9c62ce1dfafe92251372ef9f51aa11132371e5d`), reported
8 MiB PSRAM, classified credentials as `UninitializedErased` with zero recovery
steps/writes/erases, kept the API/session/bearer closed, mounted the journal,
brought LoRa/interface service ready, and completed two ordinary one-frame TXs
each. Both post-boot credential partitions remained entirely `0xff`.
This does not qualify controlled peer RX, DATA, pairing/authentication,
power-cut recovery, stack high-water, heap pressure, or the full powered owner
graph. Device-API dispatch is a
separate portable integration boundary: the target-safe authenticated adapter,
COBS framing, immutable credential authority, qualification-session core, boot-
lifetime job handoff, and E290 `ProductStorageCoordinator` port implementation
are compiled, and schema-2 acceptance retains exact authorization provenance,
but no persistent credential provisioning/pairing, external
firmware lane, or USB/BLE/Wi-Fi bearer serves through them.
The legacy `TxSupervisor` remains a separate RF-inert test aggregate. The permanent
`NodeInterfaceSupervisor` now owns the router, DATA and ordinary coordinators,
and both permit-service families; the first permanent E290 image composes it
with the ticket-aware dispatcher and E290 radio owner. That image now has the
bounded powered smoke above; broader product-graph qualification remains open.
The physical HF-module gate itself is satisfied. The older
Tracker product graphs remain TX-free, while both attached Tracker boards are
still cleared for NA915 development transmission and currently contain the
same completed final explicit-configuration semantic-roundtrip image. The
one-radio rule applies inside each LoRa actor, not across the product or its
future interfaces.

The dependency-guarded `reticulum-radio-tx-dispatch` is now the
firmware-includable persistent serializer for DATA and ordinary Rete action
owners accepted directly from one ticketed interface-actor queue over one
board-neutral `SoleRnodeRadio`. It retains the exact router ticket throughout
both typestate families, performs mandatory randomized backoff and bounded
CAD, validates the actor-stamped interface configuration, maps the exact radio
configuration fingerprint and aggregate airtime into the generic
resource-and-units permit only after a clear observation, keeps frame count and
radio meaning local, applies a fresh post-grant access check, and makes one
logical-packet radio call spanning both physical RNode frames. RX start is an
explicit scheduler choice rather than an implicit queued-TX priority rule, and
the actor exposes cancellation-safe completion-capacity readiness while
retaining its exact completion. Completion pressure, dropped CAD/TX/RX futures,
partial frame progress, and unmatched non-`Copy` control values remain retained
rather than fabricated or lost.

The direct-dependency guard keeps Embassy Futures test-only, and the target-all
normal graph guard pins all 64 current package identities and their
dispatcher-specific enabled-feature sets: local identities require exact
names, versions and workspace-relative manifest paths, while registry and Git
identities require exact names, versions and sources. The reviewed set includes
the required portable Rete, crypto, Embassy, critical-section and HAL-trait
packages; any new concrete platform HAL, driver, board, firmware, project
storage/projector, device-API, RF-inert dispatcher, supervisor or innocuously
named wrapper is therefore rejected. A separate exact manifest guard fixes
`reticulum-radio-interface` to `lora-modulation`, local RNS conformance, and
test-only Embassy Sync with their reviewed pins, paths, default-feature
settings, and empty feature selections. Both E290 boards have now passed
identity-gated flash and PSRAM qualification, and their distinct HT-RA62 radio
owner passes its software gates without reusing the Tracker's external-FEM
policy. The now-powered E290 semantic-HIL image is the controlled same-image
ANNOUNCE/DATA/proof fixture; its bounded clear CAD before each transmission
passed the intended register/CAD/RX/TX smoke evidence without another
throwaway image. The
permanent autonomous image has a separate boot/radio/ordinary-ANNOUNCE pair
test and cannot originate controlled DATA until an injector or local
submission/API edge exists.
The DATA router, both permit-only services and permanent aggregate are now
connected to the dispatcher, sole-Rete owner, timed RNode RX and one E290 LoRa
actor in the first permanent build-verified target. Its exact authorized-frame
durability handoff and ADR 0005 active-owner fail-stop now pass cross-layer host
composition tests. Live external admission is blocked by explicit credential
initialization/provisioning, pairing implementation, the external API/session
firmware lane, and a bearer—not by credential-store boot composition or another
semantic authority, session-crypto, durability-policy, partition, or cap
decision. The next software slice implements ADR 0009's bounded
physical-presence initialization/pairing manager, followed by that credential-backed USB-to-LoRa edge
and durable configuration/message hosting and client delivery.
The node-side routing
boundary remains interface-neutral so additional Reticulum links can be added
later through adapters without rewriting the LoRa actor or protocol owner; no
second transport is required to qualify the first LoRa vertical slice.

## Read first

- [Architecture](docs/firmware-architecture.md)
- [Vision Master E290 primary target](docs/heltec-vision-master-e290.md)
- [Permanent LoRa-first E290 node](docs/e290-node.md)
- [Phase-0 scaffold decision](docs/adr/0001-phase-0-scaffold.md)
- [Rete provisional-foundation decision](docs/adr/0002-rete-provisional-foundation.md)
- [LoRa-first heterogeneous-interface decision](docs/adr/0003-lora-first-interface-fabric.md)
- [Sole-flash coordinator decision](docs/adr/0004-sole-flash-coordinator.md)
- [Active DATA durability fail-stop decision](docs/adr/0005-active-data-durability-fail-stop.md)
- [Authenticated local device-API bearer decision](docs/adr/0006-authenticated-local-api-bearer.md)
- [Device-API credential authority decision](docs/adr/0007-device-api-credential-authority.md)
- [Durable authorization provenance decision](docs/adr/0008-durable-authorization-provenance.md)
- [Device-API credential store and pairing decision](docs/adr/0009-device-api-credential-store-and-pairing.md)
- [Transport-neutral interface registry and router](docs/interface-router.md)
- [Phase-0 validation contract](docs/phase-0-acceptance.md)
- [Phase-1 receive-only slice](docs/phase-1-rx-slice.md)
- [Phase-1 Tracker RX hardware qualification](docs/phase-1-rx-hil.md)
- [Phase-1 exploratory Tracker transmit HIL](docs/phase-1-tx-hil.md)
- [Device API v1 logical protocol](docs/api/device-api-v1.md)
- [Bounded node-core external-buffer packet dispatch](docs/node-core-outbox.md)
- [Owning async TX handoff](docs/async-tx-handoff.md)
- [Transport-neutral permanent node supervisor](docs/tx-supervisor.md)
- [Durable submissions and persist-before-ack projection](docs/durable-submissions.md)
- [Transport-neutral durable submission runtime](docs/submission-runtime.md)
- [Portable sole storage actor](docs/storage-actor.md)
- [Rete upstream hardening backlog](docs/rete-upstream-backlog.md)
- [Dependency provenance](docs/provenance.md)

## Toolchains

Host tools and portable crates use the Rust version pinned by
`rust-toolchain.toml`. ESP32-S3 builds use Espressif's separately installed
Xtensa toolchain:

```sh
espup install --targets esp32s3 \
  --toolchain-version 1.95.0.0 \
  --name esp
source ~/export-esp.sh
```

The export step is required for the Xtensa GCC linker. Check the local setup:

```sh
cargo run -p xtask -- doctor
```

## Initial checks

```sh
cargo test --locked
cargo test --locked -p reticulum-device-api --features experimental-rns-data
cargo test --locked -p reticulum-device-api-adapter \
  --features experimental-rns-data
python3 interop/python/generate_device_api_session_vectors.py --check
cargo run --locked -p reticulum-conformance-rete
cargo check --locked \
  -p reticulum-rns-conformance \
  -p reticulum-rns-rete \
  -p reticulum-rns-rete-rx \
  -p reticulum-device-api \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  -p reticulum-node-core \
  -p reticulum-radio-lora-phy \
  -p reticulum-radio-tx-dispatch \
  -p reticulum-semantic-roundtrip-hil \
  -p reticulum-storage-model \
  -p reticulum-submission-projector \
  -p reticulum-submission-runtime \
  -p reticulum-tx-handoff \
  -p reticulum-tx-dispatch \
  -p reticulum-tx-supervisor \
  -p reticulum-radio-interface \
  -p reticulum-board-heltec-vision-master-e290 \
  -p reticulum-board-heltec-vision-master-e290-radio \
  -p reticulum-board-heltec-tracker-v2 \
  -p reticulum-board-heltec-tracker-v2-radio \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked \
  -p reticulum-device-api \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  -p reticulum-node-core \
  -p reticulum-board-heltec-vision-master-e290 \
  -p reticulum-board-heltec-vision-master-e290-radio \
  -p reticulum-board-heltec-tracker-v2-radio \
  -p reticulum-radio-lora-phy \
  -p reticulum-radio-tx-dispatch \
  -p reticulum-storage-model \
  -p reticulum-submission-projector \
  -p reticulum-submission-runtime \
  -p reticulum-tx-handoff \
  -p reticulum-tx-dispatch \
  -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-qualification \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-semantic-hil \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --target xtensa-esp32s3-none-elf
```

The E290 qualification binary must be built in release mode and flashed only
after recording each board's physical flash identity with `espflash
board-info`. It keeps the SX1262 in reset, uses a low-address-only partition
table, and verifies the complete mapped PSRAM range before registering PSRAM
with the allocator. See [the E290 target dossier](docs/heltec-vision-master-e290.md)
for the cutover and first-flash sequence.

The separate [E290 semantic HIL runbook](docs/e290-semantic-hil.md) describes
the same-image signed-ANNOUNCE, encrypted-DATA, delivery-proof and CAD/RX/TX
vertical slice. The exact 421,296-byte artifact passed on both physically
confirmed `HT-RA62-HF` boards. A dedicated nineteen-test fail-closed verifier
accepted the E290-specific MAC/role, CAD, packet-hash, semantic-ingress,
receipt, terminal, and shutdown trace instead of reusing the older Tracker log
schema.

The [permanent E290 node runbook](docs/e290-node.md) describes the first
LoRa-first two-task product composition, its fixed capacities, 16 MiB partition
layout, durable identity/announce ordering, build gates and remaining
storage/client blockers. Its permanent image now has the bounded two-board
powered smoke above; neither that smoke nor the isolated semantic HIL
substitutes for controlled peer-RX/DATA, fault, power-cut, high-water, or full
product-graph qualification.

The receive-only lab binary has no frequency or modulation defaults. A known
host/RNode-compatible build example is:

```sh
export RETICULUM_LAB_RX_FREQUENCY_HZ=915000000
export RETICULUM_LAB_RX_SPREADING_FACTOR=7
export RETICULUM_LAB_RX_BANDWIDTH_HZ=125000
export RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR=5
export RETICULUM_LAB_RX_PREAMBLE_SYMBOLS=18
export RETICULUM_LAB_RX_EXPLICIT_HEADER=1
export RETICULUM_LAB_RX_CRC=1
export RETICULUM_LAB_RX_IQ_INVERTED=0

cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --bin reticulum-heltec-tracker-v2-lab-rx \
  --no-default-features --features lab-rx \
  --target xtensa-esp32s3-none-elf
```

Those settings authorize only a local receive experiment with a matching peer;
they are not a regional transmit profile. Missing, malformed, out-of-hardware-
range and currently unverified LDRO combinations fail the build before any
radio-bearing image is produced.

The passed same-image semantic round-trip HIL is an explicit, separately
guarded build:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-tx-hil \
  --no-default-features --features semantic-roundtrip-hil,tracker-radio \
  --target xtensa-esp32s3-none-elf
```

It is a bounded NA915 development fixture, not a product firmware profile. See
[the TX HIL record](docs/phase-1-tx-hil.md) for its exact image identity,
readbacks, exchange and limitations.

## Qualification artifacts

Phase-1 powered work uses two immutable clean-tree bundles: normal plus
backpressure, and the eight closure artifacts covering four electrical modes,
both returned-fault policies and representative retained-journal selectors
slot 0/word 4 and write ordinal 9. After exporting the eight radio-profile
variables shown above, prepare absent output directories from a clean commit
and verify them before flashing:

```sh
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
normal_pressure_bundle="artifacts/hil/phase1-rx/${stamp}-normal-pressure-bundle"
closure_bundle="artifacts/hil/phase1-rx/${stamp}-closure-bundle"
powered_evidence="artifacts/hil/phase1-rx/${stamp}-powered-evidence"

cargo run --locked -p xtask -- phase1-rx-hil-artifacts prepare \
  --output "$normal_pressure_bundle" \
  --backpressure-stall-us 7000000
cargo run --locked -p xtask -- phase1-rx-hil-artifacts verify \
  --bundle "$normal_pressure_bundle"

cargo run --locked -p xtask -- phase1-rx-closure-artifacts prepare \
  --output "$closure_bundle" \
  --journal-corrupt-slot 0 \
  --journal-corrupt-word 4 \
  --journal-torn-write-ordinal 9
cargo run --locked -p xtask -- phase1-rx-closure-artifacts verify \
  --bundle "$closure_bundle"

cargo run --locked -p xtask -- phase1-rx-powered-evidence init \
  --normal-pressure-bundle "$normal_pressure_bundle" \
  --closure-bundle "$closure_bundle" \
  --output "$powered_evidence"
```

Both artifact manifests and their tool inventories bind the project commit and
its exact raw Git root tree. Source identity and archive Git subprocesses clear
ambient repository/configuration variables, disable replacement objects and
external attributes, and reject a common-directory `info/attributes`. Archive
verification reconstructs files, modes and symlinks from raw tree/blob objects;
it does not accept a second identically filtered `git archive` as proof.

Both verifiers enforce exact directory trees. Never write flash logs,
readbacks or captures into a bundle; store all mutable evidence in the sibling
`$powered_evidence/captures` directory and complete the generated operator and
scenario records. Each scenario-schema-v2 check is an object that binds its
status to specific classified capture paths; both passing and failed attempted
checks require non-empty evidence, while a check with no capture remains
`not-run`. The soak duration, pressure-stall duration and pressure counters use
narrow machine-readable observations. Once a run is over, seal and then
independently verify the exact evidence inventory:

```sh
cargo run --locked -p xtask -- phase1-rx-powered-evidence finalize \
  --evidence "$powered_evidence"
cargo run --locked -p xtask -- phase1-rx-powered-evidence verify \
  --evidence "$powered_evidence"
```

Stop all record and capture writes before `finalize`. Finalization takes a
persistent single-writer sibling lock named
`<evidence>.phase1-powered-evidence-finalize.lock`; that coordination file is
outside the exact evidence tree and may be retained. An interrupted finalize
remains explicitly incomplete and can be rerun: it recovers only its reserved
temporary/final inventory files, rebuilds and syncs the inventory, then commits
the seal with one same-directory rename from `powered-evidence.incomplete` to
`powered-evidence.sealed`. Repeating `finalize` on an already sealed directory
is idempotent and performs full verification.

These commands are host-only: they accept no serial port and perform no flash,
monitor or RF operation. Sealing preserves honest `pass`, `fail` and `not-run`
results; it reports `pass` only when every generated gate record passes, every
required readback is bound to its prepared image, every check has classified
evidence of the required role, the soak records at least 86,400 seconds, the
pressure record contains exactly 7,000,000 microseconds and `3/2/1`
offered/queued/dropped deltas, and the electrical matrix names at least two
board samples. The validator does not parse arbitrary instrument formats;
operators and reviewers remain responsible for the content of the hash-sealed
captures. Peer-driven passing records additionally require operator-schema-v2
paths to a regular copied peer image, a self-contained peer-source Git bundle,
the pinned corpus and peer-tool files below `captures/`; the verifier hashes all
four, binds those bytes to the operator digests, verifies the official peer
commit and root tree from the Git object graph, and requires the copied corpus
and tool to equal the files in the qualification bundle's verified `source.tar`
rather than the current checkout.
Every required peer invocation must list a parsed `peer-manifest.json` and its
sibling `peer-transcript.jsonl`; verification parses the strict JSONL request,
reply, READY and DATA state machine and binds its digest, interval and payloads
with the manifest's successful enqueue status, exact scenario, target mode,
radio profile, firmware, corpus, tool and step count to the powered record. The
boot-local custom corpus is regenerated byte-for-byte by the shared,
schema-frozen generator. The unsigned seal proves internal
consistency, not measurement authenticity; externalize its SHA-256 in a signed
or write-once run log for archival trust. The manifest records canonical
absolute bundle paths, so both immutable sibling bundles must remain at those
paths for later verification. CI runs the host/negative tests, strict target
selector checks and the public eight-build closure prepare/verify pipeline for
the GitHub merge commit. It does not preserve that ephemeral smoke bundle as
qualification evidence. See the
[hardware qualification runbook](docs/phase-1-rx-hil.md) before any powered or
RF operation.

To regenerate and independently check the released-Python wire corpus, use
CPython 3.13.7, install `interop/python/requirements-rns-1.3.8.txt` in an
isolated environment and set `PYTHON` to that environment's interpreter:

```sh
python3.13 -m pip install \
  --target artifacts/phase0/rns-1.3.8-python \
  -r interop/python/requirements-rns-1.3.8.txt
PYTHONPATH=artifacts/phase0/rns-1.3.8-python PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rns-vectors
PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rnode-hil-vectors
```

The second command verifies the deterministic RNode 1.86 HIL corpus, its
project-owned KISS peer tool and the same corpus replayed through the Rust
receive-only RNode/Rete ingress tests. It does not transmit; RF requires the
separate explicit `send` command documented in the Phase-1 HIL runbook.

The default and receive-only Tracker binaries remain TX-disabled, and there is
intentionally no default LoRa frequency. Both antenna-equipped development
boards are authorized for the explicit NA915 profile; guarded integration
images and the derived RNode peer may transmit whenever useful.

## Source layout

```text
crates/          portable contracts, the provisional Rete foundation, and board data
comparisons/     separately licensed RNS oracle/fallback graphs
firmware/        target binaries
interop/         pinned peer revisions and generated-vector provenance
tools/           host conformance runners
xtask/           reproducible development commands and environment checks
reference/       ignored research checkouts; never a build dependency
```

Project-owned code is licensed under either MIT or Apache-2.0. Separately
licensed fallback and future derived-code boundaries are documented in
`docs/provenance.md`.
