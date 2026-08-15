import { describe, expect, test } from "bun:test";

import {
  isPublicReticulumEndpointSelected,
  PUBLIC_RETICULUM_TCP_ENDPOINTS,
  publicReticulumEndpoint,
} from "./public-reticulum-endpoints.ts";

describe("public Reticulum TCP endpoint catalog", () => {
  test("contains the deliberately curated, recently verified bootstrap set", () => {
    expect(PUBLIC_RETICULUM_TCP_ENDPOINTS).toEqual([
      {
        id: "rmap-world",
        label: "RMAP World",
        hostname: "rmap.world",
        port: 4242,
        expectedTransportId: "682e34edf6dd0daa867831ebc9b4e204",
        sourceUrl: "https://rmap.world/info.html",
        verifiedOn: "2026-07-26",
      },
      {
        id: "reticulumnet-nl",
        label: "ReticulumNet.nl",
        hostname: "node.reticulumnet.nl",
        port: 4242,
        expectedTransportId: "8a2c0d3c3fee8bea4a8172dc6f4d7ea6",
        sourceUrl: "https://www.reticulumnet.nl/en/get-started/",
        verifiedOn: "2026-07-26",
      },
      {
        id: "mcswain-dev",
        label: "McSwain Reticulum",
        hostname: "reticulum.mcswain.dev",
        port: 4242,
        expectedTransportId: "72d389bca0703e185155f2d2c3eace57",
        sourceUrl: "https://rmap.world/?json=1",
        verifiedOn: "2026-07-26",
      },
    ]);
  });

  test("keeps identifiers, hosts, and diagnostic transport identities well formed", () => {
    const ids = new Set<string>();
    const authorities = new Set<string>();

    for (const endpoint of PUBLIC_RETICULUM_TCP_ENDPOINTS) {
      expect(ids.has(endpoint.id)).toBeFalse();
      expect(authorities.has(`${endpoint.hostname}:${endpoint.port}`)).toBeFalse();
      expect(endpoint.hostname).toMatch(/^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$/);
      expect(endpoint.port).toBeGreaterThan(0);
      expect(endpoint.port).toBeLessThanOrEqual(65_535);
      expect(endpoint.expectedTransportId).toMatch(/^[0-9a-f]{32}$/);
      expect(endpoint.sourceUrl).toStartWith("https://");
      expect(Number.isNaN(Date.parse(`${endpoint.verifiedOn}T00:00:00Z`))).toBeFalse();
      ids.add(endpoint.id);
      authorities.add(`${endpoint.hostname}:${endpoint.port}`);
    }
  });

  test("looks up a preset without duplicating its connection fields", () => {
    expect(publicReticulumEndpoint("rmap-world")).toBe(PUBLIC_RETICULUM_TCP_ENDPOINTS[0]);
  });

  test("selects only an enabled exact hostname peer", () => {
    const endpoint = PUBLIC_RETICULUM_TCP_ENDPOINTS[0];
    expect(
      isPublicReticulumEndpointSelected(
        { enabled: true, hostname: "RMAP.WORLD", port: 4242 },
        endpoint,
      ),
    ).toBeTrue();
    expect(
      isPublicReticulumEndpointSelected(
        { enabled: false, hostname: "rmap.world", port: 4242 },
        endpoint,
      ),
    ).toBeFalse();
    expect(
      isPublicReticulumEndpointSelected(
        { enabled: true, ipv4_address: "192.0.2.1", port: 4242 },
        endpoint,
      ),
    ).toBeFalse();
  });
});
