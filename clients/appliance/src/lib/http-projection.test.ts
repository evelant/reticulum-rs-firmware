import { describe, expect, test } from "bun:test";

import type { HttpApplianceSnapshot, HttpConnectionState } from "../generated/api.ts";
import { applianceSnapshotFromHttp } from "./http-projection.ts";

function snapshot(connection: HttpConnectionState): HttpApplianceSnapshot {
  return {
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
  test("retains bearer-neutral ready metadata", () => {
    expect(
      applianceSnapshotFromHttp(
        snapshot({
          state: "ready",
          transport: "bluetooth_low_energy",
          endpoint: "peripheral-a",
          device_label: "ACA704E13E88",
        }),
      ),
    ).toEqual({
      ...snapshot({
        state: "ready",
        transport: "bluetooth_low_energy",
        endpoint: "peripheral-a",
        device_label: "ACA704E13E88",
      }),
      connection: {
        state: "ready",
        transport: "bluetooth_low_energy",
        endpoint: "peripheral-a",
        device_label: "ACA704E13E88",
      },
    });
  });

  test("preserves every connection state without bearer metadata", () => {
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
