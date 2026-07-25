# Reticulum Rust firmware

Bare-metal Rust firmware for a standalone Reticulum node. LoRa is the first and
primary complete transport vertical slice, using the ESP32-S3R8 and
HT-RA62/SX1262 combination on the Heltec Vision Master E290-HF. The node, Rete
core, registry, and router remain interface-neutral so a later second radio,
USB, Wi-Fi, or BLE link can join as an independent Reticulum interface without
pretending to be LoRa. None of those later Reticulum packet actors is being
implemented in parallel with the first LoRa path. USB's first product role is
the authenticated local client/control API. One Expo application now owns the
shared web, iOS, and Android client surface, and its compiled UniFFI bridge has
completed an Android/iOS immutable-contract round trip. A signed,
self-contained iOS Release has also completed the
[bounded physical foreground proof](docs/e290-expo-ios-ble-lora-proof.md):
system-file credential import, credential-derived exact-E290 authenticated BLE,
and one sequential LXMF message in each direction over LoRa. A follow-up cold
launch automatically reconnected in the foreground, and the keyboard-aware
composer passed physical use. Wi-Fi remains build/host-qualified. The full
mobile lifecycle matrix, background restoration, Android hardware, BLE
pressure/soak, and cross-instance `BleManager` ownership remain open. Those
bearers become Reticulum packet interfaces only through
optional actors added after the LoRa slice. The already-qualified Heltec
Wireless Tracker V2.3 pair remains a constrained LoRa regression target.

Current source also removes typed peer endpoints from the normal Expo path.
Each E290 projects up to 32 authenticated `lxmf.delivery` announces, retaining
at most 256 application-data bytes per peer, and exposes them one record at a
time through the existing authenticated BLE device session. The Expo
**Nearby** picker refreshes that projection and adds or opens a durable contact
with one tap; manual hexadecimal entry remains an advanced fallback. This is
Reticulum-native discovery over the board's learned mesh paths, not a second
phone BLE scan and not appliance authorization. QR, native proximity APIs, and
an E290-mediated public BLE share may later carry one signed public contact
card, but they cannot transfer device-control authority. A
[bounded physical proof](docs/e290-reticulum-nearby-powered-proof.md) now
qualifies existing-contact selection without endpoint entry, one short
opportunistic LoRa message in each direction reaching `Delivered` with exact
peer import, and app-process persistence. Fresh contact creation remains open.
A [separate bounded proof](docs/e290-direct-link-powered-proof.md) now
qualifies one fresh authenticated direct Link, exact receiver commit before
proof, durable sender `Delivered`, and board/app restart persistence. A
[current-image recovery proof](docs/e290-stale-link-recovery-powered-proof.md)
additionally qualifies exact stale-session retirement after the receiver
reboots: the affected submission remains terminal `DeliveryTimeout` without a
receiver commit, while the next sequential submission establishes a fresh Link
and reaches `Delivered`. A
[same-Link reuse and replay proof](docs/e290-same-link-reuse-replay-powered-proof.md)
then qualifies two back-to-back direct-required submissions reaching
`Delivered` with one LXMF message ID, two distinct Reticulum packet hashes, and
one receiver record. Exact Link-handle reuse and the receiver's internal
`Replay` classification are source-qualified because the frozen client API
exposes neither value.

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
build-verified, and powered-qualified on both development boards for the bounded
boot/credential/ordinary-TX path, one authenticated API 1.1 outbound DATA/proof
round trip, and API 1.2 raw-RNS inbox commit/readback/hard-reset/drop-newest
behavior. A 2026-07-19 powered matrix additionally proves four exact cold-mount
fail-closed cases, each followed by one direct Reticulum DATA/proof exchange.
A separate feature-gated path proves that its triggering DATA/proof exchange
completed before same-boot commit-mismatch quarantine was observed. Later work
qualified one durable inbound LXMF exchange and bounded PSRAM/stack/timing
measurements. API 1.4 adds USB-controlled LXMF list/read and source-free basic
send. The [2026-07-22 powered POC](docs/e290-api14-lxmf-poc.md) completed both
directions: each durable submission reached `Delivered`, the peer committed the
same LXMF message ID, and authenticated chunked readback matched the exact
normalized wire. The final audited image then matched exact writes/readbacks on
both boards; after a physical CPU reset, both original submissions remained
`Delivered` and both receivers served the same digest-verified 126-byte wires.
The later
[persistent chat-alpha powered proof](docs/e290-lxmf-chat-alpha-proof.md)
qualifies the current 128-entry image and SQLite CLI workflow in both
directions, including repeated same-enumeration authenticated sessions,
terminal outbox reconciliation, exact peer import, and inbox deduplication.
The subsequent
[host-appliance alpha proof](docs/e290-lxmf-appliance-alpha-proof.md) adds a
single-owner background service, SQLite schema-2 authenticated database binding,
exact-serial discovery/reconnect, and a bundled loopback client/API. One message
queued through that HTTP boundary reached `Delivered` over the E290 LoRa link
and was imported verbatim from the peer. This is a host-companion milestone;
the former hand-built SPA has since been replaced by a universal Expo client
and Rust-generated TypeScript API types, but its current bundled web export is
still not served by the E290 over USB or Wi-Fi.
That POC's initial startup failure was a cumulative pre-USB mount-stack
overflow, not a USB or board-enumeration fault; direct in-place placement of
the upper runtime/actor chain fixed that diagnosed boundary. The initially
flashed 128-entry successor then failed the final linked-path stack gate and
was not qualified. Current source adds an actor-owned replay scratch index in
PSRAM and gates the complete mount, append, and compaction paths, preventing
their capacity-sized replay state from overflowing the CPU stack. Physical
power cuts, a powered 128-message fill/pressure run, and
full-product qualification remain open. The
bounded client, storage, carrier, security, and multi-transport gaps are
tracked explicitly in
[the usable-firmware POC limits](docs/poc-known-defects.md).
Bitrate and cost are recorded but do not replace Reticulum routing. Until Rete
paths carry an interface generation, an ID/configuration stays immutable for
one node-owner lifetime or its learned paths must be purged before reuse.
The current Rete pin,
`ba73ee426a3211951f5abb400c5728dd359272be`, is the exact revision on
fork branch `codex/responder-handshake-reclaim`. It descends from
`338251b285a2447beb10d390d3e7f53694a1a916`, which adds bounded canonical
MessagePack request values including the anonymous `nil` value, and
`a443173b0829c2637ce23531a8cde15fdfec185e`, which descends from
`2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` on
`codex/link-data-receipts`, which descends from
`90570cafc812b3025011cb690ec74a27f287cb3f` (tagged
`firmware-pin-90570ca`; that predecessor tag does not name the current
revision). The current revision additionally keeps prepared request ownership
separate from confirmed dispatch ownership and starts the response timeout
only at the first real interface dispatch. It also preserves validated inbound
non-binary/string MessagePack request values, including their wire timestamp,
as an exact application event. This is bounded request foundation, not a claim
of complete NomadNet or Resource support. The pin carries learned
path, reverse, and Link decisions as
exact interface targets instead of falling back to interface zero or generic
broadcast. An exact target may intentionally equal the ingress slot, which is
required to relay between peers on one shared LoRa interface. Reverse proofs
are one-shot and return only from the interface to which the covered packet was
forwarded; relayed Link proofs additionally require the stored direction and
hop count, and LRPROOF requires a known, reconstructable responder identity and
a valid signature. A targeted HEADER_2 LRPROOF is normalized into that
canonical validation instead of bypassing it through generic Link transport;
only a direction-, hop-, identity- and signature-valid proof is forwarded. Owned
HEADER_2 traffic now reaches normal local DATA, LINKREQUEST, Link, proof and
receipt dispatch. Transported HEADER_2 DATA/SINGLE and LINKREQUEST/SINGLE use
transactional exact-path relay admission; owned- and relay-Link exhaustion,
reverse-table exhaustion, and truncated reverse-route conflicts are returned as
typed ingress rejections without forwarding or partial routing state. Foreign
non-ANNOUNCE HEADER_2 traffic is filtered before native state mutation, while
HEADER_2 ANNOUNCE remains eligible for ordinary announce validation. Relay-Link
occupancy is observable separately from locally owned Links. Arbitrary remote
HEADER_1 LINKREQUEST remains fail-closed until interface roles distinguish it
from local-origin injection, and HEADER_1 DATA retains a guarded compatibility
shim for that same role boundary.

