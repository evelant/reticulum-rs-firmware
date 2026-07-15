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
| lora-phy | <https://github.com/lora-rs/lora-rs> | crates.io version in lockfile | MIT OR Apache-2.0 | Compile-time radio-driver integration; radio is not initialized yet |

The RNode LoRa header and split/reassembly behavior in
`crates/radio-interface` is an independent project-owned implementation of the
published wire behavior. It was checked against the retained Rete
`rete-iface-lora` implementation at `9bcb7d3e…` and the working
`microReticulum_Firmware` Tracker reference; no source from either checkout is
copied into the crate. The four-bit sequence format cannot distinguish a
same-sequence duplicate from a continuation, so that limitation is documented
and tested rather than hidden behind a stronger private framing scheme.

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
