# Permanent Vision Master E290 node image

**Status:** the first permanent, LoRa-first image is implemented. Its release
record below captures the host-library, host-client, portable-target, ESP32-S3,
strict review, graph, image-size, and same-image readback gates. Its third task
owns a USB Serial/JTAG pre-authentication initialization and live-pairing bearer,
one shared exact-next sequence space, debounced GPIO21 physical presence, an
interrupt-linearized reset-epoch guard, and an application-entry USB boot
quarantine. Powered work has completed the first outbound API 1.1 path, the
bounded API 1.2 raw-RNS inbox qualification path, and a separate opt-in
runtime-measurement slice on the two E290s. In the
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
minimal authenticated USB session bearer. It accepts one handshake per USB
connection and one request at a time, and a session fault remains terminal
until reset or re-enumeration. Resumption, protocol retries, close records,
encryption, rate limiting/attempt policy, repeated handshake attempts, and
concurrent requests are deferred. Credential selection, admission handoff, and
node dispatch are bearer-neutral. The current qualification suite is USB
Serial/JTAG-only; BLE or Wi-Fi can later reuse the ownership boundary after its
binding/suite is added and qualified. Powered evidence now qualifies
authenticated capabilities and identity reads, sequential request/response
flights in one session, durable submission, LoRa DATA delivery, peer decrypt/
proof, terminal projection, API 1.2 durable raw-RNS status/peek before and after
a hard reset, one drop-newest observation, four bounded cold-mount quarantine
cases, and one same-boot missing-commit quarantine. Each fault case includes
only one direct DATA/proof exchange; it does not claim sustained forwarding,
multi-hop routing, LXMF, or general application-level message consumption,
session resumption, or either deferred wireless bearer binding.

The current source composition pins Rete commit
`fb96ac102be4b2a2697484cd5b5c1e3f1adea6a2`, with designated durable tag
`firmware-pin-fb96ac1`. That pin is newer than every powered measurement and
build-artifact digest recorded below. It removes implicit
interface-zero/broadcast fallbacks, adds exact path/reverse/Link routing and
authenticated fail-closed LRPROOF handling, and makes covered H2 relay/reverse
admission transactional with typed failures. Those host regressions do not
retroactively qualify a historical image or hardware run. The pin has no
retained E290 ELF, flashed-image readback or powered proof of its own; every
artifact and powered result below remains bound to its recorded historical
source and Rete revision.

For locally owned Links, a responder binds the LINKREQUEST ingress interface,
while an initiator remains unbound until a valid LRPROOF supplies the
authenticated ingress interface. Active application and maintenance output
then carries native `BoundInterface` and resolves to that exact physical
interface. Only an initial LINKREQUEST with no learned path interface may
broadcast. Wrong-interface Link DATA and `RESOURCE_PRF` are rejected before
deduplication, preserving a later correct-interface copy. This is currently an
interface-slot binding: a Tokio shared `Hub` still broadcasts asynchronous
owned-Link output to siblings until Link state carries endpoint-aware client
identity. Pending-Link `expected_hops`, Python keepalive wire/role parity and
fresh-hash channel-retransmission receipt replacement remain open.

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
deferral, and exposes authenticated API 1.2 status/peek through a separate
read-only port. A mount or admission fault disables only that inbox service.
The four cold-mount fixtures establish one direct peer DATA/decrypt/proof
exchange after boot quarantine; the same-boot HIL's triggering exchange
precedes the missing-commit quarantine. Neither establishes sustained or
multi-hop routing. An optional journal mount/recovery failure occurs before any
durability-gated DATA owner can exist; it disables local durable submission
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
logical dispatch synchronously through disjoint short-lived submission and
inbox-port views that cannot borrow credential records. Revoked or missing
credentials return only
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
Device configuration, final LXMF/message storage and client delivery,
LXMF/NomadNet, and production-ready host-facing USB/BLE/Wi-Fi services remain
visible product blockers. The one-entry raw-RNS qualification record is not
that final storage or client surface.

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
  depth-one pre-auth control command/reply handoff
  depth-one bearer-neutral live-pairing command/reply handoff
  authenticated API node lane
    current-authority revalidation + synchronous logical dispatch
    disjoint short-lived SubmissionPort + InboundMailboxPort views
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
RAM followed by the detected PSRAM. Because `esp-alloc` searches registered
regions in order, ordinary global allocations currently consume internal heap
first and spill into PSRAM only when no internal hole fits. That is a measured
baseline, not the intended long-term placement policy: large protocol/client
payloads will need explicit external allocation, while atomics, synchronization,
DMA/IRQ-visible state and flash-critical state must remain internal. Largest
contiguous free space is not exposed by the pinned allocator and must not be
inferred from total free bytes.

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
| Message store | `0x730000` | 2 MiB | Wired ADR 0011 format-1 raw-RNS inbox; one 576-byte commit-last item; 383-byte maximum; not LXMF |
| Unallocated | `0x930000` | 6.8125 MiB | OTA/layout decision |

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

