import { describe, expect, test } from "bun:test";

import { acquireExclusiveResource } from "./exclusive-resource.ts";

describe("exclusive process resource", () => {
  test("waits for the previous close before opening a replacement", async () => {
    const events: string[] = [];
    const first = await acquireExclusiveResource(
      () => {
        events.push("open first");
        return "first";
      },
      (value) => {
        events.push(`close ${value}`);
      },
    );
    const secondPending = acquireExclusiveResource(
      () => {
        events.push("open second");
        return "second";
      },
      (value) => {
        events.push(`close ${value}`);
      },
    );

    await Promise.resolve();
    expect(events).toEqual(["open first"]);
    await first.release();
    const second = await secondPending;
    expect(events).toEqual(["open first", "close first", "open second"]);
    await second.release();
  });

  test("releases the queue after an open failure and closes only once", async () => {
    await expect(
      acquireExclusiveResource(
        () => {
          throw new Error("open failed");
        },
        () => {},
      ),
    ).rejects.toThrow("open failed");

    let closes = 0;
    const next = await acquireExclusiveResource(
      () => "next",
      () => {
        closes += 1;
      },
    );
    await next.release();
    await next.release();
    expect(closes).toBe(1);
  });
});
