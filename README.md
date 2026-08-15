# Reticulum appliance

Bare-metal Rust firmware and a universal Expo client for a standalone
Reticulum mesh appliance. The primary hardware target is the
[Heltec Vision Master E290-HF](docs/hardware/e290.md): an ESP32-S3R8 with
8 MiB PSRAM, 16 MiB flash, an HT-RA62-HF/SX1262 LoRa radio, and an e-paper
display.

The appliance remains a Reticulum node while every client is disconnected. It
routes traffic over LoRa, can connect to an upstream Reticulum TCP peer over
Wi-Fi, stores LXMF messages and delivery state, and exposes an authenticated
local API to the Expo app over BLE. LoRa is the first packet interface, not a
global assumption: routing, persistence, and application services are designed
to support additional boards, radios, and Reticulum interfaces.

This is alpha software. It is useful for development and field testing, but it
is not a production-secure communications device.

## Current functionality

- Reticulum announces, path discovery, encrypted DATA, proofs, forwarding, and
  direct Links over LoRa.
- Durable LXMF send, receive, retry, message requests, contacts, location
  attachment, and receiver-local RSSI/SNR evidence.
- Nearby-peer, route, interface, radio, and packet-correlated diagnostics.
- A bounded NomadNet/Micron page service.
- Authenticated BLE onboarding with an e-paper passkey and board-only bond
  recovery.
- Wi-Fi station management and an optional outbound Reticulum TCP interface.
- Manual and automatic announces plus opt-in RMAP discovery and location.
- One TypeScript/Expo client for iOS, Android, and web, with native Rust
  ownership of credentials, sessions, and durable app data.

See the [roadmap and current limitations](docs/roadmap.md) for important gaps.

## Start here

1. [Build and flash the E290 firmware](docs/getting-started/firmware-e290.md).
2. [Build and install the Expo app](docs/getting-started/app.md).
3. [Pair the app with an appliance](docs/getting-started/pairing.md).

Keep an antenna suitable for the selected frequency attached whenever the radio
may transmit. The firmware is for the E290-HF/HT-RA62-HF path; do not flash it
onto an LF radio variant. The operator is responsible for selecting a legal
frequency, bandwidth, power, antenna, and duty cycle.

## Repository layout

```text
clients/appliance/  Expo app and native Rust bridge
crates/             Portable protocol, routing, storage, and client libraries
firmware/           E290 firmware composition
docs/               Current architecture, guides, reference, and roadmap
interop/             Reticulum and LXMF compatibility vectors
partitions/          ESP32-S3 partition tables
vendor/              Audited dependency overlays
xtask/               Recurring build and validation commands
reference/           Ignored local research material
```

The [documentation index](docs/README.md) links the current design and
development guides.

## Verify the workspace

```sh
cargo fmt --all -- --check
RUST_MIN_STACK=16777216 cargo test --locked
RUST_MIN_STACK=16777216 cargo test --locked -p reticulum-e290-firmware --lib
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -p xtask -- doctor

cd clients/appliance
bun install --frozen-lockfile
bun run verify
```

ESP32-S3 builds require the Espressif toolchain and the additional target gates
in the [firmware guide](docs/getting-started/firmware-e290.md).

## License

Project-owned source is licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE). Reticulum-derived compatibility material retains
the applicable [LXMF](LICENSE-RETICULUM) and
[RNS](LICENSE-RETICULUM-RNS) Reticulum license notices. See [NOTICE](NOTICE)
and [dependency provenance](docs/reference/dependencies.md).
