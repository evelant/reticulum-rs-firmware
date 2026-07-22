# ADR 0009: Device-API credential store and initial pairing policy

- **Status:** accepted design; portable lifecycle-safe authority/store path,
  interrupted-initialization classifier, E290 boot/coordinator mount integration,
  pairing-admission policy, pre-authentication initialization-control codec, and
  E290 forward-only initialization runtime/sole-owner port implemented; the
  E290 USB Serial/JTAG pre-authentication initialization bearer, GPIO21
  physical-presence sampler, and depth-one task handoff are host-, target-, and
  bounded-powered-control verified; ADR 0010 live-pairing wire/crypto core,
  independent vectors, resident E290 durable lifecycle, bounded entropy, and
  bearer-neutral secret handoff implemented and routed through the node/USB
  owners; authenticated session service and successful button-confirmed
  initialization/pairing remain pending
- **Date:** 2026-07-18
- **Decision owners:** project maintainers
- **Extends:** [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0006](0006-authenticated-local-api-bearer.md), and
  [ADR 0007](0007-device-api-credential-authority.md)

## Context

The portable credential authority and its canonical 2,048-byte semantic image
now expose lifecycle-specific successor planning, the physical store carries
those opaque transitions through commit and reconciliation, permanent E290
firmware mounts/recovers its physical store, and a portable policy owner freezes
physical-presence window admission. The feature-free policy and resident
initialization owner are now composed only into the permanent E290 graph. The
featureless framing-only initialization-control codec remains portable and is
now composed only into that graph's narrow USB bootstrap lane. A sole USB
Serial/JTAG task owns byte framing and debounced GPIO21 observations, and a
depth-one command/reply handoff invokes the sole flash coordinator through the
node task. The same byte owner and causal frontier now route coarse status,
explicit empty-store initialization, and the four live pairing operations over
one device-owned authority that survives power loss. ADR 0010 freezes
the separate Begin/ProofStart/Activate/AbortCurrent records, typed continuation,
HMAC transcript and activation confirmation; this ADR still owns the durable
policy/store integration. Pairing
must not publish a secret-bearing authority before its physical commit is
established, and an empty or erased store must not silently create a trust
relationship.

This first policy is deliberately a developer/HIL contract. The E290 image
currently rejects flash encryption, and USB Serial/JTAG provides neither peer
identity nor confidentiality from another process on the connected host. The
policy is sufficient to qualify ownership, persistence, and proof-of-
possession ordering; it is not the production physical-attacker or
out-of-band trust story.

## Decision

### Reserve one exact E290 credential range

The permanent 16 MiB E290 map assigns `api_credentials` the exact raw-NOR
range `0x614000..0x616000` (8 KiB, ESP partition type `data`, subtype
`undefined`). This splits the previously unwired configuration reservation:
`device_config` moves to `0x616000..0x630000` and shrinks to 104 KiB. The
`node_journal` at `0x630000` and every later range remain unchanged.

The target partition-table guard must require exactly one plaintext, writable
`api_credentials` entry with that type, subtype, offset, and length, and must
reject duplicate labels, overlap, or a mismatched shape before protocol
service. A valid partition table proves only the physical binding. It does not
mean the credential store is formatted, mounted, or paired.

### Use two alternating full-snapshot sectors

Physical format 1 divides the partition into sector A at relative offset 0 and
sector B at relative offset 4,096. Each 4 KiB sector has this exact shape:

| Relative range | Size | Format-1 content |
| --- | ---: | --- |
| `0..128` | 128 B | Versioned snapshot header |
| `128..2176` | 2,048 B | Canonical semantic authority image |
| `2176..2208` | 32 B | Domain-separated SHA-256 digest |
| `2208..2240` | 32 B | Irregular commit marker, programmed last for the target |
| `2240..2272` | 32 B | Separate monotonic retirement marker |
| `2272..4096` | 1,824 B | Must remain erased |

The header magic is `RDAUTH01`. It records physical and semantic versions,
sector ID, retained-record count, nonzero authority revision, predecessor
revision and digest, 16-byte physical-device binding, and the fixed capacity,
slot size, and image size. Reserved header bytes must be zero. Revision 1 has
no predecessor; every later revision names exactly revision minus one and its
nonzero committed digest. The digest covers the domain-separated complete
header and semantic image. It detects corruption and trajectory conflicts; it
does not authenticate plaintext flash against a physical attacker.

All multibyte integers are little-endian. The normative 128-byte header is:

