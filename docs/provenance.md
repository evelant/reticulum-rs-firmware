# Dependency and source provenance

This file records reviewed source decisions that are not fully represented by
the crates.io registry and `Cargo.lock`. Exact resolved crate versions remain
authoritative in the lockfile.

## Current direct sources

| Component | Source | Pin | License used here | Build role |
| --- | --- | --- | --- | --- |
| Project-owned crates | This repository | current tree | MIT OR Apache-2.0 | Product and shared tooling |
| Rete integration fork | <https://github.com/evelant/rete> | `dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08` on `codex/responder-handshake-reclaim`, descending through `ba73ee426a3211951f5abb400c5728dd359272be`, `354b8757bea63b9d1e27dec14f109fe6c7e03c5a`, `338251b285a2447beb10d390d3e7f53694a1a916`, `a443173b0829c2637ce23531a8cde15fdfec185e`, `2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` on `codex/link-data-receipts`, then `90570cafc812b3025011cb690ec74a27f287cb3f` (tagged predecessor `firmware-pin-90570ca`) and upstream `9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743`; the current revision has no designated durable tag | Apache-2.0 option from retained upstream declaration | Provisional RNS foundation and firmware compile graph; includes canonical local LINKREQUEST validation, transactional owned- and relay-Link admission, endpoint announce policy, caller-owned DATA preparation, bounded receipt backpressure, allocation-atomic proof/timeout terminals, transactional channel send/retry receipt replacement, full-hash/Link-ID-bound DATA/channel terminal candidates, ordinary Link-DATA receipts with destination proof-policy enforcement, exact path/reverse/link forwarding, typed reverse-full/conflict admission, owned HEADER_2 local dispatch, narrow exact-path HEADER_2 DATA/LINKREQUEST relay, foreign-H2 filtering, fail-closed LRPROOF validation, authenticated owned-Link interface binding and pre-dedup wrong-interface rejection, pending-Link expected-hop enforcement, Python-compatible keepalive lifecycle, microsecond/binary64 LRRTT timing with dispatch confirmation, Handshake/Active/Stale LRRTT lifecycle updates and authenticated-malformed teardown, Python-compatible responder-Handshake timeout reclamation, bounded canonical MessagePack request values including anonymous `nil` at `338251b`, prepared-versus-confirmed request ownership with timeout start at exact first dispatch at `354b875`, validated inbound encoded-value events with their original request timestamp at `ba73ee4`, phase-agnostic exact request-dispatch reclaim keyed by request and Link IDs with prior native-phase reporting at `dfcaa36`, identities-only snapshot restoration, and LXMF attempt correlation. These request primitives do not constitute full NomadNet or Resource support. The first three generic changes were already offered in upstream PRs 7, 9 and 11; the newer lifecycle, routing, Link-DATA receipt, responder-timeout, and request work remains fork-local unless the user directly approves an upstream issue or PR. |
| Leviculum | <https://codeberg.org/Lew_Palm/leviculum> | `5fb1db0e5e5a490291ee5f6b81312cf0c9de622a` | AGPL-3.0-or-later | Separate protocol oracle and fallback package |
| esp-rs platform family, including esp-rtos | <https://github.com/esp-rs/esp-hal> | exact Git revision `b50efcb0dcd94b58ec337e511891057aa1f2e8fb` in the workspace and lockfile | MIT OR Apache-2.0 | Coherent ESP32-S3 HAL, radio, runtime, bootloader, logging, and storage graph; this upstream revision contains both CPU0/CPU1 stack-slice element-count corrections and the ESP32-S3 combo-PHY fix from esp-hal #5776, so no local esp-rtos overlay is active |
| lora-phy | Published crates.io 3.0.1 source vendored at `vendor/lora-phy-3.0.1` | archive SHA-256 `61471c3b2909789e3332083577f6cf6c41a4fcf37674ef15156bcbb20504ac65`; crate-recorded upstream commit `ca04c2284eb00e015528933ea5159cd1ff36142d`; exact tree and pristine/patched hashes in `VENDOR-HASHES.json` | MIT OR Apache-2.0 | SX126x radio owner with atomic board PA/FEM hooks, arm-once continuous-RX IRQ draining, terminal receive classification, synchronized standby, and explicit IRQ quiescence before mode changes; exact edits, integrity guard, and removal condition are recorded in `PATCHES.md` |
| embedded-hal / embedded-hal-async / embedded-hal-bus / lora-modulation | crates.io | exact versions in workspace and lockfile | MIT OR Apache-2.0 | Portable pin/SPI/profile contracts and the target-exclusive async SPI device |
| Embassy futures/sync/time, static_cell and zeroize | crates.io | exact versions in workspace and lockfile | MIT OR Apache-2.0 | Bounded target coordination, in-place protocol ownership and temporary key cleanup |

