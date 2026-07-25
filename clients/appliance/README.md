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

An iOS Debug build deliberately skips embedding JavaScript and therefore needs a reachable Metro
server. Use a Release configuration for a self-contained physical-device artifact:

```sh
bun run ios -- --configuration Release --device <device-udid> --no-bundler
```

`--no-bundler` prevents Expo from leaving a development server running; the Release Xcode build
still invokes Metro once to embed `main.jsbundle` in the signed application. Before installation,
verify that the generated `Release-iphoneos/ReticulumAppliance.app/main.jsbundle` exists and is
nonempty.

The app defaults to the appliance's same-origin HTTP API on web. Native builds
default to a Rust single-owner actor with the app-private
`reticulum-lxmf-chat-alpha-schema3.sqlite3` database. The schema-3 filename is
deliberately new so submission IDs from a pre-schema-3 device journal cannot
poll or collide after the required journal-only reprovision; the separate
credential file is retained. This already provides durable contacts, timelines,
and idempotent outbox writes while offline. BLE is the default native bearer: React
Native owns foreground scanning, GATT connection, indications, and write-with-response, while Rust
owns the activated credential, authenticated session, protocol framing, and LXMF state. The initial
BLE attempt runs in the background only after the native bridge has validated an app-private
credential, so an absent credential or radio never blocks the offline database. The credential's
E290 device ID selects the exact Rust-generated advertised name rather than whichever matching
board happens to advertise first. While the app remains foregrounded, an unsuccessful attempt
re-arms after two seconds without overlapping the prior GATT operation; retries suspend when the
app backgrounds and resume when it becomes active again. The Reconnect action also retries
discovery and the complete GATT link explicitly. BLE background restoration and phone-native live
pairing are not implemented yet. USB OTG and USB serial/JTAG remain explicit unavailable connector
stubs and do not silently fall back or claim a device connection.

The **Nearby** contact action reads the connected E290's bounded projection of
authenticated `lxmf.delivery` announces through that same BLE session. It does
not scan the other Reticulum node from the phone. Refresh returns at most the
board profile's 32 peers, Rust bounds and decodes the boot-scoped API pages and
announce display data, and one tap adds or opens the existing durable contact.
Manual hexadecimal destination entry remains available. This public peer
discovery is separate from credential import and appliance authorization;
future QR or native-proximity contact cards cannot grant device control.

On compact native layouts, the conversation workspace is keyboard-aware and scrollable. iOS uses
padding plus interactive keyboard dismissal, Android uses height avoidance plus drag dismissal,
and taps on visible actions remain enabled while the keyboard is open. The
[bounded physical iOS proof](../../docs/e290-expo-ios-ble-lora-proof.md) qualified title/body
entry, composer scrolling, and Send-button reachability; it did not qualify rotation,
accessibility text scaling, external keyboards, or Android keyboard variants.

On a fresh native install, the first-run screen can scan for the generated BLE service and list
nearby appliances by advertised name, platform identifier, and RSSI. The user must select a row
explicitly; the app never auto-selects the first advertisement. This bounded scan does not connect,
subscribe, call the Rust authenticated actor, send credentials, or treat the advertised name as
identity or provisioning state. The selected row is currently only preparation for the secure
pairing step described by
[ADR 0019](../../docs/adr/0019-secure-ble-appliance-onboarding.md).

The alpha **credential import** path remains a secondary development fallback. Pair the intended
board through the qualified USB managed-profile workflow, make a temporary copy of its exact
96-byte Active `credential.rdpkey`, name the transfer copy with that board's normalized USB serial,
transfer it to the phone, and choose it in the system file picker. Verify the filename: the current
create-only alpha imports the first canonical credential immediately and has no secret-free
board-identity confirmation screen. Selecting the other board's valid file requires clearing this
app's local data before trying again. The Expo layer copies the selection to an app-owned cache
path without reading its bytes, Rust validates and create-only publishes a mode-`0600` canonical
credential, and the Expo layer removes its cache copy in a `finally` path. On iOS it also deletes
the picker-created temporary copy after staging. Cancelled, malformed, and failed imports do not
start BLE or replace an existing credential. The original transfer file remains outside the app's
control and must be deleted by the user after a successful import.

This import deliberately makes another usable copy of an authentication secret. The canonical
file currently lives in the app's private Documents directory rather than Keychain/Keystore and
may be included in platform backups. Treat the workflow as an alpha bridge for development
devices, not the final provisioning or recovery design. Production work must pair the phone
directly, give each client independently revocable authority, exclude secrets from backup, and add
an identity preview/confirmation with an identity-bound atomic install plus explicit credential
replacement/recovery.

Expo SDK 57's Android file picker takes a persistable content-provider permission and exposes no
API to release it. The app does not persist the selected URI itself, but Android can retain that
read grant until the source is removed or the app is uninstalled. A production Android import
needs a small native picker/release owner (or direct pairing) so successful and failed imports can
revoke the grant deterministically.

Set `EXPO_PUBLIC_APPLIANCE_WIFI_ENDPOINT` to the E290 proof endpoint
`192.168.4.1:29716` to opt a native development build into the first raw-TCP Wi-Fi proof connector.
The connector reloads `reticulum-device-credential.rdpkey` from the app's private Documents
directory for every handshake and uses the separately transcript-bound Wi-Fi session suite. For the
current proof, use the same first-run credential import after pairing over the qualified USB
workflow. Phone-native pairing, Keychain/Keystore migration, SoftAP joining, and credential
rotation remain follow-up work. The session authenticates and integrity-protects API records but
adds no application-layer confidentiality; the initial appliance SoftAP must therefore retain WPA2
and this path must not be described as the final wireless security profile.

The BLE connector currently uses that same app-private
`reticulum-device-credential.rdpkey`. It scans only for the Rust-generated GATT service, subscribes
to the generated TX indication characteristic before declaring the link ready, and caps initial
writes to the generated characteristic value bound. A generation-aware command pump rejects stale
callbacks and reports every platform write exactly once. Platform writes have a ten-second bound so
an OS BLE call cannot outlive Rust's longer ambiguous-write deadline. For an E290 credential, the
bridge derives the exact `reticulum-e290-<MAC suffix>` target from the authenticated device ID.
`EXPO_PUBLIC_APPLIANCE_BLE_NAME` remains an explicit diagnostic fallback for a pre-existing
canonical credential whose namespace does not provide a derivable board name. The current BLE
file-import path rejects such a credential before publication and requires an E290-derived target.
The advertised name and selected filename are discovery hints, not authentication: the suite-3
handshake must still return the credential-bound device ID before Rust accepts the session. Like
the current USB and Wi-Fi profiles, BLE authenticates and integrity-protects device-API records but
does not add application-layer confidentiality; do not use this alpha bearer for sensitive
content.

Set `EXPO_PUBLIC_APPLIANCE_URL` to retain the interim native HTTP adapter during development, then
open a `reticulum-appliance://connect?cap=...` link to bootstrap a session. The current Rust alpha
server binds loopback and enforces browser-origin headers, so remote native HTTP still needs a
deliberate server transport/authentication policy before it can connect. Both adapters implement
the same client boundary and consume Rust-generated semantic DTOs, so adding the remaining USB
connector work does not require changing screens or duplicating interface types.
