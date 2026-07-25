import { describe, expect, mock, test } from "bun:test";

mock.module("expo-linking", () => ({
  getInitialURL: async () => null,
}));
mock.module("react-native", () => ({
  Platform: { OS: "ios" },
}));

describe("HTTP appliance Nomad bridge", () => {
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
});
