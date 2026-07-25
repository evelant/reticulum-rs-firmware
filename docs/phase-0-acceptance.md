# Phase-0 Rete foundation validation contract

**Status:** Rete adopted provisionally; initial released-Python corpus implemented
**Date:** 2026-07-14

Per [ADR 0002](adr/0002-rete-provisional-foundation.md), Rete is the
provisional RNS foundation and Phase 0 validates and hardens it while product
integration begins. This is not unconditional production acceptance: Rete is
not accepted merely because it compiles or passes its own tests. Leviculum
remains an independent protocol oracle and fallback, not an equally deep
parallel implementation requirement.

## Pinned compatibility peers

The machine-readable revisions live in `interop/peers.toml`.

| Lane | Reticulum | LXMF | NomadNet | Purpose |
| --- | --- | --- | --- | --- |
| Released | 1.3.8 (`dca2a928…`) | 0.9.6 (`84997290…`) | 1.2.0 (`475c0ee2…`) | Release compatibility gate |
| Forward | `de0f399a…` | `fab12ad9…` | `ad103015…` | Detect current-main behavior changes |

Rete compatibility is based primarily on Reticulum 1.3.8. Its existing
interoperability corpus must be regenerated and rerun against that peer; older
passing Rete vectors are supporting evidence only. LXMF fixtures are generated
in Phase 0 to ensure the RNS integration can support correct Links/Resources
and arbitrary LXMF bytes. NomadNet is a fixture/version lane only at this
phase.

The firmware dependency is currently pinned to integration-fork commit
`dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08` on fork branch
`codex/responder-handshake-reclaim`. It descends from
`ba73ee426a3211951f5abb400c5728dd359272be`,
`354b8757bea63b9d1e27dec14f109fe6c7e03c5a`,
`338251b285a2447beb10d390d3e7f53694a1a916`, and
`a443173b0829c2637ce23531a8cde15fdfec185e`, then from
`2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` on
`codex/link-data-receipts`, which descends from
`90570cafc812b3025011cb690ec74a27f287cb3f`, whose tag is
`firmware-pin-90570ca`; that predecessor tag does not name the current
revision, which has no designated durable tag. In the direct-request lineage,
`338251b` adds bounded canonical MessagePack values including anonymous `nil`;
`354b875` keeps prepared requests cancelable and starts their response timeout
only at exact first dispatch; `ba73ee4` preserves validated inbound encoded
request values and their original wire timestamp; and `dfcaa36` adds
phase-agnostic exact request-dispatch reclaim keyed by request and Link IDs that
reports prepared versus confirmed native residue. The `a443173` predecessor adds
responder-Handshake timeout reclamation to the preceding descendant's ordinary
Link-DATA receipts and receiving-destination proof policy. These request
primitives do not claim full NomadNet or Resource support. No issue or pull
request was opened for this newer fork-local work; any
future upstream issue or contribution still requires direct user approval.

The preceding `14c7b4955a1ff6903e87cc40b42498f7869b6f4f` pin had host and
portable-target LRRTT validation and a build-only E290 package. Its 776,464-byte
merged image uses 710,928/6,291,456 application bytes (11.30%) and has SHA-256
`7b11c6f6a3c039d46ab0117fd362920aaa40145e7f27cbc6fa0a8a84a7ab3571`.
It has no flashed-image readback or powered proof. The subsequent pre-PSRAM
application-event release still required a two-board powered run before lifecycle/RF
qualification. Its default E290 release links with text/data/BSS of
684,167/3,676/469,152 bytes (1,156,995 bytes total by GNU size). Its
12,345,320-byte ELF has SHA-256
`ebb34e7176a8e61b6969ebf99d7dac97c6e674ef5e583bbf931a34e8b6e970a2`.
The 789,504-byte merged image uses 723,968/6,291,456 application bytes (11.51%)
and has SHA-256
`1796f161c480d0348e3d47fd8f3cda5fda5b51aa38ad6024aaad04c8ba1751ce`.
That default image matched an exact `3e:88` readback and served an authenticated
`identity-summary`; `3f:88` did not enumerate. The initial pre-PSRAM
runtime-measurement HIL build packages as 800,480 bytes with application use
734,944/6,291,456 (11.68%); its 12,498,356-byte ELF and merged image have
SHA-256 values
`4ca4eef73ff1babd00750d4a635f7644d73d1a3ae1cde4fb1dbdb434937bcfca` and
`ec23bf0a7b20b7364e12cba6ebc90aa3e0ce761650413e1ad9d6186eeecf1662`.
A target-scoped rebuild retained the same linked section/application sizes;
its 12,498,348-byte ELF and 800,480-byte package have SHA-256 values
`c84363dff0801a1679dd786b5070c4662962d299f0269efc0cd72ff9c09b8e2a` and
`058a969e0b9e099f6a5febd1b59f4a70cfd3ea932e8f0738a2ddb4b3e5569119`.
That package matched an exact `3e:88` readback. Its authenticated 108,940-ms
checkpoint retained 63,828 painted stack bytes (10,148 after the unchanged
maximum-frame deduction), bounded heap/API work, two confirmed transmissions,
and no unexpected error, failed allocation, watchdog timeout, or correlation
fault. The board was then
restored to an exact-readback 789,504-byte default rebuild, SHA-256
`a67afa72681558dc02fd0575a18711b2b3c05b365a66af45441b7cb8dd3a2577`,
and served `identity-summary`. Those are historical pre-PSRAM artifact and
checkpoint records, not the current Stage 5 image. The later 868,800-byte
post-offload placement checkpoint is itself historical pre-LXTE evidence. The
immediately following 870,656-byte LXTE/checkpoint-v2 HIL matched exact
identity-bound readbacks on both E290s, but its back-to-back primary/LXMF
announce batches reproduced a half-duplex discovery failure: B processed three
distinct A announces while submission to A's LXMF destination still returned
`no-path`. The current scheduler emits at most one destination per event,
separates primary and LXMF by eight seconds, applies two identity-phased retry
cycles, and then uses the 30-minute steady cadence. Its 871,296-byte HIL package
uses 805,760 application bytes and has SHA-256
`89d303d4880d062068bf8a9f4124bfbea322af091e5bf37c04e4e52715481cbd`
and matched exact identity-bound readbacks on both E290s. Both USB devices
disappeared before that image's required post-flash pre-submit checkpoint, so
that historical attempt has no durable-LXMF outcome.

