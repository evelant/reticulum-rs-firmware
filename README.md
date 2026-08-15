# Reticulum Rust firmware

Bare-metal Rust firmware and a universal Expo client for a standalone Reticulum
LoRa appliance. The current primary target is the
[Heltec Vision Master E290-HF](docs/heltec-vision-master-e290.md), using its
ESP32-S3R8, 8 MiB PSRAM, 16 MiB flash, HT-RA62-HF/SX1262 radio, and e-paper
display.

The project is an engineering alpha. Two E290 boards and an iOS client have
completed bounded end-to-end proofs, but the firmware is not yet a
general-purpose Reticulum replacement or a production-secure communications
device. See [current status and known limits](docs/status.md) before relying on
it.

## What works

| Area | Current alpha capability |
| --- | --- |
| Reticulum over LoRa | Signed announces, encrypted DATA, path discovery, delivery proofs, and direct Links over the fixed NA915 E290 profile |
| LXMF | Durable basic messages using opportunistic delivery or a one-packet direct Link, optional Sideband-compatible sender location, and delayed proof until receiver commit |
| Radio evidence | Unreleased API 1.14 source retains inbound receiver-local first-arrival interface and optional RSSI/SNR; **Measure path** runs a one-shot Reticulum proof probe; powered field qualification is pending |
| NomadNet | Discovery and one bounded static Micron page request/response |
| Local app connection | Authenticated BLE device API, fileless six-digit onboarding, persistent bonds, and multiple isolated appliance profiles |
| Display | Boot/readiness state, pairing instructions and passkey, board suffix, configured LoRa/BLE/LXMF/Nomad state, and a durable `NEW n` message indicator |
| Client | One TypeScript/Expo codebase for web, iOS, and Android, including foreground/resume local message notifications; physical iOS BLE-to-LoRa use is qualified, while Android hardware and locked-phone BLE wake remain unqualified |
| Persistence | Durable node identity, credentials, BLE bond, outbound journal, raw RNS inbox fixture, LXMF inbox, and an identity-bound client-collection watermark |

LoRa is the first Reticulum packet interface, not a global transport
assumption. The node, routing registry, and interface router use stable
interface identities so future Wi-Fi, BLE, USB, or additional-radio packet
links can coexist without pretending to be LoRa. BLE and the Wi-Fi SoftAP are
local client bearers. The optional Wi-Fi station/TCP profile also registers one
outbound Reticulum packet interface as interface 2; its current bounded
qualification is not full LoRa-to-TCP/TCP-to-LoRa border-routing proof.

## Quick start

1. [Build, package, and flash the E290 firmware](docs/getting-started/firmware-e290.md).
2. [Build and install the Expo app](docs/getting-started/app.md).
3. [Pair a phone, add another appliance, and switch boards](docs/getting-started/pairing.md).

Keep an antenna appropriate for the selected frequency attached before
powering or flashing a radio-bearing image. Fresh configuration uses the
915 MHz NA915 development profile; the app can save a different compatible
frequency/modulation tuple for the next restart. Operate only where the
selected profile is permitted. The image is for the E290-HF/HT-RA62-HF
variant; do not use it on the LF radio variant.

## Documentation

Start at the [documentation index](docs/README.md). The most useful entry
points are:

- [current architecture](docs/architecture/overview.md);
- [status and known limits](docs/status.md);
- [device API](docs/api/device-api-v1.md);
- [architecture decisions](docs/adr/README.md);
- [dependency and source provenance](docs/provenance.md); and
- [complete verification guide](docs/development/verification.md).

The large [architecture and feasibility record](docs/firmware-architecture.md),
[E290 implementation dossier](docs/e290-node.md), HIL runbooks, and powered
proofs preserve revision-bound engineering evidence. They are reference
records, not the normal build or installation path.

## Repository layout

```text
clients/appliance/   Universal Expo app and native Rust bridge
crates/              Portable protocol, routing, storage, API, and board crates
firmware/            Board images and hardware qualification binaries
docs/                Guides, architecture, decisions, limits, and proof records
interop/             Released-Python vectors and interoperability tooling
partitions/          Checked-in ESP32 partition tables
tools/               Host utilities and conformance tools
vendor/              Reviewed local dependency patches
xtask/               Repository policy, build, fixture, and HIL commands
reference/           Ignored local research checkouts and hardware references
```

## Basic verification

The pinned host toolchain is selected by `rust-toolchain.toml`.

```sh
cargo fmt -- --check
RUST_MIN_STACK=16777216 cargo test --locked
cargo run --locked -p xtask -- graph-policy

cd clients/appliance
bun install --frozen-lockfile
bun run verify
```

ESP32-S3 compilation and image packaging require the separate Espressif
toolchain and the explicit E290 procedure in the
[firmware guide](docs/getting-started/firmware-e290.md). Do not use a generic
`cargo run` or implicit `espflash` defaults for an E290 image.

## License

Project-owned source code is licensed under
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE). Reticulum-derived
compatibility material retains the applicable
[Reticulum license](LICENSE-RETICULUM) and notices. Individual comparison or
reference components may use other FOSS licenses; see
[NOTICE](NOTICE) and [source provenance](docs/provenance.md).
