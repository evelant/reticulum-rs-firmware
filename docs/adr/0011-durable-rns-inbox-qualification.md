# ADR 0011: Durable raw-RNS inbox qualification slice

- **Status:** accepted and implemented; bounded end-to-end, powered
  fault-isolation, and bounded target-measurement evidence exist, while physical
  power cuts and full target-bounds qualification remain open
- **Date:** 2026-07-18
- **Powered evidence updated:** 2026-07-21
- **Decision owners:** project maintainers
- **Extends:** [ADR 0003](0003-lora-first-interface-fabric.md),
  [ADR 0004](0004-sole-flash-coordinator.md),
  [ADR 0006](0006-authenticated-local-api-bearer.md), and
  [ADR 0010](0010-device-api-live-pairing-protocol.md)

## Context

Before this decision, the permanent E290 image could receive, decrypt, and
validate Reticulum DATA, and its transport-neutral Rete integration could
surface the resulting plaintext to project code. It had not proved that an
inbound application payload could cross the Reticulum boundary, survive a
reset, and remain observable through the authenticated local API, nor had it
defined fail-closed power-loss behavior.

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

## Powered fault-isolation evidence

### Deterministic cold-mount matrix

On 2026-07-19, `cargo +stable run --locked -p xtask --
e290-rns-inbox-fixture` generated complete 2 MiB partition images from a
canonical record first committed through this crate's public `mount`/`accept`
path. The tool requires an absent output, a 12-character lowercase source MAC,
and exactly one mode. It creates the output without replacement at mode `0600`,
synchronizes it, and prints only mode, length, and SHA-256. For source MAC
`aca704e13e88`, the reviewed vectors are:

| Mode/use | Exact programmed state | SHA-256 |
| --- | --- | --- |
| `interrupted-claim` | First 16 claim bytes programmed; every later byte erased | `4b9e6dad1415850588c001b17053e893ab1316aaa1b6d584082170d049f871f0` |
| `interrupted-commit` | Exact claim, body, and digest through byte 543; complete commit marker and remainder erased | `a8a8d40f63a69c7e3df59f4af1960f241f464566a5ae9251c12209eb3334c66a` |
| `invalid-digest` | Exact committed record with one digest bit monotonically cleared | `bb24e892d435a0b6888cc16f8733f096015a36f0f19dcd8a22e0978602e55ad5` |
| `committed`, used as foreign binding | Exact committed record bound to `ac:a7:04:e1:3e:88`, programmed on `ac:a7:04:e1:3f:88` | `dee21d3c72a914ac00627c49a119631999dc9e986ce18897b9a171254c79561b` |

The first three cases were programmed on `3e:88`; the fourth deliberately put a
valid `3e:88` record on `3f:88`. In all four powered boots, authenticated
capabilities reported inbox availability and maximum payload as `0/0`, status
and peek returned `CapabilityUnavailable` (code 7), and peek created no output.
One fresh peer LoRa DATA packet per case reached `Delivered` through the
receiver's proof transmission. The complete post-traffic 2 MiB readback remained
byte-identical to the injected fixture. This proves read-only fail-closed mount,
API suppression, no volatile fallback, no repair/admission write on disabled
media, and one bounded direct DATA/decrypt/proof exchange per case. It does not
prove sustained routing, forwarding, multi-hop operation, or a physical power
cut. The all-erased 2 MiB setup image used elsewhere in the qualification has
SHA-256
`4bda3a28f4ffe603c0ec1258c0034d65a1a0d35ab7bd523a834608adabf03cc5`.

### Same-boot missing-commit isolation

The non-default E290 feature `rns-inbox-commit-fault-hil` is a deterministic
target fault-injection fixture, not a product mode. It forwards the first two
inbox NOR writes, acknowledges the third without programming the terminal
commit marker, and forwards every later operation. It is mutually exclusive with
`journal-schema3-dev-reprovision` and graph policy requires its dependency graph
to be identical to the normal product graph; only the product root feature may
differ.

The 762,672-byte merged HIL image had SHA-256
`e693afad19c2eac28d958f902c1b8148ae360a6b54abb14338195ef595515239`.
On erased inbox media, one 147-byte peer packet with encoded-byte SHA-256
`0084ad098f2109b390d7c4568ba4a2dcd5285ac40062e55c9709665b2aebc73a`
reached `Delivered`. In that same boot, the receiver then reported the
commit-stage readback mismatch and inbox-service quarantine. The ELF-bound 40-byte
`RIAF` evidence structure at RAM address `0x3fc8bf7c` reported, in order,
`write_calls=3`, `commit_suppressed=1`,
`expected_commit_readback_mismatch=1`,
`unexpected_admission_failure=0`, `service_disabled=1`, and
`dropped_since_boot=1`.

