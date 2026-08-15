# Build and install the Expo app

The client uses one TypeScript/React Native source tree for web, iOS, and
Android. Project scripts and package management use the exact Bun release in
`clients/appliance/package.json`; Expo invokes Node where its own toolchain
requires it.

## Install dependencies

Install a current Node.js LTS release and the pinned Bun version, then run the
portable checks:

```sh
cd clients/appliance
bun install --frozen-lockfile
bun run verify
```

`verify` checks the Bun revision, dependency alignment, formatting, TypeScript,
tests, generated API bindings, release scripts, and deterministic web assets.

Expo Go cannot run this project because the app contains a custom native Rust
TurboModule.

## Web

For UI-only development:

```sh
bun run web
```

The browser does not connect directly to BLE. On macOS, a functional web
client uses the supported Rust host gateway, which owns one authenticated BLE
connection and serves the generated Expo web bundle:

```sh
bun run build:web
bun run assets:check
cd ../..
cargo build --locked -p reticulum-appliance-service
```

The service requires an activated device credential; first-run pairing belongs
to the native app. With the credential installed at
`PROFILE_ROOT/devices/EUI48/credential.rdpkey`, start the service for that
board and open the complete capability URL it prints:

```sh
target/debug/reticulum-appliance-service \
  --eui48 <12-hex-board-eui48> \
  --profile-root "$HOME/.local/share/reticulum-appliance"
```

The profile tree and credential must be owner-private. The app bundle is
embedded in the host executable, so rebuild the web assets before rebuilding
the service after TypeScript changes. See the
[host service README](../../crates/appliance-service/README.md) for explicit
credential/database paths and optional BLE peripheral selection.

## iOS

Requirements:

- macOS on Apple Silicon;
- Xcode, CocoaPods, and a signing identity for physical devices; and
- the arm64 iOS device and Apple-Silicon simulator Rust targets.

```sh
rustup target add --toolchain 1.97.0 \
  aarch64-apple-ios \
  aarch64-apple-ios-sim
```

Run a simulator or signed development build:

```sh
bun run ios
bun run ios -- --device <device-name-or-UDID>
```

Debug builds use Metro. Install a self-contained Release build for field use
with:

```sh
bun run release:ios
bun run release:ios -- --device <device-name-or-UDID>
```

The release wrapper regenerates native bindings, performs a clean Expo
prebuild, embeds the JavaScript bundle, builds, installs, launches, and checks
the resulting app. Intel/x86_64 Apple targets are not supported.

## Android

Install the Android SDK/NDK, platform tools, a compatible JDK, `cargo-ndk`, and
the Rust Android targets used by the generated project:

```sh
cargo install cargo-ndk --locked
rustup target add --toolchain 1.97.0 \
  aarch64-linux-android \
  armv7-linux-androideabi \
  i686-linux-android \
  x86_64-linux-android
```

Run a debug build or install a local development-signed Release build:

```sh
bun run android
bun run android -- --device <device-name>

bun run release:android
bun run release:android -- --device <device-name>
```

The release APK is a local test artifact, not an app-store package.

## Generated boundaries

Do not hand-maintain a second TypeScript device model or edit generated native
bindings.

- After changing serialized Rust application DTOs, run `bun run api:generate`.
- After changing the native Rust surface, run `bun run native:bindings`.
- Native run and release scripts regenerate the required platform bindings.
- Run `bun run native:bindings:check` to detect drift without regenerating.
- Run `bun run native:verify` when both Apple and Android toolchains are
  available.

Application-level `ios/` and `android/` directories are disposable generated
projects. Continue with [appliance pairing](pairing.md) after installing a
native build.

## Reset incompatible app data

The alpha client opens only the current per-appliance SQLite schema and leaves
an unknown schema untouched. If a saved appliance reports an unsupported local
database, switch to another profile and choose **Forget** for the incompatible
one. When it is the only profile, clear the app's data or reinstall the app,
then add the appliance again. This deletes phone-local messages, contacts, and
outbox state; it does not erase messages or identity state held by the board.
