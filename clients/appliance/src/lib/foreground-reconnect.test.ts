import { describe, expect, test } from "bun:test";

import { ForegroundReconnect, type RetryScheduler } from "./foreground-reconnect.ts";

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
