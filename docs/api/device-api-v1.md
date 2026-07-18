# Device API v1 logical protocol

Status: logical codec and portable authenticated dispatch implemented over a
narrow durable-submission port. This document freezes the operation and field
numbers exercised by `reticulum-device-api`; `reticulum-device-api-adapter`
implements capabilities and principal-scoped status in its default build plus
target-safe durable experimental outbound RNS DATA submission behind an explicit
feature. Separate portable framing, immutable credential-authority, USB-
qualification session and boot-lifetime authenticated-job handoff crates now
exist. The session core emits only a credential ID/generation grant; the
portable authority revalidates it and derives `DispatchContext` through a
borrowing `DispatchLease`. The context carries validated non-wire provenance,
and semantic journal schema 2 persists its exact credential/policy snapshot on
acceptance. The portable two-sector raw-NOR credential store is implemented,
and E290 boot now validates its exact eFuse-derived binding, mounts it
immediately after flash open, performs bounded deterministic recovery, and
retains any mounted owner in the sole coordinator. It does not auto-provision
erased media. A separate portable pairing-policy crate implements the exact
physical-presence window, connection epoch, shared attempt, and operation-
ownership state. It is now feature-free only in the permanent E290 graph, where
a resident `CredentialRuntime` privately retains the policy, exact boot binding,
mounted authority, and admitted initialization permit. The coordinator compiles
a sole-owner port that freshly reinspects node identity and creates a short-lived
bound credential view; its runtime accepts only forward erased/interrupted
trajectories. No GPIO debounce, external request lane, physical bearer, or
powered initialization invokes that path, and boot never initializes
automatically. Lifecycle-specific Add/Activate/Abort planners, opaque typed
store commit/reconcile owners, mounted-store pending selection, and the read-only
interrupted-initialization classifier are implemented. E290 boot now maps only
its canonical recoverable trajectory to an explicit disabled state. Live Begin,
Proof, Activate, and Abort mutation, the external API/session firmware lane, and
every physical bearer remain unimplemented. A product port may route an accepted
submission through the node after the durable barriers.

## Boundary

The crate is `no_std`, allocation-free, and Rete-independent. It owns logical
requests, responses, scalar capabilities, submission status, the indexed-CBOR
codec, and a small common authorization policy. It does not contain:

- USB, BLE, Wi-Fi, WebSocket, COBS, length framing, reconnect, or chunking;
- a node-core dispatcher, queue, storage, firmware, ESP, Embassy, or board code;
- an interface that returns raw Reticulum/RNode packet bytes;
- raw/direct-radio-TX authorization or access to a radio driver.

Transport framing, pairing admission, job handoff and authenticated session
establishment are separate layers. They decode and carry the message plus a
session-minted credential reference. `reticulum-device-api-credentials` implements the fixed-
capacity device-owned semantic authority: it must revalidate that reference and
separately derive the trusted `DispatchContext` and validated
`DispatchProvenance` immediately before dispatch. No principal, permission,
provenance, or session assertion is accepted from CBOR input.

`reticulum-device-api-adapter` is the separate allocation-free `no_std`
dispatcher over a narrow `SubmissionPort`. The port exposes only runtime
availability, principal-scoped status, and durable acceptance; its implementation
retains every storage actor, operation-scoped journal view, and physical backend.
The adapter repeats major-version validation,
applies the codec's authorization policy to trusted context, always emits the
current response version, echoes the request ID, and performs no direct flash,
framing, session, radio or node work. The default feature set handles public
capabilities and authenticated, principal-scoped status. Missing and cross-
principal IDs both return `NotFound`, so the adapter does not disclose another
principal's durable records. A port reports unavailable service as
`CapabilityUnavailable`; status fails closed with `Internal` while the durable
owner has an ambiguous pending mutation or a latched fault and never publishes
the deliberately lagging live index as if it were current. Public capabilities
remain available in either condition.

## Version and evolution rules

The initial version is `1.0`. A decoder accepts major version 1 with any minor
version, skips unknown numeric map fields, and rejects another major version.
Encoding an envelope with another major version fails with the typed
`EncodeError::UnsupportedVersion` before any message is emitted. All encoder
output uses definite maps, ascending numeric keys, and CBOR's preferred shortest
integer encodings. Decoders accept equivalent integer encodings but reject every
indefinite-length byte string, text string, array, or map, including one nested
inside an unknown field.