| Relative range | Field | Required value |
| --- | --- | --- |
| `0..8` | Magic | ASCII `RDAUTH01` |
| `8..10` | Physical format | `u16` value 1 |
| `10..12` | Semantic format | `u16` value 1 |
| `12` | Sector ID | 0 for A, 1 for B |
| `13` | Record count | `u8`, at most 16 and equal to the decoded image count |
| `14..16` | Reserved | All zero |
| `16..24` | Authority revision | Nonzero `u64` |
| `24..32` | Predecessor revision | Zero for revision 1; otherwise current revision minus one |
| `32..64` | Predecessor digest | All zero for revision 1; otherwise the exact predecessor digest |
| `64..80` | Device binding | Exact 16-byte product-supplied physical device ID |
| `80..84` | Record capacity | `u32` value 16 |
| `84..88` | Slot size | `u32` value 128 |
| `88..96` | Semantic image size | `u64` value 2,048 |
| `96..128` | Reserved | All zero |

The digest at `2176..2208` is exactly
`SHA-256("reticulum-rs-firmware/device-api-credential-store/snapshot/v1\0" || sector[0..2176] || [0xa5; 64])`.
The quoted ASCII domain includes its final NUL byte and is exactly 62 bytes;
the final operand is an exact 64-byte public flush trailer containing `0xa5`
in every byte. This trailer is part of physical format 1. The fixed domain and
2,176-byte secret-bearing prefix leave 62 bytes in the `sha2` 0.10 SHA-256
block buffer. Updating it with one public block first compresses that secret
tail with two public bytes, then overwrites all 64 buffer bytes with public
trailer data before finalization. This avoids leaving a raw PSK copy in a
non-zeroizing hasher buffer. The compile-time length guards and independent
empty-snapshot golden digest freeze this invariant. The normative literal
commit marker at `2208..2240` is:

```text
a3510ed8762bc4195f9037ea6d04b28c41f72d639815ce700bd456a932ef841a
```

The normative literal retirement marker at `2240..2272` is:

```text
6e13c942af7508e1349b5df02786bc4ad501793e926814cbf62c8350ad07e931
```

These marker bytes are literal format constants, chosen as irregular
SHA-256-derived values but not recomputed by decoders from an external input.
An exact marker is complete; all `0xff` bytes are absent. A partial value is a
recognized torn write only when every programmed bit could monotonically reach
the corresponding literal marker. Any other value is unknown media. The
crate's public format constants and golden-layout test are normative alongside
this table; changing any byte requires a new physical format version.

The semantic image is exactly 16 slots of 128 bytes. Unused slots are
canonical all-zero slots. The fixed-format lifetime ceiling is 16 unique
credential IDs total, including `Pending`, `Active`, `Revoked`, and aborted
pairing tombstones. An ID or tombstone is never reused or garbage-collected in
format 1. Rotation consumes a fresh ID. Exhaustion rejects new enrollment but
does not invalidate an already active credential.

Mount is read-only. Fully erased sectors report an unformatted-erased store;
programmed media without a committed authority, an unsupported version, wrong
device binding, invalid canonical snapshot, unknown marker trajectory,
unlinked snapshots, or a retired predecessor without its successor fails
closed. When both snapshots are committed, only an exact consecutive
predecessor link selects the newer. Recognized incomplete retirement or stale
inactive data yields one explicit recovery state: `RetirePredecessor` or
`CleanupInactive`, distinct from `Clean`. `publishable_authority()` and
`into_authority()` must refuse publication until predecessor retirement is
complete. Cleanup-only state may publish the already committed authority, but
the product must erase and verify that inactive sector before beginning an
unrelated credential mutation.

### Commit, retire, then publish

Every authority mutation is one complete immutable successor:

1. Validate the candidate as the exact next semantic revision without I/O.
2. Require a clean, erased inactive target sector.
3. Program and exactly read back the target header plus complete semantic
   image, then its digest.
4. Program and exactly read back the target commit marker.
5. Re-scan enough state to establish the exact target revision, predecessor
   link, digest, and canonical image.
6. Program and exactly read back the source sector's retirement marker.
7. Only then publish the candidate authority or report the mutation complete.
8. Erase and verify the retired source as a later explicit cleanup step while
   the committed target remains intact.

A backend error or uncertain readback after mutation begins retains the exact
current and candidate owners. The sole storage coordinator must reconcile that
specific mutation from media before serving either authority, erasing a
sector, or beginning unrelated credential/application work. It may resume an
exact recognized step; it must not guess whether a write happened. Boot uses
the same scan and recovery rules. This commit/retire order permits a power cut
to leave two linked committed snapshots, but never authorizes the firmware to
publish an uncommitted candidate or erase the only committed authority.

