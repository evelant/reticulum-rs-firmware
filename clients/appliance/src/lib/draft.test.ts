import { describe, expect, test } from "bun:test";

import { ensureDraftIdentity } from "./draft.ts";

describe("outbound draft identity", () => {
  test("an ambiguous retry retains both key and timestamp", () => {
    const first = ensureDraftIdentity(
      null,
      () => "11".repeat(16),
      () => 1_234,
    );
    const retried = ensureDraftIdentity(
      first,
      () => "22".repeat(16),
      () => 9_999,
    );

    expect(retried).toBe(first);
    expect(retried).toEqual({ idempotencyKey: "11".repeat(16), timestampMs: 1_234 });
  });
});