The current exact pin is
`dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08` on fork branch
`codex/responder-handshake-reclaim`; it has no designated durable tag. It
descends through `ba73ee426a3211951f5abb400c5728dd359272be`,
`354b8757bea63b9d1e27dec14f109fe6c7e03c5a`,
`338251b285a2447beb10d390d3e7f53694a1a916`,
`a443173b0829c2637ce23531a8cde15fdfec185e`, and
`2d0781838aa03370b739d4003bcd1bdd5bbb0c6c` on
`codex/link-data-receipts`, which descends from
`90570cafc812b3025011cb690ec74a27f287cb3f`, whose durable tag is
`firmware-pin-90570ca`. That predecessor
adds exact-interface transport outcomes through the
stack and Embassy/Tokio dispatch layers; one-shot reverse-proof interface
validation; direction, hop, identity, signature, and canonical-header checks
for relayed Link proofs; transactional owned/relay Link and H2 reverse
admission; typed stack rejections for owned/relay Link exhaustion and reverse
full/conflict; owned H2 local dispatch; and identities-only snapshot restore
until stable interface rebinding exists. Relay-Link occupancy is independently
observable. The `2d07818` descendant additionally registers ordinary Link-DATA
receipts and honors the receiving destination's `PROVE_NONE`, `PROVE_ALL`, or
application-selected proof policy instead of proving all context-`NONE` Link
DATA unconditionally. The `a443173` descendant additionally reclaims a
responder that remains in `Handshake` through its Python-compatible
establishment deadline. The direct-request sequence is `338251b` for bounded
canonical MessagePack values including anonymous `nil`, `354b875` for an
unforgeable prepared-request authority that becomes confirmed timeout ownership
only at exact first dispatch, `ba73ee4` for lossless inbound encoded values and
their original wire timestamp, and `dfcaa36` for phase-agnostic exact
request-dispatch reclaim keyed by request and Link IDs that reports whether the
removed native residue was prepared or confirmed.
These are direct single-packet request primitives, not full NomadNet or
Resource support. Arbitrary remote H1 LINKREQUEST and the
guarded H1 DATA compatibility seam remain gated on explicit interface roles.
No issue or pull request was opened for this newer work. Publishing it upstream
still requires the user's direct approval.

