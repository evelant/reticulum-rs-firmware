# Expo native Rust bridge proof

## Scope

On 2026-07-23, the universal Expo application compiled and loaded the
`reticulum-appliance-native` Rust crate through the pinned UniFFI
`0.31.0`/`uniffi-bindgen-react-native` `0.31.0-3` TurboModule. This proof
qualifies native binding generation, platform packaging, React Native
autoloading, and one synchronous Rust-to-TypeScript record round trip. It does
not claim a native USB, BLE, Wi-Fi, or authenticated device-session transport.

The Rust call returned bridge API `1.0`, device API `1.4`, and the exact
message/LXMF bounds compiled from `reticulum-device-api`. The app compared
every value with its Rust-generated TypeScript constants before rendering
`Rust 1.0 · API 1.4`. A missing module or mismatched field instead produces a
fault state; the web build keeps its existing HTTP adapter and does not load
the native module.

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
`Rust 1.0 · API 1.4`. As on iOS, the next visible state was the expected
missing interim HTTP endpoint rather than a bridge fault.

## iOS

Binding generation built nonempty arm64 archives for both
`aarch64-apple-ios` and `aarch64-apple-ios-sim`, then packaged them as the
`ios-arm64` device and `ios-arm64-simulator` XCFramework slices. CocoaPods
autolinking discovered `ApplianceNative` and generated the
`ReticulumApplianceNativeSpec` code.

An Xcode Debug build of the `ReticulumAppliance` scheme succeeded for the
arm64 iPhone 17 Pro simulator on iOS 26.2. The app installed and launched,
Metro loaded the local TurboModule, and the application rendered
`Rust 1.0 · API 1.4`. The app also failed clearly at the next expected
boundary—no native HTTP appliance endpoint had been configured—rather than
confusing bridge readiness with transport readiness.

## Remaining qualification

- move native generation and platform compilation into their platform CI
  jobs;
- add callable client/session operations from the portable Rust device client;
- qualify error and panic translation, cancellation, cleanup, Fast Refresh,
  background/foreground lifecycle, and reconnect;
- measure Android release size/startup/memory and decide its ABI distribution;
- qualify physical iOS/Android BLE hardware and authenticated resume behavior;
  and
- add an Intel simulator slice only if that development host remains a product
  requirement.
