# ADR 0008: Durable authorization provenance and journal schema 2

- **Status:** accepted
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0006](0006-authenticated-local-api-bearer.md), and
  [ADR 0007](0007-device-api-credential-authority.md)

## Context

The immutable credential authority can now revalidate an authenticated session
grant immediately before logical dispatch. That revalidation establishes the
credential ID and generation, the complete authority revision, the applied
authorization-policy version, the principal, and the exact granted permission
mask. The schema-1 durable acceptance record preserves only the principal,
idempotency key, semantic intent, and a redundant content digest.

That gap was acceptable only while no external mutating bearer existed, but not
once a USB, BLE, or Wi-Fi client could create durable work. A later credential rotation
or policy update would otherwise make it impossible to determine which exact
device-owned authorization facts admitted an existing submission. Reconstructing
those facts from current authority state would be historically false, and
accepting them from request CBOR would invert the trust boundary.

The maximum experimental payload is already fixed at 383 bytes. Adding the
authorization facts naively would exceed the journal's 512-byte canonical body
ceiling. The existing serialized content SHA-256 is redundant because it is a
deterministic function of the complete persisted intent and the physical
journal integrity chain already protects every encoded intent byte.

## Decision

### Carry provenance only through trusted dispatch state

`reticulum-device-api` owns a non-wire `DispatchProvenance` containing:

- the 128-bit credential ID;
- the nonzero credential generation;
- the nonzero authority revision; and
- the nonzero authorization-policy version.

An authenticated `DispatchContext` contains one validated provenance value in
addition to the principal and permission mask. It is never decoded from device-
API CBOR. `reticulum-device-api-credentials` mints it only from the current
active record and complete authority snapshot during grant revalidation.

The durable adapter authorizes the operation first, then maps the trusted
context into a storage-owned value. The dependency direction remains explicit:
the storage model does not depend on device API, credentials, sessions, framing,
or a bearer, and the adapter does not depend on the credential crate.

### Persist an exact authorization snapshot with every acceptance

`reticulum-storage-model` owns `AuthorizationSnapshot`, containing the same
four provenance fields plus the exact granted permission bits. Construction and
decode reject:

- an all-zero credential ID;
- zero generation, authority revision, or policy version;
- a generation later than the observed authority revision;
- unknown persisted permission bits; or
- a mask that lacks the permission required to submit experimental RNS DATA.

`AcceptanceCandidate` and `Accepted` contain this validated snapshot. The
principal remains a separate accepted field: it is the stable owner used for
status scoping and idempotency, while the snapshot explains the exact credential
and policy state that authorized the mutation.

Principal-scoped idempotency continues to compare only principal, key, and
semantic intent. A retry after credential rotation therefore returns the
original submission ID and preserves the original acceptance evidence. It does
not append new provenance or rewrite history. A contradictory replay of the
same durable submission ID remains a semantic conflict. If an append result is
ambiguous, the actor retains the exact planned acceptance including provenance;
a differently provenanced retry remains busy until that owner is reconciled.

### Derive, but do not serialize, the semantic content digest

`Accepted` continues to expose the domain-separated semantic content SHA-256,
but schema 2 does not encode that 32-byte value. Construction and decode compute
it from the complete destination and payload. This removes the encoded bytes
needed by the authorization snapshot without changing:

- the 383-byte API payload limit;
- the 512-byte canonical journal-body limit;
- fixed physical slot or partition geometry;
- submission lifetime reservation; or
- idempotency and content-conflict semantics.

The physical journal SHA-256 chain remains corruption detection rather than
authorization, authentication, or confidentiality.

### Make the semantic transition explicit and non-upgradable

The journal semantic schema advances from 1 to 2. The physical format version,
slot geometry, two-bank layout, and partition binding remain unchanged.

Schema-1 acceptance records contain no credential or policy evidence, so they
cannot be truthfully upgraded. Mount identifies a valid older manifest as
`UnsupportedSemanticVersion(1)` before replay and performs no writes or erases.
The E290 product treats that mount result like other optional submission-service
failures: it keeps the sole flash owner resident, closes local submission
admission, and continues route-only LoRa operation.

Development migration is explicitly destructive and journal-local. The
operator backs up flash, erases only the checked `node_journal` range, verifies
that entire range is erased, and runs an explicitly selected schema-2 journal
reprovision path. It preserves `node_identity`, `announce_clock`, and every
other partition. Ordinary firmware boot never erases, fabricates provenance,
or silently provisions an erased journal belonging to an existing identity.

No legacy record variant is retained. No external mutating bearer has shipped,
so carrying permanently unprovenanced acceptance state would weaken the new
gate without preserving user data.

## Consequences

- Every newly accepted mutation has durable evidence for the exact credential,
  authority revision, policy version, and permission mask that authorized it.
- Credential rotation does not change idempotency identity or rewrite original
  evidence.
- The API payload and physical journal geometry remain stable.
- Existing schema-1 development journals require an explicit journal-only
  reprovision before local durable admission reopens.
- Credential persistence, pairing lifecycle, async handoff composition, and the
  first physical USB bearer are now composed and pass one bounded powered happy
  path; broader admin/fault/lifecycle qualification remains a separate gate.

## Qualification requirements

- Canonical schema-2 acceptance goldens, every truncated prefix, noncanonical
  alternatives, invalid authorization facts, and the maximum 383-byte payload.
- Digest recomputation after decode with no serialized digest field.
- Replay under rotated provenance preserving the original acceptance, exact
  conflict behavior, and ambiguous-pending ownership.
- Journal append, remount, and compaction preserving every snapshot field.
- A valid schema-1 manifest returning typed unsupported-version with zero media
  mutation and route-only product behavior.
- Adapter tests proving authorization and provenance validation precede every
  port call, plus an authority-to-session-to-journal remount regression.
- Host, RISC-V, Xtensa, strict Clippy/rustdoc, dependency-graph, and fake-NOR
  product-composition gates before the schema is committed.
