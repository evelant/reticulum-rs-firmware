# ADR 0013: Bounded LXMF wire and service ownership boundary

- **Status:** accepted for the first wire and application-ingress tranches
- **Date:** 2026-07-20
- **Decision owners:** project maintainers
- **Extends:** [ADR 0002](0002-rete-provisional-foundation.md),
  [ADR 0003](0003-lora-first-interface-fabric.md),
  [ADR 0011](0011-durable-rns-inbox-qualification.md), and
  [ADR 0012](0012-application-event-and-resource-ownership.md)

## Context

The product needs one interoperable LXMF implementation shared by a headless
router, propagation service, optional local messaging client, and every local
USB, BLE, or Wi-Fi bearer. LXMF signatures and message IDs depend on Python's
exact hashed-payload rule, while its fields map can contain heterogeneous,
nested MessagePack keys and values. For an unstamped four-item payload Python
hashes the received payload bytes directly. When more than four items are
present, Python decodes, takes the first four values, canonically re-encodes
that array, and hashes the result. Decoding into a convenient typed map and
re-encoding under a different policy can therefore change a valid message even
when its apparent values are unchanged. Materializing an unbounded generic
MessagePack tree would also put attacker-controlled heap and recursion costs on
the always-on node.

None of the examined Rust implementations is a complete firmware layer.
LXMF-rs contains useful wire constants, formulas, announce codecs, fixtures,
and delivery semantics, but its complete runtime and dependency graph are not
appropriate for the bare-metal target. Rete's current LXMF codec is not a
compatibility authority: its two-byte stamp/ticket model and restricted fields
representation disagree with current LXMF. The deterministic
`interop/vectors/lxmf-1.0.1-v1.json` corpus instead records exact output from
released Python LXMF 1.0.1 with RNS 1.3.5, including binary content, nested and
unknown fields, 32-byte proof-of-work stamps, 16-byte tickets, delivery-method
boundaries, and negative mutations.

The first implementation must establish the wire contract without also
choosing a durable message schema, retry engine, RNS Resource buffering model,
or physical interface. Combining those concerns now would make later
corrections destructive and would couple LXMF to the temporary raw-RNS inbox
or to LoRa.

## Decision

### Own a narrow, bounded wire crate

The project owns `crates/lxmf-wire`, registered in the workspace as
`reticulum-lxmf-wire`. Its first tranche is limited to LXMF wire parsing,
hashing, ingress normalization, and cryptographic validation. It is a
`#![no_std]`, allocation-free protocol component built around borrowed views,
checked arithmetic, and streaming signature verification. It requires no
attacker-sized scratch buffer, allocator, radio, board, executor,
USB/BLE/Wi-Fi bearer, flash implementation, UI, or hosted LXMF router.

Every operation that walks attacker-sized wire data receives explicit
`WireLimits`. The decoder rejects excessive input length, nesting, collection
cardinality, total visited values, scan work, or trailing/malformed data before
constructing application state. Nesting also has a crate-level hard maximum of
32, independent of a caller's tighter profile limit. Proof-of-work validation
has a separate explicit CPU budget; fixed-size identity, signature, and ticket
checks do not pretend to be wire-size limits. Bounds are product policy supplied
by the caller; a smaller board profile may choose tighter values without
defining a different wire format.

The crate distinguishes complete LXMF bytes from their RNS carrier. It
normalizes opportunistic destination DATA, direct Link DATA, and a completed
direct Resource into the same complete-message view. The normalization API
mentions LXMF/RNS delivery semantics, not LoRa, SX1262, an interface slot, or a
client bearer. Actual RNS Resource segmentation, admission, streaming, and
storage remain outside this crate and fail closed under ADR 0012 until their
bounded contract is qualified.

### Preserve exact MessagePack bytes

The parsed view borrows the input wherever possible. Title and content remain
binary byte strings, and the fields object retains its exact raw MessagePack
slice after a bounded structural walk. Unknown fields, nested values, generic
extension objects, integer widths, ordering, and accepted encodings are not
flattened into a project enum or dynamically owned `Value` tree.

This first tranche admits map keys whose Python equality and hashability can be
checked without allocating: nil, booleans, integers, UTF-8 strings, binary
strings, and generic extensions. It rejects duplicate keys under Python
equality, including boolean/integer aliases. Float, array, and map keys fail
with `UnsupportedMapKey`; MessagePack timestamp extension type `-1` fails with
`UnsupportedTimestampExtension` wherever it occurs because Python datetime
normalization is not modeled yet. Arrays, maps, floats, binary data, and generic
extensions remain valid as nested values, subject to the structural limits.
These typed, fail-closed exclusions are staged compatibility limits, not a
private replacement wire format or a claim that Python LXMF forbids those
forms.

