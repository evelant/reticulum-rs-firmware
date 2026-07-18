# ADR 0007: Immutable device-API credential authority

- **Status:** accepted for the portable authority snapshot, canonical image,
  and lifecycle-specific pairing successors; ADR 0009 typed physical store path
  and E290 boot mount composition implemented; authority/session, pairing
  manager, and first powered API/DATA/peer-proof path composed
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0004](0004-sole-flash-coordinator.md) and
  [ADR 0006](0006-authenticated-local-api-bearer.md)

## Context

ADR 0006 deliberately made an authenticated session grant a reference rather
than authorization truth. The session proves possession of one selected PSK
and carries only the credential ID, credential generation and reply-routing
facts. Principal and permission values must come from current device-owned
state immediately before logical dispatch, including operations whose logical
permission is public.

That boundary needs to serve three different consumers without creating a
cycle or a second authority:

- a credential store must validate durable records without depending on
  session crypto, COBS, a bearer or firmware;
- the session must obtain one bounded zeroizing PSK owner for a handshake; and
- the serialized node/storage owner must revalidate a session-minted reference
  immediately before its synchronous logical dispatch and any resulting
  durable acceptance.

At the time this authority boundary was selected, credential persistence,
pairing, durable authorization provenance, and bearer composition were not
specified well enough to expose on hardware. ADR 0008 has since completed the
schema-2 provenance contract. ADR 0009 now selects a dedicated E290 credential
partition, physical store envelope, and bounded initial pairing policy. Their
portable store implementation and E290 boot/coordinator ownership are now
complete. External authority/session composition, pairing, and the physical USB
bearer now pass one bounded powered happy path; this ADR's session record
vocabulary still does not double as an unauthenticated pairing exchange.

## Decision

### Put credential semantics below the session

`reticulum-device-api-credentials` owns the allocation-free semantic
vocabulary and one immutable validated boot snapshot. The session depends on
this crate for `CredentialId` and `CredentialGeneration`; the credential crate
has no dependency on the session, framing, handoff, adapter, storage, executor,
ESP HAL, Reticulum identity or radio graph. This direction lets a later raw-NOR
store decode and validate authority state without pulling handshake crypto or a
physical bearer into the sole flash owner.

The first product profile fixes the retained-record table at 16 entries. The
capacity includes pending and revoked records as well as active credentials;
the canonical semantic image is exactly 16 fixed 128-byte slots (2,048 bytes),
not a promise that every client profile may enroll 16 active clients. ADR 0009
adopts that as a lifetime ID/tombstone ceiling inside a separately versioned
physical sector envelope. One selected credential remains at or below 64
bytes.

### Validate the complete immutable snapshot before service

`CredentialAuthorityBuilder` accepts one nonzero global
`AuthorityRevision`, validates every exact `CredentialRecord`, and consumes the
record into a fixed `Option` table. Each successful insertion returns the
consumed builder. Rejection returns the exact secret-bearing record owner where
one was supplied and drops the partial builder, zeroizing every earlier secret;
safe code therefore cannot ignore one corrupt record and publish a valid
prefix. `finish()` exposes an immutable `CredentialAuthority` only from the
all-success path. There is no in-place pair, revoke, rotate, reset or
permission-update API in this slice.

A canonical snapshot requires:

- a nonzero authority revision, credential generation, principal and
  authorization-policy version;
- nonzero credential IDs and PSKs, reserving erased zero values;
- globally unique retained credential IDs and generations;
- distinct PSKs for every secret-bearing credential ID;
- `created_revision <= modified_revision == credential_generation <=
  authority_revision`;
- exactly one PSK for `Pending` and `Active` records; and
- no PSK plus one bounded revocation reason for a `Revoked` tombstone.

`CredentialRecord`, `SelectedCredential` and rejected secret-bearing owners are
not cloneable, copyable or debug-formattable. Active and pending secrets use
`Zeroizing<[u8; 32]>`. A revoked tombstone retains the credential ID,
generation, principal, policy/audit facts and reason, but never a PSK and never
permissions capable of authorizing an operation.

The permission bit vocabulary is stable independently of Cargo features.
`Permissions::from_bits` rejects unknown bits rather than allowing a
feature-disabled build to reinterpret persisted authority.

### Use one global, non-repeating revision space

Every credential mutation allocates the next nonzero global authority
revision without wrapping. A record's current generation is the revision at
which its current status, secret or authorization policy became durable.
Changing an existing record's permissions or PSK, activating it, or revoking it
therefore invalidates that credential's older session grants. Enrolling a
replacement under a new ID deliberately leaves the old credential valid during
the bounded rotation overlap; only the later old-record revocation invalidates
its grants. Revision exhaustion is terminal.

