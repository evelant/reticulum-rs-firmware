import { describe, expect, test } from "bun:test";

import type { ApplianceSnapshot, ConnectionState } from "../generated/api.ts";
import {
  applianceStatusPresentation,
  connectionStateLabel,
  connectionTransportLabel,
  formatDeviceId,
} from "./appliance-status.ts";

function snapshot(
  connection: ConnectionState,
  overrides: Partial<ApplianceSnapshot> = {},
): ApplianceSnapshot {
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
    connection,
    contact_count: 2,
    device: {
      device_id: "653239302d6170692d31aca704e13f88",
      lxmf_delivery_destination: "93".repeat(16),
      primary_destination: "83".repeat(16),
    },
    imported_this_run: 3,
    last_error: null,
    pending_outbox: 2,
    revision: 9,
    ...overrides,
  };
}

describe("appliance status presentation", () => {
  test("presents an authenticated Reticulum appliance with stable identity and honest activity", () => {
    expect(
      applianceStatusPresentation(
        snapshot({
          device_label: "aca704e13f88",
          endpoint: "3D957380-0E99-4BB1-87D1-B18CF2EBFB9C",
          state: "ready",
          transport: "reticulum",
        }),
      ),
    ).toEqual({
      boardLabel: "AC:A7:04:E1:3F:88",
      connectionLabel: "Connected through Reticulum",
      contactCountLabel: "2 contacts",
      deviceId: "653239302d6170692d31aca704e13f88",
      endpoint: "3D957380-0E99-4BB1-87D1-B18CF2EBFB9C",
      importedThisRunLabel: "3 imported since app start",
      lxmfDestination: "93".repeat(16),
      pendingOutboxLabel: "2 outbound pending",
      primaryDestination: "83".repeat(16),
      tone: "ready",
    });
  });

  test("keeps counts explicit without implying queued-only or live-session RF totals", () => {
    const presentation = applianceStatusPresentation(
      snapshot(
        {
          device_label: "aca704e13f88",
          endpoint: "peripheral",
          state: "ready",
          transport: "reticulum",
        },
        { imported_this_run: 1, pending_outbox: 0 },
      ),
    );

    expect(presentation.pendingOutboxLabel).toBe("0 outbound pending");
    expect(presentation.importedThisRunLabel).toBe("1 imported since app start");
  });

  test("retains authenticated device identity while honestly reporting a disconnect", () => {
    const presentation = applianceStatusPresentation(snapshot({ state: "disconnected" }));

    expect(presentation.boardLabel).toBe("AC:A7:04:E1:3F:88");
    expect(presentation.connectionLabel).toBe("Disconnected");
    expect(presentation.endpoint).toBeNull();
    expect(presentation.tone).toBe("neutral");
  });

  test("presents unavailable Reticulum without inventing an endpoint", () => {
    const presentation = applianceStatusPresentation(
      snapshot({ state: "unavailable", transport: "reticulum" }),
    );

    expect(presentation.connectionLabel).toBe("Reticulum unavailable");
    expect(presentation.endpoint).toBeNull();
  });

  test("handles the pre-snapshot starting state without fake device data", () => {
    expect(applianceStatusPresentation(null)).toEqual({
      boardLabel: "Appliance",
      connectionLabel: "Starting",
      contactCountLabel: "0 contacts",
      deviceId: null,
      endpoint: null,
      importedThisRunLabel: "0 imported since app start",
      lxmfDestination: null,
      pendingOutboxLabel: "0 outbound pending",
      primaryDestination: null,
      tone: "neutral",
    });
  });

  test("formats EUI-48 and E290 device IDs but preserves other opaque identifiers", () => {
    expect(formatDeviceId("AC-A7-04-E1-3F-88")).toBe("AC:A7:04:E1:3F:88");
    expect(formatDeviceId("653239302d6170692d31aca704e13f88")).toBe("AC:A7:04:E1:3F:88");
    expect(formatDeviceId("00112233445566778899aabbccddeeff")).toBe(
      "00112233445566778899aabbccddeeff",
    );
    expect(formatDeviceId("field-node-alpha")).toBe("field-node-alpha");
  });

  test("covers the Reticulum connection and every lifecycle label", () => {
    expect(connectionTransportLabel("reticulum")).toBe("Reticulum");
    expect(connectionStateLabel({ state: "connecting" })).toBe("Connecting");
    expect(connectionStateLabel({ state: "backoff" })).toBe("Waiting to reconnect");
    expect(connectionStateLabel({ state: "faulted" })).toBe("Connection fault");
    expect(connectionStateLabel({ state: "stopped" })).toBe("Stopped");
  });
});