This global exclusion applies to a same-boot `PendingCredentialSuccessor`
created by a live mutation with an uncertain result, and to any cross-store
intent: either blocks unrelated durable mutation until reconciled. A
deterministic `RetirePredecessor` or `CleanupInactive` owner discovered by
read-only boot mount is not such a new ambiguous mutation. E290 boot makes one
bounded retirement attempt followed by one bounded cleanup attempt, retains the
mounted owner and resulting admission state, and quarantines only credential
admission/mutation if either step cannot finish; unrelated journal policy and
LoRa startup continue.

The semantic authority provides `plan_add_pending`, `plan_activate_pending`,
and `plan_abort_pending` for adding the sole `Pending` record, activating that
exact pending record, and replacing it with a PSK-free aborted tombstone. Each
result becomes an opaque `PairingLifecycleStoreCandidate`;
`commit_pairing_lifecycle_successor` and
`reconcile_pairing_lifecycle_successor` retain its Add/Activate/Abort
discriminator through semantic rejection, physical ambiguity, and durable
success. Commit preflight first checks generic structural succession and then
revalidates the declared transition against the exact source pending reference,
PSK, principal, permissions, and immutable audit facts retained privately by
the opaque candidate. `Structural` and `TransitionMismatch` errors preserve the
unchanged mounted authority and candidate without store I/O. The supported
product path uses
`MountedCredentialStore::select_pending_for_proof` only after commit/readback
and predecessor retirement. This is a repository-enforced trusted linked-code
boundary, not an unforgeable Rust capability: the one-way credential-store-to-
authority dependency requires narrow public integration methods because Rust
has no friend-crate visibility. The graph-policy source guard permits those
integration identifiers only in the credential authority and physical-store
owner files and rejects workspace composition that bypasses the mounted path.
Erase-only inactive cleanup may remain.

### Compose bounded recovery immediately after flash open

The E290 `ProductFlashOwner` validates the exact partition shape and derives the
credential binding from the same eFuse-based physical-device ID used by the
journal. It invokes credential mount/recovery immediately after open, before
identity preflight, journal provisioning, announce-clock reservation, identity
load/provision, or journal mount. A mechanical host regression freezes that
source order.

Boot returns the seven product states `Ready`, `AuthOnly` (the Rust
`AuthenticationOnly` variant), `Uninitialized` (the `UninitializedErased`
variant), `InitializationInterrupted`, `Blocked`, `Corrupt`, or `Backend`.
Only `Ready` permits a future credential mutation; `Ready` and `AuthOnly` may
retain a publishable authority. The image has both the distinct
pre-authentication initialization/pairing records and a deliberately minimal
authenticated USB session/API bearer. The latter admits one handshake per
connection and one request at a time and fails terminally until reset; its
initialize/pair/reboot/authenticated-capabilities happy path is qualified on one
powered board. A later image also qualifies one durable submission with
sequential status, physical LoRa peer proof, and fresh post-re-enumeration
status. Broader repeated-session, ambiguity, and fault behavior remain separate
gates.
Every successfully mounted owner, including a blocked or cleanup-failed owner,
is retained in `ProductStorageCoordinator`. Erased media is never provisioned
automatically.

### Make empty-store initialization explicit and non-pairing

No boot path automatically formats or pairs an empty store. With an existing
Reticulum identity and credential media either exactly erased or on the one
canonical interrupted empty-provision trajectory, local API admission remains
disabled. The only initial action allowed is an explicit initialization command
received on the button-confirmed USB connection described below. The resident
physical runtime reclassifies the media and either establishes or resumes the
canonical empty revision-1 authority. Programmed noncanonical media is never
reformatted by this path. The E290 node task now invokes this path only after
the USB/GPIO owner has established the exact connection-bound physical-presence
window. Both powered boards have reached `physical-presence-required`; one board
has additionally completed the qualifying hold, empty revision-1 write,
Pending-to-Active lifecycle, exact Active partition readback, reboot, and
authenticated capabilities exchange. Pending/Abort readbacks and fault cuts
remain unqualified.

