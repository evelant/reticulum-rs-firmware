# Reticulum device API

`reticulum-device-api` is the allocation-free, bearer-neutral logical protocol
used by USB, BLE, and Wi-Fi sessions. It owns bounded CBOR models and common
authorization policy, but no framing, executor, storage, board, radio, or Rete
state.

The still-unreleased API 1.18 adds an explicit `RetryLater` error for transient
retained-resource ownership. It keeps structural capacity exhaustion,
unavailable profiles, and durable faults distinct while retaining API 1.17
optional Sideband-compatible location on basic LXMF send and the API 1.16 durable-host-importable,
boot-scoped radio trace pages, API 1.15 packet-correlated LoRa DATA terminal
diagnostics, and API 1.14 portable Reticulum probe operations
and optional first-arrival interface/signal evidence shared by LXMF summaries.
API 1.13 added authenticated durable LXMF mailbox collection status and
acknowledgement behind `experimental-lxmf`. API 1.12 added authenticated,
read-only node and route diagnostics as unconditional protocol operations. API
1.11 added bounded LoRa transmit-power
configuration to the redacted network configuration behind
`experimental-network-config`. API 1.10 added bounded,
secret-free DNS-path diagnostics to the typed outbound Reticulum TCP runtime
status introduced in API 1.9. API 1.12 retains API 1.8 authenticated manual
ordinary service announces plus gateway, RMAP, and DNS-peer configuration; API
1.6 NomadNet fetch; API 1.5 nearby-LXMF peer discovery; API 1.3 optional public
`lxmf.delivery` destination on `identity.summary`; and API 1.4 source-free
basic-send. The manual-announce wire operation is always known so dispatchers
can advertise it independently.
Existing identity responses remain valid: body key `0` is the required primary
destination and key `1` is the optional 16-byte LXMF delivery destination.

## Experimental LXMF collection state

API 1.13 adds `experimental.lxmf.mailbox_status` (`0xf010`) and
`experimental.lxmf.mailbox_acknowledge` (`0xf011`). Both require an
authenticated principal; acknowledgement is mutating but has no separate
persisted permission bit in this alpha. Status returns the optional latest and
acknowledged-through handles plus their exact bounded difference. The
acknowledgement operation accepts a committed nonzero handle and returns the
complete resulting status. Equal requests are idempotent; regression and
unknown handles are invalid.

This is a client-collection watermark, not human read state. The current
product policy has one appliance-global watermark rather than per-principal
state.

## Experimental node and route diagnostics

API 1.12 reserves two authenticated read-only operations. They are always
known by the codec and authorization layer; minimal dispatchers return
`UnsupportedOperation`, while the complete appliance dispatcher routes them
through `NodeDiagnosticsPort`.

| Operation | ID | Request body | Successful response body |
| --- | ---: | --- | --- |
| `experimental.node.diagnostics` | `0xf00e` | empty | bounded node snapshot |
| `experimental.route_diagnostics.page` | `0xf00f` | optional key `0`: exclusive 16-byte destination cursor | bounded route page |

The node snapshot contains uptime, exactly four optional interface slots, an
optional LoRa record, Reticulum counters, and observed-peer, retained-route,
and usable-route counts. Interface records expose a product-owned ID, kind
(`LoRa`, `TCP`, or `Other`), state (`Offline`, `Online`, or `Faulted`),
generation, logical MTU, and optional bitrate.

The LoRa record contains the applied power, frequency, bandwidth, spreading
factor, coding-rate denominator, bounded RX/TX/CAD counters, and optional
last-RX, aggregate last-TX, and retained last-DATA-TX observations. Last-RX signal values are conservative
whole-packet metadata for the most recently accepted logical packet: a
single-frame packet reports that frame, while a split packet reports the
field-wise weaker RSSI and SNR across both frames. They are not arbitrary
recent RF energy or simply the latest physical frame; a later invalid,
incomplete, or over-MTU frame does not replace them. Last-TX outcomes use the
stable `Completed`, `AccessRejected`, and `Failed` categories. API 1.15 labels
the aggregate last TX as DATA or ordinary and attaches the selected interface,
complete encoded packet length, and SHA-256 to DATA results, including failures
before RF authorization. A later ordinary packet may replace the aggregate
record but does not replace the retained DATA record. This evidence correlates
with message packet evidence; it is not by itself proof of RF exposure.

