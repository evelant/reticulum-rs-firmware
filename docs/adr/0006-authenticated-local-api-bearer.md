# ADR 0006: Authenticated local device-API bearer

- **Status:** accepted for the qualification session, portable credential
  authority/store, durable provenance, ADR 0009 policy/store design, ADR 0010
  live-pairing protocol/core, E290 credential boot composition, and routed
  pre-authentication live lifecycle; authenticated node-side handoff, logical
  dispatch, and minimal single-flight USB session/bearer composed; powered
  authentication pending
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0003](0003-lora-first-interface-fabric.md) and
  [ADR 0005](0005-active-data-durability-fail-stop.md)

## Context

LoRa is the first complete product vertical slice. The permanent E290 image
already owns the real SX1262 interface, Reticulum packet lifecycle, routing,
durable submission state and target-safe logical device-API dispatcher. It
still lacks a local client edge that can originate a controlled DATA operation
in the powered image. That edge is needed to exercise the same-image two-board
LoRa DATA/proof path and later to support a CLI, SPA or mobile client.

USB, BLE and Wi-Fi may all carry the local device API. That does not make any
of them a Reticulum packet interface. Building USB RNS, Wi-Fi RNS or BLE RNS
actors now would delay LoRa qualification and would prematurely choose link
semantics unrelated to the local API.

The E290 schematic connects the ESP32-S3 native USB D-/D+ signals on GPIO
19/20. The hardwired USB Serial/JTAG peripheral and the programmable USB OTG
peripheral share that physical path and are alternative owners, not concurrent
backends. The current automatic `esp-println` logger can select USB
Serial/JTAG and write the same FIFO an API bearer would own, so it cannot remain
on that stream once framed API traffic is enabled.

Opening a CDC device is not authentication. A local process must not be able to
put principal IDs or permission bits into a request and thereby mint its own
authority. Likewise, dropping a USB connection must not cancel an operation
already accepted into durable node ownership.

## Decision

### Sequence the local bearer behind the LoRa outcome

The first concrete bearer is USB Serial/JTAG CDC because it is fixed in ROM,
preserves the ordinary flashing/recovery path and is sufficient for the LoRa
qualification client. It is a local control/client API only. The first powered
milestone is:

1. authenticate one USB client;
2. admit one bounded logical API request;
3. durably accept and route it through the permanent node graph;
4. transmit it over the E290 LoRa interface; and
5. return or later retrieve the durable result.

No USB Reticulum packet actor, Wi-Fi packet actor or BLE packet actor is part
of that milestone. Programmable OTG with custom descriptors, composite
endpoints, WebUSB or USB networking remains a later backend behind the same
byte-I/O/session boundary.

When the Serial/JTAG API owner is composed, product logs move to UART0 on GPIO
43/44 or to a disabled/bounded diagnostic sink. Raw log bytes may never share
the framed API FIFO. Serial/JTAG and OTG remain mutually exclusive build/runtime
profiles.

### Use a canonical bounded record independent of the physical bearer

`reticulum-device-api-framing` owns only allocation-free byte framing. One wire
record is a leading zero, a COBS-encoded decoded body and a trailing zero.
Adjacent records may share the delimiter. The decoded body is canonical:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | magic `RDA1` |
| 4 | 1 | framing version `1` |
| 5 | 1 | session-owned record kind |
| 6 | 2 | reserved, exactly zero |
| 8 | 16 | handshake-derived session ID; zero before one exists |
| 24 | 8 | direction-local little-endian sequence |
| 32 | 2 | little-endian payload length |
| 34 | 0..512 | opaque session or logical-API payload |
| final | 16 | 128-bit authentication tag slot |

The maximum decoded record is 562 bytes and the conservative complete wire
owner is 567 bytes. The framing layer does not decide record kinds, calculate
or verify authentication, accept sequences, construct principals, or dispatch
logical operations.

Before a session exists, garbage, overlong input and malformed COBS records are
discarded through the next zero delimiter. Once authenticated, any malformed
record, wrong session ID, unexpected sequence or bad tag terminates that
session; the session layer never resynchronizes from an unauthenticated
sequence value. A new session uses fresh nonces, keys, ID and sequences.

Transmit ownership uses an explicit offset and bounded chunk view. The bearer
advances the offset only after its byte-I/O backend reports a completed partial
write. A backend write future with uncertain cancellation behavior must be
driven to completion; a generic `write_all` future is not treated as
cancellation-safe merely because the record owner is retained.

### Keep connection lifetime separate from accepted work

