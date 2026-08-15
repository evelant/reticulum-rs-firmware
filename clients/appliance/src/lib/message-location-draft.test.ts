import { describe, expect, test } from "bun:test";

import { prepareDraftSubmission } from "./message-location-draft.ts";

const identity = { idempotencyKey: "11".repeat(16), timestampMs: 1_785_084_000_000 };
const location = {
  latitude_e6: 42_357_111,
  longitude_e6: -71_061_924,
  altitude_cm: 0,
  speed_cm_per_second: 0,
  bearing_centidegrees: 0,
  accuracy_cm: 825,
  updated_at_unix_seconds: 1_785_084_000,
};

describe("message location draft submission", () => {
  test("does not ask for location when this draft has sharing disabled", async () => {
    let captures = 0;
    const submission = await prepareDraftSubmission(
      null,
      false,
      () => identity,
      async () => {
        captures += 1;
        return location;
      },
    );
    expect(captures).toBe(0);
    expect(submission).toEqual({ attachLocation: false, identity, location: null });
  });

  test("fails instead of silently omitting an explicitly requested fix", async () => {
    let identities = 0;
    expect(
      prepareDraftSubmission(
        null,
        true,
        () => {
          identities += 1;
          return identity;
        },
        async () => {
          throw new Error("foreground fix unavailable");
        },
      ),
    ).rejects.toThrow("foreground fix unavailable");
    expect(identities).toBe(0);
  });

  test("reuses the exact identity and fix after an ambiguous send failure", async () => {
    const retained = { attachLocation: true, identity, location };
    let captures = 0;
    const submission = await prepareDraftSubmission(
      retained,
      true,
      () => {
        throw new Error("identity must be retained");
      },
      async () => {
        captures += 1;
        return { ...location, latitude_e6: 1 };
      },
    );
    expect(submission).toBe(retained);
    expect(captures).toBe(0);
  });
});
