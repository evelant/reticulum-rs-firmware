# Device API v1 logical protocol

Status: API 1.6 logical codec and portable authenticated dispatch implemented
over one operation-scoped owner exposing narrow durable-submission, raw-inbox,
committed-LXMF-read, source-free basic-LXMF-compose, and bounded nearby-peer
ports plus an independent bounded NomadNet-fetch port.
This document freezes the operation and field numbers exercised by
`reticulum-device-api`; `reticulum-device-api-adapter` implements capabilities,
the public primary-destination summary, and principal-scoped submission status
in its default build. It adds target-safe durable experimental outbound RNS DATA
submission behind `experimental-rns-data`, experimental raw-RNS inbox
status/peek behind `experimental-rns-inbox`, and committed-LXMF next/read plus
source-free basic send behind `experimental-lxmf`. API 1.6 adds authenticated
bounded NomadNet fetch start/poll behind `experimental-nomad`. Separate portable
framing, immutable credential-authority, USB-
qualification session and boot-lifetime authenticated-job handoff crates now
exist. The session core emits only a credential ID/generation grant; the
portable authority revalidates it and derives `DispatchContext` through a
borrowing `DispatchLease`. The context carries validated non-wire provenance,
and semantic journal schema 3 persists its exact credential/policy snapshot on
acceptance. The portable two-sector raw-NOR credential store is implemented,
and E290 boot now validates its exact eFuse-derived binding, mounts it
immediately after flash open, performs bounded deterministic recovery, and
retains any mounted owner in the sole coordinator. The permanent authenticated
dispatch path also constructs the independent API 1.6 Nomad port around one
boot-scoped product fetch slot. It does not auto-provision
erased media. A separate portable pairing-policy crate implements the exact
physical-presence window, connection epoch, shared attempt, and operation-
ownership state. It is now feature-free only in the permanent E290 graph, where
a resident `CredentialRuntime` privately retains the policy, exact boot binding,
mounted authority, and admitted initialization permit. The coordinator compiles
a sole-owner port that freshly reinspects node identity and creates a short-lived
bound credential view; its runtime accepts only forward erased/interrupted
trajectories. A separate featureless pre-authentication codec freezes zero-
session, zero-tag status and explicit-initialization records while exposing only
coarse public results; it depends solely on framing and performs no policy,
ordering, replay, or mutation work itself. The permanent E290 three-task image
now composes that codec behind a sole USB Serial/JTAG byte owner, debounced
active-low GPIO21, boot-lifetime connection epochs, exact-next sequencing, and
a depth-one scalar command/reply handoff to the node-owned coordinator. Boot
still never initializes automatically. Lifecycle-specific Add/Activate/Abort
planners, opaque typed store commit/reconcile owners, mounted-store pending
selection, and the read-only interrupted-initialization classifier are
implemented. E290 boot maps only its canonical recoverable trajectory to an
explicit disabled state. This bootstrap is not an authenticated session and
does not dispatch the logical API documented here. Its status and physical-
presence-required path has run on both boards; one sender subsequently completed
button-confirmed initialization, pairing, and exact post-write readback. ADR
0010's separate
allocation-free live-pairing core now freezes Begin, ProofStart, Activate and
AbortCurrent records, typed continuation/reference binding, HMAC-SHA256 proof
and activation confirmation, secret-owner zeroization, and independent Python
vectors. The E290 resident owner now implements bounded entropy, exact proof
continuation, durable Add/Activate/Abort mutation with reconciliation and
cleanup ordering, plus a bearer-neutral secret-owning handoff. The node schedules
that lifecycle through its journal-aware causal frontier, and the sole USB owner
routes all four records through the shared decoder and sequence gate. The
permanent graph now also instantiates the feature-free session and handoff
crates, a static depth-one authenticated request/reply handoff, and a separate
node-side dispatch lane. That lane revalidates every opaque grant against the
currently publishable authority and synchronously invokes the logical adapter
through one operation-scoped semantic port owner isolated from credential
ownership. The USB
task now composes the deliberately minimal first bearer: one active session,
one request in flight, idle ClientHello replacement into a fresh session epoch
on the same connection, and terminal fault handling until USB reset or
re-enumeration. Replacement never displaces request/reply owners. It
intentionally defers resumption, protocol retries, close records, encryption,
rate limiting/attempt policy, and concurrency. Admission and node dispatch are
transport-neutral. The separately bound Wi-Fi and BLE suites now reach the same
lane in mutually exclusive proof profiles; Wi-Fi remains without a powered
authenticated exchange, while the bounded BLE GATT carrier is powered-qualified
through direct CoreBluetooth on both E290s.
Powered E290 qualification now covers authenticated capabilities and identity
reads plus one durable experimental submission that crossed LoRa, was decrypted
by a second permanent node, returned a valid Reticulum proof for the exact
packet, and remained `Delivered` after sender USB re-enumeration. A product port
may route an accepted submission through the node only after the durable
barriers. That API 1.1 result proves peer receipt/decryption and Reticulum proof
handling; it is not evidence that the receiving device persisted application
data. API 1.2 additionally has bounded powered evidence for one maximum-size
commit, authenticated status/peek before and after hard reset, exact raw
readback, and drop-newest preservation. Bounded powered software-fault
isolation now covers mount rejection and one same-boot commit failure as
described below. API 1.3 adds committed LXMF enumeration/readback and the
optional public `lxmf.delivery` destination; API 1.4 adds source-free basic
LXMF composition and durable submission; API 1.5 adds bounded authenticated
nearby-LXMF peer discovery. API 1.6 adds the portable NomadNet logical codec and
authenticated adapter boundary. The permanent E290 API now composes that
boundary with its transport-neutral Nomad client runtime through the existing
authenticated bearer path. The one-slot product owner and composition have
source and host-test evidence but no powered qualification. The Expo universal
client now exposes manual and nearby-peer-associated destination selection,
start/poll progress, explicit retained-ID recovery, and selectable raw Micron
text through that same authenticated session. Independent
`nomadnetwork.node` directory discovery and Micron rendering remain deferred.
The
[2026-07-22 API 1.4 POC](../e290-api14-lxmf-poc.md) powered-qualified same-boot
bidirectional send, Reticulum delivery proof, peer commit, enumeration, and
digest-verified readback on the E290 pair. Its final audited image also retained
both terminal sender records and both exact receiver wires across a physical
CPU reset. Controlled electrical power cuts and broader carrier/client behavior
remain open.

## Boundary