Locally owned Links now acquire an authenticated interface binding instead of
using a generic broadcast after establishment. A responder binds to the
LINKREQUEST ingress interface; an initiator remains unbound after sending its
request and binds only when a valid LRPROOF arrives. Subsequent application,
close, keepalive, retransmit, request/response and Resource output carries
native `BoundInterface`, which the project adapter resolves to the exact
physical interface. Within this owned-Link lifecycle, only an initial
LINKREQUEST whose path has no recorded interface may broadcast. Link DATA and
`RESOURCE_PRF` received on another interface are rejected before deduplication,
so a later copy on the authoritative interface is still admissible.
Ordinary context-`NONE` Link DATA now receives a distinct `LinkData` receipt,
providing the Rete prerequisite for its exact proof to drive product delivery
state without conflating it with destination DATA or Channel traffic. Product
Link orchestration and receipt-kind-safe attempt correlation are integrated
for the bounded outbound one-packet direct path. Exact stale-session retirement
and fresh-Link recovery by a later sequential submission are source- and
powered-qualified. Per-Link single-flight and the bounded successful
same-Link/direct-replay outcome are also source- and powered-qualified;
responder/backchannel reuse, Resource, and the broader fault/pressure matrix
remain. Proof emission follows the receiving destination's `PROVE_NONE`,
`PROVE_ALL`, or application-selected policy instead of being unconditional.

The current binding is an interface-slot index, not a shared-host client
endpoint. On Rete's Tokio `Hub`, synchronous output can retain the originating
client, but asynchronous owned-Link output still broadcasts to sibling clients
on the bound slot until endpoint-aware identity and reincarnation are carried
through Link state. Keepalive wire and lifecycle parity is now implemented as
exact unencrypted 20-byte Link DATA: only the initiator sends `0xff` after both
a full inbound-silence interval and a full interval since its previous probe,
only the responder returns `0xfe`, and neither packet becomes application data.
Valid role-specific deterministic repeats bypass dedup only after the
bound-interface gate; automatic probes and replies
retain `BoundInterface`, and routing is preflighted before probe timestamps are
committed. A Link becomes stale after two keepalive intervals and retains a
revival window of `4 * RTT + 5 seconds` from the actual stale transition/final
probe (five seconds when RTT is zero); valid bound Link traffic revives it.
Channel sends now preflight MDU, pending
window allocation, and receipt capacity before entropy or logical mutation.
Maintenance discovers immutable retry tokens, NodeCore preflights the
authoritative Link route, and fresh-ciphertext retries atomically replace the
envelope's sole live receipt/proof target before retry, window, and timestamp
state commits. Obsolete proofs fail closed, replacement works at full receipt
capacity, and Link removal reclaims channel receipts. Pending-Link hop parity
is now explicit: an initiator snapshots the known path's hops when it creates
the Link, or uses the `PATHFINDER_M = 128` wildcard when no path is known;
LRPROOF hop mismatches are rejected before deduplication or Link-state mutation.
A responder records the post-ingress hop only after authenticating and
decrypting LRRTT. Pending-handshake LRRTT payload parity is now covered: the
initiator emits canonical MessagePack float64, while the responder accepts the
numeric scalar families returned by Python's u-msgpack, consumes the first
object while permitting trailing bytes, and uses the greater of its local and
the peer RTT. Link timing now uses microsecond `MonotonicInstant` and
`MonotonicDuration` values and stores RTT as binary64. The request anchor is
immutable: an initiator samples before LINKREQUEST egress begins, while a
responder samples after LRPROOF egress completes. Each outbound protocol edge
is correlated by an opaque, non-repeating eight-byte token and only the first
successful interface confirmation is accepted. In this firmware, confirmation
is the generic ordinary router/interface handoff interval, not physical LoRa
RF `TxDone`; the interval's start anchors an initiator and its completion
anchors a responder.

Fresh authenticated LRRTT is processed in `Handshake`, `Active`, and `Stale`.
The initial transition emits establishment once; an Active update or Stale
reactivation refreshes RTT, activation, hop, and keepalive state and emits
`LinkRttUpdated` without duplicating establishment statistics or events. Exact
raw replay remains deduplicated. Authenticated malformed or nonnumeric LRRTT
tears down any of those three states; only a Handshake failure increments
`links_failed`. A measured zero RTT remains zero and selects the 5-second
keepalive/10-second stale floor, while nonzero RTT uses the dynamic stale grace
`4 * RTT + 5 seconds`. Rete intentionally authenticates before changing
liveness, so corrupt stale LRRTT does not revive a Link; released Python 1.3.8
updates liveness before decryption.

The current descendant also reclaims a responder that remains in `Handshake`
without authenticated LRRTT. Its Python-compatible budget is 360 seconds plus
six seconds times at least one post-ingress hop, measured from confirmed
LRPROOF dispatch completion when available and otherwise from LINKREQUEST
admission. Expiry releases the owned-Link slot and increments aggregate
`links_closed`, but is not a malformed or cryptographic `links_failed` event.
Initiator establishment remains bounded by the product-owned dispatch-relative
deadline and exact abort operation.

