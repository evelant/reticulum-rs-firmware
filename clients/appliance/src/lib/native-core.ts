import { assertNativeBridgeContract } from "./native-contract.ts";
import type { NativeCoreStatus } from "./native-core-types.ts";

export async function readNativeCoreStatus(): Promise<NativeCoreStatus> {
  try {
    const { nativeBridgeContract } = await import("@reticulum/appliance-native");
    const contract = assertNativeBridgeContract(nativeBridgeContract());
    return {
      label: `Rust ${contract.bridgeApiMajor}.${contract.bridgeApiMinor} · API ${contract.deviceApiMajor}.${contract.deviceApiMinor}`,
      state: "ready",
    };
  } catch (error) {
    return {
      label: error instanceof Error ? error.message : "Rust native bridge failed",
      state: "faulted",
    };
  }
}
