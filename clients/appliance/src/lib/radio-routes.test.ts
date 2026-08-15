import { describe, expect, test } from "bun:test";

import type { RadioRoutesStatusView } from "../generated/api.ts";
import {
  compactDurationLabel,
  elapsedAgeLabel,
  loraDataTxEvidenceLabel,
  loraTxSummaryLabel,
  RadioRoutesController,
  type RadioRoutesControllerState,
  retainedRouteFamily,
  routeExpiryLabel,
} from "./radio-routes.ts";

function snapshot(uptimeMs: number): RadioRoutesStatusView {
  return {
    interfaces: [],
    lora: null,
    observed_peer_count: 0,
    retained_route_count: 0,
    rns: {
      announces_received: 0,
      dedup_drops: 0,
      forwarded: 0,
      invalid_drops: 0,
      links_closed: 0,
      links_established: 0,
      links_failed: 0,
      paths_expired: 0,
      paths_learned: 0,
      received: 0,
    },
    route_table_revision: 0,
    routes: [],
    uptime_ms: uptimeMs,
    usable_route_count: 0,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

describe("RadioRoutesController", () => {
  test("polls one snapshot at a time and accepts explicit refreshes", async () => {
    const scheduled: Array<() => void> = [];
    let reads = 0;
    const controller = new RadioRoutesController(
      {
        async radioRoutesStatus() {
          reads += 1;
          return snapshot(reads);
        },
      },
      {
        now: () => 42,
        pollIntervalMs: 10,
        schedule(callback) {
          scheduled.push(callback);
          return () => {};
        },
      },
    );

    await controller.activate("board-a");
    expect(controller.state).toMatchObject({
      deviceKey: "board-a",
      error: null,
      loadState: "ready",
      snapshot: { uptime_ms: 1 },
      updatedAtMs: 42,
    });
    expect(scheduled).toHaveLength(1);

    scheduled.shift()?.();
    await Bun.sleep(0);
    expect(controller.state.snapshot?.uptime_ms).toBe(2);

    await controller.refresh();
    expect(controller.state.snapshot?.uptime_ms).toBe(3);
    controller.dispose();
  });

  test("rejects an old appliance result and serializes the replacement read", async () => {
    const first = deferred<RadioRoutesStatusView>();
    const second = deferred<RadioRoutesStatusView>();
    let reads = 0;
    const states: RadioRoutesControllerState[] = [];
    const controller = new RadioRoutesController(
      {
        radioRoutesStatus() {
          reads += 1;
          return reads === 1 ? first.promise : second.promise;
        },
      },
      { schedule: () => () => {} },
    );
    controller.subscribe((state) => states.push(state));

    const firstActivation = controller.activate("board-a");
    const secondActivation = controller.activate("board-b");
    expect(reads).toBe(1);

    first.resolve(snapshot(1));
    await firstActivation;
    await Bun.sleep(0);
    expect(reads).toBe(2);
    expect(controller.state).toMatchObject({
      deviceKey: "board-b",
      loadState: "loading",
      snapshot: null,
    });

    second.resolve(snapshot(2));
    await secondActivation;
    await Bun.sleep(0);
    expect(controller.state).toMatchObject({
      deviceKey: "board-b",
      loadState: "ready",
      snapshot: { uptime_ms: 2 },
    });
    expect(states.some((state) => state.deviceKey === "board-a" && state.snapshot !== null)).toBe(
      false,
    );
    controller.dispose();
  });

  test("retains the last good snapshot across a polling error and stops when suspended", async () => {
    const scheduled: Array<{ callback: () => void; cancelled: boolean }> = [];
    let reads = 0;
    const controller = new RadioRoutesController(
      {
        async radioRoutesStatus() {
          reads += 1;
          if (reads === 2) throw new Error("BLE unavailable");
          return snapshot(reads);
        },
      },
      {
        schedule(callback) {
          const entry = { callback, cancelled: false };
          scheduled.push(entry);
          return () => {
            entry.cancelled = true;
          };
        },
      },
    );

    await controller.activate("board-a");
    scheduled.at(-1)?.callback();
    await Bun.sleep(0);
    expect(controller.state).toMatchObject({
      error: "BLE unavailable",
      loadState: "ready",
      snapshot: { uptime_ms: 1 },
    });

    controller.suspend();
    expect(scheduled.at(-1)?.cancelled).toBe(true);
    const before = reads;
    scheduled.at(-1)?.callback();
    await Bun.sleep(0);
    expect(reads).toBe(before);
    controller.dispose();
  });
});

describe("radio and route age labels", () => {
  test("renders compact known durations", () => {
    expect(compactDurationLabel(999)).toBe("now");
    expect(compactDurationLabel(59_999)).toBe("59s");
    expect(compactDurationLabel(60_000)).toBe("1m");
    expect(compactDurationLabel(3_600_000)).toBe("1h");
    expect(compactDurationLabel(172_800_000)).toBe("2d");
  });

  test("does not describe unknown route ages as elapsed time", () => {
    expect(elapsedAgeLabel(null)).toBe("unknown");
    expect(elapsedAgeLabel(1_500)).toBe("1s ago");
    expect(routeExpiryLabel(null)).toBe("Expiry: unknown");
    expect(routeExpiryLabel(120_000)).toBe("Expires in 2m");
  });

  test("labels DATA evidence without implying authorization or RF completion", () => {
    const lastTx = {
      age_ms: 1_500,
      outcome: "access_rejected",
      family: "data",
      data_evidence: {
        interface_id: 1,
        encoded_packet_len: 183,
        encoded_packet_sha256: "ab".repeat(32),
      },
    } as const;
    expect(loraTxSummaryLabel(lastTx)).toBe("1s ago · data · access rejected");
    expect(loraDataTxEvidenceLabel(lastTx)).toBe("Interface 1 · 183 bytes");
  });
});

describe("retainedRouteFamily", () => {
  const route = (retainedInterfaceId: number | null): Parameters<typeof retainedRouteFamily>[0] =>
    ({
      destination: "01ab",
      expires_in_ms: 30_000,
      hops: 1,
      last_local_use_age_ms: 1_000,
      learned_age_ms: 1_000,
      next_hop_identity: null,
      resolution: "exact_ready",
      retained_interface_id: retainedInterfaceId,
    }) as const;

  test("resolves the retained interface kind to a transport family", () => {
    const view: RadioRoutesStatusView = {
      ...snapshot(1),
      interfaces: [
        {
          id: 1,
          kind: "lora",
          state: "online",
          generation: 1,
          logical_mtu: 500,
          bitrate: null,
        },
        {
          id: 2,
          kind: "tcp",
          state: "online",
          generation: 1,
          logical_mtu: 1480,
          bitrate: 1_000_000,
        },
        {
          id: 3,
          kind: "other",
          state: "offline",
          generation: 1,
          logical_mtu: 500,
          bitrate: null,
        },
      ],
    };
    expect(retainedRouteFamily(route(1), view)).toBe("lora");
    expect(retainedRouteFamily(route(2), view)).toBe("tcp");
    expect(retainedRouteFamily(route(3), view)).toBe("other");
  });

  test("treats broadcast fallback and unresolved interfaces as other", () => {
    const view: RadioRoutesStatusView = { ...snapshot(1), interfaces: [] };
    expect(retainedRouteFamily(route(null), view)).toBe("other");
    expect(retainedRouteFamily(route(9), view)).toBe("other");
  });
});
