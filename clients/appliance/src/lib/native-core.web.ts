import type { NativeCoreStatus } from "./native-core-types.ts";

export function readNativeCoreStatus(): Promise<NativeCoreStatus | null> {
  return Promise.resolve(null);
}
