# Device API v1 logical protocol

Status: unreleased API 1.18 logical codec, portable authenticated dispatch, and
E290 product ownership implemented
over one operation-scoped owner exposing narrow durable-submission, raw-inbox,
committed-LXMF-read, source-free basic-LXMF-compose, and bounded nearby-peer
ports plus an independent bounded NomadNet-fetch port and a same-owner,
redacted network-configuration port. API 1.12 also defines authenticated,
read-only node and retained-route diagnostics through a separate narrow port.
API 1.13 adds durable LXMF mailbox collection status and acknowledgement
through the existing authenticated LXMF port. API 1.14 added optional
receiver-local ingress evidence, portable Reticulum probe
start/poll messages, and atomic LoRa-profile snapshot key 10 and mutation kind
7 while retaining the power-only key 9/kind 6 compatibility path. API 1.15
adds prepared-packet-correlated terminal LoRa DATA diagnostics. API 1.16 adds a
bounded, boot-aware trace that correlates durable submissions, route selection,
logical RX, terminal DATA TX, and delivery-attempt outcomes. API 1.17 adds an
optional typed phone-location snapshot to basic LXMF send. The device encodes
that snapshot as Sideband-compatible LXMF telemetry and signs it with the
message. API 1.18 adds the typed `RetryLater` response for transient retained
flash ownership without conflating it with permanent faults, missing profiles,
or structural capacity. The app and E290 source compose these additions, but
none has a powered field qualification yet.
This document freezes the operation and field numbers exercised by
`reticulum-device-api`; `reticulum-device-api-adapter` implements capabilities,
the public primary-destination summary, and principal-scoped submission status
in its default build. It adds target-safe durable experimental outbound RNS DATA
submission behind `experimental-rns-data`, experimental raw-RNS inbox
status/peek behind `experimental-rns-inbox`, and committed-LXMF next/read plus
source-free basic send behind `experimental-lxmf`. API 1.6 adds authenticated
bounded NomadNet fetch start/poll behind `experimental-nomad`; API 1.7 adds
redacted Wi-Fi/TCP configuration, compare-and-swap mutation, and live
secret-free status behind `experimental-network-config`. API 1.8 adds
DNS-hostname peers, gateway/RMAP policy, and authenticated manual service
announces. API 1.9 adds typed TCP backoff and last-failure diagnostics. API 1.10
adds bounded DNS-path diagnostics that distinguish system DNS from raw DHCP and
public resolver attempts. API 1.11 adds the boot-applied E290 LoRa transmit-
power selection. API 1.12 adds bounded cross-interface, LoRa, RNS, and retained-
route diagnostics. API 1.13 adds the durable LXMF collection watermark and
acknowledgement. API 1.14 adds path-and-proof probe wire types, optional
first-arrival evidence, and the atomic LoRa-profile key 10/kind 7 extension.
API 1.15 adds LoRa terminal packet-family and DATA packet-identity evidence.
API 1.16 adds the authenticated packet-correlated radio trace page. API 1.17
adds optional Sideband-compatible location to basic LXMF send. API 1.18 adds
the transient `RetryLater` error category. Separate
portable
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
authenticated adapter boundary. API 1.7 adds the board-owned network
configuration and runtime-status surface; API 1.8 extends its desired state,
API 1.9 adds typed volatile TCP failures, API 1.10 adds bounded DNS-path
details, API 1.11 adds the selected LoRa transmit power, and API 1.12 adds
authenticated node and retained-route diagnostics. API 1.13 adds authenticated
LXMF mailbox collection status and acknowledgement. The permanent E290 API
composes both
through the existing authenticated bearer path: NomadNet borrows its
independent transport-neutral client runtime, while network configuration stays
inside the sole flash-owning product port. The one-slot Nomad owner has a
bounded physical qualification: an iOS client selected an associated nearby
`nomadnetwork.node` destination, fetched the static page through one E290 over
LoRa from its peer, and rendered the Micron response. The Expo universal client
exposes manual and nearby-peer-associated destination selection, start/poll
progress, explicit retained-ID recovery, and selectable raw Micron text through
that same authenticated session. General Nomad directory behavior and richer
Micron content remain deferred; see the
[bounded powered proof](../e290-nomad-powered-proof.md).
The
[2026-07-22 API 1.4 POC](../e290-api14-lxmf-poc.md) powered-qualified same-boot
bidirectional send, Reticulum delivery proof, peer commit, enumeration, and
digest-verified readback on the E290 pair. Its final audited image also retained
both terminal sender records and both exact receiver wires across a physical
CPU reset. Controlled electrical power cuts and broader carrier/client behavior
remain open.

## Boundary

The crate is `no_std`, allocation-free, and Rete-independent. It owns logical
requests, responses, scalar capabilities, submission, inbox, bounded NomadNet
response types, and copy-only bounded diagnostics DTOs, plus the indexed-CBOR
codec and a small common authorization policy. It does not contain:

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
commit-order metadata, bounded exact-wire chunks, and durable collection state;
and `LxmfComposePort`
atomically composes with the device-owned source and durably accepts the exact
carrier. The independent `NomadFetchPort` accepts and polls principal-scoped
boot-lifetime fetches without exposing Link, request, router, radio, or
firmware-owner types. None of these ports exposes raw physical storage, a
radio, or private identity material. The E290 combines the durable and LXMF
traits on one operation-scoped value so its sole flash owner is never mutably
aliased. It separately constructs an operation-scoped NomadNet port borrowing
only the boot-lifetime Nomad API metadata and transport-neutral client runtime,
so the flash-capable owner is neither aliased nor transferred into a fetch.
`NodeDiagnosticsPort` separately copies bounded interface, radio, RNS, peer-
count, and retained-route state without exposing an actor, router, radio
driver, or mutable protocol owner.
`ReticulumProbePort` separately lends one boot-scoped, principal-owned
path-and-proof measurement. It bypasses durable submission and message storage
without exposing packet bytes or a physical interface.
The adapter repeats major-version validation,
applies the codec's authorization policy to trusted context, always emits the
current response version, echoes the request ID, and performs no direct flash,
framing, session, radio or node work. The default feature set handles public
capabilities and authenticated, principal-scoped status. Missing and cross-
principal IDs both return `NotFound`, so the adapter does not disclose another
principal's durable records. A port reports unavailable service as
`CapabilityUnavailable`; status returns `RetryLater` while another retained
mutation temporarily owns flash and fails closed with `Internal` for a latched
fault. It never publishes the deliberately lagging live index as if it were
current. Inbox capability is
advertised only while its exact durable store is mounted and enabled; there is
no volatile fallback. Public capabilities remain available in either condition.

## Version and evolution rules

