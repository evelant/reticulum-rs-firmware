// Bun host tests and non-native bundles must not load React Native's Flow
// entrypoint. Metro selects native-platform.native.ts for iOS and Android.
export const nativePlatformOs = "unsupported";
