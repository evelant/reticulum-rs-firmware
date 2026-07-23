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

```sh
bun install --frozen-lockfile
bun run verify
```

Use `bun run api:generate` after changing Rust wire types and `bun run build:web` after changing the
client. `bun run assets:check` performs two clean Expo exports to detect nondeterminism before it
compares the tracked embedded assets.

## Development

```sh
bun run web
bun run ios
bun run android
```

For a development build, `bun run prebuild` creates disposable `ios/` and `android/` directories.
The app defaults to the appliance's same-origin HTTP API on web. Native HTTP is an interim adapter:
set `EXPO_PUBLIC_APPLIANCE_URL` to an accessible appliance origin and open a
`reticulum-appliance://connect?cap=...` link to bootstrap a session. The current Rust alpha server
binds loopback and enforces browser-origin headers, so remote native HTTP needs a deliberate server
transport/authentication policy before it can connect. The client boundary is isolated so a future
BLE, USB, Wi-Fi, or embedded Rust native adapter can replace HTTP without changing screens or DTOs.
