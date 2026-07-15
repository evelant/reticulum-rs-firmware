# Reticulum Rust firmware

Bare-metal Rust firmware for a standalone Reticulum LoRa node. The first
target is the ESP32-S3 and SX1262/KCT8103L combination on the Heltec Wireless
Tracker V2.3, but protocol and application code will remain portable across
boards and radios.

The repository is in **Phase 0: foundation and conformance**. It does not yet
provide working Reticulum or LXMF firmware. The current firmware binary is a
deliberately RF-disabled compile probe: it holds the SX1262 in reset and the
external front-end controls low.

## Read first

- [Architecture](docs/firmware-architecture.md)
- [Phase-0 scaffold decision](docs/adr/0001-phase-0-scaffold.md)
- [Phase-0 acceptance contract](docs/phase-0-acceptance.md)
- [Dependency provenance](docs/provenance.md)

## Toolchains

Host tools and portable crates use the Rust version pinned by
`rust-toolchain.toml`. ESP32-S3 builds use Espressif's separately installed
Xtensa toolchain:

```sh
espup install --targets esp32s3 \
  --toolchain-version 1.95.0.0 \
  --name esp
source ~/export-esp.sh
```

The export step is required for the Xtensa GCC linker. Check the local setup:

```sh
cargo run -p xtask -- doctor
```

## Initial checks

```sh
cargo test --locked
cargo check --locked \
  -p reticulum-rns-conformance \
  -p reticulum-rns-rete \
  -p reticulum-radio-interface \
  -p reticulum-board-heltec-tracker-v2 \
  --target riscv32imac-unknown-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --target xtensa-esp32s3-none-elf
```

The Tracker binary must remain TX-disabled until a board revision, antenna,
region, frequency and conservative power profile are explicitly selected.
There is intentionally no default LoRa frequency.

## Source layout

```text
crates/          portable contracts, the Rete candidate, and board data
comparisons/     separately licensed alternative RNS candidate graphs
firmware/        target binaries
interop/         pinned peer revisions and generated-vector provenance
tools/           host conformance runners
xtask/           reproducible development commands and environment checks
reference/       ignored research checkouts; never a build dependency
```

Project-owned code is licensed under either MIT or Apache-2.0. Separately
licensed candidate and future derived-code boundaries are documented in
`docs/provenance.md`.
