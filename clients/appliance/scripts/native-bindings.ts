import { access, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { assertExpectedBun } from "./toolchain.ts";

type NativePlatform = "android" | "ios";
type NativePreparation = NativePlatform | "all";

const clientDirectory = fileURLToPath(new URL("../", import.meta.url));
const repositoryDirectory = resolve(clientDirectory, "../..");
const moduleDirectory = join(clientDirectory, "modules/appliance-native");
const moduleRepositoryPath = "clients/appliance/modules/appliance-native";
const rustToolchainPath = join(repositoryDirectory, "rust-toolchain.toml");

const commonGeneratedPaths = [
  "cpp/generated",
  "cpp/reticulum-appliance-native.cpp",
  "cpp/reticulum-appliance-native.h",
  "src/generated",
  "src/NativeApplianceNative.ts",
  "src/index.tsx",
] as const;

const platformGeneratedPaths = {
  android: ["android"],
  ios: ["ApplianceNative.podspec", "ReticulumApplianceNativeFramework.xcframework", "ios"],
} as const satisfies Record<NativePlatform, readonly string[]>;

const requiredCommonPaths = [
  "cpp/generated/reticulum_appliance_native.cpp",
  "cpp/generated/reticulum_appliance_native.hpp",
  "cpp/reticulum-appliance-native.cpp",
  "cpp/reticulum-appliance-native.h",
  "src/NativeApplianceNative.ts",
  "src/generated/reticulum_appliance_native-ffi.ts",
  "src/generated/reticulum_appliance_native.ts",
  "src/index.tsx",
] as const;

const normalizedGeneratedTextPaths = [
  "src/NativeApplianceNative.ts",
  "src/generated/reticulum_appliance_native-ffi.ts",
  "src/generated/reticulum_appliance_native.ts",
  "src/index.tsx",
] as const;

const requiredPlatformPaths = {
  android: [
    "android/CMakeLists.txt",
    "android/build.gradle",
    "android/cpp-adapter.cpp",
    "android/proguard-rules.pro",
    "android/src/main/AndroidManifest.xml",
    "android/src/main/AndroidManifestNew.xml",
    "android/src/main/java/org/reticulum/appliance/nativebridge/ApplianceNativeModule.kt",
    "android/src/main/java/org/reticulum/appliance/nativebridge/ApplianceNativePackage.kt",
    "android/src/main/jniLibs/arm64-v8a/libreticulum_appliance_native.a",
    "android/src/main/jniLibs/armeabi-v7a/libreticulum_appliance_native.a",
    "android/src/main/jniLibs/x86/libreticulum_appliance_native.a",
    "android/src/main/jniLibs/x86_64/libreticulum_appliance_native.a",
  ],
  ios: [
    "ApplianceNative.podspec",
    "ReticulumApplianceNativeFramework.xcframework/Info.plist",
    "ReticulumApplianceNativeFramework.xcframework/ios-arm64/libreticulum_appliance_native.a",
    "ReticulumApplianceNativeFramework.xcframework/ios-arm64-simulator/libreticulum_appliance_native.a",
    "ios/ApplianceNative.h",
    "ios/ApplianceNative.mm",
  ],
} as const satisfies Record<NativePlatform, readonly string[]>;

const generatedTrackedPaths = [
  "ApplianceNative.podspec",
  "android",
  "cpp",
  "ios",
  "src/NativeApplianceNative.ts",
  "src/generated",
  "src/index.tsx",
] as const;

type GeneratedSnapshot = ReadonlyMap<string, Uint8Array | null>;

const requiredRustTargets = {
  android: [
    "aarch64-linux-android",
    "armv7-linux-androideabi",
    "i686-linux-android",
    "x86_64-linux-android",
  ],
  ios: ["aarch64-apple-ios", "aarch64-apple-ios-sim"],
} as const satisfies Record<NativePlatform, readonly string[]>;

async function removeGeneratedPaths(paths: readonly string[]): Promise<void> {
  await Promise.all(
    paths.map((path) => rm(join(moduleDirectory, path), { force: true, recursive: true })),
  );
}

async function run(
  command: readonly string[],
  cwd: string,
  environment: Readonly<Record<string, string>>,
): Promise<void> {
  const child = Bun.spawn({
    cmd: [...command],
    cwd,
    env: { ...process.env, ...environment },
    stderr: "inherit",
    stdin: "inherit",
    stdout: "inherit",
  });
  const exitCode = await child.exited;
  if (exitCode !== 0) {
    throw new Error(`${command.join(" ")} failed with status ${exitCode}`);
  }
}

async function rustToolchainChannel(): Promise<string> {
  const source = await readFile(rustToolchainPath, "utf8");
  const channel = source.match(/^\s*channel\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (channel === undefined) {
    throw new Error(`could not read the pinned Rust channel from ${rustToolchainPath}`);
  }
  return channel;
}

async function commandOutput(
  command: readonly string[],
  cwd: string,
  environment: Readonly<Record<string, string>>,
): Promise<string> {
  const child = Bun.spawn({
    cmd: [...command],
    cwd,
    env: { ...process.env, ...environment },
    stderr: "pipe",
    stdout: "pipe",
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  if (exitCode !== 0) {
    throw new Error(
      `${command.join(" ")} failed with status ${exitCode}${stderr === "" ? "" : `:\n${stderr.trimEnd()}`}`,
    );
  }
  return stdout;
}

async function assertPlatformToolchain(platform: NativePlatform, toolchain: string): Promise<void> {
  const environment = { RUSTUP_TOOLCHAIN: toolchain };
  const installed = new Set(
    (
      await commandOutput(
        ["rustup", "target", "list", "--installed"],
        repositoryDirectory,
        environment,
      )
    )
      .split("\n")
      .filter((target) => target !== ""),
  );
  const missing = requiredRustTargets[platform].filter((target) => !installed.has(target));
  if (missing.length > 0) {
    throw new Error(
      `missing Rust targets for ${platform}; run rustup target add --toolchain ${toolchain} ${missing.join(" ")}`,
    );
  }
  if (platform === "android") {
    await commandOutput(["cargo", "ndk", "--version"], repositoryDirectory, environment);
    await commandOutput(
      ["cargo", "ndk-env", "--target", "arm64-v8a", "--platform", "24"],
      repositoryDirectory,
      environment,
    );
  } else {
    await commandOutput(["xcrun", "--find", "xcodebuild"], repositoryDirectory, environment);
  }
}

async function buildEnvironment(
  platform: NativePlatform,
  toolchain: string,
): Promise<Record<string, string>> {
  const environment: Record<string, string> = { RUSTUP_TOOLCHAIN: toolchain };
  if (platform !== "ios") return environment;

  // A Nix-wrapped host compiler can inject a macOS deployment target even
  // while cc-rs is compiling bundled C dependencies for iPhoneOS. Select the
  // Xcode toolchain explicitly for both supported iOS Rust targets so native
  // dependencies never receive incompatible macOS and iOS target flags.
  const clang = (
    await commandOutput(
      ["xcrun", "--sdk", "iphoneos", "--find", "clang"],
      repositoryDirectory,
      environment,
    )
  ).trim();
  const clangPlusPlus = (
    await commandOutput(
      ["xcrun", "--sdk", "iphoneos", "--find", "clang++"],
      repositoryDirectory,
      environment,
    )
  ).trim();
  const ar = (
    await commandOutput(
      ["xcrun", "--sdk", "iphoneos", "--find", "ar"],
      repositoryDirectory,
      environment,
    )
  ).trim();

  return {
    ...environment,
    AR_aarch64_apple_ios: ar,
    AR_aarch64_apple_ios_sim: ar,
    CC_aarch64_apple_ios: clang,
    CC_aarch64_apple_ios_sim: clang,
    CXX_aarch64_apple_ios: clangPlusPlus,
    CXX_aarch64_apple_ios_sim: clangPlusPlus,
    CARGO_TARGET_AARCH64_APPLE_IOS_LINKER: clang,
    CARGO_TARGET_AARCH64_APPLE_IOS_SIM_LINKER: clang,
  };
}

async function writeAndroidCompatibilityFiles(): Promise<void> {
  const manifestPath = join(moduleDirectory, "android/src/main/AndroidManifestNew.xml");
  await mkdir(dirname(manifestPath), { recursive: true });
  await writeFile(
    manifestPath,
    `<!-- Generated compatibility manifest for Android Gradle Plugin 7.3+. -->
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
</manifest>
`,
  );
  await writeFile(
    join(moduleDirectory, "android/proguard-rules.pro"),
    "# The native bridge does not currently require consumer ProGuard rules.\n",
  );
}

async function normalizeGeneratedText(): Promise<void> {
  await Promise.all(
    normalizedGeneratedTextPaths.map(async (path) => {
      const outputPath = join(moduleDirectory, path);
      const source = await readFile(outputPath, "utf8");
      const normalized = `${source.replace(/[ \t]+$/gm, "").replace(/\n+$/, "")}\n`;
      await writeFile(outputPath, normalized);
    }),
  );
}

async function normalizeAndroidCmake(): Promise<void> {
  const cmakePath = join(moduleDirectory, "android/CMakeLists.txt");
  const source = await readFile(cmakePath, "utf8");
  const generatedResolver = `execute_process(
    COMMAND node -p "require.resolve('uniffi-bindgen-react-native/package.json')"
    OUTPUT_VARIABLE UNIFFI_BINDGEN_PATH
    OUTPUT_STRIP_TRAILING_WHITESPACE
)
# Get the directory; get_filename_component and cmake_path will normalize
# paths with Windows path separators.
get_filename_component(UNIFFI_BINDGEN_PATH "\${UNIFFI_BINDGEN_PATH}" DIRECTORY)`;
  const exportedPackageResolver = `execute_process(
    COMMAND node -p "require('path').resolve(require('path').dirname(require.resolve('uniffi-bindgen-react-native')), '../../..')"
    OUTPUT_VARIABLE UNIFFI_BINDGEN_PATH
    OUTPUT_STRIP_TRAILING_WHITESPACE
)`;
  if (!source.includes(generatedResolver)) {
    throw new Error("generated Android CMake package resolver changed");
  }
  await writeFile(cmakePath, source.replace(generatedResolver, exportedPackageResolver));
}

async function requireGeneratedPaths(paths: readonly string[]): Promise<void> {
  for (const path of paths) {
    try {
      await access(join(moduleDirectory, path));
      const metadata = await stat(join(moduleDirectory, path));
      if (!metadata.isFile() || metadata.size === 0) {
        throw new Error(`${path} is not a non-empty file`);
      }
    } catch {
      throw new Error(`native binding generation did not produce ${path}`);
    }
  }
}

async function buildPlatform(platform: NativePlatform, toolchain: string): Promise<void> {
  const environment = await buildEnvironment(platform, toolchain);
  await run(
    [
      process.execPath,
      "x",
      "--bun",
      "ubrn",
      "build",
      platform,
      "--config",
      "ubrn.config.yaml",
      "--and-generate",
    ],
    moduleDirectory,
    environment,
  );
  await normalizeGeneratedText();
  if (platform === "android") {
    await writeAndroidCompatibilityFiles();
    await normalizeAndroidCmake();
  }
  await requireGeneratedPaths([...requiredCommonPaths, ...requiredPlatformPaths[platform]]);
}

export async function prepareNativeBindings(preparation: NativePreparation): Promise<void> {
  assertExpectedBun();
  const toolchain = await rustToolchainChannel();
  const platforms: readonly NativePlatform[] =
    preparation === "all" ? ["ios", "android"] : [preparation];
  await commandOutput([process.execPath, "x", "--bun", "ubrn", "--help"], moduleDirectory, {
    RUSTUP_TOOLCHAIN: toolchain,
  });
  for (const platform of platforms) await assertPlatformToolchain(platform, toolchain);
  await removeGeneratedPaths([
    ...commonGeneratedPaths,
    ...platforms.flatMap((platform) => platformGeneratedPaths[platform]),
  ]);
  for (const platform of platforms) await buildPlatform(platform, toolchain);
}

async function generatedSnapshot(): Promise<GeneratedSnapshot> {
  const output = await commandOutput(
    [
      "git",
      "ls-files",
      "-z",
      "--cached",
      "--others",
      "--exclude-standard",
      "--",
      ...generatedTrackedPaths.map((path) => join(moduleRepositoryPath, path)),
    ],
    repositoryDirectory,
    {},
  );
  const snapshot = new Map<string, Uint8Array | null>();
  for (const path of output.split("\0").filter((path) => path !== "")) {
    try {
      snapshot.set(path, await readFile(join(repositoryDirectory, path)));
    } catch {
      snapshot.set(path, null);
    }
  }
  return snapshot;
}

function bytesEqual(left: Uint8Array | null, right: Uint8Array | null): boolean {
  if (left === null || right === null) return left === right;
  return left.length === right.length && left.every((byte, index) => byte === right[index]);
}

async function assertBindingsHaveNoDrift(before: GeneratedSnapshot): Promise<void> {
  const after = await generatedSnapshot();
  const changed = [...new Set([...before.keys(), ...after.keys()])]
    .filter((path) => !bytesEqual(before.get(path) ?? null, after.get(path) ?? null))
    .sort();
  if (changed.length > 0) {
    throw new Error(`native bindings are stale:\n${changed.join("\n")}`);
  }
}

if (import.meta.main) {
  const mode = process.argv[2];
  if (mode !== "android" && mode !== "ios" && mode !== "all" && mode !== "check") {
    throw new Error("usage: bun run scripts/native-bindings.ts <android|ios|all|check>");
  }
  const before = mode === "check" ? await generatedSnapshot() : undefined;
  await prepareNativeBindings(mode === "check" ? "all" : mode);
  if (before !== undefined) await assertBindingsHaveNoDrift(before);
}
