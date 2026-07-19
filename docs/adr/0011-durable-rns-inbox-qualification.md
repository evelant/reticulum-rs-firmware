# ADR 0011: Durable raw-RNS inbox qualification slice

- **Status:** accepted and implemented; the bounded end-to-end powered proof is
  complete, while target fault-isolation and timing qualification remain open
- **Date:** 2026-07-18
- **Decision owners:** project maintainers
- **Extends:** [ADR 0003](0003-lora-first-interface-fabric.md),
  [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0006](0006-authenticated-local-api-bearer.md), and
  [ADR 0010](0010-device-api-live-pairing-protocol.md)

## Context

The permanent E290 image can receive, decrypt, and validate Reticulum DATA,
and its transport-neutral Rete integration can surface the resulting plaintext
to project code. It does not yet prove that an inbound application payload can
cross the Reticulum boundary, survive a reset or power loss, and remain
observable through the authenticated local API.

A volatile mailbox would exercise task scheduling and API encoding, but it
would skip the hardest property of a standalone node: useful traffic must not
disappear merely because the client was disconnected or the device rebooted.
It would also let a later implementation discover flash ownership, commit
ordering, and boot-mount problems only after the application protocol had grown
around an invalid persistence assumption. The first inbox slice is therefore
durable even though its capacity and operations are deliberately minimal.

Conversely, this is too early to freeze an LXMF store. A real LXMF service must
decide message identity, propagation and delivery state, duplicate handling,
queue ordering, acknowledgements and tombstones, reclamation and compaction,
encryption at rest, schema migration, and the relationship between raw RNS
packets and reconstructed LXMF messages. Those decisions should follow
interoperability work with the selected LXMF implementation. Encoding a guessed
LXMF schema now would turn a hardware/durability qualification record into a
premature product format.

Reticulum delivery proof and application durability are separate facts. A
Reticulum proof establishes that the addressed peer received, authenticated,
and decrypted a valid packet according to Reticulum. It does not establish
that this firmware committed the resulting plaintext to an application inbox,
that an LXMF client consumed it, or that either state survives power loss. The
qualification plan must observe both facts independently.

## Decision

### Project raw DATA once, independently of its transport

The Rete boundary consumes each native `NodeEvent` exactly once. A
`DataReceived` event becomes a project-owned inbound value containing the
complete 128-bit destination and the original owned `Vec<u8>` payload. The
payload owner is moved, not cloned, copied, truncated, or interpreted. Every
non-DATA event is returned unchanged. `node-core` reexports this projection so
firmware does not depend directly on the concrete Rete stack.

This boundary is intentionally unaware of LoRa, SX1262, USB, BLE, Wi-Fi, LXMF,
and the inbox policy below. Any present or future Reticulum transport that
feeds the same Rete node can produce the same project-owned DATA event. LoRa is
the first source qualified, not an architectural special case.

The current network admission profile is an encrypted Reticulum `SINGLE` DATA
packet with at most 383 plaintext bytes. The one-entry store and API use that
same ceiling. The projection itself must nevertheless preserve larger DATA
values so a future packet mode, transport, or reassembly layer remains
observable to firmware. A larger value presented to this qualification store
is rejected explicitly and never silently truncated; choosing policy for it is
not the projection's responsibility.

### Bind one exact 2 MiB message-store range

The E290 permanent partition map contains exactly one plaintext, writable
`message_store` data partition with ESP type `0x01`, subtype `0x06`, absolute
range `0x0073_0000..0x0093_0000`, and length `0x0020_0000` bytes (2 MiB).
Partition validation rejects a missing, duplicate, overlapping, mistyped, or
mis-sized entry before inbox service is advertised.

Every operation carries an exact binding consisting of the 16-byte physical
flash device ID, absolute offset, partition length, and physical format version
1. The backend capacity and read/program/erase geometry must be compatible with
that binding. A valid partition table proves only the physical range; the
inbox becomes durable and available only after a successful read-only mount of
the complete range.

The 2 MiB reservation is intentional even though qualification format 1 uses
only 576 bytes. It preserves room for a later queue/blob design without
pretending this one-entry encoding is that design.

### Use a canonical one-entry, commit-last physical format

