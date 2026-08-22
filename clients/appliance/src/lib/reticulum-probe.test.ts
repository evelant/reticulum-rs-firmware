import { describe, expect, test } from "bun:test";

import type { ReticulumProbePollResponse, ReticulumProbeStartRequest } from "../generated/api.ts";
import { syntheticReticulumInterfaceId } from "./reticulum-interface-id.ts";
import { ReticulumProbeController, type ReticulumProbeState } from "./reticulum-probe.ts";

class ManualSchedule {
  readonly callbacks: Array<() => void> = [];

  schedule = (callback: () => void): (() => void) => {
    this.callbacks.push(callback);
    return () => {
      const index = this.callbacks.indexOf(callback);
      if (index >= 0) this.callbacks.splice(index, 1);
    };
  };

  async runNext(): Promise<void> {
    const callback = this.callbacks.shift();
    if (callback === undefined) throw new Error("no scheduled probe poll");
    callback();
    await Bun.sleep(0);
  }
}

describe("Reticulum proof probe controller", () => {
  test("runs one bounded start/poll sequence and preserves receiver-local ingress", async () => {
    const starts: ReticulumProbeStartRequest[] = [];
    const polls: string[] = [];
    const responses: ReticulumProbePollResponse[] = [
      { state: "pending", phase: "awaiting_proof" },
      {
        state: "succeeded",
        result: {
          round_trip_ms: 1_234,
          hops: 2,
          ingress_observation: {
            interface_id: syntheticReticulumInterfaceId(7),
            signal: { rssi_dbm: -91, snr_db: 7 },
          },
        },
      },
    ];
    const schedule = new ManualSchedule();
    const controller = new ReticulumProbeController(
      {
        async reticulumProbeStart(request) {
          starts.push(request);
          return { id: "44".repeat(16), outcome: "accepted" };
        },
        async reticulumProbePoll(request) {
          polls.push(request.id);
          const response = responses.shift();
          if (response === undefined) throw new Error("unexpected extra poll");
          return response;
        },
      },
      {
        createIdempotencyKey: () => "22".repeat(16),
        now: () => 1_000,
        pollIntervalMs: 1,
        presentationTimeoutMs: 100,
        schedule: schedule.schedule,
      },
    );
    const states: ReticulumProbeState[] = [];
    controller.subscribe((state) => states.push(state));

    await controller.measure(` ${"AA".repeat(16)} `);
    expect(starts).toEqual([
      {
        destination: "aa".repeat(16),
        idempotency_key: "22".repeat(16),
      },
    ]);
    expect(controller.state).toMatchObject({ phase: null, status: "pending" });

    await schedule.runNext();
    expect(controller.state).toMatchObject({
      phase: "awaiting_proof",
      status: "pending",
    });
    await schedule.runNext();
    expect(controller.state).toMatchObject({
      status: "succeeded",
      result: {
        round_trip_ms: 1_234,
        hops: 2,
        ingress_observation: {
          interface_id: syntheticReticulumInterfaceId(7),
          signal: { rssi_dbm: -91, snr_db: 7 },
        },
      },
    });
    expect(polls).toEqual(["44".repeat(16), "44".repeat(16)]);
    expect(schedule.callbacks).toHaveLength(0);
    expect(states.some((state) => state.status === "starting")).toBe(true);
    controller.dispose();
  });

  test("rejects malformed inputs and stops locally at its presentation deadline", async () => {
    let now = 1_000;
    const schedule = new ManualSchedule();
    const controller = new ReticulumProbeController(
      {
        async reticulumProbeStart() {
          return { id: "55".repeat(16), outcome: "accepted" };
        },
        async reticulumProbePoll() {
          return { state: "pending", phase: "path_lookup" };
        },
      },
      {
        createIdempotencyKey: () => "33".repeat(16),
        now: () => now,
        pollIntervalMs: 1,
        presentationTimeoutMs: 10,
        schedule: schedule.schedule,
      },
    );

    await controller.measure("not-a-destination");
    expect(controller.state).toMatchObject({ status: "input_error" });
    await controller.measure("11".repeat(16));
    now = 1_010;
    await schedule.runNext();
    expect(controller.state).toMatchObject({ status: "timed_out" });
    expect(schedule.callbacks).toHaveLength(0);
    controller.dispose();
  });

  test("retains an accepted ID across poll failure and resumes without another start", async () => {
    const starts: ReticulumProbeStartRequest[] = [];
    const polls: string[] = [];
    let failPoll = true;
    const schedule = new ManualSchedule();
    const controller = new ReticulumProbeController(
      {
        async reticulumProbeStart(request) {
          starts.push(request);
          return { id: "66".repeat(16), outcome: "accepted" };
        },
        async reticulumProbePoll(request) {
          polls.push(request.id);
          if (failPoll) {
            failPoll = false;
            throw new Error("temporary BLE loss");
          }
          return {
            state: "succeeded",
            result: {
              round_trip_ms: 800,
              hops: 1,
              ingress_observation: {
                interface_id: syntheticReticulumInterfaceId(1),
                signal: null,
              },
            },
          };
        },
      },
      {
        createIdempotencyKey: () => "77".repeat(16),
        now: () => 1_000,
        pollIntervalMs: 1,
        presentationTimeoutMs: 100,
        schedule: schedule.schedule,
      },
    );

    await controller.measure("11".repeat(16));
    await schedule.runNext();
    expect(controller.state).toMatchObject({
      destination: "11".repeat(16),
      id: "66".repeat(16),
      stage: "poll",
      status: "error",
    });

    await controller.measure("11".repeat(16));
    expect(controller.state).toMatchObject({
      status: "succeeded",
      result: { round_trip_ms: 800 },
    });
    expect(starts).toHaveLength(1);
    expect(polls).toEqual(["66".repeat(16), "66".repeat(16)]);
    controller.dispose();
  });

  test("explicitly abandons only a retained poll recovery state", async () => {
    const schedule = new ManualSchedule();
    const controller = new ReticulumProbeController(
      {
        async reticulumProbeStart() {
          return { id: "88".repeat(16), outcome: "accepted" };
        },
        async reticulumProbePoll() {
          throw new Error("stale boot-scoped ID");
        },
      },
      {
        createIdempotencyKey: () => "99".repeat(16),
        pollIntervalMs: 1,
        schedule: schedule.schedule,
      },
    );

    controller.abandonRetainedProbe();
    expect(controller.state).toEqual({ status: "idle" });
    await controller.measure("11".repeat(16));
    await schedule.runNext();
    expect(controller.state).toMatchObject({ stage: "poll", status: "error" });
    controller.abandonRetainedProbe();
    expect(controller.state).toEqual({ status: "idle" });
    controller.dispose();
  });

  test("reset detaches a late start result from the next appliance presentation", async () => {
    let release: ((value: { id: string; outcome: "accepted" }) => void) | undefined;
    const controller = new ReticulumProbeController(
      {
        reticulumProbeStart: () =>
          new Promise((resolve) => {
            release = resolve;
          }),
        async reticulumProbePoll() {
          throw new Error("late start must not schedule polling");
        },
      },
      {
        createIdempotencyKey: () => "22".repeat(16),
      },
    );

    const measure = controller.measure("11".repeat(16));
    controller.reset();
    release?.({ id: "44".repeat(16), outcome: "accepted" });
    await measure;
    expect(controller.state).toEqual({ status: "idle" });
    controller.dispose();
  });
});
