import { describe, expect, mock, test } from "bun:test";
import { syntheticReticulumInterfaceId } from "./reticulum-interface-id.ts";

mock.module("expo-linking", () => ({
  getInitialURL: async () => null,
}));
mock.module("react-native", () => ({
  Platform: { OS: "ios" },
}));

describe("HTTP appliance Nomad bridge", () => {
  test("posts a bounded durable activity query without using the event stream", async () => {
    const { HttpApplianceClient } = await import("./http-appliance-client.ts");
    const originalFetch = globalThis.fetch;
    let observed:
      | {
          readonly body: unknown;
          readonly headers: Headers;
          readonly url: string;
        }
      | undefined;
    globalThis.fetch = (async (input, init) => {
      observed = {
        body: JSON.parse(String(init?.body)),
        headers: new Headers(init?.headers),
        url: String(input),
      };
      return Response.json({
        events: [
          {
            event_id: 7,
            observed_at_unix_ms: 1_500,
            timeline_sequence: 3,
            peer: "11".repeat(16),
            direction: "outbound",
            outbox_id: 2,
            attempt_number: 1,
            activity: { kind: "outbound_queued" },
          },
        ],
        next_before_event_id: null,
        history_incomplete: false,
      });
    }) as typeof fetch;

    try {
      const client = new HttpApplianceClient("http://127.0.0.1:61141/");
      expect(
        await client.messageActivity({
          before_event_id: null,
          limit: 20,
          timeline_sequence: 3,
        }),
      ).toMatchObject({
        events: [{ event_id: 7, activity: { kind: "outbound_queued" } }],
        history_incomplete: false,
      });
    } finally {
      globalThis.fetch = originalFetch;
    }

    expect(observed).toBeDefined();
    expect(observed?.url).toBe("http://127.0.0.1:61141/api/v1/activity/query");
    expect(observed?.body).toEqual({
      before_event_id: null,
      limit: 20,
      timeline_sequence: 3,
    });
    expect(observed?.headers.get("content-type")).toBe("application/json");
    expect(observed?.headers.get("x-reticulum-client")).toBe("web-alpha");
  });

  test("posts a bounded durable RF trace query", async () => {
    const { HttpApplianceClient } = await import("./http-appliance-client.ts");
    const originalFetch = globalThis.fetch;
    let observed: { readonly body: unknown; readonly url: string } | undefined;
    globalThis.fetch = (async (input, init) => {
      observed = { body: JSON.parse(String(init?.body)), url: String(input) };
      return Response.json({ events: [], next_before_event_id: null, history_incomplete: false });
    }) as typeof fetch;

    try {
      const client = new HttpApplianceClient("http://127.0.0.1:61141/");
      expect(
        await client.radioTrace({
          before_event_id: 12,
          limit: 100,
          timeline_sequence: 3,
        }),
      ).toEqual({ events: [], next_before_event_id: null, history_incomplete: false });
    } finally {
      globalThis.fetch = originalFetch;
    }

    expect(observed).toEqual({
      body: { before_event_id: 12, limit: 100, timeline_sequence: 3 },
      url: "http://127.0.0.1:61141/api/v1/radio-trace/query",
    });
  });

  test("sends generated start and poll DTOs through the guarded actor routes", async () => {
    const { HttpApplianceClient } = await import("./http-appliance-client.ts");
    const originalFetch = globalThis.fetch;
    const calls: Array<{
      readonly body: unknown;
      readonly headers: Headers;
      readonly url: string;
    }> = [];
    const fetchId = `${"33".repeat(8)}0000000000000001`;
    globalThis.fetch = (async (input, init) => {
      const headers = new Headers(init?.headers);
      calls.push({
        body: JSON.parse(String(init?.body)),
        headers,
        url: String(input),
      });
      const response =
        calls.length === 1
          ? { id: fetchId, outcome: "accepted" }
          : { state: "ready", page: ">Metalbeard" };
      return Response.json(response, { status: calls.length === 1 ? 202 : 200 });
    }) as typeof fetch;

    try {
      const client = new HttpApplianceClient("http://127.0.0.1:61141/");
      expect(
        await client.nomadFetchStart({
          destination: "11".repeat(16),
          path: "/page/index.mu",
          timestamp_unix_ms: 1,
          idempotency_key: "22".repeat(16),
        }),
      ).toEqual({ id: fetchId, outcome: "accepted" });
      expect(await client.nomadFetchPoll({ id: fetchId })).toEqual({
        state: "ready",
        page: ">Metalbeard",
      });
    } finally {
      globalThis.fetch = originalFetch;
    }

    expect(calls.map(({ body, url }) => ({ body, url }))).toEqual([
      {
        body: {
          destination: "11".repeat(16),
          path: "/page/index.mu",
          timestamp_unix_ms: 1,
          idempotency_key: "22".repeat(16),
        },
        url: "http://127.0.0.1:61141/api/v1/nomad/fetches",
      },
      {
        body: { id: fetchId },
        url: "http://127.0.0.1:61141/api/v1/nomad/fetches/poll",
      },
    ]);
    for (const call of calls) {
      expect(call.headers.get("content-type")).toBe("application/json");
      expect(call.headers.get("x-reticulum-client")).toBe("web-alpha");
    }
  });

  test("sends generated Reticulum proof-probe DTOs through the guarded actor routes", async () => {
    const { HttpApplianceClient } = await import("./http-appliance-client.ts");
    const originalFetch = globalThis.fetch;
    const calls: Array<{ readonly body: unknown; readonly url: string }> = [];
    const probeId = "44".repeat(16);
    globalThis.fetch = (async (input, init) => {
      calls.push({
        body: JSON.parse(String(init?.body)),
        url: String(input),
      });
      return Response.json(
        calls.length === 1
          ? { id: probeId, outcome: "accepted" }
          : {
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
        { status: calls.length === 1 ? 202 : 200 },
      );
    }) as typeof fetch;

    try {
      const client = new HttpApplianceClient("http://127.0.0.1:61141/");
      expect(
        await client.reticulumProbeStart({
          destination: "11".repeat(16),
          idempotency_key: "22".repeat(16),
        }),
      ).toEqual({ id: probeId, outcome: "accepted" });
      expect(await client.reticulumProbePoll({ id: probeId })).toMatchObject({
        state: "succeeded",
        result: {
          round_trip_ms: 1_234,
          hops: 2,
          ingress_observation: {
            interface_id: syntheticReticulumInterfaceId(7),
            signal: { rssi_dbm: -91, snr_db: 7 },
          },
        },
      });
    } finally {
      globalThis.fetch = originalFetch;
    }

    expect(calls).toEqual([
      {
        body: {
          destination: "11".repeat(16),
          idempotency_key: "22".repeat(16),
        },
        url: "http://127.0.0.1:61141/api/v1/reticulum/probes",
      },
      {
        body: { id: probeId },
        url: "http://127.0.0.1:61141/api/v1/reticulum/probes/poll",
      },
    ]);
  });
});