The authentication payload follows Python rather than a blanket raw-byte rule.
For an exact four-item array, signature verification and message-ID derivation
use the received payload bytes. For a stamped array, they use Python's
canonical re-encoding of the first four decoded values and exclude the stamp.
The first tranche accepts only the four- and five-item forms. Until it owns a
bounded canonical MessagePack re-encoder, it accepts a five-item form only
when the retained first-four encodings are already proven Python-canonical, so
the hashed payload can be reconstructed as the canonical `0x94` array header
plus those four raw item slices. Noncanonical stamped input and other array
arities fail closed as unsupported instead of being validated under the wrong
bytes.

A later semantic layer may decode selected standardized fields into typed
views, but it must retain the raw object and cannot make a lossy typed
projection the signed source of truth. Encoding support must either preserve
caller-supplied raw fields where Python hashes them directly or prove its
canonical output byte-for-byte against the Python corpus.

### Keep identity validation behind a project seam

The wire crate parses the source destination hash and Python-defined signed
material. Its caller obtains the announced 64-byte RNS public key through a
narrow project-owned lookup seam; the crate derives that identity's
`lxmf.delivery` destination hash, compares it with the message source, and only
then constructs a `BoundSourceIdentity`. Signature verification therefore does
not expose mutable Rete node state, and an unbound public key is insufficient.
A Rete adapter can provide the lookup today; another RNS implementation or a
test oracle can provide the same input later.

Before streaming Ed25519 verification, the firmware also requires both the
identity signing key and signature `R` point to be non-identity members of the
prime-order subgroup. This rejects small-order and mixed-torsion points. It is
a deliberate fail-closed security profile across provider-dependent Python
behavior: RNS's bundled pure25519 verifier enforces the subgroup restriction,
while the reviewed host PyCA/OpenSSL provider accepted weak and mixed-torsion
cases. Ordinary strong-key Python LXMF signatures remain interoperable and
corpus-gated.

Structural parsing is not message acceptance. The caller receives a validated
message only after the source identity/hash relationship, signature, message
ID, and configured stamp/ticket policy have succeeded. Parse/shape failures,
identity-or-signature failures, and stamp-policy failures remain distinct
outcomes so callers cannot mistake unsupported canonicalization for a bad
identity or a rejected stamp.

### Treat released Python as the compatibility authority

The checked-in Python LXMF 1.0.1 corpus is the first-tranche wire authority.
Rust-generated round trips and another Rust implementation are useful tests,
but cannot override those bytes. Corpus provenance, generator source digests,
pinned Python/RNS/LXMF revisions, ingress normalization, and negative mutations
remain part of the gate.

This authority is deliberately versioned, not permanent. Later compatibility
lanes will add current Python LXMF and independent clients. A discovered
version difference becomes an explicit version/policy decision; it is not
silently normalized into a private firmware dialect.

### Join application events through a separate ingress adapter

The project owns `crates/lxmf-ingress`, registered as
`reticulum-lxmf-ingress`. It depends normally only on `node-core` and
`lxmf-wire`; the temporary raw-RNS inbox is a test-only dependency. The adapter
borrows an ADR 0012 `ApplicationEvent`, checks that destination DATA addresses
the one explicitly supplied local `lxmf.delivery` destination, obtains the
announced source public key by value through a caller-owned lookup, and applies
the caller's wire limits and stamp policy. A validated result borrows the exact
event payload and copies only bounded scalar correlation evidence.

Classification never consumes the event. Unrelated events, work or identity
state that requires deferral, conclusive validation rejection, and complete
validation are distinct outcomes. Missing source identity and incomplete
stamp-validation work remain deferred; they are not evidence of an invalid
message. The caller must retain, quarantine, durably commit, or explicitly
discard the event before acknowledging its owner lease.

This first adapter tranche admits only opportunistic destination DATA. Link
DATA with any context other than RNS `NONE` is unrelated and remains available
to its actual service. Context-`NONE` Link DATA and Resource-complete events do
not prove that their Link terminates at the local LXMF destination, so only
those possible LXMF carriers are explicitly deferred even though the wire crate
can normalize their bytes. This avoids treating carrier shape as application
ownership. Native Resource ingress stays disabled until its segmentation and
streaming-storage contract is bounded.

`node-core` exposes only a construction-time registration helper for an
additional inbound Single destination and a read-only, by-value identity
lookup. The permanent supervisor forwards only the identity lookup. The E290
firmware does not yet register or schedule the LXMF service, so these portable
seams are not powered LXMF evidence.

### Reuse LXMF-rs selectively and preserve attribution

