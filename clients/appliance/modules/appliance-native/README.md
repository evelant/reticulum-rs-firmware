# Expo native Rust module

This private Expo TurboModule packages `reticulum-appliance-native` for the
universal client. UniFFI generates the language bindings and the Expo package
exposes them to React Native.

From `clients/appliance`, regenerate all native outputs with:

```sh
bun run native:bindings
```

The module metadata, web fallback, React Native configuration, and build
scripts are authored. Generated TypeScript, C++, Kotlin, Objective-C++, podspec,
Gradle, manifest, CMake, XCFramework, and JNI outputs must not be edited by
hand.

iOS generation builds arm64 device and Apple-Silicon simulator archives.
Android generation builds the four ABIs selected by the Expo project. The
application-level `ios/` and `android/` directories are disposable and are
created by the project wrappers.