The crate is `no_std`, allocation-free, and Rete-independent. It owns logical
requests, responses, scalar capabilities, submission, inbox, and bounded
NomadNet response types, the indexed-CBOR codec, and a small common
authorization policy. It does not contain:

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
dispatcher over narrow semantic ports. `SubmissionPort` exposes only runtime
availability, principal-scoped status, and durable acceptance;
`InboundMailboxPort` exposes bounded raw-inbox reads; `LxmfInboxPort` exposes
commit-order metadata and bounded exact-wire chunks; and `LxmfComposePort`
atomically composes with the device-owned source and durably accepts the exact
carrier. The independent `NomadFetchPort` accepts and polls principal-scoped
boot-lifetime fetches without exposing Link, request, router, radio, or
firmware-owner types. None of these ports exposes raw physical storage, a
radio, or private identity material. The E290 combines the durable and LXMF
traits on one operation-scoped value so its sole flash owner is never mutably
aliased. It separately constructs an operation-scoped NomadNet port borrowing
only the boot-lifetime Nomad API metadata and transport-neutral client runtime,
so the flash-capable owner is neither aliased nor transferred into a fetch.
The adapter repeats major-version validation,
applies the codec's authorization policy to trusted context, always emits the
current response version, echoes the request ID, and performs no direct flash,
framing, session, radio or node work. The default feature set handles public
capabilities and authenticated, principal-scoped status. Missing and cross-
principal IDs both return `NotFound`, so the adapter does not disclose another
principal's durable records. A port reports unavailable service as
`CapabilityUnavailable`; status fails closed with `Internal` while the durable
owner has an ambiguous pending mutation or a latched fault and never publishes
the deliberately lagging live index as if it were current. Inbox capability is
advertised only while its exact durable store is mounted and enabled; there is
no volatile fallback. Public capabilities remain available in either condition.

## Version and evolution rules

The initial version was `1.0`; version `1.1` added `identity.summary`; version
`1.2` added optional raw-RNS inbox capability fields and feature-gated
status/peek; version `1.3` added optional `lxmf.delivery` identity metadata and
bounded committed-LXMF reads; version `1.4` added source-free basic LXMF
submission; version `1.5` added bounded nearby-LXMF peer discovery; and the
current version is `1.6`, adding bounded authenticated NomadNet fetch
start/poll. A decoder accepts major
version 1 with any minor version, skips unknown numeric map fields, and rejects
another major version.
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
operations remain exempt from stable compatibility as described below. Within
the current API 1.6 experimental NomadNet contract, start outcome, pending
phase, poll state, and terminal failure are also closed numeric vocabularies;
their decoders reject unknown values rather than treating them as another
state.

The decoder consumes exactly one logical CBOR item. Trailing bytes are rejected;
stream recovery and message boundaries belong to the separate framing crate.
The allocation-free strict skipper bounds container/tag nesting within an
operation body or one unknown field value to eight levels.

## Hard limits

| Limit | v1 value | Enforcement |
| --- | ---: | --- |
| logical message | 512 encoded bytes | before CBOR decode and by bounded encoding |
| operation body | 448 encoded bytes | before operation-specific decode |
| fields per recognized API map | 32 | immediately after each API map header |
| body/unknown-value container or tag nesting | 8 levels | strict allocation-free skip/validation |
| experimental outbound RNS DATA payload | 383 bytes | encode and decode |
| experimental inbound RNS inbox payload | 383 bytes | encode and decode |
| experimental LXMF read chunk | 416 bytes | request length and response construction |
| basic LXMF title | 295 bytes | encode and decode; body and composer limits still apply |
| basic LXMF content | 295 bytes | encode and decode; body and composer limits still apply |
| nearby LXMF announce application data | 256 bytes | encode and decode |
| NomadNet page path | 128 UTF-8 bytes | construction, encode, and decode; must be absolute, nonempty, and contain no NUL |
| NomadNet page response | 400 valid UTF-8 bytes | construction, encode, and decode |
| NomadNet request timestamp | `1..=9_007_199_254_740_991` whole milliseconds | construction and decode |
| NomadNet fetch ID | 16 bytes | decode; final eight-byte big-endian sequence must be nonzero |
| destination hash | 16 bytes | decode |
| idempotency key | 16 bytes | decode |
| encoded-packet SHA-256 | 32 bytes | decode |

The shared 383-byte ceiling matches the generic Rete encrypted destination-DATA
boundary used by raw submission and the raw-RNS qualification inbox. It is not
a promise that either operation or record will become the product LXMF API or
message-store format. Basic LXMF send now uses a distinct durable intent for
the exact complete signed wire through 431 bytes, so it does not raise or reuse
the generic 383-byte RNS DATA ceiling. A maximum NomadNet ready response has a
407-byte operation body and a 429-byte complete logical message even with a
maximum-width request ID, so it remains below both fixed limits without
chunking.

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
| `0x0003` | `identity.summary` | v1.1 | public/read-only |
| `0xf001` | `experimental.submit_rns_data` | feature-gated experimental | authenticated + `EXPERIMENTAL_SUBMIT_RNS_DATA` |
| `0xf002` | `experimental.rns_inbox.status` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf003` | `experimental.rns_inbox.peek` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf004` | `experimental.lxmf.next` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf005` | `experimental.lxmf.read` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf006` | `experimental.lxmf.basic_send` | feature-gated experimental | authenticated + `EXPERIMENTAL_SUBMIT_RNS_DATA` |
| `0xf007` | `experimental.lxmf.peer_next` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf008` | `experimental.nomad.fetch_start` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf009` | `experimental.nomad.fetch_poll` | feature-gated experimental | authenticated principal; no permission bit |

