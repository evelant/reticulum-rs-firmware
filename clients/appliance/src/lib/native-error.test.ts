import { describe, expect, test } from "bun:test";

import type { NativeApplianceError } from "@reticulum/appliance-native";

import { nativeApplianceErrorMessage, normalizeNativeError } from "./native-error.ts";

describe("native appliance errors", () => {
  test("preserves a structured error reason", () => {
    const bridgeError = {
      tag: "InvalidArgument",
      inner: { reason: "profile key is invalid" },
    } as unknown as NativeApplianceError;
    const isBridgeError = (value: unknown): value is NativeApplianceError => value === bridgeError;

    expect(nativeApplianceErrorMessage(bridgeError)).toBe(
      "Native appliance invalid argument: profile key is invalid",
    );
    const normalized = normalizeNativeError(bridgeError, isBridgeError);
    expect(normalized.message).toContain("profile key is invalid");
    expect(normalized.cause).toBe(bridgeError);
  });

  test("retains ordinary errors and gives reasonless variants a useful label", () => {
    const busy = { tag: "Busy" } as unknown as NativeApplianceError;
    expect(nativeApplianceErrorMessage(busy)).toBe("Native appliance busy");

    const ordinary = new Error("ordinary");
    const isBridgeError = (_value: unknown): _value is NativeApplianceError => false;
    expect(normalizeNativeError(ordinary, isBridgeError)).toBe(ordinary);
  });
});