The portable store also provides the read-only
`classify_empty_provision_media` four-way classifier for this boundary:
`ExactlyErased`, `RecoverableInterrupted`,
`CommittedEmptyRevision1`, or `NotRecoverable`. The recoverable case is limited
to an ordered monotonic prefix of the canonical device-bound empty revision-1
program/digest/commit trajectory with the other sector and all forbidden bytes
erased. Classification never writes or erases. E290 boot now invokes it only
after normal mount reports programmed unformatted media and maps only
`RecoverableInterrupted` to `InitializationInterrupted`; contradictions and
ineligible media remain corrupt. The resident coordinator now consumes this
boot result into `CredentialRuntime`, which privately retains the exact binding,
any mounted authority, the feature-free pairing policy, and any admitted
initialization permit. It freshly reclassifies a short-lived bound view and
accepts only forward erased/interrupted trajectories, retaining the permit
across ambiguous backend or readback results. This is explicit request-time
recovery, never automatic boot recovery.

Initialization creates no credential, offers no secret, and does not
implicitly start pairing. A client must issue a separate pairing begin while a
valid physical-presence window is open. Factory reset and cross-store identity
reset ordering remain a separate design and may not be inferred from erased
credential media.

The sole flash coordinator also serializes mutations across the credential and
journal stores. A retained journal actor operation or pending projector
persistence defers initialization before policy admission. Once initialization
is admitted and in flight, journal physical drive and new durable-submission
acceptance are deferred until initialization reaches a stable result. This
deferral is distinct from runtime absence and must not disable projection,
status, Reticulum routing, or LoRa service.

### Freeze the pre-authentication initialization-control records

`reticulum-device-api-pairing-control` is a featureless, allocation-free codec
whose only dependency is `reticulum-device-api-framing`. It defines a narrow
bootstrap lane, not an unauthenticated device-API session. Every record in this
lane has the framing session ID and 16-byte authentication tag entirely zero.
Requests have an empty payload; responses have exactly one payload byte.

| Record kind | Value | Payload |
| --- | ---: | --- |
| status request | `0x20` | empty |
| status response | `0x21` | one `InitializationStatus` byte |
| initialize request | `0x22` | empty |
| initialize response | `0x23` | one `InitializeResult` byte |

| `InitializationStatus` | Value |
| --- | ---: |
| `Unavailable` | 0 |
| `InitializationRequired` | 1 |
| `InFlight` | 2 |
| `Completed` | 3 |
| `Blocked` | 4 |

| `InitializeResult` | Value |
| --- | ---: |
| `Completed` | 0 |
| `Retrying` | 1 |
| `PhysicalPresenceRequired` | 2 |
| `Refused` | 3 |
| `Blocked` | 4 |
| `Unavailable` | 5 |

All other kinds, codes, payload shapes, nonzero session IDs, and nonzero tags
are rejected. The framing sequence is preserved but deliberately uninterpreted:
the E290 bearer enforces boot-lifetime connection epochs, exact-next ordering,
replay rejection, and request/response correlation. The codec exposes
no media classification, backend fault, identity state, policy diagnostic,
secret, or credential existence detail. It performs no GPIO, timeout, session,
flash, or task-handoff work, and it cannot dispatch the logical device API.

### Fix the first developer/HIL pairing window

`reticulum-device-api-pairing-policy` implements the allocation-free portable
admission owner. It deliberately has no GPIO, USB, flash, entropy, HMAC, wire,
or executor dependency. Graph policy permits its feature-free edge only in the
permanent E290 product and continues to exclude it from legacy product/HIL
graphs. The E290 bearer adapter now supplies debounced active-low GPIO21
observations and uses USB Serial/JTAG as the only bearer for this first
developer/HIL ceremony.

The exact admission contract is:

- the current accepted connection must observe the button released before a
  low observation can arm it; boot, connection, replacement, closure, and clock
  fault all require another release before rearming, so a button held low cannot
  open or automatically reopen a window;
- button and unauthenticated control work receive bounded turns. A debounced
  High transition is latched ahead of a later Low. A raw-sample gap of at least
  20 ms cancels any possible hold, publishes conservative release evidence, and
  suppresses Low until a fresh debounced High is observed. Each fresh connection
  resets both the publication latch and debouncer to Low, so release evidence
  retained for an older epoch cannot arm the new epoch; the replacement epoch
  must observe a complete fresh High debounce;
- one uninterrupted active-low interval reaches its threshold at exactly
  2,000 monotonic milliseconds. At that threshold the policy invalidates prior
  ordinary-session admission and returns a single-use request for the bearer to
  acquire exclusive ownership. The window opens only after the bearer
  acknowledges that ownership;