Boot validation alone cannot prove that two independently valid snapshots form
a safe live transition. `CredentialAuthority::plan_successor` therefore
consumes a candidate and requires the authority revision to advance exactly
once, exactly one record to change, every authorization-relevant change to use
that fresh revision as its generation, and every retained ID to remain present
or become a PSK-free tombstone. Immutable enrollment audit facts and revoked
tombstones cannot change. The resulting opaque plan borrows the old authority
and owns the candidate until the sole store either drops it or asserts
durable commit/readback with `publish_after_commit`. Rejected plans retain the
complete candidate owner without debug-formatting its secrets.
`publish_after_commit` is necessarily a public caller assertion because this
portable crate cannot observe raw NOR or grant friend-crate visibility. The
type system does not prove the physical commit or prevent other linked Rust
code from calling it; sole-store use is a composition and review obligation.

This is structural monotonicity, not lifecycle authorization. The generic
planner does not by itself prove physical presence, possession of a staged PSK,
administrator intent or an allowed status transition. The implemented pairing
planners authorize only adding the sole `Pending` record, activating that exact
pending record, or replacing it with a PSK-free aborted tombstone before the
generic successor defense runs. Unrelated administration still needs its own
policy owner and lifecycle-specific planner rather than constructing an
arbitrary candidate in product composition.

`NewPendingCredential::new` consumes an existing `Zeroizing<[u8; 32]>` owner;
the API never accepts a plain PSK array that could remain in caller storage on
an early return. A lifecycle plan also carries a private zeroizing binding to
the exact source pending reference and record facts. Physical-store preflight
first proves generic structural succession and then proves the exact declared
Add/Activate/Abort delta against its supplied mounted predecessor. Typed
`Structural` and `TransitionMismatch` failures retain the complete opaque
candidate for disposal or retry without exposing a bare authority.

Safe rotation enrolls and proves a replacement credential under a new ID and
generation before a later durable transaction revokes the old record. The two
credentials may name the same principal during that bounded overlap.
Revocation prevents work that has not reached durable acceptance; it does not
undo an already accepted principal-owned operation.

### Select authentication material without transferring authorization truth

`CredentialAuthority::select_for_handshake(id)` scans the fixed table with
constant-time ID comparison and no early successful return. Missing, pending
and revoked records all produce the same `CredentialUnavailable` result. Only
an active record yields a non-cloneable, zeroizing `SelectedCredential`
containing its ID, generation and PSK.

`ActiveCredential::from_selected` consumes that exact zeroizing owner into the
session KDF. The trusted cross-crate transfer API can read or copy those bytes;
the guarantee is zeroizing ownership and no `Clone`/`Debug`, not secrecy from
arbitrary linked firmware code. A selection is not an authorization snapshot:
rotation or revocation after selection remains safe because each admitted
request carries the selected generation and must be revalidated again.

A future separate bearer task may request a `SelectedCredential` from the sole
credential owner over a bounded typed handoff. That ephemeral PSK owner does
not make the bearer a second mutable authority. No complete authority snapshot
or raw flash capability crosses that boundary.

### Revalidate grants through a borrowing dispatch lease

`AuthenticatedGrant::revalidate(&authority)` checks the exact ID and generation
against a current active record and returns a non-cloneable `DispatchLease`.
The lease derives `DispatchContext` only from the record's device-owned
principal and permissions and also exposes non-secret credential generation,
authority revision and authorization-policy version for diagnostics and future
durable provenance.

The lease immutably borrows the authority and exposes no raw context getter.
`with_dispatch_context` supplies a higher-ranked borrow intended to wrap the
immediate synchronous `reticulum-device-api-adapter::dispatch` call, while the
exact `DispatchContext` value is non-cloneable and non-copyable. This prevents
moving that exact value out of the callback and freezes the borrowed authority,
but it is not an unforgeable capability: `DispatchContext` remains a trusted
logical API value whose public scalar getters and constructor let arbitrary
linked Rust code reconstruct equivalent facts. Immediate dispatch and no
fallback are sole-owner integration and review obligations, not compiler-
enforced security against malicious firmware code. If dispatch later moves to
another task, that receiving serialized owner revalidates the credential
reference again instead.

Revalidation failure is terminal for that request and is never converted to
`DispatchContext::UNAUTHENTICATED`. Doing so would incorrectly make a stale or
revoked credential eligible for logical operations whose permission policy is
public. If the logical envelope can be recovered safely, a future bearer may
return an authenticated `AuthenticationRequired` response and then close the
session; otherwise it closes according to the established-stream policy. No
adapter or storage port is invoked on rejection.

The host integration test exercises authority selection, handshake,
authenticated request ownership types, exact CBOR decode, grant revalidation,
synchronous adapter acceptance, logical response encoding and authenticated
reply framing. It does not traverse the asynchronous handoff channels. A second
case replaces the authority with a PSK-free revoked tombstone after request
admission and proves authority revalidation rejects. The composed node owner
also regresses that rejection with neither unauthenticated fallback nor a port
call; broader powered rejection/fault qualification remains open.

