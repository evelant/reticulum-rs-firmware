# ADR 0001: Phase-0 scaffold and dependency boundaries

- **Status:** accepted
- **Date:** 2026-07-14
- **Decision owners:** project maintainers
- **Supersedes:** the generated single-package placeholder

## Context

The architecture investigation selected Rete as the leading embedded RNS
candidate, Leviculum as the comparison/fallback, and selective LXMF-rs source
reuse as the leading LXMF path. None is yet accepted as the production
firmware foundation. Phase 0 must compare real behavior and memory rather than
hide candidate differences behind a large speculative abstraction.

The local `reference/` directory contains research snapshots and is ignored by
Git. Depending on those paths would make builds depend on one workstation and
would disconnect the lockfile from reviewed upstream revisions.

The ESP32-S3 uses Xtensa and therefore needs Espressif's Rust compiler fork.
Host tests and portable checks should not inherit an Xtensa default target.

## Decision

### Build only the Phase-0 surface

The initial workspace contains:

- a small, backend-neutral conformance vocabulary with protocol-size
  invariants, candidate metadata and report types, but no universal RNS trait;
- a Rete evaluation crate pinned to one upstream commit;
- a separately licensed Leviculum comparison crate;
- a portable RNode/LoRa boundary crate;
- immutable Tracker V2.3 board facts and safe-state policy;
- an RF-disabled Tracker firmware compile probe;
- a host Rete report runner, interoperability peer manifest and `xtask`.

LXMF routing/propagation, storage, device API, NomadNet, Micron, SPA, BLE,
mobile, OTA and GNSS crates will be added when their implementation phase
begins. Empty aspirational crates are intentionally omitted.

### Pin reproducible sources

Production manifests must not use dependencies below `reference/`.

- Rete crates use the full upstream Git revision
  `9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743`.
- Leviculum uses the full upstream Git revision
  `5fb1db0e5e5a490291ee5f6b81312cf0c9de622a` only from its comparison
  package graph.
- `Cargo.lock` is committed and CI uses `--locked`.
- The first required Rete source change triggers a project fork that retains
  upstream history. Vendoring is reserved for an offline/release requirement.

Future source adapted from LXMF-rs will enter a local, explicitly EPL-2.0
crate with file-level provenance. The current scaffold does not depend on
`rete-lxmf-core`, `rete-lxmf`, or LXMF-rs.

### Keep candidate differences visible

The shared conformance crate does not define a production `PacketInterface`,
`RadioPhy`, `NodeCore`, event enum or storage trait. Phase 0 first records each
candidate's native input, output, allocation and failure behavior. A narrow
production command/event seam will be designed only after one foundation has
passed the acceptance contract.

The non-negotiable wire boundaries are recorded now:

- base RNS packets: at most 500 bytes;
- RNode physical-interface packets: at most 508 bytes;
- SX1262 LoRa frames: at most 255 bytes;
- one RNode LoRa framing byte leaves 254 data bytes per radio frame;
- a TX buffer remains owned until the radio reports completion or failure.

These boundaries do not imply that the present framing scaffold is a complete
codec.

### Use separate toolchain lanes

- Root host toolchain: Rust 1.97.0, pinned by `rust-toolchain.toml`.
- Workspace MSRV: Rust 1.95, matching the base of the current Xtensa fork.
- ESP32-S3 toolchain: the `esp` rustup toolchain, whose compiler fingerprint
  must be Espressif Rust 1.95.0.0.
- Portable crates are checked on `riscv32imac-unknown-none-elf` as an ordinary
  upstream-Rust `no_std` target.
- The root Cargo configuration contains target-specific Xtensa link flags but
  no workspace-wide default target.

The initial ESP dependency set follows the current `esp-generate` 1.3 family:
`esp-hal` 1.1.1, `esp-rtos` 0.3.0, Embassy executor 0.10, and `lora-phy` 3.0.1.
Wi-Fi and BLE dependencies are deliberately absent.

### Default to physically disabled RF

The scaffold has no frequency, modulation or power default. At boot it holds:

- SX1262 reset (GPIO 12) low;
- KCT8103L VFEM power (GPIO 7) low;
- KCT8103L CSD (GPIO 4) low;
- KCT8103L CTX (GPIO 5) low.

It does not initialize SPI, SX1262, Rete, Wi-Fi, BLE, display or GNSS. Enabling
receive requires an explicit later lab capability. Enabling transmit requires
the actual board revision, antenna, regulatory region, frequency, airtime and
power policy plus a driver-level fail-closed TX interlock.

### Preserve license graph boundaries

Project-owned crates use `MIT OR Apache-2.0`. The Leviculum comparison package
is AGPL-3.0-or-later and is not linked into the Rete firmware product. A future
LXMF-rs-derived crate will explicitly use EPL-2.0. All release artifacts will
carry generated dependency/license manifests and applicable source/notices.

## Consequences

- The repository can start measurable implementation without pretending the
  RNS selection is complete.
- Ordinary `cargo test` remains a host operation, while firmware builds always
  name the Xtensa target and toolchain.
- The BSP is currently board data plus safety policy, not a generic `Board`
  trait. Concrete peripheral ownership and FEM sequencing will be introduced
  with radio bring-up.
- Some architecture-layer APIs remain deliberately unspecified until Phase 0
  exposes their required receive, completion, cancellation, error and
  backpressure semantics.

## Deferred decisions

This ADR does not choose measured table/storage quotas, a PSRAM acceptance
board, persistent record formats, propagation peer policy, device API schema,
browser/BLE ordering, secure manufacturing mode, RNode bridge product status,
OTA layout or GNSS behavior.
