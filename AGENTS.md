# Coding agent guide

This repository builds a standalone Reticulum appliance and its universal Expo
client. The primary hardware is the Heltec Vision Master E290-HF
(ESP32-S3R8, HT-RA62-HF/SX1262, e-paper, 16 MiB flash, and at least 8 MiB
mapped PSRAM). LoRa is the first packet interface, not a global transport
assumption.

## Repository map

- `firmware/e290/`: the supported E290 firmware image and board composition.
- `crates/`: portable application protocols, storage, display, and client
  libraries. PRNS is an exact git dependency rather than an in-tree wrapper.
- `clients/appliance/`: the TypeScript/Expo application and native Rust bridge.
- `interop/`: deterministic compatibility vectors and their generators.
- `partitions/`: current ESP32 flash layouts.
- `xtask/`: small, recurring repository and firmware operations.

The client-side Rust layers are `appliance-store`, `appliance-sync`,
`appliance-runtime`, and `appliance-service`. Keep durable state, device
synchronization, protocol semantics, and service behavior in those Rust layers;
TypeScript owns presentation and platform integration.

## Architecture invariants

- The board operates and retries messages without a connected app.
- Persist outbound intent before acknowledging it to a client.
- Preserve PRNS/Python immediate proof timing. Persist and deduplicate inbound
  LXMF after delivery without adding deferred proofs.
- Keep routing and application services independent of LoRa and board details.
- Model LoRa, TCP, and Bluetooth Auto as Reticulum packet interfaces. Product
  operations use Links, requests, and Resources; USB Serial/JTAG is reserved
  for diagnostics and recovery.
- Give each radio, flash device, display, session, and network actor one owner.
  Use bounded queues and explicit ownership transfer.
- The complete E290 image may rely on PSRAM. Do not constrain the product to
  non-PSRAM boards.
- Supported firmware emits useful diagnostics over USB Serial/JTAG.

## Working rules

- Prefer modules and ordinary unit or integration tests over new crates.
- Add a tool only for a recurring operator or repository workflow. Do not add
  phase-specific, proof-only, or one-off crates, binaries, fixtures, or docs.
- Keep documentation current-tense and describe the present system and roadmap,
  not implementation chronology.
- Use TypeScript and Bun exclusively for app-side source and scripts.
- Prefer upstream dependency releases and fixes over vendoring or backports.
  Keep each necessary overlay small and document its removal condition.
- Use the exact PRNS revision pinned in the workspace without product-local
  patches. Adapt product design to PRNS public APIs first. Change PRNS only for
  a demonstrated generic gap useful to unrelated Reticulum applications or
  boards, and never place appliance or LXMF policy in PRNS core.
- Application data uses typed quotas inside one generic `product_state` arena;
  do not create physical partition layouts for application combinations.
- Alpha API and storage compatibility may change. Make reset or migration
  consequences explicit when persisted formats change.
- Add or update doc comments where ownership, durability, timing, protocol, or
  hardware behavior is not obvious from the types.
- Do not open an external issue or pull request without direct user approval.

## Generated code

Rust is the source of truth for shared API types. Do not duplicate those types
or hand-edit generated TypeScript, UniFFI, C++, Kotlin, Objective-C++, CMake,
Gradle, podspec, framework, JNI, or embedded web output.

Run generation from `clients/appliance`:

```sh
bun run api:generate
bun run native:bindings
bun run build:web
```

Commit generated output together with its source change.

## Verification

Run the host checks from the repository root:

```sh
cargo fmt --all -- --check
RUST_MIN_STACK=16777216 cargo test --locked
RUST_MIN_STACK=16777216 cargo test --locked -p reticulum-e290-firmware --lib
cargo clippy --locked --all-targets -- -D warnings
```

Protocol or wire changes also require the isolated Python authority checks in
`interop/README.md`; its RNS and LXMF suites intentionally use different pinned
dependency environments.

Run the app checks from `clients/appliance`:

```sh
bun install --frozen-lockfile
bun run verify
```

Run `bun run native:verify` when native bindings change and both Apple and
Android toolchains are available.

Build and package the default gateway image from the repository root:

```sh
source "$HOME/export-esp.sh"
firmware/e290/bootloader/build-container.sh
cargo run --locked -p xtask -- doctor
cargo run --locked -p xtask -- build
cargo run --locked -p xtask -- check-elf
cargo run --locked -p xtask -- package --output target/e290-gateway.bin
```

For the LoRa/BLE-only image, pass `--profile appliance` to `build` and
`package`, and write it to `target/e290-appliance.bin`. Follow
`docs/getting-started/firmware-e290.md` for deliberate port selection and the
exact `espflash` commands. Do not substitute generic `cargo run`, implicit
flash geometry, or an `--all-features` target build.