For a checkpoint captured as the required contiguous `RTME || RPTE` range,
decode and correlate both records together:

```sh
cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement decode-checkpoint \
  --input /path/to/checkpoint.bin [--json]
```

The combined decoder reports the RTME TX-operation total beside the sum of
the two RPTE TX-outcome counters. A mismatch is retained as a diagnostic, not
a malformed-record error: the debugger can halt the actor after RPTE records
the radio result but before the surrounding RTME operation guard completes.
Stable baseline and terminal acceptance checkpoints still require the two
totals to agree.

Both records use matching-even sequence markers. Their record methods depend
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
at most 52,752 bytes, both linker guard offsets must remain 60 bytes, and the
default/HIL usable stacks must remain at least 170,984/170,288 bytes. The
default ELF must exclude the proof trace; the HIL ELF must contain exactly one
initialized 192-byte symbol whose linked bytes decode as a valid empty `RPTE`
record. Record counts are diagnostic rather than policy: the current release
ELFs contain 816 default and 832 HIL records, while both maxima are 52,752
bytes. CI runs
Clippy and then relinks both profiles with
`-C link-arg=-nostartfiles -Z emit-stack-sizes` in isolated target directories
immediately before this inspection.

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
pair is 171,048/170,984 bytes. The unchanged largest compiler-emitted frame is
52,752 bytes.

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
debugger-reset attempt. They do not describe an ELF built from the current
`fb96ac1` pin. The immediately preceding 777,600-byte HIL image,
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
evidence for the immediately preceding trace revision, not powered evidence
for the current hashes or the pending two-board RF proof-timeout reproduction.
The 72,020-byte raw margin leaves 19,268 bytes after subtracting the unchanged
52,752-byte maximum compiler frame.

### Decisive proof-correlation trial runbook

Run four clean trials in `B→A`, `A→B`, `A→B`, `B→A` order. The fixed board
bindings are:

| Board | USB serial / MAC | Active credential | Primary destination |
| --- | --- | --- | --- |
| A | `AC:A7:04:E1:3E:88` / `ac:a7:04:e1:3e:88` | `/private/tmp/e290-rns-inbox-proof/3e-active.key` | `c99e8ff1ec8629e4e1290e14462ae8af` |
| B | `AC:A7:04:E1:3F:88` / `ac:a7:04:e1:3f:88` | `/private/tmp/e290-rns-inbox-proof/3f-active.key` | `83a09ed807a0a7c631386deaa0448fb9` |

Before using an artifact, rerun the strict default/HIL builds and ELF inspector
above, package the explicit 16 MiB merged image, and bind its size and SHA-256
to the trial manifest. Capture from the exact final HIL ELF; do not copy
addresses from an older build:

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
absent directory. The command validates one initialized
256-byte RTME symbol immediately followed by one initialized 192-byte RPTE
symbol in the final little-endian Xtensa ELF, then invokes exactly one
serial-qualified `probe-rs read` for the contiguous 448-byte range. It never
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
   submission: the current USB bearer permits one handshake per connection.
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
is a universal stack guarantee.
Current source also re-reads the innermost stack pointer immediately before
each volatile word access and reports an address at or above it as changed,
so scanner safety does not depend on Rust honoring an inlining request. The
retained two-board traffic HIL predates that source guard, but its exact linked
disassembly has the complete scanner loop in the 32-byte caller frame, uses
that live stack pointer as the exclusive read limit, and makes no call from the
scan loop. The post-run hardening therefore does not invalidate the retained
measurement. The current proof-trace image includes the guard and passes the
static ELF gate; the predecessor has the one-board powered baseline above. The
current image still needs exact powered readback plus its two-board traffic
workload.

`node_identity`, `announce_clock`, and `api_credentials` use ESP-IDF's standard
`data,undefined` subtype. All three have application-owned formats; the
credential range is checked, boot-mounted/recovered, and retained. Explicit
initialization and ADR 0010 live pairing are routed through the resident owner;
minimal single-flight authenticated USB session/API serving is powered-qualified
through identity, durable submission, sequential status, peer proof, and a
post-re-enumeration terminal status read.
`device_config` retains the standard NVS subtype while it is unwired; the
application-owned journal and wired raw-RNS inbox retain `data,undefined`.
Their labels and ranges remain distinct. The complete `message_store` range is
bound to the physical device ID, absolute offset, length, and inbox physical
format version 1. Numeric custom subtypes are only valid with custom partition
types in the image tooling and are not used here.

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