The Reticulum record contains received, forwarded, deduplication-drop,
invalid-drop, announce, path-learned/path-expired, and link
established/closed/failed counters. A route page carries a revision computed
from path-learned plus path-expired counters, the complete retained-route
count, exactly four optional dense entry slots, and an optional next cursor.
Entries are strictly ordered by destination bytes and expose optional next-hop
identity, hops, optional retained interface, current resolution, and optional
learned age, last-used age, and remaining lifetime. A present next cursor is
the last returned destination and is exclusive in the next request.

Retained routes are local routing evidence, not a connected-peer list,
last-heard table, reachability assertion, or delivery guarantee. The route
last-used value is specifically local Rete route-table LRU activity, not the
time that a peer was last heard. “Usable” means the retained target currently
resolves against an eligible local interface. The saturating
paths-learned-plus-paths-expired revision is a pagination-consistency token, not
a wall clock, peer-liveness value, or universal mutation generation; a
multi-page reader must restart when either revision or total count changes.

Both maximum node and maximum four-entry route responses fit the 512-byte
message and 448-byte body ceilings. Numeric enum representations and operation
IDs are frozen by API-v1 wire tests.

## Experimental packet-correlated radio trace

API 1.16 reserves authenticated read-only operation
`experimental.radio_trace.page` (`0xf014`). Its optional request key `0` is a
two-element cursor array `[boot_id, after_sequence]`; both values are required
together. Omitting key `0` starts at the oldest retained event. Binding the
exclusive sequence to its boot prevents a stale pre-reboot cursor from silently
skipping events produced by a new node incarnation.

The successful response is the compact fixed array
`[boot_id, applied_profile, oldest_sequence, next_sequence, history_lost,
events, next_cursor]`. `events` always has three nullable slots and populated
slots are dense and strictly ascending. `next_cursor` is null when the page is
complete or `[boot_id, last_returned_sequence]` when another page remains.
`history_lost` reports that the requested starting position preceded the
oldest event still present in the bounded boot ring. The applied profile is
`[fingerprint, frequency_hz, bandwidth_hz, preamble_symbols,
requested_power_dbm, spreading_factor, coding_rate_denominator,
explicit_header, crc, iq_inverted]`.

Every event is `[sequence, observed_at_us, kind, value]`, where time is
monotonic microseconds since the node incarnation began:

| Kind | Value array |
| ---: | --- |
| `0` DATA TX terminal | `[interface, packet_len, packet_sha256, attempt_token_or_null, outcome, planned_frames, completed_frames, authorization_observed, [tx_done_0_or_null, tx_done_1_or_null]]` |
| `1` logical RX | `[interface, packet_len, packet_sha256, attempt_token_or_null, rssi_dbm, snr_db]` |
| `2` route selected | `[interface, packet_len, packet_sha256, attempt_token, destination, next_hop_or_null, hops, resolution, submission_id]` |
| `3` attempt terminal | `[attempt_token, outcome, proof_ingress_or_null]` |

Packet SHA-256 covers every encoded interface-packet byte. The distinct
32-byte attempt token is Reticulum's hop-invariant proof-correlation hash.
Route selection carries the exact nonzero durable submission ID, forming the
authoritative bridge from an app outbox attempt to later TX, RX, and proof
events. DATA TX retains both physical-frame TxDone timestamps and does not
equate preparation or authorization with RF completion. Attempt outcomes are
`Delivered`, `DeliveryTimeout`, and `Unsent`; optional proof ingress uses the
shared interface plus all-or-neither RSSI/SNR model.

