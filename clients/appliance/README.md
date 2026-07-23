# Reticulum appliance client

This is the universal Expo client for the Reticulum appliance. The same TypeScript/React Native
source targets web, iOS, and Android. Native projects are generated with Expo Continuous Native
Generation and are deliberately not committed.

The embedded browser build is an Expo single-page export. A Bun TypeScript build step validates
Expo's output, embeds Metro-owned image resources into the bundle, and reduces the runtime to the
three fixed files served by `reticulum-lxmf-chat-service`:

- `app.js`
- `index.html`
- `style.css`

The adjacent tracked `manifest.json` records deterministic build provenance; it is not served to
the browser.

Wire DTOs in `src/generated/api.ts` are generated from Rust. Never edit or duplicate them by hand.

## Toolchain

Bun `1.3.13` at revision `bf2e2cecf` is required. Package versions and the Bun lockfile are exact.
Expo SDK 57 requires Metro for universal React Native bundling. All project-owned scripts are
TypeScript launched by Bun, and Bun owns package installation and the lockfile. Expo's native CNG
command retains its upstream Node runtime because Bun 1.3.13 corrupts the copied Xcode project when
it runs that command directly. There are no authored JavaScript files or Node-owned project scripts.
The native Rust boundary additionally requires the repository's pinned Rust toolchain and:

- Android SDK/NDK, `cargo-ndk`, and the four Rust Android targets listed by
  `scripts/native-bindings.ts`; or
- Xcode, CocoaPods, and the `aarch64-apple-ios` and `aarch64-apple-ios-sim` Rust targets.

The checked iOS XCFramework contains arm64 device and Apple-Silicon arm64 simulator slices. An Intel
iOS simulator is not currently a native-build target.

```sh
bun install --frozen-lockfile
bun run verify
```

Use `bun run api:generate` after changing Rust wire types and `bun run build:web` after changing the
client. `bun run assets:check` performs two clean Expo exports to detect nondeterminism before it
compares the tracked embedded assets. `bun run native:verify` is the opt-in macOS/mobile-toolchain
gate: it regenerates both native bindings, checks tracked output drift, and tests and lints the Rust
bridge. The ordinary `bun run verify` remains portable and does not require Android or Apple tools.

## Development

```sh
bun run web
bun run ios
bun run android
```

`bun run ios` and `bun run android` regenerate the platform's Rust/UniFFI bindings, run a clean Expo
prebuild through the Expo CLI's Node shebang, and invoke `expo run:ios` or `expo run:android`.
Arguments after `--` are forwarded to Expo, for example `bun run ios -- --device`. The resulting
application-level `ios/` and `android/` projects, XCFramework, JNI archives, and platform build
outputs are disposable and ignored. After the development client is installed,
`bun run start` provides the ordinary Metro/Fast Refresh loop for TypeScript-only
changes. `bun run prebuild` remains available when only generated native
projects are needed.

The app defaults to the appliance's same-origin HTTP API on web. Native HTTP is an interim adapter:
set `EXPO_PUBLIC_APPLIANCE_URL` to an accessible appliance origin and open a
`reticulum-appliance://connect?cap=...` link to bootstrap a session. The current Rust alpha server
binds loopback and enforces browser-origin headers, so remote native HTTP needs a deliberate server
transport/authentication policy before it can connect. The client boundary is isolated so a future
BLE, USB, Wi-Fi, or embedded Rust native adapter can replace HTTP without changing screens or DTOs.
