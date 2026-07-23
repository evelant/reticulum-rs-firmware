import { describe, expect, test } from "bun:test";

import { capabilityFromUrl, decodeSuccessResponse } from "./api-core.ts";

describe("session capability URL", () => {
  test("reads the browser fragment without accepting lookalike fields", () => {
    expect(capabilityFromUrl("http://127.0.0.1/#cap=abcd")).toBe("abcd");
    expect(capabilityFromUrl("http://127.0.0.1/#cap=abcd&mode=test")).toBe("abcd");
    expect(capabilityFromUrl("http://127.0.0.1/#capability=abcd")).toBeNull();
    expect(capabilityFromUrl("http://127.0.0.1/#cap=")).toBeNull();
    expect(capabilityFromUrl("http://127.0.0.1/?cap=abcd")).toBeNull();
  });

  test("also accepts a native deep-link query", () => {
    expect(capabilityFromUrl("reticulum-appliance://connect?cap=abcd", true)).toBe("abcd");
  });
});

describe("successful API response decoding", () => {
  test("accepts empty 202 and 204 mutation responses", async () => {
    expect(await decodeSuccessResponse<void>(new Response(null, { status: 202 }))).toBeUndefined();
    expect(await decodeSuccessResponse<void>(new Response(null, { status: 204 }))).toBeUndefined();
  });

  test("decodes a nonempty JSON response", async () => {
    const response = new Response('{"outcome":"inserted"}', {
      headers: { "Content-Type": "application/json" },
      status: 202,
    });
    expect(await decodeSuccessResponse<{ outcome: string }>(response)).toEqual({
      outcome: "inserted",
    });
  });
});