The final current default/HIL pair contains 946/962 stack-size records, a
53,680-byte maximum frame, and 175,056/174,256-byte usable stacks. The
13,648,888-byte default ELF has SHA-256
`92e63b60a5f4b830ee55d958fcc446a6878036212904b8748519ae210ba3da58`;
its 868,656-byte package uses 803,120 application bytes and has SHA-256
`c8da2af30e2d0ee24ca4b215151d1370b7e1d242991ebbeb024079a730693a3f`.
The 13,821,496-byte HIL ELF has SHA-256
`7a3fad34699f910a2050468ada6461a0f33d16641ab5425a5c795a71238861ff`;
its 881,456-byte package uses 815,920 application bytes and has SHA-256
`12c6f31a7fb64485ad9220edca4ac38ba0a57867ad88ce60fa1a24ffc195d379`.
Both E290s matched exact identity-bound HIL readbacks. Exactly one fresh A-to-B
trial then submitted a 206-byte opportunistic LXMF carrier as an exact 307-byte
RNS packet, SHA-256
`060037041c91eb5999f89bf84845c19e65bf7fa680827cce9c51e8ecc5dbe0a6`,
and reached `Delivered` on its first attempt. Receiver B advanced durable-new,
proof-ready, proof-released, and ordinary-handoff by one, with zero already-
durable and ordering-violation events. Its `LXTE` release tag
`0x3dc4588d3a205429` matched A's delivered tag, and B recorded exactly one
confirmed proof-TX delta. B's `RPTE` generated-proof field correctly remained
zero because retained LXMF proof ownership is intercepted before ordinary RNS
ingress metadata. The committed message ID is
`abdeec2e498f09c96a6fd56ec3558ca86c2598aaeacac81969b645de3b549dc3`.
The exact 2 MiB store readback, SHA-256
`c75ab2a01b3266fda1e07e0271c70bb29c06e32636d70d8a70d977b9e8b0e21e`,
contains one record matching the generator and full-wire digest
`1c1839991401e01e15e3a3146cd3177a4fb7e5dbd52008fd119beaf091d377ba`.
Baseline/terminal checkpoints contain zero allocation failures, unexpected
runtime errors, RX/CAD/TX watchdog expiries, and correlation faults. This
narrowly confirms persistent continuous RX across the split packet and the
durability-before-proof chain; direction balance, replay/remount, pressure,
fault, range, and soak remain open. The release gate conservatively carries
57,700 stack bytes and a 4,020-byte post-frame margin. Artifact bindings and
the trial are recorded in the
[E290 runbook](e290-node.md#stage-5-psram-boot-checkpoint).
Every powered result below remains bound to the project and Rete revisions
recorded with it.

## Scaffold gate

The scaffold is complete only when all of these pass from a clean checkout:

```sh
cargo test --locked
cargo run --locked -p xtask -- graph-policy
cargo run --locked -p reticulum-conformance-rete
cargo test --locked -p reticulum-rns-leviculum
cargo test --locked -p reticulum-lxmf-model
cargo test --locked -p reticulum-lxmf-store
cargo test --locked -p reticulum-lxmf-durable-ingress
cargo test --locked -p reticulum-lxmf-ingress
cargo test --locked -p reticulum-lxmf-wire
cargo clippy --locked \
  -p reticulum-lxmf-model \
  -p reticulum-lxmf-store \
  -p reticulum-lxmf-durable-ingress \
  --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked \
  -p reticulum-lxmf-model \
  -p reticulum-lxmf-store \
  -p reticulum-lxmf-durable-ingress \
  --no-deps
cargo check --locked \
  -p reticulum-device-api \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  -p reticulum-lxmf-durable-ingress \
  -p reticulum-lxmf-ingress \
  -p reticulum-lxmf-model \
  -p reticulum-lxmf-store \
  -p reticulum-lxmf-wire \
  -p reticulum-node-core \
  -p reticulum-storage-model \
  -p reticulum-submission-projector \
  -p reticulum-tx-handoff \
  -p reticulum-tx-dispatch \
  -p reticulum-tx-supervisor \
  -p reticulum-rns-conformance \
  -p reticulum-rns-rete \
  -p reticulum-rns-rete-rx \
  -p reticulum-rns-leviculum \
  -p reticulum-radio-interface \
  -p reticulum-board-heltec-vision-master-e290 \
  -p reticulum-board-heltec-tracker-v2 \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked \
  -p reticulum-device-api \
  -p reticulum-device-api-credential-store \
  -p reticulum-device-api-credentials \
  -p reticulum-device-api-framing \
  -p reticulum-device-api-pairing-control \
  -p reticulum-device-api-pairing \
  -p reticulum-device-api-handoff \
  -p reticulum-device-api-session \
  -p reticulum-lxmf-durable-ingress \
  -p reticulum-lxmf-ingress \
  -p reticulum-lxmf-model \
  -p reticulum-lxmf-store \
  -p reticulum-lxmf-wire \
  -p reticulum-node-core \
  -p reticulum-board-heltec-vision-master-e290 \
  -p reticulum-storage-model \
  -p reticulum-submission-projector \
  -p reticulum-tx-handoff \
  -p reticulum-tx-dispatch \
  -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --target xtensa-esp32s3-none-elf
```

The three Xtensa commands require the environment file generated by `espup` to
be sourced so the Xtensa GCC linker is on `PATH`.

Regenerating the released-Python corpora additionally requires CPython 3.13.7
and the pinned RNS/LXMF revisions. CI installs the Reticulum requirements for
the RNS runner and installs the LXMF requirements into an isolated target
directory before running:

```sh
PYTHON=python cargo run --locked -p xtask -- check-rns-vectors
LXMF_PYTHON="$(mktemp -d)"
python -m pip install --target "$LXMF_PYTHON" \
  -r interop/python/requirements-lxmf-1.0.1.txt
PYTHONPATH="$LXMF_PYTHON" \
  python interop/python/generate_lxmf_1_0_1_vectors.py --check
PYTHONPATH="$LXMF_PYTHON" \
  python interop/python/test_lxmf_1_0_1_vectors.py
```

The linked Tracker image must contain no radio initialization and no path that
can issue an SX1262 `SetTx` command.

### Initial compile-probe baseline

The initial 2026-07-14 release link at upstream Rete revision
`9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743` produced a GNU size summary of
57,861 bytes of text, 4,076 bytes of data and 468,748 bytes of BSS/reserved
address space. That last number is not live application state: the ELF map
includes a 327,380-byte stack reservation, 65,536 bytes of reclaimed DRAM
reserved for the heap, and linker gap sections. The named `.bss` section itself
is 132 bytes. On-device free-heap and stack high-water measurements remain
authoritative once executable scenarios exist.

After adopting integration-fork revision `05de2c2b2eda71e9ba6fc64d1f4d7a6f5ec320de`,
the same safe-idle release probe produced 57,869 bytes of text, 4,044 bytes of
data and 468,780 bytes of BSS/reserved address space. The named `.bss` section
remained 132 bytes; the map contained a 327,412-byte stack reservation and the
same 65,536-byte reclaimed-DRAM heap reservation. Rete runtime paths are still
dead-stripped from this probe, so this confirms dependency/link integrity, not
operational peak memory.

After advancing to combined integration revision
`beb84c370d2ae27209a866093fa1e6b204304384` on 2026-07-15, the safe-idle probe
remained byte-for-byte identical in the GNU size summary: 57,869 bytes of text,
4,044 bytes of data and 468,780 bytes of BSS/reserved address space, including
the same 132-byte named `.bss`, 327,412-byte stack reservation and 65,536-byte
reclaimed-DRAM heap reservation. No `lora` or `sx126` defined symbol was
retained. The first two Rete fixes are still dead-stripped until the runtime
vertical slice constructs the node.

The receive-only integration tests then exposed endpoint announce rebroadcast
as a released-Python compatibility error. Integration revision
`5ce8c4e437d3f2f07d302bc366ff06bacd6aff2d` gates that queue on transport mode;
the safe-idle size statement above remains the pre-runtime baseline because the
new Rete path is likewise dead-stripped from that image.

An initial schematic reading incorrectly identified GPIO46 as an MCU connection
to the DIO2-owned `PA_CPS` net, so the safe-idle image temporarily claimed it as
an input. That historical build contained 57,997 bytes of text (a reviewed
128-byte delta), while data remained 4,044 bytes and BSS/reserved address space
remained 468,780 bytes. A later rendered-PDF and hidden-netlist audit corrected
the premise: `PA_CPS` connects SX1262 DIO2 directly to KCT8103L CPS, while
GPIO46 is a separate header breakout. The unnecessary owner was subsequently
removed; the historical measurement is retained here rather than rewritten.
The linked image still had no defined text/data symbol matching LoRa, SX126x or
SetTx; unrelated absolute ESP ROM symbol declarations are not reachable radio
code.

No `lora-phy` or SX126x driver symbols were retained in the linked safe-idle
ELF. The ESP ROM symbol map still exposes unrelated Wi-Fi/Bluetooth TX symbol
names; those are absolute ROM declarations, not reachable radio setup in this
binary.

The compile graph currently contains `embedded-io` 0.6 and 0.7, plus multiple
`embassy-sync` compatibility generations pulled by the ESP stack. This does not
prevent the probe from linking, but Phase 1 must keep version-specific types
behind target adapters and should converge versions when upstream releases
permit it. New portable APIs must not expose those concrete generations.

### Initial released-Python conformance baseline

The committed `interop/vectors/rns-1.3.8.json` corpus is generated by the
exact released peer pinned in `interop/peers.toml`. The previously validated
host runner performed 235 checks: 112 provenance, identity, signed-announce,
packet-parse,
payload, packet-hash, owning-adapter/native-core and direct-Link checks, 40
released-Python LRRTT MessagePack checks, 8 channel-retry lifecycle checks, 40
exact keepalive lifecycle checks, and 35 deterministic three-node A--B--C
relayed-Link checks. Rete
reproduces the Python announce byte-for-byte and cryptographically validates
it. The runner also
strips the announce's RNode LoRa header through the bounded receive boundary,
delivers the Python plain packet as a native `DataReceived` event, relays the
Python announce through two deterministic nodes, learns its HEADER_2 path and
forwards native encrypted DATA through that path to a node loaded with the
pinned Python identity, where it is decrypted into a native `DataReceived`
event. The owning adapter additionally completes a deterministic direct Link
handshake between two Rust nodes, resolves LRPROOF/LRRTT transmission to the
source interface, exchanges encrypted Link data, and completes a reliable
channel message/proof receipt. Its three-node flow learns C through transport
B, relays LINKREQUEST/LRPROOF/LRRTT over B's two exact local interfaces,
rejects a wrong-hop LRPROOF before deduplication, exchanges encrypted channel
DATA and its proof, and checks endpoint delivery plus owned/relay/reverse
capacity. Malformed packets, oversized announce data,
signature tampering and full-table admission have focused Rust tests.

This clears the first packet/identity, deterministic direct-Link/forwarding and
project-owned three-node relayed-Link/channel slices. Python-originated
encrypted-packet decryption by Rete, Python-to-Rete Link interoperability,
Python relayed-Link/multi-hop behavior, ordinary and automatic Link
close/timeout beyond authenticated-malformed LRRTT teardown, requests and
responses, proofs outside the exercised channel flow, IFAC, ratchets, live
process interoperability, full routing behavior, Resources and multi-node
loss/reordering scenarios remain open gates below.

Separately, the preceding `8b5d652` Rete selected validation set passed 635
tests: 271
transport (174 library plus 97 integration: 9 computed-vector, 43 forwarding,
40 Link-integration and 5 path-request), 137 stack (136 library and one
integration), 143 LXMF library and 84 daemon library tests. The four
library targets totaled 537 tests; the 97 transport and one stack integration
tests were also run for that pin to reach 635. This is not a count of every
nested workspace test target.
These regressions complement, but do not replace, the project's historical
235-check conformance baseline or powered/live-Python multi-hop qualification.
The recorded schema-2 lifecycle/candidate runner passed 647 checks. That is
historical evidence, not an asserted current schema-3 count.

### Initial capacity audit

The initial heapless profile is explicitly `P=64` paths/reverse entries,
`A=16` pending announces, `D=128` entries per deduplication deque and `L=4`
entries in each independently counted owned- and relay-Link table.
Focused tests prove that paths evict the least-recently-used entry, packet
deduplication is a rolling FIFO window, and a full announce queue rejects the
next entry without growing. The owning `EmbeddedNode` makes the outbound
admission guard mandatory, rejects a fifth owned link before returning a
packet, applies the same preflight to new inbound Links, and verifies retained
state before releasing LRPROOF or an event. The pinned Rete integration
revision now also returns `SendError::LinkTableFull` before releasing an
outbound request. Stack ingress exposes native typed rejections for
`LinkTableFull` with `LinkTableKind::Owned` or `LinkTableKind::Relay`, plus
`ReverseTableFull` and `ReverseRouteConflict`; the adapter maps each to a
stable product disposition. A failed new relay Link or reverse route emits no
forwarding packet and leaves Link/reverse/raw/dedup state unchanged except for
the deliberate replay-filter record, so an exact retry is a duplicate while a
fresh packet can proceed after capacity is released.

At the current pin, owned HEADER_2 DATA, LINKREQUEST, Link and proof/receipt
traffic reaches normal local dispatch. Transported HEADER_2 DATA/SINGLE and
LINKREQUEST/SINGLE require an exact path and admit reverse or relay-Link state
transactionally before forwarding. A path-selected DATA packet returns an
exact-interface outcome; the adapter maps it to `Only(interface)`, including an
intentional return to the ingress slot for a shared-medium relay. Reverse state
records both ingress and outbound slots. A reverse proof is consumed on its
first attempt and routes back only when it arrives from the recorded outbound
slot; a wrong-slot proof fails closed and cannot be replayed on the right slot.
Link DATA and non-LRPROOF Link PROOF require stored direction and exact hops.
LRPROOF additionally requires the responder-side interface, stored outbound
hops, a known responder identity, successful reconstruction and a valid
signature before routing or lifetime refresh. A targeted HEADER_2 LRPROOF is
normalized into those checks rather than bypassing them through generic Link
transport. H2 LINKREQUEST records only Link-route state, not a redundant
reverse entry. Foreign non-ANNOUNCE H2 packets are filtered before state,
statistics, dedup or raw-byte mutation; H2 ANNOUNCE remains in ordinary announce
validation. Arbitrary remote H1 LINKREQUEST remains disabled until interface
roles distinguish it from local-origin injection, while the temporary H1 DATA
compatibility path explicitly guards reverse capacity and truncated-key route
conflicts.

Owned-Link interface binding is now explicit and authenticated. A responder
binds to the LINKREQUEST ingress slot; an initiator stays unbound after the
request and binds only from a valid LRPROOF ingress. Active application and
maintenance output carries `BoundInterface`, which the adapter maps to the
exact physical interface. Within this lifecycle, only the initial LINKREQUEST
may broadcast, and only if its learned path has no recorded interface. Link
DATA and `RESOURCE_PRF` from another interface are rejected before dedup, so a
later correct-interface copy remains admissible.

Pending-Link expected hops now have explicit parity coverage. An initiator
snapshots the known path's hops when it creates the Link; if no path is known,
it stores the `PATHFINDER_M = 128` wildcard. LRPROOF hop mismatch is rejected
before deduplication or Link-state mutation. A responder begins without an
expected hop and records the post-ingress hop only after LRRTT authentication
and decryption. Pending-handshake LRRTT payload parity is now covered. Rete
emits canonical MessagePack float64, accepts the numeric scalar families
returned by Python's u-msgpack, consumes the first object while allowing
trailing bytes, and selects the greater local or peer RTT with Python ordering.
Rete now retains an immutable request anchor, uses microsecond
`MonotonicInstant`/`MonotonicDuration`, and stores RTT as binary64. Opaque,
non-repeating eight-byte tokens correlate LINKREQUEST and LRPROOF output with
the first successful interface confirmation. The initiator uses the confirmed
egress interval's start and the responder its completion. The firmware confirms
at ordinary-router/interface acceptance, not physical LoRa RF `TxDone`.

Fresh authenticated LRRTT is handled in `Handshake`, `Active`, and `Stale`.
Initial activation emits establishment once; later updates/reactivation emit
`LinkRttUpdated` and refresh timing, hop, and keepalive state without duplicate
establishment statistics. Exact raw replay is deduplicated. Authenticated
malformed LRRTT tears down all three states, while `links_failed` changes only
for a Handshake failure. Zero RTT remains zero with 5-second keepalive and
10-second stale floors; nonzero RTT uses `4 * RTT + 5 seconds` stale grace.
Rete intentionally authenticates before liveness mutation, so a corrupt stale
LRRTT cannot revive a Link as it does under Python's pre-decrypt ordering.

A responder that never authenticates LRRTT now closes and releases its owned
slot at 360 seconds plus six seconds per post-ingress hop, with a minimum of
one hop. Confirmed LRPROOF completion is the preferred origin and admission is
the bounded fallback. This lifecycle closure changes aggregate `links_closed`,
not `links_failed`; initiator expiry remains the product-owned exact-abort
boundary.

The adapter supplies precise `*_at` ingress/tick samples and confirms at the
transport-neutral ordinary router. Upstream Tokio/Embassy runners remain
coarse/unconfirmed. Rete uses one pre-decrypt ingress sample over its bounded
synchronous handler; Python takes three internal method samples.

The binding is only an interface-slot index. A shared Tokio `Hub` retains a
source client for synchronous output, but asynchronous owned-Link output still
broadcasts to siblings until endpoint-aware client identity and reconnect
generation are part of Link state. The pin now emits and accepts exact
unencrypted 20-byte keepalives: `0xff` probes originate only from the initiator
after both a full inbound-silence interval and a full prior-probe interval, and
the responder alone returns `0xfe`. Valid deterministic repeats bypass dedup
only after bound-interface validation; the lifecycle
consumes them without application events and preflights the exact route before
committing an automatic probe. Stale starts after two intervals, with a
`4 * RTT + 5 seconds` revival window measured from the actual transition/final
probe (five seconds when RTT is zero); valid bound Link traffic also revives
the Link. Channel sends and retries are
transactional across MDU/window/receipt admission, entropy, route preflight,
fresh ciphertext, sole-receipt replacement, and retry/window/timestamp state.
An obsolete proof cannot complete after replacement; Link removal reclaims its
channel receipts. Established-Link watchdog timeout removal still emits no
`LINKCLOSE`.
Receipt capacity smaller than an adaptive channel window is now typed
backpressure and a product sizing/throughput policy rather than a
proof-correlation defect.

The same audit found that identity, resource, announce-replay, announce-rate,
path-request-throttle and packet-dedup occupancy are not all observable. Owned
and relay Links plus reverse entries now have separate read-only occupancy.
In particular, `announce_rate` and `path_request_times` are
separate `P`-sized maps whose insertions can fail silently; a failed
path-request timestamp insert
can bypass throttling for a new destination. Packet `dedup` and announce-replay
`announce_dedup` are separate `D`-sized rolling deques, and their occupancy is
also not exposed. Channel send/retry receipt admission and replacement,
destination-DATA receipt, relay-Link, and H2 reverse insertion are no longer in
the unaudited set at the current pin. Several other internal insertions still
need a complete transactional audit. This is
provisional evidence, not production acceptance: remaining transactional
capacity errors, drop metrics, complete occupancy APIs and bounded NodeCore
event/output storage remain upstream repair candidates and hard gates below.

### Initial owning embedded boundary

Firmware no longer receives a public raw Rete `NodeCore` alias. The adapter's
`EmbeddedNode` privately owns Rete state and remains the protocol construction
boundary. The newer `reticulum-node-core::NodeCore` owns an `EmbeddedNode`,
fixed external-buffer dispatch metadata and the DATA attempt ledger; each
500-byte outbound `TxPacketBuffer` remains caller-owned and is registered once.
The current receive-only firmware still uses its narrower opaque façade.
Together these boundaries currently enforce:

- a 500-byte ingress ceiling before Rete's hosted 300-KiB allocation path;
- project-owned endpoint/transport roles and an additional-destination quota;
- exact SINGLE/context-zero/64-or-67-byte LINKREQUEST forms plus
  `accepts_links` policy;
- mandatory owned outbound and inbound Link capacity admission;
- DATA, Link and channel payload limits before native path/channel mutation;
- receipt and channel-receipt preflight before native send-state mutation;
- Resource-context rejection until bounded Resource storage exists;
- Python-compatible foreign HEADER_2 ownership filtering before mutation,
  normal owned-H2 local dispatch, transactional exact-path H2 DATA/LINKREQUEST
  relay admission, and typed owned/relay-Link and reverse capacity/conflict
  dispositions;
- fail-closed remote H1 LINKREQUEST admission and a guarded H1 DATA reverse
  compatibility shim until stable interface roles distinguish remote ingress
  from local-origin injection;
- suppression of forwarding actions in endpoint profiles;
- resolved native `SourceInterface`, `ExactInterface(interface)` and
  `AllExceptSource` actions as project `Only(source)`, `Only(interface)` and
  `AllExcept(source)` while synchronous ingress is still known; an exact
  same-source result remains exact rather than being suppressed as an echo;
- pre-entropy dispatch/attempt reservation before caller-owned DATA
  preparation, with failure returning the exact same external buffer;
- rejection of `PrepareDataRequest` when `deadline <= owner_now` before
  reservation, entropy use, or RNS mutation;
- deterministic resolution of outbound `All`/`Only`/`AllExcept` against the
  enabled-interface snapshot and serialized, no-copy fan-out through a unique,
  byte-inaccessible routed `TxJob`;
- opaque non-`Copy` permit requests/replies binding an exact interface
  resource ID and nonzero actor-defined units, with unknown,
  mismatched/under-sized reservations denied and a covering grant irrevocably
  recording possible transmission before leaving node-core;
- one-shot packet-byte access only through an exactly matched
  `AuthorizedTx::frame(now)`, with delayed grants becoming byte-inaccessible
  `ExpiredAuthorizedTx`;
- exact-deadline authorization, completion, rollback and maintenance plus
  `NodeInstanceId`-scoped recovery records; a coherent late owner returns
  `Recovered`, while faults/invariants retain an owning quarantine;
- exact-receipt rollback only for a definitely-unsent job with no earlier
  authorized hop, cumulative receipt retention after any authorization, and
  terminal acknowledgement blocked while any typestate still owns its buffer;
- read-only exact-owner validation of reusable buffers, fixed per-slot boot
  parking, lowest-slot synchronous preparation, queued-return and retained-
  `Next` priority, no-mutation queue preflight, ordinary rejection restoration
  or fail-closed quarantine, completion reconciliation, and unchanged retry of
  every serialized `Next` job under job-channel pressure;
- exact retention of an unexpectedly rejected fresh handoff followed by
  rollback with a fresh step clock, including deadline recovery and rollback-
  failure retention;
- recovered-owner parking until exact generation-scoped record
  acknowledgement and fail-closed retention of completion validation faults;
- canonical accepted/transition/audit records, principal-scoped idempotency,
  poisoned complete replay, conservative boot recovery, and an allocation-free
  fixed-RAM submission index;
- a portable persist-before-ack projector that plans and retains the exact pre-
  preparation barrier, permits attempt binding only after the storage backend
  reports commit or exact readback, correlates complete-frame and every
  terminal/recovery/quarantine observation, and retains independent exact
  acknowledgements across retry and ordering races;
- allocation-free adapter/transport counters and capacity snapshots;
- a defensive adapter guard against any premature responder `LinkEstablished`
  event. The current native lifecycle emits establishment only when the Link
  reaches `Active`, so this guard is expected to suppress zero events.

The adapter does not claim to make all of Rete allocation-free. Native events,
packets, destinations and several Link paths still contain `Vec` allocations;
opaque native failures remain possible. The exact upstream repairs and
regression expectations are tracked in
[the Rete hardening backlog](rete-upstream-backlog.md).
The portable external-buffer route/permit/completion/recovery slice, bounded
Embassy handoff, firmware-excluded `reticulum-tx-dispatch`, and
firmware-excluded `reticulum-tx-supervisor` have focused host suites plus
generic RISC-V and ESP32-S3 checks. The dispatcher is an RF-inert persistent
packet-interface state machine with cancellation-safe short waits; its
companion permit server owns the node-side scalar exchange, and its node DATA
machine owns the job/return ports plus fixed parked-owner table. The permanent
supervisor aggregate owns those machines with node-core and an authorization
policy, samples the clock freshly before maintenance/DATA/permit/dispatcher,
waits for the exact next owner deadline or permit grace, and bounds sustained
progress to 16 passes before yielding. It now exposes the sole node owner's
proof policy, bounded announce queue/flush, registry-validated exact-owner RNS
ingress, RNS tick, and a public cancellation-safe TX-work wait; no public
supervisor method accepts a caller-selected raw interface ID. Its
`RfInertTxPolicy` denies RF. A separately constructible ordinary coordinator
now admits complete returned action envelopes into the registered static pool,
derives live eligibility from the authoritative interface router, services the
opaque authorization edge, and retains exact ticketed jobs, completions,
rejections and post-fault drain state. Its per-actor permit-only server
authorizes once, retains exact requests/replies across pressure, and continues
forced denial after coordinator fault. The separate real-radio dispatcher now
retains the router's DATA/ordinary tickets. The DATA router, both permit
services, and permanent `NodeInterfaceSupervisor` are composed with it in the
E290 three-task graph alongside the narrow pre-authentication USB/GPIO owner.
These pieces and the router's cancellation-safe capacity/completion waits pass
host and both target checks; full powered qualification of the current
permanent graph remains open.

The semantic durable model, idempotent projector, physical journal, and portable
sole storage actor are implemented and target-checked. The actor owns the live
replay index, sole projector, one optional pending mutation capped at 544 bytes,
and a fail-closed fault latch while borrowing exact operation-scoped NOR access;
it completes mount/replay before service and can autonomously reconcile an
ambiguous backend result. Narrow
actor-owned methods now project preparation, frame, terminal, recovery and
quarantine observations and exact acknowledgements without exposing mutable
projector state. Actor-owned boot recovery also commits the exact conservative
reset transition before reporting interrupted work final. The
portable authenticated device-API adapter is also implemented: default builds
serve capabilities and principal-scoped status, while the explicit target-safe
feature enables durable experimental acceptance. Both profiles are target-
checked. The permanent E290 graph keeps the journal runtime and sole flash in a
resident coordinator. Suite totals and build-only measurements are recorded
after the release and target gates; powered qualification measurements also
require the exact readback gates below.
Immediately after flash open it
validates the exact `api_credentials` partition/eFuse binding, mounts and
performs at most one retire then cleanup step, and retains any mounted credential
store without auto-provisioning. Credential failure closes only credential
admission/mutation; LoRa and the independent journal policy continue. Source
`96e38aa` then passed the first permanent-image powered smoke on both erased
E290s: exact 729,504-byte same-image readback, 8 MiB PSRAM,
`UninitializedErased` with zero recovery steps/writes/erases, all-`0xff`
credential partitions after boot, API/session/bearer closed, journal and
LoRa/interface ready, and two ordinary one-frame TXs per board. Source
`5f3f259` then passed an exact 736,144-byte two-board upgrade/readback and
counted reboot smoke with the resident pairing policy present,
`Eligible { media: ExactlyErased }` initialization status, continuing ordinary
LoRa TX, and both credential partitions still entirely `0xff`. No request lane
invoked initialization in that historical source. The current image composes
status/initialize and Begin/ProofStart/Activate/AbortCurrent through one USB
Serial/JTAG byte owner, one shared decoder and exact-next sequence gate, and
separate depth-one owning handoffs. The node-owned causal frontier orders scalar
policy observations with secret-bearing live requests and withholds mutation
success until the correlated durable terminal result. Stable-time active-low
GPIO21 debounce supplies physical presence. An 8 ms missed-SOF interval
suspends without changing the epoch or sequence; later SOF resumes it. Those
pre-authentication records do not expose the logical API; the current source
serves that separately through the minimal authenticated session bearer.
Button/control arbitration is bounded, stable High is latched before later Low,
and a raw-sample gap of at least 20 ms cancels a possible hold until a fresh
debounced High. A response owner is released only after all bytes enter the
endpoint FIFO and `WR_DONE` is requested; later responses backpressure on FIFO
capacity. Runtime bus reset blocks the epoch, removes the pull-up, scrubs USB
RAM, and permits service only after a detectable reattachment and clean reset.
Each fresh connection resets the publication latch and debouncer to Low,
preventing release evidence retained for an older epoch from arming the new
epoch and requiring a complete fresh High debounce.

Strict host/target, graph, release-link, host-client, size-cap, and final
same-image readback results are recorded in the E290 runbook. For historical
context, the preceding 652,992-byte USB-control image with SHA-256
`1727a14b58a076d65ea12feb61b564d5dfc66d6c6f0b9a8ddd39fc773332705c` was flashed
to both boards. Both returned
`initialization-required` and `physical-presence-required`. No-button,
single-open workflows on both boards advanced through sequences 0--47 before
their five-second overall deadlines, proving bounded multi-request liveness
without opening the presence window. Subsequent 8 KiB credential-partition
readbacks on both boards were entirely `0xff` with SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`,
confirming zero writes. That historical run did not attempt a successful hold,
write, or post-write readback.
The preceding boot-quarantined 701,744-byte image with SHA-256
`14d9fd6dd482c47baa9afd2fda6a5ba1d69f46785bf23ae29f6b9fe561e4b212`
then matched exact address-zero readbacks from both boards. Each board
reattached and served sequence-zero `initialization-required` after the hard
reset induced by its readback. Simultaneous 120-second no-button initialization
workflows remained responsive through sequences 1102 and 1100, respectively,
and both exact credential-partition reads remained entirely `0xff`. This is
hard-reset service-recovery through the application boot-quarantine path,
liveness, and zero-mutation evidence. With no secret response in flight it does
not independently prove USB FIFO/RAM secret erasure or non-replay, successful
credential initialization, or anything about the preceding ROM/bootloader
interval.
The last powered 718,688-byte authenticated-node-foundation image with SHA-256
`e20f6191cb2bfa78fbd7f3d588eb418913da3f1f89e3b80a4db0a28abaf414ea`
also matched exact address-zero readbacks from both boards. Both returned and
then recovered sequence-zero `initialization-required`; both credential
partitions remained entirely `0xff`. The authenticated bearer endpoint stayed
dormant in that exact image, so this is only a regression of the existing
bootstrap/reset path and does not qualify the subsequently composed minimal
bearer.
The host asserts DTR and clears RTS. TTY reopen does not start a new epoch; only
USB bus reset does. A powered macOS `USBDeviceReEnumerate` replaced the service
and restored sequence zero after firmware detachment/scrub/reattachment. A non-
seizing `ResetDevice` returned success but left the same endpoint stale and is
not an accepted recovery primitive. Status defaults to 15 seconds and the
physical-presence workflows to 120 seconds;
a post-send I/O failure or request timeout leaves the last sequence
consumed-or-ambiguous, and
`u64::MAX` is refused rather than wrapped. The current image selects no-op
firmware logging, so native USB logs are unavailable as boot evidence and cannot
interleave with the COBS control stream. USB suspend/resume, controlled power
cuts, and the ROM/bootloader interval before the earliest Rust entrypoint remain
open. A dedicated
RF-inert Tracker storage HIL image is target-
checked and its isolated clean-path/software-reset powered run passed on board
E9:44 from source `7b47113`, with strict serial and independent raw-partition
verification preserved at
`artifacts/storage-hil/20260716T211318Z-e944-7b47113`. It proves A1 format,
five appends, mutation-free retry/conflict, B2 compaction, and zero-mutation B2
replay after software reset. That image calls the journal directly and does not
qualify the actor on hardware.

The readback-qualified API 1.1 permanent image completed the next bounded
product slice. Its 751,712-byte merged image (SHA-256
`4285fcaa9df6a6f0314ed4735377ea986b0efcafafc2710ad7594489a49b4795`)
matched exact address-zero readbacks on both E290s. The sender retained its
powered initialization/pairing/Active state, authenticated over USB, exposed
its public primary destination, and durably accepted one submission. The second
permanent node decrypted the matching LoRa DATA and returned a valid Reticulum
proof; the sender durably reached `Delivered`, and a fresh authenticated status
request after full USB re-enumeration returned the same terminal metadata. This
qualifies the bounded authenticated external-bearer-to-peer-proof path, not
application inbox consumption or the full Rete production gates below.

Controlled power cuts, endurance/soak, at-rest encryption, remaining credential
lifecycle paths, full qualification of the current permanent owner graph, and
runtime flash/watchdog/OTA/radio coordination remain open. The current evidence
does not claim initialized-media interruption recovery, application-level
message consumption, multi-hop routing, or session resumption. On-device stack
high-water and scenario heap measurements also remain required.

## Rete production hard gates

Rete is not production-accepted until every hard gate passes. Implementation
may proceed through validated portions of the stack, but a failing or missing
gate keeps the affected network-facing capability disabled or strictly capped.
It does not remove that capability from the product requirements.

The preceding validated pin has focused host regressions for exact
path/reverse/Link routing, wrong-interface one-shot reverse proofs,
authenticated LRPROOF,
same-interface dispatch, owned H2 local termination, foreign-H2 filtering and
typed transactional H2 relay/reverse admission. It also covers authenticated
owned-Link interface binding and pre-dedup rejection of wrong-interface DATA
and `RESOURCE_PRF`, plus pending-Link expected-hop snapshot/wildcard behavior
and pre-dedup wrong-hop LRPROOF rejection. Its 235-check project runner also
completed a deterministic three-node A--B--C Link, channel DATA and proof
flow. Those tests close the former implicit-interface and relay/reverse
admission seams in the covered native H2 paths; they do not close Python
multi-hop behavior, H1 interface-role classification, reboot path recovery, or
a powered rerun of the E290 reverse-proof scenario captured under `f6f5fb0`.
The `90570ca` predecessor adds the LRRTT lifecycle/timing contract. Its
`2d07818` descendant additionally adds ordinary Link-DATA receipts and
destination proof-policy parity. Its `a443173` descendant adds bounded
responder-Handshake reclamation. The current `dfcaa36` descendant contains the
ordered `338251b` canonical-value, `354b875` first-dispatch-timeout, and
`ba73ee4` lossless encoded-value changes, then adds phase-agnostic exact
request-dispatch reclaim keyed by request and Link IDs with
prepared-versus-confirmed native-phase reporting. Its current root, portable,
strict E290
Clippy, and release-build gates pass. A
normal powered message exchange on the current pin does not by itself
demonstrate responder-side establishment expiry, request/response
interoperability, or full NomadNet support, so those narrow claims remain
bounded by their portable regressions rather than being overstated as powered
qualification.

### 1. Target and dependency integrity

- Builds as `no_std + alloc` on a generic bare-metal target.
- Builds in the Tracker ESP32-S3 firmware graph with one pinned lockfile.
- Portable crates have no accidental Tokio, OS socket, filesystem or
  `getrandom` dependencies.
- Network-controlled operations do not rely on an infallible allocator.
- Feature inspection finds no hidden hosted/default feature activation.

### 2. Packet and identity wire behavior

- Parse and serialize HEADER_1 and HEADER_2 packets at minimum, maximum and
  every structural boundary length.
- Reject truncated, oversized, invalid flag/context and inconsistent-length
  inputs without panic or retained partial state.
- Match destination/name/identity hashes, encryption, signing, validation,
  proofs, IFAC and ratchet vectors generated by the released Python peer.
- Preserve exact packet bytes where signature/hash identity depends on them.

### 3. Routing and transport behavior

- Announce creation, validation, path discovery and path requests.
- Duplicate suppression, hop handling and rate limits.
- Transport forwarding, reverse/link tables and all interface routing modes.
- Correct full-table eviction or rejection with observable metrics.
- Loss, duplication, reordering, delay and reboot scenarios under virtual
  time, including multi-hop forwarding against Python Reticulum.

### 4. Endpoint primitives

- Receipts and explicit/implicit proofs.
- Link establishment, identify, keepalive, close and timeout behavior.
- Requests/responses and Channels.
- Encrypted Resources at minimum, fragment and configured maximum sizes.
- Compressed Resource behavior is measured separately and may require a
  streaming refactor, but it may not silently accept an unsafe RAM profile.

### 5. Hostile input and bounded failure

- Structured malformed corpus plus coverage-guided fuzzing for exposed wire
  parsers.
- Failing-allocator runs at every allocation site reached by packet ingest,
  Link/Resource processing and event creation.
- Table-full, queue-full, event-backpressure and output-backpressure cases.
- Repeated attacker traffic cannot produce monotonic memory growth, watchdog
  starvation or an unrecoverable protocol state.
- Every rejection has an inspectable reason or metric; silent loss of a
  correctness-critical insertion is a failure.

### 6. LXMF-enabling vectors

The first checked Python LXMF 1.0.1 corpus, `reticulum-lxmf-wire` tranche, and
the separate `reticulum-lxmf-ingress` application-event adapter now cover the
supported foundation forms below. The ingress adapter admits only explicitly
owned opportunistic destination DATA or responder-side context-`NONE` Link DATA
bound to the mounted local `lxmf.delivery` destination, resolves source
identities by value, and returns a borrowed validated view without consuming
the event. The portable `reticulum-lxmf-model`, `reticulum-lxmf-store`, and
`reticulum-lxmf-durable-ingress` owner then preserve exact normalized wire in
variable extents without a message-sized copy and acknowledge the retained
application-event lease only after a new commit or a fresh retransmission is
recognized as `AlreadyDurable`.
Replays, alternate valid stamps, and same-ID/different-material collisions stay
distinct across reboot. Proof-bearing events now cross an explicit fixed-
capacity delayed-proof transaction before store I/O; a new commit or a fresh
retransmission recognized as `AlreadyDurable` makes that event's exact proof
ready, while capacity or store failure returns the
exact combined lease and releases only the empty reservation. Required mode
rejects proofless events before I/O; Optional mode admits them but still reserves
every proof that is present. The E290 LXMF composition selects Required for both
opportunistic and direct-packet carriers. This owner never drains or transmits
ready proofs.
The current E290 source composition registers the mount-gated LXMF destination
with retained-proof policy, provides the three bounded owners, and drains ready
proofs through the ordinary router. The single powered A-to-B trial above
confirms that one remote receipt followed an exact durable opportunistic record
and ordinary proof handoff; broader directions, replays, remounts, faults,
pressure, and sustained qualification remain open. Non-`NONE` Link DATA is
unrelated. ADR 0016 admits context-`NONE` Link DATA only with an opaque
Rete-derived destination binding and an independently matching complete LXMF
wire destination. It must also own the exact explicit Link-destined proof
covering the complete received RNS packet hash; that proof remains withheld
until `Committed` or a fresh `AlreadyDurable` result. Initiator/backchannel
direct receive is unsupported. The
[later forced-direct record](e290-direct-link-powered-proof.md) powers one
fresh outbound Link and responder-side new-commit/proof chain. The
[same-Link reuse and direct-replay record](e290-same-link-reuse-replay-powered-proof.md)
then powers two direct-required deliveries with one LXMF message ID, two
distinct packet hashes, and one receiver row. Exact same-`LinkHandle` reuse and
the receiver's `AlreadyDurable` classification remain source-qualified because
the frozen client API exposes neither; the broader fault/pressure matrix
remains open. Current source additionally retains an exact Link handle through
direct receipt timeout, evicts it from reuse, and asks firmware to route normal
authenticated close; the timed-out durable submission is not automatically
retried. The
[current-image powered recovery record](e290-stale-link-recovery-powered-proof.md)
qualifies that narrow receiver-reboot sequence: the failed message is absent
from the peer, and the next sequential message reaches `Delivered` over a fresh
Link. Resource completion remains explicitly deferred pending
bounded Resource ownership. Before production-accepting the RNS foundation,
retain and extend Python-derived LXMF fixtures covering:

- heterogeneous MessagePack keys and values, including unknown structured
  fields; the current allocation-free parser accepts nil/boolean/integer/
  string/binary/generic-extension map keys and has typed fail-closed tests for
  float/container keys and timestamp extension normalization;
- opportunistic and direct/Resource envelopes;
- message hashes and signatures over exact serialized bytes;
- 32-byte stamps and 16-byte tickets;
- released and forward-main announce/application-data forms.

These vectors must fail against the known incompatible `rete-lxmf-core`
encodings. That crate is not a compatibility authority.

### 7. On-target measurements

For each Rete product profile, publish:

- ELF and flash image size;
- static `.text`, `.rodata`, `.data`, `.bss` and uninitialized/reclaimed RAM;
- boot free heap and minimum free heap;
- peak live heap and largest free block per scenario;
- task stack high-water marks;
- maximum transient bytes for packet, Link, request and Resource operations;
- 24-hour idle/announce/hostile-input stability;
- current draw for boot and RX-only states once radio bring-up is enabled.

Absolute product quotas are set from these measurements. The Tracker gate is
not allowed to reduce protocol truth to make a result fit.

## Adoption, promotion and abandonment rule

1. Use Rete now for the narrow production vertical slice and add each
   discrepancy as a reproducible conformance, hostile-input or exhaustion test.
2. Do not enable an affected production path while it has a wire mismatch,
   target-build failure, malformed-input panic, silent correctness-critical
   state loss or unbounded network-controlled memory path.
3. Make focused generic repairs in the project fork. They become candidates
   for upstream contribution only after direct user approval; all Rete crates
   in the graph must remain pinned to the same exact revision.
4. Keep Leviculum compiling as the independent oracle/fallback. Run targeted
   differential scenarios when they help localize a discrepancy; do not delay
   Rete integration for a feature-complete Leviculum adapter.
5. Promote Rete to `ProductionFoundation` only when every hard gate in this
   contract has reproducible passing evidence and an explicit follow-up
   decision records the promotion.
6. Abandon Rete only when reproducible evidence meets an ADR 0002 abandonment
   criterion:
   released-Python incompatibility requiring replacement of the protocol model,
   memory bounds or recoverable failure requiring replacement of the core data
   path, an unsuitable embedded dependency/runtime graph, or a fork whose
   cumulative protocol ownership is effectively a separate RNS implementation.
7. Before abandonment, perform a bounded repair spike and record its patch
   scope and test evidence. If abandonment is warranted, run the failing
   contract and minimum product slice against Leviculum and any newly qualified
   alternative rather than weakening a hard gate.

Tracker V2 memory capacity alone does not reject Rete or narrow the full
product. Optional client modules may be disabled in constrained profiles, and
full-stack measurements may move to a PSRAM target, but the RNS behavior and
finite-memory requirements remain unchanged.

## Evidence format

Generated evidence belongs below ignored `artifacts/phase0/`, with one
directory per run containing:

- `manifest.json`: source/tool versions, target, features and commands;
- `results.json`: pass/fail cases and reasons;
- `memory.json`: section, heap, allocation and stack measurements;
- `interop.json`: peer revisions and scenario results;
- raw serial/event logs and any packet captures;
- a human-readable `summary.md`.

Golden vectors themselves are reviewed source and belong in
`interop/vectors/` with their generator revision and provenance committed.
