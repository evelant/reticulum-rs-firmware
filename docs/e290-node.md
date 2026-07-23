# Permanent Vision Master E290 node image

**Status:** the first permanent, LoRa-first image is implemented. Its release
record below captures the host-library, host-client, portable-target, ESP32-S3,
strict review, graph, image-size, and same-image readback gates. Its third task
owns a USB Serial/JTAG pre-authentication initialization and live-pairing bearer,
one shared exact-next sequence space, debounced GPIO21 physical presence, an
interrupt-linearized reset-epoch guard, and an application-entry USB boot
quarantine. Powered work has completed the first outbound API 1.1 path, the
bounded API 1.2 raw-RNS inbox qualification path, and a separate opt-in
runtime-measurement slice on the two E290s. API 1.4 additionally
implements authenticated committed-LXMF list/read and source-free basic LXMF
send over that same USB bearer. Its historical initial failure before USB
enumeration was confirmed as a cumulative startup mount-stack overflow; direct
caller-destination placement of the upper runtime/actor chain fixed that
boundary. The [2026-07-22 two-board powered POC](e290-api14-lxmf-poc.md) then completed exact
A-to-B and B-to-A sends, Reticulum delivery proofs, peer commits, authenticated
enumeration, and digest-verified normalized-wire reads. The final audited image
also matched exact writes/readbacks on both boards and, after physical CPU
reset, preserved both terminal submissions and both exact receiver wires. The
initially flashed 128-entry successor later failed the expanded final
linked-path stack gate and was not qualified. Current source adds an
actor-owned PSRAM replay scratch index and gates mount, append, and compaction,
preventing their capacity-sized replay state from overflowing the CPU stack.
The [persistent chat-alpha powered proof](e290-lxmf-chat-alpha-proof.md) binds
the current 128-entry image to both boards and completes terminal A-to-B and
B-to-A SQLite-client exchanges after fixing a retained-frame scheduler
inversion found by the first run. Its deliberate storage, carrier, security,
and client limits are
tracked in [the usable-firmware POC defect list](poc-known-defects.md). In the
historical API 1.1 image, MAC `ac:a7:04:e1:3e:88` retained its button-confirmed
empty-store initialization, durable Active generation 3, and host credential;
MAC `ac:a7:04:e1:3f:88` owned a separate durable node identity. Matching exact
address-zero readbacks preceded authenticated identity, durable submission,
physical LoRa DATA/proof, terminal `Delivered`, and a fresh post-re-enumeration
status read. The later API 1.2 image retained paired generation 5 on `3e:88` and
generation 3 on `3f:88`. A maximum 383-byte DATA item sent from `3f:88` reached
`Delivered`, committed to `3e:88`'s exact one-entry `message_store`, survived a
hard reboot, and remained observable through authenticated status and peek. A
second valid DATA item reached `Delivered` but left item 1 unchanged and raised
the boot-local drop counter from zero to one. This closes the bounded commit,
exact readback, hard-reset survival, and drop-newest proof. A later four-case
cold-mount matrix proved fail-closed isolation for partial claim, missing
commit, invalid digest, and foreign-board binding media on the exact ordinary
image. One opt-in image separately suppressed the terminal commit write and
its triggering peer DATA/proof exchange completed first. Debugger-bound RAM,
fresh authenticated API sessions, and raw flash then proved same-boot product
quarantine. The later measurement image observed one maximum-payload durable
inbound commit on each board, zero RX/CAD/TX watchdog counters, and bounded
heap, stack-watermark, boot, storage and scheduler values. Only the first sender
reached `Delivered`; the reverse receiver committed the exact payload while its
sender ended in `delivery-timeout`. Neither fault fixture is an electrical power
cut. The measurement run is instrumented, bounded single-commit evidence, not
sustained or production-image target-bounds qualification, LXMF, or a full
mailbox. Earlier controls returned
`initialization-required`, enforced GPIO21 for initialization and live Begin,
rejected stale sequence zero, and restored a fresh epoch after full host
re-enumeration. Both preceding boot-quarantined image readbacks matched exactly,
both boards served sequence zero again after the induced hard reset, and
120-second no-button workflows left both credential partitions erased. Exact
Pending and Abort readbacks, mutation ambiguity/fault cuts, and broader powered
lifecycle qualification remain open. Source `5f3f259` passed the earlier
bounded powered upgrade smoke on both `HT-RA62-HF` boards: exact same-image
readback, resident pairing-policy and erased-initialization eligibility, zero
credential mutation, journal/LoRa/interface startup, and ordinary one-frame TX.
USB suspend/resume, controlled power cuts, the ROM/bootloader interval before
the earliest Rust entrypoint, and full product qualification remain open.
The permanent source graph now additionally composes the feature-free
authenticated session and handoff crates, a static depth-one request/reply
handoff, node-side current-authority dispatch, and the first deliberately
minimal authenticated USB session bearer. It accepts one active session and one
request at a time; a canonical ClientHello replaces an idle session with a fresh
epoch on the same USB connection but never displaces request/reply owners. A
session fault remains terminal until reset or re-enumeration. Resumption,
protocol retries, close records, encryption, rate limiting/attempt policy, and
concurrent requests are deferred. Credential selection, admission handoff, and
node dispatch are bearer-neutral. The opt-in BLE profile now reuses that
ownership boundary under suite 3 while keeping the application connection limit
at one; Wi-Fi remains to be powered-qualified. Powered evidence now qualifies
authenticated capabilities and identity reads, sequential request/response
flights in one session, durable submission, LoRa DATA delivery, peer decrypt/
proof, terminal projection, API 1.2 durable raw-RNS status/peek before and after
a hard reset, one drop-newest observation, four bounded cold-mount quarantine
cases, and one same-boot missing-commit quarantine. The current continuous-RX
HIL additionally qualifies one exact 307-byte A-to-B opportunistic LXMF DATA
packet, a new durable receiver-store commit before retained-proof release,
first-attempt `Delivered`, and exact store readback. The earlier fault fixtures
each include only one direct DATA/proof exchange; they do not claim sustained
forwarding,
multi-hop routing, LXMF, or general application-level message consumption,
session resumption, Wi-Fi bearer binding, or broader BLE lifecycle behavior.

The current source composition pins Rete commit
`90570cafc812b3025011cb690ec74a27f287cb3f`, with designated durable tag
`firmware-pin-90570ca`. The older 2026-07-20 two-board measurements below
predate that pin, while the later pre-PSRAM one-board checkpoint uses it. The
Stage 5 PSRAM boot checkpoint is the first powered evidence for the post-offload
Stage 5 source and placement, and is scoped only to placement, boot, and one
authenticated API read. The pin removes implicit
interface-zero/broadcast fallbacks, adds exact path/reverse/Link routing and
authenticated fail-closed LRPROOF handling, and makes covered H2 relay/reverse
admission transactional with typed failures. It also adds precise
microsecond/binary64 LRRTT timing, dispatch confirmation, Active/Stale updates,
and authenticated-malformed teardown. Those changes do not retroactively
qualify a historical image or hardware run.

The current composition also replaces `RADIO_READY`/`LORA_ONLINE`
product globals with the transport-neutral interface fabric's queue-bound,
generation-checked lifecycle exchange. The LoRa actor reports `Ready` after
radio construction and waits for the node-owned registry acknowledgement
before service. Registration is offline-only and node-owned policy is
disable-only, so no product caller can bypass that Ready boundary. Every
terminal actor path resumes or retries its exact exchange until it observes
authoritative `Offline` before its permanent owner-retention loop. Crossed or
stale reports cannot change the
registry; acknowledgement pressure leaves the observed request unapplied.
Offline excludes fresh routing while preserving legitimate completion and
ingress owners already accepted under the same lease. The supervisor services
this as a pre-routing gate; fairness applies among actors inside the lifecycle
router, not between lifecycle and ordinary routing lanes. Two-slot host
coverage proves the graceful case: after the first actor goes Offline and
legitimately returns its in-flight completion, serialized DATA fan-out and
ingress continue through the healthy actor. It does not prove terminal
failover. An E290 terminal path
retains any exposed or otherwise ambiguous owner, so that attempt cannot
automatically advance; only fresh attempts exclude the failed interface.
Draining or revoking provably unstarted work still queued for that actor needs
a future ownership protocol. Protocol-confirmation failure now offlines the
interface returned by the routed transition rather than assuming LoRa. The
default lifecycle image was flashed to `3e:88`, matched an exact address-zero
readback, and returned an authenticated `identity-summary`. That proves one
boot and API exchange for that lifecycle source, not the retained Stage 5 or the
two-board lifecycle/RF behavior;
`3f:88` did not enumerate for this run.

The preceding `14c7b49`
build-only default E290 release links with text/data/BSS of
670,407/3,676/469,152 bytes and a 12,084,612-byte ELF, SHA-256
`d0b457165f8ec80a677f963e0608a5b9510970fde23458567b4f31e3b319822a`.
Its explicit 16 MiB package is a 776,464-byte merged image, uses
710,928/6,291,456 application bytes (11.30%), and has SHA-256
`7b11c6f6a3c039d46ab0117fd362920aaa40145e7f27cbc6fa0a8a84a7ab3571`.
This is build-only evidence for the preceding pin: the image has no flashed
readback or powered proof. The pre-PSRAM application-event ownership release
links with text/data/BSS of 684,167/3,676/469,152 bytes (1,156,995 bytes total
by GNU size). Its 12,345,320-byte ELF has SHA-256
`ebb34e7176a8e61b6969ebf99d7dac97c6e674ef5e583bbf931a34e8b6e970a2`.
Its explicit 16 MiB package is a 789,504-byte merged image, uses
723,968/6,291,456 application bytes (11.51%), and has SHA-256
`1796f161c480d0348e3d47fd8f3cda5fda5b51aa38ad6024aaad04c8ba1751ce`.
That merged image matched an exact readback on `3e:88`, where authenticated
`identity-summary` succeeded. A target-scoped rebuild of the corresponding
runtime-measurement HIL then matched another exact `3e:88` readback and
produced the bounded authenticated checkpoint recorded below. The board was
subsequently restored to an exact-readback rebuilt default image and again
served `identity-summary`. The unavailable `3f:88` prevented that
two-board lifecycle/RF run.
Every powered result below remains bound to its recorded historical source and
Rete revision.

For locally owned Links, a responder binds the LINKREQUEST ingress interface,
while an initiator remains unbound until a valid LRPROOF supplies the
authenticated ingress interface. Active application and maintenance output
then carries native `BoundInterface` and resolves to that exact physical
interface. Only an initial LINKREQUEST with no learned path interface may
broadcast. Wrong-interface Link DATA and `RESOURCE_PRF` are rejected before
deduplication, preserving a later correct-interface copy. This is currently an
interface-slot binding: a Tokio shared `Hub` still broadcasts asynchronous
owned-Link output to siblings until Link state carries endpoint-aware client
identity. Python keepalive wire/role parity is now host-qualified: exact
unencrypted 20-byte `0xff` requests originate only from the initiator after both
a full inbound-silence interval and a full prior-probe interval, and only the
responder returns `0xfe`. Both are consumed internally, repeated valid frames
bypass dedup only after the bound interface is accepted, and automatic output
preflights and retains that exact route before committing its probe timer. Stale
begins after two intervals and keeps a `4 * RTT + 5 seconds` revival window from
the actual transition/final probe (five seconds when RTT is zero); valid bound
Link traffic revives it. An
initiator now snapshots a known path's hops when the pending Link is created,
or uses the `PATHFINDER_M = 128` wildcard when no path is known. LRPROOF hop
mismatches fail before deduplication or state mutation, and a responder records
the post-ingress hop only from authenticated, decrypted LRRTT. Pending-handshake
payload parity is now covered: Rete emits canonical MessagePack float64,
accepts Python u-msgpack numeric scalars and first-object/trailing-byte
behavior, and selects the greater local or peer RTT with Python ordering.
Rete retains an immutable request anchor, uses microsecond
`MonotonicInstant`/`MonotonicDuration`, and stores RTT as binary64. An opaque,
non-repeating eight-byte token accepts only the first successful interface
confirmation. Initiator LINKREQUEST uses the confirmed egress interval start;
responder LRPROOF uses its completion. The firmware confirms at generic
ordinary-router/interface acceptance, not physical LoRa RF `TxDone`.

Fresh authenticated LRRTT is processed in `Handshake`, `Active`, and `Stale`.
Only initial activation emits `LinkEstablished`; repeats/reactivation emit
`LinkRttUpdated`, refresh RTT/activation/hop/keepalive state, and do not
duplicate establishment statistics. Exact raw replay remains deduplicated.
Authenticated malformed LRRTT tears down all three states, with
`links_failed` incremented only for Handshake. Zero RTT retains the 5-second
keepalive/10-second stale floors; nonzero RTT uses `4 * RTT + 5 seconds` stale
grace. Rete deliberately authenticates before liveness mutation, so corrupt
stale LRRTT does not revive a Link as it does under released Python's ordering.
It uses one pre-decrypt ingress sample for one bounded synchronous handler,
where Python takes three internal samples. The firmware adapter uses precise
`*_at` paths and confirms at the transport-neutral ordinary router; upstream
Tokio/Embassy runners remain coarse/unconfirmed.
Shared-Hub endpoint/reincarnation identity and automatic timeout `LINKCLOSE`
emission also remain open. Channel sends now preflight MDU,
pending-window and receipt capacity; retries preflight the exact Link route and
atomically replace the sole live receipt with the fresh ciphertext hash before
retry/window/timestamp state commits. Obsolete proofs fail closed, full-table
replacement succeeds in place, and Link removal reclaims channel receipts.
Receipt capacity below an adaptive channel window remains typed backpressure
and a sizing/throughput policy. This source result adds no powered keepalive or
channel-retry evidence.

No issue or pull
request was opened for this newer fork-local work, and any future upstream
issue or contribution requires direct user approval.

This target is the first executable product composition, not another HIL
fixture. It starts a transport-mode Rete node, one E290 LoRa actor, receive and
transmit scheduling, routed DATA and ordinary-action ownership, periodic
protocol maintenance, and local announces. It now also owns a power-loss-safe
device identity and restart-safe announce-emission clock, validates and safely
first-provisions the exact node-journal partition, and strictly completes a
submission-runtime recovery gate before constructing node or radio service. It
then transfers the sole flash backend and mounted runtime into a resident
operation-scoped storage coordinator that the node task schedules throughout
the firmware lifetime. Current source also strictly mounts ADR 0011's exact
2 MiB `message_store`, projects transport-neutral decrypted DATA into its
one-entry commit-last raw-RNS store, retains one candidate across cross-store
deferral, and exposes authenticated status/peek through a narrow semantic port.
A mount or admission fault disables only that inbox service.
The four cold-mount fixtures establish one direct peer DATA/decrypt/proof
exchange after boot quarantine; the same-boot HIL's triggering exchange
precedes the missing-commit quarantine. Neither establishes sustained or
multi-hop routing.

Current source separately mount-gates a derived `lxmf.delivery` destination
backed by ADR 0014's 2 MiB append-only store and 512-slot PSRAM index. It admits
only opportunistic destination DATA, resolves the sender through the
transport-neutral node identity cache, and applies Rete's per-destination
`Retain` proof policy: a proof becomes eligible for the ordinary supervisor
only after a new commit or a fresh retransmission is recognized as
`AlreadyDurable`. No delayed-proof state survives reboot. Signatures remain
mandatory. The initial `StampPolicy::NotRequired` profile allows an absent stamp
and preserves/parses a supplied stamp, but does not yet verify ticket trust or
proof of work. Sixteen static internal-RAM application-event slots keep ingress
admission bounded. Sixteen delayed-proof slots plus the bounded retry set,
authority-fault set, non-cloneable packet-action holder, and fault flags are
allocated explicitly in validated PSRAM for the boot lifetime with no internal-
RAM fallback. These slot counts bound this E290 profile's volatile concurrency;
they are not a protocol, store-capacity, or full-feature ceiling and may be
raised after measured E290 qualification.
The first profile deliberately has no age or attempt expiry for
`AdmissionDeferred`: an unknown source identity retains its exact slot while an
announce may make verification possible. This keeps memory bounded but means
sixteen never-resolved sources can occupy all application-event slots until
reboot. A path-request/identity-retention strategy plus an explicit expiry or
attempt policy is required before hostile or sustained deployment.
When the event pool is actually full and a ready proof cannot enter the ordinary
path, pressure relief discards only the oldest non-pending LXMF retry; exact
store-reconciliation and store-fault owners are never selected. Clean collision
or capacity outcomes discard only that candidate without disabling replay. A
clean invariant or pre-pending media fault retains the exact event and fail-stops
only LXMF admission. A post-pending mutation fault also blocks all other flash
mutations until reset/remount because its exact ambiguous store owner must remain
exclusive; routing and nonmutating consumers continue. Local Link
admission is disabled for both the primary and LXMF destinations until a bounded
Link/Resource owner exists. A mounted service emits a separately signed
`lxmf.delivery` discovery announce with canonical LXMF 1.0.1 `[nil, nil, []]`
application data unless a clean fault has disabled that service. The scheduler
attempts at most one local destination per event: primary first, LXMF eight
seconds later, two short retry cycles, then a 30-minute steady cadence. The first
retry is identity-phased by `13 + (u32_le(primary[0..4]) mod 23)` seconds after
the initial pair; the known A and B identities therefore retry their primaries
at 26 and 43 seconds from boot. Explicit events and Rete's five-second native
retransmissions remain at least three seconds apart for that pair. A queue or
native admission rejection retains the same scheduled destination and retries
it one second later without consuming bootstrap budget. An ambiguous pending
`StoreFaultHold` retains its exact owner but does not currently suppress
discovery. Direct/Resource delivery, propagation, direct/Resource/Link outbound
LXMF, responsive discovery beyond the current local path-response wrapper,
ticket/PoW requirements, and reclamation remain deferred. Basic opportunistic
outbound LXMF and its USB client API are included in the separate
[API 1.4 bidirectional powered record](e290-api14-lxmf-poc.md). The historical
record below remains the earlier HIL-only, one-way exact
opportunistic new-commit-before-proof result. Neither result is general LXMF
interoperability qualification.

