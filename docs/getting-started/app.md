# Build and install the Expo app

The Reticulum appliance client uses one TypeScript/React Native source tree for
web, iOS, and Android. Project-owned package management, scripts, tests, and
build orchestration use the exact Bun version pinned in
`clients/appliance/package.json`. Expo's own CLI still invokes its required
Node runtime internally.

Install a current Node.js LTS release, then install the repository's exact Bun
release:

```sh
curl -fsSL https://bun.sh/install | bash -s "bun-v1.3.13"
node --version
bun --revision
```

`bun --revision` must report
`1.3.13+bf2e2cecf27e800962b1e7f03d66278f9d5d2e79`. The scripts fail early on
another Bun build so generated assets cannot silently drift.

All commands in this guide start from:

```sh
cd clients/appliance
bun install --frozen-lockfile
bun run verify
```

`verify` checks the Bun revision, exact Expo dependency set, formatting,
strict TypeScript, tests, Rust-generated API types, and deterministic embedded
web assets. It does not require Xcode or the Android SDK.

## Web

Start the development server:

```sh
bun run web
```

This is a UI-only development server. The web app uses the appliance service's
same-origin HTTP adapter, so appliance calls fail when the page is served
directly by Expo. It does not use the native Rust bridge or connect directly
to BLE.

For a functional web client, rebuild the deterministic assets, then rebuild
and run the Rust host service against a paired board:

```sh
bun run build:web
bun run assets:check
cd ../..
cargo build --locked -p reticulum-lxmf-chat-service
target/debug/reticulum-lxmf-chat-service --discover
target/debug/reticulum-lxmf-chat-service \
  --usb-serial <12-hex-device-id> \
  --profile-root "$HOME/.local/share/reticulum-lxmf-chat"
```

Open the complete capability URL printed by the service. `build:web` updates
`crates/lxmf-chat-service/assets/{app.js,index.html,style.css,manifest.json}`;
the following Cargo build embeds those files in the executable. This is not a
generic `dist` deployment. See the
[host-service guide](../../crates/lxmf-chat-service/README.md) for BLE mode,
explicit credential paths, and onboarding details.

## iOS

### Prerequisites

- macOS on Apple Silicon;
- Xcode with an available simulator or signing identity;
- CocoaPods with `pod --version` working;
- the repository's pinned Rust 1.97.0 toolchain; and
- the iOS device and Apple-Silicon simulator Rust targets.

```sh
rustup target add --toolchain 1.97.0 \
  aarch64-apple-ios \
  aarch64-apple-ios-sim
```

The generated native bridge currently supports arm64 devices and
Apple-Silicon arm64 simulators, with a minimum iOS version of 16.4. Intel iOS
simulators are not supported.

### Simulator Debug build

```sh
bun run ios
```

This regenerates the Rust/UniFFI bindings, performs a clean Expo prebuild,
compiles the app, installs it in the selected simulator, launches it, and
starts Metro.

### Physical-device Debug build

```sh
bun run ios -- --device <device-name-or-UDID>
```

Select a signing team if Xcode requests one. Debug builds do not embed the
application script and therefore require a reachable Metro server. After the
development app is installed, TypeScript-only changes normally need only:

```sh
bun run start
```

### Self-contained physical-device Release

The Release wrapper performs the same clean binding/prebuild sequence, builds
the Rust bridge with Cargo's release profile, opens Expo's device picker,
builds, installs, launches, and verifies the generated application:

```sh
bun run release:ios
```

Choose an attached physical phone, or bypass the picker:

```sh
bun run release:ios -- --device <device-name-or-UDID>
```

The wrapper owns `--configuration Release`, `--no-bundler`, and the clean
`.tmp/ios-release` output. It rejects overrides that could install an old
binary or skip installation. A Release Xcode build still runs Metro once to
embed `main.jsbundle`; the wrapper fails unless that bundle exists and is
nonempty after installation. The wrapper targets physical phones; Intel
simulator Release support is outside the current native bridge target set.

## Android

### Prerequisites

- Android SDK, NDK, platform tools, a compatible JDK, and a configured emulator
  or USB-debuggable device;
- `cargo-ndk`; and
- all four Rust Android targets used by the generated Expo application.

```sh
cargo install cargo-ndk --locked
rustup target add --toolchain 1.97.0 \
  aarch64-linux-android \
  armv7-linux-androideabi \
  i686-linux-android \
  x86_64-linux-android
```

The binding script checks the SDK/NDK environment and reports missing targets.

### Emulator or device Debug build

```sh
# Default configured emulator
bun run android

# Select a physical device or emulator
bun run android -- --device <device-name>
```

The wrapper regenerates bindings, performs a clean prebuild, compiles, installs,
launches, and starts Metro.

### Local Release build

```sh
bun run release:android
```

Choose the attached phone or emulator, or bypass the picker:

```sh
bun run release:android -- --device <device-name>
```

The wrapper builds the Rust bridge with Cargo's release profile, owns
`--variant release` and `--no-bundler`, performs the install and launch, and
fails unless
`android/app/build/outputs/apk/release/app-release.apk` is a nonempty fresh
artifact. This is a local development-signed Release build, not an app-store
artifact. Android native compilation is supported, but the physical BLE
onboarding and messaging path has not yet completed the powered hardware
qualification already performed on iOS.

## Two-phone test setup

With an iPhone and Android phone attached and unlocked, run:

```sh
bun run release:ios -- --device <iPhone-name-or-UDID>
bun run release:android -- --device <Android-device-name>
```

For two phones on the same platform, run that platform's command once per
explicit device. Run native build commands sequentially because each
regenerates shared Rust bindings and performs a clean Expo prebuild. After both
installs complete, use the [pairing guide](pairing.md) to pair each phone with
its own E290.

## Native and generated-code workflow

Expo Go cannot run this app because it contains a custom native TurboModule.
The application-level `ios/` and `android/` directories are generated,
disposable, and ignored.

- After changing serialized Rust API DTOs, run `bun run api:generate`.
- After changing the native Rust boundary, `bun run ios` or `bun run android`
  regenerates the needed bindings automatically.
- To generate without running, use `bun run native:bindings:ios` or
  `bun run native:bindings:android`.
- `bun run native:verify` checks both platforms and therefore requires both
  Apple and Android toolchains.

Once the native app is installed, continue with
[fileless appliance pairing](pairing.md).
