import { describe, expect, test } from "bun:test";

import {
  directLxmfPayloadBudget,
  directLxmfPayloadError,
  encodedMessagePackBinaryBytes,
} from "./lxmf-message-size.ts";

describe("direct LXMF composer preflight", () => {
  test("uses exact MessagePack bin8 and bin16 widths", () => {
    expect(encodedMessagePackBinaryBytes(0)).toBe(2);
    expect(encodedMessagePackBinaryBytes(255)).toBe(257);
    expect(encodedMessagePackBinaryBytes(256)).toBe(259);
    expect(encodedMessagePackBinaryBytes(65_535)).toBe(65_538);
    expect(() => encodedMessagePackBinaryBytes(65_536)).toThrow("bin16");
  });

  test("accepts current located empty-title content at 268 bytes", () => {
    expect(directLxmfPayloadBudget(0, 268, true)).toEqual({
      fieldsEncodedBytes: 52,
      fits: true,
      maximumPayloadBytes: 335,
      overByBytes: 0,
      payloadBytes: 335,
    });
    expect(directLxmfPayloadError(0, 268, true)).toBeNull();
  });

  test("rejects current located empty-title content at 269 bytes", () => {
    expect(directLxmfPayloadBudget(0, 269, true)).toMatchObject({
      fits: false,
      overByBytes: 1,
      payloadBytes: 336,
    });
    expect(directLxmfPayloadError(0, 269, true)).toContain(
      "1 byte too large with attached location",
    );
  });

  test("rejects an individually valid but combined oversized unlocated message", () => {
    const budget = directLxmfPayloadBudget(295, 295, false);
    expect(budget).toEqual({
      fieldsEncodedBytes: 1,
      fits: false,
      maximumPayloadBytes: 335,
      overByBytes: 272,
      payloadBytes: 607,
    });
    expect(directLxmfPayloadError(295, 295, false)).toContain("272 bytes too large");
  });
});