Numbers `0xf000..=0xffff` are experimental and can disappear or change without
API compatibility. `0xf001` is compiled only with the target-safe
`experimental-rns-data` Cargo feature; `0xf002` and `0xf003` are compiled only
with `experimental-rns-inbox`; and `0xf004` through `0xf007` are compiled only
with `experimental-lxmf`. `0xf008` and `0xf009` are compiled only with
`experimental-nomad`. A build without the corresponding feature returns
`UnsupportedOperation`. The adapter mirrors all four feature boundaries.
`dispatch_with_lxmf` independently exposes LXMF reads and basic send without
requiring or compiling the raw-RNS qualification mailbox;
`dispatch_with_inbox_and_lxmf` accepts one owner
implementing both stores and is compiled only when both corresponding features
are enabled. `dispatch_with_nomad` composes independent submission and NomadNet
ports, while
`dispatch_with_inbox_lxmf_peer_discovery_and_nomad` adds that independent
NomadNet owner to the complete existing appliance surface. Capability responses
restrict the codec snapshot to the adapter's own compiled operations and each
port's runtime availability, so Cargo feature unification on another dependency
edge cannot make an adapter build advertise a missing dispatch arm.

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
| 7 | u8 | no | `experimental_rns_inbox`: 0 unavailable, 1 disabled, 2 available |
| 8 | u16 | no | maximum experimental inbox payload bytes; 383 when implemented, otherwise 0 |
| 9 | u8 | no | `experimental_lxmf`: committed enumeration/read availability |
| 10 | u16 | no | maximum exact normalized-wire bytes per LXMF read response; 416 when implemented, otherwise 0 |
| 11 | u8 | no | `experimental_lxmf_basic_send`: source-free composition/submission availability |
| 12 | u16 | no | structural per-field basic-LXMF title limit; 295 when implemented, otherwise 0 |
| 13 | u16 | no | structural per-field basic-LXMF content limit; 295 when implemented, otherwise 0 |
| 14 | u8 | no | `experimental_lxmf_peer_discovery`: bounded authenticated nearby-peer reads |
| 15 | u16 | no | maximum retained announce application data per peer; at most 256 |
| 16 | u8 | no | `experimental_nomad`: bounded authenticated NomadNet fetch availability; 0 unavailable, 1 disabled, 2 available |
| 17 | u16 | no | maximum UTF-8 NomadNet page-path bytes; 128 when implemented, otherwise 0 |
| 18 | u16 | no | maximum valid UTF-8 Micron page bytes returned by one fetch; 400 when implemented, otherwise 0 |

`CapabilitySnapshot::current()` is device-owned and cannot advertise packet
output or direct-radio TX. Key 2 says nothing about node-owned RNS traffic: an
accepted request advertised by key 3 may be routed over LoRa or any other
eligible Reticulum interface without granting a client direct radio control. A
higher dispatcher uses `CapabilitySnapshot::for_dispatch` to restrict that
codec-build snapshot; it can disable a capability but cannot enable one omitted
from the codec build. API 1.2 introduced keys 7 and 8, API 1.3 introduced keys 9
and 10, API 1.4 introduced keys 11 through 13, and API 1.5 introduced keys 14
and 15. API 1.6 introduced keys 16 through 18. All are optional on decode;
an older response therefore maps absent capabilities to unavailable with zero
limits. The E290 reports raw-inbox and committed-LXMF reads only after their
exact durable stores mount, and reports basic send only when durable submission
and the local `lxmf.delivery` source are available. Faults disable the affected
capability rather than inventing a volatile substitute. Peer discovery is
advertised only by a dispatcher with the bounded projection port. NomadNet
fetch is advertised only by a dispatcher with the independent bounded fetch
port; keys 17 and 18 are zero when that capability is unavailable and retain
the structural limits when runtime policy reports it disabled.
Keys 12 and 13 are independent codec field bounds, not a guarantee that every
295-byte title/content combination fits the 448-byte request body or the
current product's 319-byte direct-content/431-byte complete-wire boundary.

### `identity.summary` (`0x0003`)

Request body: a map with no recognized fields. Unknown fields are permitted for
evolution.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | complete destination hash for the node's primary permanent application destination |
| 1 | bytes(16) | no | complete local inbound Single `lxmf.delivery` destination hash |

The response is intentionally public, copy-only identity metadata; it contains
no private key, credential, principal, permission, route, or storage handle. It
requires no logical operation permission. A physical E290 device-API bearer is
still authenticated before it carries any logical operation, including this
one. The adapter receives this scalar from the node owner separately from
`SubmissionPort`, so the read performs no storage or radio I/O and cannot gain a
mutable device capability. API 1.3 and later E290 images include key 1 only when
the durable LXMF service activated and registered that local destination.

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
the node router and the LoRa-first interface after the durable barriers. The
minimal single-flight USB bearer now exposes this API and has completed one
bounded powered submission/peer-proof/status path. The future client-facing message
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
the credential store is now boot-composed. External live admission now has its
first powered-qualified USB handshake and session bearer. Initialization,
pairing, authenticated request/reply, durable submission, peer proof, and post-
re-enumeration status have run on the E290 pair; broader lifecycle, fault, and
bearer qualification remains open.

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
`Internal`. The minimal authenticated USB bearer now exposes this operation
when a credential is active. The pre-authentication bootstrap cannot create an
authenticated owner. A powered E290 submission has completed durable acceptance,
LoRa delivery, peer decrypt/proof, terminal projection, and post-re-enumeration
status. Acceptance
is not a delivery guarantee; a later status can report no
path, delivery timeout, downstream rejection, or an internal failure. The ID
can be queried through `submission.status`. The response contains no
destination, payload, prepared packet, packet fragment, or packet-borrowing
handle.

The E290 host utility's `submit-and-wait` command writes and explicitly flushes
one machine-readable stdout record immediately after authenticating this
accepted response and before beginning status polling:
`command=submit-and-wait outcome=accepted device_id=<32-hex>
session_id=<32-hex> submission_id=<u64>`. A rejected request, malformed
response, or host failure before authenticated acceptance emits no such record.
This is a host-tool observation boundary, not an additional wire message or a
change to acceptance semantics.

The storage model scopes idempotency by the authenticated principal. Repeating
the same key with identical semantic destination/payload content returns the
original submission ID. Reusing it for different content returns immediate
error 10 and must not mutate the original submission. The portable adapter
derives the principal from `DispatchContext`; it never trusts request bytes.

### `experimental.rns_inbox.status` (`0xf002`)

This read-only operation exposes the bounded state of ADR 0011's durable
raw-RNS qualification slot. It is not an LXMF inbox, a conversation model, or a
general message-store API.

Request body: a map with no recognized fields. Unknown fields are permitted for
experimental evolution.

Successful experimental response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u16 | yes | retained item depth; 0 or 1 in the E290 qualification profile |
| 1 | u16 | yes | retained item capacity; exactly 1 in that profile |
| 2 | u64 | yes | saturating count of inbound items dropped since this boot |
| 3 | u16 | yes | maximum retained payload bytes; exactly 383 |
| 4 | bool | yes | whether retained items survive reboot; `true` for the mounted E290 store |

