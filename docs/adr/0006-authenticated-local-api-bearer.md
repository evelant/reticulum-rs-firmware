# ADR 0006: Authenticated local device-API bearer

- **Status:** proposed
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

The request owner carries:

- a local session epoch and request correlation used only for reply routing;
- an opaque authenticated grant minted from device-side credential state; and
- the exact bounded logical API message.

The node side receives the unique owner. Cancellation while waiting for
capacity retains the owner at the caller; cancellation while waiting to
receive leaves it in the channel. Once enqueued, connection loss cannot revoke
the node operation. The persistent bearer manager drains replies, discards a
reply from a stale epoch and allows an idempotent retry to become a new request
owner. Dropping either boot-lifetime endpoint is a fatal service teardown.

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

Physical presence authorizes pairing but does not prove which local host
process received a secret. The first lab profile explicitly trusts the
currently connected USB host during a short, exclusive, button-confirmed
pairing window. Before shipping a turnkey client, pairing adds an independently
confirmed display/code/QR ceremony or an equivalent reviewed out-of-band
binding. The qualification shortcut is not advertised as protection against a
malicious process already controlling the connected host.

Before session code is composed, v1 freezes deterministic encodings and test
vectors for this transcript:

1. `ClientHello`: protocol/version, suite, credential ID and 32-byte client
   nonce;
2. `ServerHello`: selected version/suite, device ID, 32-byte device nonce,
   credential generation and bounded limits/capabilities;
3. a hash over both complete length-delimited messages;
4. HKDF-SHA256 extraction from the per-client PSK with a transcript-bound
   salt;
5. distinct labelled expansion for server proof, client proof,
   client-to-device key, device-to-client key and 128-bit session ID;
6. server proof first, followed by a client proof bound to the transcript and
   server proof; and
7. no logical API admission before the client proof succeeds.

Every negotiable value is transcript-bound. Unsupported versions or suites
fail closed with no unauthenticated fallback. Directional record sequences
start at zero and accept only the exact next value. Duplicate, gap, overflow,
reflection, bad tag or wrong session ID terminates the session. Proof and tag
comparison is constant-time, session key material is zeroized where practical,
and authentication attempts are rate-limited.

The first wired qualification suite may use HKDF-SHA256 plus
HMAC-SHA256 truncated to the fixed 128-bit record tag. That suite provides
authentication and integrity, not confidentiality, and is labelled and gated
as such. It is not enabled for Wi-Fi or BLE and is not sufficient for private
LXMF/configuration traffic against a passive USB observer. A production client
profile selects a reviewed AEAD suite, binds the canonical header as associated
data and disables any downgrade to the qualification-only suite. The fixed
128-bit tag and session ID do not require a framing change for that transition.

### Bind authorization at durable acceptance

Immediately before accepting a state-changing request, the node/storage owner
revalidates the credential generation and required permission. Durable
acceptance records the principal, authorized operation/policy snapshot and a
principal-scoped idempotency key. Revocation or disconnect prevents work not
yet accepted but does not undo an already accepted mutation.

A reply is delivered only to its accepting live session or retrieved later by
the same authorized principal. Session epochs route replies; they are not
principal identity. A stale reply is drained and discarded rather than allowed
to enter a new session's response stream.

Credentials are atomically versioned and recoverable. Rotation enrolls a
replacement before revoking the old record. Factory reset, revocation, pairing
timeouts/exclusivity and failure-rate limits receive explicit tests. Until
secure boot and flash encryption are enabled, a device PSK is extractable by a
physical attacker and must not be described as tamper-resistant.

## Consequences

- USB provides the missing local admission edge for the complete LoRa-first
  E290 path without becoming a second Reticulum interface.
- Framing, session, authenticated grant and node acceptance remain reusable by
  later BLE or Wi-Fi local-API bearers.
- A future USB/Wi-Fi/BLE Reticulum packet actor still uses the separate
  interface registry and native-packet ownership contract from ADR 0003.
- Framing and handoff are allocation-free and fixed-capacity; session and
  credential stores must preserve the same bounded behavior.
- USB API composition cannot coexist with raw automatic Serial/JTAG logging.
- The authentication-only lab suite is a deliberate qualification aid, not the
  final confidentiality story for LXMF or administration.

## Required validation before acceptance

- Deterministic handshake, key-schedule and record-tag interoperability
  vectors from an independent host implementation.
- Negative replay, reflection, downgrade, wrong-generation, reset, sequence
  gap/duplicate/overflow and bad-tag vectors.
- Pairing timeout, exclusive-window, revoke, rotate and factory-reset tests.
- Cancellation at every partial RX/TX, request admission and reply boundary.
- Exact stale-reply draining and idempotent retry after disconnect.
- RISC-V and ESP32-S3 `no_std` checks, strict Clippy/rustdoc and static memory
  accounting.
- Powered USB reconnect tests on macOS, Linux and Windows without corrupting
  LoRa scheduling or durable storage.

## Deferred decisions

This ADR does not select the production AEAD construction, implement the
credential journal, define a USB OTG composite descriptor, add WebUSB/NCM, or
create any non-LoRa Reticulum packet actor. Those decisions do not block the
first authenticated USB-to-LoRa qualification path.