The resulting complete store had SHA-256
`ad6d549f73681da7453870606fb34eeabad75b387f081176103562d84e5700c7`.
Its first 576 bytes had SHA-256
`acb43e7be289c5c4f822441670ce11554b6386ca3e1cfcee47907ee82c81d7f8`;
the exact claim, body, and digest were present and every byte from the commit
marker at 544 through the end of the partition was `0xff`. The RAM evidence was
captured before reset because it is deliberately boot-local and nondurable. The
ordinary 761,952-byte image was then restored with exact SHA-256
`d26587a2506408ec40cd42facb9bb87cc9c32e79c2afd2e1ab09f0e1268641cb`.

This HIL proves target execution of one deliberately missing commit write,
commit readback detection, same-boot drop accounting, and local service
quarantine after the triggering direct Reticulum proof completed. It does not
establish post-quarantine RF operation and is not
an electrical interruption, brownout, arbitrary-stage or partial-program fault,
backend error-after-write test, persistent counter, timing measurement, or
authorization to ship the feature.

## Powered target-bounds evidence

On 2026-07-20, the separate non-default `runtime-measurement-hil` image ran the
permanent E290 graph on both boards. The feature is mutually exclusive with the
other two exceptional feature profiles and adds only `esp-alloc/alloc-hooks`
below the product root. Its exact 256-byte initialized `RTME` version-1 ABI brackets each
low-to-high debugger capture with matching even sequence markers; an odd or
mismatched pair is rejected as torn. The default ELF excludes the evidence,
stack marker, stack-initialization hook, and allocator callbacks.

The 768,624-byte HIL image had SHA-256
`c20032b04a87fc8c33982bd7e4a5788f59ae5a00f7d26a1caf9f6ecf0473fa14`
and matched complete address-zero readbacks on both boards. The corresponding
default image was 761,792 bytes with SHA-256
`77b6a48e71d62facf39bae380387397dcbc79417c05372bc31c4a240f326b066`.
Across six accepted baseline/phase captures around the two traffic phases, the
measurement HIL observed:

| Bound | Observation |
| --- | ---: |
| Registered heap / detected PSRAM | 8,454,144 / 8,388,608 bytes |
| Maximum allocator use / minimum internal free | 988 / 64,548 bytes |
| External allocator use / failed allocations | 0 / 0 bytes/count |
| CPU0 usable stack / modified-word high-water | 170,480 / 98,268 bytes |
| Raw painted margin / margin after the 52,752-byte maximum frame | 72,212 / 19,460 bytes |
| Journal / inbox cold mount | 134,498--137,373 / 545,258--545,674 us |
| Maximum-payload inbound commit | 548,073--548,148 us |
| Worst node / radio loop gap | 646,388 / 1,065,406 us |
| RX / CAD / TX operation maximum | 933,255 / 38,229 / 885,258 us |
| RX / CAD / TX actor-watchdog count | 0 / 0 / 0 |
| Measurement lateness / work | 422,138 / 1,767 us |
| Unexpected measurement errors | 0 |

The project-local release gate preserves the current static inputs to the stack
calculation. CI runs Clippy and then relinks isolated default and measurement
ELFs with compiler `.stack_sizes` evidence; the inspector accepts only final
little-endian Xtensa executables with no remaining section relocations. The
retained Stage 5 default/HIL pair contains 946/962 records, has a 53,680-byte
maximum frame, leaves 175,056/174,256 usable stack bytes, and fixes both linker
guard offsets at 60 bytes. The later, now-historical pre-LXTE Stage 5 placement
checkpoint measured 57,716 powered raw bytes. That retained artifact
calculation deducts the independent announce scheduler's sixteen linked bytes,
preserving 57,700 carried bytes and 4,020 bytes after its maximum frame.
Current source instead gates named cumulative storage and startup paths against
each final linked stack. The preceding pre-PSRAM pair was 165,032/164,336 with
a 12,388-byte carried margin.
The painter already covers the one-shot maximum-frame constructor, so the
carry-forward deduction is deliberately pessimistic rather than a measured
runtime remainder. It still does not establish interrupt/nesting headroom. It
qualifies the
E290 CPU0/main-executor stack, which remains in internal SRAM; it is not a
compatibility ceiling for non-PSRAM boards. The full E290 profile separately
requires PSRAM, while Tracker V2 remains a reduced profile.

Phase A delivered one 383-byte payload from `3e:88` to `3f:88`; the receiver
durably committed the exact payload and the sender reached `Delivered`. The
immediate reverse attempt returned `no-path`. After explicit journal-only
reprovisioning and a fresh peer ANNOUNCE, phase B sent a different 383-byte
payload from `3f:88` to `3e:88`. The receiver recorded and exposed the exact
durable item, but the sender terminated in `delivery-timeout`. These compatible
observations prove one bounded durable maximum-payload inbound commit on each
board and in each direction, not bidirectional `Delivered`. The product-level
proof/status timeout is not an RX, CAD, or TX driver-watchdog count and remains
a diagnostic residual.