Rete takes one pre-decrypt ingress sample for the bounded synchronous handler,
where Python takes three internal samples. The project adapter uses the precise
`*_at` ingress/tick paths and confirms output at the transport-neutral ordinary
router acceptance boundary. Rete's generic Tokio and Embassy runners still use
coarse/unconfirmed compatibility paths until they adopt the same contract.
The released-Python schema-2 corpus source-hash-binds `Link.py` and `Packet.py`,
executes the released request/proof/send methods through a recorded
`Transport.outbound` boundary, and applies five case-unique packets directly to
released `Link.receive`: Handshake valid, Active valid repeat, Stale valid
repeat, Stale decrypt failure, and authenticated malformed Active. The probe
explicitly excludes `Transport` exact-replay deduplication and the full real
teardown's external side effects; Rust tests cover those boundaries separately.
Remaining Link work includes established-Link watchdog timeout `LINKCLOSE`
emission and shared-Hub endpoint/reincarnation identity. Adaptive channel
windows can also exceed the product's `L` receipt capacity; that produces typed
backpressure and remains a sizing/throughput policy decision.
Snapshot loading currently restores identities only; saved paths and cached
announces remain inactive until stable interface rebinding is defined.

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
legacy bounded RX, persistent continuous RX, CAD, and atomic one/two-frame TX
mechanics now live in `reticulum-radio-lora-phy`. The
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
uninitialized-erased, initialization-interrupted, blocked, corrupt, or backend-
failed, and transfers the exact boot binding and any mounted owner into the
resident `CredentialRuntime` inside `ProductStorageCoordinator`. These developer
partitions are deliberately plaintext, so a provisioned full flash dump is
secret material.

Current source also validates and read-only mounts the exact 2 MiB
`message_store` through the same sole flash owner. ADR 0011 format 1 admits one
576-byte commit-last raw-RNS record containing a 16-byte destination and at most
383 decrypted payload bytes; the rest of the range must remain erased. Mount or
admission failure disables only inbox capability for that boot rather than
preventing ordinary Reticulum service. This plaintext qualification record is
not the eventual LXMF store.

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
optional pending cell is compile-time capped at 544 bytes. Its focused host
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
stays disabled and route-only LoRa continues. The current PSRAM product profile
retains 128 accepted-history entries and reports explicit capacity exhaustion
for a 129th novel request. This remains a bounded non-reclaiming profile, not
the intended long-term product capacity: the physical journal has a separate
154-acceptance lifetime ceiling. Earlier one-entry and 16-entry proof artifacts
remain valid only for their named source/image and must not be read as the
current limit or as powered qualification of the 128-entry profile. The
source-composed minimal authenticated USB
lane now originates work through this path from an Active credential; the
bounded powered E290 run completed durable acceptance, physical LoRa DATA/proof,
terminal projection, and status after USB re-enumeration. Node-core emits an
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

The transport-neutral Rete boundary now moves every local `DataReceived` event
into a fixed project-owned value. The permanent node converts that value into an
ADR 0011 inbox candidate, retains one candidate across credential/journal flash
deferral, and otherwise commits or drops newest. `reticulum-rns-inbox-store`
binds the complete E290 `message_store` to the physical device/range/format and
accepts only erased media or one canonical committed item with an erased
remainder. It performs no erase and exposes no acknowledgement, deletion,
overwrite, or reclamation. The boot-local saturating drop counter is separate
from the committed plaintext destination/payload.

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
target-safe submission, raw-inbox, committed-LXMF-read, and device-owned
LXMF-compose semantic boundaries. With the
`experimental-rns-data` feature, the indexed-CBOR request and borrowed payload
are converted into one owned acceptance candidate, and an ID is returned only
after the submission port reports durable acceptance or exact replay. With
`experimental-rns-inbox`, authenticated status and peek obtain only bounded
state or an owned item copy; there is no persisted permission bit or mutation
operation. With `experimental-lxmf`, authenticated clients can enumerate and
chunk-read committed normalized messages or source-free compose a basic
method-neutral send; the appliance's automatic policy currently chooses the
eligible opportunistic carrier. `dispatch_with_lxmf` does not require the
raw-inbox feature.
The resident E290 `ProductStorageCoordinator` exposes all enabled operations
through one short-lived owner and stable mappings for availability, replay,
conflict, capacity, ambiguity, and faults. Its current PSRAM profile retains 128
accepted submissions, rejects a 129th novel request without mutation, and
preserves exact replay at capacity; this is not product capacity.
Allocation-free COBS framing, bounded public `no_std`
USB-qualification server/client session typestates, and a depth-one
boot-lifetime authenticated-job handoff now define the portable edge, including
mutual PSK proofs,
directional tags, exact sequences, partial-TX ownership, reconnect epochs and
stale-reply handling. Independent Python vectors freeze the transcript and wire
records. A separate allocation-free immutable credential authority now owns the
shared ID/generation types, validates fixed `Pending`/`Active`/PSK-free
`Revoked` records, selects zeroizing handshake material, and revalidates grants
through a borrowing dispatch lease. Semantic journal schema 3 persists the
exact credential ID/generation, authority revision, policy version, and granted
permission mask with every accepted submission. Its closed intent vocabulary
keeps generic RNS DATA at 383 plaintext bytes and separately retains the exact
complete signed LXMF message through 431 bytes in a 544-byte journal body,
without binding that durable message to opportunistic or direct delivery.
The canonical authority
image, dedicated credential-partition contract, and two-sector commit/retire
format are implemented. Lifecycle-specific authority planners construct only
checked Add-`Pending`, Activate-`Pending`, and Abort-`Pending` successors; their
opaque candidates pass through typed store commit/reconcile owners that retain
the exact transition across semantic rejection and ambiguous physical results.
The supported typed product path exposes pending proof material through a
mounted publishable authority; the repository source guard confines the two
unchecked integration bridges to the semantic authority and physical store.
A separate read-only classifier distinguishes exactly erased, recoverably
interrupted canonical empty revision-1 provisioning, already committed empty
revision 1, and ineligible media without mutating either sector. The initial
bounded developer/HIL pairing policy is also implemented as a portable,
allocation-free admission owner: it freezes the exact physical-presence hold/
window, connection epoch, shared attempt budget, one-pending, and asynchronous
operation-ownership rules without owning GPIO, USB, flash, secrets, or proof
verification. The portable store is boot-mounted/recovered and retained by the
firmware coordinator, and E290 boot now maps only the exact canonical
interrupted-initialization trajectory into a distinct read-only disabled state.
A featureless pre-authentication control codec now freezes zero-session,
zero-tag status and explicit-initialization records plus their coarse public
results. It depends only on the COBS framing crate; sequence ordering, replay,
connection ownership, physical presence, task handoff, and flash mutation stay
outside the codec.
ADR 0010 now freezes the separate live Begin/ProofStart/Activate/AbortCurrent
protocol. Its allocation-free `no_std` core owns the exact fixed record layouts,
typed credential-reference continuation, HMAC-SHA256 transcript and mutual
activation confirmation while zeroizing PSKs, challenges, proofs, decoded
records, framing scratch, and encoded frames. Independent standard-library
Python vectors fix all eight successful COBS flights and both proof domains.
The core is composed only into the permanent E290 graph. Its resident
credential owner implements bounded entropy, exact Begin/ProofStart/Activate/
AbortCurrent policy completion, typed Add/Activate/Abort store mutation,
ambiguous-result reconciliation, cleanup-before-next-mutation, and exact
connection/window/deadline challenge binding. A separate bearer-neutral,
depth-one handoff returns the exact unsent secret-bearing owner under pressure.
Both handoff endpoints are instantiated: the node schedules the resident
lifecycle through the journal-aware causal frontier, while the sole USB owner
demultiplexes control and live records in one exact-next sequence space.
The feature-free pairing policy is now a permanent E290 dependency only, and a
resident `CredentialRuntime` inside `ProductStorageCoordinator` privately
retains that policy, the exact boot binding, any mounted authority, and any
admitted initialization permit. Its physical drive reclassifies a fresh bound
view and accepts only forward progress along the exact erased or interrupted
trajectory, retaining ownership across ambiguous results. The coordinator's
compiled sole-owner initialization port freshly reinspects node identity and
creates the short-lived bound credential view. The node task invokes that port
through the pre-authentication USB/GPIO command lane; powered testing has
completed button-confirmed initialization, durable activation, and exact
post-write credential readback.
Its explicit cross-store gate defers initialization while journal mutation is
retained and defers journal mutation or new submission acceptance while
initialization is in flight, without disabling projection, status, routing, or
LoRa service.
Boot never starts initialization automatically. Live lifecycle ownership is
resident and host/target verified; the node now schedules it through the
bearer-neutral owning handoff, and the sole USB owner multiplexes all four live
pairing records with initialization control in one exact sequence space. The
permanent graph now additionally composes the feature-free session/handoff
crates, a static depth-one authenticated API channel, and the first deliberately
minimal USB session bearer. Its node endpoint revalidates current authority and
dispatches synchronously through one credential-disjoint, operation-scoped
owner implementing narrow submission, raw-inbox, committed-LXMF-read, and
LXMF-compose ports.
The USB endpoint admits one active authenticated session and one request at a
time. A canonical ClientHello can replace an idle established session on the
same connection with a fresh session epoch; it never displaces an in-flight
request or reply. Any session fault is terminal until USB reset or
re-enumeration. This first profile deliberately omits resumption, protocol
retries, close records, encryption, rate limiting/attempt policy, and concurrent
requests. Credential selection, admission
handoff, and node dispatch remain transport-neutral. USB Serial/JTAG uses the
wired qualification suite; BLE has its separate suite-3 binding and a bounded
powered iOS qualification. Wi-Fi has an implemented, host-qualified suite-2
binding plus the E290 SoftAP/raw-TCP endpoint and native proof connector; its
powered field qualification remains open. Focused
host, compile-fail,
and fake-NOR gates cover the credential authority, successor rules, and physical
store. The accepted
authentication, authority, provenance, and USB ownership contracts are
recorded in [ADR 0006](docs/adr/0006-authenticated-local-api-bearer.md) and
[ADR 0007](docs/adr/0007-device-api-credential-authority.md), with the durable
schema transition in
[ADR 0008](docs/adr/0008-durable-authorization-provenance.md) and the physical
store/pairing decision in
[ADR 0009](docs/adr/0009-device-api-credential-store-and-pairing.md). Default and
experimental host tests/clippy plus the corresponding ESP32-S3 Xtensa checks
pass.

