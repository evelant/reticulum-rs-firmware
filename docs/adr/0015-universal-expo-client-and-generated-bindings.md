# ADR 0015: Universal Expo client and generated TypeScript boundaries

- **Status:** Accepted
- **Date:** 2026-07-22
- **Supersedes:** the separate hand-built SPA and later React Native shell
  ordering in the initial architecture

## Context

The appliance needs one client experience across a host-served web page,
device-served Wi-Fi, Android, and iOS. The initial loopback proof used a small
hand-built TypeScript SPA, while the architecture deferred a separate React
Native shell until BLE. Continuing those as independent applications would
duplicate UI, state, validation, recovery behavior, and API types precisely as
the public device API begins to evolve.

The application will also need platform-specific transports. Web can use the
current loopback HTTP service and later a device Wi-Fi endpoint. Native mobile
builds can own BLE, local-network discovery, background lifecycle, and a Rust
client library. Expo supports one React Native component tree on Android, iOS,
and web, static web export, custom native modules in development builds, and
Bun for dependency installation and script execution.

Rust is already the source of truth for the host JSON responses and the
embedded CBOR protocol. Maintaining a second handwritten TypeScript model for
those records creates silent drift. `ts-rs` supports the serde tagging and
renaming used by the current JSON adapter and can export declarations from a
host build without entering a firmware dependency graph.

## Decision

### One universal application

Maintain one Expo React Native application under `clients/appliance/`, authored
only in TypeScript/TSX. It is the product client for all three presentation
targets:

- a static web export embedded in the current host companion and later in a
  device HTTP server;
- an Android application; and
- an iOS application.

Use Expo Continuous Native Generation. Generated `android/` and `ios/`
projects are build products and are not committed. Because the future Rust
bridge and BLE implementation are custom native code, mobile development uses
an Expo development build rather than Expo Go.

Bun 1.3.13 is the pinned application package manager, runtime, test runner, and
script orchestrator. All project-owned application scripts are TypeScript.
Expo's Metro pipeline remains the framework-required React Native and web
bundler invoked by those Bun scripts; it is an explicit toolchain exception,
not a second project-owned scripting surface. Expo's template/prebuild tooling
currently retains a Node.js LTS prerequisite internally. With the pinned Expo
57/Bun 1.3.13 pair, forcing `expo prebuild` itself through Bun corrupts the
generated Xcode project with NUL padding before Expo parses it. Therefore
`bun run prebuild` remains the project entrypoint and verifies Bun first, but
allows the upstream Expo CLI to retain its Node shebang. Project commands must
not introduce npm-, yarn-, pnpm-, or handwritten-JavaScript workflows.

The web export is normalized into a deterministic, bounded asset set before
Rust embeds it. No application runtime asset may depend on a CDN or network
build service.

### Rust-generated application contracts

The Rust type that is actually serialized is authoritative for every JSON API
DTO. The host service uses exactly pinned `ts-rs` generation with serde
compatibility to emit the corresponding TypeScript declarations. CI fails when
the checked binding differs from a fresh export. Application code imports that
generated file and must not restate wire DTOs manually.

`ts-rs` is a host tooling dependency only. It must not appear in any firmware,
HIL, transport, or `no_std` dependency closure. When a native or web app begins
speaking the public CBOR device API directly, its serializable DTOs should live
in a small `no_std` source-of-truth crate. TypeScript generation may then be
enabled only in a host generator feature or companion crate; the embedded
build keeps that feature disabled.

JSON integer fields exposed as TypeScript `number` must be proven to remain in
the JavaScript safe-integer range at serialization. A field without that bound
must use a string or another exact wire representation instead of merely
changing its generated declaration.

### Transport and native-Rust boundary

UI and domain hooks depend on a narrow application transport interface, not on
HTTP, USB, BLE, or Expo globals directly. Initial implementations are:

1. loopback HTTP/SSE for the USB host companion;
2. device HTTP/WebSocket for Wi-Fi when that bearer exists; and
3. a native Rust-backed transport for BLE and background operation.

The first native-Rust spike selected UniFFI `0.31.0` through exactly pinned
`uniffi-bindgen-react-native` `0.31.0-3`. A private Expo local TurboModule owns
platform packaging and generated TypeScript, C++, Kotlin, Objective-C++, CMake,
Gradle, podspec, and framework boundaries. Its first callable Rust operation
returns immutable bridge/device-API versions and message bounds. The
TypeScript caller compares every field with Rust-generated device-API
constants and fails closed before exposing the bridge as ready.