The first eligible item is retained as item 1. While occupied, each newer
inbound item is dropped rather than replacing the oldest item. The saturating
drop counter increments exactly once for every projected DATA item that is not
durably retained: an occupied slot, pressure behind the single deferred
candidate, an oversize payload, unavailable or fault-disabled inbox service, or
an admission fault. Deferral alone is not a drop. The counter is diagnostic
runtime state: it resets on reboot and does not mutate the committed record.
Payloads over 383 bytes are rejected without truncation. A
successful E290 status response always reports `durable=true`; if the exact
store did not mount or was disabled after a fault, the capability is unavailable
and this operation returns `CapabilityUnavailable` instead of presenting a
volatile mailbox.

### `experimental.rns_inbox.peek` (`0xf003`)

Request body: a map with no recognized fields. Unknown fields are permitted for
experimental evolution.

Successful occupied response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | opaque, nonzero device-assigned item ID; exactly 1 in format 1 |
| 1 | bytes(16) | yes | complete local Reticulum destination hash that received the DATA item |
| 2 | bytes(0..383) | yes | exact decrypted RNS DATA payload |

An empty mounted slot returns `NotFound`. Peek copies into an allocation-free,
fixed-capacity response owner and does not consume or acknowledge the item.
API 1.2 defines no acknowledgement, delete, overwrite, erase, reclamation, or
garbage-collection operation, so a committed item remains until an explicit
future developer/product migration or erase policy is designed outside this
API. Destination and payload are plaintext at rest in this developer
qualification format.

Both inbox operations require an authenticated principal, but API 1.2 adds no
persisted permission bit for them: any currently authenticated principal may
read the retained item. This intentionally simple developer policy must be
revisited before a production mailbox, multi-principal messaging, mutation, or
wireless bearer is enabled.

### `experimental.lxmf.next` (`0xf004`)

This read-only operation enumerates committed normalized LXMF messages in
physical commit order without returning their title, content, or fields.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero u64 | no | exclusive stable handle cursor; omit to request the first commit |

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero u64 | yes | stable committed-message handle |
| 1 | bytes(32) | yes | Python-compatible authenticated LXMF message ID |
| 2 | bytes(16) | yes | local `lxmf.delivery` destination |
| 3 | bytes(16) | yes | authenticated source `lxmf.delivery` destination |
| 4 | u64 | yes | exact IEEE-754 bits of the decoded LXMF timestamp |
| 5 | u32 | yes | complete normalized-wire length |
| 6 | u32 | yes | decoded title byte length |
| 7 | u32 | yes | decoded content byte length |
| 8 | u32 | yes | encoded MessagePack fields-map length |
| 9 | bytes(32) | yes | SHA-256 of the complete normalized wire bytes |

No commit after the cursor, an empty store, or an unknown/stale cursor returns
`NotFound`. The handle is logical store identity, not a flash address. It is
designed to remain stable across reboot and a future compactor.

### `experimental.lxmf.read` (`0xf005`)

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero u64 | yes | committed-message handle from `experimental.lxmf.next` |
| 1 | u32 | yes | zero-based normalized-wire offset |
| 2 | u16 | yes | requested maximum bytes, 1 through 416 |

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero u64 | yes | echoed handle |
| 1 | u32 | yes | first returned normalized-wire offset |
| 2 | u32 | yes | complete normalized-wire length |
| 3 | bytes(1..416) | yes | exact nonempty committed bytes |

Clients advance by `offset + bytes.len()` until that value equals key 2. They
must require a stable handle/total length and verify the complete SHA-256 from
the corresponding summary before using the message. The E290 host client also
parses the complete normalized wire and cross-checks destination, source,
timestamp, message ID, and component lengths. An unknown handle returns
`NotFound`; an out-of-range offset reaches product dispatch and returns
`InvalidRequest`. A requested length of zero or greater than 416 is structurally
invalid and is rejected by CBOR decoding before dispatch. Reads do not consume,
acknowledge, or delete a message.

### `experimental.lxmf.basic_send` (`0xf006`)

This mutation is the first semantic client send path. It selects no radio,
interface, or delivery method: its exact signed LXMF-message intent is routed
by the same transport-neutral node and can later use LoRa, Wi-Fi, BLE, or
another eligible Reticulum interface.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | remote inbound Single `lxmf.delivery` destination |
| 1 | u64 | yes | Unix timestamp in whole milliseconds, `1..=8_796_093_022_207_999` |
| 2 | bytes(0..295) | yes | binary title |
| 3 | bytes(0..295) | yes | binary content |
| 4 | bytes(16) | yes | principal-scoped idempotency key |

There is deliberately no source field. The device uses its registered inbound
Single `lxmf.delivery` destination and resident private identity to construct
and sign the Python-compatible basic message. The first subset has an empty
fields map and no stamp. The codec accepts the
timestamp as an unsigned integer; the E290 product composer rejects zero and
values above `8_796_093_022_207_999`, the exact positive whole-millisecond
binary64 subset. It also rejects title/content combinations that exceed the
single-Link-packet direct boundary: 319 bytes of Python LXMF `content_size` or
431 bytes of complete signed wire. Empty title and empty content together are
valid and match the canonical Python vector. The 448-byte encoded-body limit
also applies to the combination even though keys 2 and 3 each have a 295-byte
structural field limit. Durable acceptance does not promise immediate delivery:
the current runtime can send an eligible carrier through 391 bytes, or a
407-byte complete wire, using the compatible Header-1 opportunistic path.
Complete wires of 408--431 bytes select or reuse a direct Link and fit one Link
DATA packet; a routed Header-2 path can impose the smaller 383-byte carrier
ceiling and trigger the same direct selection for a smaller message. Resource
delivery above the 431-byte inline boundary remains unfinished.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | durable submission ID, queryable through `submission.status` |
| 1 | bytes(32) | yes | Python-compatible authenticated LXMF message ID |

The product composer creates the exact complete signed wire before invoking
durable acceptance. Composition failure, unavailable local source, or a wire
that exceeds the current 431-byte inline LXMF-message intent performs no
acceptance write. A
successful reply means those exact bytes and destination are durably queued;
it does not mean the peer has received it. `lxmf-send-and-wait` follows key 0
through `submission.status` until delivery or a terminal failure.

