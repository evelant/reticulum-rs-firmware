import { describe, expect, test } from "bun:test";
import { join } from "node:path";

import { nativeBindingBuildArguments } from "./native-bindings.ts";
import { releasePlan } from "./run-release.ts";

describe("native Release plan", () => {
  const root = "/project/clients/appliance";

  test("iOS owns a clean Release output and prompts for a device by default", () => {
    expect(releasePlan("ios", [], root)).toEqual({
      artifact: join(root, ".tmp", "ios-release", "ReticulumAppliance.app"),
      expoArguments: [
        "--configuration",
        "Release",
        "--no-bundler",
        "--output",
        join(root, ".tmp", "ios-release"),
        "--device",
      ],
      outputDirectory: join(root, ".tmp", "ios-release"),
    });
  });

  test("Android owns the release variant and preserves an explicit device", () => {
    expect(releasePlan("android", ["--device", "phone-1"], root)).toEqual({
      artifact: join(
        root,
        "android",
        "app",
        "build",
        "outputs",
        "apk",
        "release",
        "app-release.apk",
      ),
      expoArguments: ["--variant", "release", "--no-bundler", "--device", "phone-1"],
    });
  });

  test("rejects caller overrides that can skip or weaken the Release install", () => {
    for (const argument of [
      "--binary=old.app",
      "--configuration",
      "--no-install",
      "--output=elsewhere",
      "-o",
      "--variant=debug",
    ]) {
      expect(() => releasePlan("ios", [argument], root)).toThrow();
    }
    expect(() => releasePlan("ios", ["--device", "generic"], root)).toThrow("physical iOS device");
  });

  test("builds the Rust bridge with Cargo's release profile", () => {
    expect(nativeBindingBuildArguments("ios", "release")).toContain("--release");
    expect(nativeBindingBuildArguments("android", "release")).toContain("--release");
    expect(nativeBindingBuildArguments("ios", "debug")).not.toContain("--release");
  });
});
