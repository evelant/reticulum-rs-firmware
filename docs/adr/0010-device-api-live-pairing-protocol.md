# ADR 0010: Wired developer pairing protocol

- **Status:** accepted for the USB Serial/JTAG developer/HIL profile; portable
  protocol/core, independent vectors, E290 resident durable lifecycle, bounded
  entropy, and bearer-neutral secret handoff implemented and target-verified;
  node/USB scheduling, recoverable pairing utility, and minimal authenticated
  USB session/API bearer implemented; successful powered qualification pending
- **Date:** 2026-07-18
- **Decision owners:** project maintainers
- **Extends:** [ADR 0006](0006-authenticated-local-api-bearer.md),
  [ADR 0007](0007-device-api-credential-authority.md), and
  [ADR 0009](0009-device-api-credential-store-and-pairing.md)

## Context

ADR 0009 fixes the physical-presence window, attempt budget, durable
`Pending`/`Active`/aborted lifecycle, and typed storage ownership, but
deliberately leaves the live pairing records and proof transcript unspecified.
The authenticated session core needed a frozen way to create and activate at
least one credential before it could become useful in the permanent E290
image. That ordering led to the pairing protocol below; the minimal session
bearer is now source-composed, while powered activation and authentication
remain open.

The first profile is a wired developer and hardware-qualification aid. It
trusts the process controlling the physically connected USB host after the user
holds GPIO21. It does not claim confidentiality against another process on that
host, a USB interposer, or physical flash access. Production pairing still
requires an independently confirmed display/code/QR ceremony or an equivalent
reviewed out-of-band binding.

A lost Begin response creates a special recovery requirement: the device may
have durably committed a `Pending` record while the host never received its ID
or PSK. Exact-next USB sequencing correctly prevents replaying that response.
The protocol therefore needs an explicit way to abort the device's sole current
pending enrollment without requiring the lost identifier.

## Decision

### Keep live pairing separate from initialization and sessions

`reticulum-device-api-pairing-control` remains the narrow, non-secret
status/initialize codec. A separate allocation-free, `no_std`
`reticulum-device-api-pairing` core owns the live pairing records and possession
proof. Neither codec dispatches the logical device API.

All live pairing records use the canonical `RDA1` framing record with an
all-zero session ID and all-zero framing authentication tag. They share the
same boot-lifetime, connection-wide exact-next pre-authentication sequence gate
as status and initialization. A response echoes its request sequence. The
portable pairing codec preserves but does not interpret that sequence; the
bearer owns ordering, replay rejection, connection epochs, and response
correlation.

The fixed record kinds are:

| Kind | Value | Direction |
| --- | ---: | --- |
| Begin request | `0x24` | client to device |
| Begin response | `0x25` | device to client |
| ProofStart request | `0x26` | client to device |
| Proof challenge response | `0x27` | device to client |
| Activate continuation request | `0x28` | client to device |
| Activate response | `0x29` | device to client |
| AbortCurrent request | `0x2a` | client to device |
| AbortCurrent response | `0x2b` | device to client |

Pairing protocol major/minor are `1.0`, the proof suite is `1`
(HMAC-SHA256 with a 256-bit PSK), and bearer binding `1` means ESP32-S3 USB
Serial/JTAG. Other bearer values remain unavailable in this profile. Pairing
never accepts a client-supplied principal, permission set, policy version, or
credential generation.

### Begin only offers a durably recoverable Pending credential

The Begin request payload is empty. A bound open window spends one shared
Begin/Proof attempt before the node rechecks mutation readiness, retained-ID
capacity, and next authority revision.

On admission, the device generates independent fresh nonzero 128-bit credential
and principal IDs plus a fresh nonzero 256-bit PSK. It constructs the fixed
device-owned developer policy described below, commits the exact Add-`Pending`
successor, reconciles any ambiguous physical result, reads back the complete
candidate, retires its predecessor, and installs a publishable mounted
authority. Only then may it emit an offer.

A successful Begin response is exactly 88 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 1 | result `0` (`Offered`) |
| 1 | 7 | reserved, exactly zero |
| 8 | 2 | pairing major `1`, little-endian |
| 10 | 2 | pairing minor `0`, little-endian |
| 12 | 2 | proof suite `1`, little-endian |
| 14 | 1 | bearer binding `1` |
| 15 | 1 | reserved, exactly zero |
| 16 | 16 | stable public device-API ID |
| 32 | 16 | new credential ID |
| 48 | 8 | nonzero credential generation, little-endian |
| 56 | 32 | new credential PSK |