- the deadline is exactly 60,000 monotonic milliseconds after the hold
  threshold, not after the later exclusivity acknowledgement. A request at
  `now >= deadline` loses to timeout, including a late acknowledgement;
- accepted connection epochs are nonzero and strictly increasing for the whole
  boot. A newer epoch replaces the current connection, closes its acquiring or
  open window as a disconnect, and resets the hold cycle. An older or reused
  epoch is refused;
- acquiring exclusivity, an open window, and any accepted operation exclude
  ordinary device-API session establishment and unrelated credential/admin
  mutation. An open window is bound to its exact window and connection epochs;
- no boot, cable connection, empty store, timeout, disconnect, or completed
  operation opens another window automatically.

The shared attempt budget is exactly three classified `Begin`/`Proof`
requests. A request from the bound connection while its window is open and not
timed out spends the next ordinal before checking operation, pending-record, or
store eligibility. Thus wrong pending ID/generation, missing/existing pending,
operation-in-flight, mutation-blocked, retained-capacity-exhausted, and next-
revision-exhausted refusals all spend budget. A request with no connection, the
wrong connection, no open window, or an expired window does not. Explicit
initialization and pending abort do not spend this budget.

The third classified request is still evaluated: it may be refused or may
return an admitted operation permit, but it immediately closes the window to
new work. If admitted, the owner enters a draining state and retains that exact
operation until its definite result is reconciled. Timeout, disconnect, or
connection replacement likewise closes window admission without discarding an
already accepted operation. A dropped or ambiguous operation permit therefore
leaves the policy fail closed; it must be retained across the asynchronous
physical operation and completed only with a definite result.

Initialization admission consumes trusted `identity_ready` plus an optional
`InitializableMedia::{ExactlyErased, RecoverableInterrupted}` classification
from the sole identity/flash owner and additionally requires no durable pending
enrollment. Its single-use permit retains the exact admitted classification.
Begin admission consumes trusted
`mutation_ready`, retained-capacity-available, and next-revision-available facts
and requires the pending slot to be empty. These values are policy preconditions,
not evidence that media is erased, writable, or unchanged: the sole physical
owner must revalidate them immediately before mutation. The policy starts from
either no pending record or one already validated durable pending reference and
tracks only its non-secret credential ID and generation.

The resident mutation coordinator, not the portable policy, allocates for each
accepted begin a never-before-used random nonzero 128-bit credential ID and
random 256-bit PSK, plus the next authority revision. The sole owner commits
and exactly reads back that `Pending` successor before it offers the ID and PSK
to the same bound USB connection. A disconnect or failed write never converts
`Pending` to `Active` and never permits a new pending ID to bypass it.

The client proves PSK possession with a domain-separated HMAC-SHA256 over a
fresh single-use device challenge and the pairing transcript, including the
credential ID and generation, stable public device-API ID, and current
connection/window binding. ADR 0010 freezes the exact record encoding and
domain bytes; the implementation may not weaken
these bindings or accept a client-supplied principal/permission assertion.
Failure responses do not reveal whether any unrelated credential exists.

After valid proof, the resident mutation coordinator constructs the exact next
successor changing that record from `Pending` to `Active`. It commits, reads
back, retires the old snapshot, and publishes the new authority before sending
pairing completion or allowing session authentication. Reporting that exact
durable, publishable result to the policy closes an otherwise-open window as
successfully activated. Thus both `Pending` before secret offer and `Active`
before completion are durable facts.

The permanent E290 image selects `esp-println`'s `no-op` backend and does not
initialize its logger, so application, panic, and framework log text cannot
share the USB Serial/JTAG FIFO. Binary COBS control records are the sole
firmware-owned bytes on that stream. Boot-ROM output can still precede the
application; the streaming decoder deliberately ignores bytes until a leading
zero delimiter. The minimal authenticated bearer preserves this ownership, and
any later bearer must do likewise or move diagnostics to a different sink. No raw log byte may appear between
binary COBS records.

The USB response owner is released only after every byte enters the endpoint
FIFO and firmware requests hardware `WR_DONE`. Waiting for a later completion
observation can deadlock RX after the host already received the response. A
later response remains losslessly backpressured at the FIFO until capacity is
available.

A surviving `Pending` record remains the only enrollment in progress. A later
button-confirmed window may prove it again if the client retained the secret,
or explicitly abort it by committing a PSK-free revoked/aborted tombstone
before allocating another ID. The firmware never silently restores, reuses,
or discards a pending secret-bearing ID.

