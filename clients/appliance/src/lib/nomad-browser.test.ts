import { describe, expect, test } from "bun:test";

import type {
  NomadFetchPollRequest,
  NomadFetchPollResponse,
  NomadFetchStartRequest,
  NomadFetchStartResponse,
} from "../generated/api.ts";
import {
  DEFAULT_NOMAD_PAGE_PATH,
  NomadBrowserController,
  type NomadBrowserState,
  type NomadFetchClient,
  type NomadPollScheduler,
  nomadDestinationHintApplication,
  nomadRequestProvenance,
} from "./nomad-browser.ts";

interface ScheduledPoll {
  readonly callback: () => void;
  cancelled: boolean;
  readonly delayMs: number;
}

interface Deferred<Value> {
  readonly promise: Promise<Value>;
  reject(error: unknown): void;
  resolve(value: Value): void;
}

function deferred<Value>(): Deferred<Value> {
  let reject: (error: unknown) => void = () => undefined;
  let resolve: (value: Value) => void = () => undefined;
  const promise = new Promise<Value>((resolvePromise, rejectPromise) => {
    reject = rejectPromise;
    resolve = resolvePromise;
  });
  return { promise, reject, resolve };
}

function recordingScheduler(scheduled: ScheduledPoll[]): NomadPollScheduler {
  return (callback, delayMs) => {
    const poll = { callback, cancelled: false, delayMs };
    scheduled.push(poll);
    return () => {
      poll.cancelled = true;
    };
  };
}

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function accepted(id = "33".repeat(16)): NomadFetchStartResponse {
  return { id, outcome: "accepted" };
}

