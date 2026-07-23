import { describe, expect, test } from "bun:test";

import { LatestRequest } from "./latest-request.ts";

describe("latest request gate", () => {
  test("rejects stale completion after a newer request starts", () => {
    const requests = new LatestRequest();
    const first = requests.begin();
    const second = requests.begin();

    expect(requests.accepts(first)).toBeFalse();
    expect(requests.accepts(second)).toBeTrue();
  });

  test("can invalidate in-flight work without starting a replacement", () => {
    const requests = new LatestRequest();
    const request = requests.begin();
    requests.invalidate();

    expect(requests.accepts(request)).toBeFalse();
  });
});