The follow-up diagnosis establishes that Rete generates the delivery proof and
the inbound DATA event in one synchronous ingress result and targets the proof
to the source interface; no learned reverse route is required. The product
currently persists the exposed DATA event before its ordinary proof action can
be staged, adding roughly the measured inbox-commit interval to the reverse
path. That delay is material to half-duplex overlap but remains far below the
fixed 30-second receipt timeout and does not by itself explain phase B. The
leading hypothesis is therefore a one-shot proof RF/reassembly loss or a
sender receive-blind interval; proof ingress rejection/correlation is the next
boundary to distinguish. No protocol correction is justified until that
boundary is observed.

The opt-in runtime HIL now adds a separate exact 192-byte `RPTE` version-1
record without changing the existing 256-byte `RTME` ABI. It counts logical
radio reassembly, ingress handoff outcomes and retry pressure, RNS dispositions,
locally generated explicit delivery proofs, delivered and timed-out receipt
terminals, action-pressure observations, correlation faults, confirmed versus
not-confirmed-success TX-wrapper outcomes, and Ready-gate inbox-admission
attempt boundaries. Compact generated/delivered/timeout tags use the first
eight bytes of the covered receipt hash and are useful only as correlation aids
under the controlled single-active-attempt fixture. LRPROOF handshake packets
and forwarded transport proofs are explicitly excluded from generated
delivery-proof counts. The default ELF excludes the record; the HIL ELF has one
initialized 192-byte instance whose linked bytes are decoder-validated.