Physical format 1 has capacity one. It stores one fixed 576-byte record at
partition-relative offset zero and requires every byte in
`576..0x20_0000` to remain erased (`0xff`). All multibyte integers are little-
endian.

| Relative range | Size | Format-1 content |
| --- | ---: | --- |
| `0..32` | 32 B | Literal irregular claim marker |
| `32..40` | 8 B | ASCII magic `RNSINBX1` |
| `40..42` | 2 B | Physical format `u16` value 1 |
| `42..44` | 2 B | Reserved, exactly zero |
| `44..60` | 16 B | Exact physical flash device ID |
| `60..68` | 8 B | Absolute partition offset as `u64` |
| `68..76` | 8 B | Exact partition length as `u64` |
| `76..84` | 8 B | Nonzero item ID `u64`, exactly 1 |
| `84..100` | 16 B | Complete Reticulum destination hash |
| `100..102` | 2 B | Payload length `u16`, at most 383 |
| `102..112` | 10 B | Reserved, exactly zero |
| `112..495` | 383 B | Payload followed by canonical zero fill |
| `495..512` | 17 B | Canonical zero padding |
| `512..544` | 32 B | SHA-256 digest |
| `544..576` | 32 B | Literal irregular commit marker, programmed last |
| `576..0x20_0000` | 2,096,576 B | Must remain erased |

The literal claim marker is:

```text
b62d814ae35709cc7118f4932ad5600e8b34ca761fa945d26803be59e7209d41
```

The digest is exactly:

```text
SHA-256(
  "reticulum-rs-firmware/rns-inbox-store/record/v1\0" ||
  record[0..512]
)
```

The literal commit marker is:

```text
43da168f25b16ce80972cd34915af02eb8670cd34f9521ea7d38a45c12f96087
```

The markers are public physical-format constants. They distinguish an erased
record, recognized monotonic NOR write trajectories, and unrelated programmed
media; they are not authentication secrets. SHA-256 detects accidental or torn
record corruption but does not authenticate plaintext flash against a physical
attacker. The crate constants, canonical encoder, and independent golden vector
are normative with this table. Changing any byte requires a new physical
format version.

Mount is read-only and fail-closed. A completely erased range mounts `Empty`.
An exact committed record with a matching device/range binding, canonical body,
valid digest, and entirely erased remainder mounts `Occupied`. A partial claim,
an exact claim followed by incomplete body or commit, a monotonic partial commit,
unknown programmed bytes, an unsupported format, a binding mismatch, a bad
digest, noncanonical padding, or any programmed remainder is a stable fault.
Mount never guesses, repairs, erases, acknowledges, or garbage-collects media.

### Commit one item, then remain read-only

An empty mounted store re-inspects the complete range immediately before
admission. One accepted item is programmed in this order:

1. Construct the complete canonical body and its SHA-256 digest in memory.
2. Program the claim marker and read it back exactly.
3. Program the canonical body and digest and read them back exactly.
4. Program the commit marker last and read it back exactly.
5. Re-inspect and fully decode the committed record under the retained binding.
6. Publish `Occupied` in runtime state only after that final decode matches the
   intended item.

If a backend reports an error after a program operation, exact stage readback
is used to reconcile whether the intended bytes reached media. In particular,
a lost success result for the commit write can still become `Accepted` only
after the final complete decode. A mismatch or unresolved backend result never
invents success.

A cut before any claim programming leaves `Empty`. A cut after programming
begins but before the exact commit marker completes leaves a recognized fault,
never a publishable item. A cut after the commit marker and complete record
reach media restores the exact occupied item on the next mount. Format 1 has no
automatic repair for an interrupted record; that is acceptable for this
qualification format and makes the failure visible instead of silently losing
or replacing an item.

There is no acknowledgement, deletion, overwrite, erase, reclamation, or
garbage collection operation. Once occupied, the oldest committed item remains
unchanged. Each later inbound DATA item is dropped newest and increments
`dropped_since_boot` exactly once. The same counter covers every DATA payload
that reaches this projection but is not durably retained: an occupied slot, the
single boot-local deferred candidate already being occupied, an oversize
payload, unavailable or fault-disabled inbox service, or an admission fault.
An oversize item is never truncated. Deferral of the one retained candidate
does not increment the counter unless it is later discarded; while that
candidate is retained, every newer DATA item is dropped newest. The saturating
counter is runtime diagnostic state and resets on reboot; it is not another
flash mutation.