`reticulum-device-api-handoff` provides one independent depth-one request
channel and one depth-one reply channel. A boot-lifetime bearer manager retains
the sole bearer role across all USB reconnects and authenticated session
epochs. An ordinary disconnect does not drop this role.

That manager constructs exactly one `SessionEpochAllocator` after boot and
retains it while any node-side request or reply can exist. Every server
handshake flight consumes the next epoch, including a flight that never
authenticates, so reconnecting cannot reuse the `(epoch, correlation)` key of
delayed work. Correlations restart at zero only under a newly allocated epoch.
The allocator uses the final `u64` epoch once and then fails terminally; it
never wraps. Reconstructing it during the same boot is a service-contract
violation. Reboot clears the volatile handoff before a new allocator starts.

The request owner carries:

- a local session epoch and request correlation used only for reply routing;
- an opaque authenticated grant containing credential ID/generation and
  session routing facts, but no principal, permissions or PSK; and
- the exact bounded logical API message.

The node side receives the unique owner. Cancellation while waiting for
capacity retains the owner at the caller; cancellation while waiting to
receive leaves it in the channel. Once enqueued, connection loss cannot revoke
the node operation. The persistent bearer manager drains replies, discards a
reply from a stale epoch and allows an idempotent retry to become a new request
owner. Dropping either boot-lifetime endpoint is a fatal service teardown.

The permanent E290 graph now instantiates that depth-one handoff statically.
The node endpoint is scheduled as its own fair lane, and the USB task owns the
first deliberately minimal bearer manager across reconnects. The manager admits
one active session and one authenticated request at a time. A canonical
ClientHello replaces an idle established session with a fresh epoch on the same
connection; replacement never displaces request/reply owners. Any session fault
is terminal until USB reset or re-enumeration. This initial source profile
intentionally omits resumption, protocol retries, close records, encryption,
rate limiting/attempt policy, and concurrent requests. Its admission/handoff
and node-dispatch boundary is bearer-neutral. The present integrity-only
suite 1 remains explicitly USB Serial/JTAG-only and byte-for-byte compatible
with its committed vectors. A separate integrity-only suite 2 is now bound
exclusively to the Wi-Fi local-API profile and reuses the same ownership
boundary under a distinct transcript. Portable client/server and partial-stream
tests cover that binding, but the E290 SoftAP/TCP endpoint is not yet powered
qualified. BLE still requires its own binding/suite and connection mechanics.
Before two bearers run
simultaneously, the product must also choose either globally unique,
bearer-qualified connection/session epochs or strictly disjoint per-bearer
reply channels governed by one global pairing-exclusivity coordinator. A second
bearer must not reuse an independent epoch allocator against the current shared
routing namespace. Source and portable tests cover this composition; the
bounded powered USB handshake, sequential request/reply, and fresh post-re-
enumeration session paths pass. Broader lifecycle, rate, and wireless-bearer
qualification remains open.

The handoff's 512-byte limit is the authoritative
`reticulum-device-api::MAX_MESSAGE_BYTES`, not a duplicated constant.

### Authenticate from device-owned credential records

The initial credential design is one random 256-bit PSK per paired client. A
credential record contains at least:

- opaque credential ID;
- principal ID;
- PSK;
- permission set;
- non-repeating generation;
- active/revoked status; and
- bounded audit metadata.

The device derives `DispatchContext` only from that authenticated record.
Principal and permission bytes arriving from a client are never trusted.
Reticulum identity keys are not reused for local API authentication.

[ADR 0007](0007-device-api-credential-authority.md) now implements the first
portable authority slice below the session layer. It owns a fixed 16-record,
allocation-free immutable snapshot with globally unique nonzero generations,
stable permission decoding, bounded audit/policy metadata, secret-bearing
`Pending`/`Active` records and PSK-free `Revoked` tombstones. Complete snapshot
validation precedes service. The fixed table uses constant-time ID comparison;
only an active exact ID yields a zeroizing `SelectedCredential` consumed by the
session handshake. Missing, pending and revoked IDs share one outward failure.

This semantic authority is not itself persistence. The separate portable
credential store now implements ADR 0009's physical commit/retire contract and
power-loss recovery. E290 firmware now mounts and performs bounded boot recovery
immediately after flash open, then retains any `MountedCredentialStore` in the
sole coordinator. That coordinator now owns explicit empty-media
initialization and the ADR 0010 Begin/ProofStart/Activate/AbortCurrent lifecycle,
including correlated durable Add/Activate/Abort mutation and reconciliation.
Replacing the immutable authority outside that bounded lifecycle remains a
future sole-firmware-owner operation performed only after the store has
committed and validated a complete durable snapshot.