The final `9bceacd` diagnostic HIL merged image is 779,184 bytes with SHA-256
`fe5fae51d83ef248a46965f75dab87196c1e79c2b4a72797cdf995e9c99a3e15`.
It passes build, graph, ELF, and static-stack gates but has not yet been flashed
because both boards were absent after the preceding debugger-reset attempt.
The immediately preceding 777,600-byte HIL image, SHA-256
`151a66cc92b83268050c61bfc983ad6d9452fac0626d260c26da877c552c800e`,
matched an identity-qualified exact address-zero readback on `3e:88`. A powered
boot-only baseline before any authenticated request decoded stable `RPTE`
sequence 4 with no saturation/input inconsistency, zero
RX/RNS/proof/receipt/correlation/inbox observations, and two boot-announce
action-pressure observations. Its words 45 and 46 were still reserved, so this
run does not power-qualify the final TX-outcome counters. The companion runtime
record retained a 72,020-byte painted stack margin and zero radio watchdog
expiries. The trace record costs exactly 192 bytes of linked HIL stack space;
the default image is free of it. The separate decisive raw-RNS qualification
plan remains four clean direction-balanced trials (`3f→3e`, `3e→3f`, `3e→3f`, `3f→3e`) with a unique
maximum payload and idempotency key per trial. Both boards must be reset and
independently reprovisioned before every trial; the sender boots first, the
receiver boots last to supply a fresh ANNOUNCE, and no pre-submit authenticated
request is allowed. Matching-even RTME/RPTE snapshots are retained at
baseline, about five seconds after submission, and terminal result using
addresses resolved from the exact HIL ELF. For those immediate raw-RNS proofs,
receiver generated tags must correlate with sender Delivered or timeout tags,
and durable payload readback remains the inbox success oracle.
The complete artifact naming and counter/tag acceptance matrix are frozen in
the [E290 runbook](../e290-node.md#decisive-proof-correlation-trial-runbook).
Board `3f:88` must first be physically re-enumerated and recovered from its
interrupted pre-write reset at this checkpoint; no result is claimed from the
one-board baseline.

The later Stage 5 retained-LXMF path has a different observation boundary and
must not inherit that receiver-`RPTE` requirement. It intercepts the retained
proof before ordinary RNS ingress metadata, so receiver `RPTE` generated-proof
count/tag correctly remains zero. Its correlation joins the `LXTE` release tag,
the confirmed-TX counter delta, and the sender's delivered tag instead.

The retained Stage 5 default ELF is 13,648,888 bytes with SHA-256
`92e63b60a5f4b830ee55d958fcc446a6878036212904b8748519ae210ba3da58`;
its 868,656-byte package uses 803,120 application bytes and has SHA-256
`c8da2af30e2d0ee24ca4b215151d1370b7e1d242991ebbeb024079a730693a3f`.
The matching retained runtime-measurement HIL ELF is 13,821,496 bytes with SHA-256
`7a3fad34699f910a2050468ada6461a0f33d16641ab5425a5c795a71238861ff`;
its 881,456-byte package uses 815,920 application bytes and has SHA-256
`12c6f31a7fb64485ad9220edca4ac38ba0a57867ad88ce60fa1a24ffc195d379`.
Both boards matched exact HIL readbacks. Exactly one fresh A-to-B Stage 5 trial
sent a 206-byte LXMF carrier as an exact 307-byte RNS packet with SHA-256
`060037041c91eb5999f89bf84845c19e65bf7fa680827cce9c51e8ecc5dbe0a6`
and reached `Delivered` on the first attempt. B advanced LXTE durable-new,
proof-ready, proof-released, and ordinary-handoff by one, with zero already-
durable/order events; release tag `0x3dc4588d3a205429` matched A's delivered
tag, and B confirmed one proof TX. The exact 2 MiB B-store readback has SHA-256
`c75ab2a01b3266fda1e07e0271c70bb29c06e32636d70d8a70d977b9e8b0e21e`
and contains one record for message
`abdeec2e498f09c96a6fd56ec3558ca86c2598aaeacac81969b645de3b549dc3`
whose full-wire digest
`1c1839991401e01e15e3a3146cd3177a4fb7e5dbd52008fd119beaf091d377ba`
matches the generator. Both checkpoint pairs report zero failed allocations,
unexpected runtime errors, RX/CAD/TX watchdog expiries, and correlation faults.
This narrowly power-confirms persistent continuous RX for that split packet and
the durable-LXMF proof chain; it does not close this ADR's direction-balanced
raw-RNS plan or broader sustained/fault qualification.

These values are workload-specific instrumented observations. The HIL enables
allocator callbacks, updates atomic evidence, scans roughly 170 KiB of stack
every second, and is captured by a target-halting debugger. Baseline captures
were followed by resets before traffic, but the instrumentation itself still
perturbs scheduling. No sustained, forwarded, multi-hop, concurrent-store,
low-memory, or allocation-failure workload ran. Heap values cover the registered
global allocator, not static, DMA, interrupt, or future-client memory. The stack
watermark reports the deepest painted word observed modified, not a historical
minimum stack pointer; static frame evidence and interrupt/nesting headroom
remain required. This HIL qualifies only the current LoRa actor and does not
change or prove the transport-neutral projection across heterogeneous links.
After capture, exact 3 MiB all-erased readbacks, one-shot journal provisioning,
protected-range comparisons, exact default-image readbacks, and authenticated
empty-inbox status returned both boards to the ordinary feature-free image; the
full hashes are retained in the E290 runbook.

## Consequences

- The first inbound persistence proof exercises the real transport-neutral
  event and sole-flash-coordinator boundaries instead of a LoRa-specific or
  volatile shortcut.
- A valid Reticulum proof can precede or exist without an application-store
  commit. Clients and tests must not report a message as durably inboxed from
  proof evidence alone.
- The format and host fault model restore an exact committed record after
  reboot or a post-commit power cut. Powered evidence now covers the four
  selected cold-mount faults and one deterministic same-boot missing-commit
  admission. Actual electrical cuts and target-bounds confirmation remain exit
  criteria. Capacity one and the absence of acknowledgement or reclamation make
  it deliberately unsuitable for normal messaging. Repeating destructive
  qualification requires an explicit developer erase/reflash.
- Reserving 2 MiB for a 576-byte record wastes space in format 1 but avoids
  repartitioning before the real message store is designed.
- One occupied item is never displaced by traffic bursts. Newest-drop behavior
  is deterministic and RAM-bounded, at the cost of losing all later messages.
- A corrupt or interrupted inbox disables only local inbox service. One direct
  DATA/decrypt/proof exchange per tested fault remained available; sustained or
  forwarded routing has not been qualified by these runs.
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

As of 2026-07-20, criteria 1 through 8 have their bounded host, target, or
powered evidence. Criterion 9 has powered evidence for the four selected
cold-mount cases and one same-boot terminal-commit suppression, including one
direct DATA/proof exchange in each case; its broader ordinary/sustained routing
claim remains open. Criterion 10 now has bounded evidence for image/static size,
cold journal and inbox mount, one maximum-payload commit on each board,
registered-heap behavior, CPU0/main-executor stack watermarking, actor watchdog
counters, and LoRa scheduling impact. It remains open because the workload was
neither sustained nor forwarded, the reverse sender ended in a product-level
delivery timeout despite receiver commit, and the opt-in instrumentation
perturbs the measured scheduler. The stack watermark also requires static frame
and interrupt/nesting headroom. No RX, CAD, or TX actor watchdog fired in the
bounded run, but that does not close the broader deadline requirement. Physical
power interruption at claim, body/digest, and commit boundaries is also still
required even though the exhaustive host fault model and deterministic target
suppression pass.

Passing these criteria authorizes work on the real LXMF queue design. It does
not promote physical format 1, HMAC-only USB, plaintext storage, or the shared
authenticated-principal policy into production requirements.
