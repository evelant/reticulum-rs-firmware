import { describe, expect, test } from "bun:test";

import type { NetworkRuntimeStatusView, RmapRuntimeStatusView } from "../generated/api.ts";
import { rmapRuntimePresentation } from "./rmap-runtime-diagnostics.ts";

function status(overrides: Partial<RmapRuntimeStatusView> = {}): NetworkRuntimeStatusView {
  return {
    active_wifi_profile: null,
    applied_revision: 7,
    configured_revision: 7,
    connected_ssid: null,
    dns_diagnostics: null,
    ipv4_address: "192.0.2.10",
    last_tcp_failure: null,
    rmap_status: {
      config_applied: true,
      deferred_reason: null,
      egress_confirmation: "not_applicable",
      initial_tcp_gate: "open",
      last_queue_attempt_at_uptime_seconds: null,
      last_queue_outcome: "not_attempted",
      next_due_in_seconds: null,
      queued_count: 0,
      stamp_attempts: 120,
      stamp_phase: "ready",
      ...overrides,
    },
    rssi_dbm: -70,
    tcp_peer_state: "connected",
    wifi_state: "connected",
  };
}

describe("RMAP runtime diagnostics", () => {
  test("does not confuse desired configuration with applied firmware state", () => {
    expect(rmapRuntimePresentation(status({ config_applied: false }), true)).toMatchObject({
      headline: "Restart required",
      tone: "warning",
    });
  });

  test("makes the exact public TCP gate visible", () => {
    const presentation = rmapRuntimePresentation(
      status({ deferred_reason: "initial_tcp_not_ready", initial_tcp_gate: "waiting" }),
      true,
    );
    expect(presentation.headline).toBe("Waiting for public TCP");
    expect(presentation.rows).toContain("Public TCP interface is not ready");
  });

  test("reports authoritative coordinator acceptance and cadence", () => {
    const presentation = rmapRuntimePresentation(
      status({
        egress_confirmation: "not_observed",
        last_queue_attempt_at_uptime_seconds: 91,
        last_queue_outcome: "accepted",
        next_due_in_seconds: 21_600,
        queued_count: 1,
      }),
      true,
    );
    expect(presentation).toEqual({
      headline: "Publication accepted",
      rows: [
        "Stamp Ready (120 attempts)",
        "Public TCP ready",
        "Accepted 1 publication at 1m uptime",
        "Accepted for transmission; radio or link completion is not tracked",
        "Next publication due in 6h 0m",
      ],
      tone: "success",
    });
  });

  test("retains a typed admission failure instead of implying publication", () => {
    const presentation = rmapRuntimePresentation(
      status({
        deferred_reason: "ordinary_queue_rejected",
        last_queue_outcome: "ordinary_admission_deferred",
      }),
      true,
    );
    expect(presentation.headline).toBe("Publication deferred");
    expect(presentation.rows).toContain("Transmit coordinator is busy");
  });

  test("degrades explicitly when connected firmware has no status projection", () => {
    expect(rmapRuntimePresentation({ ...status(), rmap_status: null }, true)).toEqual({
      headline: "Status unavailable",
      rows: ["The connected firmware did not report RMAP publication state."],
      tone: "neutral",
    });
  });
});