An optional journal mount/recovery failure occurs before any
durability-gated DATA owner can exist; it disables local durable submission
service while the LoRa node still starts in route-only mode. The exact
authorized-frame request/durable-echo handoff is source-composed and now passes
cross-layer host qualification. The current USB-usable submission profile
retains 128 accepted records in PSRAM and has no terminal reclamation; a 129th
novel request is rejected while exact replay remains available. This is a
bounded profile below the journal's separate 162-acceptance lifetime ceiling,
not a product-capacity commitment. The earlier 16-entry profile and its proof
artifacts remain historical and do not qualify this larger profile on hardware.
Portable API framing, a featureless pre-authentication
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
logical dispatch synchronously through one operation-scoped owner implementing
narrow submission, raw-inbox, committed-LXMF-read, and LXMF-compose ports that
cannot borrow credential records. Revoked or missing
credentials return only
the generic authentication-required response with zero port I/O and no
unauthenticated fallback. Source-level external admission now reaches that lane
through the single-flight USB bearer, and the powered happy path above exercises
it after durable activation and reboot. Exact Pending/Abort readbacks,
activation ambiguity, failure cuts, busy-owner non-displacement, and richer
session fault/recovery behavior remain open. Repeated idle-session replacement
across separate authenticated client processes is powered-qualified. ADR 0005's
active-owner policy is implemented: a
permanent fault
with an unresolved frame enters interface-local `ActiveOwnerFailStopped`, takes
the same LoRa lease offline without changing its generation, retains the exact
frame/completion/ticket, and permits no fresh LoRa work for the rest of the boot.
Device configuration, propagated/direct/Link LXMF, durable delete/reclaim and
migration policy, local NomadNet clients, and production-ready host-facing
USB/BLE/Wi-Fi services remain visible product work. API 1.4 and the host CLI now
provide a basic USB LXMF send/list/read POC; they are not the final client
surface. The one-entry raw-RNS qualification record remains separate from the
dedicated opportunistic LXMF receive store.

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
    MountedInboxStore + operation-scoped BoundInboxStore views
    one deferred fixed candidate + drop-newest admission
    MountedLxmfStore + bounded committed-wire reads
    device-owned basic-LXMF composition + exact durable acceptance
  depth-one pre-auth control command/reply handoff
  depth-one bearer-neutral live-pairing command/reply handoff
  authenticated API node lane
    current-authority revalidation + synchronous logical dispatch
    one short-lived combined semantic port owner, selected operation only
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
    one active session; idle ClientHello replacement creates a fresh epoch
    one request in flight; replacement never displaces request/reply owners
    fault terminal until USB reset/re-enumeration
    no resumption, retries, close records, encryption, rate/attempt policy,
      or concurrency in this first profile
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

At the current Rete pin, native `SourceInterface`,
`ExactInterface(interface)` and `AllExceptSource` outcomes resolve to project
`Only(source)`, `Only(interface)` and `AllExcept(source)` actions before the
asynchronous queue. Exact delivery to the ingress slot remains valid for a
shared-medium LoRa relay rather than being suppressed as an echo. Path-selected
DATA has no interface-zero fallback; reverse proofs are one-shot and accepted
only from the stored outbound slot; Link DATA/PROOF direction and hops are
checked; and LRPROOF must arrive from the responder side at the stored hop count
and pass identity reconstruction and signature validation before routing or
lifetime refresh. A targeted HEADER_2 LRPROOF is normalized into those checks
instead of bypassing them through generic Link handling. Owned H2 local DATA,
LINKREQUEST, Link and proof/receipt traffic reaches normal dispatch. Transported
H2 DATA/SINGLE and LINKREQUEST/SINGLE require an exact path and admit reverse
or relay-Link state transactionally before forwarding; owned/relay Link full,
reverse full and reverse-key conflict are typed product drops. Foreign
non-ANNOUNCE H2 traffic is filtered before native mutation, while H2 ANNOUNCE
remains eligible for normal validation, and relay-Link occupancy is separately
observable. Arbitrary remote H1 LINKREQUEST remains disabled pending explicit
interface roles; H1 DATA retains a guarded reverse-capacity/conflict shim for
that same boundary. A Rete snapshot currently restores identities only. Saved
paths are deliberately inactive after reboot until the node relearns them
because a transient `u8` slot has no stable interface identity, generation or
rebind.

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

The LoRa task keeps the modem in `RxMode::Continuous` across physical frames.
The former 248-symbol hardware search window is now a 253,952-us software-only
scheduler cadence: when that timer wins, the actor may inspect queued TX
without issuing standby or another `SetRx`. Once preamble, sync-word, or valid-
header progress is observed, the scheduler timer no longer competes. A separate
maximum-frame-airtime-plus-margin deadline then waits for a terminal receive
IRQ; if a false preamble leaves the SX1262 latched, expiry reissues continuous
`SetRx` and returns a recoverable invalid-frame outcome before TX can be
considered. A partial RNode packet separately retains receive priority until
completion or the profile-derived fragment deadline.

Completed bytes move into an exact fabric-owned ingress buffer. If the ingress
queue is full, the sealed packet is retained unchanged; if no reusable buffer
exists, the task skips RX and gives TX one turn. Accepting a ticketed TX owner
invalidates the current receive epoch. Before CAD or TX, the radio enters
standby, disables receive IRQ routing, and clears pending IRQ flags; the
dispatcher then drives backoff, CAD, resource permission, one logical one/two-
frame transmit, and exact completion return before starting a fresh continuous-
RX epoch.

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
shutdown occurred. They report Offline through the generation-bound lifecycle
gate before permanent retention, which excludes fresh attempts but does not
reinterpret an ambiguous retained owner as safe to reroute. Restart and
reinitialization, plus terminal drain/revocation of provably unstarted queued
work, remain later lifecycle work.

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
claim. Fixed channels, task storage, permit stores, and IRQ/synchronization-
visible state remain in internal static RAM. The LXMF backing allocations named
below are deliberately external. The ordinary and Wi-Fi profiles retain their
measured 64 KiB reclaimed heap. The opt-in BLE profile receives 72 KiB followed
by the detected PSRAM. That is the largest whole-KiB allocation that fits the
ESP32-S3 linker's separate 73,744-byte DRAM2 segment; it leaves 16 bytes rather
than reducing the product executor stack in ordinary DRAM. The BLE-only
additional 8 KiB is available to esp-radio's 8,192-byte strict-internal
controller-task stack and other controller allocations. This remains below
the pinned esp-radio 0.18 documentation's conservative 100 KiB total
recommendation (64 KiB reclaimed plus 36 KiB ordinary). The 2026-07-23 powered
BLE startup diagnostic nevertheless completed controller initialization,
Trouble host/GATT construction, runner startup, and advertising under exactly
72 KiB, with 41,040 internal-heap bytes free after advertising. No further heap
increase is required for this startup path; sustained authenticated load and
high-water qualification remain open. Because `esp-alloc` searches registered
regions in order, ordinary global allocations currently consume internal heap
first and spill into PSRAM only when no internal hole fits. That is a measured
baseline, not the intended long-term placement policy: large protocol/client
payloads will need explicit external allocation, while atomics, synchronization,
DMA/IRQ-visible state and flash-critical state must remain internal. Largest
contiguous free space is not exposed by the pinned allocator and must not be
inferred from total free bytes.

The explicitly external allocations are now the 128-entry submission runtime,
LXMF store index, delayed-proof slot backing, and retry/fault/proof-holder
state. The target submission runtime is exactly 375,544 bytes (the 64-bit host
fixture is 375,568 bytes), including its independent actor-owned journal-replay
scratch index. The index's 512 opaque slots
are derived from the exact 2 MiB partition length divided by the store's 4 KiB
extent size. Each allocation is made with `ExternalMemory` after PSRAM
registration, checked for its expected initialized length/bytes/alignment and
containment in the detected PSRAM mapping, then leaked for the boot lifetime.
Allocation or validation failure leaves the product inert; there is no internal-
RAM fallback. The application-event slots remain in internal static RAM.

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
| Node journal | `0x630000` | 1 MiB | Resident operation-scoped submission runtime; current 128-entry non-reclaiming cap in PSRAM below the 162-acceptance journal lifetime; authenticated submission and post-re-enumeration terminal status powered-qualified, but not a powered 128-entry fill |
| Message store | `0x730000` | 2 MiB | Wired ADR 0011 format-1 raw-RNS inbox; one 576-byte commit-last item; 383-byte maximum; not LXMF |
| LXMF store | `0x930000` | 2 MiB | Wired ADR 0014 append-only store; 512-slot PSRAM index; mount-gated opportunistic `lxmf.delivery` admission; one earlier HIL A-to-B commit-before-proof exchange plus API 1.4 bidirectional send/commit/list/read are powered-qualified; `AlreadyDurable` replay remains unqualified |
| Unallocated | `0xb30000` | 4.8125 MiB | OTA/layout decision |

The workspace runner in `.cargo/config.toml` hardcodes an 8 MiB flash size and
must not be used for this target.

### Opt-in runtime-measurement HIL

The non-default `runtime-measurement-hil` feature instruments the permanent
product graph without changing its transport or storage ownership. It is
mutually exclusive with `journal-schema2-dev-reprovision` and
`rns-inbox-commit-fault-hil`, and its only dependency-graph addition below the
product root is `esp-alloc/alloc-hooks`. The ordinary ELF contains neither the
measurement evidence/stack marker nor allocator callbacks. Boot, storage,
inbox, authenticated-API, allocator, stack and node-loop observations remain
product/node concerns; RX, CAD, TX and radio-loop observations remain local to
the LoRa actor. A later Wi-Fi, BLE, USB or second-radio Reticulum actor can add
its own actor observations through the same diagnostics boundary without
changing the transport-neutral DATA projection or durable inbox contract.

The HIL exposes an initialized, exact 256-byte `RTME` version-1 ABI. Its 64
little-endian words contain a leading sequence marker, header/state, memory and
stack snapshots, eight boot-phase last/maximum pairs, operation/scheduler
aggregates, error/allocation counters, and a trailing sequence marker. A
low-to-high capture is valid only when the two markers match and are even; an
odd or mismatched pair is a torn observation and must be discarded. Locate the
evidence symbol from the exact ELF being measured rather than treating one
run's RAM address as stable. The checked decoder rejects the wrong length,
header, flags, sentinels, inconsistent counts/timings, impossible heap/stack
relations, and unstable sequence markers:

```sh
cargo +stable run --locked -p xtask -- e290-runtime-measurement decode \
  --input /path/to/evidence.bin [--json]
```

The same HIL now exposes a separate initialized, exact 192-byte `RPTE`
version-1 proof trace. Its 48 words distinguish logical radio reassembly from
ingress handoff outcomes, RNS disposition, locally generated explicit delivery
proofs, delivered and timed-out receipt terminals, action pressure,
correlation faults, confirmed versus not-confirmed-success radio-TX wrapper
outcomes, and Ready-gate inbox-admission attempt boundaries. It retains only
counts, millisecond timestamps, packet classification, and three compact
first-eight-byte correlation tags; it contains no payload or complete hash.
Tags are diagnostic aids for one isolated attempt, not cryptographic proof or
globally unique identifiers. JSON renders each combined 64-bit tag as a fixed
hex string so JavaScript clients cannot lose bits above `2^53`.

```sh
cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement decode-proof-trace \
  --input /path/to/proof-trace.bin [--json]
```

Stage 5 adds a third initialized, exact 96-byte `LXTE` version-1 trace. It
records durable-new and already-durable observations, the complete 32-byte
LXMF message ID, opaque durable handle, proof-ready/released/ordinary-handoff
frontiers, a compact proof-correlation tag, and any commit-before-proof ordering
violation. It contains no message body or exact carrier. Decode it directly:

```sh
cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement decode-lxmf-trace \
  --input /path/to/lxmf-trace.bin [--json]
```

For a checkpoint captured as the required exact 544-byte contiguous
`LXTE || RTME || RPTE` range, decode and correlate all three records together:

```sh
cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement decode-checkpoint \
  --input /path/to/checkpoint.bin [--json]
```

The combined decoder emits schema `reticulum.e290-runtime-checkpoint.v2`, with
the decoded records named `lxte`, `rtme`, and `rpte`. It reports the RTME
TX-operation total beside the sum of
the two RPTE TX-outcome counters. A mismatch is retained as a diagnostic, not
a malformed-record error: the debugger can halt the actor after RPTE records
the radio result but before the surrounding RTME operation guard completes.
Stable baseline and terminal acceptance checkpoints still require the RTME
and RPTE totals to agree.

All three records use matching-even sequence markers. Their record methods depend
on the current single-core cooperative executor and never yield; the sequence
protocol detects torn debugger reads but is not a multi-writer lock. A future
multicore Wi-Fi/BLE actor must add synchronization or use a separate per-writer
record. The always-present RNS metadata remains transport-neutral. Only the
logical reassembly/enqueue hooks are LoRa-actor-specific, so another Reticulum
interface can add its own ingress-frontier evidence without changing receipt
or inbox semantics.

The same project-local command inspects the two release ELFs used to preserve
the static side of the stack bound:

```sh
cargo +stable run --locked -p xtask -- e290-runtime-measurement inspect-elf \
  --default-elf target/e290-default/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node \
  --hil-elf target/e290-runtime-measurement-hil/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node
```

It accepts only final little-endian 32-bit Xtensa `ET_EXEC` images with one
nonempty, relocation-free `.stack_sizes` section. Both maximum frames must be
at most 53,680 bytes, both linker guard offsets must remain 60 bytes, and the
post-offload default/HIL usable stacks must remain at least 162,376/161,576
bytes. Those checks constrain individual frames and total linked stack space;
they do not by themselves constrain simultaneously live nested frames. The
inspector therefore resolves and sums the exact compiler-emitted components of
all three capacity-sensitive storage paths, then adds a reviewed 4,096-byte
reserve for the lower ROM flash-read implementation and interrupt entry that
`.stack_sizes` cannot describe:

| Profile | Mount | Append | Compact | Reserve per path |
| --- | ---: | ---: | ---: | ---: |
| Default | 53,072 B | 52,816 B | 52,704 B | 4,096 B |
| Runtime-measurement HIL | 53,248 B | 53,040 B | 52,928 B | 4,096 B |

The resulting enforced totals are 57,168/56,544/56,432 bytes for the default
mount/append/compact paths and 57,344/56,768/56,656 bytes for HIL. Missing,
ambiguous, conflicting, or changed component topology fails closed for review
rather than silently shortening a path.

The original cumulative mount gate was added after the first API 1.4 default
startup failed before USB enumeration. Every individual emitted frame passed
the old maximum-frame check, but the nested default mount chain totaled 175,296
bytes against only 174,912 usable, a 384-byte deficit. The corresponding HIL
chain exceeded its 174,112-byte usable stack by 1,360 bytes. Direct
caller-destination placement fixed that historical 16-entry boundary.

Increasing the profile to 128 entries exposed the same class of risk in lower
boot and mutation replay. The initially flashed 128-entry image failed the
expanded final linked-path gate and is not qualified. The current
`StorageActor` owns a second index in PSRAM: mount initializes both indexes at
their final addresses, and append validation plus compaction reconstruct flash
state in the scratch index while retaining the live index until a durable
outcome. No capacity-sized replay value is returned through those active CPU
stack paths. The table above is the final static bound for that source; it is
not a powered high-water measurement. The corrected 128-entry image has passed
a bounded powered bidirectional chat proof; a 128-message fill and pressure
qualification remain open.

The default ELF must exclude both traces; the HIL ELF must contain
exactly one initialized 192-byte symbol and one initialized 96-byte symbol
whose linked bytes decode as valid empty `RPTE` and `LXTE` records. Record
counts are diagnostic rather than policy and must be captured from the ELFs
actually inspected. The retained continuous-RX artifact pair described below
contains 946 default and 962 HIL records with 53,680-byte maxima; the earlier
powered release ELFs contain 816/832 records with 52,752-byte maxima. CI runs
Clippy and then
relinks both current profiles with
`-C link-arg=-nostartfiles -Z emit-stack-sizes` in isolated target directories
immediately before this inspection.

