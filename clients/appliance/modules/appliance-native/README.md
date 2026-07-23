# Expo native Rust module

This private local React Native TurboModule packages the Rust crate
`reticulum-appliance-native` for the universal Expo application. It uses the
exactly pinned UniFFI `0.31.0` and `uniffi-bindgen-react-native` `0.31.0-3`
toolchain. Expo Continuous Native Generation discovers the package directly
beneath `modules/`; the generated application-level `ios/` and `android/`
projects remain disposable.

The package metadata, `.gitignore`, web fallback, React Native config, UBRN
config, and this README are authored. UniFFI/UBRN owns the tracked TypeScript,
C++, Kotlin, Objective-C++, podspec, Gradle, manifest, and CMake outputs.
Regenerate them from the client directory with:

```sh
bun run native:bindings
```

Do not hand-edit generated files. The XCFramework, Android JNI archives,
React Native Codegen output, and application projects are ignored build
products. Generation requires two nonempty arm64 iOS archives (device and
Apple-Silicon simulator) and all four Expo Android ABIs. The Android generator
currently needs fail-closed compatibility normalization for the current
Android Gradle manifest, ProGuard file, and package-exports-safe CMake
resolver; the script stops if the upstream CMake text changes.
