import { runExpo } from "./expo-cli.ts";
import { prepareNativeBindings } from "./native-bindings.ts";
import { assertExpectedBun } from "./toolchain.ts";

assertExpectedBun();

const arguments_ = process.argv.slice(2);
const platformIndex = arguments_.indexOf("--platform");
const requestedPlatform = platformIndex === -1 ? undefined : arguments_[platformIndex + 1];
if (
  requestedPlatform !== undefined &&
  requestedPlatform !== "android" &&
  requestedPlatform !== "ios" &&
  requestedPlatform !== "all"
) {
  throw new Error(`unsupported Expo prebuild platform ${requestedPlatform}`);
}

await prepareNativeBindings(requestedPlatform === undefined ? "all" : requestedPlatform);
await runExpo(["prebuild", ...arguments_]);