The retained Stage 5 post-offload default/HIL artifact usable stacks are
175,056/174,256 bytes and both guard offsets are 60 bytes. The preceding
pre-PSRAM pair was
165,032/164,336. Before the post-offload image was powered, its linked-only
interim policy carried the older raw margin forward as 63,436 bytes after
8,584 bytes of post-proof linked internal-RAM growth, leaving 9,756 bytes under
the 53,680-byte ceiling. The historical pre-LXTE Stage 5 placement checkpoint
superseded that interim policy with 57,716 powered raw bytes. The independent
announce scheduler adds sixteen linked bytes, so current policy carries forward
57,700 bytes. Subtracting the current 53,680-byte maximum-frame ceiling leaves
a fail-closed 4,020-byte policy margin. The pre-scheduler historical values
remain 4,036 and 4,052 bytes respectively. The final
default-profile linked-layout delta is 2,632 bytes; the HIL
delta is 2,640 bytes. Both remain useful provenance and neither is the size of
the externally placed LXMF state. This floor qualifies the
E290's internal CPU0/main-
executor task stack; it is not a compatibility ceiling for non-PSRAM ESP32
boards, and PSRAM cannot back this internal task stack. The full E290 profile
already requires PSRAM for its separate application/storage capacity, while
Tracker V2 remains a separately sized reduced profile.

### Stage 5 PSRAM boot checkpoint

#### Retained continuous-RX LXTE/v2 artifact and readback binding

The retained final Stage 5 pair is bound to the independently scheduled primary
and LXMF discovery source, the persistent continuous-RX epoch, and the 544-byte
checkpoint-v2 layout. The default ELF is 13,648,888 bytes with SHA-256
`92e63b60a5f4b830ee55d958fcc446a6878036212904b8748519ae210ba3da58`;
its explicit address-zero package is 868,656 bytes with SHA-256
`c8da2af30e2d0ee24ca4b215151d1370b7e1d242991ebbeb024079a730693a3f`
and uses 803,120 application bytes. The HIL ELF is 13,821,496 bytes with SHA-256
`7a3fad34699f910a2050468ada6461a0f33d16641ab5425a5c795a71238861ff`.
The explicit address-zero package is 881,456 bytes with SHA-256
`12c6f31a7fb64485ad9220edca4ac38ba0a57867ad88ce60fa1a24ffc195d379`
and uses 815,920 application bytes. Identity-bound address-zero readbacks from
both `AC:A7:04:E1:3E:88` and `AC:A7:04:E1:3F:88` matched that package exactly.
Those paired ELFs also pass the static 946/962-record, 53,680-byte
maximum-frame, 175,056/174,256-byte usable-stack, 60-byte guard, RPTE, and LXTE
gates. The powered result below then adds clean baselines, one exact A-to-B
delivery, correlated post-terminal checkpoints, and an exact receiver-store
readback; it is no longer only a build/layout/readback claim.

#### Historical paired-announce discovery failure

The immediately preceding LXTE/v2 pair sent primary and `lxmf.delivery`
announces back-to-back in one batch. Its default ELF/package were 13,478,724 and
859,424 bytes with SHA-256
`eea897c967f8e2ebd8aeadd1c8c45def4b85536622af216d5d9be3d95adb9ede`
and `3d2a4c7e1140130fc3abe51675e8b047bd7bd1606fb2a6514e62a06f22e2b51d`.
Its HIL ELF/package were 13,637,240 and 870,656 bytes with SHA-256
`46b8c880cb1f6da1c38c7f1f03f4b3d2ff6ea91153ba48b9f93f4261b0322bc4`
and `3c07fceb619f1cdf89e08c1039bac99b32847974fb22468690e92517bf220b04`;
identity-bound exact readbacks passed on both E290s.

That historical powered attempt established a discovery failure, not durable
LXMF delivery. Across the deterministic bootstrap cycles B processed exactly
three distinct announces from A, yet submission to A's announced
`lxmf.delivery` hash returned `no-path`. Rete transport mode immediately queues
the first accepted announce for rebroadcast. On half-duplex LoRa, the receiver
therefore begins relaying the primary while the sender transmits the second
service announce, and misses that service announce. Repeating the same paired
ordering repeated the collision. The current scheduler fixes the product
composition by attempting at most one destination per event and separating the
two destinations; it does not change Rete's transport relay behavior. The
current A-to-B confirmation below includes powered discovery through that
replacement schedule.

#### Historical pre-LXTE placement checkpoint

Earlier on 2026-07-21, E290 `AC:A7:04:E1:3E:88` received the first
post-offload runtime-measurement image. This historical artifact predates LXTE
and checkpoint v2. Its 13,607,972-byte ELF has SHA-256
`da392e91b3a6ace58fca9d0064700f249ca9876df6ed5d109a9a683c2dc873ca`.
Its explicit address-zero 16 MiB package is 868,800 bytes with SHA-256
`2e5d898bd55da61b132555d628eae9cc7ec42fc84e9c9e33dc04fdd8875813d0`;
an exact 868,800-byte readback matched that digest.

At uptime 20,577 ms, one stable combined `RTME || RPTE` capture had an intact
guard, complete composition, consistent two-operation TX partition, zero
failed allocations, zero unexpected errors, and zero RX/CAD/TX watchdog
timeouts. The allocator held exactly 163,536 bytes in external PSRAM, matching
that composition's LXMF index plus delayed-proof and retry/fault/proof-holder
allocations. Minimum free external and internal heap were 8,225,072 and 63,428
bytes. The HIL retained 57,716 painted stack bytes. The painter was initialized
before the one-shot `NodeCore::new` constructor that owns the actual 53,664-byte
maximum frame, so the raw watermark already includes that boot invocation.
Subtracting the same frame again leaves a deliberately pessimistic 4,052-byte
co-location allowance; it is not evidence that only 4,052 bytes remained at
runtime. Interrupt/nesting and later traffic call chains are still unquantified,
so this is a successful placement and boot checkpoint, not final stack-safety
qualification. A subsequent
authenticated `identity-summary` returned primary destination
`c99e8ff1ec8629e4e1290e14462ae8af`. The later 93,663-ms capture retained the
same memory and stack observations and recorded one 669-us API dispatch, but it
also recorded one 1,501,156-us RX watchdog timeout after the debugger-attached
API read. It is therefore not folded into the clean pre-API watchdog baseline,
and no cause is assigned by this evidence. No LXMF packet was offered, and the
then-absent second E290 prevented powered remote durable-delivery qualification
in that run.

The target-scoped pre-PSRAM runtime-measurement HIL rebuild links with
text/data/BSS of 695,315/4,180/468,648 bytes (1,168,143 bytes total by GNU
size). Its 12,498,348-byte ELF has SHA-256
`c84363dff0801a1679dd786b5070c4662962d299f0269efc0cd72ff9c09b8e2a`.
Its explicit 16 MiB package uses 734,944/6,291,456 application bytes (11.68%)
and produces an 800,480-byte merged image with SHA-256
`058a969e0b9e099f6a5febd1b59f4a70cfd3ea932e8f0738a2ddb4b3e5569119`.
That image matched an exact address-zero readback on `3e:88`.

At uptime 108,940 ms, an authenticated API checkpoint reported 8,388,608
bytes of PSRAM, 928 bytes of maximum allocator use, 64,608 bytes of minimum
internal-heap free, and no external-heap allocation. The painted main stack
retained 63,828 bytes; subtracting the unchanged 53,680-byte compiler-emitted
maximum frame leaves a 10,148-byte conservative powered margin before the
still-unquantified interrupt/nesting allowance. One authenticated API dispatch
completed with a 594-us maximum. The checkpoint recorded zero unexpected
errors, failed allocations, RX/CAD/TX watchdog timeouts, correlation faults,
and not-confirmed-success transmissions; both observed radio transmissions
were confirmed successful. This is a bounded one-board idle/API/TX checkpoint,
not a sustained workload or two-board RF result.

After the checkpoint, `3e:88` was restored to a rebuilt 789,504-byte default
package with SHA-256
`a67afa72681558dc02fd0575a18711b2b3c05b365a66af45441b7cb8dd3a2577`.
The address-zero readback matched exactly and authenticated
`identity-summary` succeeded. Board `3f:88` still did not enumerate.

The exact release artifacts used for the 2026-07-20 powered run compare as
follows. Paths embedded by the build can affect rebuild digests, so these
identify the retained run artifacts rather than universal source digests.

| Image | Text | Data | BSS | GNU total | Merged image |
| --- | ---: | ---: | ---: | ---: | ---: |
| Default | 655,347 | 3,676 | 469,152 | 1,128,175 | 761,792 |
| Measurement HIL | 662,279 | 3,988 | 468,840 | 1,135,107 | 768,624 |
| Delta | +6,932 | +312 | -312 | +6,932 | +6,832 |

The default ELF and merged image have SHA-256
`ddd3852bee960f837fd6c472fb525af92e00acc9077b71c8d8f5ab7fb269aed2`
and `77b6a48e71d62facf39bae380387397dcbc79417c05372bc31c4a240f326b066`.
The measurement ELF and merged image have SHA-256
`84146930cad448f6aa5d4ecc8bd8493bb49de7b623ea9341ebc0b930c96f2aa8`
and `c20032b04a87fc8c33982bd7e4a5788f59ae5a00f7d26a1caf9f6ecf0473fa14`.
The HIL linker reserves 170,544 stack bytes, of which 170,480 are measured
after guard/scanner exclusions; the corresponding default reservation/usable
pair is 171,048/170,984 bytes. For those exact historical artifacts, the
largest compiler-emitted frame is 52,752 bytes.

The later proof-trace diagnostic extension deliberately preserved those
historical artifacts while producing an isolated `9bceacd` pair:

| Image | Text | Data | BSS | GNU total | Merged image |
| --- | ---: | ---: | ---: | ---: | ---: |
| Historical `9bceacd` default | 661,147 | 3,676 | 469,152 | 1,133,975 | 767,552 |
| Historical `9bceacd` measurement + proof trace HIL | 672,935 | 4,180 | 468,648 | 1,145,763 | 779,184 |
| Delta | +11,788 | +504 | -504 | +11,788 | +11,632 |

The historical `9bceacd` default/HIL ELF SHA-256 values are
`4d2b20271e92ce175e8acccdd8440344849e1c75dd3bd6a6994e4eefa343c2b2`
and `8fcf71705a6f59a8346d42d8f5eda4228a84f90fa860d8f0becb5d9385ccf86e`.
The corresponding merged-image values are
`b6a93f0ac20e1c151cd8797b4a0a0e3731da75c82b9bd623103b29c94ede82a9`
and `fe5fae51d83ef248a46965f75dab87196c1e79c2b4a72797cdf995e9c99a3e15`.
The added initialized trace record moves the HIL reservation/usable pair down
by exactly 192 bytes to 170,352/170,288; the default pair is unchanged.

Those historical artifacts passed build, graph, ELF, and static-stack gates but
were not powered-qualified: both boards were absent after the preceding
debugger-reset attempt. They do not describe an ELF built from the preceding
`8b5d652` or `14c7b49` pins, or from the current `90570ca` pin. The immediately
preceding 777,600-byte HIL image,
SHA-256
`151a66cc92b83268050c61bfc983ad6d9452fac0626d260c26da877c552c800e`,
did pass an identity-qualified flash and exact address-zero readback on board
`3e:88`. It used the same 192-byte `RPTE` layout but predated the current
TX-outcome recording in words 45 and 46. At 30,835 ms uptime, after boot and
before any authenticated request or Reticulum ingress, its `RTME` reported a
72,020-byte painted stack margin, 904-byte maximum allocator use, 64,632-byte
minimum internal free space, no external allocation, no failed allocation, no
radio watchdog expiry, and no unexpected measurement error. `RPTE` decoded
with stable sequence 4, no saturation or input inconsistency, zero logical RX,
RNS ingress, proof, receipt terminal, correlation fault, and inbox-commit
counts. Its only observations were two initial action-pressure observations at
1,205 ms while the boot announce entered the ordinary transmit path; the two
then-reserved TX-outcome words remained zero. This is powered boot-only
evidence for the immediately preceding trace revision, not evidence for the
current hashes or the now-completed two-board continuous-RX confirmation.
The 72,020-byte raw margin leaves 19,268 bytes after subtracting the unchanged
52,752-byte maximum compiler frame in that artifact. The pre-PSRAM policy
conservatively deducted 5,952 bytes of subsequent linked internal-RAM growth,
including the application-event tranche's exact 2,408-byte reduction. The
post-offload linked-only interim policy used the 2,632-byte default-profile
layout delta; the separately gated HIL delta was 2,640 bytes. That policy left
63,436 carried-forward raw bytes, leaving 9,756 bytes
under the 53,680-byte frame ceiling or 9,772 bytes under the actual 53,664-byte
maximum. The historical pre-LXTE Stage 5 placement checkpoint supersedes those
interim values with 57,716 raw bytes, a 4,036-byte policy margin at the ceiling, and 4,052
bytes under the actual maximum. The painter already includes the one-shot
maximum-frame constructor invocation, so the second frame subtraction is
deliberately pessimistic rather than a measured runtime remainder.

### Stage 5 durable-LXMF two-board trial

**Powered outcome: pass after the split-frame receive fix.** The current narrow
trial is one newly committed opportunistic LXMF message from A to B. It proves
the complete chain below and nothing broader:

1. pinned Python LXMF 1.0.1 derives the two boards' real `lxmf.delivery`
   destinations and generates one signed opportunistic carrier;
2. A's authenticated API submits that carrier as RNS DATA addressed to B's
   derived `lxmf.delivery` hash, and the permanent LoRa/Rete path carries it;
3. B validates the LXMF carrier and writes exactly one committed record to its
   dedicated 2 MiB store;
4. B makes the retained RNS proof ready only after the store returns the new
   durable receipt, then hands that proof to the ordinary transport-neutral
   supervisor; and
5. A receives the correlated proof and its submission reaches `Delivered`.

This trial does not qualify Link or Resource delivery, propagation, outbound
LXMF state, app-level LXMF receipts, sustained routing, power-cut recovery, or
replay after reboot. In particular, replaying the authenticated API request
with the same idempotency key is only a submission-journal replay and emits no
second RF packet. A later powered store-replay trial must freshly retransmit the
same LXMF carrier after receiver remount; only that event may exercise
`AlreadyDurable` and release its own freshly retained proof. No volatile proof
is expected to survive reset. The confirmation used one and only one fresh
A-to-B submission. It reached `Delivered` on that first attempt; no API or RF
retry occurred.

#### Split-frame receive-blind diagnosis

The diagnostic B-to-A run used an exact 880,176-byte HIL package with SHA-256
`73280d77171e204fff9cedd87ef76c3faaed9b1dab5ce86b0dbe4f2232bd9641`;
identity-bound address-zero readbacks matched on both boards. Its 206-byte LXMF
carrier became a 288-byte encrypted payload and an exact 307-byte RNS DATA
packet. RNode framing produced 255-byte and 54-byte physical frames with
535,040 us total nominal airtime. B's 630,732-us maximum TX and durable journal
show that the complete logical packet entered `AwaitingDelivery`. A completed
the preceding discovery exchange but admitted no LXMF event, and its exact
2 MiB LXMF store remained all `0xff`; B terminated in `delivery-timeout`.
This localizes the loss before A's logical DATA admission.

The pre-fix checkpoint did not count individual physical frames, so it cannot
prove whether A received the first half. Source inspection nevertheless found
a matching receive-blind interval: the permanent actor used one finite
`RxMode::Single(248)` operation per physical frame, entered standby after the
frame, returned through scheduling and RNode reassembly, and then completely
reconfigured RX. An interoperable RNode sender may transmit the continuation
immediately. Earlier successful split deliveries make this a timing race, not
a deterministic framing or reassembly defect.

The source fix keeps one `RxMode::Continuous` epoch armed across packets and
reassembly work. Only the cancellation-safe DIO wait races the 253,952-us
software scheduler timer; after preamble, sync-word, or valid-header progress,
IRQ processing runs to a terminal frame/error or a recoverable progress
deadline without TX selection. The latter rearms continuous RX, matching
RNode's false-preamble unlatch behavior. Taking a TX job invalidates the
receive epoch, and CAD/TX explicitly performs standby,
IRQ-routing disable, and pending-IRQ clear before changing modes. No artificial
inter-frame TX delay was added, because external RNode senders are not required
to honor one.

Host command-trace coverage now feeds this exact 307-byte logical shape as
255- and 54-byte frames, reassembles it after one continuous `SetRx`, and proves
there is no standby between frames. Separate tests cover repeated scheduler
yields without rearm, stalled-preamble rearm followed by a valid frame,
invalid-frame recovery while RX remains armed, receive-epoch invalidation, and
exact quiesce ordering before CAD/TX. The host suites, strict host and ESP32-S3
Clippy, default/HIL target
links, and the ELF stack/resource inspector pass. These deterministic results
justified exactly one powered confirmation. The evidence recorded below is that
single confirmation, not a succession of empirical timing trials.

Use fresh owner-only full-flash backups from both identity-qualified boards.
Create the public trial material with the isolated dependencies pinned by
`interop/python/requirements-lxmf-1.0.1.txt`:

```sh
LXMF_PYTHON=/path/to/isolated-lxmf-1.0.1-venv/bin/python
"$LXMF_PYTHON" interop/python/generate_e290_lxmf_trial.py \
  --source-flash "$A_FLASH" \
  --destination-flash "$B_FLASH" \
  --source-primary-hash c99e8ff1ec8629e4e1290e14462ae8af \
  --destination-primary-hash 83a09ed807a0a7c631386deaa0448fb9 \
  --timestamp "$LXMF_TIMESTAMP" \
  --title "$LXMF_TITLE" \
  --content "$LXMF_CONTENT" \
  --output "$TRIAL/lxmf-trial.json"
```