Every envelope and operation body is an unsigned-integer-keyed CBOR map. Known
fields may appear in any order. Every known field, including an optional field,
may appear at most once. Missing required fields and duplicate known fields are
errors. Unknown unsigned field numbers are skipped without allocation. An
unknown request operation or response kind is a typed error, not an alias for
another operation.

All known numeric enum vocabularies in stable API major 1 are closed: capability
availability, submission state, submission failure, and API error code. Their
documented discriminants are frozen, unknown discriminants are rejected, and a
new discriminant requires a new API major version. Minor-version evolution uses
new optional numeric map fields instead of extending these enums. Experimental
operations remain exempt from stable compatibility as described below.

The decoder consumes exactly one logical CBOR item. Trailing bytes are rejected;
stream recovery and message boundaries belong to the future framing crate.
The allocation-free strict skipper bounds container/tag nesting within an
operation body or one unknown field value to eight levels.

## Hard limits

| Limit | v1 value | Enforcement |
| --- | ---: | --- |
| logical message | 512 encoded bytes | before CBOR decode and by bounded encoding |
| operation body | 448 encoded bytes | before operation-specific decode |
| fields per recognized API map | 32 | immediately after each API map header |
| body/unknown-value container or tag nesting | 8 levels | strict allocation-free skip/validation |
| experimental RNS DATA payload | 383 bytes | encode and decode |
| destination hash | 16 bytes | decode |
| idempotency key | 16 bytes | decode |
| encoded-packet SHA-256 | 32 bytes | decode |

The 383-byte payload limit matches the current Rete encrypted-DATA preparation
boundary. It is not a promise that raw RNS submission will become a product API.

## Common envelope

Requests and responses share this shape:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | map | yes | version `{0: major u16, 1: minor u16}` |
| 1 | u64 | yes | client-selected request ID, echoed by the response |
| 2 | u16 | yes | request operation or response kind |
| 3 | map | yes | operation-specific body |

The request ID correlates messages only. It is neither an idempotency key nor an
authorization credential.

## Operations

| Number | Name | Stability | Authorization |
| ---: | --- | --- | --- |
| `0x0001` | `system.capabilities` | v1 | public/read-only |
| `0x0002` | `submission.status` | v1 | authenticated + `READ_SUBMISSION_STATUS` |
| `0xf001` | `experimental.submit_rns_data` | feature-gated experimental | authenticated + `EXPERIMENTAL_SUBMIT_RNS_DATA` |

Numbers `0xf000..=0xffff` are experimental and can disappear or change without
API compatibility. `0xf001` is compiled only with the target-safe
`experimental-rns-data` Cargo feature. A build without that feature returns
`UnsupportedOperation(0xf001)`. The adapter mirrors this
feature boundary. Its capability response restricts the codec snapshot to the
adapter's own compiled operation and the port's runtime availability, so Cargo
feature unification on a separate
`reticulum-device-api/experimental-rns-data` dependency edge cannot make an
adapter default build advertise its missing dispatch arm.

### `system.capabilities` (`0x0001`)

Request body: a map with no recognized fields. Unknown fields are permitted for
evolution.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | map | yes | highest API version, using the common version-map shape |
| 1 | bool | yes | `packet_output`; always `false` in this slice |
| 2 | u8 | yes | `direct_radio_tx`: raw/direct-radio access; 0 unavailable, 1 disabled, 2 available; always 0 in this slice |
| 3 | bool | yes | transport-neutral experimental outbound RNS DATA submission advertised by this dispatcher |
| 4 | u16 | yes | maximum logical message bytes (512) |
| 5 | u16 | yes | maximum encoded body bytes (448) |
| 6 | u16 | yes | maximum experimental payload bytes (383) |

`CapabilitySnapshot::current()` is device-owned and cannot advertise packet
output or direct-radio TX. Key 2 says nothing about node-owned RNS traffic: an
accepted request advertised by key 3 may be routed over LoRa or any other
eligible Reticulum interface without granting a client direct radio control. A
higher dispatcher uses `CapabilitySnapshot::for_dispatch` to restrict that
codec-build snapshot; it can disable a capability but cannot enable one omitted
from the codec build.

### `submission.status` (`0x0002`)

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | device-assigned submission ID |

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | submission ID |
| 1 | u8 | yes | state |
| 2 | u16 | no | prepared packet length |
| 3 | bytes(32) | no | SHA-256 of the complete encoded packet bytes |
| 4 | u8 | no | terminal failure category |