describe("Nomad browser controller", () => {
  test("consumes a nearby hint once so it cannot overwrite a later manual destination", () => {
    const nearby = "11".repeat(16);
    const manual = "22".repeat(16);
    let hint: string | null = nearby;
    const initial = nomadDestinationHintApplication("", hint, false);
    expect(initial).toEqual({ consumed: true, destination: nearby });
    if (initial.consumed) hint = null;

    expect(nomadDestinationHintApplication(manual, hint, true)).toEqual({
      consumed: false,
      destination: manual,
    });
    expect(nomadDestinationHintApplication(manual, hint, false)).toEqual({
      consumed: false,
      destination: manual,
    });
  });

  test("keeps a ready page bound to its request when a deferred hint targets another peer", () => {
    const fetched = "22".repeat(16);
    const deferredNearby = "11".repeat(16);
    const state: NomadBrowserState = {
      id: "33".repeat(16),
      outcome: "accepted",
      page: ">Fetched from B",
      phase: "awaiting_response",
      request: {
        destination: fetched,
        idempotency_key: "44".repeat(16),
        path: "/page/from-b.mu",
        timestamp_unix_ms: 700,
      },
      status: "ready",
    };

    expect(nomadDestinationHintApplication(fetched, deferredNearby, false)).toEqual({
      consumed: true,
      destination: deferredNearby,
    });
    expect(nomadRequestProvenance(state)).toEqual({
      destination: fetched,
      path: "/page/from-b.mu",
    });
  });

  test("uses the default index path and retains timestamp/key across an ambiguous retry", async () => {
    const starts: NomadFetchStartRequest[] = [];
    let identityCreations = 0;
    let timestampReads = 0;
    let startAttempts = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart(request) {
        starts.push(request);
        startAttempts += 1;
        if (startAttempts === 1) throw new Error("connection closed after write");
        return { ...accepted(), outcome: "replayed" };
      },
      async nomadFetchPoll() {
        return { state: "ready", page: ">Metalbeard" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => {
        identityCreations += 1;
        return "22".repeat(16);
      },
      now: () => {
        timestampReads += 1;
        return 1_784_732_100_123;
      },
    });

    await controller.start("11".repeat(16));
    expect(controller.state.status).toBe("start_error");
    await controller.retryStart();

    expect(starts).toHaveLength(2);
    expect(starts[0]).toEqual({
      destination: "11".repeat(16),
      idempotency_key: "22".repeat(16),
      path: DEFAULT_NOMAD_PAGE_PATH,
      timestamp_unix_ms: 1_784_732_100_123,
    });
    expect(starts[1]).toEqual(starts[0] as NomadFetchStartRequest);
    expect(identityCreations).toBe(1);
    expect(timestampReads).toBe(3);
    expect(controller.state).toMatchObject({
      id: "33".repeat(16),
      outcome: "replayed",
      page: ">Metalbeard",
      status: "ready",
    });
  });

  test("awaits each pending poll before scheduling the next one", async () => {
    const scheduled: ScheduledPoll[] = [];
    const polls: Deferred<NomadFetchPollResponse>[] = [];
    let activePolls = 0;
    let maximumActivePolls = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return accepted();
      },
      async nomadFetchPoll(_request: NomadFetchPollRequest) {
        const result = deferred<NomadFetchPollResponse>();
        polls.push(result);
        activePolls += 1;
        maximumActivePolls = Math.max(maximumActivePolls, activePolls);
        try {
          return await result.promise;
        } finally {
          activePolls -= 1;
        }
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 100,
      schedule: recordingScheduler(scheduled),
    });

    const running = controller.start("11".repeat(16), "/page/demo.mu");
    await flushPromises();
    expect(polls).toHaveLength(1);
    expect(scheduled).toHaveLength(0);
    const pending = controller.state;
    controller.abandonRetainedFetch();
    expect(controller.state).toBe(pending);

    polls[0]?.resolve({ state: "pending", phase: "link_establishment" });
    await flushPromises();
    expect(scheduled).toHaveLength(1);
    expect(scheduled[0]?.delayMs).toBe(1_000);
    expect(polls).toHaveLength(1);

    scheduled[0]?.callback();
    await flushPromises();
    expect(polls).toHaveLength(2);
    expect(maximumActivePolls).toBe(1);
    polls[1]?.resolve({ state: "ready", page: "hello" });
    await running;
    expect(controller.state).toMatchObject({ page: "hello", status: "ready" });
  });

  test("projects a terminal device failure without another poll", async () => {
    let polls = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return accepted();
      },
      async nomadFetchPoll() {
        polls += 1;
        return { state: "failed", failure: "no_path" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 200,
    });

    await controller.start("11".repeat(16));

    expect(polls).toBe(1);
    expect(controller.state).toMatchObject({
      failure: "no_path",
      id: "33".repeat(16),
      status: "failed",
    });
  });

  test("poll transport error retains the accepted ID for an exact resume", async () => {
    const polledIds: string[] = [];
    let polls = 0;
    let starts = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        starts += 1;
        return accepted("55".repeat(16));
      },
      async nomadFetchPoll(request) {
        polledIds.push(request.id);
        polls += 1;
        if (polls === 1) throw new Error("BLE link reconnecting");
        return { state: "ready", page: "after reconnect" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 250,
    });

    await controller.start("11".repeat(16));
    expect(controller.state).toMatchObject({
      error: "BLE link reconnecting",
      id: "55".repeat(16),
      status: "poll_error",
    });
    const retained = controller.state;
    await controller.start("66".repeat(16), "/page/other.mu");
    expect(starts).toBe(1);
    expect(controller.state).toBe(retained);

    await controller.resumePolling();
    expect(polledIds).toEqual(["55".repeat(16), "55".repeat(16)]);
    expect(controller.state).toMatchObject({
      id: "55".repeat(16),
      page: "after reconnect",
      status: "ready",
    });
  });

  test("explicit abandon releases a poll-error ID and permits a fresh request identity", async () => {
    const starts: NomadFetchStartRequest[] = [];
    let polls = 0;
    let keys = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart(request) {
        starts.push(request);
        return accepted(starts.length === 1 ? "66".repeat(16) : "77".repeat(16));
      },
      async nomadFetchPoll() {
        polls += 1;
        if (polls === 1) throw new Error("fetch ID is stale after reset");
        return { state: "ready", page: "fresh boot" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => {
        keys += 1;
        return (keys === 1 ? "22" : "33").repeat(16);
      },
      now: () => 500,
    });

    await controller.start("11".repeat(16));
    expect(controller.state).toMatchObject({
      id: "66".repeat(16),
      status: "poll_error",
    });

    controller.abandonRetainedFetch();
    expect(controller.state).toEqual({ status: "idle" });
    await controller.start("44".repeat(16), "/page/new.mu");

    expect(starts).toHaveLength(2);
    expect(starts[0]?.idempotency_key).toBe("22".repeat(16));
    expect(starts[1]).toMatchObject({
      destination: "44".repeat(16),
      idempotency_key: "33".repeat(16),
      path: "/page/new.mu",
    });
    expect(controller.state).toMatchObject({
      id: "77".repeat(16),
      page: "fresh boot",
      status: "ready",
    });
  });

  test("presentation timeout retains the ID and resume polls that exact fetch", async () => {
    const scheduled: ScheduledPoll[] = [];
    const polledIds: string[] = [];
    let now = 1_000;
    let polls = 0;
    let starts = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        starts += 1;
        return accepted("44".repeat(16));
      },
      async nomadFetchPoll(request) {
        polledIds.push(request.id);
        polls += 1;
        return polls === 3
          ? { state: "ready", page: "resumed" }
          : { state: "pending", phase: "awaiting_response" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => now,
      pollIntervalMs: 1_000,
      presentationTimeoutMs: 1_500,
      schedule: recordingScheduler(scheduled),
    });

    const running = controller.start("11".repeat(16));
    await flushPromises();
    expect(scheduled[0]?.delayMs).toBe(1_000);
    now += 1_000;
    scheduled[0]?.callback();
    await flushPromises();
    expect(scheduled[1]?.delayMs).toBe(500);
    now += 500;
    scheduled[1]?.callback();
    await running;

    expect(controller.state).toMatchObject({
      id: "44".repeat(16),
      phase: "awaiting_response",
      status: "timed_out",
    });
    const retained = controller.state;
    await controller.start("66".repeat(16), "/page/other.mu");
    expect(starts).toBe(1);
    expect(controller.state).toBe(retained);
    await controller.resumePolling();
    expect(polledIds).toEqual(["44".repeat(16), "44".repeat(16), "44".repeat(16)]);
    expect(controller.state).toMatchObject({
      id: "44".repeat(16),
      page: "resumed",
      status: "ready",
    });
  });

  test("explicit abandon also releases a presentation-timeout ID", async () => {
    let now = 600;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return accepted("88".repeat(16));
      },
      async nomadFetchPoll() {
        now += 1;
        return { state: "pending", phase: "awaiting_response" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => now,
      presentationTimeoutMs: 1,
    });

    await controller.start("11".repeat(16));
    expect(controller.state).toMatchObject({
      id: "88".repeat(16),
      status: "timed_out",
    });
    controller.abandonRetainedFetch();
    expect(controller.state).toEqual({ status: "idle" });
  });

  test("reset ignores a deferred start result and does not begin polling", async () => {
    const starting = deferred<NomadFetchStartResponse>();
    let polls = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return starting.promise;
      },
      async nomadFetchPoll() {
        polls += 1;
        return { state: "ready", page: "stale page" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 700,
    });

    const running = controller.start("11".repeat(16));
    await flushPromises();
    expect(controller.state.status).toBe("starting");

    controller.reset();
    expect(controller.state).toEqual({ status: "idle" });
    starting.resolve(accepted());
    await running;

    expect(polls).toBe(0);
    expect(controller.state).toEqual({ status: "idle" });
  });

  test("reset ignores a deferred poll result", async () => {
    const polling = deferred<NomadFetchPollResponse>();
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return accepted();
      },
      async nomadFetchPoll() {
        return polling.promise;
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 800,
    });

    const running = controller.start("11".repeat(16));
    await flushPromises();
    expect(controller.state.status).toBe("pending");

    controller.reset();
    expect(controller.state).toEqual({ status: "idle" });
    polling.resolve({ state: "ready", page: "wrong appliance" });
    await running;

    expect(controller.state).toEqual({ status: "idle" });
  });

  test("reset cancels a scheduled poll and prevents another request", async () => {
    const scheduled: ScheduledPoll[] = [];
    let polls = 0;
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return accepted();
      },
      async nomadFetchPoll() {
        polls += 1;
        return { state: "pending", phase: "path_lookup" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 900,
      schedule: recordingScheduler(scheduled),
    });

    const running = controller.start("11".repeat(16));
    await flushPromises();
    expect(polls).toBe(1);
    expect(scheduled).toHaveLength(1);

    controller.reset();
    await running;
    expect(scheduled[0]?.cancelled).toBeTrue();
    expect(controller.state).toEqual({ status: "idle" });

    scheduled[0]?.callback();
    await flushPromises();
    expect(polls).toBe(1);
    expect(controller.state).toEqual({ status: "idle" });
  });

  test("dispose cancels a scheduled poll and ignores later work", async () => {
    const scheduled: ScheduledPoll[] = [];
    const client: NomadFetchClient = {
      async nomadFetchStart() {
        return accepted();
      },
      async nomadFetchPoll() {
        return { state: "pending", phase: "path_lookup" };
      },
    };
    const controller = new NomadBrowserController(client, {
      createIdempotencyKey: () => "22".repeat(16),
      now: () => 300,
      schedule: recordingScheduler(scheduled),
    });

    const running = controller.start("11".repeat(16));
    await flushPromises();
    controller.dispose();
    await running;

    expect(scheduled[0]?.cancelled).toBeTrue();
    const state = controller.state;
    scheduled[0]?.callback();
    await flushPromises();
    expect(controller.state).toBe(state);
  });
});