The generator refuses to overwrite its output, validates both mirrored private
identity records and the expected primary destinations, derives the inbound and
outbound `lxmf.delivery` hashes independently, and verifies the Python message
ID and signature. Its JSON contains only public material, including
`destination_lxmf_hash`, `message_id`, `full_wire_sha256`, `carrier_sha256`,
`carrier_bytes`, and `carrier_hex`; it does not export private identity bytes.

Start with an empty, verified receiver `lxmf_store` and a sender journal able to
accept one fresh submission. Provision the isolated interpreter from the pinned
requirements file and retain its package inventory. Blank B's exact store range
with the identity-owning helper before flashing the final HIL image:

```sh
PATH="$ESPFLASH_BIN_DIR:$PATH" \
python3.13 interop/python/e290_qualification_host.py erase-region \
  --usb-serial AC:A7:04:E1:3F:88 \
  --expected-mac ac:a7:04:e1:3f:88 \
  --expected-flash-bytes 16777216 \
  --evidence-prefix "$TRIAL/b-lxmf-store-erase" \
  --offset 0x930000 --length 0x200000
```

Require the exact 2 MiB readback to contain only `0xff` and have SHA-256
`4bda3a28f4ffe603c0ec1258c0034d65a1a0d35ab7bd523a834608adabf03cc5`.
Flash the exact current HIL package to both boards with identity-bound readback.
Reset both into fresh product boots and wait through both bootstrap retry pairs
before submission. Relative to each board's boot, the expected local schedule is
primary at 0 seconds and LXMF at 8 seconds; A then emits at 26/34 and 64/72
seconds, while B emits at 43/51 and 81/89 seconds. Capture the required clean
pre-submit checkpoint only after that discovery window. A `no-path` terminal is
a failed discovery/setup attempt, not LXMF delivery evidence; preserve its
submission and restart from controlled trial state instead of consuming the
bounded non-reclaiming journal with repeated novel requests.

Before submission and immediately after the terminal result, capture each board
with the final HIL ELF and the current tool:

```sh
cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement capture-checkpoint \
  --hil-elf "$HIL_ELF" \
  --usb-serial "$USB_SERIAL" \
  --output "$OUT"
```

The output must carry schema
`reticulum.e290-runtime-checkpoint-capture.v2`; `checkpoint.bin` must be the
exact 544-byte contiguous `LXTE || RTME || RPTE` range, and its decoded record
must carry schema `reticulum.e290-runtime-checkpoint.v2`. The directory also
contains exact 96-byte `lxmf-trace.bin`, 256-byte `runtime.bin`, and 192-byte
`proof-trace.bin` splits plus human/JSON decodes and a hash-bound manifest.
Accept only independently matching-even records and a complete marker. A
debugger failure or incomplete marker invalidates the trial until both boards
are recovered and the clean sequence is restarted.

Submit exactly the generator's `carrier_hex` to its
`destination_lxmf_hash`, using a fresh 16-byte idempotency key and A's Active
credential:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port "$A_PORT" \
  --state-file "$A_ACTIVE_CREDENTIAL" \
  --timeout-ms 120000 \
  --destination-hash "$DESTINATION_LXMF_HASH" \
  --payload-hex "$CARRIER_HEX" \
  --idempotency-key "$IDEMPOTENCY_KEY_HEX" \
  submit-and-wait \
  --evidence-output "$TRIAL/sender-terminal.json"
```

After terminal captures, reset B into the loader, read exactly
`0x930000..0xb30000` to a fresh private 2 MiB file with
`e290_qualification_host.py read-region`, and inspect it read-only:

```sh
PATH="$ESPFLASH_BIN_DIR:$PATH" \
python3.13 interop/python/e290_qualification_host.py read-region \
  --usb-serial AC:A7:04:E1:3F:88 \
  --expected-mac ac:a7:04:e1:3f:88 \
  --expected-flash-bytes 16777216 \
  --evidence-prefix "$TRIAL/b-lxmf-store" \
  --offset 0x930000 --length 0x200000 \
  --output "$TRIAL/b-lxmf-store.bin"

cargo +stable run --locked -p xtask -- e290-lxmf-store-inspect \
  --image "$TRIAL/b-lxmf-store.bin" \
  --source-mac aca704e13f88 \
  > "$TRIAL/b-lxmf-store.json"
```

The inspector accepts only an exact 2 MiB image mounted under B's physical
binding and emits metadata, not message contents. A narrow pass requires all of
the following terminal-minus-baseline facts:

- A's authenticated evidence ends in `Delivered`, with one delivered receipt,
  no timeout, and no extra terminal;
- B's LXTE delta is durable-new `+1`, already-durable `+0`, proof-ready `+1`,
  proof-released `+1`, ordinary-handoff `+1`, and ordering-violation `+0`;
- B's LXTE `last_commit_kind` is `new`, its complete `last_message_id` equals
  the generator's value, its durable handle is nonzero, and neither saturation
  nor input inconsistency is present;
- B's LXTE released-proof tag equals A's RPTE delivered-receipt tag; B has
  exactly one post-baseline confirmed TX and no not-confirmed-success TX. The
  retained LXMF proof is intercepted before ordinary ingress metadata is
  formed, so B's RPTE generated-proof count and tag remain zero by design and
  are not part of this correlation;
- RTME/RPTE TX partitions are consistent at the accepted checkpoints, and no
  new allocation, unexpected-error, or radio-watchdog fault invalidates the
  clean trial; and
- the store inspector reports exactly one committed record whose message ID,
  destination/source hashes, normalized-wire length, and exact-wire digest
  match the generator's `message_id`, derived hashes, full wire, and
  `full_wire_sha256`.

Do not infer ordering merely from a final store dump or from A's `Delivered`
terminal. The LXTE new/ready/released/handoff frontiers and cross-board proof-tag
correlation are the intended evidence that durability preceded release through
the ordinary supervisor.

The completed evidence is retained under
`/private/tmp/e290-continuous-rx-confirm-20260721.2AGH6m`. A's exact 1 MiB
journal preflight was erased, with SHA-256
`f5fb04aa5b882706b9309e885f19477261336ef76a150c3b4d3489dfac3953ec`.
One boot of the 868,672-byte schema-2 reprovision package, SHA-256
`4998fb2ce23f6f1a351dce8bfda6567f533d61168c791239713df51df2106b8e`,
created its manifest; the identity-bound journal readback then had SHA-256
`a6d0b254e7fee84f2f00c45f4075fdafc8f5630dc162cfaf22a72d4de0add054`.
B's exact 2 MiB receiver store was written and read back as all `0xff`, with
SHA-256
`4bda3a28f4ffe603c0ec1258c0034d65a1a0d35ab7bd523a834608adabf03cc5`,
before the current HIL was flashed to both boards.

Both post-discovery baselines were complete, independently matching-even
checkpoint-v2 captures with consistent RTME/RPTE TX partitions. A began at 24
confirmed TX outcomes and B at 20; both LXTE records were empty, both RPTE
records had zero generated, delivered, timeout, and correlation-fault counts,
and both RTME records had zero unexpected, allocation-failure, and RX/CAD/TX
watchdog counts. One and only one fresh A-to-B submission then used a 206-byte
carrier. The generator produced message ID
`abdeec2e498f09c96a6fd56ec3558ca86c2598aaeacac81969b645de3b549dc3`
and full-wire SHA-256
`1c1839991401e01e15e3a3146cd3177a4fb7e5dbd52008fd119beaf091d377ba`.
The authenticated API encoded an exact 307-byte RNS packet with SHA-256
`060037041c91eb5999f89bf84845c19e65bf7fa680827cce9c51e8ecc5dbe0a6`
and terminated `Delivered` on the first attempt. No retry occurred.

| Stage 5 powered evidence | Outcome |
| --- | --- |
| Exact current HIL package/readback on A and B | Pass: 881,456 bytes, 815,920 application bytes, SHA-256 `12c6f31a7fb64485ad9220edca4ac38ba0a57867ad88ce60fa1a24ffc195d379` |
| Post-flash pre-submit checkpoints | Pass: complete clean baselines on both boards; TX partitions consistent; all 44 baseline TX outcomes confirmed |
| Fresh Python carrier and empty receiver-store baseline | Pass: one 206-byte carrier; B's exact 2 MiB store was verified all `0xff` |
| A submission terminal and RPTE receipt correlation | Pass: one 307-byte packet reached `Delivered`; A delivered tag `0x3dc4588d3a205429`; no timeout or retry |
| B LXTE durable-before-proof frontiers and TX correlation | Pass: new/ready/released/handoff `+1`, already-durable/ordering-violation `+0`; message ID exact; durable handle 1; LXTE tag `0x3dc4588d3a205429`; exactly one additional B TX, confirmed |
| RPTE generated-proof expectation | Pass by design: B generated count/tag stayed zero because retained LXMF proofs are intercepted before ordinary ingress metadata; B LXTE release tag plus B's one confirmed TX plus A's delivered tag form the valid correlation |
| Fault counters at terminal checkpoints | Pass: zero new unexpected, allocation-failure, RX/CAD/TX-watchdog, correlation, not-confirmed-success, saturation, input-inconsistency, or LXTE-ordering faults |
| B exact 2 MiB store readback and metadata inspection | Pass: SHA-256 `c75ab2a01b3266fda1e07e0271c70bb29c06e32636d70d8a70d977b9e8b0e21e`; exactly one committed record with the generated message ID, destination/source hashes, 222-byte normalized wire, 206-byte carrier, and exact full-wire digest |
| Overall narrow durable-LXMF claim | **Pass: one exact A-to-B new commit preceded retained-proof release and first-attempt delivery** |

### Historical decisive proof-correlation trial runbook

The historical plan called for four clean trials in `B→A`, `A→B`, `A→B`,
`B→A` order. It remains a separate raw-RNS proof-correlation procedure, not
the retained Stage 5 durable-LXMF trial. Its fixed board bindings were:

This runbook is pinned to the exact pre-stage-4 `bac2dcc` proof-correlation
artifact. Its `0x930000` upper boundary, 3 MiB erase/readback length, and known
all-`0xff` hash are deliberately artifact-specific historical evidence. Do not
silently substitute the current `0xb30000` product boundary or recompute those
published values when reproducing that artifact. This artifact predates LXTE:
its checkpoint ABI is the historical 448-byte `RTME || RPTE` layout. Reproduce
it only with the exact `bac2dcc` source and capture tool. The current v2 tool
requires LXTE and intentionally rejects that historical ELF.

| Board | USB serial / MAC | Active credential | Primary destination |
| --- | --- | --- | --- |
| A | `AC:A7:04:E1:3E:88` / `ac:a7:04:e1:3e:88` | `/private/tmp/e290-rns-inbox-proof/3e-active.key` | `c99e8ff1ec8629e4e1290e14462ae8af` |
| B | `AC:A7:04:E1:3F:88` / `ac:a7:04:e1:3f:88` | `/private/tmp/e290-rns-inbox-proof/3f-active.key` | `83a09ed807a0a7c631386deaa0448fb9` |

Before using the historical artifact, rerun its strict default/HIL builds and
ELF inspector from the exact checkout, package the explicit 16 MiB merged
image, and bind its size and SHA-256 to the trial manifest. Capture from that
exact final HIL ELF; do not copy addresses from another build:

```sh
source ~/export-esp.sh
HIL_ELF=target/e290-runtime-measurement-hil/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node
cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement capture-checkpoint \
  --hil-elf "$HIL_ELF" \
  --usb-serial "$USB_SERIAL" \
  --output "$OUT"
