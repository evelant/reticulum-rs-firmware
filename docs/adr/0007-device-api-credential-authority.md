# ADR 0007: Immutable device-API credential authority and deferred persistence

- **Status:** accepted for the portable authority snapshot; persistence and pairing pending
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

- a future credential store must validate durable records without depending on
  session crypto, COBS, a bearer or firmware;
- the session must obtain one bounded zeroizing PSK owner for a handshake; and
- the serialized node/storage owner must revalidate a session-minted reference
  immediately before its synchronous logical dispatch and any resulting
  durable acceptance.

Credential persistence, pairing and bearer composition are not yet specified
well enough to expose on hardware. In particular, the qualification session's
record-kind vocabulary has no unauthenticated pairing exchange, the current
E290 partition map has no dedicated credential-store contract, and the durable
submission schema records the principal and operation intent but no explicit
credential-generation or authorization-policy provenance.

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
it is a semantic/RAM ceiling, not a frozen physical format or a promise that
every client profile may enroll 16 active clients. The current host size gate
keeps the complete authority at or below 2,048 bytes and one selected
credential at or below 64 bytes.

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

Every future credential mutation allocates the next nonzero global authority
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
and owns the candidate until a future sole store either drops it or asserts
durable commit/readback with `publish_after_commit`. Rejected plans retain the
complete candidate owner without debug-formatting its secrets.

This is structural monotonicity, not lifecycle authorization. The planner does
not by itself prove physical presence, possession of a staged PSK, administrator
intent or an allowed status transition. Before persistence lands, the sole
pairing/admin mutation owner must define and test whether a new record may begin
`Active`, exactly what `Pending -> Active` may change, whether an active record
may return to `Pending`, and whether principal or permission reassignment is
allowed. It authorizes those semantics before constructing a candidate; the
successor planner then enforces revision/generation integrity.

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
admission and proves authority revalidation rejects; the future composed owner
must still prove it neither falls back to unauthenticated dispatch nor invokes
a port after that error.

### Keep persistence and pairing behind explicit later contracts

This slice does not make the immutable snapshot durable. A later credential
store remains owned by the ADR 0004 product storage coordinator and must:

- use a dedicated checked raw-NOR range and explicit physical binding;
- commit and read back a complete replacement snapshot before publishing it;
- retain and reconcile one ambiguous mutation before unrelated credential or
  application mutation;
- fail local API service closed on unknown, conflicting or unreconciled media;
- preserve the global revision across revoke, rotation and credential reset;
  and
- treat hashes as corruption detection only until secure boot and flash
  encryption provide a reviewed physical-attacker story.

The current `device_config` partition remains reserved and is not silently
treated as credential storage. Partition selection, physical format, power-cut
recovery and a cross-store factory-reset transaction require a separate
decision and powered qualification.

Pairing likewise remains outside the session core. A later bounded manager must
own the exclusive physical-presence window, monotonic timeout, attempt budget,
connection binding, secret delivery and proof-of-possession transition from
`Pending` to `Active`. The first lab policy may trust the connected USB host
only during the explicitly button-confirmed window accepted by ADR 0006.
Production pairing adds an independently confirmed display/code/QR or
equivalent out-of-band ceremony. Neither policy reuses Reticulum identity
private material.

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
- The first slice is host- and target-checkable without inventing flash,
  pairing, reset or USB behavior.
- Live external admission remains disabled until persistence, pairing/rate
  policy, firmware ownership, and a physical bearer are complete.

## Validation status

- Complete: six focused unit tests, seven public successor regressions and seven
  compile-fail doctests cover selection, lease derivation, fail-closed builder
  ownership, safe live successors, same-generation escalation rejection,
  revoked tombstones, canonical snapshot rejection, generation/revision and
  permission vocabularies, non-escaping dispatch context, secret shape and
  bounded E290 RAM.
- Complete: the session suite contains twelve tests, including one direct
  authority-to-adapter request/reply path with exact durable provenance and one revoke-after-admission
  authority-revalidation rejection. Composed handoff/no-fallback proof remains.
- Remaining: persistent physical format, operation-scoped binding, ambiguous
  mutation reconciliation and exhaustive power-cut tests.
- Remaining: pairing protocol, physical-presence UI, timeouts, rate limits,
  rotation, reset and firmware task composition.
- Complete: semantic schema 2 persists and replays exact authorization-policy
  provenance while preserving the 383-byte payload and 512-byte actor pending
  owner ceiling; see ADR 0008.
- Remaining: powered bearer tests.
