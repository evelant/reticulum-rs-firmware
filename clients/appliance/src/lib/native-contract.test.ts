import { describe, expect, test } from "bun:test";

import type { NativeBridgeContract } from "@reticulum/appliance-native";

import { assertNativeBridgeContract } from "./native-contract.ts";

const EXPECTED_CONTRACT = {
  bridgeApiMajor: 1,
  bridgeApiMinor: 1,
  deviceApiMajor: 1,
  deviceApiMinor: 4,
  maxMessageBytes: 512,
  maxLxmfReadChunkBytes: 416,
  maxLxmfBasicTitleBytes: 295,
  maxLxmfBasicContentBytes: 295,
} satisfies NativeBridgeContract;

describe("native Rust bridge contract", () => {
  test("accepts the Rust and generated-TypeScript contract", () => {
    expect(assertNativeBridgeContract(EXPECTED_CONTRACT)).toBe(EXPECTED_CONTRACT);
  });

  test("fails closed on a native binary compiled for another device API", () => {
    expect(() =>
      assertNativeBridgeContract({
        ...EXPECTED_CONTRACT,
        deviceApiMinor: EXPECTED_CONTRACT.deviceApiMinor + 1,
      }),
    ).toThrow("deviceApiMinor: expected 4, observed 5");
  });
});
