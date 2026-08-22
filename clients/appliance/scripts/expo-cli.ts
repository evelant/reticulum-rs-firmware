import { readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const clientDirectory = fileURLToPath(new URL("../", import.meta.url));
const expoExecutable = fileURLToPath(new URL("../node_modules/.bin/expo", import.meta.url));
const require = createRequire(import.meta.url);

export type ExpoPrebuildPlatform = "all" | "android" | "ios" | undefined;

export async function runExpo(
  arguments_: readonly string[],
  environment: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  const child = Bun.spawn({
    cmd: [expoExecutable, ...arguments_],
    cwd: clientDirectory,
    env: environment,
    stderr: "inherit",
    stdin: "inherit",
    stdout: "inherit",
  });
  const exitCode = await child.exited;
  if (exitCode !== 0) {
    throw new Error(`Expo ${arguments_.join(" ")} failed with status ${exitCode}`);
  }
}

async function restoreAndroidGradleWrapper(): Promise<void> {
  const reactNativeDirectory = dirname(require.resolve("react-native/package.json"));
  const source = resolve(
    reactNativeDirectory,
    "..",
    "@react-native/gradle-plugin/gradle/wrapper/gradle-wrapper.jar",
  );
  const target = join(clientDirectory, "android/gradle/wrapper/gradle-wrapper.jar");
  const expected = await readFile(source);

  // Expo prebuild under the pinned Bun runtime can leave trailing zero bytes
  // after copying this archive on macOS. Rewriting it from React Native's
  // pinned package keeps the generated wrapper executable by `java -jar`.
  await writeFile(target, expected);
  const actual = await readFile(target);
  if (!actual.equals(expected)) {
    throw new Error("generated Android Gradle wrapper differs from the pinned React Native copy");
  }
}

export async function runExpoPrebuild(
  arguments_: readonly string[],
  platform: ExpoPrebuildPlatform,
  environment: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  await runExpo(["prebuild", ...arguments_], environment);
  if (platform === undefined || platform === "all" || platform === "android") {
    await restoreAndroidGradleWrapper();
  }
}