```

`USB_SERIAL` is the exact uppercase colon-separated serial from the board table,
`EXPECTED_MAC` is the same board's lowercase eFuse MAC, and `OUT` must name an
absent directory. The historical command validates initialized 256-byte RTME
and 192-byte RPTE symbols in that order in the final little-endian Xtensa ELF,
then invokes exactly one serial-qualified `probe-rs read` for the contiguous
448-byte range. It never
resets, flashes, authenticates, or opens the serial port. The owner-only output
contains the raw range, exact splits, human and JSON decodes, and a manifest
binding the canonical ELF size/hash, symbol addresses, USB serial, debugger
arguments, resolved debugger executable size/hash, isolated launch policy, and
file hashes. The helper clears the inherited debugger environment, supplies
only an isolated `HOME`, runs from an isolated working directory with an empty
configuration, and refuses a default probe configuration beside the resolved
executable. It retains an `incomplete` marker on failure and atomically replaces
that marker with `checkpoint.complete` only after every derived artifact and
manifest is durably synced.

One 448-byte debugger read places both records on the same halted-target
boundary. The matching-even sequence rule still applies to each split record
independently. An RTME/RPTE TX-total mismatch is an explicit combined-decoder
diagnostic and requires another stable capture for acceptance; it does not
discard otherwise valid boundary evidence.

`probe-rs 0.31` resumes all cores after a successful read, but a debugger/read
failure can return before that resume and leave target state uncertain. Never
continue a trial after `capture-checkpoint` leaves an `incomplete` marker:
retain the failed directory as classified evidence, recover/reset both boards,
and restart the complete clean trial. The helper deliberately does not guess at
a second debugger resume or reset operation after failure.

For every trial, perform all of the following rather than reusing state from a
preceding direction:

1. Identity-qualify each USB serial/MAC, preserve a fresh private full-flash
   backup, and leave the board in the loader. Erase exactly
   `0x630000..0x930000` on both boards, verify the complete 3 MiB range is
   erased (the known all-`0xff` SHA-256 is
   `908b6cfc9aef496dd5ab5c5540d80c6383ed6e92f86044574c996315381bc064`),
   boot the documented one-shot schema-2 journal reprovision image, and verify
   the 1 MiB journal SHA-256
   `a6d0b254e7fee84f2f00c45f4075fdafc8f5630dc162cfaf22a72d4de0add054`.
   Reflash the exact final proof-trace HIL image with the repository's
   identity-owning helper and require its address-zero readback:

   ```sh
   IMAGE=/path/to/e290-node-runtime-measurement-proof-trace.bin
   IMAGE_SHA256="$(shasum -a 256 "$IMAGE" | cut -d ' ' -f 1)"
   PATH="$ESPFLASH_BIN_DIR:$PATH" \
   python3.13 interop/python/e290_qualification_host.py flash-merged \
     --usb-serial "$USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$TRIAL/$BOARD/flash" \
     --image "$IMAGE" --expected-image-sha256 "$IMAGE_SHA256" \
     --confirmed-radio-module HT-RA62-HF
   ```

   Set `ESPFLASH_BIN_DIR` to the directory containing the working `espflash`
   binary and bind its version in the trial manifest. Never erase below
   `0x630000`; identity, announce clock, credentials, and device configuration
   must survive. Erase and verify the exact trial range through the same
   identity-owning helper; `USB_SERIAL` must be the uppercase value from the
   board table, and every invocation needs a fresh evidence prefix:

   ```sh
   PATH="$ESPFLASH_BIN_DIR:$PATH" \
   python3.13 interop/python/e290_qualification_host.py erase-region \
     --usb-serial "$USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$TRIAL/$BOARD/erase-runtime-range" \
     --offset 0x630000 --length 0x300000
   ```

   Require the resulting `.erase-region.verified.json` to record offset
   `6488064`, length/readback size `3145728`, and readback SHA-256
   `908b6cfc9aef496dd5ab5c5540d80c6383ed6e92f86044574c996315381bc064`.
   Despite the compatibility command name, this does not invoke espflash
   4.5's native `erase-region`, because that action emits no DeviceInfo and
   cannot attribute the destructive operation to a MAC. The helper uses
   identity-reporting `write-bin` with a retained exact-length all-`0xff`
   input. Sector alignment makes the erase-before-write span exact; an already
   blank range may be checksum-skipped without weakening the required logical
   all-`0xff` state. The verified record names the operation
   `identity_bound_all_ff_write` and binds its action target, post-write target,
   retained input hash, and independent read target. The helper scans every
   readback byte for `0xff`, fails without a verified record on any mismatch,
   and leaves the target in the loader. Full-flash,
   exact-region, erase-readback, and merged-image readback destinations are
   exclusively reserved as owner-only `0600` regular files before their read
   action. `espflash` writes through a retained inherited descriptor, so a
   raced visible-path symlink cannot redirect the dump. Only a fully verified
   result becomes owner-read-only `0400`; failed output remains private
   `0600` for diagnosis.
   `flash-merged` deliberately invokes espflash with `--after no-reset`; USB
   re-enumeration alone does not reset the CPU or start the application. After
   every loader-mode operation in this step is complete, use step 8 of the
   [connected-board flash procedure](#connected-board-identity-and-future-flash-procedure)
   to clear only the retained force-download bit and request the RTC-watchdog
   full-chip reset. Pressing the board's physical EN reset button is the
   accepted recovery alternative. Then rediscover the application port; do not
   reuse the loader callout name as an identity.
2. Allocate a new 383-byte payload and a new 16-byte idempotency key. Retain
   their exact hex plus SHA-256 values; no payload or key may be reused across
   trials. Create one directory such as `trial-01-b-to-a`, with sender
   `pre-route` and per-board `baseline`, `post-submit-5s-if-active`, and
   `terminal` 448-byte captures, split RTME/RPTE binaries, and decoded JSON.
   Record the ELF/image hashes, symbol addresses, board bindings, destination,
   command start/end times, and terminal API result in that directory.
3. Boot the sender first, allow its own boot TX to settle, and capture a stable
   sender `pre-route` checkpoint. Boot the receiver last and allow its fresh
   ANNOUNCE to reach the listening sender. The sender route-baseline minus
   `pre-route` delta is the readiness oracle: require exactly one logical RX,
   one successful ingress handoff, and one RNS ingress whose
   `rns_ingress.last_disposition` is `processed` and
   `rns_ingress.last_wire_packet_type` is `announce`, with zero ingress
   failures, zero counts for every non-`processed` disposition, and zero
   correlation faults. If it does not, reset both boards and restart the
   complete trial; do not accumulate another receiver ANNOUNCE on the same
   sender epoch. Before opening either
   authenticated API, take two matching-even combined baseline checkpoints on
   each board one second apart. Require `radio_tx.confirmed_success.count`,
   `radio_tx.not_confirmed_success.count`, RTME `operation.tx.count`, and RTME
   `operation.tx.timeout_count` to be unchanged between the pair. Other RTME
   fields such as uptime and scheduler maxima may advance. Do not run
   `identity-summary`, inbox status, or any other authenticated command before
   submission: that historical artifact's USB bearer permitted one handshake
   per connection.
   Raw TX counters need not be zero because either boot ANNOUNCE can contribute;
   only later terminal-minus-the-second-stable-baseline deltas are acceptance
   evidence.
4. Start exactly one `submit-and-wait` using the sender's Active credential,
   the receiver's fixed destination, that trial's payload/key, and
   `--evidence-output "$TRIAL/sender-terminal.json"`. The evidence path must
   be absent and is reserved before the serial session begins. Start the
   five-second timer only after stdout yields the explicitly flushed
   `command=submit-and-wait outcome=accepted` record with the expected device,
   session, and submission IDs; process launch or handshake start is not the
   timing boundary. While that command remains active, capture both records
   about five seconds after the accepted marker. If a terminal result wins
   that race, capture it immediately and mark the five-second checkpoint
   `not-applicable`; never delay a terminal capture to reach the clock time.
   Capture both again immediately after `Delivered`, `delivery-timeout`, or
   another terminal result. A debugger capture halts its target, so retain the
   marker/capture times and do not treat resulting loop-gap maxima as ordinary
   RF timing.
5. Decode only matching-even snapshots; recapture any odd, mismatched, or torn
   record rather than accepting a partial checkpoint. After the terminal
   captures, reset and re-enumerate the receiver, then authenticate separately
   to peek with an absent payload path plus
   `--evidence-output "$TRIAL/receiver-peek.json"`, and verify the exact durable
   destination/payload. This post-trial
   read must not be mixed into the proof-timing capture window.

Evaluate terminal-minus-baseline deltas, not raw totals. Subtract only
monotonic `.count` fields. Evaluate `last_*`, `tag.*`, and `flags.*` fields as
the value or transition at that checkpoint: enum/tag values and timestamps are
not arithmetic counters. Use this acceptance matrix, where `G` is the
receiver's terminal `tag.generated` value:

| Boundary / decoder keys | Receiver | Sender after `Delivered` | Sender after `delivery-timeout` |
| --- | --- | --- | --- |
| Logical handoff: `logical_rx.completed.count`, `ingress.enqueue.count`, `ingress.fail.count` | `+1`, `+1`, `+0` for DATA | `+1`, `+1`, `+0` for PROOF | Deltas locate whether any proof reached reassembly or handoff; `ingress.fail` remains `+0` |
| RNS: `rns_ingress.count`, `rns_ingress.last_disposition`, `rns_ingress.last_wire_packet_type`, and `disposition.processed.count`, `disposition.native_duplicate.count`, `disposition.native_invalid.count`, `disposition.no_observable_outcome.count`, `disposition.rejected.count` | `+1`, last values `processed` / `data`; every other disposition `+0` | `+1`, last values `processed` / `proof`; every other disposition `+0` | Deltas locate RNS rejection; every clean-path disposition other than `processed` is `+0` |
| Proof action: `proof.generated.count`, `rns_ingress.last_emitted_packets`, `tag.generated`, `flags.generated_tag_present`, `flags.generated_tags_consistent`, `flags.input_inconsistent` | `+1`, last emitted `1`, present/consistent tag `G`, input-consistent | Generated-proof delta exactly `+0`; input-consistent | Generated-proof delta exactly `+0`; input-consistent; receiver zero after processed DATA identifies generation/action failure |
| TX wrapper: `radio_tx.confirmed_success.count`, `radio_tx.not_confirmed_success.count`, RTME `operation.tx.count`, `operation.tx.timeout_count` | Confirmed success `+1`, not-confirmed `+0`, wrapper operation `+1`, timeout `+0` for the sole post-baseline proof action | Confirmed success `+1`, not-confirmed `+0`, wrapper operation `+1`, timeout `+0` for maximum DATA | Same sender TX gate; receiver outcome distinguishes confirmed logical TX completion from the earlier proof-action boundary |
| Receipt terminal: `receipt.delivered.count`, `receipt.timeout.count`, `tag.delivered`, `tag.timeout`, and the matching `flags.delivered_tag_present`, `flags.delivered_tags_consistent`, `flags.timeout_tag_present`, `flags.timeout_tags_consistent` | Both deltas exactly `+0` | Delivered `+1` with present/consistent tag `G`; timeout `+0` | Timeout `+1` with present/consistent tag `G`; Delivered `+0` |
| Inbox: `inbox.commit.count`, `inbox.commit.last_start_ms`, `inbox.commit.last_end_ms`, `flags.inbox_commit_in_progress`, `flags.inbox_commit_order_consistent`, authenticated peek | `+1`, paired/order-consistent and not in progress; exact durable destination/payload | Commit delta `+0` | Commit delta `+0` |

The dispatch report carries no proof tag, so TX attribution depends on this
fixture's exactly one receiver post-baseline emitted action, exactly one TX
wrapper invocation, and absence of other ordinary work. Under those gates,
receiver proof generation plus confirmed TX and sender logical-RX delta zero
isolates the remaining uncertainty to over-air loss or a receive-blind
interval. Sender logical RX without RNS processing moves it to handoff/RNS,
while a correlation-fault delta identifies the terminal correlation boundary.
`not_confirmed_success` deliberately combines pre-radio rejection, confirmed
operation faults, and cancellation/cleanup ambiguity; it does not prove that
zero RF energy or frames occurred. At every post-baseline checkpoint, each
board's RTME `operation.tx.count` delta must equal the sum of its two RPTE
TX-outcome deltas; otherwise treat the capture or instrumentation as
inconsistent.

Any saturation, input/tag inconsistency, torn snapshot, ingress-failure or
correlation-fault delta, non-`processed` disposition, not-confirmed TX or radio
watchdog, unexpected error, payload mismatch, extra receipt terminal, or tag mismatch
prevents a clean-path qualification claim, but retain it as classified failure
evidence. Enqueue deferrals and action pressure are retry observations and may
increment more than once; retain their counts and times but do not require a
one-transition total. Likewise, the inbox counter brackets a Ready-gate
admission attempt, not only successful physical programming, so the durable
peek remains the success oracle.

The runtime watermark reports the deepest painted word observed modified on
the CPU0/main-executor stack. It is not a historical minimum-stack-pointer
proof: a frame can reserve untouched padding below its lowest write.
Qualification must retain compiler-emitted frame evidence plus interrupt and
nesting headroom. The earlier two-board run's 72,212-byte painted margin became
a deliberately conservative 19,460 bytes after subtracting the 52,752-byte
maximum frame; the predecessor's one-board diagnostic baseline updates those
values to 72,020/19,268 after the exact 192-byte linked-RAM cost. Neither pair
is a universal stack guarantee. The pre-PSRAM policy carried those values to
66,068/12,388 after 5,952 bytes of exact post-proof linked internal-RAM growth.
The post-offload linked-only interim policy carried them to 63,436/9,756 under
the 53,680-byte frame ceiling. The historical pre-LXTE Stage 5 placement
checkpoint supersedes that policy with 57,716 powered raw bytes and 4,036 bytes
under the ceiling (4,052
under the actual 53,664-byte frame). The second deduction remains deliberately
pessimistic because the painter already covers the constructor invocation.
This is an internal CPU0/main-executor task-stack bound,
not a no-PSRAM board-support ceiling.
Current source also re-reads the innermost stack pointer immediately before
each volatile word access and reports an address at or above it as changed,
so scanner safety does not depend on Rust honoring an inlining request. The
retained two-board traffic HIL predates that source guard, but its exact linked
disassembly has the complete scanner loop in the 32-byte caller frame, uses
that live stack pointer as the exclusive read limit, and makes no call from the
scan loop. The post-run hardening therefore does not invalidate the retained
measurement. The retained post-run proof-trace image includes the guard and
passes the static ELF gate; the predecessor has the one-board powered baseline
above. That retained diagnostic image still needs exact powered readback plus
its two-board traffic workload.

`node_identity`, `announce_clock`, and `api_credentials` use ESP-IDF's standard
`data,undefined` subtype. All three have application-owned formats; the
credential range is checked, boot-mounted/recovered, and retained. Explicit
initialization and ADR 0010 live pairing are routed through the resident owner;
minimal single-flight authenticated USB session/API serving is powered-qualified
through identity, durable submission, sequential status, peer proof, and a
post-re-enumeration terminal status read.
`device_config` retains the standard NVS subtype while it is unwired; the
application-owned journal and wired raw-RNS inbox retain `data,undefined`.
The append-only LXMF store also retains `data,undefined`; all labels and ranges
remain distinct. The complete `message_store` range is
bound to the physical device ID, absolute offset, length, and inbox physical
format version 1. The separate `lxmf_store` is bound to the same device ID and
its own exact range/format version. Numeric custom subtypes are only valid with
custom partition types in the image tooling and are not used here.

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

The announce lane attempts at most one local destination per scheduling event.
It starts with the primary destination, then attempts `lxmf.delivery` eight
seconds later when the dedicated LXMF store is mounted and no clean service
fault has disabled it. Two complete retry pairs follow: the first primary is
delayed by `13 + (u32_le(primary[0..4]) mod 23)` seconds after the initial pair,
and the second by another 30 seconds after that pair. Steady pairs then recur
every 30 minutes. For the qualified A/B identities, this produces primary phases
at 26 and 43 seconds from boot; including Rete's native retransmissions at five
seconds, every post-initial nominal emission opportunity remains at least three
seconds apart. A queue or native admission rejection retains the same destination
behind a one-second retry deadline without consuming bootstrap budget. A pending
ambiguous `StoreFaultHold` does not currently
suppress the service announce. Each successfully queued destination consumes
its own durable-clock ordinal, and its independent ordinary-action flush moves
only that event toward every eligible packet interface. The exact four-byte LXMF
application data is MessagePack `[nil, nil, []]`: unnamed, no stamp requirement,
and no optional functionality advertised. An unmounted or clean-fault-disabled
LXMF service is not advertised; the primary node destination continues
independently.

Two discovery limitations exist in pinned Rete `90570ca`. Its native handling
rebroadcasts a path request for a registered local secondary destination rather
than returning that destination's PATH_RESPONSE. The current product wrapper
temporarily detects and answers that request on the source interface and
suppresses its own response from rebroadcast; this is qualified for the
one-interface POC but lacks the per-interface pending-forwarding state needed by
the eventual multi-transport router. Announce retransmission also computes a
value modulo 500 ms and then rounds it to a whole second using a threshold that
cannot be reached, so its effective jitter is zero. The product scheduler
applies a stable identity-derived bootstrap phase instead of relying on that
jitter. Both behaviors should ultimately move into the owned Rete layer before
simultaneous LoRa, Wi-Fi, and BLE routing is enabled.

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
the checked 1 MiB region and retains at most 128 accepted historical
submissions in the current external-PSRAM profile before making any recovery
mutation. This bounded, non-reclaiming limit remains below the physical
journal's 162-acceptance lifetime and is not long-term product capacity. It
drives recovery
through `RecoveryStep::Complete`,
then moves into `ProductStorageCoordinator` with the sole physical flash owner.
The node task drives that resident runtime in its fifth fair lane; each physical
operation creates a short-lived `BoundJournal` over the exact partition and
releases the borrow afterward. This preserves one flash authority without
locking future configuration, final message-store, and OTA work into a permanent
journal-only borrow.

The same coordinator read-only mounts the complete 2 MiB `message_store` before
node service. ADR 0011 format 1 permits either an entirely erased range or one
canonical 576-byte committed item at relative offset zero with every later byte
still erased. It stores the exact 16-byte local destination and at most 383
decrypted payload bytes in plaintext. The node moves transport-neutral Rete
`DataReceived` output into a fixed candidate, retains one candidate while a
credential or journal mutation owns flash, and otherwise commits or drops newest
without erase, overwrite, acknowledgement, deletion, or reclamation. Mount or
admission failure disables only inbox capability for that boot and leaves
ordinary Reticulum routing/proof work available.

The coordinator now also read-only mounts the separate 2 MiB `lxmf_store`
through its caller-owned 512-slot PSRAM index and retains the mounted owner for
the boot. Its pending-mutation fact participates in every credential, journal,
and raw-inbox physical-mutation gate. The inverse LXMF gate admits its own
pending state to the store so only the store can structurally validate an exact
retry; credential or journal ownership still defers it. Mount success now also
enables the derived `lxmf.delivery` destination, signed opportunistic DATA
admission, durable-ingress call, per-destination `Retain` policy, sixteen-slot
delayed-proof owner, and ordinary-supervisor ready-proof drain. Local Links are
disabled until a bounded Link/Resource owner exists. The current A-to-B
confirmation powers this exact opportunistic new-commit-before-proof path; it
does not qualify replay, Link/Resource, propagation, or a client mailbox.

Journal mount, unsupported history, or recovery failure is isolated because it
occurs during boot before a durability-gated DATA owner can exist: the
coordinator retains the flash backend with no runtime, local durable admission
remains closed, and the LoRa node/radio tasks still start in route-only mode.
The accepted-history cap is 128 in current source. Earlier powered artifacts
used smaller profiles and do not claim a 128-message hardware fill. The minimal
authenticated USB edge now has powered initialize/pair/reboot/capabilities/identity evidence
plus one durable submission whose LoRa DATA/proof terminal state survived USB
re-enumeration. The API 1.2 image separately has powered exact inbox commit,
full-range readback, authenticated status/peek, hard-reset survival, and
drop-newest evidence under the deliberately one-entry qualification format. The
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

The resident `ProductStorageCoordinator` exposes one operation-scoped owner
implementing the target-safe device-API `SubmissionPort`, raw-RNS
`InboundMailboxPort`, committed-message `LxmfInboxPort`, and source-free
`LxmfComposePort`. These are semantic seams rather than physical-store handles;
the raw inbox remains a one-entry qualification format and the submission
runtime's 128-entry non-reclaiming cap is not product capacity. Portable
framing and job handoff enter the permanent graph through a
static depth-one
authenticated request/reply channel. The node endpoint decodes the logical
request, revalidates its opaque grant against the resident current authority,
and calls the adapter synchronously through that credential-disjoint owner. The
adapter invokes only the semantic method selected by the request. Missing,
revoked, replaced, or generation-mismatched credentials
produce a generic authentication-required response without port I/O or fallback.
Reply pressure retains the exact owner, while malformed logical CBOR is a
terminal retained fault rather than a redispatch candidate. The USB endpoint
now runs a fixed-capacity session manager with one active session and one
request in flight. A canonical ClientHello replaces an idle established
session on the same connection with a fresh session epoch; replacement is
dropped while a request or reply owner exists. A malformed replacement or
other session fault still fails terminally until USB reset. It intentionally
has no resumption, protocol retry, close record, encryption, rate/attempt
policy, or concurrency yet. Its admission and request/reply handoffs remain
bearer-neutral. The opt-in BLE adapter now uses those same boundaries with an
explicitly enabled and bounded suite-3 session; the Wi-Fi binding remains to be
powered-qualified. The separate USB
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

All four physical-store bindings name the device with the domain-separated
16-byte value `"e290-flash" || eFuse base MAC`. The credential view additionally fixes
absolute offset `0x614000`, length `0x2000`, and credential physical layout
version 1. The journal view fixes offset `0x630000`, length `0x100000`, and
journal physical layout version 1. The inbox view fixes offset `0x730000`, length
`0x200000`, and inbox physical format version 1. The LXMF view fixes offset
`0x930000`, length `0x200000`, and LXMF physical format version 1. Each store
validates its exact values and view capacity/alignment before I/O; every later
borrowed operation must match its retained binding exactly.

## Software composition and build gates

From the workspace root:

```sh
cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node --lib
cargo +stable clippy --locked \
  -p reticulum-heltec-vision-master-e290-node --lib -- -D warnings
cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node --lib \
  --features rns-inbox-commit-fault-hil
cargo +stable clippy --locked \
  -p reticulum-heltec-vision-master-e290-node --lib --tests \
  --features rns-inbox-commit-fault-hil -- -D warnings
cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node --lib \
  --features runtime-measurement-hil
cargo +stable clippy --locked \
  -p reticulum-heltec-vision-master-e290-node --lib --tests \
  --features runtime-measurement-hil -- -D warnings
cargo +stable test --locked -p xtask
RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked \
  -p reticulum-heltec-vision-master-e290-node --lib --no-deps
