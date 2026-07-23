import { runExpo } from "./expo-cli.ts";
import { prepareNativeBindings } from "./native-bindings.ts";
import { assertExpectedBun } from "./toolchain.ts";

type NativePlatform = "android" | "ios";

async function commandSucceeds(
  command: readonly string[],
  environment: NodeJS.ProcessEnv,
): Promise<boolean> {
  try {
    const child = Bun.spawn({
      cmd: [...command],
      env: environment,
      stderr: "ignore",
      stdout: "ignore",
    });
    return (await child.exited) === 0;
  } catch {
    return false;
  }
}

async function iosEnvironment(): Promise<NodeJS.ProcessEnv> {
  if (await commandSucceeds(["pod", "--version"], process.env)) return process.env;

  const environment = { ...process.env };
  delete environment.GEM_HOME;
  delete environment.GEM_PATH;
  environment.PATH = environment.PATH?.split(":")
    .filter((entry: string) => !entry.includes("/.rvm/") && !entry.includes("/.gem/"))
    .join(":");

  if (await commandSucceeds(["pod", "--version"], environment)) {
    console.warn(
      "The active Ruby environment cannot run CocoaPods; using the first working pod outside RVM/user-gem paths.",
    );
    return environment;
  }

  throw new Error(
    "CocoaPods is unavailable. Ensure `pod --version` succeeds before running the iOS development build.",
  );
}

assertExpectedBun();

const [requestedPlatform, ...arguments_] = process.argv.slice(2);
if (requestedPlatform !== "android" && requestedPlatform !== "ios") {
  throw new Error("usage: bun run scripts/run-native.ts <android|ios> [...Expo arguments]");
}
const platform: NativePlatform = requestedPlatform;
const environment = platform === "ios" ? await iosEnvironment() : process.env;

await prepareNativeBindings(platform);
await runExpo(["prebuild", "--platform", platform, "--clean", "--no-install"], environment);
await runExpo([`run:${platform}`, ...arguments_], environment);