Journal mount, unsupported history, or recovery failure is isolated because it
occurs during boot before a durability-gated DATA owner can exist: the
coordinator retains the flash backend with no runtime, local durable admission
remains closed, and the LoRa node/radio tasks still start in route-only mode.
The accepted-history cap is one for qualification. The minimal authenticated
USB edge now has powered initialize/pair/reboot/capabilities/identity evidence
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

The resident `ProductStorageCoordinator` also implements the target-safe
device-API `SubmissionPort` for capability, principal-scoped status, and
`experimental-rns-data` acceptance plus a separate read-only
`InboundMailboxPort` for authenticated API 1.2 status/peek. Those are only the
product-side semantic seams: neither one-entry qualification cap is product
capacity. Portable framing and job handoff enter the permanent graph through a
static depth-one
authenticated request/reply channel. The node endpoint decodes the logical
request, revalidates its opaque grant against the resident current authority,
and calls the adapter synchronously through credential-disjoint submission and
inbox-port views. Missing, revoked, replaced, or generation-mismatched credentials
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

All three physical-store bindings name the device with the domain-separated
16-byte value `"e290-flash" || eFuse base MAC`. The credential view additionally fixes
absolute offset `0x614000`, length `0x2000`, and credential physical layout
version 1. The journal view fixes offset `0x630000`, length `0x100000`, and
journal physical layout version 1. The inbox view fixes offset `0x730000`, length
`0x200000`, and inbox physical format version 1. Each store validates its exact
values and view capacity/alignment before I/O; every later borrowed operation
must match its retained binding exactly.

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
The 129-test default host library suite, 135-test commit-fault profile, and
145-test runtime-measurement profile have
passing policy/product/credential-boot/
credential-runtime/USB-control/live-routing tests, including the source-order
regressions, every canonical empty-initialization byte cut, adversarial media changes between
mount and classification, off-trajectory media, and classifier failure phases,
two authenticated inbox-port isolation tests, and two real cross-layer
composition tests. The happy path proves unauthenticated
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
ownership, policy completion, and fail-closed noncanonical states. Six
cross-store tests cover both retained journal owners, initialization before and
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
The initialization-control host client has 12 focused tests for parsing,
default deadlines, single-open sequence progression, terminal result/exit
behavior, ambiguity guidance, and sequence exhaustion. The live-pairing client
adds 15 tests for its CLI contract, exact response-family correlation and
device-ID validation, pair/resume sequence headroom, owner-only reservation and
atomic Pending persistence, fixed 96-byte binary layout, and atomic complete
Pending-to-Active replacement that persists the HMAC-bound durable Active
generation while retaining owner-only permissions, and secret-free output. The
bounded authenticated client adds 29 tests for operation parsing, public-
identity formatting, inbox status/peek, owner-only non-overwriting payload and
evidence output, sequential request IDs, version policy, polling terminal
semantics, coalesced-record preservation, authenticated terminal binding, and
submission-input non-disclosure. Together these 56 focused tests are part of
the full 248-test xtask gate. The portable
Rete integration and inbox-store suites independently pass 53 and 17 tests,
respectively. The 53 are project adapter tests. The project conformance runner
now performs 144 checks, including a 32-check deterministic three-node A--B--C
relayed Link/LRPROOF/LRRTT/channel/proof flow; it is not powered or live-Python
multi-hop qualification. The exact nested Rete selected validation set
separately passes 614 tests: 254 transport (165 library, 9 computed-vector, 43
forwarding, 32 Link-integration and 5 path-request), 133 stack (132 library and
one integration), 143 LXMF library and 84 daemon library tests. The four
library targets total 524 tests; the 89 transport and one stack integration
tests bring this named set to 614. It is not a count of every nested workspace
test target.
Thirty-one of the xtask tests freeze the measurement decoder's
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
handshake, or concurrent-request behavior. Each one-shot command drops its
restored client session after one response while firmware remains established,
so reset/re-enumerate USB before a later invocation. `submit-and-wait` is the
exception: it deliberately retains that one session for sequential status
requests. Do not infer whether a credential identifier exists from failure
text.

The one-entry accepted-history cap is a qualification profile. Each additional
powered submission below used the documented journal-only erased schema-2
reprovision workflow; that operator action is neither a product retry mechanism
nor evidence that the intended product capacity is one.

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
avoid PSRAM: the current allocator searches the 64 KiB internal region first,
and this workload never exhausted it. Resource, LXMF, NomadNet, SPA and future
wireless buffers still require an explicit internal/external placement policy.
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
preceding `f6f5fb0637d00691e09fa0105be4df902405fee4` Rete pin. The current
`fb96ac1` host suite now covers exact reverse-interface routing and proof
consumption, typed transactional reverse admission, and a deterministic
three-node relayed Link/channel/proof flow, but only a new powered run can
determine whether that fixes this end-to-end timeout or whether another product
boundary remains faulty. A final
authenticated peek on `3f:88` likewise returned phase A's exact 383-byte
payload from destination `83a09ed807a0a7c631386deaa0448fb9`.

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

