# ADR 0009: Device-API credential store and initial pairing policy

- **Status:** accepted design; firmware integration, pairing implementation,
  and powered qualification pending
- **Date:** 2026-07-17
- **Decision owners:** project maintainers
- **Extends:** [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0006](0006-authenticated-local-api-bearer.md), and
  [ADR 0007](0007-device-api-credential-authority.md)

## Context

The portable credential authority and its canonical 2,048-byte semantic image
exist, but the permanent E290 firmware cannot admit an authenticated external
request until one device-owned authority survives power loss. Pairing must not
publish a secret-bearing authority before its physical commit is established,
and an empty or erased store must not silently create a trust relationship.

This first policy is deliberately a developer/HIL contract. The E290 image
currently rejects flash encryption, and USB Serial/JTAG provides neither peer
identity nor confidentiality from another process on the connected host. The
policy is sufficient to qualify ownership, persistence, and proof-of-
possession ordering; it is not the production physical-attacker or
out-of-band trust story.

## Decision

### Reserve one exact E290 credential range

The permanent 16 MiB E290 map assigns `api_credentials` the exact raw-NOR
range `0x614000..0x616000` (8 KiB, ESP partition type `data`, subtype
`undefined`). This splits the previously unwired configuration reservation:
`device_config` moves to `0x616000..0x630000` and shrinks to 104 KiB. The
`node_journal` at `0x630000` and every later range remain unchanged.

The target partition-table guard must require exactly one plaintext, writable
`api_credentials` entry with that type, subtype, offset, and length, and must
reject duplicate labels, overlap, or a mismatched shape before protocol
service. A valid partition table proves only the physical binding. It does not
mean the credential store is formatted, mounted, or paired.

### Use two alternating full-snapshot sectors

Physical format 1 divides the partition into sector A at relative offset 0 and
sector B at relative offset 4,096. Each 4 KiB sector has this exact shape:

| Relative range | Size | Format-1 content |
| --- | ---: | --- |
| `0..128` | 128 B | Versioned snapshot header |
| `128..2176` | 2,048 B | Canonical semantic authority image |
| `2176..2208` | 32 B | Domain-separated SHA-256 digest |
| `2208..2240` | 32 B | Irregular commit marker, programmed last for the target |
| `2240..2272` | 32 B | Separate monotonic retirement marker |
| `2272..4096` | 1,824 B | Must remain erased |

The header magic is `RDAUTH01`. It records physical and semantic versions,
sector ID, retained-record count, nonzero authority revision, predecessor
revision and digest, 16-byte physical-device binding, and the fixed capacity,
slot size, and image size. Reserved header bytes must be zero. Revision 1 has
no predecessor; every later revision names exactly revision minus one and its
nonzero committed digest. The digest covers the domain-separated complete
header and semantic image. It detects corruption and trajectory conflicts; it
does not authenticate plaintext flash against a physical attacker.

All multibyte integers are little-endian. The normative 128-byte header is:

| Relative range | Field | Required value |
| --- | --- | --- |
| `0..8` | Magic | ASCII `RDAUTH01` |
| `8..10` | Physical format | `u16` value 1 |
| `10..12` | Semantic format | `u16` value 1 |
| `12` | Sector ID | 0 for A, 1 for B |
| `13` | Record count | `u8`, at most 16 and equal to the decoded image count |
| `14..16` | Reserved | All zero |
| `16..24` | Authority revision | Nonzero `u64` |
| `24..32` | Predecessor revision | Zero for revision 1; otherwise current revision minus one |
| `32..64` | Predecessor digest | All zero for revision 1; otherwise the exact predecessor digest |
| `64..80` | Device binding | Exact 16-byte product-supplied physical device ID |
| `80..84` | Record capacity | `u32` value 16 |
| `84..88` | Slot size | `u32` value 128 |
| `88..96` | Semantic image size | `u64` value 2,048 |
| `96..128` | Reserved | All zero |

The digest at `2176..2208` is exactly
`SHA-256("reticulum-rs-firmware/device-api-credential-store/snapshot/v1\0" || sector[0..2176] || [0xa5; 64])`.
The quoted ASCII domain includes its final NUL byte and is exactly 62 bytes;
the final operand is an exact 64-byte public flush trailer containing `0xa5`
in every byte. This trailer is part of physical format 1. The fixed domain and
2,176-byte secret-bearing prefix leave 62 bytes in the `sha2` 0.10 SHA-256
block buffer. Updating it with one public block first compresses that secret
tail with two public bytes, then overwrites all 64 buffer bytes with public
trailer data before finalization. This avoids leaving a raw PSK copy in a
non-zeroizing hasher buffer. The compile-time length guards and independent
empty-snapshot golden digest freeze this invariant. The normative literal
commit marker at `2208..2240` is:

```text
a3510ed8762bc4195f9037ea6d04b28c41f72d639815ce700bd456a932ef841a
```

The normative literal retirement marker at `2240..2272` is:

```text
6e13c942af7508e1349b5df02786bc4ad501793e926814cbf62c8350ad07e931
```

These marker bytes are literal format constants, chosen as irregular
SHA-256-derived values but not recomputed by decoders from an external input.
An exact marker is complete; all `0xff` bytes are absent. A partial value is a
recognized torn write only when every programmed bit could monotonically reach
the corresponding literal marker. Any other value is unknown media. The
crate's public format constants and golden-layout test are normative alongside
this table; changing any byte requires a new physical format version.

The semantic image is exactly 16 slots of 128 bytes. Unused slots are
canonical all-zero slots. The fixed-format lifetime ceiling is 16 unique
credential IDs total, including `Pending`, `Active`, `Revoked`, and aborted
pairing tombstones. An ID or tombstone is never reused or garbage-collected in
format 1. Rotation consumes a fresh ID. Exhaustion rejects new enrollment but
does not invalidate an already active credential.

Mount is read-only. Fully erased sectors report an unformatted-erased store;
programmed media without a committed authority, an unsupported version, wrong
device binding, invalid canonical snapshot, unknown marker trajectory,
unlinked snapshots, or a retired predecessor without its successor fails
closed. When both snapshots are committed, only an exact consecutive
predecessor link selects the newer. Recognized incomplete retirement or stale
inactive data yields one explicit recovery state: `RetirePredecessor` or
`CleanupInactive`, distinct from `Clean`. `publishable_authority()` and
`into_authority()` must refuse publication until predecessor retirement is
complete. Cleanup-only state may publish the already committed authority, but
the product must erase and verify that inactive sector before beginning an
unrelated credential mutation.

### Commit, retire, then publish

Every authority mutation is one complete immutable successor:

1. Validate the candidate as the exact next semantic revision without I/O.
2. Require a clean, erased inactive target sector.
3. Program and exactly read back the target header plus complete semantic
   image, then its digest.
4. Program and exactly read back the target commit marker.
5. Re-scan enough state to establish the exact target revision, predecessor
   link, digest, and canonical image.
6. Program and exactly read back the source sector's retirement marker.
7. Only then publish the candidate authority or report the mutation complete.
8. Erase and verify the retired source as a later explicit cleanup step while
   the committed target remains intact.

A backend error or uncertain readback after mutation begins retains the exact
current and candidate owners. The sole storage coordinator must reconcile that
specific mutation from media before serving either authority, erasing a
sector, or beginning unrelated credential/application work. It may resume an
exact recognized step; it must not guess whether a write happened. Boot uses
the same scan and recovery rules. This commit/retire order permits a power cut
to leave two linked committed snapshots, but never authorizes the firmware to
publish an uncommitted candidate or erase the only committed authority.

### Make empty-store initialization explicit and non-pairing

No boot path automatically formats or pairs an empty store. With an existing
Reticulum identity and both credential sectors exactly erased, local API
admission remains disabled. The only initial action allowed is an explicit
erased-only initialization command received on the button-confirmed USB
connection described below. It rechecks that both sectors are fully erased and
commits the canonical empty revision-1 authority. Programmed noncanonical
media is never reformatted by this path.

Initialization creates no credential, offers no secret, and does not
implicitly start pairing. A client must issue a separate pairing begin while a
valid physical-presence window is open. Factory reset and cross-store identity
reset ordering remain a separate design and may not be inferred from erased
credential media.

### Fix the first developer/HIL pairing window

The initial pairing manager, when implemented, uses GPIO21 as the E290 user
button and USB Serial/JTAG as its only bearer:

- a continuous roughly two-second button hold opens one exclusive 60-second
  monotonic window bound to the currently accepted USB connection;
- while open, ordinary device-API session establishment and all unrelated
  credential/admin mutation are refused;
- the window permits at most three total begin/proof attempts, closes on the
  third attempt, timeout, USB disconnect, or successful activation, and holds
  at most one `Pending` enrollment;
