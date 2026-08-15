import { describe, expect, test } from "bun:test";

import { SettledPoll, type SettledPollScheduler } from "./settled-poll.ts";

interface ScheduledPoll {
  readonly callback: () => void;
  cancelled: boolean;
  readonly delayMs: number;
}

function recordingScheduler(polls: ScheduledPoll[]): SettledPollScheduler {
  return (callback, delayMs) => {
    const poll = { callback, cancelled: false, delayMs };
    polls.push(poll);
    return () => {
      poll.cancelled = true;
    };
  };
}

function deferred(): { readonly promise: Promise<void>; resolve(): void } {
  let resolvePromise: (() => void) | undefined;
  return {
    promise: new Promise<void>((resolve) => {
      resolvePromise = resolve;
    }),
    resolve: () => resolvePromise?.(),
  };
}

async function settleMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("settled polling", () => {
  test("does not schedule another turn until a slow read settles", async () => {
    const scheduled: ScheduledPoll[] = [];
    const first = deferred();
    let reads = 0;
    const poll = new SettledPoll(
      () => {
        reads += 1;
        return first.promise;
      },
      2_000,
      undefined,
      recordingScheduler(scheduled),
    );

    poll.start();
    expect(scheduled).toHaveLength(1);
    scheduled[0]?.callback();
    expect(reads).toBe(1);
    expect(scheduled).toHaveLength(1);

    first.resolve();
    await settleMicrotasks();
    expect(scheduled).toHaveLength(2);
    expect(scheduled[1]?.delayMs).toBe(2_000);
  });

  test("skips a turn blocked by a mutation instead of queueing a read", async () => {
    const scheduled: ScheduledPoll[] = [];
    let blocked = true;
    let reads = 0;
    const poll = new SettledPoll(
      async () => {
        reads += 1;
      },
      500,
      () => blocked,
      recordingScheduler(scheduled),
    );

    poll.start();
    scheduled[0]?.callback();
    await settleMicrotasks();
    expect(reads).toBe(0);
    expect(scheduled).toHaveLength(2);

    blocked = false;
    scheduled[1]?.callback();
    await settleMicrotasks();
    expect(reads).toBe(1);
    expect(scheduled).toHaveLength(3);
  });

  test("stopping cancels a scheduled turn and prevents an in-flight read from rearming", async () => {
    const scheduled: ScheduledPoll[] = [];
    const read = deferred();
    const poll = new SettledPoll(() => read.promise, 500, undefined, recordingScheduler(scheduled));

    poll.start();
    poll.stop();
    expect(scheduled[0]?.cancelled).toBeTrue();

    const second = new SettledPoll(
      () => read.promise,
      500,
      undefined,
      recordingScheduler(scheduled),
    );
    second.start();
    scheduled[1]?.callback();
    second.stop();
    read.resolve();
    await settleMicrotasks();
    expect(scheduled).toHaveLength(2);
  });
});