The initial version was `1.0`; version `1.1` added `identity.summary`; version
`1.2` added optional raw-RNS inbox capability fields and feature-gated
status/peek; version `1.3` added optional `lxmf.delivery` identity metadata and
bounded committed-LXMF reads; version `1.4` added source-free basic LXMF
submission; version `1.5` added bounded nearby-LXMF peer discovery; and version
`1.6` added bounded authenticated NomadNet fetch start/poll; `1.7` added
redacted network configuration, compare-and-swap mutation, and live status;
`1.8` added DNS-hostname peers, gateway/RMAP policy, and manual announces;
`1.9` added typed TCP backoff/failure status; `1.10` added bounded DNS-path
status; `1.11` added boot-applied LoRa transmit-power selection; `1.12` added
authenticated node and retained-route diagnostics; `1.13` added durable LXMF
mailbox collection status and acknowledgement; `1.14` added portable Reticulum
probe wire types, receiver-local ingress evidence, and atomic LoRa-profile
snapshot key 10 and mutation kind 7 while retaining key 9/kind 6 power-only
compatibility; `1.15` added packet-correlated LoRa DATA terminal diagnostics;
`1.16` added the bounded boot-aware packet-correlated radio trace page; `1.17`
added optional Sideband-compatible message location to basic LXMF send; and the
current unreleased version is `1.18`, adding explicit transient `RetryLater`
responses for retained flash ownership. A decoder accepts major
version 1 with any minor version, skips unknown numeric map fields, and rejects
another major version.
Encoding an envelope with another major version fails with the typed
`EncodeError::UnsupportedVersion` before any message is emitted. All encoder
output uses definite maps, ascending numeric keys, and CBOR's preferred shortest
integer encodings. Decoders accept equivalent integer encodings but reject every
indefinite-length byte string, text string, array, or map, including one nested
inside an unknown field.

Every envelope and request body is an unsigned-integer-keyed CBOR map. Successful
responses also use map bodies except for API 1.16
`experimental.radio_trace.page`, whose bounded response is a compact fixed
array so three useful trace events still fit under the frozen body ceiling.
Known map fields may appear in any order. Every known field, including an
optional field, may appear at most once. Missing required fields and duplicate
known fields are errors. Unknown unsigned field numbers are skipped without
allocation. An unknown request operation or response kind is a typed error, not
an alias for another operation.

