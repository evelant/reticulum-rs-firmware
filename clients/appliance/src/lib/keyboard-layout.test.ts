import { describe, expect, test } from "bun:test";

import { keyboardLayoutPolicy } from "./keyboard-layout.ts";

describe("keyboard layout policy", () => {
  test("uses padding and interactive dismissal on iOS", () => {
    expect(keyboardLayoutPolicy("ios")).toEqual({
      avoidingBehavior: "padding",
      avoidingEnabled: true,
      dismissMode: "interactive",
      inputAccessoryEnabled: true,
    });
  });

  test("uses height and drag dismissal on Android", () => {
    expect(keyboardLayoutPolicy("android")).toEqual({
      avoidingBehavior: "height",
      avoidingEnabled: true,
      dismissMode: "on-drag",
      inputAccessoryEnabled: false,
    });
  });

  test("does not alter the web or desktop layout", () => {
    expect(keyboardLayoutPolicy("web").avoidingEnabled).toBeFalse();
    expect(keyboardLayoutPolicy("web").inputAccessoryEnabled).toBeFalse();
    expect(keyboardLayoutPolicy("macos").avoidingEnabled).toBeFalse();
    expect(keyboardLayoutPolicy("macos").inputAccessoryEnabled).toBeFalse();
  });
});