- no boot, cable connection, empty store, or elapsed timeout opens another
  window automatically.

Each accepted begin allocates a never-before-used random nonzero 128-bit
credential ID and random 256-bit PSK, plus the next authority revision. The
sole owner commits and exactly reads back that `Pending` successor before it
offers the ID and PSK to the same bound USB connection. A disconnect or failed
write never converts `Pending` to `Active` and never permits a new pending ID
to bypass it.

The client proves PSK possession with a domain-separated HMAC-SHA256 over a
fresh single-use device challenge and the pairing transcript, including the
credential ID and generation, stable public device-API ID, and current
connection/window binding. The later wire specification must freeze the exact
record encoding and domain bytes before implementation; it may not weaken
these bindings or accept a client-supplied principal/permission assertion.
Failure responses do not reveal whether any unrelated credential exists.

After valid proof, the manager constructs the exact next successor changing
that record from `Pending` to `Active`. It commits, reads back, retires the old
snapshot, and publishes the new authority before sending pairing completion or
allowing session authentication. Successful activation then closes the pairing
window. Thus both `Pending` before secret offer and `Active` before completion
are durable facts.

The current firmware logger shares the USB Serial/JTAG stream. Binary COBS
pairing/session service must not start while arbitrary text logs can interleave
with its records. Bearer composition must first move logs to a different sink,
disable them for that stream, or encapsulate them as an explicitly typed framed
channel. No raw log byte may appear between binary COBS records.

A surviving `Pending` record remains the only enrollment in progress. A later
button-confirmed window may prove it again if the client retained the secret,
or explicitly abort it by committing a PSK-free revoked/aborted tombstone
before allocating another ID. The firmware never silently restores, reuses,
or discards a pending secret-bearing ID.

### Defer the production security profile

Format 1 stores PSKs in plaintext. Raw flash access reveals them, just as it
reveals the current plaintext Reticulum identity. This policy is allowed only
for developer and HIL images with secret backup/dump handling.

A production decision must provide reviewed secure-boot and flash-encryption
provisioning or application-level authenticated encryption with independently
protected keys. Production pairing must also bind the intended client with an
independent display code, QR flow, or equivalent out-of-band confirmation.
Those decisions may reuse the semantic authority and commit/retire ordering,
but cannot claim security from the developer USB trust shortcut.

## Consequences

- Credential state has one checked range and one full-snapshot trajectory;
  `device_config` is not a competing authority.
- Secret offer, activation, and live publication each occur only after the
  durable transition required for that step.
- A fixed 16-ID lifetime ceiling is intentionally small but makes non-reuse,
  tombstone retention, RAM, scan time, and recovery behavior bounded.
- Empty or damaged media fails local admission closed and requires deliberate
  physical recovery rather than manufacturing a new trust root at boot.
- The lab profile is easy to qualify over the E290's built-in button and USB,
  but it does not protect against a malicious process on the connected host or
  physical flash extraction.

## Validation status and remaining composition gates

- Complete: 22 portable fake-NOR tests cover exact header/layout bytes, the
  independently generated digest golden vector, canonical empty provisioning
  and deterministic no-erase recovery, both
  sector roles, successor commit/readback/retirement, publication gating,
  cleanup, wrong binding, conflict, unknown media, revision exhaustion,
  corrupt-successor non-fallback, and every-byte/lost-reply/read-error
  trajectories at the program, readback, retirement, and erase boundaries.
- Complete: strict host Clippy and warning-free rustdoc plus generic bare-metal
  and ESP32-S3 Xtensa checks.
- Pairing tests must cover hold debounce/duration, exclusive window ownership,
  monotonic timeout, the combined three-attempt ceiling, disconnect at every
  secret/proof/completion boundary, one-pending enforcement, proof replay and
  wrong binding, explicit abort, erased-only initialization, and 16-ID
  exhaustion.
- Target composition must give the ADR 0004 coordinator the only flash and
  mutable-authority ownership, zeroize temporary secrets, preserve LoRa
  scheduling under USB pressure, and prove no API/session service starts from
  unformatted, unrecovered, or conflicting media. It must also prove USB
  disconnect closes pairing and that logging cannot interleave with binary
  COBS framing.
- Powered E290 tests must interrupt initial provisioning, pending creation,
  activation, retirement, and cleanup at every reachable boundary and verify
  exact flash readback before this path is enabled outside developer/HIL use.

None of those firmware, pairing, or powered gates is claimed complete by this
ADR.