Android development compilation now passes for all four Expo ABIs. An arm64
iOS simulator development build also passes and renders the contract returned
by the compiled Rust library. The later bridge revisions add a host-tested
raw-TCP Wi-Fi connector and a foreground BLE central/Rust command pump.
Direct macOS CoreBluetooth sessions qualify the BLE firmware carrier on both
E290s; generated iOS/Android builds and Expo source tests qualify the native
composition. A create-only native credential import now gates BLE and derives
the exact E290 advertising name without exposing PSK bytes to TypeScript.
Physical-phone file picking, permissions, TurboModule transport calls,
foreground/background behavior, reconnect, and authenticated LXMF remain
unqualified. Direct handwritten Turbo Modules remain a fallback. Nitro
Modules remain deferred until profiling shows that the control/message
interface needs a lower-overhead JSI boundary; selecting them pre-emptively
would add a second code generator and C++ ownership surface without a
demonstrated throughput requirement.

The app is an optional client, not the Reticulum router. Routing, identity,
durable inbox/outbox state, LXMF propagation, and transport forwarding continue
on the board while it is powered and do not depend on the app remaining alive.

## Consequences

- Web, Android, and iOS share components, onboarding semantics, validation,
  Micron rendering, and generated API types from the first product client.
- The host companion remains useful for deterministic USB development and for
  browsers that cannot directly own the board transport.
- Expo/React Native Web has a larger flash and download footprint than the
  proof SPA. Embedded UI remains feature-gated so constrained boards can omit
  it without reducing the full product design.
- The current React Native Web export creates StyleSheet rules dynamically and
  embeds Metro image modules as data URLs. The host CSP permits inline styles
  and `data:` images for those exact requirements, but retains external-only
  scripts and otherwise same-origin resources.
- Native Rust or BLE changes require rebuilding the Expo development client;
  ordinary TypeScript UI changes retain the Metro fast-refresh loop.
- Mobile platform bindings and WASM remain separate from `ts-rs`: `ts-rs`
  describes serialized DTOs, while UniFFI/WASM tooling describes callable Rust
  APIs and ownership.

## Rejected alternatives

- **Maintain a bespoke SPA and add React Native later:** rejected because it
  duplicates the fastest-changing product logic and wire types.
- **Use a PWA as the universal client:** rejected because Web Bluetooth and
  background execution do not provide a credible universal iOS path.
- **Handwrite TypeScript DTOs:** rejected because serde changes would not force
  the application contract to change.
- **Put `ts-rs` in the embedded graph:** rejected because declaration export is
  host tooling and must not affect firmware size, portability, or `no_std`
  qualification.
- **Select Nitro or a handwritten Turbo Module immediately:** deferred until a
  small UniFFI integration spike and measurements identify a concrete gap.

## Verification

The application gate must, from a frozen Bun lockfile:

1. check the exact Bun revision and dependency pins;
2. fail on authored `.js` files;
3. check formatting, lint, and strict TypeScript types;
4. run component/domain tests on web-compatible code;
5. verify freshly generated Rust API bindings byte-for-byte;
6. export the Expo web target twice and prove normalized assets identical; and
7. compile the Rust host service from those checked assets without invoking
   Bun implicitly.

The first native gate regenerates both platform bindings, checks only their
tracked generated surfaces for drift, and tests and lints the source Rust
crate. Android and iOS development builds and the immutable binding round trip
have passed manually and are recorded in the
[native bridge proof](../expo-native-rust-bridge-proof.md). Platform CI,
cancellation/panic behavior, cleanup and Fast Refresh idempotence, background
lifecycle, and physical BLE hardware remain gates for their owning phases.

## References

- [Expo: using Bun](https://docs.expo.dev/guides/using-bun/)
- [Expo Router: static rendering](https://docs.expo.dev/router/web/static-rendering/)
- [Expo: add custom native code](https://docs.expo.dev/workflow/customizing/)
- [React Native: Turbo Native Modules](https://reactnative.dev/docs/turbo-native-modules-introduction)
- [`ts-rs`](https://github.com/Aleph-Alpha/ts-rs)
- [`uniffi-bindgen-react-native`](https://github.com/jhugman/uniffi-bindgen-react-native)
- [Nitro Modules](https://nitro.margelo.com/)
