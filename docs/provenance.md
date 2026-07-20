# Dependency and source provenance

This file records reviewed source decisions that are not fully represented by
the crates.io registry and `Cargo.lock`. Exact resolved crate versions remain
authoritative in the lockfile.

## Current direct sources

| Component | Source | Pin | License used here | Build role |
| --- | --- | --- | --- | --- |
| Project-owned crates | This repository | current tree | MIT OR Apache-2.0 | Product and shared tooling |
| Rete integration fork | <https://github.com/evelant/rete> | `90570cafc812b3025011cb690ec74a27f287cb3f` (designated durable tag `firmware-pin-90570ca`), based on upstream `9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743` | Apache-2.0 option from retained upstream declaration | Provisional RNS foundation and firmware compile graph; includes canonical local LINKREQUEST validation, transactional owned- and relay-Link admission, endpoint announce policy, caller-owned DATA preparation, bounded receipt backpressure, allocation-atomic proof/timeout terminals, transactional channel send/retry receipt replacement, full-hash/Link-ID-bound DATA/channel terminal candidates, exact path/reverse/link forwarding, typed reverse-full/conflict admission, owned HEADER_2 local dispatch, narrow exact-path HEADER_2 DATA/LINKREQUEST relay, foreign-H2 filtering, fail-closed LRPROOF validation, authenticated owned-Link interface binding and pre-dedup wrong-interface rejection, pending-Link expected-hop enforcement, Python-compatible keepalive lifecycle, microsecond/binary64 LRRTT timing with dispatch confirmation, Handshake/Active/Stale LRRTT lifecycle updates and authenticated-malformed teardown, identities-only snapshot restoration, and LXMF attempt correlation. The first three generic changes were already offered in upstream PRs 7, 9 and 11; the newer lifecycle and routing work remains fork-local unless the user directly approves an upstream issue or PR. |
| Leviculum | <https://codeberg.org/Lew_Palm/leviculum> | `5fb1db0e5e5a490291ee5f6b81312cf0c9de622a` | AGPL-3.0-or-later | Separate protocol oracle and fallback package |
| esp-hal family | <https://github.com/esp-rs/esp-hal> | crates.io versions in lockfile | MIT OR Apache-2.0 | ESP32-S3 platform |
| esp-rtos | Published crates.io 0.3.0 source vendored at `vendor/esp-rtos-0.3.0` | archive SHA-256 `551f90766e1527edaa0c91e8d559e9e2a60397b545e93357ac61fb31845e5712`; crate-recorded upstream commit `347003de8a48320bb7724f53045be3afa9204411`; exact tree and pristine/patched hashes in `VENDOR-HASHES.json` | MIT OR Apache-2.0, with canonical license texts added as project provenance files | Local CPU0 and CPU1 main-stack slice unit corrections; exact edits, mechanical integrity guard and removal condition are recorded in `PATCHES.md` |
| lora-phy | Published crates.io 3.0.1 source vendored at `vendor/lora-phy-3.0.1` | archive SHA-256 `61471c3b2909789e3332083577f6cf6c41a4fcf37674ef15156bcbb20504ac65`; crate-recorded upstream commit `ca04c2284eb00e015528933ea5159cd1ff36142d`; exact tree and pristine/patched hashes in `VENDOR-HASHES.json` | MIT OR Apache-2.0 | SX126x radio owner with an atomic, default-preserving board override for high-power PA/OCP/encoded power, default-no-op post-initialization and early-TX RF-path hooks, and public standby state synchronization; exact edits, integrity guard and removal condition are recorded in `PATCHES.md` |
| embedded-hal / embedded-hal-async / embedded-hal-bus / lora-modulation | crates.io | exact versions in workspace and lockfile | MIT OR Apache-2.0 | Portable pin/SPI/profile contracts and the target-exclusive async SPI device |
| Embassy futures/sync/time, static_cell and zeroize | crates.io | exact versions in workspace and lockfile | MIT OR Apache-2.0 | Bounded target coordination, in-place protocol ownership and temporary key cleanup |

The designated durable tag for commit
`90570cafc812b3025011cb690ec74a27f287cb3f` is
`firmware-pin-90570ca`. The pin
adds exact-interface transport outcomes through the
stack and Embassy/Tokio dispatch layers; one-shot reverse-proof interface
validation; direction, hop, identity, signature, and canonical-header checks
for relayed Link proofs; transactional owned/relay Link and H2 reverse
admission; typed stack rejections for owned/relay Link exhaustion and reverse
full/conflict; owned H2 local dispatch; and identities-only snapshot restore
until stable interface rebinding exists. Relay-Link occupancy is independently
observable. Arbitrary remote H1 LINKREQUEST and the guarded H1 DATA
compatibility seam remain gated on explicit interface roles. No issue or pull
request was opened for this newer work. Publishing it upstream still requires
the user's direct approval.

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
Shared-Hub endpoint/reincarnation identity and automatic timeout `LINKCLOSE`
emission also remain unresolved. Adaptive channel windows
larger than receipt capacity produce typed backpressure and remain a product
sizing/throughput policy.