The fixed page maximum is three, but the model rejects a worst-width event
combination that would exceed the frozen 448-byte body ceiling; a producer then
returns fewer events and continues with the typed cursor. The verified
worst-width three-DATA page uses 441 body bytes, and a route/DATA/RX page at
worst widths uses 447.

## Experimental network configuration

API 1.8 retains three bearer-neutral network operations:

| Operation | ID | Request body | Successful response body |
| --- | ---: | --- | --- |
| `experimental.network.config_get` | `0xf00a` | empty | desired redacted configuration |
| `experimental.network.config_mutate` | `0xf00b` | one mutation, expected revision, and idempotency key | applied revision/reboot flag or revision conflict |
| `experimental.network.status` | `0xf00c` | empty | live station and TCP-peer state |

Reads require an authenticated principal. Mutations additionally require
`Permissions::MANAGE_NETWORK_CONFIG` (persisted bit 2). The API owns four
ordered Wi-Fi records with opaque nonzero 16-byte identities, one unambiguous
outbound Reticulum TCP endpoint, gateway-wide enable/announce policy, and
opt-in RMAP policy, plus a requested LoRa transmit power. Changing one bounded
concern per request keeps the maximum secret-bearing message within the
512-byte envelope and makes bounded CAS storage commits straightforward.

Mutation body key `0` is one of these frozen API-v1 discriminators:

| Kind | Mutation body key `1` |
| ---: | --- |
| `0` | upsert one Wi-Fi profile |
| `1` | remove one Wi-Fi profile |
| `2` | replace/clear the legacy IPv4 TCP peer |
| `3` | replace/clear the DNS-hostname TCP peer |
| `4` | replace Wi-Fi transport and automatic-announce policy |
| `5` | replace RMAP discovery, sharing, and optional position policy |
| `6` | replace requested LoRa transmit power |

Every mutation is compare-and-swap against `expected_revision`; revision zero
names exactly erased, empty configuration. An applied response returns the new
revision and whether a controlled reboot is required. A normal CAS race
returns `RevisionConflict` with the current revision so the client can refresh.
The idempotency key helps resolve an ambiguous reply and can identify an exact
already-applied mutation, but is not durable replay authority by itself.

Physical configuration v1 supports WPA2-Personal only. SSIDs are opaque
nonempty byte strings of at most 32 bytes. WPA2 passphrases are 8 through 63
printable ASCII bytes. Credential updates keep (`0`) or replace (`1`) the
secret; Debug output redacts replacements.

Configuration reads return this top-level map:

| Key | Value |
| ---: | --- |
| `0` | committed revision |
| `1` | at most four ordered redacted Wi-Fi profiles |
| `2` | legacy IPv4 TCP peer or `null` |
| `3` | global Wi-Fi transport enabled flag |
| `4` | scheduled ordinary service announces enabled flag |
| `5` | signed RMAP discovery enabled flag |
| `6` | RMAP location sharing enabled flag |
| `7` | phone-sourced fixed-point location or `null` |
| `8` | DNS-hostname TCP peer or `null` |
| `9` | requested LoRa transmit power in dBm |

Each profile contains its opaque identity, enabled state, station-selection
priority, SSID, and only a `credential_configured` boolean. Larger priority
values are preferred. Password bytes are never present in a response. The
IPv4 peer contains enabled state, exact four-byte unicast address, and nonzero
port. The hostname peer uses the same enabled/port fields and a nonempty ASCII
DNS hostname of at most 96 bytes; labels are at most 63 bytes and may contain
letters, digits, and interior hyphens. The two endpoint slots are mutually
exclusive. Port 4242 is exposed as the conventional default.

Location is an optional map containing signed latitude and longitude in
decimal degrees multiplied by one million. Latitude is bounded to
`-90_000_000..=90_000_000`; longitude is bounded to
`-180_000_000..=180_000_000`. RMAP discovery and location sharing are separate
explicit flags so a stored position is never publication authority by itself.