cargo +stable run --locked -p xtask -- graph-policy

source ~/export-esp.sh
CARGO_TARGET_DIR=target/e290-default \
RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --target xtensa-esp32s3-none-elf -- -D warnings
CARGO_TARGET_DIR=target/e290-default \
RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --target xtensa-esp32s3-none-elf
CARGO_TARGET_DIR=target/e290-inbox-commit-fault-hil \
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --no-default-features --features rns-inbox-commit-fault-hil \
  --target xtensa-esp32s3-none-elf
CARGO_TARGET_DIR=target/e290-inbox-commit-fault-hil \
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --no-default-features --features rns-inbox-commit-fault-hil \
  --target xtensa-esp32s3-none-elf -- -D warnings
CARGO_TARGET_DIR=target/e290-runtime-measurement-hil \
RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --no-default-features --features runtime-measurement-hil \
  --target xtensa-esp32s3-none-elf -- -D warnings
CARGO_TARGET_DIR=target/e290-runtime-measurement-hil \
RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --no-default-features --features runtime-measurement-hil \
  --target xtensa-esp32s3-none-elf
cargo +stable run --locked -p xtask -- e290-runtime-measurement inspect-elf \
  --default-elf target/e290-default/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node \
  --hil-elf target/e290-runtime-measurement-hil/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node
```

The HIL ELF is isolated at
`target/e290-inbox-commit-fault-hil/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node`.
Package it only under a HIL-specific image name. The ordinary `e290-node.bin`
procedure below must continue to use the isolated default ELF under
`target/e290-default`.
The runtime-measurement ELF is separately isolated under
`target/e290-runtime-measurement-hil`; it is likewise never the source for the
ordinary product image.

The build script rejects an unreviewed `esp-rtos` main-stack implementation and
links `linkall.x`. Debug Xtensa builds are compile-time rejected.
The three exceptional development/HIL features are pairwise mutually exclusive,
so an E290 `--all-features` build is invalid. `graph-policy` separately inspects
the ordinary and exceptional roots. The commit-fault dependency tail must be
identical to default; the measurement tail may add only the reviewed
`esp-alloc/alloc-hooks` feature. The default ELF must contain neither fault nor
measurement wrappers, hooks, markers or evidence identifiers.
The default, commit-fault, and runtime-measurement validation profiles cover
policy/product/credential-boot/credential-runtime/USB-control/live-routing,
including the source-order
regressions, every canonical empty-initialization byte cut, adversarial media changes between
mount and classification, off-trajectory media, and classifier failure phases,
authenticated semantic-port isolation, and cross-layer composition. The
capacity path proves unauthenticated and permission-denied requests cause zero
NOR writes, 128 authenticated novel acceptances succeed, a 129th novel request
reaches capacity without a write, and exact replay remains available
without mutation at capacity. The routing path proves the durable `Preparing`
barrier precedes node ownership,
drives the real `NodeInterfaceSupervisor`, exact E290 LoRa policy, and real
dispatcher through one DATA transmit, persists the exact authorized frame,
echoes it durably, and releases the completion. Delivery timeout, owner status,
foreign-principal `NotFound`, and remount of the durable final state complete the
path. The fault test injects a permanent wrong-binding error after frame
exposure with an ordinary announce queued behind it; the result is
`ActiveOwnerFailStopped`, no acknowledgement or completion, every owner retained,
and no later host-radio TX or RX. Focused durability-policy tests cover retry,
route-only degradation, pending durable acknowledgement, sticky fail-stop, and
the request-after-disable race. Credential-runtime tests additionally
cover both initialization trajectories, fresh binding and identity checks,
forward-only media movement, ambiguous backend/readback retention, disconnect
ownership, policy completion, and fail-closed noncanonical states. Cross-store
tests cover both retained journal owners, initialization before and
after physical I/O, stable credential states, inbox exclusion, and the distinct
deferred versus unavailable result. The USB-control additions cover stable-time active-low
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

The generic 128-entry fake runtime exceeds Rust's default host test-thread
stack because that fixture owns the aggregate by value. The qualified 176-test
host run is:

```sh
RUST_MIN_STACK=16777216 \
  cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node -- --test-threads=1
```

This host-fixture setting is not evidence of target stack placement. The target
constructs the resident runtime in place in external PSRAM; powered fill and
pressure qualification of all 128 entries remain open.

Host-client gates cover parsing, deadlines, single-open progression, terminal
and ambiguity behavior, owner-only persistence, public identity and LXMF
metadata formatting, non-overwriting payload/wire output, sequential request
IDs, version policy, coalesced records, authenticated terminal binding, and
submission-input non-disclosure. Portable Rete integration, inbox-store,
released-vector, direct-Link, LRRTT, channel-retry, keepalive, schema-2
lifecycle/candidate, and deterministic three-node A--B--C relay lanes remain
separate gates. They are not powered or live-Python multi-hop qualification.
The preceding
`8b5d652` nested Rete selected validation set separately passed 635 tests: 271
transport (174
library plus 97 integration: 9 computed-vector, 43 forwarding, 40
Link-integration and 5 path-request), 137 stack (136 library and one
integration), 143 LXMF library and 84 daemon library tests. The four library
targets totaled 537 tests; the 97 transport and one stack integration tests
brought that named set to 635. It is not a count of every
nested workspace test target.
The focused current Rete regression lane remains separate and does not relabel
that broader historical selected set. Focused xtask tests freeze the measurement decoder's
CLI, exact individual/combined ABI rendering, torn/header/sentinel/invariant
rejection, input-file behavior, one-read checkpoint capture, strict final-ELF
parsing, compiler-frame inventory, linker-stack derivation and the reviewed
static bounds.

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
the shared framed initialization, live-pairing, and authenticated-session records
have the sole application-owned byte stream.
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
ceiling. The E290-specific gate above now rejects a larger frame, a smaller
linked usable stack, a changed guard offset or missing/unresolved compiler
evidence in either final ELF. Broader powered stack qualification remains
required; the bounded HIL baseline above is not closure.

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

The historical powered authenticated-node-foundation release links at 611,479 bytes text,
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

The historical Active-generation-binding source linked at 641,419 bytes text, 3,596
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
- `rns-inbox-status` for the five bounded qualification-state scalars;
- `rns-inbox-peek --output <path> [--evidence-output <absent-json>]` for an
  owner-only, non-overwriting copy of the exact retained payload plus optional
  authenticated-result evidence containing its destination and metadata;
- `lxmf-list`, which walks committed messages in physical commit order and
  prints authenticated handles, message IDs, endpoints, lengths, timestamp
  bits, and full-wire digests without printing title or content bytes;
- `lxmf-read --handle <nonzero-u64> --output <absent-path>`, which streams the
  exact normalized wire into a private non-overwriting file, verifies its
  complete SHA-256, and cross-checks parsed LXMF metadata when host size permits;
- `lxmf-send`, which source-free composes, signs, and durably accepts one basic
  opportunistic message; and
- `lxmf-send-and-wait`, which performs that same acceptance and then polls the
  returned submission ID to `Delivered` or a terminal failure;
- `submission-status` with `--submission-id`;
- `submit-rns-data` with destination, payload, and idempotency key; and
- `submit-and-wait [--evidence-output <absent-json>]`, which submits once and
  polls every 500 ms over the same authenticated session until `Delivered` or
  a terminal failure, with optional device-terminal evidence. After the device
  accepts and assigns the submission, it first writes and flushes
  `command=submit-and-wait outcome=accepted device_id=<32-hex>
  session_id=<32-hex> submission_id=<u64>` to stdout, before its first status
  delay or poll. Pre-acceptance failures and device rejections emit no accepted
  marker; the later terminal output and errors retain their existing forms.

Read the public primary destination:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  identity-summary
```

For LXMF, use the peer's reported `lxmf_delivery_destination`, never its primary
destination. After entering ordinary-session mode (including the required reset
after pairing), send one binary title/content pair and wait for the Reticulum
delivery proof:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  --timeout-ms 120000 \
  lxmf-send-and-wait \
  --destination-hash <peer-lxmf-delivery-32-hex> \
  --title-hex 68656c6c6f \
  --content-hex 66726f6d2d65323930 \
  --timestamp-ms <1-to-8796093022207999> \
  --idempotency-key <32-hex>
```

Title and content are binary; empty values are valid as `--title-hex ''` and
`--content-hex ''`. Each field is structurally limited to 295 bytes, but the
448-byte encoded request and product composition can reject a smaller combined
pair. The current E290 durable intent accepts a selected carrier only through
383 bytes even though Python's dedicated opportunistic carrier can reach 391;
an otherwise valid 384--391-byte carrier is rejected before journal acceptance.
The timestamp must be exactly
`1..=8_796_093_022_207_999`. If timestamp or idempotency key is omitted, the
host samples its current millisecond clock or generates a random key once. It
prints and flushes the exact timestamp, key, destination, and field lengths
before `0xf006`, so retain that record and the exact title/content bytes. An
ambiguous retry must reuse every one of those values. `lxmf-send` stops after
durable acceptance; `lxmf-send-and-wait` defaults to a 45-second absolute
deadline and keeps the same authenticated session open while polling.

On a receiver, list committed message metadata and then use a second process to
stream one handle to a new private file. Current firmware replaces the first
idle session, so no USB reset is required between these successful commands:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  lxmf-list

cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  lxmf-read --handle <nonzero-u64> \
  --output /secure/message.lxmf
```

`lxmf-list` prints authenticated metadata but no title/content bytes.
`lxmf-read` reserves its output before serial I/O with create-new owner-only
permissions, streams 416-byte-or-smaller authenticated chunks, verifies the
summary's complete wire digest, read-verifies the file, and never prints the raw
message. The file is still plaintext normalized LXMF and can contain message
content and metadata; protect it like the raw inbox export. The diagnostic CLI
also receives title/content hex in process arguments, where shell history or a
same-host process listing can expose it. The authenticated USB session provides
record integrity but currently no confidentiality. Do not use this POC client
for sensitive messages. Any authenticated principal can currently list/read the
global LXMF store; there is no mailbox ACL or per-principal filtering. These
LXMF commands also have no structured `--evidence-output` sidecar, so a powered
proof must retain authenticated stdout, the private read file, exact retry
inputs, and hashes manually.

Submit DATA and wait up to the default 45-second overall deadline:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  --destination-hash <32-lowercase-hex> \
  --payload-hex <0-to-766-hex> \
  --idempotency-key <32-hex> \
  submit-and-wait \
  --evidence-output /secure/submit-terminal.json
```

Observe the raw-RNS qualification inbox without consuming it:

```sh
cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  rns-inbox-status

cargo +stable run --locked -p xtask -- e290-authenticated-usb \
  --port /dev/cu.usbmodemXXXX \
  --state-file /secure/new-e290-pairing.key \
  rns-inbox-peek \
  --output /secure/inbox-item.bin \
  --evidence-output /secure/inbox-item.json
```

Peek refuses to replace an existing output file, creates a private file, syncs
it, and prints only item metadata. The binary file contains only the plaintext
payload; its destination is printed as metadata and retained in the optional
JSON sidecar. Both artifacts must be handled accordingly. Empty inbox returns
a clear not-found result without creating output.

The optional evidence sidecars are private, create-new, durably synced JSON.
They bind the authenticated device and session to either the device-reported
submission terminal (including exact delivered packet length/hash or terminal
failure reason) or the independently hashed inbox payload and retained item
metadata. Output paths are reserved before serial I/O, so an occupied or
aliased path cannot consume a one-shot session and then produce only half of a
result. Host transport/authentication errors, cancellation, and the host
deadline do not masquerade as device-terminal evidence; ordinary errors remove
empty reservations, while a process crash can leave an unmistakable empty
reservation rather than overwrite prior evidence.

The current secret-bearing payload/evidence writer requires Unix permission
semantics and fails before credential or serial I/O on other hosts; a future
cross-platform client must provide an equivalent private-file/ACL guarantee
before enabling these exports.

The command accepts only the canonical Active state-file format, verifies the
device ID and credential generation during the handshake, preserves coalesced
records, accepts any response minor within API major 1, uses one absolute
deadline, and prints only non-secret scalar results. `submit-and-wait` uses
strictly increasing request IDs and treats only status `Internal` as a transient
poll result; `Failed`, `Cancelled`, any other API error, or deadline expiry is a
failure. It has no resumption, close, encryption, rate-policy, repeated-
handshake retry, or concurrent-request behavior. Each one-shot command drops
its restored client session while firmware remains established. A later process
can replace that idle session with a canonical ClientHello and a fresh session
epoch on the same USB connection; it cannot displace an in-flight request or
reply, and a session fault still requires reset/re-enumeration.
`submit-and-wait`, `lxmf-send-and-wait`, `lxmf-list`, and `lxmf-read`
deliberately retain one session for their own sequential status, enumeration,
or chunk requests. Do not infer whether a credential identifier exists from
failure text.

Historical one-entry evidence below and the later 16-entry proof profile are
revision-bound. Current source retains 128 accepted submissions, rejects a
129th novel request without mutation, and preserves exact replay at capacity;
the journal remains non-reclaiming with a 162-acceptance lifetime ceiling, and
this still is not the intended long-term product capacity. No historical
16-entry artifact is evidence of a powered 128-entry fill.

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
consumed the plaintext. Current API 1.2 source now projects inbound DATA into
the separate raw-RNS qualification store described below; that later result
must not be retroactively inferred from this API 1.1 proof.

The first powered end-to-end attempt usefully failed closed during post-reboot
authentication with an expected generation 2 versus observed generation 3
mismatch: the durable store assigned generation 3 to the committed Active
record after the Pending record at generation 2, while the host retained the
Pending generation. Pairing proof suite 2 fixes that boundary by authenticating
the actual committed Active generation in the activation confirmation and
atomically persisting that exact generation on the host. The successful
rebooted proof above exercises that fix; it does not assume that Active is
Pending plus one.

### Powered API 1.2 durable raw-RNS inbox qualification

On 2026-07-18, the final post-audit API 1.2 release reported 655,511 bytes text,
3,676 bytes initialized data, 469,152 bytes BSS/reservations, and 1,128,339 bytes
total by GNU size. Its 696,416-byte application occupied 696,416 of 6,291,456
factory bytes and was packaged as a 761,952-byte merged image with SHA-256
`ba10b04408368c3f5cbcc91f5d514f454595a7812986764c1e95ef528cc71f03`.
Both boards received that exact image and both address-zero readbacks matched.
For this clean qualification only, the operator deliberately erased the sender's
complete `message_store` once; the post-audit image then mounted it with depth 0
and `durable=true` before traffic. This is setup evidence, not a firmware erase
or product-reset behavior.

The paired sender `ac:a7:04:e1:3e:88` held Active generation 5; receiver
`ac:a7:04:e1:3f:88` held Active generation 3.

The receiver's qualification-cap journal was explicitly reprovisioned through
the documented journal-only erased schema-2 image, after which the ordinary
post-audit image was restored and read back. Four path-acquisition attempts
failed `no-path` before any sender inbox write. Those attempts exposed a tooling
detail: macOS USB-only re-enumeration replaces the USB session but neither
reboots the firmware nor reschedules its one-shot boot ANNOUNCE. One subsequent
reverse-direction delivery attempt timed out and produced no inbox commit. An
explicit `espflash` DTR hard-reset cycle restarted boot ANNOUNCE scheduling, and
the following attempt succeeded. The receiver journal-only reprovision was
repeated as required by its one-submission qualification cap. Before the later
overflow packet, only that receiver journal reprovision/ordinary-image restore
was repeated. The sender's `message_store` remained untouched from the maximum
commit through hard reset, overflow, final status/peek, and final raw dump. This
is controlled qualification workflow evidence, not product capacity, an
automatic retry claim, or evidence that USB re-enumeration restarts the node.

The receiver then submitted the maximum 383-byte payload to the sender. It
reached `Delivered` as a 483-byte RNS packet with encoded-byte SHA-256
`695a2e78ed379378ea9481f1fa37e7e20596237cc1f545470c4d251613b9586a`.
Authenticated sender status reported depth 1, capacity 1,
`dropped_since_boot=0`, maximum payload 383, and `durable=true`. Peek returned
destination `c99e8ff1ec8629e4e1290e14462ae8af` and the exact payload whose SHA-256
was `38796658031a3eb2fbec2185daa31f8e3d2cc6a998278ee1fc1cc39b28541a4c`.
The exact 576-byte record had SHA-256
`58ce28dd27476360dd297fbf3e250d43c72935c90be6d88e32495b6130988d58`;
independent domain-separated digest recomputation matched the stored
`9f2910bdd42de13c5fc45e1e6f6574ea6aa143d9054c774c7f5c62d3b68ecdec`.
The flash read induced a hard reset; a fresh authenticated session afterward
returned the identical item through peek.

Finally, a second valid 33-byte DATA payload reached `Delivered` as a 147-byte
packet with encoded-byte SHA-256
`1bf74485c87a38791d40366095085908c1df189e97814b6d68be9357f2707079`.
Sender status remained depth 1 and reported `dropped_since_boot=1`; peek still
returned item 1 with the same destination and maximum-payload digest. The final
exact 2,097,152-byte `message_store` dump had SHA-256
`f50dab680d46ef20cd875eff778296a3b92f9d7eef34684f29eedc10b468d724`.
Its first 576 bytes exactly matched the record above, and bytes 576 through the
end contained zero non-`0xff` bytes. This closes the bounded commit/readback,
hard-reset survival, and drop-newest proof. A final raw-read-induced hard reset
remounted the occupied record at depth 1 with `dropped_since_boot=0`, confirming
that the drop counter is boot-local while the item is durable.

That happy-path run did not itself qualify a controlled power cut, corrupt-media
mount, or admission fault. The two bounded software-fault subsets are addressed
below. Electrical claim/body/commit cuts, partial-body and partial-commit media,
backend error-after-write behavior, mount/commit timing, internal-RAM or PSRAM
high-water, watchdog/radio-deadline effects, endurance, encryption, LXMF, and a
full mailbox remain open.

### Powered cold-mount fault matrix

On 2026-07-19, four deterministic, board-bound 2 MiB `message_store` fixtures
were installed and exercised against the exact ordinary 761,952-byte API 1.2
image above, SHA-256
`ba10b04408368c3f5cbcc91f5d514f454595a7812986764c1e95ef528cc71f03`:

| Mounted case | Fixture SHA-256 | Direct peer packet after quarantine |
| --- | --- | --- |
| interrupted claim on `3e:88` | `4b9e6dad1415850588c001b17053e893ab1316aaa1b6d584082170d049f871f0` | 147 bytes; `dc9877ac09c335c696b3141d87e92e27bc92e482b7fcb69433bf2225ba4e3fcf` |
| exact pre-commit record on `3e:88` | `a8a8d40f63a69c7e3df59f4af1960f241f464566a5ae9251c12209eb3334c66a` | 131 bytes; `6bd4f9f009b8598b1edbf9b2364695b1e39e01272e17fa64622cd34ef7f341b8` |
| invalid digest on `3e:88` | `bb24e892d435a0b6888cc16f8733f096015a36f0f19dcd8a22e0978602e55ad5` | 131 bytes; `85dfe3219dc0463c60577d10eb5cda634d2c2c3060d6266bcd861e6dad4c4b95` |
| valid `3e:88` record mounted on `3f:88` | `dee21d3c72a914ac00627c49a119631999dc9e986ce18897b9a171254c79561b` | 131 bytes; `3af3ce5a327a303e37e2b90ad80d6186e0ceda496da31da56905ec5d78d2d874` |

For every case, authenticated capabilities reported inbox availability and
maximum payload as `0/0`; status and peek returned `CapabilityUnavailable`
(code 7, operations 61442 and 61443); peek created no output; one fresh direct
peer DATA packet was decrypted and answered with a valid proof so its sender
reached `Delivered`; and a complete post-traffic dump remained byte-identical
to the injected fixture. This proves bounded node/RF continuation and
non-mutation after quarantine for one controlled packet per case. It is not
sustained routing, forwarding, multi-hop, or soak evidence.

The fixture generator uses the real public store mount/accept path, emits only
an owner-restricted create-new file, and prints mode, length, and SHA-256:

```sh
cargo +stable run --locked -p xtask -- e290-rns-inbox-fixture \
  --output /secure/absent-message-store.bin \
  --source-mac aca704e13e88 \
  interrupted-commit
