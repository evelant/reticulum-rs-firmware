# Dependency and source provenance

This file records reviewed source decisions that are not fully represented by
the crates.io registry and `Cargo.lock`. Exact resolved crate versions remain
authoritative in the lockfile.

## Current direct sources

| Component | Source | Pin | License used here | Build role |
| --- | --- | --- | --- | --- |
| Project-owned crates | This repository | current tree | MIT OR Apache-2.0 | Product and shared tooling |
| Rete integration fork | <https://github.com/evelant/rete> | `5ce8c4e437d3f2f07d302bc366ff06bacd6aff2d` (durable tag `firmware-pin-5ce8c4e`), based on upstream `9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743` | Apache-2.0 option from retained upstream declaration | Provisional RNS foundation and firmware compile graph; canonical local LINKREQUEST validation is in [upstream PR 7](https://github.com/s-retlaw/rete/pull/7), transactional owned-Link admission is in [upstream PR 9](https://github.com/s-retlaw/rete/pull/9), and endpoint announce rebroadcast policy is in [upstream PR 11](https://github.com/s-retlaw/rete/pull/11) |
| Leviculum | <https://codeberg.org/Lew_Palm/leviculum> | `5fb1db0e5e5a490291ee5f6b81312cf0c9de622a` | AGPL-3.0-or-later | Separate protocol oracle and fallback package |
| esp-hal family | <https://github.com/esp-rs/esp-hal> | crates.io versions in lockfile | MIT OR Apache-2.0 | ESP32-S3 platform |
| esp-rtos | Published crates.io 0.3.0 source vendored at `vendor/esp-rtos-0.3.0` | archive SHA-256 `551f90766e1527edaa0c91e8d559e9e2a60397b545e93357ac61fb31845e5712`; crate-recorded upstream commit `347003de8a48320bb7724f53045be3afa9204411`; exact tree and pristine/patched hashes in `VENDOR-HASHES.json` | MIT OR Apache-2.0, with canonical license texts added as project provenance files | Local CPU0 and CPU1 main-stack slice unit corrections; exact edits, mechanical integrity guard and removal condition are recorded in `PATCHES.md` |
| lora-phy | <https://github.com/lora-rs/lora-rs> | exact crates.io version `3.0.1` in lockfile | MIT OR Apache-2.0 | Opaque receive-only Tracker radio owner; TX-capable upstream surface is not exported |
| embedded-hal / embedded-hal-async / embedded-hal-bus / lora-modulation | crates.io | exact versions in workspace and lockfile | MIT OR Apache-2.0 | Portable pin/SPI/profile contracts and the target-exclusive async SPI device |
| Embassy futures/sync/time, static_cell and zeroize | crates.io | exact versions in workspace and lockfile | MIT OR Apache-2.0 | Bounded target coordination, in-place protocol ownership and temporary key cleanup |

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

Rete's reviewed snapshot declares `MIT OR Apache-2.0` in Cargo metadata and
its README but does not contain canonical license files. This is release
packaging hygiene to resolve with upstream or in the corresponding-source
bundle; it is not being silently inferred from code.

All Rete workspace crates in the product graph move together. The integration
fork retains upstream history and contains only focused commits intended for
upstream review. Once a fix is merged, the graph returns atomically to one
exact upstream revision rather than retaining a parallel implementation.

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