Submission states are 0 queued, 1 preparing, 2 awaiting delivery, 3 delivered,
4 failed, and 5 cancelled. Failure categories are 0 no path, 1 delivery
timeout, 2 downstream rejection, and 3 internal. Keys 2 and 3 are both required
for awaiting-delivery and delivered states and are forbidden for every other
state. Key 4 is required only for failed and forbidden for every other state.
The Rust model mirrors this with state-specific enum variants, so contradictory
combinations cannot be constructed. Awaiting delivery means the proof or
application acknowledgement is still pending; it does not claim that encoded
packet bytes still occupy a bound external dispatch buffer.

Length and encoded-packet SHA-256 allow diagnostics and correlation without
exposing the prepared packet. `EncodedPacketSha256` is a distinct Rust type so
the dispatcher cannot accidentally substitute the RNS proof-correlation hash.
No API in this crate can take, drain, or borrow packet bytes.

This encoded-byte digest is deliberately distinct from Reticulum's delivery-
proof receipt hash, which covers the protocol-defined hashable part. Node-core
supplies that RNS attempt token and also hashes every encoded packet byte at
preparation. The RF-inert dispatcher independently rehashes the complete frame
while its sole packet-byte view is the one-shot frame borrowed from an exactly
permitted node-core `AuthorizedTx`. The projector requires those lengths and
digests to agree, retains a planned semantic metadata record without packet
bytes, and applies it only after the storage actor reports commit or exact
readback.

The adapter reads only the small principal-owned lifecycle state from the live
index, not a copied complete intent. Missing and foreign identifiers use the
same combined ownership lookup. If an exact physical mutation is pending, the
live index intentionally lags flash; if the actor is faulted, its authority is
unavailable. In both cases `submission.status` returns `Internal` until the
owner reconciles the pending mutation or is remounted or recovered.

Node-core retains an in-RAM terminal-attempt tombstone until explicit
acknowledgement. The portable projector maps it to the corresponding final
submission state and exposes the exact acknowledgement only after a storage
actor reports that record committed or readback-equivalent. Node-core rejects
acknowledgement while an external TX typestate still binds its
`TxPacketBuffer`, so the action remains retryable until ownership returns. The
proof or receipt timeout may become terminal before a dispatcher frame
observation; delivery uses the preparation-bound digest and length, while a
timeout remains a metadata-free API failure. The encoded-byte digest remains
retained submission metadata, but the v1 response
exposes it only for `AwaitingDelivery` and `Delivered`; a delivery timeout is a
`Failed` response without keys 2 or 3. Device API v1 does not expose the
internal attempt handle, packet-slot ID, dispatch generation or deadline, and
it does not expose volatile attempt correlation. `reticulum-storage-journal`
supplies complete integrity-validated physical replay, and
`reticulum-storage-actor` can expose a live index only after full mount and
semantic replay. Product reboot safety still requires the firmware task to
finish that mount plus conservative boot recovery before enabling any API
transport or RF/node service.

### `experimental.submit_rns_data` (`0xf001`)

This target-safe experimental operation proves strict decode, authenticated
durable acceptance, idempotency input, and the API-to-storage boundary. It is a
transport-neutral submission: the client neither selects LoRa nor receives a
radio capability. The resident E290 composition can carry accepted work through
the node router and the LoRa-first interface after the durable barriers, although
no firmware bearer exposes this API yet. The future client-facing message
operation remains `messages.send` through embedded LXMF. A separately authorized
raw RNS/RNode or direct-radio bridge, if ever implemented, remains a distinct
capability and mode.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | complete destination hash |
| 1 | bytes(0..383) | yes | borrowed application payload |
| 2 | bytes(16) | yes | principal-scoped idempotency key |

The decoder returns key 1 as a slice of the caller's input buffer. An adapter
must copy it into the model's owned bounded intent before the input buffer is
released. `reticulum-storage-model` defines that intent, semantic content
digest, principal/key idempotency rule, and opaque acceptance plan;
`reticulum-storage-actor` appends it through the implemented physical journal
before a successful adapter may reply. Only after that acceptance and the durable
`Queued -> Preparing` barrier may the sole node owner prepare into one
separately registered, caller-owned 500-byte `TxPacketBuffer`. Node-core rejects
an already-expired owner deadline before mutation, resolves the enabled-
interface route, and returns a
unique routed `TxJob`; that prompt dispatch ownership is not client-intent
storage, and its RNS receipt timeout has already started. Packet bytes remain
inaccessible until an opaque permit exchange produces `AuthorizedTx`, whose
`frame(now)` accessor is one-shot and exact-deadline checked. A standalone
bounded async handoff carries these typestates, and the portable projector
models their persist-before-ack observations. The E290 host composition test
exercises that API-to-runtime-to-router-to-LoRa software path; portable framing,
immutable credential authority, qualification-session establishment, and job
handoff and raw-NOR credential storage are implemented separately, while
the credential store is now boot-composed. External live admission still
requires invoking the resident initialization path when media is empty, live
credential pairing lifecycle composition, an external API/session firmware
lane, and a bearer.