Every non-success Begin response has exactly one byte:

| Result | Value |
| --- | ---: |
| `Offered` | 0 |
| `PhysicalPresenceRequired` | 1 |
| `Refused` | 2 |
| `Blocked` | 3 |
| `Unavailable` | 4 |

The PSK is a non-copyable, non-debuggable, zeroizing owner through the codec,
task handoff, framing, and host credential-file path. A disconnect or lost
response never activates or silently discards the durable pending record.

### ProofStart creates a fresh current-window challenge

The client durably stores and synchronizes the offered credential before
starting proof. ProofStart supports both a just-created pending credential and
one resumed in a later button-confirmed window.

Its request payload is exactly 64 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 2 | pairing major `1`, little-endian |
| 2 | 2 | pairing minor `0`, little-endian |
| 4 | 2 | proof suite `1`, little-endian |
| 6 | 1 | bearer binding `1` |
| 7 | 1 | reserved, exactly zero |
| 8 | 16 | exact pending credential ID |
| 24 | 8 | exact pending generation, little-endian |
| 32 | 32 | fresh nonzero client nonce |

A bound open-window ProofStart spends one shared attempt and must name the exact
durable pending record. The node selects its PSK only through the mounted,
publishable credential store, generates a fresh nonzero 256-bit challenge, and
retains the policy permit, selected secret, transcript, and connection/window
binding until a terminal continuation.

A successful challenge response is exactly 104 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 1 | result `0` (`Challenge`) |
| 1 | 7 | reserved, exactly zero |
| 8 | 2 | pairing major `1`, little-endian |
| 10 | 2 | pairing minor `0`, little-endian |
| 12 | 2 | proof suite `1`, little-endian |
| 14 | 1 | bearer binding `1` |
| 15 | 1 | reserved, exactly zero |
| 16 | 16 | stable public device-API ID |
| 32 | 8 | nonzero boot-lifetime connection epoch, little-endian |
| 40 | 8 | nonzero boot-lifetime pairing-window ID, little-endian |
| 48 | 16 | exact pending credential ID |
| 64 | 8 | exact pending generation, little-endian |
| 72 | 32 | fresh single-use device challenge |

Every non-success challenge response is exactly one byte and uses the same
coarse values as Begin, with `Challenge` at value zero. Missing, stale, wrong-
generation, and otherwise unavailable pending records are not distinguished.

### Activate is a continuation, not fresh authority

The transcript hash is SHA-256 over the exact bytes:

```text
"reticulum-rs-firmware/device-api/pairing/transcript/v1\0" ||
0x26 || LE16(64)  || complete ProofStart request payload ||
0x27 || LE16(104) || complete successful challenge payload
```

The request and challenge together bind protocol version, suite, bearer,
device ID, connection epoch, window ID, credential ID, generation, client
nonce, device challenge, lengths, and message roles. Framing sequences remain
independently protected by the bearer's exact-next state and are not duplicated
in this transcript.

The client proof is the full 32 bytes:

```text
HMAC-SHA256(
  PSK,
  "reticulum-rs-firmware/device-api/pairing/client-proof/v1\0" ||
  transcript_hash
)
```

The Activate continuation request is exactly 56 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 16 | exact pending credential ID |
| 16 | 8 | exact pending generation, little-endian |
| 24 | 32 | full client proof |

It is accepted only as the next message for the one outstanding ProofStart.
It is not a new policy attempt and cannot name another pending record. The
device compares the proof in constant time. A mismatch consumes and rejects
the outstanding proof operation, zeroizes its secret state, and performs no
credential mutation.

For a valid proof, the policy converts the retained proof permit into the exact
activation capability. The sole store owner commits/reconciles the
Activate-`Pending` successor and installs its publishable authority before any
success response or authenticated session admission.

The successful device confirmation is:

```text
HMAC-SHA256(
  PSK,
  "reticulum-rs-firmware/device-api/pairing/activation-proof/v1\0" ||
  transcript_hash || client_proof
)
```

A successful Activate response is exactly 64 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 1 | result `0` (`Activated`) |
| 1 | 7 | reserved, exactly zero |
| 8 | 16 | activated credential ID |
| 24 | 8 | activated generation, little-endian |
| 32 | 32 | full device activation confirmation |