LoRa transmit power is restricted to the explicit values 14, 17, 20, and
22 dBm. This is a requested radio setting, not a claim about calibrated
conducted output or regulatory EIRP.

API-1.7 snapshots remain decodable. Missing API-1.8 keys default to Wi-Fi
transport enabled, automatic announces enabled, RMAP discovery disabled,
location sharing disabled, no location, and no hostname peer. The legacy
`NetworkConfigSnapshot::new` constructor applies exactly those defaults;
`new_full` validates the complete API-1.8 state and also defaults LoRa transmit
power to 14 dBm. API-1.10 and older snapshots that omit key `9` likewise decode
with 14 dBm. `new_complete` accepts the complete API-1.11 state, including an
explicit validated power. Revision zero is still erased state and therefore
accepts only those defaults and no saved records.

Status reports configured and applied revisions, Wi-Fi state, optional active
profile identity, connected SSID, optional four-byte IPv4 address, optional
whole-dBm RSSI, TCP-peer state at key `7`, and optional last TCP failure at key
`8`. API 1.10 optionally adds bounded DNS diagnostics at key `9`. Different
configured and applied revisions let a client show that a controlled reboot is
still needed. API-1.8 status bodies omit key `8`; API-1.9 bodies omit key `9`;
both decode with absent newer diagnostics. Encoders omit either optional key
when its value is absent.

TCP-peer states preserve their existing wire codes and append backoff:

| Code | State |
| ---: | --- |
| `0` | `disabled` |
| `1` | `waiting_for_network` |
| `2` | `connecting` |
| `3` | `connected` |
| `4` | `faulted` |
| `5` | `backoff` |

When key `8` is present, it contains exactly one typed retryable failure:

| Code | Failure |
| ---: | --- |
| `0` | `dns_timeout` |
| `1` | `dns_lookup_failed` |
| `2` | `dns_no_ipv4_result` |
| `3` | `connect_invalid_state` |
| `4` | `connect_reset` |
| `5` | `connect_timeout` |
| `6` | `connect_no_route` |
| `7` | `socket_closed` |
| `8` | `transmit_failed` |

Unknown state/failure codes, duplicate keys, and non-integer failure values are
rejected. The API 1.9 failure category contains no hostname, address,
credential, or implementation-specific error value.

When status key `9` is present, it contains a fixed, allocation-free DNS
snapshot:

| Key | Value |
| ---: | --- |
| `0` | DHCP gateway IPv4 bytes or `null` |
| `1` | exactly three DHCP resolver slots, each IPv4 bytes or `null` |
| `2` | built-in system-resolver outcome |
| `3` | common raw-UDP socket setup state |
| `4` | exactly five raw-attempt slots, each an attempt map or `null` |
| `5` | successful resolution map or `null` |

The five raw slots can retain all three DHCP resolvers followed by two public
fallback resolvers. Each attempt map contains source (`0` DHCP, `1` public),
the exact four-byte resolver address, and a typed outcome. Outcome `10` carries
an additional nonzero DNS response code; every other outcome forbids that
field. A successful resolution records the selected IPv4 address, whether it
came from system DNS, raw DHCP DNS, or raw public DNS, and the exact resolver
when known. This deliberately exposes network-path addresses needed for local
diagnosis, but never a Wi-Fi credential, hostname, packet payload, or raw
implementation error string.

Capability key `19` reports runtime
availability (`0` unavailable, `1` disabled, `2` available); older responses
that omit it decode as unavailable.

## Manual ordinary service announce

API 1.8 reserves operation `experimental.manual_service_announce` at `0xf00d`.
The request body is an empty map. It requires an authenticated principal but no
persisted permission bit. It is mutating because it schedules network output,
but duplicate requests are explicitly coalesced.

The successful response body is `{0: disposition}`. Disposition `0` means a
fresh set of ordinary primary, LXMF, and NomadNet service announces was queued;
disposition `1` means an equivalent set was already pending. Unknown values,
missing key `0`, and duplicate key `0` are rejected. This operation does not
publish RMAP interface-discovery data; RMAP remains separately opt-in.

