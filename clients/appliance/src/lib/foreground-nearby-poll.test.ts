import { describe, expect, test } from "bun:test";

import { ForegroundNearbyPoll, type NearbyPollScheduler } from "./foreground-nearby-poll.ts";

interface ScheduledPoll {
  readonly callback: () => void;
  cancelled: boolean;
  readonly delayMs: number;
}

function recordingScheduler(polls: ScheduledPoll[]): NearbyPollScheduler {
  return (callback, delayMs) => {
    const poll = { callback, cancelled: false, delayMs };
    polls.push(poll);
    return () => {
      poll.cancelled = true;
    };
  };
}

function deferred(): {
  readonly promise: Promise<void>;
  readonly resolve: () => void;
} {
  let resolvePromise: (() => void) | undefined;
  const promise = new Promise<void>((resolve) => {
    resolvePromise = resolve;
  });
  return {
    promise,
    resolve: () => resolvePromise?.(),
  };
}

async function settleMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("foreground Nearby polling", () => {
  test("waits for a read to settle before scheduling the next non-overlapping read", async () => {
    const scheduled: ScheduledPoll[] = [];
    const reads = [deferred(), deferred()];
    let readCount = 0;
    const poll = new ForegroundNearbyPoll(
      () => {
        const read = reads[readCount];
        readCount += 1;
        if (read === undefined) throw new Error("unexpected extra read");
        return read.promise;
      },
      10_000,
      recordingScheduler(scheduled),
    );

    poll.start();
    poll.start();
    expect(readCount).toBe(1);
    expect(scheduled).toHaveLength(0);

    reads[0]?.resolve();
    await settleMicrotasks();
    expect(scheduled).toHaveLength(1);
    expect(scheduled[0]?.delayMs).toBe(10_000);

    scheduled[0]?.callback();
    expect(readCount).toBe(2);
    expect(scheduled).toHaveLength(1);
    reads[1]?.resolve();
    await settleMicrotasks();
    expect(scheduled).toHaveLength(2);
  });

  test("cancels a pending poll and does not re-arm an in-flight read after stop", async () => {
    const scheduled: ScheduledPoll[] = [];
    const first = deferred();
    const poll = new ForegroundNearbyPoll(
      () => first.promise,
      10_000,
      recordingScheduler(scheduled),
    );

    poll.start();
    first.resolve();
    await settleMicrotasks();
    poll.stop();
    expect(scheduled[0]?.cancelled).toBeTrue();
    scheduled[0]?.callback();

    const inFlight = deferred();
    const second = new ForegroundNearbyPoll(
      () => inFlight.promise,
      10_000,
      recordingScheduler(scheduled),
    );
    second.start();
    second.stop();
    inFlight.resolve();
    await settleMicrotasks();
    expect(scheduled).toHaveLength(1);
  });

  test("continues its bounded cadence after the read owner reports a failure", async () => {
    const scheduled: ScheduledPoll[] = [];
    const poll = new ForegroundNearbyPoll(
      () => Promise.reject(new Error("temporary authenticated read failure")),
      10_000,
      recordingScheduler(scheduled),
    );

    poll.start();
    await settleMicrotasks();

    expect(scheduled).toHaveLength(1);
    expect(scheduled[0]?.delayMs).toBe(10_000);
  });

  test("rejects a non-positive or non-finite cadence", () => {
    expect(() => new ForegroundNearbyPoll(async () => undefined, 0)).toThrow(
      "foreground Nearby poll delay must be positive",
    );
    expect(() => new ForegroundNearbyPoll(async () => undefined, Number.NaN)).toThrow(
      "foreground Nearby poll delay must be positive",
    );
  });
});