All known numeric enum vocabularies in stable API major 1 are closed: capability
availability, submission state, submission failure, and API error code. Their
documented discriminants are frozen, unknown discriminants are rejected, and a
new discriminant requires a new API major version. Minor-version evolution uses
new optional numeric map fields instead of extending these enums. Experimental
operations remain exempt from stable compatibility as described below. Within
the API 1.6 experimental NomadNet contract, start outcome, pending
phase, poll state, and terminal failure are also closed numeric vocabularies;
their decoders reject unknown values rather than treating them as another
state. API 1.7 applies the same rule to network mutation outcome, mutation and
credential kinds, Wi-Fi station state, and TCP-peer state. API 1.8 applies it
to the manual-announce disposition, and API 1.12 applies it to diagnostics
interface kinds/states, LoRa TX outcomes, and route-resolution categories.

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
| Reticulum probe ID | 16 nonzero bytes | construction and decode; boot-scoped |
| saved Wi-Fi profiles | 4 | model construction and decode |
| node-diagnostics interface slots | 4 | fixed response array |
| route-diagnostics entries per page | 4 | fixed dense-prefix response array |
| packet-correlated radio-trace entries per page | 3 | fixed dense-prefix response array plus encoded-body budget |
| Wi-Fi SSID | 1..32 bytes | model construction, encode, and decode |
| WPA2-Personal passphrase | 8..63 printable ASCII bytes | mutation construction and decode; never returned |
| Wi-Fi profile ID | 16 nonzero bytes | construction and decode |
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
| 3 | map, or array for a successful `0xf014` response | yes | operation-specific body |

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
| `0xf00a` | `experimental.network.config_get` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf00b` | `experimental.network.config_mutate` | feature-gated experimental | authenticated + `MANAGE_NETWORK_CONFIG` |
| `0xf00c` | `experimental.network.status` | feature-gated experimental | authenticated principal; no permission bit |
| `0xf00d` | `experimental.manual_service_announce` | experimental | authenticated principal; no permission bit |
| `0xf00e` | `experimental.node.diagnostics` | API 1.12 experimental | authenticated principal; no permission bit |
| `0xf00f` | `experimental.route_diagnostics.page` | API 1.12 experimental | authenticated principal; no permission bit |
| `0xf010` | `experimental.lxmf.mailbox_status` | API 1.13 feature-gated experimental | authenticated principal; no permission bit |
| `0xf011` | `experimental.lxmf.mailbox_acknowledge` | API 1.13 feature-gated experimental | authenticated principal; no permission bit |
| `0xf012` | `experimental.reticulum_probe.start` | API 1.14 experimental | authenticated + `EXPERIMENTAL_SUBMIT_RNS_DATA` |
| `0xf013` | `experimental.reticulum_probe.poll` | API 1.14 experimental | authenticated principal; no permission bit |
| `0xf014` | `experimental.radio_trace.page` | API 1.16 experimental | authenticated principal; no permission bit |

Numbers `0xf000..=0xffff` are experimental and can disappear or change without
API compatibility. `0xf001` is compiled only with the target-safe
`experimental-rns-data` Cargo feature; `0xf002` and `0xf003` are compiled only
with `experimental-rns-inbox`; and `0xf004` through `0xf007` plus `0xf010` and
`0xf011` are compiled only with `experimental-lxmf`. `0xf008` and `0xf009` are
compiled only with
`experimental-nomad`; `0xf00a` through `0xf00c` are compiled only with
`experimental-network-config`. A build without the corresponding feature
returns `UnsupportedOperation`. Operations `0xf00d` through `0xf00f` and
`0xf012` through `0xf014` are always
known by the codec; a dispatcher without the corresponding product port or
runtime capability returns `UnsupportedOperation` or
`CapabilityUnavailable`. The adapter mirrors all five Cargo-feature
boundaries while keeping manual announce and diagnostics availability under
the higher product composition. `experimental.radio_trace.page` is not behind
a Cargo feature: the complete appliance dispatcher routes it through the
diagnostics port, while a minimal dispatcher returns `UnsupportedOperation`.
`dispatch_with_lxmf` independently exposes LXMF reads and basic send without
requiring or compiling the raw-RNS qualification mailbox;
`dispatch_with_inbox_and_lxmf` accepts one owner
implementing both stores and is compiled only when both corresponding features
are enabled. `dispatch_with_nomad` composes independent submission and NomadNet
ports, while
`dispatch_with_inbox_lxmf_peer_discovery_and_nomad` adds that independent
NomadNet owner to the complete existing appliance surface.
`dispatch_with_inbox_lxmf_peer_discovery_nomad_and_network_config` retains
network configuration on that same flash-capable appliance owner while
NomadNet remains independent. Capability responses
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
| 19 | u8 | no | `experimental_network_config`: redacted configuration, mutation, and live-status availability; 0 unavailable, 1 disabled, 2 available |
| 20 | u8 | no | `manual_service_announce`: authenticated ordinary-announce availability; 0 unavailable, 1 disabled, 2 available |
| 21 | u8 | no | `experimental_reticulum_probe`: authenticated path-and-proof probe availability; 0 unavailable, 1 disabled, 2 available |

`CapabilitySnapshot::current()` is device-owned and cannot advertise packet
output or direct-radio TX. Key 2 says nothing about node-owned RNS traffic: an
accepted request advertised by key 3 may be routed over LoRa or any other
eligible Reticulum interface without granting a client direct radio control. A
higher dispatcher uses `CapabilitySnapshot::for_dispatch` to restrict that
codec-build snapshot; it can disable a capability but cannot enable one omitted
from the codec build. API 1.2 introduced keys 7 and 8, API 1.3 introduced keys 9
and 10, API 1.4 introduced keys 11 through 13, and API 1.5 introduced keys 14
and 15. API 1.6 introduced keys 16 through 18, and API 1.7 introduced key 19.
API 1.8 introduced key 20, and API 1.14 introduced key 21. All are optional on decode;
an older response therefore maps absent capabilities to unavailable with zero
limits. The E290 reports raw-inbox and committed-LXMF reads only after their
exact durable stores mount, and reports basic send only when durable submission
and the local `lxmf.delivery` source are available. Faults disable the affected
capability rather than inventing a volatile substitute. Peer discovery is
advertised only by a dispatcher with the bounded projection port. NomadNet
fetch is advertised only by a dispatcher with the independent bounded fetch
port; keys 17 and 18 are zero when that capability is unavailable and retain
the structural limits when runtime policy reports it disabled. Network
configuration is advertised only while the exact bound store is available;
corrupt, foreign, or unreconciled media disables key 19 without creating a
volatile fallback. Keys 20 and 21 are available only when the higher product
dispatcher has composed the manual-announce and probe ports respectively.
API 1.12 diagnostics and API 1.13
mailbox collection state do not add another capability-map field; their
operations remain known and return
`UnsupportedOperation` or `CapabilityUnavailable` when the selected product
composition cannot serve them.
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

The shared numeric vocabulary has intent-qualified lifecycle semantics. Raw
`experimental.submit_rns_data` is conservative and one-shot: its prepared
attempt advances through `AwaitingDelivery`, a receipt timeout becomes terminal
`Failed(DeliveryTimeout)`, and ambiguous `Preparing` or `AwaitingDelivery`
replay after reboot becomes an internal `InterruptedByReset` failure. An
accepted `experimental.lxmf.basic_send` instead stays durably `Preparing`
across its serialized volatile RNS attempts. An attempt timeout does not emit
state 4, and LXMF does not append state 2 for each attempt. A valid proof moves
the logical submission directly from `Preparing` to `Delivered`; reboot
restores `Preparing` for another board-owned attempt. Permanent policy,
semantic, cancellation, or invariant outcomes may still become terminal.

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
live index intentionally lags flash and `submission.status` returns
`RetryLater` until the owner reconciles it. If the actor is faulted, its
authority remains unavailable and status returns `Internal` until the owner is
remounted or recovered.

Node-core retains an in-RAM terminal-attempt tombstone until explicit
acknowledgement. For raw RNS and successful LXMF delivery, the portable
projector maps it to the corresponding final submission state and exposes the
exact acknowledgement only after a storage actor reports the required record
committed or readback-equivalent. An LXMF timeout instead retires only that
volatile attempt while the durable submission remains `Preparing`. Node-core
rejects acknowledgement while an external TX typestate still binds its
`TxPacketBuffer`, so the action remains retryable until ownership returns. The
proof or receipt timeout may become attempt-terminal before a dispatcher frame
observation. Delivery uses the preparation-bound digest and length. A raw-RNS
timeout remains a metadata-free `Failed` response without keys 2 or 3; an LXMF
timeout remains a metadata-free `Preparing` response. The encoded-byte digest
is retained where required for durable terminal evidence, but the v1 response
exposes it only for `AwaitingDelivery` and `Delivered`. Device API v1 does not expose the
internal attempt handle, packet-slot ID, dispatch generation or deadline, and
it does not expose volatile attempt correlation. `reticulum-storage-journal`
supplies complete integrity-validated physical replay, and
`reticulum-storage-actor` can expose a live index only after full mount and
semantic replay. Product reboot safety still requires the firmware task to
finish that mount plus intent-qualified boot recovery before enabling any API
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
Idempotency conflict and capacity map to their stable API categories. Actor
busy maps to `RetryLater`; identifier exhaustion, backend ambiguity, and a
latched fault remain `Internal`. The minimal authenticated USB bearer now exposes this operation
when a credential is active. The pre-authentication bootstrap cannot create an
authenticated owner. A powered E290 submission has completed durable acceptance,
LoRa delivery, peer decrypt/proof, terminal projection, and post-re-enumeration
status. Acceptance
is not a delivery guarantee; a later status can report no
path, delivery timeout, downstream rejection, or an internal failure. The ID
can be queried through `submission.status`. The response contains no
destination, payload, prepared packet, packet fragment, or packet-borrowing
handle.

This raw-RNS operation has no LXMF message identity or receiver-side durable
message-ID deduplication, so it does not inherit the LXMF retry relaxation. One
receipt timeout is terminal `Failed(DeliveryTimeout)`, and ambiguous in-flight
state at reboot is conservatively finalized as an internal reset interruption.

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
| 10 | map | no | receiver-local first-arrival ingress observation |

No commit after the cursor, an empty store, or an unknown/stale cursor returns
`NotFound`. The handle is logical store identity, not a flash address. It is
designed to remain stable across reboot and a future compactor.

API 1.14 ingress observations use key `0` for the device-local interface ID.
Signed whole-dBm RSSI at key `1` and whole-dB SNR at key `2` must either both
be present or both be absent. These values describe only the final hop into
the receiver and may therefore describe a relay rather than the original
message sender.
Summaries written by physical-format-1 firmware, imported before app schema 5,
or returned by a pre-1.14 peer can legitimately omit key `10`; clients must
preserve that as unknown instead of substituting a Nearby announce reading.

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

### `experimental.lxmf.mailbox_status` (`0xf010`)

This authenticated read returns the appliance-global durable collection
watermark for the committed LXMF store. Its request body is empty.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero u64 | no | latest committed LXMF handle; omitted for an empty store |
| 1 | nonzero u64 | no | highest handle durably collected by the client; omitted before the first acknowledgement |
| 2 | u32 | yes | exact contiguous committed-handle count after key 1 through key 0 |

Key 1 may not exceed key 0, and key 2 must equal their difference when omitted
handles are interpreted as zero. Contradictory snapshots are rejected during
decode. The E290 persists the acknowledgement in a two-sector, commit-last
store bound to the physical device and the acknowledged message's ID and exact
wire digest. At first migration it baselines at the existing LXMF tail. On
later boots, a missing or contradictory receipt identity proves that the LXMF
store was erased or recreated, so the firmware atomically rebaselines to that
store's current tail before opening ingress.

This state means collected into the controlling client's durable message store.
It does not mean a human read the message, and the current alpha has one global
watermark rather than one watermark per authenticated principal.

### `experimental.lxmf.mailbox_acknowledge` (`0xf011`)

This authenticated mutation advances the collection watermark only after the
client has durably imported every committed message through the named handle.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero u64 | yes | highest committed LXMF handle durably imported by the client |

The successful response uses the complete mailbox-status body documented for
`experimental.lxmf.mailbox_status`. Repeating the exact current handle and
identity is an idempotent success. A lower handle, a handle beyond the current
tail, or an unknown handle returns `InvalidRequest`; backend ambiguity fails
closed. The app batches the highest cursor only after each preceding message is
already present in its local durable store and retries the same request after
an ambiguous transport failure.

### `experimental.reticulum_probe.start` (`0xf012`)

This API 1.14 mutation begins a bounded, transport-neutral path-and-proof probe
to a known Reticulum destination. It requires an authenticated principal and
`EXPERIMENTAL_SUBMIT_RNS_DATA`.

Request body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | known remote Reticulum destination |
| 1 | bytes(16) | yes | principal-scoped idempotency key |

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | nonzero bytes(16) | yes | opaque boot-scoped probe ID |
| 1 | u8 | yes | `0` accepted, `1` replayed |

While the one-slot record remains retained, repeating the same principal,
destination, and idempotency key returns the original probe ID with replayed
outcome. Reusing the key for different semantic content is an idempotency
conflict. An active or as-yet-unpolled terminal record cannot be displaced by a
different start. After the device processes the owning principal's first
terminal poll, a later different start may reuse the slot; clients therefore
resume an accepted ID after ambiguous poll transport failures instead of
starting a second probe.

### `experimental.reticulum_probe.poll` (`0xf013`)

This authenticated read-only operation polls one principal-owned probe. Its
request body contains the nonzero 16-byte probe ID at key `0`. Missing IDs,
another principal's IDs, and IDs from a prior boot are intentionally
indistinguishable as `NotFound` at the product dispatch boundary.

The response body always contains state at key `0` and its state-specific value
at key `1`:

| State | Value |
| ---: | --- |
| 0 | pending phase: `0` path lookup, `1` awaiting dispatch, `2` awaiting proof |
| 1 | success map |
| 2 | terminal failure: `0` identity unavailable, `1` no path, `2` dispatch, `3` timeout, `4` internal |

The success map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u32 | yes | round-trip milliseconds |
| 1 | u8 | yes | Reticulum hop count |
| 2 | map | yes | receiver-local ingress observation for the returning proof |

The shared ingress map contains interface ID at key `0`. RSSI at key `1` and
SNR at key `2` are signed whole-unit values and must be paired; both are absent
for transports without physical signal information. This is final-hop evidence
at the probing receiver, not an end-to-end signal metric. A successful probe
validates Reticulum path-and-proof reachability to the remote
`rnstransport.probe` responder only. It does not prove that the remote node
offers LXMF, estimate application throughput, or report the RSSI at which the
remote node received the request. The returning proof may have been relayed,
so even its final-hop signal need not describe the remote device. Public or
third-party nodes may omit or disable the responder; that failure is not
evidence that all Reticulum or LXMF traffic to the node is impossible. Probe
traffic is volatile and bypasses durable submission and message stores. All
probe requests and responses remain far below the common 512-byte
logical-message and 448-byte body ceilings.

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
| 5 | map | no | API 1.17 Sideband-compatible message location |

When key `5` is present, all seven location-map fields are required:

| Key | Type | Meaning |
| ---: | --- | --- |
| 0 | i32 | latitude in decimal microdegrees, `-90_000_000..=90_000_000` |
| 1 | i32 | longitude in decimal microdegrees, `-180_000_000..=180_000_000` |
| 2 | i32 | altitude above mean sea level in centimetres; zero when unavailable |
| 3 | u32 | ground speed in centimetres per second; zero when unavailable |
| 4 | i32 | bearing in centidegrees; zero may mean unavailable or due north |
| 5 | u16 | horizontal accuracy radius in centimetres; zero when unavailable |
| 6 | u32 | source-fix update time in whole Unix seconds |

There is deliberately no source field and no caller-supplied raw LXMF fields
map. The device uses its registered inbound Single `lxmf.delivery` destination
and resident private identity to construct and sign the Python-compatible basic
message. Without key `5`, the basic subset has an empty fields map and no stamp.
With key `5`, the device emits one Sideband-compatible `FIELD_TELEMETRY` (`0x02`)
entry containing the time and location sensors. This is an LXMF application
field, not a Reticulum routing or transport header. Arbitrary fields,
attachments, stamps/tickets, Resources, and propagation remain outside this
operation. The codec accepts the
timestamp as an unsigned integer; the E290 product composer rejects zero and
values above `8_796_093_022_207_999`, the exact positive whole-millisecond
binary64 subset. It also rejects title/content combinations that exceed the
single-Link-packet direct boundary: 319 bytes of Python LXMF `content_size` or
431 bytes of complete signed wire. Empty title and empty content together are
valid and match the canonical Python vector. The 448-byte encoded-body limit
also applies to the combination even though keys 2 and 3 each have a 295-byte
structural field limit. The location fields map occupies at most 52 bytes,
instead of the one-byte empty map, so it can reduce the title/content budget by
up to 51 bytes before the same content-size and complete-wire ceilings apply.
Durable acceptance does not promise immediate delivery:
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
through `submission.status` until delivery or a permanent terminal failure.

After acceptance, the board owns the LXMF delivery obligation without a
connected app. The durable state remains `Preparing` across fresh serialized
RNS attempts. A receipt timeout retires and exactly acknowledges only the
volatile attempt, then schedules base backoff of 5 seconds, 15 seconds,
60 seconds, 5 minutes, and capped 15 minutes with deterministic additive jitter
no greater than 20 percent. One automatic retry may run globally, fresh
submissions are preferred, and an exact destination path transition from
unusable to usable wakes that destination early. Reboot restores `Preparing`
after a 15-second base delay plus the same bounded jitter. The signed LXMF wire,
message ID, optional location, and device submission ID stay fixed; each
carrier attempt receives fresh RNS ciphertext and a fresh attempt token.

Idempotency uses the authenticated principal plus key 4. An exact retry must
retain destination, timestamp, title, content, optional location, and key; it
returns the original submission and message IDs without adding a record.
Reusing the key with different semantic content returns
`IdempotencyConflict`. Once the app commits an outbound message, board-owned
automatic carrier attempts retain its original location and LXMF message
identity; they do not sample a new coordinate or rebuild the signed message. A
transitional explicit app retry for a legacy or permanently terminal outbox row
uses a fresh key and creates a replacement durable submission while preserving
that same signed LXMF material. The complete-wire
intent closes the former 384-through-391 opportunistic carrier gap without
raising the separate 383-byte generic-RNS DATA ceiling. Automatic delivery
selection currently reuses a compatible active product Link first, otherwise prefers an
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

### `experimental.network.config_get` (`0xf00a`)

This authenticated read returns the complete desired network configuration
without credential bytes. The request body is an empty map.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | monotonic committed configuration revision; zero is exactly erased/empty state |
| 1 | array(0..4) | yes | ordered redacted Wi-Fi profile maps |
| 2 | map or null | yes | configured literal-IPv4 outbound Reticulum TCP peer |
| 3 | bool | yes | global Wi-Fi station and Reticulum TCP enable |
| 4 | bool | yes | scheduled ordinary primary/LXMF/Nomad announces enabled |
| 5 | bool | yes | signed RMAP interface discovery enabled |
| 6 | bool | yes | a retained phone position may be included in RMAP discovery |
| 7 | map or null | yes | retained phone-sourced RMAP position |
| 8 | map or null | yes | configured DNS-hostname outbound Reticulum TCP peer |
| 9 | u8 | yes | requested LoRa radio output in dBm: exactly 14, 17, 20, or 22 |
| 10 | map | yes | complete LoRa profile saved for the next radio start |

Each Wi-Fi profile map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | opaque nonzero profile ID |
| 1 | bool | yes | enabled |
| 2 | bytes(1..32) | yes | exact SSID bytes |
| 3 | bool | yes | whether a passphrase is stored |
| 4 | u8 | yes | selection priority; larger values are preferred |

The optional TCP-peer map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bool | yes | enabled |
| 1 | bytes(4) | yes | network-order unicast IPv4 address |
| 2 | u16 | yes | nonzero TCP port |

The optional hostname-peer map has the same keys, except key 1 is a validated
1-to-96-byte ASCII DNS hostname encoded as text. Keys 2 and 8 are mutually
exclusive: at most one peer form may be non-null. The optional phone-position
map is `{0: latitude_e6 i32, 1: longitude_e6 i32}`, where the signed values are
decimal degrees multiplied by one million and are bounded to the world.

The LoRa profile map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u32 | yes | center frequency in whole hertz; nonzero |
| 1 | u32 | yes | canonical bandwidth in whole hertz |
| 2 | u8 | yes | spreading factor, 7 through 12 |
| 3 | u8 | yes | denominator of coding rate 4/n, 5 through 8 |
| 4 | u8 | yes | requested radio output in dBm: 14, 17, 20, or 22 |

The canonical bandwidth values are 7,810, 10,420, 15,630, 20,830, 31,250,
41,670, 62,500, 125,000, 250,000, and 500,000 Hz. These are portable numeric
limits, not product or regulatory authorization. The E290 narrows them by
requiring the complete occupied channel to fit its 863--928 MHz HT-RA62-HF path
and rejecting bandwidth/SF combinations not yet qualified against RNode's
low-data-rate optimization behavior.

Passphrases are deliberately absent. A profile can be edited with a `keep`
credential update without first reading its secret.

API 1.11 introduced key 9. API 1.16 and later encoders retain that
legacy power projection and add key 10, for eleven fields total. A decoder
requires key 9 to equal profile key 4 when both are present. Without key 10 it
uses the historical 915 MHz, 125 kHz, SF7, CR 4/5 profile and the key-9 power;
without either field it also assigns the historical +14 dBm default. Retaining
key 9 lets the previous decoder read current snapshots while skipping unknown
key 10. No decoder infers or rounds arbitrary power values.

### `experimental.network.config_mutate` (`0xf00b`)

This authenticated, permission-gated operation applies one bounded
compare-and-swap mutation. Its request body is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u8 | yes | mutation kind; see the closed table below |
| 1 | map or null | yes | kind-specific mutation value |
| 2 | u64 | yes | complete configuration revision the caller observed |
| 3 | bytes(16) | yes | principal-scoped idempotency key |

| Kind | Mutation value |
| ---: | --- |
| 0 | upsert Wi-Fi: `{0: profile_id bytes(16), 1: network map}` |
| 1 | remove Wi-Fi: `{0: profile_id bytes(16)}` |
| 2 | replace/clear literal-IPv4 TCP peer: IPv4 peer map or `null` |
| 3 | replace/clear DNS TCP peer: hostname peer map or `null` |
| 4 | gateway policy: `{0: wifi_transport_enabled bool, 1: automatic_announces_enabled bool}` |
| 5 | RMAP policy: `{0: discovery_enabled bool, 1: share_location bool, 2: phone-position map or null}` |
| 6 | requested LoRa radio output: one `u8` equal to 14, 17, 20, or 22 |
| 7 | atomically replace the complete five-field LoRa profile map defined above |

The Wi-Fi network map is
`{0: enabled bool, 1: SSID bytes, 2: credential map, 3: priority u8}`. The
credential map is `{0: 0}` to retain the existing passphrase or
`{0: 1, 1: passphrase bytes}` to replace it. `keep` is invalid for a new
profile. Replacing either TCP-peer form clears the other form.

Kind 6 remains compatible with power-only clients: current firmware preserves
the saved frequency, bandwidth, SF, and coding rate while changing power. Kind
7 is the normal current operation and validates the complete tuple before any
snapshot is committed; there is no sequence of partially applied field writes.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u8 | yes | outcome: 0 applied, 1 revision conflict |
| 1 | u64 | yes | committed revision when applied, current revision on conflict |
| 2 | bool | applied only | controlled reboot is required before actors apply this revision |

The E290 first slice commits and verifies a complete successor snapshot, then
reports `reboot_required = true`; it does not live-rebind interfaces. Exact
same-principal retries retain key 3. A stale or conflicting request returns the
typed revision-conflict outcome instead of silently overwriting another
client's change. Secret-bearing input is never returned or formatted in
diagnostics.

The complete LoRa profile follows the same reboot-to-apply rule. Firmware binds
the boot-selected tuple into the immutable radio-configuration fingerprint,
driver setup, advertised bitrate, and airtime-sensitive policy; a successful
mutation does not alter the active radio. `config_get` therefore describes the
saved profile, while `experimental.node.diagnostics` LoRa keys 0 through 4
describe the running profile. Frequency, bandwidth, SF, and coding rate must
match on LoRa peers that should exchange frames directly; power may differ.
Requested output is not measured conducted power, antenna EIRP, or a range
guarantee, and the operator remains responsible for regional spectrum,
duty-cycle, antenna, and EIRP requirements.

Durable network-configuration semantic formats 1 and 2 mount with the complete
historical +14 dBm profile. Format 3 retains its saved power with historical
915 MHz, 125 kHz, SF7, CR 4/5 modulation. Every material current mutation writes
semantic format 4 with the complete tuple. Firmware predating format 4
cannot mount that snapshot: deliberately erase the network-configuration store
before downgrading, because an ordinary merged-image flash preserves it.

### `experimental.network.status` (`0xf00c`)

This authenticated read has an empty request body and returns volatile,
secret-free actor state separately from desired configuration:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | latest committed desired revision |
| 1 | u64 | yes | revision currently applied by running network actors |
| 2 | u8 | yes | Wi-Fi state: 0 disabled, 1 disconnected, 2 connecting, 3 connected |
| 3 | bytes(16) or null | yes | selected Wi-Fi profile ID |
| 4 | bytes(1..32) or null | yes | associated SSID |
| 5 | bytes(4) or null | yes | DHCP-assigned IPv4 address |
| 6 | i16 or null | yes | whole-dBm RSSI |
| 7 | u8 | yes | TCP-peer state: 0 disabled, 1 waiting for network, 2 connecting, 3 connected, 4 faulted, 5 backoff |
| 8 | u8 | no | most recent retryable TCP failure; omitted when none is retained |
| 9 | map | no | bounded DNS-path diagnostics; omitted when no hostname lookup is retained |

`configured_revision != applied_revision` is the transport-neutral signal that
a reboot remains necessary. `waiting for network` means an enabled peer has
been applied but the Wi-Fi station has no usable address. `faulted` is reserved
for a local actor, ownership, or interface-fabric invariant failure. `backoff`
means a bounded retry delay is active after an ordinary DNS, connect, socket,
or transmit failure; `connecting` now means an actual attempt is active.

API 1.9 appends key 8 without changing keys 0 through 7 or their existing
codes. Encoders omit key 8 when no failure is retained, and decoders map an
API-1.8 body without key 8 to `None`. The closed diagnostic vocabulary is:

| Code | Failure |
| ---: | --- |
| 0 | `dns_timeout` |
| 1 | `dns_lookup_failed` |
| 2 | `dns_no_ipv4_result` |
| 3 | `connect_invalid_state` |
| 4 | `connect_reset` |
| 5 | `connect_timeout` |
| 6 | `connect_no_route` |
| 7 | `socket_closed` |
| 8 | `transmit_failed` |

The failure value contains no hostname, address, credential, packet data, or
implementation-specific error text. A later connection attempt may retain the
preceding failure for diagnosis. Losing the station network without a more
specific TCP failure may clear it.

API 1.10 appends optional key 9. API-1.9 bodies without it decode to `None`;
API-1.9 decoders skip it as one unknown bounded map. The DNS-diagnostics map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(4) or null | yes | DHCP default gateway |
| 1 | array(3) | yes | fixed DHCP-resolver slots, each bytes(4) or null |
| 2 | u8 | yes | built-in system-DNS outcome |
| 3 | u8 | yes | common raw-UDP DNS socket setup state |
| 4 | array(5) | yes | fixed raw-attempt slots, each map or null |
| 5 | map or null | yes | successful resolution |

The fixed capacities cover all three resolver addresses accepted from DHCP plus
two product-selected public resolvers. Optional slots need not be dense, which
lets the board replace a complete `Copy` snapshot as each bounded stage changes.
System-DNS outcomes are:

| Code | Outcome |
| ---: | --- |
| 0 | not started |
| 1 | resolving |
| 2 | resolved |
| 3 | DHCP supplied no resolvers |
| 4 | timeout |
| 5 | lookup/protocol failure |
| 6 | no usable IPv4 result |

Raw-socket setup states are `0` not started, `1` binding, `2` ready, `3` bind
failed, and `4` query encoding failed. Each non-null raw-attempt map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u8 | yes | source: 0 DHCP, 1 public fallback |
| 1 | bytes(4) | yes | exact resolver IPv4 address |
| 2 | u8 | yes | latest attempt outcome |
| 3 | u8 | outcome 10 only | nonzero DNS response code |

Raw-attempt outcomes are:

| Code | Outcome |
| ---: | --- |
| 0 | not started |
| 1 | skipped duplicate resolver |
| 2 | public resolver skipped for a local/private name |
| 3 | sending |
| 4 | awaiting response |
| 5 | resolved |
| 6 | send failed |
| 7 | timeout |
| 8 | packet was not a DNS response |
| 9 | truncated UDP response |
| 10 | nonzero DNS response code at key 3 |
| 11 | echoed question mismatch |
| 12 | malformed response |
| 13 | no usable IPv4 result |

Outcome 10 requires key 3 with a nonzero value. Every other outcome forbids key
3. The successful-resolution map is `{0: address bytes(4), 1: source u8,
2: resolver bytes(4) or null}`. Resolution source `0` is built-in system DNS,
`1` is raw DHCP DNS, and `2` is raw public DNS. The resolver can be null only
when the successful abstraction did not identify its exact upstream server.

These fields intentionally expose the DHCP gateway, DHCP resolver addresses,
public resolver addresses, and resolved peer address because they are necessary
to distinguish resolver configuration, routing, and response failures. They
remain secret-free: no Wi-Fi credential, configured hostname, DNS packet,
Reticulum packet, or implementation-specific error text crosses this boundary.

### `experimental.manual_service_announce` (`0xf00d`)

This authenticated mutating operation requests one ordinary primary, LXMF, and
NomadNet announce cycle. The request body is empty. The successful response is
`{0: disposition u8}`:

| Code | Disposition | Meaning |
| ---: | --- | --- |
| 0 | queued | a new announce cycle was queued |
| 1 | already pending | an equivalent cycle was already pending and the request coalesced |

Both dispositions are successful. The operation admits product-owned,
spacing-aware work; it does not synchronously transmit, bypass channel access,
or expose a raw-radio primitive.

### `experimental.node.diagnostics` (`0xf00e`)

This API-1.12 authenticated read takes an empty request body and returns one
bounded, copy-only node snapshot:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | milliseconds since this node incarnation started |
| 1 | array(4) | yes | fixed interface slots, each an interface map or `null` |
| 2 | map | no | LoRa diagnostics; omitted when no LoRa owner is present |
| 3 | map | yes | Reticulum counters |
| 4 | u32 | yes | volatile observed-peer record count |
| 5 | u32 | yes | retained route count, regardless of interface usability |
| 6 | u32 | yes | retained routes whose selected local interface is currently usable |

An interface map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u8 | yes | product-owned interface ID |
| 1 | u8 | yes | kind: 0 LoRa, 1 Reticulum TCP, 2 other |
| 2 | u8 | yes | state: 0 offline, 1 online, 2 faulted |
| 3 | u64 | yes | product-owned incarnation or reconfiguration generation |
| 4 | u16 | yes | maximum logical Reticulum packet bytes accepted |
| 5 | u32 | no | approximate raw bitrate, when meaningful and known |

The optional LoRa map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | i16 | yes | applied whole-dBm radio output setting |
| 1 | u32 | yes | carrier center frequency in hertz |
| 2 | u32 | yes | bandwidth in hertz |
| 3 | u8 | yes | spreading factor |
| 4 | u8 | yes | coding-rate denominator |
| 5 | u64 | yes | physical receive frames presented by the radio |
| 6 | u64 | yes | logical Reticulum packets reconstructed and accepted |
| 7 | u64 | yes | receive operations ending in radio or decode error |
| 8 | u64 | yes | frames or packets dropped after radio delivery |
| 9 | u64 | yes | transmission jobs reaching a terminal result |
| 10 | u64 | yes | terminal jobs completed successfully |
| 11 | u64 | yes | physical frames completed across successful or partially completed jobs |
| 12 | u64 | yes | jobs rejected by channel-access policy |
| 13 | u64 | yes | jobs ending in another radio or scheduler failure |
| 14 | u64 | yes | channel-activity detections reporting busy |
| 15 | u64 | yes | channel-activity detections reporting clear |
| 16 | map | no | last accepted logical-packet RX observation |
| 17 | map | no | last terminal logical TX-job observation |
| 18 | map | no | most recent terminal DATA observation retained when key 17 is ordinary |

The last-RX map is `{0: age_ms u64, 1: rssi_dbm i16, 2: snr_db i16}`. It
describes conservative signal metadata for the most recently accepted logical
LoRa packet: a single-frame packet reports that frame, while a split packet
reports the field-wise weaker RSSI and SNR across both frames. It is not
arbitrary recent RF energy and is not simply the latest physical frame
presented by the radio. A later invalid, incomplete, or over-MTU frame does not
replace it.

The API-1.15 last-TX map is
`{0: age_ms u64, 1: outcome u8, 2: family u8, 3?: interface_id u8,
4?: encoded_packet_len u16, 5?: encoded_packet_sha256 bytes(32)}`. Outcome 0
means every physical frame completed, 1 means channel-access policy rejected
the job, and 2 means another radio, setup, scheduler, or completion failure
ended the job. Family 0 is DATA and family 1 is ordinary. DATA requires keys 3
through 5 together and a nonzero packet length; ordinary forbids them. API-1.14
records containing only keys 0 and 1 remain valid and decode with unknown
family.

Keys 3 through 5 identify the prepared DATA packet even when channel access or
another pre-authorization step rejected it. They do not assert byte exposure,
RF transmission, or delivery. Their length and SHA-256 use the same complete
encoded-packet definition as the app's per-message packet evidence. Key 17 is
the latest terminal job of either family. If it is DATA, key 18 is omitted to
avoid duplicating the 32-byte digest; if key 17 is ordinary, key 18 can retain
the preceding DATA result and is structurally constrained to the DATA form.

The Reticulum-counter map contains ten required `u64` fields:

| Key | Counter |
| ---: | --- |
| 0 | packets received by the Reticulum owner |
| 1 | packets forwarded |
| 2 | duplicate drops |
| 3 | structurally or cryptographically invalid drops |
| 4 | valid announces received |
| 5 | paths learned or replaced |
| 6 | paths expired or removed |
| 7 | Links established |
| 8 | established Links closed normally |
| 9 | Link establishment attempts failed |

The observed-peer, retained-route, and usable-route values are diagnostics of
different local projections. None, by itself, is a count of peers currently
connected, presently radio-visible, or end-to-end reachable. In particular,
“usable” means that the route resolves against the node's currently eligible
local interface registry; it is not a delivery guarantee.

### `experimental.route_diagnostics.page` (`0xf00f`)

This API-1.12 authenticated read returns bounded details for retained routing
state. The request body is empty for the first page, or
`{0: after_destination bytes(16)}`. The cursor is exclusive: the response starts
at the first destination strictly greater in lexicographic byte order.

Successful response body:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | u64 | yes | route-page revision |
| 1 | u32 | yes | complete retained-route count at snapshot time |
| 2 | array(4) | yes | route maps followed by a `null` tail |
| 3 | bytes(16) | no | continuation cursor, equal to the last returned destination |

Populated route slots form a dense prefix and their destination hashes are
strictly increasing. A present key 3 is copied into request key 0 for the next
exclusive page. Each route map is:

| Key | Type | Required | Meaning |
| ---: | --- | --- | --- |
| 0 | bytes(16) | yes | complete destination hash |
| 1 | bytes(16) | no | selected public next-hop identity hash |
| 2 | u8 | yes | Reticulum hop count |
| 3 | u8 | no | product-owned retained interface ID |
| 4 | u8 | yes | current route-resolution category |
| 5 | u64 | no | saturating age since the route was learned |
| 6 | u64 | no | saturating age since this node locally used the route |
| 7 | u64 | no | remaining lifetime before expiry |

Route-resolution codes are:

| Code | Resolution | Meaning |
| ---: | --- | --- |
| 0 | exact ready | the exact retained route's local interface is usable |
| 1 | exact offline | the exact interface is offline or faulted |
| 2 | exact missing | exact next-hop or interface state is incomplete |
| 3 | broadcast ready | no exact route resolves, but a broadcast interface is usable |
| 4 | broadcast unavailable | neither an exact route nor a broadcast fallback is usable |

A retained route is local routing evidence, not a connected-peer list,
last-heard table, end-to-end reachability assertion, or delivery guarantee.
Wire key 6 is specifically the Rete route table's **local LRU-use age**. It
means this node consulted or used the route; it does not mean the peer was
heard at that time.

The revision is the saturating sum of the node's paths-learned and
paths-expired counters. It is a bounded pagination-consistency token, not a wall
clock, peer-liveness value, or universal mutation generation. A client
collecting a complete route list must require the same revision and total count
on every page and restart the read if either changes. Local LRU touches and the
natural passage of time can change age values without advancing this token.
The maximum node response is 413 body bytes/435 complete-message bytes; the
maximum four-route response is 337/359 bytes. Both remain inside the common
448-byte body and 512-byte message ceilings.

### `experimental.radio_trace.page` (`0xf014`)

This API-1.16 authenticated read exposes a bounded packet-correlated trace. It
is always known by the codec and is not Cargo-feature gated. The complete
appliance dispatcher invokes `NodeDiagnosticsPort::radio_trace_page`; a product
without that diagnostics port returns `UnsupportedOperation`.

The first request body is empty. A continuation request is
`{0: [boot_id u64, after_sequence u64]}`. The cursor is exclusive and both
array members are mandatory. Binding the sequence to the opaque boot ID makes
a pre-reboot cursor unambiguous instead of silently skipping events from the
new node incarnation.

The successful body is the compact fixed array:

```text
[boot_id, applied_profile, oldest_sequence, next_sequence, history_lost,
 [event_or_null, event_or_null, event_or_null], next_cursor_or_null]