### Serialize with every other durable owner

ADR 0004's sole product flash coordinator owns the `message_store` access and
creates only operation-scoped, range-checked views. No node, Rete, radio, USB,
BLE, Wi-Fi, or client task receives raw flash access.

An inbox commit may begin only when the credential store has no retained
physical mutation and the submission journal has neither actor nor projector
mutation outstanding. Conversely, once the synchronous inbox transaction
starts, no credential, journal, configuration, or other store transaction can
interleave with its claim/body/commit/readback sequence. Deferral before inbox
I/O retains no ambiguous physical mutation.

The inbox implementation reconciles a reported stage failure by exact readback
inside the same coordinator operation. If it still cannot establish a clean
empty or exact committed result, the product disables the inbox for that boot
instead of retaining an unbounded retry owner or allowing unrelated code to
touch the range. This quarantine does not disable LoRa receive, transmit,
proof, or routing. Existing stronger credential/journal ambiguity rules still
apply globally to those stores; inbox failure does not weaken them.

### Expose only authenticated read-only API operations

Logical Device API version 1.2 adds the feature-gated experimental capability
`experimental-rns-inbox` and two operations:

| Operation | Number | Request | Successful response |
| --- | ---: | --- | --- |
| `experimental.rns_inbox.status` | `0xf002` | Empty map `{}` | Status map |
| `experimental.rns_inbox.peek` | `0xf003` | Empty map `{}` | Oldest item map |

The status response is the canonical map
`{0: depth u16, 1: capacity u16, 2: dropped_since_boot u64, 3:
max_payload_bytes u16, 4: durable bool}`. For this format, capacity is 1 and
maximum payload is 383. `durable` is true only after the exact store mounts
successfully. The E290 profile does not advertise or dispatch a volatile
fallback; a failed or unavailable mount makes the capability unavailable
rather than returning `durable=false`.

The occupied peek response is
`{0: item_id u64, 1: destination bytes16, 2: payload bytes}`. An empty mounted
store returns the protocol error `NotFound`. Peek does not consume, acknowledge,
or mutate the item.

Both operations require a valid authenticated principal. Because this is an
experimental, read-only developer qualification surface, version 1.2 does not
add another bit to the persisted permission vocabulary: every authenticated
principal may call status and peek. A final inbox/LXMF policy must revisit that
choice before adding mutation or multi-principal message access.

Capability maps add optional key 7 for inbox availability and key 8 for the
maximum inbox payload. Their absence decodes as unavailable and zero so API
1.0/1.1 peers remain compatible. The existing dispatcher constructor continues
to suppress inbox advertisement; composition must opt in explicitly with both
an implemented dispatcher and a successfully mounted durable store.

### Keep the qualification security limits explicit

Format 1 stores the decrypted destination and payload in plaintext. The E290
developer image does not enable flash encryption. Its USB developer bearer
uses HMAC-based authentication and integrity but does not encrypt the local
API traffic. A process that can observe the USB link, an interposer, or an
attacker with physical flash access may therefore read message contents.

These are accepted developer/HIL limits, not production confidentiality
claims. Wireless client bearers, production pairing, API encryption, and
encryption at rest require separate design and threat review before this inbox
contains sensitive user traffic.

### Do not treat format 1 as the LXMF store

The final LXMF service remains a separate decision. It is expected to define a
multi-entry queue, stable message identity and duplicate handling, delivery and
propagation states, acknowledgements and tombstones, bounded compaction and
reclamation, encryption at rest, schema evolution, and migration. It may reuse
the 2 MiB partition while replacing every byte of physical format 1.

No future firmware may silently reinterpret a format-1 record as a final LXMF
record. A replacement must carry a new version and an explicit preserve,
export, migrate, or erase policy. This ADR promises only that the raw DATA item
and its durability behavior are sufficient to qualify the end-to-end boundary;
it does not promise on-media compatibility with the product queue.

## Consequences

- The first inbound persistence proof exercises the real transport-neutral
  event and sole-flash-coordinator boundaries instead of a LoRa-specific or
  volatile shortcut.
- A valid Reticulum proof can precede or exist without an application-store
  commit. Clients and tests must not report a message as durably inboxed from
  proof evidence alone.