Successful experimental response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | device-assigned submission ID |

For any adapter port backed by the sole storage actor, acceptance means the exact
intent is durable and the backend has reserved the physical capacity required
by its lifecycle contract. The adapter's `experimental-rns-data` path enforces
that ordering: it copies the borrowed payload into the owned intent, invokes the
port's durable `accept`,
and publishes `Accepted` or exact `Replay` only after durable success. A lost
backend reply maps to `Internal` while the actor retains the exact mutation;
after `drive_pending()` reconciles it, retry returns the same durable ID.
Idempotency conflict and capacity map to their stable API categories, while
identifier exhaustion, actor busy, backend ambiguity and a latched fault map to
`Internal`. No firmware transport currently exposes this operation. Acceptance
is not a delivery guarantee; a later status can report no
path, delivery timeout, downstream rejection, or an internal failure. The ID
can be queried through `submission.status`. The response contains no
destination, payload, prepared packet, packet fragment, or packet-borrowing
handle.

The storage model scopes idempotency by the authenticated principal. Repeating
the same key with identical semantic destination/payload content returns the
original submission ID. Reusing it for different content returns immediate
error 10 and must not mutate the original submission. The portable adapter
derives the principal from `DispatchContext`; it never trusts request bytes.

## Responses and errors

A successful response uses the corresponding operation number at envelope key
2. An error uses response kind `0x0000` and this body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u16 | yes | error code |
| 1 | u16 | no | related request operation |

Error codes are 1 unsupported operation, 2 unsupported version, 3 authentication
required, 4 permission denied, 5 not found, 6 invalid request, 7 capability
unavailable, 8 internal, 9 capacity exhausted, and 10 idempotency conflict.
Capacity exhaustion is an immediate rejection: no submission ID is allocated,
and retrying later may succeed. Idempotency conflict means the principal reused
a key for different request content; repeating the original content remains
safe. Neither immediate rejection is represented as a terminal state of an
accepted submission.

Codec failures (`DecodeError`) happen before dispatch and therefore do not have
a trusted request ID to echo in every case. A transport/session adapter may
return a logical error only after it has safely recovered the necessary
envelope context; otherwise it closes or resynchronizes according to its own
framing rules.

## Authorization contract

`authorize_request(context, request)` applies the common baseline:

- `system.capabilities` requires no logical operation permission;
- `submission.status` requires an authenticated principal and its read bit;
- experimental outbound RNS DATA submission requires an authenticated principal
  and `EXPERIMENTAL_SUBMIT_RNS_DATA`.

The logical codec can represent an unauthenticated context for internal callers
and policy tests. ADR 0006's physical device-API bearers are stricter: every
wire operation crosses an authenticated application session. A stale, missing,
pending or revoked credential is never converted to
`DispatchContext::UNAUTHENTICATED`, even for `system.capabilities`.

Authentication, ownership filtering, rate limiting, idempotency scope, physical
presence, and high-assurance session encryption are not solved by this codec.
The portable adapter scopes status and experimental-operation idempotency by the
principal from `DispatchContext`, never by bytes supplied in a request. The
portable session core deliberately emits only a credential ID/generation grant.
`AuthenticatedGrant::revalidate` checks that reference against the immutable
device-owned authority and returns a `DispatchLease` whose borrow remains alive
through immediate synchronous dispatch. Its higher-ranked callback supplies a
borrow of the non-copyable `DispatchContext`; the exact context value cannot be
moved out, but trusted linked code can reconstruct equivalent scalar facts with
the public constructor. Immediate dispatch, no unauthenticated fallback and no
port call after rejection remain composition rules, not an unforgeable Rust
capability. Principal and permissions come from the exact active record. Live authority
replacement must also pass exact-next-revision successor validation so changed
authorization cannot reuse a session generation. E290 firmware now mounts and
recovers the portable store before any other product-store write and retains
its `Ready`, authentication-only, uninitialized-erased,
initialization-interrupted, blocked, corrupt, or backend-failed state. The
resident initialization runtime and sole-owner physical drive are compiled but
have no bearer/request caller. A future external serving runtime must invoke
that path explicitly when needed, add live Begin/Proof/Activate/Abort pairing,
enforce connection-level rate limits, and keep authentication state outside
request CBOR.

