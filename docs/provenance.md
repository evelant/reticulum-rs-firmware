# Dependency and source provenance

This file records reviewed source decisions that are not fully represented by
the crates.io registry and `Cargo.lock`. Exact resolved crate versions remain
authoritative in the lockfile.

## Current direct sources

| Component | Source | Pin | License used here | Build role |
| --- | --- | --- | --- | --- |
| Project-owned crates | This repository | current tree | MIT OR Apache-2.0 | Product and shared tooling |
| Rete | <https://github.com/s-retlaw/rete> | `9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743` | Apache-2.0 option from upstream declaration | Lead RNS evaluation and firmware compile graph |
| Leviculum | <https://codeberg.org/Lew_Palm/leviculum> | `5fb1db0e5e5a490291ee5f6b81312cf0c9de622a` | AGPL-3.0-or-later | Separate comparison package only |
| esp-hal family | <https://github.com/esp-rs/esp-hal> | crates.io versions in lockfile | MIT OR Apache-2.0 | ESP32-S3 platform |
| lora-phy | <https://github.com/lora-rs/lora-rs> | crates.io version in lockfile | MIT OR Apache-2.0 | Compile-time radio-driver integration; radio is not initialized yet |

Rete's reviewed snapshot declares `MIT OR Apache-2.0` in Cargo metadata and
its README but does not contain canonical license files. This is release
packaging hygiene to resolve with upstream or in the corresponding-source
bundle; it is not being silently inferred from code.

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