Every non-success response has exactly one byte:

| Result | Value |
| --- | ---: |
| `Activated` | 0 |
| `ProofRejected` | 1 |
| `Refused` | 2 |
| `Blocked` | 3 |
| `Unavailable` | 4 |

An ambiguous or lost success response does not roll back Active. The client
keeps its credential file and resolves the outcome by attempting a fresh
authenticated session after a confirmed USB reset. That authenticated
reconciliation lane is not yet composed in the current host utility: after an
ambiguous Activate it must retain the file and must not guess Active, invoke
AbortCurrent, or treat `resume` as an activation-state oracle.

### AbortCurrent recovers an orphaned pending enrollment

AbortCurrent has an empty request payload. It is accepted only inside the bound
physical-presence window and does not spend the Begin/Proof attempt budget. The
node, not the client, selects the sole exact current pending reference and asks
the policy for an `AbortPendingPermit`. It commits/reconciles the exact
PSK-free aborted tombstone before reporting success. It never revokes an Active
credential and never accepts an identifier or authority assertion from the
wire.

The one-byte response values are:

| Result | Value |
| --- | ---: |
| `Aborted` | 0 |
| `PhysicalPresenceRequired` | 1 |
| `Refused` | 2 |
| `Blocked` | 3 |
| `Unavailable` | 4 |

This operation is deliberately explicit and physically confirmed. It permits
recovery when a Begin offer was lost before the host could learn or persist its
secret.

### Fix the initial E290 device-owned policy

The permanent E290 stable public device-API ID is the 16 exact bytes
`"e290-api-1" || EUI-48`, where the EUI-48 is the factory eFuse MAC in network
byte order. It is separate from the physical flash/store binding.

For this developer profile, each new independent pairing receives a fresh
nonzero random principal ID selected by the device. The device assigns
`READ_SUBMISSION_STATUS | EXPERIMENTAL_SUBMIT_RNS_DATA`,
`PairingOrigin::UsbPhysicalPresence`, and authorization-policy version `1`.
The client supplies none of those values. A future rotation operation preserves
the existing principal explicitly; it does not infer rotation semantics from a
new Begin.

Entropy generation is bounded. The integration may make at most eight
candidate-generation attempts for nonzero and non-retained credential ID,
nonzero principal, and nonzero/non-duplicate PSK values, and at most eight
attempts for a nonzero challenge. Exhaustion fails closed without publishing a
new credential or reusing a challenge.

### Retain exact owners across asynchronous boundaries

- A PSK is never offered before Add-`Pending` is durable, read back, and
  publishable.
- A credential never authenticates before Activate-`Pending` is durable, read
  back, and publishable.
- A physical Add, Activate, or Abort owner that may have touched flash is
  retained until definite reconciliation. Connection loss cannot discard it.
- Disconnect or deadline expiry may definitely reject a challenge-only proof
  operation before activation begins, because no physical mutation has yet
  occurred.
- A secret-bearing reply remains owned and backpressured until every byte is
  accepted into the USB endpoint FIFO and `WR_DONE` is requested. Delivery
  ambiguity does not repeat a sequence or allocate around Pending.
- Credential mutation and journal physical mutation remain mutually excluded.
  Route-only Reticulum/LoRa service continues while local mutation is deferred.
- Pending credentials are never selectable by the ordinary authenticated
  session core. An open/acquiring pairing window or retained pairing operation
  excludes new ordinary-session admission.

Decoded records, streaming decoder scratch, encoded frames, pending PSKs,
challenges, proof owners, task handoff owners, host serial scratch, and host
credential buffers are zeroized on every terminal/drop path. Secret-bearing types implement neither
`Copy` nor `Clone` nor secret-revealing `Debug`. Firmware logs never contain a
PSK, proof, challenge, or raw pairing record. The selected RustCrypto
`Sha256`/`Hmac<Sha256>` contexts do not implement `Zeroize`; their internal
context memory is an explicit cryptographic-backend residual even though every
project-owned input, output and scratch owner wipes. Rust moves may also leave
compiler-created copies of a value's former representation; `zeroize` cannot
guarantee those copies are erased. The resident lifecycle therefore guarantees
wipe-on-drop for the current project-owned secret owner, but does not claim that
typestate/enum moves leave no transient stack copy. Avoiding that residual would
require a separately reviewed pinned/in-place secret-state design and compiler
assumptions stronger than this developer profile currently makes.

