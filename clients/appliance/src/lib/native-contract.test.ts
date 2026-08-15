import { describe, expect, test } from "bun:test";

import type {
  NativeBleOnboardingSnapshot,
  NativeBridgeContract,
} from "@reticulum/appliance-native";

import { assertNativeBridgeContract } from "./native-contract.ts";

const EXPECTED_CONTRACT = {
  bridgeApiMajor: 1,
  bridgeApiMinor: 23,
  deviceApiMajor: 1,
  deviceApiMinor: 18,
  maxMessageBytes: 512,
  maxLxmfReadChunkBytes: 416,
  maxLxmfBasicTitleBytes: 295,
  maxLxmfBasicContentBytes: 295,
  maxNomadPagePathBytes: 128,
  maxNomadPageBytes: 400,
  maxNomadRequestTimestampUnixMs: 9_007_199_254_740_991n,
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
    ).toThrow("deviceApiMinor: expected 18, observed 19");
  });

  test("keeps BLE onboarding progress coarse and free of secret or path fields", () => {
    type ExpectedOnboardingKeys =
      | "completedProfile"
      | "failure"
      | "linkGeneration"
      | "operation"
      | "phase"
      | "revision";
    type KeysMatch = keyof NativeBleOnboardingSnapshot extends ExpectedOnboardingKeys
      ? ExpectedOnboardingKeys extends keyof NativeBleOnboardingSnapshot
        ? true
        : false
      : false;
    const keysMatch: KeysMatch = true;

    expect(keysMatch).toBe(true);
  });
});
