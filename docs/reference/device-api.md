# Device API

The local device API is an allocation-bounded, bearer-neutral protocol between
an appliance and a trusted client. The current unreleased version is **3.0**.
Rust definitions in `crates/device-api` and their wire tests are authoritative;
this document explains the stable boundary and current product surface.

## Layering

The logical protocol is a strict CBOR request/response exchange. It has no
executor, storage, board, radio, or Rete dependency. Separate crates provide:

1. record framing over a byte stream;
2. a bearer-bound authenticated session;
3. device-owned credential and authorization policy;
4. synchronous dispatch into product service ports; and
5. BLE or future physical-bearer ownership.

Pairing and initial credential activation use a separate pre-authentication
protocol. A pairing connection cannot be mistaken for an authenticated device
API session.

The current BLE application bearer provides authentication and integrity. It
does not add a second end-to-end confidentiality layer above Bluetooth and
Reticulum.

## BLE link lifecycle

The E290 accepts a central's valid connection interval, latency, and event
lengths, but raises supervision timeouts shorter than six seconds to six
seconds. It also proactively requests that floor immediately after connecting,
using the already-negotiated interval and latency unchanged, because a central
need not send a later parameter request. Firmware diagnostics report the
initial, requested, applied, and subsequently negotiated parameters. The
proactive HCI request is bounded to two seconds so it cannot indefinitely delay
GATT service; rejection or timeout retains the central's current parameters.

Trouble's per-connection event queue is intentionally eight entries, but event
delivery is not a lifecycle correctness requirement. The bearer also polls the
manager's connected state, releases an already-disconnected slot when the
terminal event was dropped, and bounds an incomplete controller disconnect to
five seconds. Because the pinned public Trouble API cannot reinitialize its
host and controller in place, the first irrecoverable drain performs a full
software reset. An RTC-retained, torn-write-safe marker suppresses an early
second reset and disables only BLE until a power cycle; the LoRa/node tasks
remain active.

## Version and encoding

Every message carries major and minor version numbers. A decoder accepts any
minor revision within its major generation and skips unknown numeric map fields,
but it rejects another major version, missing or duplicate known fields, unknown
closed-enum values, indefinite CBOR containers, excessive nesting, and trailing
bytes. New minor revisions must preserve all fields defined by 3.0.

Encoder output uses definite containers, ascending unsigned numeric keys, and
preferred integer representations. One logical message is exactly one CBOR
item; framing owns stream boundaries and recovery.

| Limit | Value |
| --- | ---: |
| Logical message | 512 encoded bytes |
| Operation body | 448 encoded bytes |
| Raw RNS DATA payload | 383 bytes |
| Recognized fields in one API map | 32 |
| Container/tag nesting | 8 levels |
| Basic LXMF title | 295 bytes |
| Basic LXMF content | 295 bytes |
| LXMF read chunk | 416 bytes |
| Nearby announce application data | 256 bytes |
| Nomad page path | 128 UTF-8 bytes |
| Nomad page response | 400 UTF-8 bytes |
| Saved Wi-Fi profiles | 4 |
| Diagnostic interface slots | 4 |
| Route entries per page | 4 |
| Radio-trace events per page | 2 |
| Wi-Fi SSID | 1–32 bytes |
| WPA2-Personal passphrase | 8–63 printable ASCII bytes |

Title and content limits are structural per-field limits. The complete request
and one-packet LXMF composer can impose a lower limit on a particular
combination.

## Envelope

Requests and responses use this map:

| Key | Type | Meaning |
| ---: | --- | --- |
| 0 | map | `{0: major u16, 1: minor u16}` |
| 1 | u64 | client-selected request ID echoed by the response |
| 2 | u16 | operation number or response kind |
| 3 | map or operation-specific fixed array | operation body |

The request ID correlates one exchange. It is not an idempotency key or an
authorization credential.

## Current operations

| Number | Operation | Purpose |
| ---: | --- | --- |
| `0x0001` | `system.capabilities` | version, bounds, and runtime capability discovery |
| `0x0002` | `submission.status` | durable outbound submission state |
| `0x0003` | `identity.summary` | public primary and optional LXMF destination hashes |
| `0xf001` | `rns_data.submit` | durably submit outbound RNS DATA without selecting an interface |
| `0xf004` | `lxmf.next` | page committed LXMF summaries |
| `0xf005` | `lxmf.read` | read normalized LXMF wire bytes |
| `0xf006` | `lxmf.basic_send` | compose and durably submit a basic LXMF message |
| `0xf007` | `lxmf.peer_next` | page observed `lxmf.delivery` peers |
| `0xf008` | `nomad.fetch_start` | begin one bounded Nomad page request |
| `0xf009` | `nomad.fetch_poll` | poll the principal-owned request |
| `0xf00a` | `network.config_get` | read redacted desired network state |
| `0xf00b` | `network.config_mutate` | compare-and-swap a network policy change |
| `0xf00c` | `network.status` | read live Wi-Fi, DNS, TCP, and RMAP publication state |
| `0xf00d` | `manual_service_announce` | queue one coalesced announce cycle |
| `0xf00e` | `node.diagnostics` | bounded interface, LoRa, and Reticulum snapshot |
| `0xf00f` | `route_diagnostics.page` | page retained route evidence |
| `0xf010` | `lxmf.mailbox_status` | read durable client-collection state |
| `0xf011` | `lxmf.mailbox_acknowledge` | advance the collection watermark |
| `0xf012` | `reticulum_probe.start` | begin one path-and-proof probe |
| `0xf013` | `reticulum_probe.poll` | poll its boot-scoped result |
| `0xf014` | `radio_trace.page` | import packet-correlated trace events |