Physical presence authorizes pairing but does not prove which local host
process received a secret. ADR 0009 fixes the first lab profile: a continuous
roughly two-second GPIO21 hold opens one exclusive 60-second USB Serial/JTAG
window with at most three begin/proof attempts and one pending enrollment.
That profile explicitly trusts the connected USB host. Before shipping a
turnkey client, pairing adds an independently confirmed display/code/QR
ceremony or an equivalent reviewed out-of-band binding. The qualification
shortcut is not advertised as protection against a malicious process already
controlling the connected host.

`reticulum-device-api-session` freezes the qualification protocol implemented
by the allocation-free Rust server, its public allocation-free `no_std` client
typestates, and the independent Python vector generator. The client owns exact
partial-TX hello, proof, and request flights, authenticates the next response,
and restores its idle session only after successful verification; no E290 host
utility or USB task drives those typestates yet. Record kinds are `0x01` client
hello, `0x02` server hello, `0x03`
server proof, `0x04` client proof, `0x10` request, `0x11` response and `0x12`
reserved authenticated close. No other kind is accepted.

The 56-byte `ClientHello` payload is:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 2 | protocol major `1`, little-endian |
| 2 | 2 | protocol minor `0`, little-endian |
| 4 | 2 | suite `1`, little-endian |
| 6 | 1 | bearer binding (`1` = USB Serial/JTAG) |
| 7 | 1 | reserved zero |
| 8 | 16 | opaque credential ID |
| 24 | 32 | fresh client nonce |

The 76-byte `ServerHello` payload is:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 2 | selected major `1`, little-endian |
| 2 | 2 | selected minor `0`, little-endian |
| 4 | 2 | selected suite `1`, little-endian |
| 6 | 1 | actual bearer binding |
| 7 | 1 | reserved zero |
| 8 | 16 | stable public device-API ID |
| 24 | 32 | fresh device nonce |
| 56 | 8 | credential generation, little-endian |
| 64 | 2 | maximum record payload `512`, little-endian |
| 66 | 2 | maximum logical message `512`, little-endian |
| 68 | 1 | maximum in-flight requests `1` |
| 69 | 3 | reserved zero |
| 72 | 4 | flags `0x00000007`: qualification-only, integrity-only, device API |

Both hello records use a zero session ID, sequence zero and zero framing tag.
The transcript hash is SHA-256 over the exact domain
`reticulum-rs-firmware/device-api/session/transcript/v1\0`, then the client
record kind, little-endian `u16` payload length and complete client payload,
then the server record kind, length and complete server payload. This binds
every negotiable value and the message roles without binding COBS transport
bytes.

The HKDF extraction salt is SHA-256 of the exact
`reticulum-rs-firmware/device-api/session/hkdf-salt/v1\0` domain plus the
transcript hash. HKDF-SHA256 uses the 32-byte client PSK as input key material.
Five independent expansions use the exact
`reticulum-rs-firmware/device-api/session/hkdf-expand/v1\0` domain, a one-byte
purpose (`1` server proof, `2` client proof, `3` client-to-device record, `4`
device-to-client record, `5` session ID), and the transcript hash. The first
four outputs are 32 bytes; the session ID is 16 bytes.

The server proof is the full HMAC-SHA256 of the exact
`reticulum-rs-firmware/device-api/session/server-proof/v1\0` domain plus
transcript hash. The client proof is the full HMAC-SHA256 of the exact
`reticulum-rs-firmware/device-api/session/client-proof/v1\0` domain, transcript
hash and server proof. Proofs occupy their 32-byte record payloads; those
records carry the derived session ID, sequence zero and a zero framing-tag
slot. A logical API record is not admitted until the client proof verifies.

Established sequences begin at zero independently in each direction. A record
tag is the leftmost 16 bytes of HMAC-SHA256 over the exact
`reticulum-rs-firmware/device-api/session/client-to-device-record/v1\0` or
`reticulum-rs-firmware/device-api/session/device-to-client-record/v1\0` domain
plus `Record::write_authenticated_data`, which contains the full canonical
34-byte header and valid payload. Independent keys and domains prevent
reflection. The server accepts only the exact next request sequence and permits
only one in-flight request. Reply typestate retains the session and reserves
its TX sequence until every framed byte is acknowledged. Dropping a partial
flight drops the session, so a possibly transmitted sequence is never reused.

The complete non-secret known-answer inputs, intermediate keys, proofs, decoded
records and COBS wire bytes are committed in
[`interop/vectors/device-api-session-v1.json`](../../interop/vectors/device-api-session-v1.json)
and independently regenerated by
`interop/python/generate_device_api_session_vectors.py`.