Capability key `20` reports manual-service-announce availability using the
standard unavailable/disabled/available vocabulary. Older capability responses
that omit key `20` decode as unavailable. `CapabilitySnapshot::for_dispatch`
defaults it to unavailable; a dispatcher that owns the queue explicitly enables
it with `with_dispatch_manual_service_announce`.

## Experimental Reticulum probe

API 1.14 reserves unconditional bearer-neutral operations
`experimental.reticulum_probe.start` (`0xf012`) and
`experimental.reticulum_probe.poll` (`0xf013`). Start is mutating and requires
an authenticated principal with `EXPERIMENTAL_SUBMIT_RNS_DATA`; poll is an
authenticated read with no additional permission bit.

Start request body key `0` is the peer's known 16-byte Reticulum destination
and key `1` is a 16-byte principal-scoped idempotency key. Its response contains
a nonzero opaque 16-byte boot-scoped probe ID at key `0` and outcome `0`
accepted or `1` replayed at key `1`. Poll request key `0` names that probe ID.
Missing, stale-boot, and foreign-principal IDs are product-dispatch concerns
and share the normal `NotFound` result.

Poll responses use key `0` state and key `1` state-specific value. State `0`
is pending with phase `0` path lookup, `1` awaiting dispatch, or `2` awaiting
proof. State `2` is failed with category `0` identity unavailable, `1` no path,
`2` dispatch, `3` timeout, or `4` internal. State `1` is success with this map:

| Key | Value |
| ---: | --- |
| `0` | `u32` round-trip milliseconds |
| `1` | `u8` Reticulum hop count |
| `2` | returning-proof ingress observation |

An ingress observation always contains interface ID at key `0`. RSSI at key
`1` and SNR at key `2` are either both present as signed whole-unit values or
both absent for a transport without physical signal data. The observation is
receiver-local final-hop evidence and may describe a relay.

A successful probe establishes only that Reticulum found a path to an enabled
`rnstransport.probe` responder and returned a valid proof. It does not establish
LXMF availability or throughput, and the returned signal is not the remote
request RSSI. Public nodes may omit or disable the responder.

Capability key `21` reports probe runtime availability with the standard
unavailable/disabled/available vocabulary. It is optional on decode and
defaults unavailable. `CapabilitySnapshot::for_dispatch` leaves it unavailable;
a dispatcher with a probe owner selects it with
`with_dispatch_reticulum_probe`.

## Experimental LXMF operations

The operation numbers are reserved after the existing API-v1 experimental
range:

| Operation | ID | Request body | Successful response body |
| --- | ---: | --- | --- |
| `experimental.lxmf.next` | `0xf004` | optional key `0`: exclusive non-zero handle | committed-message summary |
| `experimental.lxmf.read` | `0xf005` | key `0`: handle, key `1`: offset, key `2`: maximum bytes | exact normalized-wire chunk |
| `experimental.lxmf.basic_send` | `0xf006` | destination, timestamp, title, content, idempotency key, and optional API-1.17 message location | submission ID and LXMF message ID |
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
| 10 | optional first-arrival ingress observation using the API 1.14 interface/RSSI/SNR shape |

As with probe success, an LXMF ingress observation requires interface key `0`
and permits RSSI key `1` only when SNR key `2` is also present. The optional
evidence describes the receiver-local final hop, not an end-to-end path.
Older records and pre-1.14 summaries legitimately omit key `10`; clients must
show unknown evidence rather than borrowing signal from a different announce.

`read` returns map key `0` handle, key `1` offset, key `2` complete normalized
wire length, and key `3` exact bytes. A successful chunk is non-empty, lies
inside the declared message boundary, and contains at most 416 bytes. Clients
continue at `offset + bytes.len()` until `LxmfReadChunk::is_final()` is true.
The handle, complete length, and digest let clients detect replacement or
partial reads without coupling the protocol to the underlying store.

