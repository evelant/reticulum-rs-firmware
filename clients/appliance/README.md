# Reticulum appliance client

This is the universal Expo application for the Reticulum appliance. One
TypeScript/React Native source tree targets iOS, Android, and web. Native Rust
owns one persisted PRNS node and identity, identified management Links, durable
per-appliance SQLite data, and message synchronization; TypeScript owns
presentation and platform integration.

Use the [app build guide](../../docs/getting-started/app.md) for prerequisites
and installation, and the [pairing guide](../../docs/getting-started/pairing.md)
for onboarding and recovery.

## Contributor loop

```sh
bun install --frozen-lockfile
bun run verify
bun run web
```

Expo Go cannot load the custom native module. Use the project wrappers for a
native build:

```sh
bun run ios
bun run android

bun run release:ios
bun run release:android
```

Pass `-- --device <name-or-id>` to select a target. Run `bun run start` after a
Debug build is installed to use Metro and Fast Refresh.

## Generated boundaries

- `src/generated/api.ts` is generated from Rust DTOs with `ts-rs`.
- `modules/appliance-native` packages the Rust core through UniFFI and an Expo
  TurboModule.
- `bun run api:generate` updates serialized application types.
- `bun run native:bindings` updates both native bridges.
- `bun run build:web` writes the deterministic embedded bundle to
  `../../crates/appliance-service/assets`.

Do not hand-edit generated TypeScript, C++, Kotlin, Objective-C++, CMake,
Gradle, podspec, framework, JNI, or Expo project output. The application-level
`ios/` and `android/` directories are disposable Continuous Native Generation
products.

## Source layout

```text
src/app/                  Expo Router screens
src/generated/            Rust-generated serialized types
src/lib/                  state, presentation adapters, and tests
modules/appliance-native/ local native module package
scripts/                  Bun/TypeScript build and verification tools
```

## Product semantics

Each appliance profile is keyed by a verified management destination and has
an isolated database. One app-owned Reticulum identity can be enrolled with
multiple appliances. Contacts are phone-local names for LXMF destinations;
receiving from an unsaved sender does not require a reciprocal contact.
Accepted sends are durable on the board, which owns delivery retry while the
app is disconnected.

The Activity, message-details, Network, and Map surfaces project the same
durable message and radio evidence. Receiver RSSI/SNR is final-hop data and may
describe a relay. Retained routes are not a live peer list, and map lines join
phone observations rather than reconstructing RF paths.

The web target uses the local Rust service as its appliance gateway. Browsers
do not own a PRNS node or connect directly to Bluetooth. Native builds use the
Rust PRNS/profile/database owner. Locked-phone Bluetooth Auto collection and
notification delivery remain future work.