### Keep persistence implementation behind the selected physical contract

This semantic slice does not itself make the immutable snapshot durable. The
ADR 0009 credential store, now boot-composed under the ADR 0004 product storage
coordinator, supplies the following physical rules:

- use a dedicated checked raw-NOR range and explicit physical binding;
- commit and read back a complete replacement snapshot before publishing it;
- retain and reconcile one ambiguous mutation before unrelated credential or
  application mutation;
- fail local API service closed on unknown, conflicting or unreconciled media;
- preserve the global revision across revoke, rotation and credential reset;
  and
- treat hashes as corruption detection only until secure boot and flash
  encryption provide a reviewed physical-attacker story.

`device_config` remains reserved and is not silently treated as credential
storage. ADR 0009 assigns the distinct plaintext developer/HIL
`api_credentials` raw-NOR range at `0x614000..0x616000` and selects its
two-sector commit/retire contract. The target validates that range and exact
eFuse-derived binding, immediately mounts/recovers it after flash open, and
retains any mounted owner. Live credential mutation and the bounded powered
happy path are composed; cross-store reset and broader fault qualification
remain open.

Pairing remains outside the session core. ADR 0009's bounded manager owns a
roughly two-second GPIO21 confirmation, exclusive
60-second USB Serial/JTAG window, three-attempt ceiling, one pending enrollment,
durable-before-offer `Pending`, HMAC possession proof, and durable-before-
completion `Active` transition. Empty media never pairs or initializes
automatically. Production pairing adds an independently confirmed
display/code/QR or equivalent out-of-band ceremony. Neither policy reuses
Reticulum identity private material.

### Close durable authorization provenance in semantic schema 2

ADR 0008 advances the durable semantic schema to 2. `DispatchLease` now mints a
validated non-wire `DispatchProvenance`; after logical authorization, the
adapter maps it plus the exact granted permission mask into a storage-owned
`AuthorizationSnapshot`. Every new acceptance persists the credential ID and
generation, complete authority revision, policy version, and permission mask.
Retries after rotation preserve the original acceptance evidence. Schema-1
media is typed unsupported and cannot be silently upgraded because those facts
never existed.

## Consequences

- Principal and permissions now have one device-owned semantic source below
  session establishment, without coupling durable credential decoding to
  handshake crypto or a bearer.
- Session handshakes consume an exact zeroizing selection, while the intended
  sole-owner request path rechecks current generation and policy through a
  borrow immediately before dispatch.
- Pending credentials cannot authenticate; revoked credentials retain a
  PSK-free non-reuse tombstone; no revalidation failure becomes an
  unauthenticated request.
- The portable crate now fixes a canonical 2,048-byte, 16-slot semantic
  snapshot image without inventing physical flash headers, commit mechanics,
  pairing, reset or USB behavior. The exact image owner zeroizes on drop and
  decoding consumes it before revalidating every record through the builder.
- The minimal USB authority/session owner and physical bearer are composed.
  Explicit initialization/pairing and a bounded authenticated submission/peer-
  proof/status exchange now pass on powered hardware; broader lifecycle and
  fault qualification remains open.

## Validation status

- Complete: 23 unit tests, eight public successor regressions and 18 compile-
  fail doctests cover active and exact-pending selection, lease derivation,
  fail-closed builder ownership, safe live and lifecycle-specific successors,
  opaque store-candidate preflight, unpublished-candidate recovery, lifecycle
  conflicts, same-generation escalation rejection, revoked tombstones, the
  golden canonical snapshot layout and round trip, noncanonical image
  rejection, generation/revision and permission vocabularies, image/secret/
  lease ownership, non-escaping dispatch context, and bounded E290 RAM.
- Complete: the session suite contains twelve tests, including one direct
  authority-to-adapter request/reply path with exact durable provenance and one revoke-after-admission
  authority-revalidation rejection. Composed handoff/no-fallback proof remains.
- Complete in ADR 0009's portable store: two-sector commit/retire envelope,
  operation-scoped binding, typed lifecycle commit/reconciliation, mounted-
  store pending selection, four-way interrupted-initialization classification,
  explicit no-erase provisioning recovery, and 32 fake-NOR cut/error tests.
- Complete outside this semantic crate: E290 exact-binding boot mount/recovery,
  retained coordinator ownership, no auto-provisioning, and credential-domain
  failure isolation pass host and target build checks.
- Remaining outside this semantic crate: pairing-manager implementation,
  rotation/reset composition, and powered
  cut/pairing tests.
- Complete: semantic schema 2 persists and replays exact authorization-policy
  provenance while preserving the 383-byte payload and 512-byte actor pending
  owner ceiling; see ADR 0008.
- Remaining: powered bearer tests.