Capability body keys `9` and `10` report LXMF availability (`0` unavailable,
`1` disabled, `2` available) and the maximum chunk length. Older responses
that omit them decode as unavailable with a zero limit.

## Experimental NomadNet fetch

API 1.6 reserves two bearer-neutral operations behind `experimental-nomad`:

| Operation | ID | Request body | Successful response body |
| --- | ---: | --- | --- |
| `experimental.nomad.fetch_start` | `0xf008` | destination, absolute path, timestamp, and idempotency key | boot-scoped fetch ID and accepted/replayed outcome |
| `experimental.nomad.fetch_poll` | `0xf009` | fetch ID | pending phase, complete UTF-8 page, or terminal failure |

Both operations require an authenticated principal and consume no persisted
permission bit. Start is a mutation with principal-scoped idempotency; poll is
read-only and hides missing, foreign-principal, and stale-boot IDs behind the
same `NotFound` response.

A start request uses a 16-byte remote `nomadnetwork.node` destination, an
absolute nonempty UTF-8 path of at most 128 bytes with no NUL, a whole-
millisecond timestamp in `1..=9_007_199_254_740_991`, and a 16-byte
idempotency key. Its 16-byte fetch ID contains an eight-byte boot incarnation
followed by a nonzero big-endian sequence. Start outcomes are closed: `0`
accepted and `1` replayed.

Poll state is also closed: `0` pending, `1` ready, and `2` failed. Pending
phases are path lookup (`0`), Link establishment (`1`), request preparation
(`2`), awaiting dispatch confirmation (`3`), and awaiting response (`4`).
Failures are no path (`0`), Link (`1`), request (`2`), timeout (`3`), page too
large (`4`), invalid UTF-8 (`5`), and internal (`6`). A ready response owns one
complete valid UTF-8 Micron page of at most 400 bytes; it is never a truncated
page.

Capability key `16` reports NomadNet fetch availability (`0` unavailable, `1`
disabled, `2` available), key `17` reports the 128-byte maximum path, and key
`18` reports the 400-byte maximum page. Older responses omit these keys and
decode as unavailable with zero limits.

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
| 5 | optional API-1.17 Sideband-compatible location map |

The optional key-`5` map requires all of these values:

| Key | Value |
| ---: | --- |
| 0 | signed latitude in decimal microdegrees, within world bounds |
| 1 | signed longitude in decimal microdegrees, within world bounds |
| 2 | signed altitude in centimetres; zero when unavailable |
| 3 | unsigned ground speed in centimetres per second; zero when unavailable |
| 4 | signed bearing in centidegrees; zero may mean unavailable or due north |
| 5 | unsigned 16-bit horizontal accuracy radius in centimetres; zero when unavailable |
| 6 | unsigned source-fix update time in whole Unix seconds |

The request never accepts a source hash or an arbitrary raw fields map. The
product composer derives the source from the authenticated device identity,
constructs the complete Python-compatible signed LXMF wire, validates the timestamp and
single-Link-packet direct limit, and durably accepts those exact bytes. Empty
title and empty content are a valid Python-compatible basic message and are
covered by the independent Python vector corpus. A successful response map
contains the durable submission ID at key `0` and the 32-byte
Python-compatible LXMF message ID at key `1`.

If key `5` is absent, the composed payload retains the one-byte empty fields
map. If present, the board encodes only Sideband's LXMF `FIELD_TELEMETRY`
(`0x02`) with time and location sensors. The location is therefore part of the
signed LXMF payload and message identity; it is not a Reticulum route, interface,
or packet-header field. `BasicLxmfSend::with_location` callers require a
negotiated API minor of at least 17 so an older device cannot silently discard
the requested location. Arbitrary fields, attachments, stamps/tickets,
Resources, and propagated delivery remain separate future operations.

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
the 319-byte direct-content limit, and 431-byte durable wire capacity. The
current location fields map uses at most 52 bytes rather than the one-byte empty
map, reducing the available title/content budget by as much as 51 bytes under
those same limits. Unknown fields are
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