Every negotiable value is transcript-bound. Unsupported versions or suites
fail closed with no unauthenticated fallback. Directional record sequences
start at zero and accept only the exact next value. Duplicate, gap, overflow,
reflection, bad tag or wrong session ID terminates the session. Proof and tag
comparison is constant-time, session key material is zeroized where practical,
and the future bearer manager must enforce handshake timeout and attempt-rate
policy around this core.

The USB and Wi-Fi qualification suites use HKDF-SHA256 plus
HMAC-SHA256 truncated to the fixed 128-bit record tag. That suite provides
authentication and integrity, not confidentiality, and is labelled and gated
as such. Each suite is bound to exactly one bearer; neither is enabled for BLE.
They are not sufficient for private LXMF/configuration traffic against a
passive observer on their respective bearer. A production client
profile selects a reviewed AEAD suite, binds the canonical header as associated
data and disables any downgrade to the qualification-only suite. The fixed
128-bit tag and session ID do not require a framing change for that transition.

### Bind authorization at durable acceptance

Immediately before dispatching every authenticated request, the node/storage
owner revalidates the grant's credential ID and generation and derives a fresh
device-owned `DispatchContext`. Public logical operations such as
`system.capabilities` and `identity.summary` require no operation permission,
but they still cross an authenticated bearer session; “public” does not mean
unauthenticated wire access. Immediately before accepting a state-changing request, the same
serialized owner revalidates the required permission. The durable acceptance
contract requires the principal, authorized operation/policy snapshot and a
principal-scoped idempotency key. Semantic schema 2 persists the principal,
operation intent, credential ID/generation, authority revision, policy version,
and exact granted permission mask. A rotated retry preserves the original
acceptance evidence. Revocation or disconnect prevents work not yet accepted
but does not undo an already accepted mutation. See ADR 0008.

ADR 0009's separate zero-session, zero-tag initialization-control records are
not logical device-API operations and cannot invoke this adapter or a session
fallback. They expose only coarse initialization status and a physically gated
explicit initialization request needed before any credential exists. Every
logical operation, including public capabilities, remains session-authenticated.

The implemented `AuthenticatedGrant::revalidate` returns a non-cloneable
`DispatchLease` that immutably borrows the current authority. It derives the
principal and permissions from the exact active record, exposes them only
through a higher-ranked synchronous callback, and remains alive through the
immediate adapter dispatch. `DispatchContext` is no longer cloneable or
copyable, so the exact value cannot be moved out of that callback. Its scalar
facts remain reconstructible by trusted linked Rust code; immediate use and no
fallback are sole-owner integration/review obligations, not an unforgeable
capability boundary. Revalidation failure must never be downgraded to an
unauthenticated context or invoke an adapter/storage port. The permanent E290
node now implements that composition: it decodes the bounded logical request,
asks the resident credential runtime to revalidate against the currently
publishable authority, and invokes the adapter synchronously through a
short-lived submission-port view borrowing only coordinator fields disjoint
from credential authority. Missing, replaced, revoked, or generation-mismatched
state returns the generic authentication-required response with zero
submission-port I/O and no unauthenticated fallback. A malformed logical
request is retained as a terminal quarantine owner because it has no
trustworthy logical request ID; an unexpected post-dispatch encoding failure
is likewise never redispatched. Cross-snapshot successor regressions separately reject
same-generation authorization changes and silent credential removal. The USB
bearer now reaches this path in source, but powered hardware has not exercised
it.

A reply is delivered only to its accepting live session or retrieved later by
the same authorized principal. Session epochs route replies; they are not
principal identity. A stale reply is drained and discarded rather than allowed
to enter a new session's response stream.

The portable authority defines non-wrapping global revisions and the required
rotation order: enroll a replacement before revoking the old record. Boot
persistence/recovery, live pairing successor mutation, and pairing timeout/
exclusivity policy are now composed. General rotation, revocation, factory
reset, and production failure-rate policy remain future work and require
explicit tests. Until secure
boot and flash encryption are enabled, a device PSK is extractable by a
physical attacker and must not be described as tamper-resistant.

## Consequences

- USB provides the missing local admission edge for the complete LoRa-first
  E290 path without becoming a second Reticulum interface.
- Framing, authenticated grant, admission, and node acceptance are reused by
  the separately transcript-bound Wi-Fi suite. BLE still needs its own
  extension and qualification.
- A future USB/Wi-Fi/BLE Reticulum packet actor still uses the separate
  interface registry and native-packet ownership contract from ADR 0003.