The portable policy does not itself implement live pairing. The resident
credential runtime now combines it with ADR 0010 proof verification and the
typed store to retain Begin/ProofStart/Activate/AbortCurrent owners through
definite commit or reconciliation. The coordinator has a source-composed,
target-checked sole-owner port that freshly inspects node identity and constructs
the short-lived bound credential view. The third E290 task owns USB bytes, active-low GPIO21
debounce, SOF/bus-reset observations, a missed-SOF suspension threshold that
retains the current epoch, and bus-reset-delimited connection epochs; depth-one
command/reply channels keep the
opaque exclusivity capability with the node/storage owner. The node schedules
live operations through a causal control/live frontier, retains exact request
correlation across durable drive/reconciliation, and returns only the matching
  terminal result through the secret-owning handoff. This composition is host-
  and target-verified. Both boards have also returned
  `initialization-required` and `physical-presence-required`; one board then
  completed initialization, pairing, durable activation, reboot, and an
  authenticated capabilities exchange. The remaining boundary includes:

- exact powered Pending and Abort readbacks plus ambiguity/fault/cut
  qualification across every mutation boundary;
- no-fallback failure cases and broader repeated-session/lifecycle
  qualification beyond the bounded submission/status happy path;
- USB suspend/resume behavior
  still requires powered host-matrix validation; the present SOF/missed-SOF
  policy is not a final suspend contract.

### Defer the production security profile

Format 1 stores PSKs in plaintext. Raw flash access reveals them, just as it
reveals the current plaintext Reticulum identity. This policy is allowed only
for developer and HIL images with secret backup/dump handling.

A production decision must provide reviewed secure-boot and flash-encryption
provisioning or application-level authenticated encryption with independently
protected keys. Production pairing must also bind the intended client with an
independent display code, QR flow, or equivalent out-of-band confirmation.
Those decisions may reuse the semantic authority and commit/retire ordering,
but cannot claim security from the developer USB trust shortcut.

## Consequences

- Credential state has one checked range and one full-snapshot trajectory;
  `device_config` is not a competing authority.
- Secret offer, activation, and live publication each occur only after the
  durable transition required for that step.
- A fixed 16-ID lifetime ceiling is intentionally small but makes non-reuse,
  tombstone retention, RAM, scan time, and recovery behavior bounded.
- Empty or damaged media fails local admission closed and requires deliberate
  physical recovery rather than manufacturing a new trust root at boot.
- Deterministic boot recovery failure is credential-domain quarantine, not a
  reason to suppress unrelated LoRa routing or journal policy.
- The lab profile is easy to qualify over the E290's built-in button and USB,
  but it does not protect against a malicious process on the connected host or
  physical flash extraction.

## Validation status and remaining composition gates

- Complete in the portable authority: 23 unit tests, eight public successor
  tests, and 18 compile-fail doctests cover canonical snapshots and ownership,
  lifecycle-specific Add/Activate/Abort planning, exact pending selection,
  structural and exact-transition preflight retention, lifecycle conflicts,
  cross-predecessor rejection, and secret-bearing type boundaries.
- Complete: 32 portable fake-NOR tests cover exact header/layout bytes, the
  independently generated digest golden vector, the read-only four-way empty-
  provision classifier, canonical empty provisioning and deterministic no-
  erase recovery, both sector roles, generic and typed lifecycle successor
  commit/readback/retirement/reconciliation, mounted-store pending selection,
  publication gating, cleanup, wrong binding, conflict, unknown media, revision
  exhaustion, corrupt-successor non-fallback, and every-byte/lost-reply/read-
  error trajectories at the program, readback, retirement, and erase
  boundaries.
- Complete: strict host Clippy and warning-free rustdoc plus generic bare-metal
  and ESP32-S3 Xtensa checks.
- Complete in E290 host/target composition: exact partition and eFuse-derived
  binding, immediate post-open mount/recovery ordering, bounded retire then
  cleanup, retained `MountedCredentialStore`, no auto-provisioning, and the
  seven boot admission classes including read-only interrupted initialization;
  resident `CredentialRuntime` retention of the exact binding, mounted authority,
  pairing policy, and permit; forward-only erased/interrupted recovery; and the
  sole-owner fresh-identity/fresh-view initialization port. Cross-store gating
  defers admission behind retained journal work and defers journal mutation or
  acceptance behind in-flight initialization without disabling the service.
  These checks contribute to the E290 host-library gate; they are not powered
  integration or live-authentication evidence.