```

The four modes are `interrupted-claim`, `interrupted-commit`, `invalid-digest`,
and `committed`. `committed` is healthy only for the source MAC used to generate
it; installing that image on the other board creates the binding-mismatch case.
The all-erased 2 MiB setup image had SHA-256
`4bda3a28f4ffe603c0ec1258c0034d65a1a0d35ab7bd523a834608adabf03cc5`.
The tool does not access hardware, and ordinary firmware did not erase, repair,
or rewrite any injected fixture. These preprogrammed states model stable cut
trajectories; they are not live electrical power cuts.

### Powered same-boot commit-fault HIL

The non-default `rns-inbox-commit-fault-hil` image wraps only the inbound
admission operation. It forwards the first two NOR writes, acknowledges but
suppresses write call three, and forwards any later write. The store therefore
reads back an erased terminal commit marker after successfully programming and
checking claim plus body/digest. The feature is empty, opt-in, absent from the
default dependency graph below the root, and mutually exclusive with
`journal-schema2-dev-reprovision`.

The module itself compiles only for feature-enabled host tests or
feature-enabled Xtensa builds. `embedded-storage` therefore remains an Xtensa
normal dependency and a host-test dev dependency, rather than becoming part of
the ordinary host product graph.

The exact HIL build reported 656,223 bytes text, 3,716 bytes initialized data,
469,112 bytes BSS/reservations, and 1,129,051 bytes total. Its 697,136-byte
application was packaged as a 762,672-byte merged image with SHA-256
`e693afad19c2eac28d958f902c1b8148ae360a6b54abb14338195ef595515239`;
MAC `ac:a7:04:e1:3e:88` read back that exact image before the measured boot. The
corresponding ELF had SHA-256
`8409c94653cd6e10e4eca198365f6fe06f282711907a61b9271977f2da9037c6`.
Those digests identify the retained artifacts physically flashed and read back
for this run. They are not universal rebuild digests: build-directory paths can
change bytes embedded in the ELF and merged image.
Its unique 40-byte `RIAF` evidence object was bound from that ELF to RAM address
`0x3fc8bf7c`. Version 1 stores magic/version/size followed by write calls,
suppressed commits, expected commit mismatches, unexpected failures, service
disabled, and the low/high words of the boot-local drop count.

Before traffic, authenticated capabilities on `3e:88` reported inbox available
with maximum payload 383, while the ten 32-bit evidence words were
`RIAF/1/40/0/0/0/0/0/0/0`. Board `3f:88` then submitted one payload to
destination `c99e8ff1ec8629e4e1290e14462ae8af`. The 147-byte encoded DATA packet,
SHA-256
`0084ad098f2109b390d7c4568ba4a2dcd5285ac40062e55c9709665b2aebc73a`,
was decrypted by `3e:88`; its proof drove submission 1 to `Delivered`. Product
readback classified the admission as `ReadbackMismatch { stage: Commit }`,
disabled inbox service, and recorded one dropped candidate. Debugger evidence
then reported writes/suppressed/expected/unexpected/disabled/drop as
`3/1/1/0/1/1`.

A series of macOS USB-only re-enumerations established fresh one-shot
authenticated sessions without rebooting the CPU. Capabilities then reported
inbox `0/0`; separate status and peek sessions again returned code 7; peek
created no file; and the same nonzero RAM evidence remained unchanged. That
retained snapshot is the direct no-CPU-reset witness. Only after those API and
RAM observations was flash read. The exact 2,097,152-byte dump had
SHA-256
`ad6d549f73681da7453870606fb34eeabad75b387f081176103562d84e5700c7`.
Its first record had SHA-256
`acb43e7be289c5c4f822441670ce11554b6386ca3e1cfcee47907ee82c81d7f8`;
all 544 bytes at relative offsets 0 through 543 were programmed and non-`0xff`,
while every byte from relative offset 544 through partition end was `0xff`.
The first capture wrapper exited nonzero only because its post-read assertion
still expected the superseded 543-byte prefix. Direct checks of the preserved
dump established the exact length, hashes, 544-byte programmed prefix, and
fully erased tail reported here.
The deterministic interrupted-commit matrix separately qualifies cold-mount
classification of this state class; the contained rerun did not add a post-reset
API observation.

The software-suppressed write is controlled admission-fault evidence, not a
brownout, torn page program, or timing measurement. It covers one candidate and
one direct DATA/proof exchange. Partial-body/live partial-commit cuts, backend
errors after physical mutation, sustained or forwarded traffic, watchdog
effects, memory high-water, and final mailbox behavior remain open.

After capture, both boards' contiguous journal plus inbox range was erased and
verified with zero non-`0xff` bytes; each 3 MiB readback had SHA-256
`908b6cfc9aef496dd5ab5c5540d80c6383ed6e92f86044574c996315381bc064`.
Both journals were explicitly reprovisioned, then both boards received the new
default 761,952-byte image and returned exact address-zero readbacks with
SHA-256
`d26587a2506408ec40cd42facb9bb87cc9c32e79c2afd2e1ab09f0e1268641cb`.
The default ELF contains neither the fault evidence identifier nor the wrapper
string. Fresh authenticated status on each restored board reported depth 0,
capacity 1, dropped 0, maximum 383, and `durable=true`.

### Powered runtime-measurement HIL

On 2026-07-20, both E290s received the exact 768,624-byte measurement image
identified above, and both complete address-zero readbacks matched it. Each
debugger capture copied the exact 256-byte evidence object from low to high and
was accepted only after the decoder observed matching even sequence markers,
the complete version-1 header, initialized heap/stack state, intact guard, and
internally consistent counters. Baseline captures were followed by fresh board
resets before traffic so debugger pause time did not inflate the traffic-phase
loop gaps.

The six accepted baseline/phase captures across the two maximum-payload traffic
phases produced these bounded maxima/minima:

| Measurement | Bounded observation |
| --- | ---: |
| Detected PSRAM | 8,388,608 bytes |
| Registered global heap | 8,454,144 bytes |
| Maximum global-allocator use | 988 bytes |
| Minimum internal heap free | 64,548 bytes |
| External/PSRAM allocator use | 0 bytes |
| Failed allocations / unexpected errors | 0 / 0 |
| CPU0 measured stack usable | 170,480 bytes |
| Modified-word high-water | 98,268 bytes |
| Raw painted margin | 72,212 bytes |
| Margin after subtracting maximum compiler frame | 19,460 bytes |
| Composition ready | 816,645--821,073 us |
| Journal cold mount | 134,498--137,373 us |
| Inbox cold mount | 545,258--545,674 us |
| Maximum-payload inbound commit | 548,073--548,148 us |
| Worst node loop gap | 646,388 us |
| Worst radio loop gap | 1,065,406 us |
| RX / CAD / TX operation maxima | 933,255 / 38,229 / 885,258 us |
| RX / CAD / TX actor-watchdog timeouts | 0 / 0 / 0 |
| Measurement-task maximum lateness | 422,138 us |
| Measurement-task maximum work | 1,767 us |

The heap values describe the registered global allocator under this specific
LoRa/node/API workload. They do not include static reservations, DMA-visible or
interrupt-owned memory, or future client and wireless stacks. In particular,
zero observed PSRAM allocation does not imply that the full appliance should
avoid PSRAM: that measured image searched a 64 KiB internal region first, and
this workload never exhausted it. Current source raises the separate reclaimed
region to 72 KiB for esp-radio BLE controller headroom; that change is not part
of the historical measurements above. The measured image placed its
then-current 16-entry submission runtime, LXMF index, delayed proofs, and
retry/fault/proof-holder state explicitly in PSRAM. Current source expands the
runtime to 128 entries; the historical one-message high-water result does not
qualify that larger profile. Sustained pressure remains open. Resource,
NomadNet, SPA, and
future wireless buffers still require an explicit internal/external policy.
The stack observations have the modified-word/minimum-SP limitation documented
above and must retain static frame plus interrupt/nesting headroom.

Phase A submitted a maximum 383-byte payload from `3e:88` to `3f:88`. The
payload SHA-256 was
`a24462c3c6b5ef334180bd948e3696e3ea45e69c66558ef8000d03402d8ed34f`;
the 483-byte encoded Reticulum packet SHA-256 was
`5930069b8fa8274f3aac3f13cb3a108221137a60ffb6aaa69144b27bb4cd771a`.
The receiver durably committed the exact payload and the sender reached
`Delivered`. An immediate reverse submission returned `no-path`, showing that
the preceding DATA/proof exchange had not left a usable return path in this
run. It also consumed the reverse board's one-entry qualification journal.

After explicit journal-only reprovisioning and a fresh peer ANNOUNCE, phase B
submitted a different maximum payload from `3f:88` to `3e:88`, with SHA-256
`762461ea015e00e3b5b7071b8ecb720afdc51a71c7c1145563fa0893e9d3653d`.
The receiver recorded one 548,073-us inbound commit and authenticated peek
returned those exact 383 bytes from destination
`c99e8ff1ec8629e4e1290e14462ae8af`. The sender nevertheless terminated in
`delivery-timeout`. Thus the two phases establish one bounded durable inbound
commit on each board and in each direction, not bidirectional `Delivered`.
The reverse proof or terminal status was not observed by the sender before its
deadline; this product-level timeout is distinct from the zero radio-actor
watchdog counters and remains a diagnostic residual. This capture used the
preceding `f6f5fb0637d00691e09fa0105be4df902405fee4` Rete pin. The preceding
`14c7b49` host suite covers exact reverse-interface routing and proof
consumption, typed transactional reverse admission, a deterministic three-node
relayed Link/channel/proof flow, pending-Link expected-hop enforcement, and
atomic channel retry receipt replacement, plus pending-handshake MessagePack
LRRTT validation and authenticated-malformed teardown. The current `90570ca`
lifecycle/timing change passes the root validation and E290 build gates; only
a new powered run can determine whether the combined behavior
fixes this end-to-end timeout or whether another product boundary remains
faulty. A final authenticated peek on `3f:88` likewise returned phase A's exact
383-byte payload from destination `83a09ed807a0a7c631386deaa0448fb9`.

These are instrumented-workload observations, not production-image timing
guarantees. The HIL enables allocator callbacks, updates atomic evidence, and
scans roughly 170 KiB of painted stack every second. Debugger capture halts the
target even though the traffic phases were reset after their baseline captures.
Zero actor-watchdog counters therefore means no measured RX, CAD or TX watchdog
fired; it does not prove every soft deadline or a sustained, forwarded,
multi-hop, concurrent-store, low-memory or allocation-failure workload. The
measurement slice does not move persistence or routing ownership into the LoRa
actor and qualifies no other Reticulum transport. ADR 0011 target-bounds
criterion 10 now has this bounded baseline but remains open.

After capture, both boards' exact contiguous `node_journal` plus
`message_store` range was erased. Each complete 3 MiB readback contained zero
non-`0xff` bytes and had SHA-256
`908b6cfc9aef496dd5ab5c5540d80c6383ed6e92f86044574c996315381bc064`.
The one-shot journal image then provisioned an identical empty schema-2 journal
on each board; both 1 MiB readbacks had SHA-256
`a6d0b254e7fee84f2f00c45f4075fdafc8f5630dc162cfaf22a72d4de0add054`
and no programmed byte after the first 160-byte manifest area. Independent
reads proved the identity and credential/configuration ranges remained exact
matches for their pre-HIL full-flash backups. Finally, both boards received the
761,792-byte feature-free image and returned exact address-zero readbacks with
SHA-256
`77b6a48e71d62facf39bae380387397dcbc79417c05372bc31c4a240f326b066`.
Each ordinary boot advertised submission plus inbox service, and a fresh
authenticated session reported inbox depth 0, capacity 1, dropped 0, maximum
383, and `durable=true`.

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
post-write readback; the later credential-bearing API 1.1 lineage above closes
only that happy path. ROM and bootloader execution before the earliest Rust entrypoint also
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

### Powered BLE startup and CoreBluetooth proof

On 2026-07-23, a diagnostic image isolated the apparent BLE startup failure to
the controller activity budget, not the application connection limit or
registered-heap size. The pinned esp-radio 0.18
`Config::with_max_connections` name is misleading for this version: it writes
Espressif's `ble_max_act`, the total concurrent controller activity count. The
official ESP32-S3
[`CONFIG_BT_CTRL_BLE_MAX_ACT` reference](https://docs.espressif.com/projects/esp-idf/en/v5.5.3/esp32s3/api-reference/kconfig-reference.html#config-bt-ctrl-ble-max-act)
counts connections, scanning, synchronization, and advertising, and
Espressif's
[multi-connection guide](https://docs.espressif.com/projects/esp-idf/en/release-v5.1/esp32c3/api-guides/ble/ble-multiconnection-guide.html)
defines the required value as maximum connections plus simultaneous
advertising, scanning, and periodic-synchronization instances. This peripheral
still admits exactly one application connection, but its connectable
advertisement and eventual ACL connection require two controller activities.

The digest-bound artifacts were:

| Artifact | SHA-256 |
| --- | --- |
| historical activity-2 startup-diagnostic image | `b94c8b10c5107cce2365e72fa30cd383a17a5122339e5ca4dd88700fd0d6e38b` |
| historical activity-2 startup-diagnostic ELF | `83df24da890d4d539d370d72f84e40024d2164cd24aef039876832fc2c9ba58a` |
| historical activity-2 production BLE image | `71d3bff3ce535c7246e7c65d8b05a51615932ad521aae340eb034d7291a7d00a` |
| historical activity-2 production BLE ELF | `1d7d0ae0b5b3a55462119a5c9327031e40b1f9a02843a473da35d495f4207ca0` |
| disconnect-barrier production BLE image | `74ce5f8a8ef5ddb1eec105a843c4fd633753585eaf81b592738f3f7b5c14b8ea` |
| disconnect-barrier production BLE ELF | `39789a94cf060056f320765bbece079410e7352b953169e400e4bad48a712891` |

With the activity count set to two, the powered diagnostic completed controller
construction, Trouble host construction, GATT-server construction, controller
runner startup, and connectable advertising. Its reclaimed internal heap
remained exactly 72 KiB, and 41,040 internal-heap bytes were free after
advertising. No heap increase was needed to fix startup.

The historical activity-2 production image was identity-safely flashed and read
back on Board B before two direct macOS CoreBluetooth suite-3 sessions. That
older diagnostic artifact observed the first immediate post-disconnect
re-advertise transiently return HCI `0x07` (`Memory Capacity Exceeded`) before
controller teardown completed; its 100 ms advertise retry recovered. This
`0x07` observation is historical to the older activity-2 artifact, not a
current-source limitation.

The final disconnect barrier records whether `serve_connection` consumed
Trouble's exact `Disconnected` event. Every other exit requests disconnect and
waits without a success timeout for the raw disconnect event; timer rechecks
only emit prolonged-drain diagnostics. The old `GattConnection` is then
explicitly dropped so Trouble's sole host-resource refcount reaches zero before
the loop can create another advertiser. This fail-closed drain does not raise
the two-activity controller budget or one-link application limit, and a stalled
BLE teardown does not stop the separately spawned autonomous LoRa/node tasks.

The exact disconnect-barrier production image was identity-safely flashed and
read back on both boards:

| Board | USB serial | eFuse MAC | Flash | Radio |
| --- | --- | --- | --- | --- |
| A | `AC:A7:04:E1:3E:88` | `ac:a7:04:e1:3e:88` | 16 MiB | `HT-RA62-HF` |
| B | `AC:A7:04:E1:3F:88` | `ac:a7:04:e1:3f:88` | 16 MiB | `HT-RA62-HF` |

Board B then completed three consecutive production CoreBluetooth suite-3
authenticated sessions in 10,907 ms, 12,351 ms, and 11,595 ms. Board A
independently completed the same path in 12,193 ms. All four runs used 20-byte
fragments, write-with-response to RX, and indications from TX. Board B returned
device ID `653239302d6170692d31aca704e13f88`, primary destination
`83a09ed807a0a7c631386deaa0448fb9`, and LXMF delivery destination
`935caba93f7cd97c7c6658350ac02b45`; Board A returned device ID
`653239302d6170692d31aca704e13e88`, primary destination
`c99e8ff1ec8629e4e1290e14462ae8af`, and LXMF delivery destination
`03869ee76b74d1e2a4626f0c02ae3248`. This qualifies the production firmware's
bounded disconnect/drain/drop/re-advertise sequence across consecutive sessions
and independently on both hardware identities.

This proof does not qualify a powered Expo iOS/Android
foreground/background/reconnect lifecycle matrix, pressure, or soak. The
process-global React Native `BleManager` still needs the P2 cross-instance
ownership epoch before overlapping owners or restoration can be qualified, and
BLE controller initialization can still panic/assert before the API bearer
reaches its recoverable isolation boundary.

The private local evidence root is
`/private/tmp/e290-ble-powered-20260723.YcRky1`. The final flash/readback records
are
`board-a-ble-disconnect-barrier-production-flash.flash-image.verified.json` and
`board-b-ble-disconnect-barrier-production-flash.flash-image.verified.json`.
The final session records are
`board-b-ble-disconnect-barrier-production-qualification-1.json`,
`board-b-ble-disconnect-barrier-production-qualification-2.json`,
`board-b-ble-disconnect-barrier-production-qualification-3.json`, and
`board-a-ble-disconnect-barrier-production-qualification.json`. The historical
activity-2 diagnostic monitor and first authentication records are
`board-b-ble-activity2-diagnostic.monitor.txt` and
`board-b-ble-activity2-native-qualification.json`. These paths bind this
development proof but are not portable repository artifacts.

## Connected-board identity and future flash procedure

ROM download mode uses the dedicated `BOOT_KEY` on ESP32-S3 GPIO0: hold the
board button silk-screened `BOOT`, tap `RST`, wait one second, then release
`BOOT`. GPIO21 is the separate application user/pairing key and cannot select
the ROM loader. An application profile that quarantines native USB will
therefore remain absent after an ordinary `RST` until this GPIO0 sequence is
used.

The read-only 2026-07-17 discovery snapshot was:

| Ephemeral port | eFuse MAC | Chip | Flash | Security |
| --- | --- | --- | --- | --- |
| `/dev/cu.usbmodem101` | `ac:a7:04:e1:3e:88` | ESP32-S3 rev 0.2 | 16 MiB | secure boot and flash encryption disabled |
| `/dev/cu.usbmodem1101` | `ac:a7:04:e1:3f:88` | ESP32-S3 rev 0.2 | 16 MiB | secure boot and flash encryption disabled |

Ports are not identities and can change after reset or reconnection. Before a
future write:

Set `EXPECTED_USB_SERIAL` to the selected board's exact uppercase native-USB
serial (for example `AC:A7:04:E1:3E:88`) and `EXPECTED_MAC` to the same eFuse
MAC in lowercase (for example `ac:a7:04:e1:3e:88`). Do not derive either value
from the current callout-device name.

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
   python3.13 interop/python/e290_qualification_host.py read-flash \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/full-flash-before" \
     --output "$BACKUP_DIR/flash-before.bin"
   ```

   The helper finalizes the dump owner-read-only (`0400`) after fd-bound hashing
   and durable sync. Keep the board in the serial loader after this backup. Any
   later copy or archive of the dump must retain equivalent access control and
   encryption.
