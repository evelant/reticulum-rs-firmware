# Reticulum device API

`reticulum-device-api` is the allocation-free, bearer-neutral logical protocol
used by USB, BLE, and Wi-Fi sessions. It owns bounded CBOR models and common
authorization policy, but no framing, executor, storage, board, radio, or Rete
state.

API 1.4 includes the API 1.3 optional public `lxmf.delivery` destination on
`identity.summary` and the `experimental-lxmf` feature. Existing identity
responses remain valid: body key `0` is the required primary destination and
key `1` is the optional 16-byte LXMF delivery destination.

## Experimental LXMF operations

The operation numbers are reserved after the existing API-v1 experimental
range:

| Operation | ID | Request body | Successful response body |
| --- | ---: | --- | --- |
| `experimental.lxmf.next` | `0xf004` | optional key `0`: exclusive non-zero handle | committed-message summary |
| `experimental.lxmf.read` | `0xf005` | key `0`: handle, key `1`: offset, key `2`: maximum bytes | exact normalized-wire chunk |
| `experimental.lxmf.basic_send` | `0xf006` | destination, timestamp, title, content, and idempotency key | submission ID and LXMF message ID |

Operations `0xf004` and `0xf005` are read-only and require an authenticated
principal. They do not consume a persisted permission bit. `dispatch_with_lxmf`
exposes committed reads and basic send independently of the raw-RNS
qualification mailbox. Products that retain both stores use the single-owner
`dispatch_with_inbox_and_lxmf` entry point. Older dispatcher entry points
deliberately leave LXMF unavailable.

`next` walks physical commit order. Its summary map contains:

| Key | Value |
| ---: | --- |
| 0 | stable, non-zero committed-message handle |
| 1 | 32-byte Python-compatible LXMF message ID |
| 2 | 16-byte local delivery destination |
| 3 | 16-byte authenticated source destination |
| 4 | exact IEEE-754 timestamp bits |
| 5 | complete normalized-wire length |
| 6 | decoded title byte length |
| 7 | decoded content byte length |
| 8 | encoded MessagePack fields-map length |
| 9 | SHA-256 of the complete normalized wire bytes |

`read` returns map key `0` handle, key `1` offset, key `2` complete normalized
wire length, and key `3` exact bytes. A successful chunk is non-empty, lies
inside the declared message boundary, and contains at most 416 bytes. Clients
continue at `offset + bytes.len()` until `LxmfReadChunk::is_final()` is true.
The handle, complete length, and digest let clients detect replacement or
partial reads without coupling the protocol to the underlying store.

Capability body keys `9` and `10` report LXMF availability (`0` unavailable,
`1` disabled, `2` available) and the maximum chunk length. Older responses
that omit them decode as unavailable with a zero limit.

## Experimental basic LXMF send

`experimental.lxmf.basic_send` is a distinct transport-neutral mutation. Its
request body contains exactly these known fields:

| Key | Value |
| ---: | --- |
| 0 | 16-byte remote `lxmf.delivery` destination hash |
| 1 | Unix timestamp in whole milliseconds, exactly `1..=8_796_093_022_207_999` in the current product |
| 2 | borrowed binary title, structurally at most 295 bytes |
| 3 | borrowed binary content, structurally at most 295 bytes |
| 4 | 16-byte principal-scoped idempotency key |

The request never accepts a source hash. The product composer derives the
source from the authenticated device identity, selects the final
Python-compatible LXMF carrier, validates the timestamp, opportunistic-size,
and durable-carrier limits, and durably accepts that exact message. Empty title
and empty content are a valid Python-compatible basic message and are covered
by the independent Python vector corpus. A successful response map contains
the durable submission ID at key `0` and the 32-byte Python-compatible LXMF
message ID at key `1`.

Basic send reuses `Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA`; authentication
alone is not sufficient. Capability key `11` reports its independent runtime
availability, while keys `12` and `13` report the title and content limits.
Older capability responses omit these keys and decode as unavailable with
zero limits. Combined dispatchers use
`CapabilitySnapshot::for_dispatch_with_inbox_lxmf_and_basic_send`; older
dispatcher constructors intentionally leave send unavailable.

All messages remain limited to 512 bytes and operation bodies to 448 encoded
bytes. The codec validates required and unique fields, fixed widths, structural
per-field bounds, and definite-length CBOR nesting; it intentionally accepts
the timestamp as any `u64`. The product composer separately enforces
`1..=8_796_093_022_207_999`, Python-compatible packed-LXMF rules,
opportunistic selection, and durable carrier capacity. Unknown fields are
skipped only when their definite-length CBOR remains within the protocol's
nesting, map-entry, message, and body limits. Capability keys `12` and `13`
therefore advertise independent
295-byte syntactic field bounds, not a promise that every such title/content
combination is accepted. The 448-byte encoded-body ceiling applies first, and
the current E290 generic durable intent accepts carriers only through 383 bytes
although Python's dedicated opportunistic carrier can reach 391 bytes.