The default, opt-in inbox-commit-fault, and runtime-measurement E290 validation
profiles cover the resident live-pairing
lifecycle, its causal control/live frontier, shared USB decoder and
sequence gate, secret-owning handoff, initialization/product policy,
authenticated inbox-port isolation, and
cross-layer composition. The durable-capacity path rejects unauthenticated and
unauthorized requests without a NOR write, durably accepts 128 submissions,
rejects a 129th novel request without mutation, and preserves exact replay
of an accepted idempotency key at capacity. The routing happy path
commits `Preparing` before touching the real `NodeInterfaceSupervisor`, then
drives that supervisor, `ExactLoRaAirtimePolicy`, the real dispatcher, and a
host radio through exact frame persistence, durable echo, and completion. It
then projects a delivery timeout, enforces principal-scoped status, and remounts
the same durable final state. The fault path injects a wrong journal binding
after frame exposure with an ordinary announce queued behind the DATA owner;
`ActiveOwnerFailStopped` retains every owner and acknowledgement gate, takes the
LoRa lease offline, and permits no later host-radio TX or RX. This qualifies the
software composition, not ESP32-S3 execution or RF hardware.
The current root gates include focused host-client, portable Rete-integration,
raw-RNS inbox-store, durable lifecycle/candidate, released-vector, direct-Link,
LRRTT, channel-retry, keepalive, and deterministic three-node A--B--C relayed
Link/LRPROOF/channel/proof coverage. The project adapter tests remain separate
from the pinned Rete fork's selected validation set. This is deterministic
project-side conformance, not a powered or live-Python multi-hop claim. At the
preceding
`8b5d65283cd370dee4cbb17594ef9c88d2805416` pin, the selected upstream set
passed 635 tests: 271 transport (174 library plus 97 integration: 9
computed-vector, 43 forwarding, 40 Link-integration, and 5 path-request), 137
stack (136 library and one integration), 143 LXMF library, and 84 daemon
library tests. The four library targets totaled 537 tests; adding the 97
transport and one stack integration tests produced 635. This is a named
selected set, not a count of every nested workspace test target.
The focused current Rete regression lane is also run separately from that
historical selected set.

The preceding `14c7b4955a1ff6903e87cc40b42498f7869b6f4f` pin had host and
portable-target LRRTT validation and a build-only default E290 package. Its
776,464-byte merged image uses
710,928/6,291,456 application bytes (11.30%) and has SHA-256
`7b11c6f6a3c039d46ab0117fd362920aaa40145e7f27cbc6fa0a8a84a7ab3571`.
It has no flashed-image readback or powered proof. The preceding pre-PSRAM
application-event ownership default E290 release links with text/data/BSS of
684,167/3,676/469,152 bytes (1,156,995 bytes total by GNU size), and its
12,345,320-byte ELF has SHA-256
`ebb34e7176a8e61b6969ebf99d7dac97c6e674ef5e583bbf931a34e8b6e970a2`.
The explicit 16 MiB package is a 789,504-byte merged image, uses
723,968/6,291,456 application bytes (11.51%), and has SHA-256
`1796f161c480d0348e3d47fd8f3cda5fda5b51aa38ad6024aaad04c8ba1751ce`.
The matching pre-PSRAM runtime-measurement HIL rebuild links with
text/data/BSS of 695,315/4,180/468,648 bytes (1,168,143 bytes total). Its
12,498,348-byte ELF has SHA-256
`c84363dff0801a1679dd786b5070c4662962d299f0269efc0cd72ff9c09b8e2a`;
the 800,480-byte merged image uses 734,944/6,291,456 application bytes (11.68%)
and has SHA-256
`058a969e0b9e099f6a5febd1b59f4a70cfd3ea932e8f0738a2ddb4b3e5569119`.
That HIL matched an exact address-zero readback on `3e:88`. At uptime
108,940 ms, an authenticated API checkpoint reported 8 MiB PSRAM, 928 bytes
maximum heap use, 64,608 bytes minimum internal-heap free, no external-heap
use, and 63,828 bytes of painted stack remaining. Subtracting the unchanged
53,680-byte maximum frame leaves a 10,148-byte conservative powered margin.
One API dispatch completed in at most 594 us; the checkpoint recorded no
unexpected error, failed allocation, radio watchdog timeout, or correlation
fault, and both radio transmissions were confirmed successful. The board was
then restored to an exact-readback 789,504-byte default rebuild, SHA-256
`a67afa72681558dc02fd0575a18711b2b3c05b365a66af45441b7cb8dd3a2577`,
and served an authenticated `identity-summary`. Board `3f:88` still did not
enumerate, so that historical checkpoint is not two-board lifecycle/RF
qualification.