Idempotency uses the authenticated principal plus key 4. An exact retry must
retain destination, timestamp, title, content, and key; it returns the original
submission and message IDs without adding a record. Reusing the key with
different semantic content returns `IdempotencyConflict`. The complete-wire
intent closes the former 384-through-391 opportunistic carrier gap without
raising the separate 383-byte generic-RNS DATA ceiling. Automatic delivery
currently reuses a compatible active product Link first, otherwise prefers an
eligible opportunistic packet, and establishes a direct Link when the selected
packet form cannot fit. The first bounded establishment and Link-DATA receipt
projection lifecycle is implemented and powered-qualified for one fresh-Link
success. Active-Link reuse, responder/backchannel reuse, Resource, and the
broader fault/pressure matrix remain unqualified. The runtime uses bytes
`16..` as the compatible opportunistic carrier without recomposing the
accepted message.

The current E290 PSRAM profile retains 128 accepted submissions without
terminal reclamation. Its 129th novel request returns `CapacityExhausted`
without a write, while an exact retry of any retained idempotency key still
returns the original IDs at capacity. The physical journal separately reserves
at most 154 complete submission lifetimes. This is a bounded current profile
rather than an API-v1 or long-term product ceiling. Earlier one-entry and
16-entry proof artifacts remain revision-bound; they neither set the current
limit nor constitute powered qualification of a 128-entry fill.

### `experimental.lxmf.peer_next` (`0xf007`)

This authenticated read exposes one record from the firmware's volatile
projection of validated `lxmf.delivery` announces. It is display and contact
selection evidence, not route authority, appliance pairing, or a private-key
export. The E290 product profile retains 32 peers; the logical API permits at
most 256 authenticated announce application-data bytes per record.

The first request body is empty. A continuation must contain both:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(8) | together | public boot/incarnation token |
| 1 | u64 | together | exclusive observation generation; zero means before all observations |

The response contains:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(8) | yes | current incarnation for the next cursor |
| 1 | u64 | yes | exclusive generation for the next cursor |
| 2 | nonzero u64 | no | latest generation in the response snapshot |
| 3 | nonzero u64 | no | oldest generation represented by a retained peer |
| 4 | bool | yes | requested history was reset, updated away, or evicted |
| 5 | map | no | one peer record |

The peer map contains destination and identity hash at keys 0 and 1,
application data at key 2, hop count and observing-interface ID at keys 3 and
4, optional whole-dBm RSSI and whole-dB SNR at keys 5 and 6, saturating
observation age in milliseconds at key 7, and its nonzero generation at key 8.
A cursor from an older boot resets to the first retained record with
`history_gap=true`. The API never fabricates signal data or mutates Rete's path
table.

### `experimental.nomad.fetch_start` (`0xf008`)

This authenticated mutation begins one bounded, principal-owned NomadNet page
fetch. It identifies a Reticulum destination and request path but does not
select LoRa, Wi-Fi, BLE, or any other bearer. Acceptance means that the
boot-lifetime fetch owner accepted or replayed the semantic request; it does
not mean that a path, Link, remote response, or page already exists.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | complete remote `nomadnetwork.node` destination hash |
| 1 | text(1..128 UTF-8 bytes) | yes | absolute page path; first byte `/`, with no NUL |
| 2 | u64 | yes | caller-selected Unix timestamp in whole milliseconds, `1..=9_007_199_254_740_991` |
| 3 | bytes(16) | yes | principal-scoped idempotency key |

The timestamp range is lossless in JSON and JavaScript integer interchange.
It does not promise exact binary64 millisecond spacing at extreme dates when a
product later converts milliseconds to Reticulum's seconds representation.
Repeating a request must retain the original integer value.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | opaque boot-scoped fetch ID |
| 1 | u8 | yes | start outcome: 0 accepted, 1 replayed |

The fetch ID's first eight bytes identify the boot incarnation. Its final eight
bytes are a nonzero big-endian sequence. Clients must compare and return all
16 bytes without treating either component as authority or deriving another
ID. The authenticated principal remains the ownership boundary.

Idempotency is scoped by authenticated principal plus key 3. Repeating the
same destination, path, timestamp, and key returns the original ID with outcome
1. Reusing that principal/key pair for different semantic content returns
`IdempotencyConflict`. A fresh request for which no bounded owner slot is
available returns `CapacityExhausted` and allocates no ID. The portable
contract defines no cancellation operation in API 1.6.

The current E290 product profile provides exactly one slot. While that slot is
nonterminal, any distinct principal/key request returns
`CapacityExhausted`. Exact same-principal/key semantics replay the original ID.
Ready and failed outcomes remain repeatable until the next distinct accepted
start, which evicts the terminal outcome and allocates the next boot-scoped ID.
Polling the evicted ID then returns the same `NotFound` used for foreign and
stale-boot IDs.

The Expo controller retains the exact ID after a bearer poll error or its
120-second presentation timeout and blocks distinct starts until the user
resumes that ID or explicitly abandons it. Abandon is local UI recovery for an
ID made stale by a board reset; it sends no cancellation and makes no claim
about a still-running same-boot device fetch.

### `experimental.nomad.fetch_poll` (`0xf009`)

This authenticated read polls one fetch owned by the current principal.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | opaque fetch ID returned by `fetch_start` |

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u8 | yes | poll state: 0 pending, 1 ready, 2 failed |
| 1 | state-specific | yes | pending phase, complete page bytes, or terminal failure |

For state 0, key 1 is one pending-phase `u8`:

| Value | Phase |
| ---: | --- |
| 0 | path lookup |
| 1 | Link establishment |
| 2 | request preparation |
| 3 | awaiting first-dispatch confirmation |
| 4 | awaiting the correlated response |

For state 1, key 1 is `bytes(0..400)` containing one complete valid UTF-8
Micron page. Empty page text is valid. The page is never truncated: an
oversized or invalid-UTF-8 downstream result becomes a typed failed state
instead of a partial ready response.

For state 2, key 1 is one terminal-failure `u8`:

| Value | Failure |
| ---: | --- |
| 0 | no usable path |
| 1 | Link preparation, dispatch, establishment, or retention |
| 2 | request preparation, dispatch, or remote processing |
| 3 | confirmed-request response timeout |
| 4 | page too large |
| 5 | invalid UTF-8 |
| 6 | internal invariant or backend failure |

The port receives the authenticated principal together with the fetch ID.
Missing, foreign-principal, and stale-boot IDs all return the same `NotFound`
response. A ready or failed result is stable only while the bounded
boot-lifetime owner retains it; API 1.6 makes no persistence or reboot-survival
claim. The portable codec and adapter tests cover these states, principal
forwarding, hidden foreign IDs, closed discriminants, and the maximum 400-byte
page. Product-owner host tests additionally cover the E290 one-slot lifecycle
and its permanent authenticated-dispatch composition. None of these tests
constitutes RF or powered qualification.