LXMF-rs is an approved implementation reference and source for selective
extraction. We may reuse reviewed constants, algorithms, fixtures, and small
protocol modules where they match the Python authority, while leaving its
Tokio/SQLite/runtime graph behind. The initial `reticulum-lxmf-wire` source is
independently authored against the Python corpus and contains no copied or
modified LXMF-rs implementation source. If a later tranche copies or modifies
LXMF-rs source, that source remains under EPL-2.0 with its headers, notices,
modification history, and corresponding source preserved. Each such file or
derived crate is identified in the provenance record rather than being
relabelled as project-owned permissive code.

Independent code written from the public protocol behavior and Python corpus
remains under the repository's `MIT OR Apache-2.0` policy. A shared API does
not erase the source boundary. The product graph must continue to reject an
unreviewed mixture of EPL-only and AGPL-only implementation source; executable
AGPL peers may still serve as independent interoperability oracles.

### Separate the future model, engine, store, and Resource service

The first `reticulum-lxmf-wire` commit does not own durable delivery state.
Later components remain separate:

- `lxmf-model` will define versioned message, attempt, receipt, ticket, stamp,
  propagation, and client-visible state without embedding wire buffers;
- the LXMF engine will own delivery selection, retries, sibling-receipt
  cancellation, announce/propagation policy, and cooperative work budgets;
- durable stores will own message metadata, tickets, ratchets, and flash-backed
  blobs through stable handles and explicit commit/recovery contracts; and
- the RNS Resource service will own bounded offer admission, segmentation,
  streaming hash/crypto/decompression, and completed blob handles.

The engine consumes project-owned `ApplicationEvent` values from ADR 0012 and
submits transport-neutral node operations. It never sends through a LoRa
driver or local client bearer directly. USB, BLE, Wi-Fi, an embedded SPA, and
an onboard UI consume the same semantic service and cannot acquire ownership
of RNS or LXMF protocol state.

## Consequences

- The first crate can be qualified on host and generic bare-metal targets
  before RNS Resource or durable mailbox work is ready.
- Exact borrowed fields prevent accidental signature breakage and avoid a
  second attacker-sized MessagePack allocation, at the cost of tying views to
  their input owner.
- Semantic consumers must copy only selected bounded values or retain a stable
  blob/message owner; they cannot keep an event slot borrowed indefinitely.
- The ingress adapter cannot acknowledge or drop an application event. Durable
  semantic acceptance remains a separate owner transition to design and test.
- Full-product PSRAM can enlarge engine/store/blob quotas, but does not weaken
  wire limits or permit network-controlled allocation.
- Proof-of-work validation is allocation-free but still performs the protocol's
  fixed 3,000-round expansion synchronously. The future engine must schedule it
  as explicit bounded work outside the sole RNS/radio actor; this first tranche
  does not place it on live ingress.
- LoRa remains the first delivery path without becoming part of the LXMF type
  system. Additional Reticulum interfaces and local API bearers reuse the same
  validated-message boundary.
- The temporary ADR 0011 raw-RNS record remains qualification evidence, not the
  LXMF mailbox schema. Its 383-byte payload limit is not imported into the
  product ingress graph; a test proves that the Python-valid 391-byte
  opportunistic carrier remains admissible.

## Acceptance evidence

The first wire and application-ingress tranches must prove:

1. allocation-free `#![no_std]` compilation of the wire crate on a generic
   bare-metal target and strict host tests without an allocator, board,
   transport actor, or Rete node dependency;
2. byte-exact parsing, normalization, Python-rule hashing, signature/source
   validation, and message-ID results for every supported Python LXMF 1.0.1
   foundation fixture, including separate four-item raw and five-item
   canonical-first-four paths;
3. correct 32-byte proof-of-work stamps, 16-byte tickets, opportunistic/direct
   boundaries, and Link DATA/Resource-complete normalization;
4. borrowed preservation of accepted known and unknown MessagePack fields,
   including invalid UTF-8 binary values and nested structures, plus typed
   fail-closed rejection of the deliberately unsupported map-key and timestamp
   extension forms;
5. explicit, class-preserving rejection for all committed negative mutations
   plus hostile depth (including the hard maximum of 32), length, cardinality,
   scan-work, unsupported arity/noncanonical stamped encoding, truncation, and
   trailing-data cases; and
6. dependency-graph and provenance checks that preserve the project-owned,
   Rete-adapter, and any future EPL-derived source boundaries; and
7. `#![no_std]`, zero-copy application-event admission that performs no new
   allocation, with explicit destination ownership, by-value source lookup,
   distinct retry/rejection outcomes, retained event ownership, and an explicit
   regression against importing the raw-inbox size limit. Its reviewed normal
   closure may include the existing alloc-backed `node-core`/Rete event adapter,
   but no platform, board, radio, firmware, storage, device-API, supervisor, or
   executor package.

This evidence establishes the wire/validation foundation and its portable
opportunistic application-event ingress seam. It does not claim target-firmware
composition, durable send/receive, RNS Resource transfer, retries, propagation,
NomadNet, RF interoperability, or a complete LXMF service.