The `0xf000..=0xffff` range is reserved for product extension operations.
Availability is reported by capabilities and can also depend on mounted
storage and runtime state. A known but unavailable operation returns a typed
error instead of falling back to volatile behavior.

## Messaging semantics

`lxmf.basic_send` accepts destination, timestamp, title, content,
delivery preference, idempotency key, and an optional typed phone location. The
device constructs and signs the LXMF message. Callers cannot inject arbitrary
MessagePack fields or signed wire bytes.

The optional location uses fixed-point values compatible with the recognized
Sideband LXMF telemetry location sensor. It becomes part of the signed message
and remains unchanged across every board-owned retry.

A successful send response means the exact intent is durable; it does not mean
the radio transmitted or the recipient delivered it. `submission.status`
reports the later state. Repeating an uncertain mutation with the same
idempotency key and identical content is safe; reusing that key for different
content is an error.

Mailbox acknowledgement is a monotonic appliance-wide collection watermark,
not human read/unread state. The app advances it only after importing the
contiguous inbox prefix into its own durable store.

## Diagnostics semantics

Route pages expose retained local routing evidence. A retained route is not a
connected peer or a delivery guarantee, and its last-use age is local route-
table activity rather than last-heard time.

LoRa receive signal is whole-packet final-hop evidence. For a split RNode
packet, the firmware retains the weaker RSSI and SNR across its frames. It does
not infer remote signal or end-to-end path quality.

The boot-scoped radio trace contains route selection, terminal DATA dispatch,
physical frame completion, logical receive, and delivery proof/timeout events.
It also correlates the receiver-side path from reconstructed DATA through
durable LXMF commit, retained/staged/queued proof ownership, and physical proof
TxDone or failure. A page carries at most two events so every maximum-sized
proof stage fits the fixed 512-byte response envelope. Sequence cursors include
a boot identifier so a reboot cannot silently skip events. Ring overwrite is
reported as incomplete history.

`network.status` distinguishes desired RMAP configuration from configuration
applied by the running firmware. It reports the current stamp phase, initial
TCP gate, queue admission outcome, next due time, and any typed deferral or
failure reason; coordinator acceptance is not presented as physical egress.

A Reticulum probe establishes only that a compatible destination returned a
proof through the normal routing path. It does not establish LXMF availability,
throughput, remote request RSSI, or hop history.

## Errors

Error responses use kind `0x0000` and contain an error code plus the related
operation when known.

| Code | Meaning |
| ---: | --- |
| 1 | unsupported operation |
| 2 | unsupported version |
| 3 | authentication required |
| 4 | permission denied |
| 5 | not found |
| 6 | invalid request |
| 7 | capability unavailable |
| 8 | internal failure |
| 9 | capacity exhausted |
| 10 | idempotency conflict |
| 11 | retry later |

`RetryLater` means another exact device-owned operation temporarily owns a
required resource. The authenticated session remains valid and the client
should retry after a short bounded delay. Framing, transport, and authentication
failures invalidate the session instead of becoming logical errors.

## Authorization

Every supported physical product bearer establishes an authenticated session
before logical dispatch, including public identity and capability reads. BLE
GATT is the current product bearer. The logical codec can represent public
operations independently so it stays transport neutral.

The session grants a principal, credential generation, authority revision, and
permission snapshot from device-owned state. Dispatch revalidates that grant
immediately before borrowing an operation-scoped service port. Rejection
performs no service I/O.

Network mutation requires its dedicated permission. Submission and probe start
require the message-submission permission. Read-only messaging, Nomad, network
status, diagnostics, trace, and mailbox operations require an authenticated
principal. This is still a single-appliance alpha policy rather than a
multi-user mailbox ACL.

## Generated clients

The Expo application uses Rust-generated DTOs in
`clients/appliance/src/generated/api.ts`. Change the Rust source first, then
run from `clients/appliance`:

```sh
bun run api:generate
bun run api:check
```

The device API version, operation vocabulary, bounds, and generated native
contract must advance together.