- The format and host fault model restore an exact committed record after
  reboot or a post-commit power cut, but powered fault-isolation and target-
  bounds confirmation remain exit criteria. Capacity one and the absence of
  acknowledgement or reclamation make it deliberately unsuitable for normal
  messaging. Repeating destructive qualification may require an explicit
  developer erase/reflash.
- Reserving 2 MiB for a 576-byte record wastes space in format 1 but avoids
  repartitioning before the real message store is designed.
- One occupied item is never displaced by traffic bursts. Newest-drop behavior
  is deterministic and RAM-bounded, at the cost of losing all later messages.
- A corrupt or interrupted inbox disables only local inbox service. The node
  continues ordinary Reticulum LoRa routing and proof behavior.
- All authenticated developer principals can read the retained plaintext item.
  This is simple enough for qualification but is not the final authorization or
  confidentiality policy.
- Synchronous full-range mount inspection and NOR programming share flash with
  radio and other durable services. Powered tests must measure watchdog,
  scheduling, and radio-deadline effects rather than assuming host correctness
  implies acceptable target timing.

## Qualification and exit criteria

The slice is complete only when all of the following pass in the permanent E290
graph, with exact evidence retained in the runbook:

1. **Transport-neutral projection:** unit tests prove exact destination and
   payload ownership transfer without clone/allocation, preservation of a
   non-DATA event including allocation-backed fields, acceptance of the
   encrypted `SINGLE` maximum of 383 bytes, and preservation of a larger future
   DATA value at the projection boundary.
2. **Golden physical format:** independent vectors freeze every format-1 byte,
   both markers, the domain-separated digest, device/range binding, item ID 1,
   canonical zero fill, and the erased remainder. The store implementation
   issues no erase operation.
3. **Mount classification:** tests cover erased, exact occupied, interrupted
   claim, interrupted body/commit, monotonic partial commit, unknown programmed
   data, wrong device/range/version, bad digest, invalid lengths/ID/padding, and
   programmed remainder. Every fault fails closed without mount-time writes.
4. **Power-loss ordering:** exhaustive host fault injection cuts before and
   after each claim, body/digest, and commit program/readback boundary. No cut
   publishes an uncommitted item; an error-after-write is accepted only after
   exact reconciliation and final decode.
5. **Capacity policy:** an occupied store returns item 1 unchanged, performs no
   flash write for newer traffic, drops each new DATA item exactly once, and
   reports a boot-local counter that resets without altering the committed
   record. Tests cover the occupied slot, retained-candidate pressure,
   oversize input, unavailable/faulted service, and admission failure without
   truncation or double counting.
6. **API contract:** canonical and negative codec vectors cover API 1.2,
   operations `0xf002`/`0xf003`, optional capability keys 7/8, authenticated-
   principal admission, empty `NotFound`, exact destination/payload peek, no
   mutation, and API 1.0/1.1 decoding when the new capability keys are absent.
7. **Cross-store exclusion:** composition tests prove credential and journal
   mutation owners defer inbox programming, the complete inbox commit excludes
   every other flash mutation, an unreconciled inbox fault disables its API
   service, and that quarantine does not disable LoRa.
8. **Powered end-to-end proof:** one E290 sends an encrypted 383-byte `SINGLE`
   DATA packet to the other. The receiver independently records a valid
   Reticulum proof and, through authenticated USB, reports durable depth one and
   peeks the exact destination and payload before and after reset. A newer
   packet leaves the first item unchanged and increments the drop counter once.
9. **Powered failure isolation:** controlled pre-commit and corrupted/mismatched
   mount cases never advertise a durable inbox or return an item, while ordinary
   Reticulum LoRa receive/transmit/routing continues. Target logs distinguish
   Reticulum validation, inbox admission, durable commit, and API observation.
10. **Target bounds:** image size, internal-RAM and PSRAM high-water marks,
    complete-range mount time, commit latency, watchdog behavior, and LoRa
    scheduling impact are recorded. Any missed radio deadline or unbounded
    coordinator stall blocks qualification even when the stored bytes are
    correct.

Passing these criteria authorizes work on the real LXMF queue design. It does
not promote physical format 1, HMAC-only USB, plaintext storage, or the shared
authenticated-principal policy into production requirements.
