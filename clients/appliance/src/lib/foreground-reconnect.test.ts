import { describe, expect, test } from "bun:test";

import {
  ensureForegroundConnection,
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
  test("uses the required non-destructive ensure for automatic recovery", async () => {
    const events: string[] = [];
    await ensureForegroundConnection({
      async ensureConnected(): Promise<void> {
        events.push("ensure");
      },
    });

    expect(events).toEqual(["ensure"]);
  });

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

  test("presents unavailable routes as saved authorization with a neutral retry", () => {
    expect(foregroundReconnectMessage({ state: "attempting" })).toBe(
      "Appliance authorization is saved. Connecting through Reticulum.",
    );
    expect(
      foregroundReconnectMessage({
        state: "waiting_retry",
        reason: "No route to the management destination",
      }),
    ).toContain("Appliance authorization is saved");
    expect(
      foregroundReconnectMessage({
        state: "waiting_retry",
        reason: "No route to the management destination",
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

  test("keeps automatic reconnect inhibited until an explicit action allows it", () => {
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
    reconnect.inhibit();

    expect(scheduled[0]?.cancelled).toBeTrue();
    scheduled[0]?.callback();
    expect(retryRequests).toBe(0);
    expect(reconnect.begin(0)).toBeFalse();
    expect(reconnect.begin(1)).toBeFalse();

    reconnect.allow();
    expect(reconnect.begin(1)).toBeTrue();
  });

  test("rejects invalid retry delays", () => {
    expect(() => new ForegroundReconnect(() => undefined, 0)).toThrow(
      "foreground reconnect delay must be positive",
    );
  });
});
