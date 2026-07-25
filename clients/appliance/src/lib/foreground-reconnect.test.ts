import { describe, expect, test } from "bun:test";

import {
  ForegroundReconnect,
  foregroundReconnectMessage,
  type RetryScheduler,
} from "./foreground-reconnect.ts";

interface ScheduledRetry {
  readonly callback: () => void;
  cancelled: boolean;
  readonly delayMs: number;
}

function recordingScheduler(retries: ScheduledRetry[]): RetryScheduler {
  return (callback, delayMs) => {
    const retry = { callback, cancelled: false, delayMs };
    retries.push(retry);
    return () => {
      retry.cancelled = true;
    };
  };
}

describe("foreground reconnect gate", () => {
  test("coalesces an active attempt and re-arms it after settlement", () => {
    const scheduled: ScheduledRetry[] = [];
    let retryRequests = 0;
    const reconnect = new ForegroundReconnect(
      () => {
        retryRequests += 1;
      },
      2_000,
      recordingScheduler(scheduled),
    );

    expect(reconnect.begin(0)).toBeTrue();
    expect(reconnect.begin(0)).toBeFalse();
    reconnect.settle();

    expect(scheduled).toHaveLength(1);
    expect(scheduled[0]?.delayMs).toBe(2_000);
    scheduled[0]?.callback();
    expect(retryRequests).toBe(1);
    expect(reconnect.begin(1)).toBeTrue();
  });

  test("continues scheduling settled failures until the owner suspends", () => {
    const scheduled: ScheduledRetry[] = [];
    let retryRequests = 0;
    const reconnect = new ForegroundReconnect(
      () => {
        retryRequests += 1;
      },
      2_000,
      recordingScheduler(scheduled),
    );

    for (let generation = 0; generation < 3; generation += 1) {
      expect(reconnect.begin(generation)).toBeTrue();
      reconnect.settle();
      expect(scheduled[generation]?.cancelled).toBeFalse();
      scheduled[generation]?.callback();
    }
    expect(retryRequests).toBe(3);

    reconnect.suspend();
    expect(reconnect.begin(3)).toBeTrue();
    reconnect.suspend();
    reconnect.settle();
    expect(scheduled).toHaveLength(3);
  });

  test("presents automatic scan misses as saved pairing with a neutral retry", () => {
    expect(foregroundReconnectMessage({ state: "attempting" })).toBe(
      "Pairing is saved. Connecting to the node.",
    );
    expect(
      foregroundReconnectMessage({
        state: "waiting_retry",
        reason: "No BLE appliance was found",
      }),
    ).toContain("Pairing is saved");
    expect(
      foregroundReconnectMessage({
        state: "waiting_retry",
        reason: "No BLE appliance was found",
      }),
    ).toContain("retrying automatically");
  });

  test("suspending cancels a pending retry and prevents an in-flight attempt from re-arming", () => {
    const scheduled: ScheduledRetry[] = [];
    let retryRequests = 0;
    const reconnect = new ForegroundReconnect(
      () => {
        retryRequests += 1;
      },
      2_000,
      recordingScheduler(scheduled),
    );

    expect(reconnect.begin(0)).toBeTrue();
    reconnect.settle();
    reconnect.suspend();
    expect(scheduled[0]?.cancelled).toBeTrue();
    scheduled[0]?.callback();
    expect(retryRequests).toBe(0);

    expect(reconnect.begin(0)).toBeTrue();
    reconnect.suspend();
    reconnect.settle();
    expect(scheduled).toHaveLength(1);
  });

  test("rejects invalid retry delays", () => {
    expect(() => new ForegroundReconnect(() => undefined, 0)).toThrow(
      "foreground reconnect delay must be positive",
    );
  });
});
