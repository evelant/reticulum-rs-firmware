import { describe, expect, test } from "bun:test";

import type { HttpApplianceSnapshot, HttpConnectionState } from "../generated/api.ts";
import { applianceSnapshotFromHttp } from "./http-projection.ts";

function snapshot(connection: HttpConnectionState): HttpApplianceSnapshot {
  return {
    capabilities: {
      manual_service_announce: false,
      nearby_peers: false,
      network_config: false,
      nomad: false,
      radio_routes: false,
      radio_trace: false,
      reticulum_probe: false,
    },
    revision: 4,
    connection,
    device: null,
    pending_outbox: 2,
    contact_count: 3,
    imported_this_run: 1,
    last_error: null,
  };
}

describe("HTTP appliance projection", () => {
  test("retains Reticulum ready metadata", () => {
    expect(
      applianceSnapshotFromHttp(
        snapshot({
          state: "ready",
          transport: "reticulum",
          endpoint: "peripheral-a",
          device_label: "ACA704E13E88",
        }),
      ),
    ).toEqual({
      ...snapshot({
        state: "ready",
        transport: "reticulum",
        endpoint: "peripheral-a",
        device_label: "ACA704E13E88",
      }),
      connection: {
        state: "ready",
        transport: "reticulum",
        endpoint: "peripheral-a",
        device_label: "ACA704E13E88",
      },
    });
  });

  test("preserves every connection state without ready metadata", () => {
    for (const state of [
      "starting",
      "disconnected",
      "connecting",
      "backoff",
      "faulted",
      "stopped",
    ] as const) {
      expect(applianceSnapshotFromHttp(snapshot({ state })).connection).toEqual({ state });
    }
  });
});
