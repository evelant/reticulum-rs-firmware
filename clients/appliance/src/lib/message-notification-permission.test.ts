import { describe, expect, test } from "bun:test";

import { projectMessageNotificationPermission } from "./message-notification-permission.ts";

describe("message notification permission projection", () => {
  test("enables delivery only when both the app and Android channel allow it", () => {
    expect(
      projectMessageNotificationPermission({
        androidChannelBlocked: false,
        applicationCanAskAgain: false,
        applicationGranted: true,
      }),
    ).toEqual({ state: "enabled" });

    expect(
      projectMessageNotificationPermission({
        androidChannelBlocked: true,
        applicationCanAskAgain: true,
        applicationGranted: true,
      }),
    ).toEqual({
      canAskAgain: false,
      reason: "android_channel",
      state: "disabled",
    });
  });

  test("preserves whether the application permission can be requested again", () => {
    expect(
      projectMessageNotificationPermission({
        androidChannelBlocked: false,
        applicationCanAskAgain: true,
        applicationGranted: false,
      }),
    ).toEqual({
      canAskAgain: true,
      reason: "application",
      state: "disabled",
    });
  });
});
