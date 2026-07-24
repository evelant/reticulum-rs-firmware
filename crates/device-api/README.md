# Reticulum device API

`reticulum-device-api` is the allocation-free, bearer-neutral logical protocol
used by USB, BLE, and Wi-Fi sessions. It owns bounded CBOR models and common
authorization policy, but no framing, executor, storage, board, radio, or Rete
state.

API 1.5 adds bounded nearby-LXMF peer discovery. It retains the API 1.3
optional public `lxmf.delivery` destination on `identity.summary` and the API
1.4 source-free basic-send operation. Existing identity responses remain
valid: body key `0` is the required primary destination and key `1` is the
optional 16-byte LXMF delivery destination.

## Experimental LXMF operations

The operation numbers are reserved after the existing API-v1 experimental
range:

| Operation | ID | Request body | Successful response body |
| --- | ---: | --- | --- |
| `experimental.lxmf.next` | `0xf004` | optional key `0`: exclusive non-zero handle | committed-message summary |
| `experimental.lxmf.read` | `0xf005` | key `0`: handle, key `1`: offset, key `2`: maximum bytes | exact normalized-wire chunk |
| `experimental.lxmf.basic_send` | `0xf006` | destination, timestamp, title, content, and idempotency key | submission ID and LXMF message ID |
| `experimental.lxmf.peer_next` | `0xf007` | empty first page, or key `0`: incarnation and key `1`: exclusive generation | one-record nearby peer page |

Operations `0xf004`, `0xf005`, and `0xf007` are read-only and require an
authenticated principal. They do not consume a persisted permission bit.
`dispatch_with_lxmf` exposes committed reads and basic send independently of
the raw-RNS qualification mailbox. A product opts into peer discovery through
`dispatch_with_lxmf_and_peer_discovery`, or through
`dispatch_with_inbox_lxmf_and_peer_discovery` when it also retains the raw
inbox. Older dispatcher entry points deliberately leave peer discovery
unavailable.

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

## Experimental nearby LXMF peers

`experimental.lxmf.peer_next` exposes only signed, validated
`lxmf.delivery` announce observations already retained by the firmware. It is
display and contact-selection evidence, not route authority. The 64-byte
Reticulum identity public key is deliberately absent; the destination and
16-byte identity hash are sufficient for the client picker while the protocol
owner retains key material for path validation.

The first request body is an empty map. Every continuation request must include
both fields or decoding fails:

| Key | Value |
| ---: | --- |
| 0 | 8-byte public boot/incarnation token |
| 1 | exclusive observation generation; zero means before all observations |

The one-record response body uses:

| Key | Value |
| ---: | --- |
| 0 | current 8-byte incarnation token for the next cursor |
| 1 | exclusive generation for the next cursor |
| 2 | optional latest generation observed at this response snapshot |
| 3 | optional oldest generation represented by a retained peer |
| 4 | `history_gap`: requested history was reset, updated away, or evicted |
| 5 | optional peer record |

A peer record is a compact map:

| Key | Value |
| ---: | --- |
| 0 | 16-byte announced `lxmf.delivery` destination |
| 1 | 16-byte hash of the identity that authenticated the announce |
| 2 | authenticated announce application data, at most 256 bytes |
| 3 | latest observed Reticulum hop count |
| 4 | product-owned observing-interface ID |
| 5 | optional RSSI in whole dBm |
| 6 | optional SNR in whole dB |
| 7 | saturating age in milliseconds at the response snapshot |
| 8 | non-zero generation of this retained observation |

The incarnation explicitly scopes both cursor generations and observation age.
A cursor from an older boot, or ahead of current history, resets to the first
retained record and sets `history_gap` instead of failing. Capability key `14`
reports runtime availability and key `15` reports the port's app-data bound,
capped by the 256-byte logical ceiling. Older capability responses omit these
keys and decode as unavailable with a zero bound.

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
source from the authenticated device identity, constructs the complete
Python-compatible signed LXMF wire, validates the timestamp and
single-Link-packet direct limit, and durably accepts those exact bytes. Empty
title and empty content are a valid Python-compatible basic message and are
covered by the independent Python vector corpus. A successful response map
contains the durable submission ID at key `0` and the 32-byte
Python-compatible LXMF message ID at key `1`.

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
the 319-byte direct-content limit, and 431-byte durable wire capacity. Unknown fields are
skipped only when their definite-length CBOR remains within the protocol's
nesting, map-entry, message, and body limits. Capability keys `12` and `13`
therefore advertise independent
295-byte syntactic field bounds, not a promise that every such title/content
combination is accepted. The 448-byte encoded-body ceiling applies first.
Current E290 storage keeps generic RNS DATA at 383 plaintext bytes and uses a
distinct method-neutral intent for the exact complete signed LXMF wire through
431 bytes, so Python's 391-byte opportunistic carrier no longer inherits the
generic-RNS ceiling. The current automatic policy immediately sends an
eligible carrier through 391 bytes, or a 407-byte complete wire, using that
dedicated Header-1 opportunistic path. Complete wires of 408--431 bytes remain
durably pending until the direct-Link capability is implemented; a routed
Header-2 path can impose the smaller 383-byte carrier ceiling.
