import { describe, expect, test } from "bun:test";

import type { NativeApplianceError } from "@reticulum/appliance-native";

import { nativeApplianceErrorMessage, normalizeNativeError } from "./native-error.ts";

describe("native appliance errors", () => {
  test("preserves the structured transport reason", () => {
    const bridgeError = {
      tag: "TransportUnavailable",
      inner: {
        transport: 2,
        reason: "Bluetooth Low Energy requires NativeAppliance.open_ble and a platform GATT link",
      },
    } as unknown as NativeApplianceError;
    const isBridgeError = (value: unknown): value is NativeApplianceError => value === bridgeError;

    expect(nativeApplianceErrorMessage(bridgeError)).toBe(
      "Native appliance transport unavailable: Bluetooth Low Energy requires NativeAppliance.open_ble and a platform GATT link",
    );
    const normalized = normalizeNativeError(bridgeError, isBridgeError);
    expect(normalized.message).toContain("platform GATT link");
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
