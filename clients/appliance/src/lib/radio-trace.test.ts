import { describe, expect, test } from "bun:test";

import type {
  RadioTraceEventKindView,
  RadioTraceEventView,
  RadioTraceTxOutcomeView,
} from "../generated/api.ts";
import { filterRadioTrace, isRadioTraceAttention, radioTracePresentation } from "./radio-trace.ts";

const PACKET = {
  encoded_packet_len: 211,
  encoded_packet_sha256: "ab".repeat(32),
} as const;

function event(
  kind: RadioTraceEventKindView,
  overrides: Partial<RadioTraceEventView> = {},
): RadioTraceEventView {
  return {
    event_id: 7,
    boot_id: 42,
    event_sequence: 9,
    observed_at_us: 2_123_456,
    imported_at_unix_ms: 1_700_000_030_000,
    profile: {
      fingerprint: "12".repeat(16),
      frequency_hz: 915_000_000,
      bandwidth_hz: 125_000,
      preamble_symbols: 8,
      requested_power_dbm: 22,
      spreading_factor: 10,
      coding_rate_denominator: 5,
      explicit_header: true,
      crc: true,
      iq_inverted: false,
    },
    correlation: {
      timeline_sequence: 3,
      outbox_id: 2,
      attempt_number: 4,
      attempt_location: {
        state: "available",
        latitude_e6: 42_357_111,
        longitude_e6: -71_061_924,
        altitude_mm: 17_234,
        horizontal_accuracy_mm: 8_250,
        vertical_accuracy_mm: 3_125,
        captured_at_unix_ms: 1_700_000_000_000,
        authorization: "precise",
        source: "foreground_stream",
        mocked: false,
      },
    },
    event: kind,
    ...overrides,
  };
}

describe("RF trace presentation", () => {
  test("shows route, packet, token, profile, correlation and attempt location", () => {
    const presentation = radioTracePresentation(
      event({
        kind: "route_selected",
        submission_id: 88,
        destination: "34".repeat(16),
        next_hop_identity: "56".repeat(16),
        hops: 2,
        interface_id: 1,
        resolution: "exact_ready",
        packet_evidence: PACKET,
        rns_attempt_token: "78".repeat(32),
      }),
    );

    expect(presentation.title).toBe("Exact retained route ready");
    expect(presentation.metadata).toContain("Device submission 88");
    expect(presentation.metadata).toContain(`Packet SHA-256 ${PACKET.encoded_packet_sha256}`);
    expect(presentation.metadata).toContain(`RNS attempt token ${"78".repeat(32)}`);
    expect(presentation.metadata).toContain("Message row 3 · outbox 2 · attempt 4");
    expect(presentation.metadata).toContain("Phone location 42.357111, -71.061924 · ±8.3 m");
    expect(presentation.metadata).toContain(
      "915.000 MHz · BW 125 kHz · SF10 · CR 4/5 · requested +22 dBm",
    );
  });

  test("distinguishes definitive TxDone from every non-transmitted outcome", () => {
    const tx = (outcome: RadioTraceTxOutcomeView) =>
      event({
        kind: "data_tx",
        interface_id: 1,
        packet_evidence: PACKET,
        rns_attempt_token: "78".repeat(32),
        outcome,
        planned_physical_frames: 2,
        completed_physical_frames: outcome === "transmitted" ? 2 : 0,
        frame_0_completed_at_us: outcome === "transmitted" ? 2_000_000 : null,
        frame_1_completed_at_us: outcome === "transmitted" ? 2_100_000 : null,
        authorized_frame_observed: outcome === "transmitted",
      });

    const transmitted = radioTracePresentation(tx("transmitted"));
    expect(transmitted.title).toBe("LoRa DATA reached TxDone");
    expect(transmitted.tone).toBe("success");
    expect(transmitted.metadata).toContain("Frame 2 TxDone · 2.100 s since boot");

    const outcomes: RadioTraceTxOutcomeView[] = [
      "access_rejected",
      "permit_denied",
      "authorization_expired",
      "post_grant_access_rejected",
      "airtime_rejected",
      "deadline_conversion_overflow",
      "radio_inactive",
      "interface_configuration_mismatch",
      "radio_configuration_changed_before_permit",
      "radio_configuration_changed_after_permit",
      "cad_fault",
      "tx_fault",
      "control_plane_recovery",
      "frame_invariant_recovery",
      "cancelled_radio_operation",
    ];
    for (const outcome of outcomes) {
      const observed = tx(outcome);
      expect(radioTracePresentation(observed).tone).toBe("danger");
      expect(isRadioTraceAttention(observed)).toBeTrue();
    }
  });

  test("shows receiver-local RSSI/SNR and terminal proof return signal honestly", () => {
    const rx = event({
      kind: "logical_rx",
      interface_id: 1,
      packet_evidence: PACKET,
      rns_packet_hash: "90".repeat(32),
      rssi_dbm: -98,
      snr_db: 4,
    });
    expect(radioTracePresentation(rx).metadata).toContain("Interface 1 · RSSI -98 dBm · SNR 4 dB");

    const delivered = event({
      kind: "attempt_terminal",
      rns_attempt_token: "78".repeat(32),
      outcome: "delivered",
      proof_interface_id: 1,
      proof_rssi_dbm: -101,
      proof_snr_db: 2,
    });
    expect(radioTracePresentation(delivered).metadata).toContain(
      "Proof final hop · RSSI -101 dBm · SNR 2 dB",
    );
    expect(isRadioTraceAttention(delivered)).toBeFalse();

    const timeout = event({
      kind: "attempt_terminal",
      rns_attempt_token: "78".repeat(32),
      outcome: "delivery_timeout",
      proof_interface_id: null,
      proof_rssi_dbm: null,
      proof_snr_db: null,
    });
    expect(radioTracePresentation(timeout).title).toBe("Delivery proof timed out");
    expect(isRadioTraceAttention(timeout)).toBeTrue();
  });

  test("filters by kind, attention, correlation and searchable evidence", () => {
    const route = event({
      kind: "route_selected",
      submission_id: 88,
      destination: "34".repeat(16),
      next_hop_identity: null,
      hops: 1,
      interface_id: 1,
      resolution: "broadcast_unavailable",
      packet_evidence: PACKET,
      rns_attempt_token: "78".repeat(32),
    });
    const rx = event(
      {
        kind: "logical_rx",
        interface_id: 1,
        packet_evidence: PACKET,
        rns_packet_hash: null,
        rssi_dbm: -98,
        snr_db: 4,
      },
      { event_id: 8, correlation: null },
    );
    const events = [route, rx];

    expect(filterRadioTrace(events, "route", "")).toEqual([route]);
    expect(filterRadioTrace(events, "rx", "")).toEqual([rx]);
    expect(filterRadioTrace(events, "correlated", "")).toEqual([route]);
    expect(filterRadioTrace(events, "attention", "")).toEqual([route]);
    expect(filterRadioTrace(events, "all", "-98 dbm")).toEqual([rx]);
    expect(filterRadioTrace(events, "all", PACKET.encoded_packet_sha256)).toEqual([rx, route]);
  });
});
