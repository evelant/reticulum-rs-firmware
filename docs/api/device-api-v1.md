# Device API v1 logical protocol

Status: initial host-simulation schema plus a separate portable durable
submission model. This document freezes the operation and field numbers
exercised by `reticulum-device-api`; no adapter connects that codec to the
durable model, firmware transport, or radio transmission.

## Boundary

The crate is `no_std`, allocation-free, and Rete-independent. It owns logical
requests, responses, scalar capabilities, submission status, the indexed-CBOR
codec, and a small common authorization policy. It does not contain:

- USB, BLE, Wi-Fi, WebSocket, COBS, length framing, reconnect, or chunking;
- a node-core dispatcher, queue, storage, firmware, ESP, Embassy, or board code;
- an interface that returns raw Reticulum/RNode packet bytes;
- radio-TX authorization or any path to a radio driver.

Transport framing and authenticated session establishment are later layers.
Those layers decode a message and supply a trusted `DispatchContext` separately.
No principal, permission, or session assertion is accepted from CBOR input.

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
| `0xf001` | `experimental.prepare_rns_data` | host simulation only | authenticated + `EXPERIMENTAL_PREPARE_RNS_DATA` |

Numbers `0xf000..=0xffff` are experimental and can disappear or change without
API compatibility. `0xf001` is compiled only with the `host-sim` Cargo feature.
Enabling `host-sim` on `target_os = "none"` is a compile error. A build without
that feature returns `UnsupportedOperation(0xf001)`.

### `system.capabilities` (`0x0001`)

Request body: a map with no recognized fields. Unknown fields are permitted for
evolution.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | map | yes | highest API version, using the common version-map shape |
| 1 | bool | yes | `packet_output`; always `false` in this slice |
| 2 | u8 | yes | `radio_tx`: 0 unavailable, 1 disabled, 2 available; always 0 in this slice |
| 3 | bool | yes | experimental prepare operation compiled in |
| 4 | u16 | yes | maximum logical message bytes (512) |
| 5 | u16 | yes | maximum encoded body bytes (448) |
| 6 | u16 | yes | maximum experimental payload bytes (383) |

`CapabilitySnapshot::current()` is device-owned and cannot advertise packet
output or radio TX. The host experiment changes only key 3.

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
it does not expose volatile attempt correlation. Reboot safety still depends on
an unimplemented physical journal actor that integrity-validates every
record through a known end-of-log before completing semantic replay.

### `experimental.prepare_rns_data` (`0xf001`)

This host-only operation proves strict decode, authentication, idempotency input,
and a future device-API-to-node boundary. It is deliberately not named
`rns.send`, is not a stable product operation, and has no firmware integration.
The future client-facing send operation is `messages.send` through embedded
LXMF. A separately authorized raw RNS/RNode bridge, if ever implemented, remains
a distinct capability and mode.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | complete destination hash |
| 1 | bytes(0..383) | yes | borrowed application payload |
| 2 | bytes(16) | yes | principal-scoped idempotency key |

The decoder returns key 1 as a slice of the caller's input buffer. An adapter
must not retain it past that buffer's lifetime. `reticulum-storage-model`
defines the owned bounded intent, semantic content digest, principal/key
idempotency rule, and opaque acceptance plan that a future physical storage
actor must append before replying. Only after that acceptance and the durable
`Queued -> Preparing` barrier may the sole node owner prepare into one
separately registered, caller-owned 500-byte `TxPacketBuffer`. Node-core rejects
an already-expired owner
deadline before mutation, resolves the enabled-interface route, and returns a
unique routed `TxJob`; that prompt dispatch ownership is not client-intent
storage, and its RNS receipt timeout has already started. Packet bytes remain
inaccessible until an opaque permit exchange produces `AuthorizedTx`, whose
`frame(now)` accessor is one-shot and exact-deadline checked. A standalone
bounded async handoff carries these typestates, and the portable projector
models their persist-before-ack observations, but no device-API/storage/runtime
adapter invokes that path. There is no radio integration, and the host-only
operation remains disconnected from it.

Successful host-simulation response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | device-assigned submission ID |

In the future firmware adapter, acceptance will mean the exact intent is
durable and the backend has reserved the physical capacity required by its
lifecycle contract. The current host simulation does not make that durability
claim. Acceptance is not a delivery guarantee; a later status can report no
path, delivery timeout, downstream rejection, or an internal failure. The ID
can be queried through `submission.status`. The response contains no
destination, payload, prepared packet, packet fragment, or packet-borrowing
handle.

The storage model scopes idempotency by the authenticated principal. Repeating
the same key with identical semantic destination/payload content returns the
original submission ID. Reusing it for different content returns immediate
error 10 and must not mutate the original submission. The missing adapter must
derive the principal from `DispatchContext`; it cannot trust request bytes.

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

- `system.capabilities` is available before application authentication;
- `submission.status` requires an authenticated principal and its read bit;
- experimental preparation requires an authenticated principal and its
  experimental permission.

Authentication, ownership filtering, rate limiting, idempotency scope, physical
presence, and high-assurance session encryption are not solved by this codec.
The future device-API adapter and session runtime must scope status and
idempotency by the principal from `DispatchContext`, never by bytes supplied in
a request.

## Golden vectors

The canonical `system.capabilities` request for request ID 42 is:

```text
a4 00 a2 00 01 01 00 01 18 2a 02 01 03 a0
```

The wire tests freeze this request, both default and host-simulation capability
responses, typed permission/capacity/idempotency error responses, the
experimental preparation request, and its submission-ID-only accepted response.
They also cover every submission failure, state invariants, closed numeric
enums, unknown fields, unknown operations, missing and duplicate known fields,
every truncated golden prefix, trailing bytes,
message/body/payload/nesting limits, indefinite-value rejection, fixed
byte-string widths, borrowed payload storage, authorization, and the
packet-output/radio-TX safety values.

## Validation profiles

Run all three supported profiles explicitly from the workspace root:

```sh
cargo test --locked -p reticulum-device-api
cargo clippy --locked -p reticulum-device-api --all-targets -- -D warnings

cargo test --locked -p reticulum-device-api --features host-sim
cargo clippy --locked -p reticulum-device-api --all-targets --features host-sim -- -D warnings

cargo check --locked -p reticulum-device-api --no-default-features \
  --target riscv32imac-unknown-none-elf
```

The first pair validates the default host profile, the second pair validates the
host-only experiment, and the final command proves the default crate remains
`no_std` on an installed `target_os = "none"` target. `host-sim` is intentionally
rejected on that target.