- Framing and handoff are allocation-free and fixed-capacity; session and
  credential stores must preserve the same bounded behavior.
- USB API composition cannot coexist with raw automatic Serial/JTAG logging.
- The authentication-only lab suite is a deliberate qualification aid, not the
  final confidentiality story for LXMF or administration.

## Validation status and remaining bearer gates

- Complete: deterministic handshake, key-schedule, proof, record-tag and COBS
  interoperability vectors from an independent Python implementation.
- Complete in the portable core: reflection, downgrade, wrong-generation,
  reset-nonce, sequence gap/duplicate/overflow, wrong-session and bad-tag
  rejection, partial hello/proof/reply acknowledgement typestate, exact
  correlation matching and old-reply/new-session epoch-alias rejection.
- Complete in the portable session/client core: Wi-Fi suite 2 is explicitly
  bearer-bound, rejects suite/bearer mismatch on both peers, survives
  arbitrarily partial stream reads/writes, and advances the shared epoch across
  reconnect. This is host evidence, not powered E290 Wi-Fi qualification.
- Complete in the portable authority: immutable fixed-capacity snapshot
  validation, stable permission vocabulary, constant-time active selection,
  zeroizing handoff to session, PSK-free revocation tombstones, grant-to-lease
  revalidation and revoke-after-admission authority rejection.
- Complete in semantic schema 2: exact authorization provenance is validated,
  durably encoded, retained across replay/remount, and mapped only after logical
  authorization succeeds.
- Complete in the permanent E290 source graph: static depth-one authenticated
  request/reply handoff, a fair node dispatch lane, current-authority
  revalidation, synchronous dispatch through a credential-disjoint submission
  view, retained reply pressure, and generic rejection with no fallback or
  submission-port I/O. The minimal USB bearer admits one active session and one
  request in flight, with idle ClientHello replacement into a fresh epoch and
  fault-until-reset behavior. Replacement never displaces request/reply owners.
  The host may issue sequential requests in one established session; the
  powered `submit-and-wait` path uses this for status polling. Idle replacement
  is powered-qualified across consecutive authenticated client processes on
  one unchanged USB enumeration. Busy-owner non-displacement and richer
  established-stream fault/recovery behavior remain to qualify.
- Deliberately deferred from the first bearer profile: resumption, protocol
  retries, close records, encryption, rate limiting/attempt policy, concurrency,
  and richer established-stream recovery.
- Complete in the portable store: the dedicated two-sector format,
  operation-scoped binding, erased-only initialization/recovery,
  commit/retire/publication ordering, and 32-test power-cut/error matrix.
- Complete in the portable bootstrap codec: the four ADR 0009
  status/initialize record kinds, zero session/tag and exact payload shapes,
  with no logical API dispatch or bearer behavior.
- Complete in E290 boot composition: exact partition/eFuse binding, immediate
  post-open mount, bounded retire then cleanup, retained mounted ownership, no
  auto-provisioning, and credential-domain failure isolation while LoRa
  continues. This is host/target-build evidence, not powered authentication.
- Complete in the pre-authentication E290 path: debounced GPIO21 presence,
  shared USB control/live decoding and exact-next sequencing, reset-generation
  gating, secret-owning handoff, node causal scheduling, and correlated durable
  Begin/Activate/Abort replies.
- Remaining: durable revoke/rotate/reset transactions and powered successful
  USB handshake/request/reply plus lifecycle/cut/window/rate/API tests.
- Remaining: cancellation at the concrete USB RX/TX, request-admission and
  reply-channel boundaries.
- Remaining: concrete stale-reply channel draining and idempotent retry after
  disconnect.
- Complete: RISC-V and ESP32-S3 `no_std` checks and strict Clippy/rustdoc.
- Remaining: static memory accounting in the composed bearer task.
- Complete on macOS for full USB re-enumeration restoring sequence zero without
  credential mutation; Linux/Windows reconnect and macOS suspend/resume remain
  powered matrix work without corrupting LoRa scheduling or durable storage.

## Deferred decisions

This ADR does not select the production AEAD construction, extend the minimal
authenticated USB bearer beyond its single-handshake/single-request profile, define
a USB OTG composite descriptor, add WebUSB/NCM, or create any non-LoRa
Reticulum packet actor. The qualification pairing/rate policy is selected, but
its powered successful mutation path and powered USB handshake/request/reply are
still required before the authenticated USB-to-LoRa qualification path can run.
Production AEAD and the later transport/interface decisions are not
prerequisites for that explicitly integrity-only wired lab profile.