For locally owned Links, the responder binds LINKREQUEST ingress and the
initiator binds only after valid LRPROOF ingress; preliminary path selection is
not authoritative Link state. Established output carries `BoundInterface` and
resolves to the exact physical interface. Only an initial LINKREQUEST whose
path lacks a recorded interface may broadcast. Wrong-interface Link DATA and
`RESOURCE_PRF` fail before dedup admission. This pin still stores only an
interface-slot index: asynchronous output on a bound Tokio shared `Hub`
broadcasts to sibling clients until endpoint-aware client identity and
reconnect generation are retained. This pin adds exact unencrypted 20-byte
keepalives: the initiator alone emits `0xff` after both a full inbound-silence
interval and a full interval since its previous probe, and the responder alone
returns `0xfe`. Valid role-specific repeats bypass dedup only after
bound-interface validation; lifecycle results are internal, and
automatic output preflights and retains that route before advancing its timer.
Stale begins after two intervals and preserves `4 * RTT + 5 seconds` from the
actual transition/final probe for any valid bound Link traffic to revive it
(five seconds when RTT is zero).
Channel sends preflight MDU, pending-window allocation and receipt capacity;
retries preflight the authoritative route and atomically move the envelope's
sole live proof target to the fresh ciphertext hash before committing retry
state. Obsolete proofs fail closed and Link removal reclaims channel receipts.
Pending-Link expected hops are now retained as an initiation-time known-path
snapshot, or as the `PATHFINDER_M = 128` wildcard when no path is known.
LRPROOF mismatch fails before deduplication or Link-state mutation, and a
responder records the post-ingress hop only from authenticated, decrypted
LRRTT. Rete emits canonical MessagePack float64, accepts the numeric scalar
families and first-object/trailing-byte behavior of Python's u-msgpack, and
selects the greater local or peer RTT with Python ordering. It retains an
immutable request anchor, represents Link time with microsecond
`MonotonicInstant`/`MonotonicDuration`, and stores RTT as binary64. Opaque,
non-repeating eight-byte protocol tokens accept only the first successful
interface confirmation: initiator LINKREQUEST uses the confirmed egress
interval start, and responder LRPROOF uses its completion. This boundary is
interface/router acceptance, not physical RF `TxDone`.

Fresh authenticated LRRTT is processed in `Handshake`, `Active`, and `Stale`.
Only first activation establishes the Link; repeats refresh timing, hops, and
keepalive state and emit `LinkRttUpdated`, while exact raw replay remains
deduplicated. Authenticated malformed LRRTT tears down all three states and
increments `links_failed` only in Handshake. Zero RTT retains the 5-second
keepalive/10-second stale floors; nonzero RTT uses `4 * RTT + 5 seconds` stale
grace. The authenticated-before-liveness order intentionally hardens Rete
against corrupt stale packets that released Python would count as liveness.
Rete uses one pre-decrypt ingress sample across a bounded synchronous handler,
not Python's three internal samples. The firmware adapter uses precise `*_at`
paths and confirms at generic ordinary-router acceptance; upstream Tokio and
Embassy runners remain coarse/unconfirmed compatibility users.

Responder establishment maintenance closes and reclaims an owned Link at
`360 + 6 * max(1, post-ingress hops)` seconds. Its timing origin is confirmed
LRPROOF completion when available and LINKREQUEST admission otherwise. The
closure contributes only to aggregate `closed_links`/`links_closed`; it is not a
malformed or cryptographic `links_failed` event. The product retains its
separate initiator deadline and exact-abort ownership.

Shared-Hub endpoint/reincarnation identity and established-Link watchdog
timeout `LINKCLOSE` emission also remain unresolved. Adaptive channel windows
larger than receipt capacity produce typed backpressure and remain a product
sizing/throughput policy.