## Connected-board identity and future flash procedure

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
   `0x610000..0x930000`. Flashing it over arbitrary old bytes therefore does
   not create blank identity, clock, credential, configuration, journal, or
   inbox media, and the firmware will correctly fail closed. Preserve all other
   ranges and use the identity-owning helper's erase-equivalent all-`0xff`
   write and readback to verify exactly the contiguous first-boot
   durability/configuration region:

   ```sh
   python3.13 interop/python/e290_qualification_host.py erase-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/durability-erase" \
     --offset 0x610000 --length 0x320000
   ```

   This action accepts only the exact uppercase USB serial and 16 MiB target,
   requires sector-aligned in-bounds operands, leaves every `espflash` phase in
   `no-reset`, reads back exactly 3,276,800 bytes, scans the entire file for
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

7. On every **subsequent upgrade**, preserve a new secret full-flash backup but
   do not erase `node_identity`, `announce_clock`, `api_credentials`,
   `node_journal`, or any newer product store. The unpadded merged-image write
   must stop at or below
   `0x610000`. For an upgrade-layout check, read the complete application-data
   region `0x610000..0x930000` before the write, leave the board in the loader,
   read it again immediately afterward and require exact equality before the
   first upgraded boot:

   ```sh
   python3.13 interop/python/e290_qualification_host.py read-region \
     --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$BACKUP_DIR/upgrade-app-data-before" \
     --offset 0x610000 --length 0x320000 \
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
     --offset 0x610000 --length 0x320000 \
     --output "$BACKUP_DIR/upgrade-app-data-after.bin"
   cmp "$BACKUP_DIR/upgrade-app-data-before.bin" \
     "$BACKUP_DIR/upgrade-app-data-after.bin"
   ```

   Each verified record binds the exact offset, length, output path, output
   byte count and SHA-256 to the independently qualified board identity. A
   future partition-map, identity, journal, or message format change requires
   an explicit migration procedure; it is not a normal upgrade.

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
  handoff, current-authority node dispatch, disjoint submission and inbox-port
  views, and minimal single-flight USB session bearer. Preserve the now-qualified API 1.1
  identity, authenticated RNS DATA, durable runtime, real LoRa peer proof,
  sequential status, and fresh post-re-enumeration session path. Also preserve
  API 1.2's exact one-entry inbox binding/format, authenticated status/peek, and
  powered commit/readback/hard-reset/drop-newest evidence, the four-case
  cold-mount quarantine matrix, and the same-boot missing-commit admission
  quarantine.
  Resumption, retries, close records, encryption, rate/attempt policy, repeated
  attempts, and concurrency remain later hardening work; the transport-neutral
  admission boundary must remain reusable by BLE and Wi-Fi, whose session
  bindings/suites still require explicit implementation and qualification. The narrow
  pre-authentication bearer, one-entry composition cap, and ADR 0005 host
  behavior already pass. A later product-capacity policy must not weaken the same
  durability contract, and future interface actors fail-stop only their
  affected actor.
- Extend ADR 0011's bounded single-commit timing/high-water baseline across
  live electrical power cuts, partial-body and partial-commit programming,
  backend error-after-write cases, sustained and forwarded traffic, concurrent
  durable activity, low-memory/allocation-failure pressure, and default-image
  observation. Rerun the reverse delivery-proof scenario at the current
  `fb96ac1` pin and diagnose any remaining timeout before claiming bidirectional
  delivery completion. Then design final LXMF/message storage and
  device configuration with explicit wear, migration, reclamation,
  authorization, and cross-store ordering behavior.
- Define and qualify the production key backup/recovery and at-rest protection
  policy. The current developer image deliberately requires flash encryption
  disabled and stores its mirrored private identity in plaintext.
- Deliver non-DATA node events to a durable/client owner. Inbound DATA now enters
  the raw-RNS qualification store; other events remain drained so transport
  progress cannot deadlock.
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
- Extend the completed controlled two-board API 1.1 DATA/proof, API 1.2
  inbox/fault-isolation, and bounded runtime-measurement runs with electrical
  power cuts, sustained traffic, multi-hop/Resource coverage, concurrent-store
  pressure, and full production-image memory/timing qualification.
- Keep display and GNSS/location integration stubbed until the network,
  persistence and client ownership paths are stable.