Earlier build and powered-evidence records retain the Rete revisions and
artifact hashes they actually used. In particular, records naming `9bceacd`,
`f6f5fb0`, or `8b5d652` remain historical evidence only and do not qualify the
current pin. The preceding `14c7b49` pin's build-only default E290 release
packages as a 776,464-byte merged image with SHA-256
`7b11c6f6a3c039d46ab0117fd362920aaa40145e7f27cbc6fa0a8a84a7ab3571`.
It has no flashed-image readback or powered proof. The current `90570ca` pin
has a build-only default release with text/data/BSS of
674,431/3,676/469,152 bytes (1,147,259 bytes total by GNU size). Its ELF has
SHA-256 `d370039c3872d34a74b9bbc0b52567a24be607bc01ea660b6dfbd8d5dd12072d`;
the 780,448-byte merged image uses 714,912/6,291,456 application bytes (11.36%)
and has SHA-256
`a912bb6c910c0145a9431f2a94b95a0a6560662678c457fc9c49e8641050b72c`.
The current runtime-measurement HIL links with text/data/BSS of
686,203/4,180/468,648 bytes (1,159,031 bytes total); its ELF has SHA-256
`5aaa4c7029b35b55c5f2eb0f673c04ac11ae695c09a8cc1d1797990fe0a4ab30`,
and its 792,048-byte merged image uses 726,512/6,291,456 application bytes
(11.55%) with SHA-256
`938d944c9373638b475e48e804fc0211b92da1ef49d0e875233d052b19064881`.
Both current images are unflashed and unpowered because neither E290 currently
enumerates; a new powered run remains required for any hardware claim.

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

Published `esp-rtos` 0.3.0 constructs both CPU0 and CPU1 main-task
`*mut [MaybeUninit<u32>]` slices with stack byte counts as their element counts,
representing four times each actual stack reservation. The vendored patch
divides the CPU0 symbol difference and CPU1 `STACK_SIZE` by
`size_of::<MaybeUninit<u32>>()` before slice construction. The checked vendor
manifest records the published archive, exact base inventory, pristine hashes,
project provenance files and both reviewed source replacements. `xtask
graph-policy` verifies that exact tree and reconstructs the pristine
`src/lib.rs` by reversing only those two replacements. The firmware build also
verifies both corrected source shapes and embeds
`esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2` in every `esp-rtos`-based
Tracker ELF. The RF-inert retained-journal HIL binaries use `esp_hal::main` and
do not carry that runtime identity. Remove the path dependency only after an
upstream release contains both equivalent fixes and the regression guard is
updated.

The published crate README retains repository-layout links to
`../LICENSE-APACHE` and `../LICENSE-MIT`; those links do not resolve from the
package-local vendor directory. The canonical texts intentionally added at
`vendor/esp-rtos-0.3.0/LICENSE-APACHE` and `LICENSE-MIT` are the applicable
copies. The upstream-marked README remains byte-identical to the registry
archive so the vendor reconstruction check stays meaningful.

Published `lora-phy` 3.0.1 derives every SX1262 high-power PA command from the
requested output power. The local patch adds an atomic, default-`None`
`Sx126xVariant::high_power_pa_override()` policy carrying PA duty cycle,
`hpMax`, the raw signed `SetTxParams` power byte and optional OCP trim together,
plus default-no-op post-initialization and early-transmit RF-path hooks. It also
makes the public standby operation update the software radio mode after the
hardware command succeeds, so later TX preparation does not issue a redundant
standby command after an early external-FEM gate is armed. All PA fields are
validated before PA/OCP commands are written; the existing TxClamp operation
remains first, and a valid override then emits PA, optional OCP and TX-parameter
commands in order. Existing variants and interfaces retain upstream behavior.
The Tracker HIL alone uses the post-initialization hook to enable and settle its
external FEM with CTX low, then asserts CTX after modem/power/standby
normalization but before packet/FIFO preparation while preserving the final
pre-`SetTx` gate. The checked vendor manifest records every published file, the
exact crates.io archive and crate-recorded source commit, `PATCHES.md`, four
patched source files and sixteen reviewed source replacements.
`xtask graph-policy` requires Cargo to resolve the local path, verifies the
complete inventory and digests, rejects symlinks, and reconstructs each
pristine source file by reversing only those replacements. Remove the path
patch after an upstream release provides equivalent atomic PA/OCP,
post-initialization and early-TX RF-path hooks, preserves public standby state
synchronization, and the project's regression guard has moved to that release.

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

## Future derived-code boundaries

- Reused or modified LXMF-rs source will live in an explicitly EPL-2.0 crate,
  with SPDX identifiers and source file/commit notes. It will not inherit the
  workspace MIT/Apache declaration.
- Directly reused Reticulum/LXMF Python reference source retains the Reticulum
  License and notice.
- AGPL implementation code is linked only in coherent AGPL packages or
  binaries. It is otherwise used as a black-box peer or behavioral reference.
- Source without a clear grant, including the reviewed Precursor root, is not
  copied until its license is clarified.

## Release requirements

Before distributing firmware, generate a per-binary dependency bill of
materials and third-party license/notice bundle from the locked graph. Retain
the exact corresponding source for applicable reciprocal components. The
device's About/API surface must expose the same component/version/license
inventory in a compact form.