- Complete as bounded powered erased-media smoke at source `96e38aa`: both
  boards reported `UninitializedErased` with zero recovery steps/writes/erases,
  API/session/bearer closed, LoRa continuing, and exact post-boot credential
  partitions still entirely `0xff`. No credential was initialized or
  authenticated.
- Complete as bounded powered resident-runtime smoke at source `5f3f259`: exact
  two-board image readback, resident pairing policy, erased-media initialization
  eligibility, continuing LoRa, and unchanged all-`0xff` credential partitions.
  No request source invoked initialization, so this is not powered recovery,
  pairing, or authentication evidence.
- Complete in the portable pairing-policy slice: 22 unit tests and four
  compile-fail doctests freeze the exact 2,000/60,000 ms boundaries,
  release-to-rearm behavior, strictly
  increasing connection epochs, ordinary-session invalidation, trusted
  exact erased/interrupted initialization facts and permit ownership, counted
  refused attempts, third-operation draining,
  exact pending begin/proof/activation/abort transitions, operation ownership
  across disconnect, clock regression, overflow faults, and the 256-byte policy-
  owner RAM ceiling. Its feature-free edge is composed only into permanent E290
  firmware; the E290 USB/GPIO bearer invokes status/initialize and routed live
  Begin/ProofStart/Activate/AbortCurrent through node-owned handoffs.
- Complete in the portable initialization-control slice: eight tests freeze all
  four record kinds, every status/result code, exact COBS round trips, zero
  session/tag requirements, payload shapes, unknown-code rejection, and framing
  fault ownership. Its featureless graph reaches only framing and is composed
  only into the permanent E290 product, not a legacy product or HIL graph.
- Complete in the portable live-pairing slice: ADR 0010's 14 unit tests, four
  compile-fail doctests and six independent Python tests freeze all eight
  successful flights, fixed payloads and result vocabularies, COBS bytes,
  transcript/proof/confirmation KATs, every transcript-bound byte and role,
  the actual advanced Active generation, substituted-reference rejection, and
  secret-owner drop behavior. Strict
  host, generic bare-metal and ESP32-S3 Xtensa gates pass. The core is required
  only by the permanent E290 graph and remains forbidden from legacy/HIL graphs.
- Complete in the resident E290 lifecycle and handoff slice: bounded entropy;
  exact connection/window/deadline proof binding; durable Begin/Activate/Abort;
  cleanup ordering; ambiguous commit reconciliation; disconnect, timeout and
  replacement cancellation; fail-closed semantic preflight; exact unsent-owner
  pressure; and compile-time RAM ceilings all pass host and target gates. The
  lifecycle and handoff are scheduled by the node and connected to the shared
  USB control/live byte owner.
- Complete as E290 host/target USB bootstrap composition: the current firmware
  library suite covers the stable-time active-low debouncer, held-low boot,
  clock regression, missed-SOF suspension with bus-reset-delimited epochs,
  sequence exhaustion, duplicate/gap rejection, bounded button/control
  arbitration, latched High-before-Low publication, raw-sample continuity loss,
  fresh-connection publication-latch/debouncer reset, endpoint-FIFO response
  ownership, depth-one pressure, capability-free handoff, coarse public result
  mapping, single USB ownership, GPIO21 pull-up composition, initialization-
  before-journal scheduling, shared control/live decoding, causal ordering,
  durable reply correlation, reset-generation blocking, physical detachment,
  USB-RAM scrubbing, and earliest-Rust-entry boot quarantine. At this milestone,
  the 125-test firmware library, 42 focused host-client tests, full 189-test xtask suite,
  strict host/target Clippy, rustdoc, release linking, graph policy, and image-
  size ceilings pass. Final measurements are recorded with the qualified image.
- Complete as historical bounded powered bootstrap control: the 544,371/3,548/469,280/
  1,017,199-byte text/data/BSS/total ELF packaged a 587,456-byte application.
  Its 652,992-byte explicit-16-MiB repository-partition-table image has SHA-256
  `1727a14b58a076d65ea12feb61b564d5dfc66d6c6f0b9a8ddd39fc773332705c` and was
  flashed to both boards. Both returned `initialization-required` and
  `physical-presence-required`. Five-second no-button workflows on both boards
  advanced through sequences 0--47 before their overall deadlines. Subsequent
  8 KiB credential-partition readbacks on both boards were entirely `0xff` with
  SHA-256
  `7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`,
  confirming zero writes. That historical run did not qualify a post-write
  readback.