Semantic journal schema 2 persists the principal, idempotency key,
operation-specific intent, credential ID/generation, complete authority
revision, authorization-policy version, and exact granted permission mask.
The adapter constructs that storage-owned snapshot only after authorization
succeeds; a rejected request invokes no port. A retry after credential rotation
returns the original ID and retains the original evidence. See
[ADR 0008](../adr/0008-durable-authorization-provenance.md).

## Golden vectors

The canonical `system.capabilities` request for request ID 42 is:

```text
a4 00 a2 00 01 01 00 01 18 2a 02 01 03 a0
```

The wire tests freeze this request, both default and experimental capability
responses, typed permission/capacity/idempotency error responses, the
experimental submission request, and its submission-ID-only accepted response.
They also cover every submission failure, state invariants, closed numeric
enums, unknown fields, unknown operations, missing and duplicate known fields,
every truncated golden prefix, trailing bytes,
message/body/payload/nesting limits, indefinite-value rejection, fixed
byte-string widths, borrowed payload storage, authorization, and the
packet-output/direct-radio-TX safety values and the separate outbound-RNS
submission advertisement.

## Validation profiles

Run all three supported profiles explicitly from the workspace root:

```sh
cargo test --locked -p reticulum-device-api
cargo clippy --locked -p reticulum-device-api --all-targets -- -D warnings

cargo test --locked -p reticulum-device-api --features experimental-rns-data
cargo clippy --locked -p reticulum-device-api --all-targets \
  --features experimental-rns-data -- -D warnings

cargo check --locked -p reticulum-device-api --no-default-features \
  --target riscv32imac-unknown-none-elf
cargo check --locked -p reticulum-device-api --no-default-features \
  --features experimental-rns-data --target riscv32imac-unknown-none-elf
```

The first pair validates the default host profile, the second pair validates the
experimental operation, and the final two commands prove both feature profiles
remain `no_std` on an installed `target_os = "none"` target.

Validate authenticated dispatch independently in both host profiles and the
default ESP32-S3 graph:

```sh
cargo test --locked -p reticulum-device-api-adapter
cargo clippy --locked -p reticulum-device-api-adapter --all-targets -- -D warnings

cargo test --locked -p reticulum-device-api-adapter --features experimental-rns-data
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features experimental-rns-data -- -D warnings

cargo test --locked -p reticulum-device-api-adapter \
  --features reticulum-device-api/experimental-rns-data
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features reticulum-device-api/experimental-rns-data -- -D warnings

cargo +esp check --locked --release -p reticulum-device-api-adapter \
  --features experimental-rns-data --target xtensa-esp32s3-none-elf
cargo +esp clippy --locked --release -p reticulum-device-api-adapter \
  --features experimental-rns-data --target xtensa-esp32s3-none-elf -- -D warnings
```

Validate the separately bounded bearer-edge contracts without composing a
physical transport:

```sh
cargo test --locked \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session
cargo clippy --locked --all-targets \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session -- -D warnings
cargo check --locked \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  --target xtensa-esp32s3-none-elf
python3 interop/python/generate_device_api_session_vectors.py --check
PYTHONPATH=interop/python python3 -m unittest -v \
  interop/python/test_device_api_session_vectors.py
```

These checks pass. The dependency-only experimental profile proves feature
unification cannot advertise an adapter-local operation that is absent. The
adapter's focused tests cover exact authorization and zero-port-call rejection,
request context, version/capability behavior, principal isolation, every durable
lifecycle mapping, maximum-size owned-payload acceptance/replay/conflict,
acceptance across remount, stable capacity and identifier-exhaustion errors,
faulted and pending status gating, wrong-binding rejection without I/O, and
lost-write reconciliation. The session tests and independent Python vectors
cover canonical hello/proof derivation, direction-separated record tags,
downgrade/reflection/replay/generation/reset failures, exact sequence policy and
partial-write typestate. Target checks exercise the portable layers directly on
`no_std` bare-metal builds.
