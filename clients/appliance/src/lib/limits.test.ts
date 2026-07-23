import { describe, expect, test } from "bun:test";

import { byteLimitError, utf8ByteLength } from "./limits.ts";

describe("UTF-8 API limits", () => {
  test("counts protocol bytes rather than JavaScript UTF-16 units", () => {
    expect("😀".length).toBe(2);
    expect(utf8ByteLength("😀")).toBe(4);
    expect(utf8ByteLength("a".repeat(295))).toBe(295);
  });

  test("rejects non-ASCII text beyond the exact byte ceiling", () => {
    expect(byteLimitError("😀".repeat(73), 295, "Message")).toBeNull();
    expect(byteLimitError("😀".repeat(74), 295, "Message")).toBe(
      "Message is 296 bytes; the maximum is 295",
    );
  });
});