Earlier build and powered-evidence records retain the Rete revisions and
artifact hashes they actually used. In particular, records naming `9bceacd`,
`f6f5fb0`, or `8b5d652` remain historical evidence only and do not qualify the
current pin. The preceding `14c7b49` pin's build-only default E290 release
packages as a 776,464-byte merged image with SHA-256
`7b11c6f6a3c039d46ab0117fd362920aaa40145e7f27cbc6fa0a8a84a7ab3571`.
It has no flashed-image readback or powered proof. The subsequent pre-PSRAM
application-event release has text/data/BSS of 684,167/3,676/469,152 bytes (1,156,995 bytes
total by GNU size). Its 12,345,320-byte ELF has SHA-256
`ebb34e7176a8e61b6969ebf99d7dac97c6e674ef5e583bbf931a34e8b6e970a2`;
the 789,504-byte merged image uses 723,968/6,291,456 application bytes (11.51%)
and has SHA-256
`1796f161c480d0348e3d47fd8f3cda5fda5b51aa38ad6024aaad04c8ba1751ce`.
That image matched an exact `3e:88` readback and served an authenticated
`identity-summary`; `3f:88` did not enumerate, so a current two-board
lifecycle/RF run remained open. The initial pre-PSRAM runtime-measurement HIL
links with text/data/BSS of 695,315/4,180/468,648 bytes (1,168,143 bytes total);
its 12,498,356-byte ELF has SHA-256
`4ca4eef73ff1babd00750d4a635f7644d73d1a3ae1cde4fb1dbdb434937bcfca`,
and its 800,480-byte merged image uses 734,944/6,291,456 application bytes
(11.68%) with SHA-256
`ec23bf0a7b20b7364e12cba6ebc90aa3e0ce761650413e1ad9d6186eeecf1662`.
A target-scoped rebuild retained those section/application sizes. Its
12,498,348-byte ELF has SHA-256
`c84363dff0801a1679dd786b5070c4662962d299f0269efc0cd72ff9c09b8e2a`,
and its 800,480-byte merged image has SHA-256
`058a969e0b9e099f6a5febd1b59f4a70cfd3ea932e8f0738a2ddb4b3e5569119`.
That package matched an exact `3e:88` readback and produced the authenticated
108,940-ms bounded memory/API/two-confirmed-TX checkpoint recorded in the E290
runbook. The board was then restored to an exact-readback 789,504-byte default
rebuild, SHA-256
`a67afa72681558dc02fd0575a18711b2b3c05b365a66af45441b7cb8dd3a2577`,
and served `identity-summary`. These are historical pre-PSRAM artifacts and
checkpoints. The later 870,656-byte LXTE/v2 HIL matched exact readbacks on both
boards but retained the now-historical paired-announce discovery failure. The
independently scheduled replacement HIL ELF is 13,642,544 bytes with SHA-256
`e2fb2bee32026d28d7ec2cc727788a267ff19a5fd0a1b6b194f6a08ea643e9b8`;
its 871,296-byte package uses 805,760 application bytes and has SHA-256
`89d303d4880d062068bf8a9f4124bfbea322af091e5bf37c04e4e52715481cbd`.
Identity-bound exact package readbacks passed on both boards. Both USB devices
disappeared before the required post-flash pre-submit checkpoint, so no powered
durable-LXMF outcome follows from that historical attempt.

