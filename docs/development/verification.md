# Verification

Run the smallest relevant check while editing, then run the full host and app
gates before handing off a change. Hardware behavior still requires a board;
host tests do not establish radio, BLE, Wi-Fi, flash, or display behavior.

## Toolchains

Three Rust version numbers appear in the workspace, and each names a different
constraint:

| Number | Where | Meaning |
| --- | --- | --- |
| `1.95` | `rust-version` in `Cargo.toml` | Minimum supported Rust version (MSRV) for every crate |
| `1.97.0` | `rust-toolchain.toml` | The pinned host toolchain used by the `host` CI job and local development |
| `1.95.0.0` | `espup --toolchain-version` | The Espressif Xtensa fork (based on upstream 1.95) used by the `firmware` CI job |

The Xtensa fork is the binding constraint: it tracks upstream releases with a
delay, so the MSRV is `1.95` and portable code must compile with the older
fork. The host toolchain is intentionally newer than the MSRV for better
diagnostics. The firmware CI job compiles and clips the portable crates with
the `+esp` toolchain, so firmware code cannot silently drift onto post-1.95
features; `cargo clippy` on the host alone does not establish MSRV
compatibility.

## Host workspace

From the repository root:

```sh
cargo fmt --all -- --check
RUST_MIN_STACK=16777216 cargo test --locked
RUST_MIN_STACK=16777216 cargo test --locked -p reticulum-e290-firmware --lib
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -p xtask -- doctor
```

`RUST_MIN_STACK` gives the larger protocol and storage tests enough stack on
the host. The explicit firmware library command covers its portable policy and
state-machine tests; the default workspace members exclude the ESP32-S3
firmware. Use the target commands below for embedded compilation and final-ELF
validation.

To check one package while iterating:

```sh
cargo test --locked -p PACKAGE
cargo clippy --locked -p PACKAGE --all-targets -- -D warnings
```

## E290 firmware

Install the Espressif Rust environment, then run:

```sh
cargo run --locked -p xtask -- doctor
cargo run --locked -p xtask -- build
cargo run --locked -p xtask -- check-elf
```

The default firmware profile is `gateway`. Check the smaller BLE appliance
composition separately when changing feature boundaries:

```sh
cargo run --locked -p xtask -- build --profile appliance
```

See [Build and flash E290 firmware](../getting-started/firmware-e290.md) for
direct Cargo equivalents and flashing commands.

## Expo client

From `clients/appliance`:

```sh
bun install --frozen-lockfile
bun run verify
```

Changes to a Rust DTO or native callable surface also require regenerated
artifacts and native checks:

```sh
bun run api:generate
bun run native:bindings
bun run native:verify
```

Generated files are checked in where the client build consumes them. Do not
edit those outputs by hand.

## Interoperability vectors

The deterministic Python compatibility suites are documented in
[interop/README.md](../../interop/README.md). Run them when changing Reticulum,
LXMF, pairing, or authenticated-session encoding.

## Device checks

Before treating a firmware change as usable, check at least:

- boot reaches `Ready` and USB logs remain active;
- the app reconnects over BLE after a board reset;
- pairing and board-only recovery still reach an authenticated session;
- a durable LXMF message survives an app disconnect and board reset;
- LoRa delivery works in both directions between two identically configured
  boards;
- a gateway build can keep BLE usable while Wi-Fi and the TCP interface are
  enabled;
- the display reflects pairing, connection, and unread-message state without
  blocking the node.

For radio changes, use the controlled procedure in
[Range testing](range-testing.md). Record failures as tests when they can be
reproduced without hardware, and keep device-specific observations in an issue
or exported diagnostic file rather than the live design reference.
