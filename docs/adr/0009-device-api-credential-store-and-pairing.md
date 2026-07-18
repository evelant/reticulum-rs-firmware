# ADR 0009: Device-API credential store and initial pairing policy

- **Status:** accepted design; portable lifecycle-safe authority/store path,
  interrupted-initialization classifier, E290 boot/coordinator mount integration,
  and pairing-admission policy implemented; E290 initialization/mutation runtime,
  firmware/bearer composition, and powered pairing qualification pending
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0006](0006-authenticated-local-api-bearer.md), and
  [ADR 0007](0007-device-api-credential-authority.md)

## Context

The portable credential authority and its canonical 2,048-byte semantic image
now expose lifecycle-specific successor planning, the physical store carries
those opaque transitions through commit and reconciliation, permanent E290
firmware mounts/recovers its physical store, and a portable policy owner freezes
physical-presence window admission. The firmware still cannot admit an
authenticated external request until one device-owned authority survives power
loss and the policy is composed with the sole flash owner and a bearer. Pairing
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

Boot returns the six product states `Ready`, `AuthOnly` (the Rust
`AuthenticationOnly` variant), `Uninitialized` (the `UninitializedErased`
variant), `Blocked`, `Corrupt`, or `Backend`. Only `Ready` permits a future
credential mutation; `Ready` and `AuthOnly` may retain a publishable authority,
but this image has no session or bearer and therefore performs no live
authentication.
Every successfully mounted owner, including a blocked or cleanup-failed owner,
is retained in `ProductStorageCoordinator`. Erased media is never provisioned
automatically.

### Make empty-store initialization explicit and non-pairing

No boot path automatically formats or pairs an empty store. With an existing
Reticulum identity and both credential sectors exactly erased, local API
admission remains disabled. The only initial action allowed is an explicit
erased-only initialization command received on the button-confirmed USB
connection described below. It rechecks that both sectors are fully erased and
commits the canonical empty revision-1 authority. Programmed noncanonical
media is never reformatted by this path.

The portable store also provides the read-only
`classify_empty_provision_media` four-way classifier for this boundary:
`ExactlyErased`, `RecoverableInterrupted`,
`CommittedEmptyRevision1`, or `NotRecoverable`. The recoverable case is limited
to an ordered monotonic prefix of the canonical device-bound empty revision-1
program/digest/commit trajectory with the other sector and all forbidden bytes
erased. Classification never writes or erases. E290 boot does not yet map this
result to an explicit interrupted-initialization state, and its resident
coordinator does not yet retain a same-boot ambiguous initialization permit;
those are the next composition slice, not automatic boot recovery.

Initialization creates no credential, offers no secret, and does not
implicitly start pairing. A client must issue a separate pairing begin while a
valid physical-presence window is open. Factory reset and cross-store identity
reset ordering remain a separate design and may not be inferred from erased
credential media.

### Fix the first developer/HIL pairing window

`reticulum-device-api-pairing-policy` implements the allocation-free portable
admission owner. It deliberately has no GPIO, USB, flash, entropy, HMAC, wire,
or executor dependency. A later E290 adapter must supply debounced active-low
GPIO21 observations and use USB Serial/JTAG as the only bearer for this first
developer/HIL ceremony.

The exact admission contract is:

- the current accepted connection must observe the button released before a
  low observation can arm it; boot, connection, replacement, closure, and clock
  fault all require another release before rearming, so a button held low cannot
  open or automatically reopen a window;
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
erased initialization and pending abort do not spend this budget.

The third classified request is still evaluated: it may be refused or may
return an admitted operation permit, but it immediately closes the window to
new work. If admitted, the owner enters a draining state and retains that exact
operation until its definite result is reconciled. Timeout, disconnect, or
connection replacement likewise closes window admission without discarding an
already accepted operation. A dropped or ambiguous operation permit therefore
leaves the policy fail closed; it must be retained across the asynchronous
physical operation and completed only with a definite result.

Initialization admission consumes trusted `identity_ready` and
`exactly_erased` facts from the sole identity/flash owner and additionally
requires no durable pending enrollment. Begin admission consumes trusted
`mutation_ready`, retained-capacity-available, and next-revision-available facts
and requires the pending slot to be empty. These values are policy preconditions,
not evidence that media is erased, writable, or unchanged: the sole physical
owner must revalidate them immediately before mutation. The policy starts from
either no pending record or one already validated durable pending reference and
tracks only its non-secret credential ID and generation.