The first API 1.4 default image nested 175,296 bytes of compiler-emitted
pre-USB mount frames in a 174,912-byte usable stack; the corresponding HIL
chain exceeded its usable stack by 1,360 bytes. The former maximum-single-frame
gate could not detect that cumulative overflow. Direct caller-destination
placement of the upper runtime and actor reduced those historical default/HIL
mount chains to 142,432/142,608 bytes.

The initially flashed 128-entry successor failed the expanded final linked-path
gate and was not qualified. Current source keeps an independent replay scratch
index inside the PSRAM-resident `StorageActor`: boot initializes both indexes at
their final addresses, while append validation and compaction replay into the
scratch index without moving a capacity-sized value through the CPU stack or
overwriting the live index before a durable result. The target runtime is
375,544 bytes; its 64-bit host fixture is 375,568 bytes. The final inspector
gates these compiler-emitted path sums:

| Profile | Mount | Append | Compact |
| --- | ---: | ---: | ---: |
| Default | 53,072 B | 52,816 B | 52,704 B |
| Runtime-measurement HIL | 53,248 B | 53,040 B | 52,928 B |

Each path also carries a 4,096-byte ROM flash-read/interrupt reserve, so the
enforced totals are 57,168/56,544/56,432 bytes for default and
57,344/56,768/56,656 bytes for HIL. This statically closes the diagnosed
boot/append/compact stack-overflow boundary. The current 128-entry image has
passed a bounded powered bidirectional chat proof; a 128-message fill and
pressure qualification remain open.

The retained Stage 5 default/HIL pair links 946/962 compiler stack-size records,
has a 53,680-byte maximum frame, and leaves 175,056/174,256 bytes of usable
CPU0 stack. The default ELF is 13,648,888 bytes with SHA-256
`92e63b60a5f4b830ee55d958fcc446a6878036212904b8748519ae210ba3da58`;
its 868,656-byte package uses 803,120/6,291,456 application bytes and has
SHA-256
`c8da2af30e2d0ee24ca4b215151d1370b7e1d242991ebbeb024079a730693a3f`.
The runtime-measurement HIL ELF is 13,821,496 bytes with SHA-256
`7a3fad34699f910a2050468ada6461a0f33d16641ab5425a5c795a71238861ff`;
its 881,456-byte package uses 815,920 application bytes and has SHA-256
`12c6f31a7fb64485ad9220edca4ac38ba0a57867ad88ce60fa1a24ffc195d379`.
Both boards matched exact identity-bound HIL readbacks. In exactly one fresh
`3e:88` A-to-`3f:88` B trial, a 206-byte opportunistic LXMF carrier became an
exact 307-byte RNS packet with SHA-256
`060037041c91eb5999f89bf84845c19e65bf7fa680827cce9c51e8ecc5dbe0a6`
and reached `Delivered` on the first attempt. The receiver durably committed
message
`abdeec2e498f09c96a6fd56ec3558ca86c2598aaeacac81969b645de3b549dc3`,
advanced new/ready/released/ordinary-handoff exactly once with no replay or
ordering event, and confirmed one proof TX. Its release tag
`0x3dc4588d3a205429` matched the sender's delivered tag. The exact 2 MiB receiver
store readback has SHA-256
`c75ab2a01b3266fda1e07e0271c70bb29c06e32636d70d8a70d977b9e8b0e21e`
and contains exactly one record whose full-wire digest
`1c1839991401e01e15e3a3146cd3177a4fb7e5dbd52008fd119beaf091d377ba`
matches the generator. Neither checkpoint reported an allocation failure,
unexpected runtime error, RX/CAD/TX watchdog expiry, or correlation fault.
This narrowly power-confirms persistent continuous RX across the exact
two-frame RNode packet and durable LXMF proof release; it is not
direction-balanced, replay/remount, pressure, fault, range, or soak evidence.
The conservative stack policy still carries forward 57,700 bytes and a
4,020-byte margin after the maximum frame.
All powered records below remain bound to the source and Rete revisions they
name.

The journal's isolated powered storage HIL passed on
board E9:44 from clean source `7b47113`: one counted capture spans raw-flash
format, five appends, mutation-free retry/conflict checks, A1-to-B2 compaction,
a software reset and zero-write/zero-erase B2 replay. An independent raw dump
check confirms generation 2, all five committed records, the revision-4
`Delivered` state, the erased A manifest and the erased B tail. Evidence is at
`artifacts/storage-hil/20260716T211318Z-e944-7b47113`.