The retained Stage 5 default/HIL pair has 946/962 stack-size records, a
53,680-byte maximum frame, and 175,056/174,256-byte usable stacks. The default
ELF is 13,648,888 bytes with SHA-256
`92e63b60a5f4b830ee55d958fcc446a6878036212904b8748519ae210ba3da58`;
its 868,656-byte package uses 803,120 application bytes and has SHA-256
`c8da2af30e2d0ee24ca4b215151d1370b7e1d242991ebbeb024079a730693a3f`.
The HIL ELF is 13,821,496 bytes with SHA-256
`7a3fad34699f910a2050468ada6461a0f33d16641ab5425a5c795a71238861ff`;
its 881,456-byte package uses 815,920 application bytes and has SHA-256
`12c6f31a7fb64485ad9220edca4ac38ba0a57867ad88ce60fa1a24ffc195d379`.
Identity-bound exact HIL readbacks passed on both boards. Exactly one fresh
A-to-B trial converted a 206-byte LXMF carrier to the exact 307-byte RNS packet
with SHA-256
`060037041c91eb5999f89bf84845c19e65bf7fa680827cce9c51e8ecc5dbe0a6`
and reached `Delivered` on its first attempt. Receiver B committed message
`abdeec2e498f09c96a6fd56ec3558ca86c2598aaeacac81969b645de3b549dc3`,
advanced new/ready/released/ordinary-handoff by one with zero replay/order
events, and confirmed one proof TX. Its `LXTE` release tag
`0x3dc4588d3a205429` matches A's delivered tag; receiver `RPTE` generated-proof
metadata remains zero by design because retained LXMF proofs are intercepted
before ordinary ingress metadata. The exact 2 MiB B-store SHA-256 is
`c75ab2a01b3266fda1e07e0271c70bb29c06e32636d70d8a70d977b9e8b0e21e`;
its sole record matches the generator and full-wire digest
`1c1839991401e01e15e3a3146cd3177a4fb7e5dbd52008fd119beaf091d377ba`.
Baseline/terminal evidence reports zero allocation failures, unexpected runtime
errors, RX/CAD/TX watchdog expiries, and correlation faults. This is narrow
powered provenance for persistent continuous RX and one opportunistic durable-
LXMF proof chain, not direction-balanced, replay/remount, fault, range, or soak
qualification. The conservative stack carry-forward remains 57,700 bytes with
a 4,020-byte post-frame margin. The complete post-offload placement, scheduler,
historical failure, artifact, and powered-trial records remain in the
[E290 runbook](e290-node.md#stage-5-psram-boot-checkpoint).

Phase-1 normal/pressure and closure artifact manifests bind the project commit
and its raw Git root tree; their tool inventories record the same pair and the
source-Git isolation policy. Powered-evidence initialization and verification
require both bundles to agree on both object IDs. Project-source Git commands
clear ambient repository/configuration variables, disable replacement objects,
null system/global configuration, override hooks, fsmonitor and external
attributes, validate the canonical repository root, reject nonstandard index
flags, and reject any common-directory `info/attributes`. The source-tar proof
compares extracted files, modes and symlinks directly with raw tree/blob
objects. It intentionally does not compare one `git archive` output with
another, because committed or repository-local export attributes could filter
or substitute both archives identically.

The RNode LoRa header and split/reassembly behavior in
`crates/radio-interface` is an independent project-owned implementation of the
published wire behavior. It was checked against the retained Rete
`rete-iface-lora` implementation at `9bcb7d3e…` and the working
`microReticulum_Firmware` Tracker reference; no source from either checkout is
copied into the crate. The four-bit sequence format cannot distinguish a
same-sequence duplicate from a continuation, so that limitation is documented
and tested rather than hidden behind a stronger private framing scheme.
The product-only `crates/rns-rete-rx` adapter owns the local composition of
that RNode reassembler with the pinned Rete receive owner. The underlying
`crates/rns-rete` integration and `crates/node-core` do not depend on this
LoRa/RNode layer.

The Phase-1 schema-3 RNode HIL corpus and `interop/python/rnode_hil.py` KISS
peer are also independent project-owned implementations of the published
command and escaping behavior. Official RNode Firmware 1.86 at
`9b39b6ce5962007fafefc22034082f354eff3374` is an external GPL-3.0-or-later
device peer; that commit has root tree
`12f583c5f0fd8ae83c59a391267f0fe9ce184d86`. No firmware or Python-module
source is copied into the host tool. Powered qualification preserves a
self-contained Git bundle rooted at the official `1.86` tag, including its
complete reachable history while the runbook command deliberately omits
unrelated local refs. The verifier does not require an exact ref inventory;
extra refs do not weaken its proof. It clones and strictly checks the object
graph, requires the pinned commit to be reachable from a preserved ref, and
requires that exact root tree; a tar archive or forged metadata header cannot
substitute for the source proof. The project-owned
corpus and tool copies are compared with the files in the qualification
bundle's verified `source.tar`, not mutable working-tree files.

The boot-bound local-DATA generator uses the pinned Apache-2.0 Rete graph and
marks its predictable deterministic entropy as HIL-only and non-secret. Its
shared implementation is `tools/phase1-rx-local-data/src/generator.rs`.
Powered-evidence verification pins that archived generator source, regenerates
the custom corpus from the recorded boot identity and base corpus, and requires
byte-for-byte equality with the preserved JSON.

The separately hazardous Tracker TX HIL has an opt-in
`semantic-announce-hil` graph that uses the same pinned Apache-2.0 Rete adapter
through its conformance-only announce constructor. It embeds the public test
key, zero RNG, fixed timestamp and `testapp.aspect1` values from the committed
Python-RNS 1.3.8 announce vector. Host tests require exact 167-byte equality,
and firmware reparses and cryptographically validates the result before its
one authorized transmission. The feature-free TX HIL remains the invalid
sentinel graph and resolves no Rete packages. Neither HIL mode supplies a
production identity, entropy source, clock, announce scheduler or TX policy.

### Historical esp-rtos overlay (removed)

Earlier builds temporarily carried a local patch over published `esp-rtos`
0.3.0 because that package constructed the CPU0 and CPU1 main-task
`*mut [MaybeUninit<u32>]` slices with byte counts instead of element counts.
Those archived builds embedded
`esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2`; the old vendor manifest and
source-shape guard described that historical overlay only.

Canonical builds now resolve `esp-rtos` directly from exact upstream esp-rs
revision `b50efcb0dcd94b58ec337e511891057aa1f2e8fb`, which includes both
equivalent stack-slice corrections. `xtask graph-policy` verifies the resolved
Git source and revision. Firmware embeds
`esp-rtos-upstream-b50efcb-stack-words-v1` as runtime evidence, and no build
script reads or requires a local esp-rtos source tree. The obsolete vendor
directory, inventory reconstruction, and local-license copies were removed.

Published `lora-phy` 3.0.1 derives every SX1262 high-power PA command from the
requested output power and calls `do_rx()` from every `LoRa::rx()` invocation,
including continuous mode. It exposes no public separation between the
cancellation-safe DIO wait and the non-cancel-safe SPI work that drains the
observed IRQ, so an arm-once continuous receiver otherwise has to issue another
`SetRx` between packets.

The local patch adds an atomic, default-`None`
`Sx126xVariant::high_power_pa_override()` policy carrying PA duty cycle,
`hpMax`, the raw signed `SetTxParams` power byte and optional OCP trim together,
plus default-no-op post-initialization and early-transmit RF-path hooks. It also
adds public arm-once `start_rx()` and drain-many `process_rx_irq()` operations,
uses them in the LoRaWAN continuous path, recognizes preamble/sync/header
progress and terminal invalid-frame/timeout precedence, synchronizes public
standby state, and explicitly disables IRQ routing and clears pending flags
before CAD/TX reconfiguration. SX127x ordinary IRQ handling clears only its
captured snapshot so a later distinct flag is preserved. All PA fields are
validated before PA/OCP commands are written; the existing TxClamp operation
remains first, and a valid override then emits PA, optional OCP and TX-parameter
commands in order. Existing variants retain the default PA and no-op board-hook
behavior. The Tracker HIL uses the hooks to settle and arm its external FEM;
the shared RNode core and permanent E290 actor use the continuous-RX and
quiescence behavior. No artificial split-frame TX delay is added. The single
fresh 307-byte powered E290 trial above narrowly confirms that persistent RX
keeps the two physical RNode frames receivable across the scheduler boundary.

The checked vendor manifest records every published file, the exact crates.io
archive and crate-recorded source commit, `PATCHES.md`, seven patched source
files, and thirty-three reviewed source replacements.
`xtask graph-policy` requires Cargo to resolve the local path, verifies the
complete inventory and digests, rejects symlinks, and reconstructs each
pristine source file by reversing only those replacements. Remove the path
patch after an upstream release provides equivalent atomic PA/OCP,
post-initialization and early-TX RF-path hooks, arm-once continuous receive,
terminal receive classification, captured-mask IRQ clearing, and safe
standby/IRQ quiescence, and after the project's regression guard has moved to
that release.

Rete's reviewed snapshot declares `MIT OR Apache-2.0` in Cargo metadata and
its README but does not contain canonical license files. This is release
packaging hygiene to resolve with upstream or in the corresponding-source
bundle; it is not being silently inferred from code.

All Rete workspace crates in the product graph move together. The integration
fork retains upstream history and contains only focused commits that may be
considered for upstream review after direct user approval. If an approved fix
is later merged, the graph returns atomically to one exact upstream revision
rather than retaining a parallel implementation.

The full AGPL-3.0 text for the isolated Leviculum comparison is retained at
`comparisons/rns-leviculum/LICENSE`.

## Hardware reference evidence

The Phase-1 Tracker pin correction was checked against the following local
Heltec V2.3 reference files. They remain research evidence under the ignored
`reference/` directory; the digests make the exact inputs identifiable.

| Evidence | SHA-256 | Finding used by the board profile |
| --- | --- | --- |
| `reference/heltec_tracker_v2.3_schematic.pdf` | `148672bdc7ca8646d9de5d3e9a9e58c647b1c46bd5b0b68616efa80dbd225ea7` | Hidden netlist joins `PA_CPS` to SX1262 `U12-12`, KCT8103L `U10-5` and `C92-1`; it does not join ESP32-S3 GPIO46 |
| `reference/heltec_tracker_v2.3_pin_map.png` | `81b2e47d94dd0d3a3749c9b89ba46f22f343a8eab5d979bff721454bf4a0a5a3` | GPIO46 is shown as a header breakout, consistent with schematic net `46` from `U6-52` to `P3-17` |

Consequently the firmware does not claim GPIO46 as an RF interlock. SX1262
DIO2 directly owns the KCT8103L CPS input; powered qualification probes the
actual `PA_CPS` net at `C92-1` with high impedance.

## Research sources not used by builds

`reference/` is ignored and never appears in a committed dependency path. Its
checkouts are research evidence only. A useful local snapshot does not grant a
build dependency or permission to copy code without recording the source and
license here.

The first `reticulum-lxmf-wire` compatibility corpus uses CPython 3.13.7,
Python LXMF 1.0.1 at revision
`fab12ad9bf9f997797034950f289fe41a79dcf5a`, and Python RNS 1.3.5 at revision
`50e03a24e8e10256363f6b73af7f6804ddb90e6f`. The generated corpus records
the Reticulum License, exact authority-source and u-msgpack digests, and the
distinct upstream notices retained in `LICENSE-RETICULUM` for LXMF and
`LICENSE-RETICULUM-RNS` for RNS. Its generator SHA-256 is
`3a6f07a6380fc18ca533e1b31a7b2a25d9059a668e7e91a689f600a1893c20b0`, and its
requirements SHA-256 is
`f7876030b5e143e89bea278c2dc4892cd58c47f5709b72c2cc848861670b1c86`.
Those Python packages are version-pinned test/reference authorities; no Python
implementation source is linked into the firmware crate. The crate enables
`ed25519-dalek`'s `hazmat` feature only for its allocation-free streaming
verification API and uses feature-disabled `curve25519-dalek` directly to
enforce the prime-order subgroup checks required by the strict firmware
profile. The exact locked normal closure is independently guarded and contains
no allocator, standard library, Rete, board, executor, or transport dependency.
The mixed-order regression corpus constructs its points locally from
`curve25519-dalek`'s basepoint and torsion constants. No third-party encoded
test-vector literals are retained.

## Future derived-code boundaries

- The initial `reticulum-lxmf-wire` source is independently authored against
  the Python corpus and remains under the workspace `MIT OR Apache-2.0`
  declaration. It contains no copied or modified LXMF-rs implementation source.
- Any future directly copied or modified LXMF-rs source will live in an
  explicitly EPL-2.0 file or crate, with SPDX identifiers, notices, and source
  file/commit notes. That derived source will not inherit the workspace
  MIT/Apache declaration.
- Any future directly reused Reticulum/LXMF Python reference source retains the
  Reticulum License and notice; using the pinned implementation as a test peer
  or generated-byte authority does not itself make project source a copy.
- AGPL implementation code is linked only in coherent AGPL packages or
  binaries. It is otherwise used as a black-box peer or behavioral reference.
- Source without a clear grant, including the reviewed Precursor root, is not
  copied until its license is clarified. Reusing its published deterministic
  interoperability input parameters as behavioral facts does not copy its
  implementation source; their origin remains identified in the generator.

## Release requirements

Before distributing firmware, generate a per-binary dependency bill of
materials and third-party license/notice bundle from the locked graph. Retain
the exact corresponding source for applicable reciprocal components. The
device's About/API surface must expose the same component/version/license
inventory in a compact form.