```

Its elements are:

| Index | Type | Meaning |
| ---: | --- | --- |
| 0 | u64 | opaque node-incarnation boot ID |
| 1 | array(10) | immutable boot-applied LoRa profile |
| 2 | u64 | oldest event sequence still retained, equal to index 3 when empty |
| 3 | u64 | sequence that will be allocated to the next event |
| 4 | bool | requested history preceded the oldest retained event |
| 5 | array(3) | dense, strictly ascending event prefix followed by `null` slots |
| 6 | null or array(2) | `[boot_id, last_returned_sequence]` when another page remains |

The applied-profile array is
`[configuration_fingerprint bytes(16), frequency_hz u32, bandwidth_hz u32,
preamble_symbols u16, requested_power_dbm i16, spreading_factor u8,
coding_rate_denominator u8, explicit_header bool, crc bool, iq_inverted bool]`.
The requested power is the radio setting, not a measurement or antenna-path
claim. The 16-byte fingerprint allows traces to detect any board-owned
configuration mismatch without reconstructing the fingerprint.

Each event is `[sequence u64, observed_at_us u64, kind u8, value]`, where the
timestamp is monotonic microseconds since this boot. Event kinds and values are:

| Kind | Event | Value array |
| ---: | --- | --- |
| 0 | terminal DATA TX | `[interface, packet_len, packet_sha256, attempt_token_or_null, outcome, planned_frames, completed_frames, authorization_observed, [tx_done_0_or_null, tx_done_1_or_null]]` |
| 1 | accepted logical RX | `[interface, packet_len, packet_sha256, attempt_token_or_null, rssi_dbm, snr_db]` |
| 2 | route selected | `[interface, packet_len, packet_sha256, attempt_token, destination, next_hop_or_null, hops, resolution, submission_id]` |
| 3 | attempt terminal | `[attempt_token, outcome, proof_ingress_or_null]` |

`packet_len` is nonzero and `packet_sha256` covers every byte of the complete
encoded interface packet. The distinct 32-byte attempt token is Reticulum's
hop-invariant proof-correlation hash. Route-selection records require the token
and a nonzero durable submission ID, forming the authoritative bridge from the
app outbox attempt to later packet and proof events. Route resolution reuses the
codes documented for `experimental.route_diagnostics.page`.

DATA TX terminal outcome codes are:

| Code | Outcome |
| ---: | --- |
| 0 | every planned physical frame completed |
| 1 | initial channel access rejected |
| 2 | exact permit request denied |
| 3 | matching authorization arrived after its deadline |
| 4 | post-grant channel access rejected |
| 5 | airtime calculation or admission rejected |
| 6 | dispatch deadline conversion overflowed |
| 7 | radio inactive |
| 8 | router/dispatcher interface configuration mismatch |
| 9 | radio configuration changed before permit negotiation |
| 10 | radio configuration changed after permit negotiation |
| 11 | channel-activity detection fault |
| 12 | physical transmit fault |
| 13 | permit/control-plane recovery |
| 14 | authorized frame/byte-exposure invariant recovery |
| 15 | cancelled radio operation reconciled |

A DATA TX plans one or two frames. Completed-frame count cannot exceed the
plan, and its dense two-slot TxDone timestamp prefix contains exactly one entry
per completed frame. Preparation or authorization alone is not RF completion.
Logical RX signal is conservative whole-packet receiver-local evidence.

Attempt-terminal codes are `0` delivered, `1` delivery timeout, and `2`
definitely unsent. Optional proof ingress uses the shared ingress map: interface
ID is required, and RSSI/SNR are both present or both absent. It describes the
first accepted returning proof at this receiver, not signal at the remote node.

The fixed page maximum is three, but the producer returns fewer events when the
next combination would exceed the 448-byte body ceiling and continues with the
typed cursor. Model construction rejects an oversized combination. The exact
worst-width three-DATA body is 441 bytes; the exact worst-width
route/DATA/RX body is 447 bytes.

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
unavailable, 8 internal, 9 capacity exhausted, 10 idempotency conflict, and 11
retry later. `RetryLater` means another exact device-owned operation temporarily
owns a required resource; the authenticated session remains valid and the
client should retry the exact operation after a short bounded delay.
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
  no persisted permission bit;
- experimental network configuration and status reads require an authenticated
  principal but no persisted permission bit;
- experimental network mutation requires an authenticated principal with
  `MANAGE_NETWORK_CONFIG` (persisted permission bit 2);
- experimental manual service announce requires an authenticated principal but
  no persisted permission bit;
- experimental Reticulum probe start requires an authenticated principal and
  `EXPERIMENTAL_SUBMIT_RNS_DATA`, while poll requires only an authenticated
  principal;
- experimental node, route, and packet-correlated radio-trace diagnostics
  require an authenticated principal but no persisted permission bit.

As a temporary E290 alpha compatibility policy, a network-mutation dispatch
also treats one exact predecessor developer credential shape as carrying that
permission: the active exact-generation record must contain exactly persisted
bits 0 and 1, `UsbPhysicalPresence` origin, and authorization-policy version 1.
The overlay exists only for that operation and only in its ephemeral dispatch
context. It does not rewrite the record, generation, authority revision, or
permission provenance used by other operations. Subsets, supersets, other
origins, and other policy versions are not widened. ADR 0020 records why this
runtime rule precedes a durable generation-advancing permission migration.

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

The canonical API 1.16 `system.capabilities` request for request ID 42 uses
minor byte `10` and is:

```text
a4 00 a2 00 01 01 10 01 18 2a 02 01 03 a0
```

The canonical API 1.16 `identity.summary` request for request ID 42 is:

```text
a4 00 a2 00 01 01 10 01 18 2a 02 03 03 a0
```

The canonical API 1.16 `experimental.radio_trace.page` continuation request
for request ID 42, boot ID 99, and exclusive sequence 39 is:

```text
a4 00 a2 00 01 01 10 01 18 2a 02 19 f0 14 03 a1 00 82 18 63 18 27
```

The wire tests freeze these requests, both identity-response forms, all feature
compositions of the twenty-two-field API 1.16 capability response, older maps
with absent optional capability fields, typed permission/capacity/idempotency
error responses, raw submission request/acceptance, exact
`0xf002`/`0xf003` inbox vectors, exact `0xf004`/`0xf005` LXMF list/read vectors,
and source-free `0xf006` request/acceptance plus boot-scoped `0xf007` peer-page
vectors. API 1.6 adds exact `0xf008` start request and accepted/replayed
response vectors plus `0xf009` poll request and pending/ready/failed vectors.
API 1.7 adds exact `0xf00a` redacted configuration, `0xf00b`
upsert/remove/peer mutation and typed outcome, and `0xf00c` live-status vectors.
API 1.11 extends the `0xf00a` snapshot with key 9 and adds the `0xf00b` kind-6
power mutation; vectors cover all four accepted values, rejection of 21 and
other unsupported values, and API-1.10 snapshot defaulting to +14 dBm.
The API 1.16 suite freezes the earlier optional
LXMF-summary ingress maps and exact authenticated request and response vectors
for the coalescing `0xf00d` announce operation and read-only
`0xf00e`/`0xf00f` diagnostics operations plus `0xf010` mailbox status and
`0xf011` acknowledgement, plus `0xf012` probe start, `0xf013` poll, and
`0xf014` radio-trace pagination.
API 1.17 keeps those API 1.16 vectors unchanged and adds round-trip and negative
wire coverage for optional basic-send key `5`, including every required nested
location field and coordinate-bound validation. A client that requests message
location requires an observed device minor version of at least 17; it does not
silently ask an older device to drop the field.
API 1.18 advances current-envelope goldens to minor 18 and freezes `RetryLater`
as error code 11. Adapter coverage distinguishes retained-owner busy responses
from structural `CapacityExhausted`, unavailable capabilities, backend
ambiguity, and latched faults.
Diagnostics coverage includes maximum
node and four-route response maps, closed-enum rejection, fixed/dense route
slots, strict ordering, continuation-cursor invariants, and duplicate-cursor
rejection, plus DATA-only last-TX construction, complete packet-evidence
round trips, legacy last-TX decoding, and contradictory retained-slot rejection.
Radio-trace coverage adds the exact boot-bound request, all event-kind and
terminal-outcome codes, dense and ordered boot-window invariants, partial-cursor
rejection, the 441-byte maximum DATA page, the 447-byte mixed page, and
model-level rejection of a three-route combination that would exceed the body
ceiling.
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

cargo test --locked -p reticulum-device-api --features experimental-network-config
cargo clippy --locked -p reticulum-device-api --all-targets \
  --features experimental-network-config -- -D warnings

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

cargo test --locked -p reticulum-device-api-adapter \
  --features experimental-network-config
cargo clippy --locked -p reticulum-device-api-adapter --all-targets \
  --features experimental-network-config -- -D warnings

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
minimal bearer. That historical no-wireless image selected no-op logging,
leaving one shared
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