- Complete as preceding-image boot-quarantine and no-mutation control: the
  701,744-byte image with SHA-256
  `14d9fd6dd482c47baa9afd2fda6a5ba1d69f46785bf23ae29f6b9fe561e4b212`
  matched exact address-zero reads from both boards. Both boards reattached and
  served sequence-zero `initialization-required` after the induced hard reset.
  Simultaneous 120-second no-button workflows stayed responsive through
  sequences 1102 and 1100, and both exact credential-partition reads remained
  entirely `0xff`. This does not qualify a successful hold/write, controlled
  power cuts, or the ROM/bootloader interval before the earliest Rust entry.
- Complete as a historical dormant-handoff regression: the 718,688-byte
  authenticated-node-foundation image with SHA-256
  `e20f6191cb2bfa78fbd7f3d588eb418913da3f1f89e3b80a4db0a28abaf414ea`
  matched exact address-zero reads from both boards. Both returned and then
  recovered sequence-zero `initialization-required`, and both credential
  partitions remained entirely `0xff`. No authenticated record was admitted by
  that image; this does not qualify the subsequently composed minimal bearer.
- Complete as the first bounded powered happy path on MAC
  `ac:a7:04:e1:3e:88`: the 748,016-byte image with SHA-256
  `4864180ab1d51081758ec3bec53068d6c75316209a2ccc269a0aad48c210fe2c`
  matched exact address-zero readback. A physical hold completed initialization,
  pairing committed Active generation 3, the exact post-activation credential
  partition read SHA-256
  `ce7c4937b0e72c3a8a332a040267b0c408a8946ea75f22041688cd7f5bd99170`,
  and a fresh post-reset USB epoch completed authenticated
  `system.capabilities`. Only the digest of the secret-bearing partition is
  recorded. This does not qualify Pending/Abort images, ambiguity/fault cuts,
  or the deferred session features.
- Complete as the next bounded powered path on both E290s: the API 1.1
  751,712-byte image with SHA-256
  `4285fcaa9df6a6f0314ed4735377ea986b0efcafafc2710ad7594489a49b4795`
  matched exact address-zero readbacks. The Active sender completed
  authenticated identity, durable submission, and sequential status requests;
  the receiver decrypted matching LoRa DATA and returned a valid proof. Sender
  `Delivered` status survived full USB re-enumeration and a fresh session. This
  does not qualify application inbox consumption or the deferred lifecycle and
  fault cases.
- The host asserts DTR, clears RTS, and keeps initialize on one open TTY. TTY
  reopen is not an epoch boundary; only USB bus reset is. Status defaults to 15
  seconds and initialize to 120 seconds. A post-send I/O failure or timed-out
  request leaves its last sequence consumed-or-ambiguous. `u64::MAX` is refused
  and cannot wrap.
- The live host client reserves and verifies an owner-only state path before
  Begin, atomically installs a complete Pending credential before ProofStart,
  and can `resume` a known Pending file in a fresh confirmed window. It
  zeroizes serial/state scratch and validates the returned device identity.
  Secure pair/resume persistence is Unix-only. Pair and resume require three-
  and two-request sequence headroom respectively. An ambiguous Activate remains
  unreconciled by the current host; the client retains the file and must not
  guess Active or abort.
- Pairing integration tests must still cover powered GPIO debounce/sampling,
  USB reset/suspend/resume behavior, disconnect at every secret/proof/completion boundary,
  proof replay and wrong transcript binding, unique ID/PSK allocation, E290
  interrupted-initialization powered recovery, retained same-boot ambiguity
  under real faults, and 16-ID exhaustion.
- Authenticated session/bearer composition must keep the ADR 0004 coordinator as the
  only flash and mutable-authority owner, zeroize temporary secrets, preserve LoRa
  scheduling under USB pressure, and prove no API/session service starts from
  unformatted, unrecovered, or conflicting media. It must also prove USB
  disconnect closes pairing. The current no-op logging selection mechanically
  excludes application log bytes from the binary COBS stream; powered capture
  must still verify that ownership.
- Powered E290 tests must interrupt initial provisioning, pending creation,
  activation, retirement, and cleanup at every reachable boundary and verify
  exact flash readback before this path is enabled outside developer/HIL use.

The software-routed pre-authentication lifecycle and durable credential mutation
are complete. Powered qualification now covers initialization/pair/activation/
reboot, capabilities/identity, one durable submission, peer proof, sequential
status, and a fresh post-re-enumeration status session. This ADR does not claim
powered-cut recovery, exact Pending/Abort readbacks, application inbox
consumption, or a production session/API security profile.