That powered run qualifies only the journal's isolated clean path and software-
reset replay. The permanent E290 graph has separate powered-smoke evidence.
Source `96e38aa` first established zero-mutation erased credential boot and
ordinary LoRa TX. Source `5f3f259` then passed an exact two-board 736,144-byte
upgrade/readback (SHA-256
`f422a8003762f9579ee0f4faf8c85cf78961327f7bb2c6db8c8878bc071d389b`), reported
8 MiB PSRAM, retained the clean identity/journal, and exposed
`credential_pairing_policy_resident=true` with
`credential_initialization=Eligible { media: ExactlyErased }`. The API/session/
bearer remained closed, both LoRa actors continued ordinary TX, and both post-
boot credential partitions retained the all-`0xff` SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.
The preceding boot-quarantined routed live-pairing image is 701,744 bytes with
SHA-256
`14d9fd6dd482c47baa9afd2fda6a5ba1d69f46785bf23ae29f6b9fe561e4b212`;
exact address-zero readbacks from both boards matched. Each board reattached
and served sequence-zero `initialization-required` after the hard reset induced
by its readback. Simultaneous 120-second no-button workflows remained responsive
through sequences 1102 and 1100, respectively, and exact reads confirmed both
credential partitions remained entirely erased. The application detaches and
scrubs USB at its earliest Rust entry before product initialization, then
canonically reattaches and waits for a clean enumeration reset. The preceding
ROM/bootloader interval is not covered by that quarantine.
The historical powered authenticated-node-foundation build is 718,688 bytes with SHA-256
`e20f6191cb2bfa78fbd7f3d588eb418913da3f1f89e3b80a4db0a28abaf414ea`.
Exact address-zero readbacks matched on both boards. Both returned sequence-zero
`initialization-required`; both 8 KiB credential partitions remained the exact
all-`0xff` SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`;
and both recovered sequence-zero service after the readback reset. This proves
the then-dormant handoff did not regress the existing USB bootstrap in that
bounded run. It does not exercise an authenticated handshake, request, or
reply, and it is not evidence for the subsequently composed minimal USB session
bearer. The powered API 1.1 source was 645,159 bytes text, 3,596 bytes initialized
data, 469,232 bytes BSS/reservations, and 1,117,987 bytes total by GNU size. Its
686,176-byte application is packaged as a 751,712-byte merged image with
SHA-256
`4285fcaa9df6a6f0314ed4735377ea986b0efcafafc2710ad7594489a49b4795`.
Exact address-zero readbacks matched on both E290s. The Active sender exposed
its public primary destination through `identity.summary`, durably accepted
submission 1, and kept one authenticated session open for sequential status
polls. The second permanent node decrypted the matching LoRa DATA and returned
a valid Reticulum proof; the sender reached `Delivered` in about 2.6 seconds.
After full sender USB re-enumeration, a fresh authenticated session returned the
same 131-byte packet length and encoded-byte SHA-256
`df937860f5225deb9d2350c6f3a46f33bd659ccbcb6b47267add47c9a287a4fe`.
This qualifies controlled peer RX/DATA/proof, successful pairing/authentication,
powered credential initialization, and the bounded permanent outbound owner
graph. It does not qualify application-level inbox consumption, physical power-
cut recovery, suspend/resume, stack high-water, heap pressure, or the full product.

The original post-audit API 1.2 qualification packages a 696,416-byte
application as a 761,952-byte merged image with SHA-256
`ba10b04408368c3f5cbcc91f5d514f454595a7812986764c1e95ef528cc71f03`;
both E290 address-zero readbacks matched exactly. Starting from an explicitly
erased sender inbox, a 383-byte DATA payload reached `Delivered`, published
item 1 through authenticated status/peek, and survived a hard reset. A later
valid packet also reached `Delivered`, incremented the boot-local drop count,
and left item 1 unchanged. The final exact 2 MiB partition dump had SHA-256
`f50dab680d46ef20cd875eff778296a3b92f9d7eef34684f29eedc10b468d724`:
the first 576 bytes were the canonical record and the complete remainder was
erased. The E290 runbook records that qualification artifact and its readbacks,
both delivered-packet hashes, the record, stored digest, payload, reset/remount, and
failed-attempt workflow evidence.

The 2026-07-19 cold-mount matrix reused that exact default image against four
deterministic 2 MiB fixtures. Their SHA-256 values were partial claim
`4b9e6dad1415850588c001b17053e893ab1316aaa1b6d584082170d049f871f0`,
complete precommit record with no commit marker
`a8a8d40f63a69c7e3df59f4af1960f241f464566a5ae9251c12209eb3334c66a`,
invalid digest
`bb24e892d435a0b6888cc16f8733f096015a36f0f19dcd8a22e0978602e55ad5`,
and a valid record bound to the other board
`dee21d3c72a914ac00627c49a119631999dc9e986ce18897b9a171254c79561b`.
Every cold boot advertised inbox availability/maximum as `0/0`; authenticated
status and peek both returned code 7, peek created no output, one direct DATA
exchange still reached `Delivered`, and the complete fixture remained
byte-for-byte unchanged. This qualifies read-only mount classification and
local inbox-service isolation for those four exact states. It is not evidence
of a physical cut occurring during a flash program operation.

The separate `rns-inbox-commit-fault-hil` image is 762,672 bytes, SHA-256
`e693afad19c2eac28d958f902c1b8148ae360a6b54abb14338195ef595515239`.
It acknowledged but suppressed only the third inbox program call. A 147-byte
packet with encoded SHA-256
`0084ad098f2109b390d7c4568ba4a2dcd5285ac40062e55c9709665b2aebc73a`
still reached `Delivered`; the fixed RAM evidence at `0x3fc8bf7c` reported
write calls/suppressed commits/expected commit mismatches/unexpected failures/
service disabled/dropped as `3/1/1/0/1/1`. USB-only re-enumeration left that
RAM evidence unchanged and the inbox API unavailable. The exact raw store,
SHA-256
`ad6d549f73681da7453870606fb34eeabad75b387f081176103562d84e5700c7`,
had all 544 bytes at offsets 0 through 543 programmed and non-`0xff`; every byte
from offset 544 through the end remained erased. The separate deterministic
interrupted-commit matrix qualifies cold-mount classification of this physical
state class; the contained rerun did not add a post-reset API observation.
Graph policy proves that this HIL changes only the product root feature and has
a dependency tail identical to the default graph. The HIL
module compiles only for feature-enabled host tests or feature-enabled Xtensa
builds; the default ELF contains neither the hook nor its evidence symbol.

After the matrix and HIL, the restored 2026-07-19 default image remained
761,952 bytes, with SHA-256
`d26587a2506408ec40cd42facb9bb87cc9c32e79c2afd2e1ab09f0e1268641cb`.
Both boards matched it exactly and booted with empty inboxes. These bounded
results do not qualify physical power cuts, mount/commit timing, RAM/PSRAM
high-water, watchdog/radio scheduling, LXMF, or a full mailbox.

Device-API dispatch is a separate portable integration boundary: the
target-safe authenticated adapter,
COBS framing, pre-authentication initialization-control codec, immutable
credential authority, qualification-session core, boot-lifetime job handoff,
and E290 `ProductStorageCoordinator` port implementations are compiled. The
permanent node now receives exact owners from a static depth-one handoff,
revalidates each grant against the currently publishable authority, and invokes
one operation-scoped owner implementing narrow submission, raw-inbox,
committed-LXMF-read, and device-owned LXMF-compose ports; rejection has zero
port I/O and no unauthenticated fallback. Schema-2 acceptance retains exact
authorization provenance. The current source graph serves that boundary
through its minimal single-flight USB bearer; capabilities, identity, durable
submission, sequential status, peer proof, and fresh post-re-enumeration status
are powered-qualified. An opt-in Wi-Fi SoftAP/raw-TCP bearer now reuses this
boundary with a separately bound session suite and a host-tested native client.
Its exact image has been safely flashed to one credentialed E290 with product
data preserved, but association, DHCP, authenticated exchange, reconnect, and
LoRa coexistence remain manually powered qualification. The mutually exclusive
BLE profile now passes exact flash/readback on both E290s and four authenticated
direct CoreBluetooth sessions, including three consecutive sessions on one
board. The bounded signed-iOS foreground proof additionally covers imported
credentials, exact-board authenticated BLE, bidirectional LXMF over LoRa,
automatic cold-launch reconnect, and corrected keyboard UX. The full Expo
mobile lifecycle matrix, background restoration, Android hardware,
cross-instance central ownership, pressure, and soak remain open.
The
resident credential runtime now also retains live pairing permits, proofs,
secrets, typed store candidates, and reconciliation owners through definite
outcomes. The bearer-neutral secret handoff preserves exact owners under
pressure and is now split between the USB and node tasks. Mutation-producing
live requests cross the node's journal-aware causal frontier and do not receive
success until the exact durable terminal outcome; powered activation has now
completed on the sender.
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
logical-packet radio call spanning both physical RNode frames. RX service is an
explicit scheduler choice rather than an implicit queued-TX priority rule, but
software scheduler yields leave one continuous-RX epoch armed across packets;
receive progress excludes TX until a terminal IRQ. Taking a job invalidates the
old RX epoch, which is explicitly quiesced before CAD/TX. The actor exposes
cancellation-safe completion-capacity readiness while retaining its exact
completion. Completion pressure, dropped CAD/TX/RX futures,
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
test. Its local submission/API edge has now originated controlled DATA in a
powered run and completed exact peer proof plus terminal status after USB re-
enumeration.
The DATA router, both permit-only services and permanent aggregate are now
connected to the dispatcher, sole-Rete owner, timed RNode RX and one E290 LoRa
actor in the first permanent build-verified target. Its exact authorized-frame
durability handoff and ADR 0005 active-owner fail-stop now pass cross-layer host
composition tests. Powered live external admission now passes its bounded
credential initialization, authenticated USB handshake/request/reply, durable
submission, physical LoRa peer-proof path, and bounded API 1.2 raw-RNS inbox
commit/readback/reset/drop-newest workflow, four exact cold-mount fault states,
one same-boot simulated commit-suppression path, and one exact opportunistic
LXMF durable-commit/proof exchange. API 1.4 now implements authenticated
committed-message list/read and source-free basic method-neutral send through
the same transport-neutral durable submission runtime. Broader lifecycle work,
physical-interruption qualification, additional LXMF carriers/directions, and
a production client remain—not credential-store boot composition, the
frozen pairing/session cryptography, another semantic authority, durability
policy, partition, or capacity decision. The feature-free
ADR 0009 admission policy, resident
initialization owner, and sole-owner physical drive are compiled only into the
permanent E290 graph. The pre-authentication initialization and live-pairing
codecs, debounced physical presence, sole USB byte owner, reset-epoch guard,
bounded command/reply handoffs, static authenticated handoff, node-side
current-authority dispatch, and the minimal credential-backed USB session state
machine are composed. ADR 0012 now fixes the application-event owner, and ADR
0013 plus the Python-derived LXMF 1.0.1 corpus establish the allocation-free,
destination-bound wire/signature/stamp validation boundary. The separate
`reticulum-lxmf-ingress` adapter now borrows an explicitly owned
`lxmf.delivery` application event, resolves its announced source identity by
value, and classifies opportunistic or bound direct Link DATA without copying
or consuming it. Non-`NONE` Link DATA remains unrelated. Context-`NONE` Link
DATA is admitted only on a responder-side Link bound to the mounted local
service. It carries ADR 0016's opaque Rete-derived Link destination binding,
and must contain the same destination in its complete LXMF wire. The subsequent
product durable-admission step separately requires the application-event owner
to carry the exact Link-destined packet proof covering the received RNS packet
hash before store I/O. Initiator/backchannel direct receive remains unsupported.
Resource completion remains explicitly deferred. ADR 0014 adds the separate dependency-free
semantic model, variable-extent append-only NOR store, and durable-ingress
lease adapter. Exact normalized LXMF bytes stream from the retained event into
the store without a message-sized copy, and only a verified durable receipt can
acknowledge that event. Replays, alternate valid stamps, and forced
same-ID/different-material conflicts remain distinct. The permanent E290 graph
allocates the store's 512-slot opaque index, sixteen delayed-proof slots, and
the retry/fault/proof-holder state in explicitly validated PSRAM, while the
sixteen application-event slots remain static internal RAM. It mounts a
dedicated 2 MiB `lxmf_store` partition at boot. On mount success it derives and
registers `lxmf.delivery`, admits signed opportunistic destination DATA and
responder-side direct-packet Link DATA, and selects Rete's per-destination
retained-proof policy. Both carrier classes require their exact proof; the
primary destination accepts no inbound Links. Discovery emits a separately
signed `lxmf.delivery` announce with canonical LXMF 1.0.1 `[nil, nil, []]`
application data. Valid remote announcements also feed a volatile,
generation-ordered 32-peer application projection with at most 256
authenticated application-data bytes per peer. An authenticated API 1.5 cursor
read exposes destination, identity fingerprint, hop/interface hints, age, and
optional signal observations without becoming route authority. The Rust chat
runtime bounds and decodes this projection before the Expo **Nearby** picker
offers one-tap durable contact creation. The current LoRa actor reports its
interface identity but does not invent unavailable RSSI/SNR. An unmounted store or a clean fault
that disables the LXMF service suppresses that secondary announce while the
primary destination continues to announce. An ambiguous pending
`StoreFaultHold` retains its exact owner but does not currently suppress the
secondary announce. Local scheduling emits at most one destination per event:
primary first, `lxmf.delivery` eight seconds later, then two short retry cycles
whose first primary deadline is `13 + (primary-prefix mod 23)` seconds after the
preceding pair, followed by a 30-minute steady cadence. The known E290 identities
therefore place their first post-pair primary retries at 26 and 43 seconds from
boot. Those phases keep their explicit events and Rete's five-second native
retransmissions at least three seconds apart. Protocol queue rejection retains
the selected destination behind a one-second retry deadline instead of consuming
one of the bootstrap attempts. This replaces the historical back-to-back batches: in that powered run B
processed three distinct A announces but still returned `no-path` for A's LXMF
destination because transport-mode relay of the first announce occupied the
half-duplex radio while the second was sent. The first independently scheduled
replacement image had exact readbacks on both boards, but both USB devices
disappeared before its required post-flash pre-submit checkpoint, so that
historical attempt made no durable-LXMF claim.
The portable durable-ingress owner reserves one of sixteen fixed delayed-proof
slots before store I/O for every admitted opportunistic or direct event, returns
the exact combined lease on failure, and makes that proof ready only after a new
commit or a fresh
retransmission recognized as `AlreadyDurable`; the permanent task drains it
only through the ordinary transport-neutral supervisor. Those sixteen
event/proof slots are this E290
profile's tunable volatile-concurrency bound, not a protocol or storage ceiling.
Native Resource admission remains disabled until a bounded segmented/streaming
owner exists.
The project wrapper now answers a path request for a local secondary destination
and suppresses rebroadcast of that locally consumed request. This is a temporary
one-interface qualification path pending an owning Rete implementation with
multi-interface forwarding state. Rete's sub-second retransmission jitter also currently
rounds to zero; the product's identity-phased bootstrap retries mitigate that
collision without changing the pinned Rete behavior.
The current composition now has the single powered remote durable-delivery
confirmation summarized above for opportunistic DATA. On the receiver, retained
LXMF proof ownership
is intercepted before ordinary RNS ingress metadata, so `RPTE` generated-proof
count/tag correctly remains zero; the evidence instead joins the `LXTE` release
tag, one confirmed proof-TX delta, and the sender's matching delivered tag. The
raw-RNS inbox remains separate qualification evidence rather than a product
mailbox. Native RNS Resource ingress stays disabled until its allocation and
streaming-storage boundary is bounded.
Responder-side direct Link receive has the same source-level durable proof
contract, and the
[forced-direct two-board record](docs/e290-direct-link-powered-proof.md)
qualifies one fresh-Link/new-commit physical success path. The later
[same-Link reuse and direct-replay record](docs/e290-same-link-reuse-replay-powered-proof.md)
adds two direct-required deliveries with one LXMF message ID, distinct packet
hashes, and one receiver row; exact same-handle reuse and `AlreadyDurable`
classification remain source-qualified because the client API exposes neither.
The broader fault/pressure matrix remains open. The
[current-image stale-Link recovery record](docs/e290-stale-link-recovery-powered-proof.md)
separately qualifies durability-first retirement after a peer reboot and
successful delivery of a later sequential submission over a fresh Link.
The node-side routing
boundary remains interface-neutral so additional Reticulum links can be added
later through adapters without rewriting the LoRa actor or protocol owner; no
second transport is required to qualify the first LoRa vertical slice.

## Read first

- [Architecture](docs/firmware-architecture.md)
- [Vision Master E290 primary target](docs/heltec-vision-master-e290.md)
- [Permanent LoRa-first E290 node](docs/e290-node.md)
- [LXMF chat alpha CLI](crates/lxmf-chat-cli/README.md)
- [LXMF host appliance alpha](crates/lxmf-chat-service/README.md)
- [Expo universal appliance client](clients/appliance/README.md)
- [E290 LXMF chat-alpha powered proof](docs/e290-lxmf-chat-alpha-proof.md)
- [E290 LXMF host-appliance alpha proof](docs/e290-lxmf-appliance-alpha-proof.md)
- [E290 Expo managed first-run proof](docs/e290-expo-appliance-first-run-proof.md)
- [E290 Expo iOS BLE-to-LoRa powered proof](docs/e290-expo-ios-ble-lora-proof.md)
- [E290 Reticulum-native Nearby powered proof](docs/e290-reticulum-nearby-powered-proof.md)
- [E290 outbound direct-Link powered proof](docs/e290-direct-link-powered-proof.md)
- [E290 stale-Link recovery powered proof](docs/e290-stale-link-recovery-powered-proof.md)
- [Expo native Rust bridge proof](docs/expo-native-rust-bridge-proof.md)
- [Usable-firmware POC limits and known defects](docs/poc-known-defects.md)
- [Phase-0 scaffold decision](docs/adr/0001-phase-0-scaffold.md)
- [Rete provisional-foundation decision](docs/adr/0002-rete-provisional-foundation.md)
- [LoRa-first heterogeneous-interface decision](docs/adr/0003-lora-first-interface-fabric.md)
- [Sole-flash coordinator decision](docs/adr/0004-sole-flash-coordinator.md)
- [Active DATA durability fail-stop decision](docs/adr/0005-active-data-durability-fail-stop.md)
- [Authenticated local device-API bearer decision](docs/adr/0006-authenticated-local-api-bearer.md)
- [Device-API credential authority decision](docs/adr/0007-device-api-credential-authority.md)
- [Durable authorization provenance decision](docs/adr/0008-durable-authorization-provenance.md)
- [Device-API credential store and pairing decision](docs/adr/0009-device-api-credential-store-and-pairing.md)
- [Wired developer pairing protocol](docs/adr/0010-device-api-live-pairing-protocol.md)
- [Durable raw-RNS inbox qualification](docs/adr/0011-durable-rns-inbox-qualification.md)
- [Application-event ownership and bounded RNS Resource admission](docs/adr/0012-application-event-and-resource-ownership.md)
- [Bounded LXMF wire and service ownership boundary](docs/adr/0013-bounded-lxmf-wire-boundary.md)
- [Durable LXMF message ownership](docs/adr/0014-durable-lxmf-message-ownership.md)
- [Universal Expo client and generated TypeScript boundary](docs/adr/0015-universal-expo-client-and-generated-bindings.md)
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
cargo run --locked -p xtask -- graph-policy
cargo test --locked -p reticulum-lxmf-model
cargo test --locked -p reticulum-lxmf-store
cargo test --locked -p reticulum-lxmf-durable-ingress
cargo test --locked -p reticulum-lxmf-ingress
cargo test --locked -p reticulum-lxmf-wire
cargo test --locked -p reticulum-device-api --features experimental-rns-data
cargo test --locked -p reticulum-device-api-adapter \
  --features experimental-rns-data
cargo test --locked -p reticulum-device-api --features experimental-rns-inbox
cargo test --locked -p reticulum-device-api-adapter \
  --features experimental-rns-inbox
cargo test --locked -p reticulum-device-api --all-features
cargo test --locked -p reticulum-device-api-adapter --all-features
python3 interop/python/generate_device_api_session_vectors.py --check
python3 interop/python/test_device_api_session_vectors.py
python3 interop/python/generate_device_api_pairing_vectors.py --check
python3 interop/python/test_device_api_pairing_vectors.py
LXMF_PYTHON="$(mktemp -d)"
python3 -m pip install --target "$LXMF_PYTHON" \
  -r interop/python/requirements-lxmf-1.0.1.txt
PYTHONPATH="$LXMF_PYTHON" \
  python3 interop/python/generate_lxmf_1_0_1_vectors.py --check
PYTHONPATH="$LXMF_PYTHON" \
  python3 interop/python/test_lxmf_1_0_1_vectors.py
cargo run --locked -p reticulum-conformance-rete
cargo check --locked \
  -p reticulum-rns-conformance \
  -p reticulum-rns-rete \
  -p reticulum-rns-rete-rx \
  -p reticulum-rns-inbox-store \
  -p reticulum-device-api \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-pairing-policy \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  -p reticulum-lxmf-durable-ingress \
  -p reticulum-lxmf-ingress \
  -p reticulum-lxmf-model \
  -p reticulum-lxmf-store \
  -p reticulum-lxmf-wire \
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
  -p reticulum-device-api-pairing-policy \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  -p reticulum-lxmf-durable-ingress \
  -p reticulum-lxmf-ingress \
  -p reticulum-lxmf-model \
  -p reticulum-lxmf-store \
  -p reticulum-lxmf-wire \
  -p reticulum-rns-inbox-store \
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
LoRa-first three-task node/LoRa/USB product composition, its fixed capacities, 16 MiB partition
layout, durable identity/announce ordering, build gates, API 1.1 outbound proof,
API 1.2 raw-RNS inbox evidence, the implemented API 1.4 LXMF client slice, and
remaining storage/client blockers. The
powered record now includes controlled permanent-image peer DATA/proof, bounded
inbox commit/readback/hard-reset/drop-newest behavior, the four-state exact
cold-mount matrix, the feature-gated same-boot commit-suppression HIL, and one
fresh exact 307-byte opportunistic LXMF durable-commit/proof exchange over the
persistent continuous-RX path. The separate API 1.4 POC adds same-boot,
direction-balanced source-free send, proof, peer commit, list, and exact read.
Its final audited image additionally preserved both terminal sender records and
both exact receiver wires across a physical CPU reset. Neither result
substitutes for electrical power-cut persistence, sustained/multi-hop traffic,
Resource LXMF, initiator/backchannel direct receive, production client-store
semantics, or full-product qualification. The later
[forced-direct powered record](docs/e290-direct-link-powered-proof.md)
qualifies one fresh initiator/new receiver-commit one-packet success path. The
[current-image recovery record](docs/e290-stale-link-recovery-powered-proof.md)
then qualifies a receiver reboot, durable sender timeout without receiver
commit, exact stale-Link retirement, and successful fresh-Link delivery by the
next sequential submission. The
[same-Link reuse and direct-replay record](docs/e290-same-link-reuse-replay-powered-proof.md)
adds the bounded two-delivery/one-message/one-row outcome and documents which
internal values remain source-qualified.

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