### Powered inbox fault-isolation evidence

Four powered E290 boots exercised deterministic mount faults: an interrupted
claim, an interrupted commit, an invalid digest, and a valid committed record
bound to board 3e but mounted on board 3f. In every case, `system.capabilities`
reported inbox availability 0 and maximum inbox payload 0. Authenticated
`experimental.rns_inbox.status` and `experimental.rns_inbox.peek` each returned
`CapabilityUnavailable` (error code 7), and the peek client created no output
file. One direct peer DATA/proof exchange nevertheless reached `Delivered` in
each case. This is bounded evidence that these four local mount failures
quarantine the inbox capability without disabling the exercised direct
Reticulum path; it is not a routing soak or a claim about every storage fault.

A separate powered same-boot admission-fault image first advertised inbox
availability 2 (`Available`) and the 383-byte limit. Its software NOR wrapper
forwarded the claim and body writes, acknowledged but suppressed the terminal
commit write, and the exact commit-stage readback mismatch disabled inbox
service and counted the candidate as dropped. After USB-only re-enumerations
established fresh one-shot authenticated sessions, capabilities reported inbox
availability 0 and limit 0; separate status and peek sessions again returned
error 7, and peek created no output file. The
debugger-visible RAM evidence remained `3/1/1/0/1/1`, in the fixed order
`write_calls`, `commit_suppressed`, `expected_commit_readback_mismatch`,
`unexpected_admission_failure`, `service_disabled`, and
`dropped_since_boot`. Retaining those same-boot counters across USB detaches and
reattachments establishes that this observation did not depend on a CPU reset.

Neither fault path changes the API 1.2 wire encoding, operation numbers,
authentication handshake, or inbox authorization policy. Both are deliberately
narrow software-injection qualifications: they do not simulate a physical
power cut, and each routing observation covers one direct peer packet rather
than sustained routing, multi-hop behavior, or a full mailbox lifecycle.

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
Capacity exhaustion is an immediate rejection: no submission or fetch ID is
allocated, and retrying later may succeed. Idempotency conflict means the
principal reused a key for different request content; repeating the original
content remains safe. Neither immediate rejection is represented as a terminal
state of an accepted submission or fetch.

Codec failures (`DecodeError`) happen before dispatch and therefore do not have
a trusted request ID to echo in every case. A transport/session adapter may
return a logical error only after it has safely recovered the necessary
envelope context; otherwise it closes or resynchronizes according to its own
framing rules.

## Authorization contract

`authorize_request(context, request)` applies the common baseline:

- `system.capabilities` requires no logical operation permission;
- `identity.summary` requires no logical operation permission;
- `submission.status` requires an authenticated principal and its read bit;
- experimental outbound RNS DATA submission requires an authenticated principal
  and `EXPERIMENTAL_SUBMIT_RNS_DATA`;
- experimental RNS inbox status and peek require an authenticated principal but
  no persisted permission bit;
- experimental LXMF next/read require an authenticated principal but no
  persisted permission bit;
- experimental basic LXMF send requires an authenticated principal and reuses
  `EXPERIMENTAL_SUBMIT_RNS_DATA` until a future stable messaging policy is
  designed;
- experimental NomadNet fetch start/poll require an authenticated principal but
  no persisted permission bit.

The logical codec can represent an unauthenticated context for internal callers
and policy tests. ADR 0006's physical device-API bearers are stricter: every
wire operation crosses an authenticated application session. A stale, missing,
pending or revoked credential is never converted to
`DispatchContext::UNAUTHENTICATED`, even for `system.capabilities`.

Authentication, ownership filtering, rate limiting, idempotency scope, physical
presence, and high-assurance session encryption are not solved by this codec.
The inbox and LXMF store's shared authenticated-principal read policy is global:
any authenticated principal can read the retained LXMF messages, with no
per-principal mailbox ACL. This is POC policy, not a general authorization
decision for later multi-user messaging.
The portable adapter scopes status and experimental-operation idempotency by the
principal from `DispatchContext`, never by bytes supplied in a request. NomadNet
polling also supplies that principal to the fetch owner; absent and
other-principal IDs are deliberately indistinguishable. The
portable session core deliberately emits only a credential ID/generation grant.
`AuthenticatedGrant::revalidate` checks that reference against the immutable
device-owned authority and returns a `DispatchLease` whose borrow remains alive
through immediate synchronous dispatch. Its higher-ranked callback supplies a
borrow of the non-copyable `DispatchContext`; the exact context value cannot be
moved out, but trusted linked code can reconstruct equivalent scalar facts with
the public constructor. Immediate dispatch, no unauthenticated fallback and no
port call after rejection remain composition rules, not an unforgeable Rust
capability. The permanent E290 node now follows them: the resident credential
runtime revalidates current authority, then borrows one credential-disjoint,
operation-scoped owner implementing submission, raw-inbox, LXMF-read, and
LXMF-compose ports only for synchronous adapter dispatch, plus an independent
operation-scoped API 1.6 NomadNet port borrowing its boot-lifetime fetch owner.
Revalidation failure returns the generic authentication-required response with
zero port I/O; it never constructs an unauthenticated context. Principal and
permissions come from the exact active record. Live authority
replacement must also pass exact-next-revision successor validation so changed
authorization cannot reuse a session generation. E290 firmware now mounts and
recovers the portable store before any other product-store write and retains
its `Ready`, authentication-only, uninitialized-erased,
initialization-interrupted, blocked, corrupt, or backend-failed state. The
resident credential runtime and sole-owner physical drive are invoked by both
the E290 pre-authentication status/initialize lane and its routed Begin/
ProofStart/Activate/AbortCurrent lifecycle. The composed node lane consumes only
session-minted grants and retains malformed logical owners terminally. The USB
serving runtime selects active credentials and keeps authentication state
outside request CBOR; connection-level rate limits remain deferred. The pre-
authentication bootstrap does not create a session grant or admit a logical
request.

Semantic journal schema 3 persists the principal, idempotency key,
operation-specific intent, credential ID/generation, complete authority
revision, authorization-policy version, and exact granted permission mask.
The adapter constructs that storage-owned snapshot only after authorization
succeeds; a rejected request invokes no port. A retry after credential rotation
returns the original ID and retains the original evidence. See
[ADR 0008](../adr/0008-durable-authorization-provenance.md).

## Golden vectors

The canonical API 1.6 `system.capabilities` request for request ID 42 uses minor
byte `06` and is:

```text
a4 00 a2 00 01 01 06 01 18 2a 02 01 03 a0
```

The canonical API 1.6 `identity.summary` request for request ID 42 is:

```text
a4 00 a2 00 01 01 06 01 18 2a 02 03 03 a0
```

The wire tests freeze these requests, both identity-response forms, all feature
compositions of the nineteen-field API 1.6 capability response, older maps with
absent optional capability fields, typed permission/capacity/idempotency error
responses, raw submission request/acceptance, exact `0xf002`/`0xf003` inbox
vectors, exact `0xf004`/`0xf005` LXMF list/read vectors, and source-free
`0xf006` request/acceptance plus boot-scoped `0xf007` peer-page vectors. API 1.6
adds exact `0xf008` start request and accepted/replayed response vectors plus
`0xf009` poll request and pending/ready/failed vectors.
They also cover every submission failure, state invariants, closed numeric
enums, unknown fields, unknown operations, missing and duplicate known fields,
every truncated golden prefix, trailing bytes,
message/body/payload/nesting limits, indefinite-value rejection, fixed
byte-string widths, borrowed payload storage, authorization, and the
packet-output/direct-radio-TX safety values, separate outbound-RNS submission
advertisement, authenticated-only inbox reads, empty `NotFound`, and bounded
owned peek payloads. NomadNet coverage additionally proves absolute/no-NUL
128-byte path validation, the JavaScript-safe nonzero timestamp bound, nonzero
fetch-ID sequences, authentication without a new persisted permission bit,
principal forwarding and hidden foreign IDs, start replay/conflict/capacity
mapping, closed state/phase/failure values, and the exact 407-byte body /
429-byte complete-message maximum for a 400-byte page.

## Validation profiles

Run the default and each experimental feature composition explicitly from the
workspace root:

```sh
cargo test --locked -p reticulum-device-api
cargo clippy --locked -p reticulum-device-api --all-targets -- -D warnings

cargo test --locked -p reticulum-device-api --features experimental-rns-data
cargo clippy --locked -p reticulum-device-api --all-targets \
  --features experimental-rns-data -- -D warnings

cargo test --locked -p reticulum-device-api --features experimental-rns-inbox
cargo clippy --locked -p reticulum-device-api --all-targets \
  --features experimental-rns-inbox -- -D warnings

cargo test --locked -p reticulum-device-api --features experimental-lxmf
cargo clippy --locked -p reticulum-device-api --all-targets \
  --features experimental-lxmf -- -D warnings

cargo test --locked -p reticulum-device-api --features experimental-nomad
cargo clippy --locked -p reticulum-device-api --all-targets \
  --features experimental-nomad -- -D warnings

cargo test --locked -p reticulum-device-api --all-features
cargo clippy --locked -p reticulum-device-api --all-targets \
  --all-features -- -D warnings

cargo check --locked -p reticulum-device-api --no-default-features \
  --target riscv32imac-unknown-none-elf
cargo check --locked -p reticulum-device-api --no-default-features \
  --all-features --target riscv32imac-unknown-none-elf
```

The host commands validate the default, each independent experimental surface,
and their composition. The final two commands prove the default and complete
feature graph remain `no_std` on an installed `target_os = "none"` target.

Validate authenticated dispatch independently across the host feature profiles
and the ESP32-S3 graph:

```sh
cargo test --locked -p reticulum-device-api-adapter
cargo clippy --locked -p reticulum-device-api-adapter --all-targets -- -D warnings

cargo test --locked -p reticulum-device-api-adapter --features experimental-rns-data
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features experimental-rns-data -- -D warnings

cargo test --locked -p reticulum-device-api-adapter --features experimental-rns-inbox
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features experimental-rns-inbox -- -D warnings

cargo test --locked -p reticulum-device-api-adapter --features experimental-lxmf
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features experimental-lxmf -- -D warnings

cargo test --locked -p reticulum-device-api-adapter --features experimental-nomad
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features experimental-nomad -- -D warnings

cargo test --locked -p reticulum-device-api-adapter --all-features
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --all-features -- -D warnings

cargo test --locked -p reticulum-device-api-adapter \
  --features reticulum-device-api/experimental-rns-data
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features reticulum-device-api/experimental-rns-data -- -D warnings

cargo test --locked -p reticulum-device-api-adapter \
  --features reticulum-device-api/experimental-rns-inbox
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features reticulum-device-api/experimental-rns-inbox -- -D warnings

cargo test --locked -p reticulum-device-api-adapter \
  --features reticulum-device-api/experimental-lxmf
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features reticulum-device-api/experimental-lxmf -- -D warnings

cargo test --locked -p reticulum-device-api-adapter \
  --features reticulum-device-api/experimental-nomad
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features reticulum-device-api/experimental-nomad -- -D warnings

cargo +esp check --locked --release -p reticulum-device-api-adapter \
  --all-features --target xtensa-esp32s3-none-elf
cargo +esp clippy --locked --release -p reticulum-device-api-adapter \
  --all-features --target xtensa-esp32s3-none-elf -- -D warnings
```

Validate the separately bounded portable bearer-edge contracts independently
of the product-specific physical adapter:

```sh
cargo test --locked \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session
cargo clippy --locked --all-targets \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session -- -D warnings
cargo check --locked \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  --target xtensa-esp32s3-none-elf
python3 interop/python/generate_device_api_session_vectors.py --check
PYTHONPATH=interop/python python3 -m unittest -v \
  interop/python/test_device_api_session_vectors.py
python3 interop/python/generate_device_api_pairing_vectors.py --check
python3 interop/python/test_device_api_pairing_vectors.py
```

Together these required gates prove that feature unification cannot make a
dependency-only experimental profile advertise an adapter-local operation that
is absent. The
adapter's focused tests cover exact authorization and zero-port-call rejection,
request context, version/capability behavior, principal isolation, every durable
lifecycle mapping, maximum-size owned-payload acceptance/replay/conflict,
acceptance across remount, stable capacity and identifier-exhaustion errors,
faulted and pending status gating, wrong-binding rejection without I/O, and
lost-write reconciliation. Its inbox profile separately covers authentication
before either port, no-permission-bit reads, disjoint port dispatch, available/
disabled/faulted capability mapping, empty `NotFound`, and an owned maximum-size
peek response. The standalone LXMF profile proves that `dispatch_with_lxmf`
needs no raw-inbox feature or port; the combined profile proves that
`dispatch_with_inbox_and_lxmf` invokes only the selected method on its single
flash-capable owner. The NomadNet profile proves authenticated start/poll,
principal forwarding, hidden foreign IDs, replay/conflict/capacity outcomes,
independent availability, and composition with the complete existing
appliance dispatcher without acquiring its flash-capable owner. The session
crate now exposes both server and public
allocation-free `no_std` client typestates. Its tests and the live-pairing tests
plus their independent Python vectors cover canonical hello/proof derivation,
direction-separated record tags, downgrade/reflection/replay/generation/reset
failures, exact sequence policy, partial-write typestate, every pairing flight and
transcript byte, substituted continuations, activation confirmation, malformed
shapes, and secret-owner drop behavior. Target checks exercise the portable
layers directly on `no_std` bare-metal builds.