`WR_DONE` transfers the completed response to USB hardware; software no longer
owns those FIFO bytes. The firmware therefore detaches the native USB pad and
power-cycles USB memory at the earliest application entry on every boot, keeps
the pad detached through initialization, installs the reset ISR, and opens no
new epoch until a detectable reattachment produces the expected clean reset.
Runtime bus reset applies the same block/detach/scrub/reattach sequence. The ROM
and bootloader interval before the earliest Rust entry remains a hardware/
boot-chain residual and must not be described as a proven secret-erasure point.

## Host persistence contract

Before Begin, `pair` creates without overwrite, synchronizes, and read-verifies
an owner-only 96-byte Reserved marker. That marker is secret-free. A definite
no-offer removes it; an ambiguous Begin retains it because the device may have
committed an unrecoverable Pending PSK. After receiving an offer, the client
writes and verifies a complete owner-only staging file containing magic,
format, Pending state, device ID, credential ID, generation, and PSK, then
atomically renames it over the reservation and synchronizes the parent before
ProofStart. It never prints the PSK. Secure `pair` and `resume` persistence is
currently Unix-only; `abort-current` remains available on other hosts.

`resume` reopens only a canonical Pending file, generates a fresh nonce, and
validates the exact returned device ID and credential reference before proof.
Pair requires Begin sequence `<= u64::MAX - 3`; resume requires ProofStart
sequence `<= u64::MAX - 2`. Lost ProofStart can retry through `resume` in a
fresh physically confirmed window. A lost Begin offer leaves only the Reserved
marker and requires assessment plus physically confirmed AbortCurrent. An
ambiguous Activate retains the complete file for the future authenticated
reconciliation lane; the current client must not guess or abort.

## Validation

The portable core and an independent standard-library Python implementation now
freeze known-answer records, COBS bytes, transcript hash, client proof, and
activation confirmation. Thirteen Rust unit tests and four ownership
compile-fail doctests cover every successful flight, malformed profiles and
shapes, coarse results, substituted credential references, secret-owner drop
glue, and constant-time proof rejection. Five Python tests independently
regenerate the corpus, mutate every transcript-bound byte and both message
roles, and verify COBS framing and proof-domain separation. Host, strict
Clippy/rustdoc, generic `riscv32imac-unknown-none-elf`, and ESP32-S3 Xtensa
checks pass.

The permanent E290 graph contains the live-pairing core only through its
resident credential lifecycle. Its host library suite covers durable
Add -> Proof -> Activate, Add -> AbortCurrent, cleanup before subsequent
mutation, resumed Pending under a fresh connection/window, partial writes and
lost replies, disconnect/timeout/replacement challenge cancellation, exact
connection/window/deadline binding, eight-attempt entropy exhaustion, semantic
no-I/O failure, RAM ceilings, and exact secret-bearing depth-one pressure. The
stable E290 device ID is captured once by the resident runtime and cannot vary
between Begin and ProofStart. The same package passes strict Clippy/rustdoc plus
generic bare-metal and ESP32-S3
Xtensa checks.

The routed composition additionally covers generalized cross-store exclusion,
node/bearer reply retention through partial USB TX, shared sequence admission,
causal control/live scheduling, bus-reset challenge invalidation, reset-
generation blocking, physical detachment, and USB-memory scrubbing. The minimal
authenticated session/API lane and its no-fallback proof are now
source-composed. Exact final suite totals are recorded with the qualified image.
Powered qualification next reads the exact
credential partition after Pending, Active, and Abort and proves that only
Active authenticates after reboot.

## Consequences

- The authenticated session bearer can be composed against a reviewed,
  independently vectored credential-creation protocol instead of an ad hoc
  firmware exchange.
- A lost Begin offer has an explicit, physically confirmed recovery path.
- The qualification protocol remains reusable above another local byte bearer,
  but suite 1 is permitted only for the trusted wired developer/HIL profile.
- The protocol adds authentication of possession and activation confirmation,
  not confidentiality or production host identity binding.
- Authenticated session/API composition, activation-ambiguity reconciliation,
  and powered lifecycle/fault testing remain separate implementation gates.
