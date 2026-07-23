import { describe, expect, test } from "bun:test";

import { keyboardLayoutPolicy } from "./keyboard-layout.ts";

describe("keyboard layout policy", () => {
  test("uses padding and interactive dismissal on iOS", () => {
    expect(keyboardLayoutPolicy("ios")).toEqual({
      avoidingBehavior: "padding",
      avoidingEnabled: true,
      dismissMode: "interactive",
    });
  });

  test("uses height and drag dismissal on Android", () => {
    expect(keyboardLayoutPolicy("android")).toEqual({
      avoidingBehavior: "height",
      avoidingEnabled: true,
      dismissMode: "on-drag",
    });
  });

  test("does not alter the web or desktop layout", () => {
    expect(keyboardLayoutPolicy("web").avoidingEnabled).toBeFalse();
    expect(keyboardLayoutPolicy("macos").avoidingEnabled).toBeFalse();
  });
});
