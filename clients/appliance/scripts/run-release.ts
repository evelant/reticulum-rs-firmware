import { rm, stat } from "node:fs/promises";
import { join } from "node:path";

import { clientDirectory } from "./expo-cli.ts";
import { type NativePlatform, runNative } from "./run-native.ts";
import { assertExpectedBun } from "./toolchain.ts";

export interface ReleasePlan {
  readonly artifact: string;
  readonly expoArguments: readonly string[];
  readonly outputDirectory?: string;
}

const forbiddenOptions = new Set([
  "--binary",
  "--configuration",
  "--no-install",
  "--output",
  "-o",
  "--variant",
]);

function optionName(argument: string): string {
  return argument.split("=", 1)[0] ?? argument;
}

function hasDeviceSelection(arguments_: readonly string[]): boolean {
  return arguments_.some(
    (argument) =>
      argument === "--device" ||
      argument === "-d" ||
      argument.startsWith("--device=") ||
      argument.startsWith("-d="),
  );
}

function rejectsGenericIosTarget(arguments_: readonly string[]): boolean {
  return arguments_.some((argument, index) => {
    if (argument === "--device" || argument === "-d") {
      return arguments_[index + 1]?.toLowerCase() === "generic";
    }
    const [name, value] = argument.split("=", 2);
    return (name === "--device" || name === "-d") && value?.toLowerCase() === "generic";
  });
}

export function releasePlan(
  platform: NativePlatform,
  forwardedArguments: readonly string[],
  root: string = clientDirectory,
): ReleasePlan {
  for (const argument of forwardedArguments) {
    const name = optionName(argument);
    if (forbiddenOptions.has(name)) {
      throw new Error(
        `${name} is owned by the ${platform} Release wrapper and cannot be overridden`,
      );
    }
  }
  if (platform === "ios" && rejectsGenericIosTarget(forwardedArguments)) {
    throw new Error("the Release install wrapper requires a physical iOS device");
  }

  const deviceArguments = hasDeviceSelection(forwardedArguments)
    ? forwardedArguments
    : [...forwardedArguments, "--device"];

  if (platform === "ios") {
    const outputDirectory = join(root, ".tmp", "ios-release");
    return {
      artifact: join(outputDirectory, "ReticulumAppliance.app"),
      expoArguments: [
        "--configuration",
        "Release",
        "--no-bundler",
        "--output",
        outputDirectory,
        ...deviceArguments,
      ],
      outputDirectory,
    };
  }

  return {
    artifact: join(root, "android", "app", "build", "outputs", "apk", "release", "app-release.apk"),
    expoArguments: ["--variant", "release", "--no-bundler", ...deviceArguments],
  };
}

async function requireNonemptyFile(path: string, label: string): Promise<number> {
  const metadata = await stat(path);
  if (!metadata.isFile() || metadata.size === 0) {
    throw new Error(`${label} is not a nonempty file: ${path}`);
  }
  return metadata.size;
}

async function verifyReleaseArtifact(platform: NativePlatform, artifact: string): Promise<void> {
  if (platform === "ios") {
    const bundle = join(artifact, "main.jsbundle");
    const bytes = await requireNonemptyFile(bundle, "embedded iOS script bundle");
    console.log(`Verified self-contained iOS Release: ${artifact}`);
    console.log(`Embedded main.jsbundle: ${bytes} bytes`);
    return;
  }

  const bytes = await requireNonemptyFile(artifact, "Android Release APK");
  console.log(`Verified Android Release APK: ${artifact} (${bytes} bytes)`);
}

function printHelp(platform?: NativePlatform): void {
  const command = platform === undefined ? "<ios|android>" : platform;
  const target =
    platform === "ios"
      ? "selected physical device"
      : platform === "android"
        ? "selected device or emulator"
        : "selected target";
  console.log(`usage: bun run scripts/run-release.ts ${command} [Expo device arguments]

Builds a clean self-contained Release, installs it on the ${target}, launches it,
and verifies the generated app artifact. If --device is omitted, Expo opens its device picker.

Examples:
  bun run release:ios
  bun run release:ios -- --device <iOS-name-or-UDID>
  bun run release:android
  bun run release:android -- --device <Android-device-name>`);
}

if (import.meta.main) {
  assertExpectedBun();
  const [requestedPlatform, ...forwardedArguments] = process.argv.slice(2);
  if (requestedPlatform !== "android" && requestedPlatform !== "ios") {
    printHelp();
    throw new Error("release platform must be ios or android");
  }
  if (forwardedArguments.includes("--help") || forwardedArguments.includes("-h")) {
    printHelp(requestedPlatform);
    process.exit(0);
  }

  const plan = releasePlan(requestedPlatform, forwardedArguments);
  if (plan.outputDirectory !== undefined) {
    await rm(plan.outputDirectory, { force: true, recursive: true });
  }
  await runNative(requestedPlatform, plan.expoArguments, "release");
  await verifyReleaseArtifact(requestedPlatform, plan.artifact);
}
