# Expo native Rust bridge proof

## Scope

On 2026-07-23, the universal Expo application compiled and loaded the
`reticulum-appliance-native` Rust crate through the pinned UniFFI
`0.31.0`/`uniffi-bindgen-react-native` `0.31.0-3` TurboModule. This proof
qualifies native binding generation, platform packaging, React Native
autoloading, synchronous contract calls, asynchronous facade calls, Rust-owned
SQLite mutation, and process-restart persistence. It does not claim a native
USB, BLE, Wi-Fi, or authenticated device-session transport.

The completed mutable proof used bridge API `1.1`, device API `1.4`, and the
exact message/LXMF bounds compiled from `reticulum-device-api`. The app
compared every value with its Rust-generated TypeScript constants before
rendering `Rust 1.1 · API 1.4`. A missing module or mismatched field instead
produces a fault state; the web build keeps its existing HTTP adapter and does
not load the native module.

Bridge API `1.2` was generated after this runtime proof. It adds the named
raw-TCP Wi-Fi constructor, fixed sandbox credential path integration, and
local-network declaration. Its Rust connector passes a real localhost
suite-2 handshake with partial stream writes, but bridge `1.2` has not yet run
against an E290 or through a rebuilt mobile application. Do not fold that
host-side result into the completed bridge `1.1` runtime claim.

## Android

Expo Continuous Native Generation discovered the local package and React
Native Codegen generated the matching TurboModule specification. A complete
debug `assembleDebug` build succeeded for:

- `arm64-v8a`;
- `armeabi-v7a`;
- `x86`; and
- `x86_64`.

The resulting package was `org.reticulum.appliance`, version `0.0.1`, with
minimum Android API 24. The 220 MiB multi-ABI debug APK had SHA-256
`3db2915d43f322bea88e26b10e188b7a54cf07a68fef9bc26c4f86ec44dc2afc`.
Archive inspection found `libreticulum-appliance-native.so` in all four ABI
directories. This debug artifact is build evidence, not a product-size
measurement; release stripping and ABI splits remain unmeasured.

The APK was then installed on an arm64 Android 17/API 37 Pixel emulator. Metro
loaded the application and local TurboModule, and the running app rendered
`Rust 1.0 · API 1.4`. This was the earlier immutable-contract proof; the
mutable bridge API `1.1` still needs the same Android runtime exercise.

## iOS

Binding generation built nonempty arm64 archives for both
`aarch64-apple-ios` and `aarch64-apple-ios-sim`, then packaged them as the
`ios-arm64` device and `ios-arm64-simulator` XCFramework slices. CocoaPods
autolinking discovered `ApplianceNative` and generated the
`ReticulumApplianceNativeSpec` code.

An Xcode Debug build of the `ReticulumAppliance` scheme succeeded for the
arm64 iPhone 17 Pro simulator on iOS 26.5. The app installed and launched,
Metro loaded the local TurboModule, and the application rendered
`Rust 1.1 · API 1.4`.

The running Expo screen then called the generated asynchronous facade to open
an app-private SQLite database, create contact `Native smoke` at destination
`abababababababababababababababab`, and durably queue an LXMF outbox message
with title `Hermes smoke` and content `Mutable Rust facade`. The screen
immediately showed one pending message and a committed timeline entry. A
read-only inspection of the simulator application container independently
confirmed SQLite schema version 2, the exact contact, and outbox row 1 in
status 0. After force-terminating and relaunching the process, the contact,
pending count, and complete timeline entry were restored. The selected BLE
connector remained `unavailable`, as intended by the explicit future-work
stub; offline success did not masquerade as device connectivity.

The successful link emitted one warning that an object built for the iOS 26.5
simulator was linked into the application's iOS 16.4 deployment target. It did
not prevent build, launch, mutation, or persistence, but the generated native
dependency target policy remains a cleanup item.

## Remaining qualification

- move native generation and platform compilation into their platform CI
  jobs;
- qualify the mutable facade in an Android runtime;
- complete the E290 endpoint and physically qualify the first authenticated
  native device bearer;
- qualify error and panic translation, cancellation, cleanup, Fast Refresh,
  background/foreground lifecycle, and reconnect;
- measure Android release size/startup/memory and decide its ABI distribution;
- qualify physical iOS/Android BLE hardware and authenticated resume behavior;
  and
- add an Intel simulator slice only if that development host remains a product
  requirement.