The future mutation coordinator, not the portable policy, allocates for each
accepted begin a never-before-used random nonzero 128-bit credential ID and
random 256-bit PSK, plus the next authority revision. The sole owner commits
and exactly reads back that `Pending` successor before it offers the ID and PSK
to the same bound USB connection. A disconnect or failed write never converts
`Pending` to `Active` and never permits a new pending ID to bypass it.

The client proves PSK possession with a domain-separated HMAC-SHA256 over a
fresh single-use device challenge and the pairing transcript, including the
credential ID and generation, stable public device-API ID, and current
connection/window binding. The later wire specification must freeze the exact
record encoding and domain bytes before implementation; it may not weaken
these bindings or accept a client-supplied principal/permission assertion.
Failure responses do not reveal whether any unrelated credential exists.

After valid proof, the future mutation coordinator constructs the exact next
successor changing that record from `Pending` to `Active`. It commits, reads
back, retires the old snapshot, and publishes the new authority before sending
pairing completion or allowing session authentication. Reporting that exact
durable, publishable result to the policy closes an otherwise-open window as
successfully activated. Thus both `Pending` before secret offer and `Active`
before completion are durable facts.

The current firmware logger shares the USB Serial/JTAG stream. Binary COBS
pairing/session service must not start while arbitrary text logs can interleave
with its records. Bearer composition must first move logs to a different sink,
disable them for that stream, or encapsulate them as an explicitly typed framed
channel. No raw log byte may appear between binary COBS records.

A surviving `Pending` record remains the only enrollment in progress. A later
button-confirmed window may prove it again if the client retained the secret,
or explicitly abort it by committing a PSK-free revoked/aborted tombstone
before allocating another ID. The firmware never silently restores, reuses,
or discards a pending secret-bearing ID.

The portable policy does not implement or imply live pairing. The remaining
boundary includes:

- an E290 boot class that maps only `RecoverableInterrupted` to explicit
  interrupted initialization, plus a resident same-boot mutation owner that
  retains the exact policy permit and typed physical successor until definite
  reconciliation;
- board debounce/sampling, boot-lifetime USB connection-epoch allocation,
  exclusive bearer arbitration, and exact disconnect classification;
- entropy, unique-ID/PSK allocation and collision handling; exact pairing wire
  records, challenge/HMAC transcript domains, proof verification, response
  delivery, COBS/log separation, and secret zeroization;
- sole-flash mutation, trusted-fact rechecks, ambiguous-result and power-cut
  reconciliation, firmware task composition, and powered hardware qualification.

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
  cleanup, retained `MountedCredentialStore`, no auto-provisioning, and the six
  boot admission classes. These checks contribute to the 37-test E290 host
  suite; they are not powered integration or live-authentication evidence.
- Complete as bounded powered erased-media smoke at source `96e38aa`: both
  boards reported `UninitializedErased` with zero recovery steps/writes/erases,
  API/session/bearer closed, LoRa continuing, and exact post-boot credential
  partitions still entirely `0xff`. No credential was initialized or
  authenticated.
- Complete in the portable pairing-policy slice: focused host tests freeze the
  exact 2,000/60,000 ms boundaries, release-to-rearm behavior, strictly
  increasing connection epochs, ordinary-session invalidation, trusted
  initialization facts, counted refused attempts, third-operation draining,
  exact pending begin/proof/activation/abort transitions, operation ownership
  across disconnect, clock regression, overflow faults, and the 256-byte policy-
  owner RAM ceiling. This crate is not composed into firmware or a bearer.
- Pairing integration tests must still cover real GPIO debounce/sampling,
  exclusive USB ownership, disconnect at every secret/proof/completion boundary,
  proof replay and wrong transcript binding, unique ID/PSK allocation, E290
  interrupted-initialization boot/runtime composition, retained same-boot
  ambiguity, and 16-ID exhaustion.
- Live mutation/bearer composition must keep the ADR 0004 coordinator as the
  only flash and mutable-authority owner, zeroize temporary secrets, preserve LoRa
  scheduling under USB pressure, and prove no API/session service starts from
  unformatted, unrecovered, or conflicting media. It must also prove USB
  disconnect closes pairing and that logging cannot interleave with binary
  COBS framing.
- Powered E290 tests must interrupt initial provisioning, pending creation,
  activation, retirement, and cleanup at every reachable boundary and verify
  exact flash readback before this path is enabled outside developer/HIL use.

No live pairing, credential mutation through this policy, bearer composition,
powered-cut recovery, or live-authentication gate is claimed complete by this
ADR.