4. Create the explicit 16 MiB merged image rather than invoking the 8 MiB
   workspace runner:

   ```sh
   ELF=target/e290-default/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node
   espflash save-image --skip-update-check \
     --chip esp32s3 --merge --skip-padding \
     --flash-mode dio --flash-freq 80mhz --flash-size 16mb \
     --xtal-freq 40mhz \
     --partition-table partitions/heltec-vision-master-e290-node.csv \
     --target-app-partition factory "$ELF" e290-node.bin
   IMAGE_BYTES="$(wc -c < e290-node.bin | tr -d ' ')"
   IMAGE_SHA256="$(shasum -a 256 e290-node.bin | cut -d ' ' -f 1)"
   test "$IMAGE_BYTES" -le $((0x610000))
   ```

5. Before the **first product provisioning boot**, after the backup, logically
   blank the durability range. The unpadded merged image contains the bootloader,
   partition table and application; it does not initialize
   `0x610000..0xb30000`. Flashing it over arbitrary old bytes therefore does
   not create blank identity, clock, credential, configuration, journal, or
   raw-inbox or LXMF-store media, and the firmware will correctly fail closed.
   Preserve all other ranges and use the identity-owning helper's
   erase-equivalent all-`0xff` write and readback to verify exactly the
   contiguous first-boot durability/configuration region:

   ```sh
   python3.13 interop/python/e290_qualification_host.py erase-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/durability-erase" \
     --offset 0x610000 --length 0x520000
   ```

   This action accepts only the exact uppercase USB serial and 16 MiB target,
   requires sector-aligned in-bounds operands, leaves every `espflash` phase in
   `no-reset`, reads back exactly 5,373,952 bytes, scans the entire file for
   `0xff`, and records its size and SHA-256 in
   `.erase-region.verified.json`. Its `operation` must be
   `identity_bound_all_ff_write`; a native `EraseRegion` claim is invalid. No
   verified record means the blanking is not acceptable, even if `espflash`
   reported success. Do not allow an intermediate normal boot between blanking verification and the
   merged-image write.
6. Write and read back the exact merged image while leaving the board in the
   loader:

   ```sh
   python3.13 interop/python/e290_qualification_host.py flash-merged \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/product-image-flash" \
     --image e290-node.bin --expected-image-sha256 "$IMAGE_SHA256" \
     --confirmed-radio-module HT-RA62-HF
   ```

   The helper flashes a retained descriptor for the digest-qualified input,
   validates the `write-bin` action's own chip/flash/MAC output, captures the
   unchanged post-write USB mapping, and runs loader-preserving `board-info`
   before readback. It then independently validates the `read-flash` action
   identity and mapping. The verified JSON records distinct
   `write_action_target`, `post_write_target`, and `read_target` facts; absence
   of any phase evidence makes the write unverified even when `espflash`
   returned success.

7. A first install from any pre-`lxmf_store` partition layout requires a
   one-time migration because `0x930000..0xb30000` was previously unallocated
   and cannot be assumed erased. After the new secret full-flash backup and
   while the board remains in the loader, blank and readback-verify exactly the
   new partition before the first image with this layout is booted:

   ```sh
   python3.13 interop/python/e290_qualification_host.py erase-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/lxmf-store-first-install" \
     --offset 0x930000 --length 0x200000
   ```

   Require the verified record to bind offset `0x930000`, length and readback
   size `0x200000` / 2,097,152 bytes, and an all-`0xff` readback to the qualified
   board identity. Do not boot the new image without that record. This migration
   must never erase or rewrite `message_store` or any earlier product range.

   On every **subsequent upgrade after that partition exists**, preserve a new
   secret full-flash backup but do not erase `node_identity`, `announce_clock`,
   `api_credentials`, `node_journal`, `message_store`, `lxmf_store`, or any
   newer product store. The unpadded merged-image write must stop at or below
   `0x610000`. For an upgrade-layout check, read the complete application-data
   region `0x610000..0xb30000` before the write, leave the board in the loader,
   read it again immediately afterward and require exact equality before the
   first upgraded boot:

   ```sh
   python3.13 interop/python/e290_qualification_host.py read-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/upgrade-app-data-before" \
     --offset 0x610000 --length 0x520000 \
     --output "$BACKUP_DIR/upgrade-app-data-before.bin"
   python3.13 interop/python/e290_qualification_host.py flash-merged \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/upgrade-product-image-flash" \
     --image e290-node.bin --expected-image-sha256 "$IMAGE_SHA256" \
     --confirmed-radio-module HT-RA62-HF
   python3.13 interop/python/e290_qualification_host.py read-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/upgrade-app-data-after" \
     --offset 0x610000 --length 0x520000 \
     --output "$BACKUP_DIR/upgrade-app-data-after.bin"
   cmp "$BACKUP_DIR/upgrade-app-data-before.bin" \
     "$BACKUP_DIR/upgrade-app-data-after.bin"
   ```

   Each verified record binds the exact offset, `0x520000` length, 5,373,952
   output bytes, output path, and SHA-256 to the independently qualified board
   identity. A
   future partition-map, identity, journal, or message format change requires
   an explicit migration procedure; it is not a normal upgrade.
8. Start the flashed application deliberately. `flash-merged` uses
   `--after no-reset` for every write/readback phase and therefore leaves the
   ESP32-S3 in the ROM serial loader. A USB-only re-enumeration changes the host
   session but is not a CPU reset and does not boot the firmware. `espflash
   4.5.0 reset` is also insufficient here: on native USB it only toggles serial
   control lines, and its watchdog path will not clear a retained
   `FORCE_DOWNLOAD_BOOT` bit. After all first-install migration or upgrade
   readback work is complete, install the audited esptool dependency set into a
   fresh isolated target and run the following while the identity-qualified
   loader callout still exists:

   ```sh
   ESPTOOL_PYTHON="$(mktemp -d)"
   python3.13 -m pip install --target "$ESPTOOL_PYTHON" \
     -r interop/python/requirements-esptool-5.3.0.txt
   PYTHONPATH="$ESPTOOL_PYTHON" python3.13 -m esptool \
     --chip esp32s3 --port "$LOADER_PORT" \
     --before no-reset --after watchdog-reset --no-stub \
     write-mem 0x6000812c 0x0 0x1
   ```

   The masked register write clears only `RTC_CNTL_OPTION1` bit 0; esptool then
   initiates the RTC-watchdog full-chip reset. Do not follow it with host USB
   re-enumeration or `probe-rs reset`. Wait for the old USB service to
   disappear, then resolve the matching serial's newly attached application
   service and require it to remain stable for at least 500 ms before opening
   its fresh callout. Port names are ephemeral. A physical EN reset or VBUS
   power cycle is the recovery alternative when the loader USB service is no
   longer reachable.

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
3. Apply the same identity-bound all-`0xff` erase-equivalent operation to the
   exact 1 MiB journal partition and verify every byte is blank:

   ```sh
   python3.13 interop/python/e290_qualification_host.py erase-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/node-journal-erase" \
     --offset 0x630000 --length 0x100000
   ```

4. Flash the one-shot feature image and boot it once. The permanent image now
   uses no-op logging to reserve USB Serial/JTAG for framed control, so the old
   serial-log proof (`journal-reprovision-policy`, `node-journal-provision`, and
   schema-2 mount lines) is no longer available. Do not count this migration as
   verified without an independent exact raw-journal readback/parser or a
   separately reviewed diagnostic build/sink. The firmware still scans the
   complete partition before provisioning and rejects any schema-1, corrupt,
   torn, or otherwise programmed byte without a write or erase. If this one-
   shot boot is interrupted during provision, repeat the all-`0xff` operation
   and verify the same journal range again; it does not repair programmed
   migration media.
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
The later API 1.1 permanent-image run supplied controlled peer DATA/proof and
terminal status, and the API 1.2 run separately supplied receiver-side durable
raw-RNS commit/peek/reset/drop-newest evidence. The subsequent mount matrix and
commit-write HIL add bounded fail-closed isolation plus one direct DATA/proof
exchange per injected state. Fragment reassembly, forwarding, multi-hop,
sustained routing, and broader protocol behavior still require dedicated
fixtures; none of the later runs changes the deliberately narrow claim of this
first smoke.

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
  handoff, current-authority node dispatch, combined operation-scoped
  submission/raw-inbox/LXMF-read/LXMF-compose owner, and minimal single-flight
  USB session bearer. Preserve the now-qualified API 1.1
  identity, authenticated RNS DATA, durable runtime, real LoRa peer proof,
  sequential status, and fresh post-re-enumeration session path. Also preserve
  API 1.2's exact one-entry inbox binding/format, authenticated status/peek, and
  powered commit/readback/hard-reset/drop-newest evidence, the four-case
  cold-mount quarantine matrix, and the same-boot missing-commit admission
  quarantine.
  Resumption, retries, close records, encryption, rate/attempt policy, repeated
  attempts, and concurrency remain later hardening work; the transport-neutral
  admission boundary must remain reusable by every bearer. BLE suite 3 now has
  a bounded one-connection implementation, three consecutive authenticated
  CoreBluetooth sessions on Board B, and one independent session on Board A;
  the fail-closed disconnect barrier is therefore powered-qualified. Wi-Fi still
  requires powered qualification, while the mobile Expo lifecycle matrix,
  P2 cross-instance `BleManager` epoch, pressure, and soak remain open. The
  narrow pre-authentication bearer, current 128-entry non-reclaiming submission
  profile, 129th-request rejection, mutation-free replay at capacity, and ADR
  0005 host behavior are covered in source/host tests. Historical 16-entry
  artifacts remain bound to their older profile; powered pressure and fill of
  the 128-entry profile are still open. A later product-capacity policy must not
  weaken the same durability contract, and future interface actors fail-stop only their
  affected actor.
- Extend ADR 0011's bounded single-commit timing/high-water baseline across
  live electrical power cuts, partial-body and partial-commit programming,
  backend error-after-write cases, sustained and forwarded traffic, concurrent
  durable activity, low-memory/allocation-failure pressure, and default-image
  observation. Preserve the completed API 1.4 E290-pair POC: each board
  source-free sent through USB, reached a terminal Reticulum proof, and let the
  peer list and digest-verified-read the exact committed normalized wire.
  Preserve the completed final-image physical-reset check: both sender journals
  retained `Delivered`, both stores remounted the original message IDs, and
  both exact 126-byte reads matched their pre-reset digests. Next design LXMF
  delete/acknowledgement, retention/reclamation, compaction, and migration
  policy plus device configuration with explicit wear, authorization, and
  cross-store ordering.
- Define and qualify the production key backup/recovery and at-rest protection
  policy. The current developer image deliberately requires flash encryption
  disabled and stores its mirrored private identity in plaintext.
- Bind non-DATA LXMF events to bounded durable/client owners. Opportunistic
  destination DATA now reaches the dedicated LXMF store, while local Link
  admission remains disabled so unowned Link/Resource events cannot saturate
  the generic event queue.
- Preserve implemented source-free basic opportunistic LXMF send plus committed
  list/read. Extend send to nonempty fields, stamps/tickets, direct/Resource and
  propagation delivery. Move the temporary one-interface local path-response
  wrapper into an owned Rete implementation with per-interface forwarding state
  before multi-transport routing. Add store delete/reclaim/migration and local
  LXMF/NomadNet client services or an external cross-platform client.
- Bound `AdmissionDeferred` lifetime/attempts together with source-identity
  discovery and retention; the first profile can otherwise let sixteen
  never-resolved source identities occupy every application-event slot until
  reboot.
- Preserve the composed independently vector-tested ADR 0006 authentication
  model, ADR 0009 pairing, and first USB bearer. Replace the POC's
  integrity-only session with an appropriately confidential, rate-limited
  wireless profile before treating the now-exposed BLE proof bearer or a future
  Wi-Fi bearer as production-ready. Add Wi-Fi or BLE as a Reticulum transport
  only when that separate link behavior and
  interface-scoped path forwarding are specified; packet transports remain
  deferred behind the primary LoRa slice.
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
- Extend the completed controlled two-board API 1.1 DATA/proof, API 1.2
  inbox/fault-isolation, bounded runtime-measurement runs, and API 1.4
  bidirectional client POC with electrical power cuts, sustained traffic,
  multi-hop/Resource coverage, concurrent-store pressure, composer allocation
  failure/high-water checks, and full production-image memory/timing
  qualification.
- Keep display and GNSS/location integration stubbed until the network,
  persistence and client ownership paths are stable.