The separate permanent-E290 composition gate now covers the static depth-one
authenticated handoff, current-authority node dispatch through the combined
submission/raw-inbox/LXMF-read/LXMF-compose owner, retained reply pressure, and
terminal malformed-owner quarantine in addition to the third USB/GPIO task,
active-low stable-time debounce, an 8 ms missed-SOF suspension that retains its
epoch and sequence until bus reset, connection-epoch and sequence exhaustion,
duplicate/gap rejection, depth-one pressure, exact durable reply correlation,
causal control/live ordering, and node-owned status/initialize plus live-
pairing dispatch. The API 1.2 inbox assertions in that composition gate are
source/host evidence; the separate E290 runbook records the bounded powered
commit/readback/reset/drop-newest run. The earlier authenticated-node-foundation
release image predates the minimal bearer.
Button/control arbitration is bounded. A stable High transition is latched
before a later Low; a raw-sample gap of at least 20 ms cancels a possible hold
and suppresses Low until a fresh debounced High. Once every response byte enters
the endpoint FIFO, firmware requests `WR_DONE` and releases that software
owner; a later response remains backpressured until FIFO space is available.
Each fresh connection also resets the publication latch and debouncer to Low,
so release evidence retained for an older epoch cannot arm the new epoch; the
replacement epoch must observe a complete fresh High debounce.

The `e290-pairing-control` client keeps one serial port open across status,
initialization, and polling. The separate `e290-pairing-live` client implements
`pair`, `resume`, and physically confirmed `abort-current`. Before Begin, `pair`
creates, syncs, and read-verifies an owner-only 96-byte reservation. After a
durable offer it atomically installs a complete Pending file before ProofStart;
`resume` reopens that file and validates the exact device ID, credential ID,
generation, and PSK continuation. Secret serial and state scratch are zeroized.
Secure `pair`/`resume` persistence is currently Unix-only. Pair requires a
starting sequence no greater than `u64::MAX - 3`; resume requires no greater
than `u64::MAX - 2`.

Both clients assert DTR and clear RTS; closing/reopening the TTY does not start a
new epoch. A post-send I/O failure or request timeout leaves its last sequence
consumed-or-ambiguous and requires a confirmed USB reset epoch before restarting
at zero. After an ambiguous Activate, `e290-pairing-live` does not yet enter an
authenticated session to distinguish Pending from Active. It retains the state
file and must not guess Active or invoke AbortCurrent. `resume` is a proof
retry, not activation reconciliation; the firmware's ordinary authenticated
session remains available through a separate fresh USB epoch.

An earlier explicit-16-MiB image returned `initialization-required` and
`physical-presence-required` from both boards without writing credentials. The
preceding boot-quarantined 701,744-byte image with SHA-256
`14d9fd6dd482c47baa9afd2fda6a5ba1d69f46785bf23ae29f6b9fe561e4b212`
then matched exact address-zero reads from both boards. Each board reattached
and served sequence-zero `initialization-required` after the induced hard
reset. Simultaneous 120-second no-button workflows remained responsive through
sequences 1102 and 1100, respectively. Subsequent exact 8 KiB credential-
partition reads on both boards were entirely `0xff` with SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`,
confirming zero writes. The last powered 718,688-byte authenticated-node-foundation
image with SHA-256
`e20f6191cb2bfa78fbd7f3d588eb418913da3f1f89e3b80a4db0a28abaf414ea`
also matched exact address-zero reads from both boards. Both returned and then
recovered sequence-zero `initialization-required`, and both credential
partitions retained the same all-`0xff` digest. Its authenticated endpoint was
dormant in that exact image, so successful post-write readback and powered
authentication remain open; this is not evidence for the subsequently composed
minimal bearer. The firmware selects no-op logging, leaving one shared
COBS-framed owner to multiplex initialization control, live pairing, and
authenticated-session records as the sole application-owned USB byte stream.
Powered macOS full re-enumeration replaced
the service and restored sequence zero after firmware detachment, USB-RAM scrub,
and reattachment. A non-seizing in-place `ResetDevice` returned success but left
the endpoint stale and is not an accepted recovery primitive. The image-readback
hard-reset reattachment path is bounded powered evidence, while suspend/resume,
controlled cuts, and the ROM/bootloader interval before the application
quarantine remain to be qualified.

The powered API 1.1 image packaged a 686,176-byte application as a
751,712-byte merged image with SHA-256
`4285fcaa9df6a6f0314ed4735377ea986b0efcafafc2710ad7594489a49b4795`.
Exact address-zero readbacks matched on both E290 boards. The authenticated
sender reported primary destination
`c99e8ff1ec8629e4e1290e14462ae8af`; the provisioned receiver reported
`83a09ed807a0a7c631386deaa0448fb9`. Submission 1 prepared a 131-byte packet
whose full encoded-byte SHA-256 was
`df937860f5225deb9d2350c6f3a46f33bd659ccbcb6b47267add47c9a287a4fe`.
The receiver matched its local destination, decrypted the DATA packet, and
returned a valid Reticulum proof; sender status became `Delivered` in about 2.6
seconds. A full sender USB re-enumeration followed by a fresh authenticated
session returned the same terminal state, length, and digest. This qualifies
the exact sender USB-to-durable-runtime-to-LoRa-to-peer-proof path. The proof
means that the receiver accepted/decrypted the protocol packet and produced the
Reticulum delivery proof; it does not mean the receiver committed the plaintext
to the API 1.2 inbox. Application persistence/peek, multi-hop routing, session
resumption, Wi-Fi bearer exchange, and the mobile BLE lifecycle matrix remain
unqualified on powered hardware. The later BLE proof independently qualifies
the local GATT carrier and authenticated device-API session, not this USB-to-LoRa
submission path.
