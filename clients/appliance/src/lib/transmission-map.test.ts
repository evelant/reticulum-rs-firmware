import { describe, expect, test } from "bun:test";

import type {
  MessageActivityEventView,
  PhoneLocationObservationView,
  RadioTraceEventKindView,
  RadioTraceEventView,
  TimelineView,
} from "../generated/api.ts";
import {
  buildTransmissionMapScene,
  selectedTransmissionMapFeatures,
  transmissionMapViewport,
} from "./transmission-map.ts";

const PACKET = {
  encoded_packet_len: 211,
  encoded_packet_sha256: "ab".repeat(32),
} as const;

function location(
  latitudeE6: number,
  longitudeE6: number,
  capturedAtUnixMs: number,
): PhoneLocationObservationView {
  return {
    state: "available",
    latitude_e6: latitudeE6,
    longitude_e6: longitudeE6,
    horizontal_accuracy_mm: 8_250,
    altitude_mm: 82_250,
    vertical_accuracy_mm: 3_500,
    captured_at_unix_ms: capturedAtUnixMs,
    authorization: "precise",
    source: "foreground_stream",
    mocked: false,
  };
}

function activity(
  eventId: number,
  attemptNumber: number,
  attemptLocation: PhoneLocationObservationView | null,
  kind: MessageActivityEventView["activity"],
  overrides: Partial<MessageActivityEventView> = {},
): MessageActivityEventView {
  return {
    event_id: eventId,
    observed_at_unix_ms: 1_700_000_030_000 + eventId,
    timeline_sequence: 3,
    peer: "34".repeat(16),
    direction: "outbound",
    outbox_id: 2,
    attempt_number: attemptNumber,
    attempt_location: attemptLocation,
    message_location: null,
    receiver_location: null,
    ingress_observation: null,
    activity: kind,
    ...overrides,
  };
}

function radio(
  eventId: number,
  attemptNumber: number,
  kind: RadioTraceEventKindView,
  attemptLocation: PhoneLocationObservationView,
  overrides: Partial<RadioTraceEventView> = {},
): RadioTraceEventView {
  return {
    event_id: eventId,
    boot_id: 42,
    event_sequence: eventId,
    observed_at_us: 2_123_456,
    imported_at_unix_ms: 1_700_000_040_000 + eventId,
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
      attempt_number: attemptNumber,
      attempt_location: attemptLocation,
    },
    event: kind,
    ...overrides,
  };
}

function timeline(
  sequence: number,
  latitudeE6: number,
  longitudeE6: number,
  accuracyCm = 825,
): TimelineView {
  return {
    sequence,
    direction: "inbound",
    timestamp_ms: 1_700_000_100_000,
    message_id: "56".repeat(32),
    outbox_id: null,
    submission_id: null,
    current_attempt_number: null,
    status: null,
    packet_evidence: null,
    ingress_observation: null,
    receiver_location: null,
    location: {
      latitude_e6: latitudeE6,
      longitude_e6: longitudeE6,
      altitude_cm: 1_700,
      speed_cm_per_second: 0,
      bearing_centidegrees: 0,
      accuracy_cm: accuracyCm,
      updated_at_unix_seconds: 1_700_000_000,
    },
    title: { encoding: "utf8", value: "Location" },
    content: { encoding: "utf8", value: "Here" },
  };
}

describe("transmission map scene", () => {
  test("aggregates one point per attempt and chooses the newest durable activity state", () => {
    const attemptLocation = location(42_357_111, -71_061_924, 1_700_000_000_000);
    const scene = buildTransmissionMapScene({
      contacts: [{ destination: "34".repeat(16), name: "Dad" }],
      messageActivityEvents: [
        activity(9, 1, null, {
          kind: "outbound_status",
          status: "delivered",
          packet_evidence: PACKET,
        }),
        activity(4, 1, null, {
          kind: "outbound_accepted",
          submission_id: 88,
          message_id: "56".repeat(32),
        }),
        activity(3, 1, attemptLocation, { kind: "outbound_queued" }),
      ],
      profileKey: "board-a",
    });

    expect(scene.summary.attemptCount).toBe(1);
    expect(scene.points.features[0]?.geometry.coordinates).toEqual([-71.061924, 42.357111]);
    expect(scene.points.features[0]?.properties).toMatchObject({
      kind: "attempt",
      label: "Dad · #1 · Delivered",
      timestamp_ms: 1_700_000_000_000,
      tone: "success",
    });
    const id = scene.points.features[0]?.properties.id ?? "";
    expect(scene.detailsByFeatureId[id]?.subtitle).toBe("Delivered");
    expect(scene.detailsByFeatureId[id]?.rows).toContainEqual({
      label: "Meaning",
      value:
        "Phone position when the app submission was queued; board retries reuse it; not exact RF emission or board GNSS.",
    });
  });

  test("uses the exact lifecycle status in marker text instead of inferring it from color", () => {
    const attemptLocation = location(42_357_111, -71_061_924, 1_700_000_000_000);
    const scene = buildTransmissionMapScene({
      messageActivityEvents: [
        activity(6, 1, null, {
          kind: "outbound_status",
          status: "cancelled",
          packet_evidence: null,
        }),
        activity(3, 1, attemptLocation, { kind: "outbound_queued" }),
      ],
    });

    expect(scene.points.features[0]?.properties.label).toEndWith(" · Cancelled");
    expect(scene.points.features[0]?.properties.tone).toBe("warning");
  });

  test("joins exact correlated RF evidence and labels return-proof signal honestly", () => {
    const attemptLocation = location(42_357_111, -71_061_924, 1_700_000_000_000);
    const scene = buildTransmissionMapScene({
      messageActivityEvents: [activity(4, 1, attemptLocation, { kind: "outbound_queued" })],
      radioTraceEvents: [
        radio(
          12,
          1,
          {
            kind: "attempt_terminal",
            rns_attempt_token: "78".repeat(32),
            outcome: "delivered",
            proof_interface_id: 1,
            proof_rssi_dbm: -101,
            proof_snr_db: 2,
          },
          attemptLocation,
        ),
      ],
    });

    const id = scene.points.features[0]?.properties.id ?? "";
    expect(scene.points.features[0]?.properties.tone).toBe("success");
    expect(scene.detailsByFeatureId[id]?.rows).toContainEqual({
      label: "Proof return signal",
      value: "RSSI -101 dBm · SNR 2 dB · final return hop only",
    });
  });

  test("does not invent a map point for uncorrelated receiver-local radio evidence", () => {
    const event = radio(
      8,
      1,
      {
        kind: "logical_rx",
        interface_id: 1,
        packet_evidence: PACKET,
        rns_packet_hash: "90".repeat(32),
        rssi_dbm: -98,
        snr_db: 4,
      },
      location(42_357_111, -71_061_924, 1_700_000_000_000),
      { correlation: null },
    );
    const scene = buildTransmissionMapScene({ radioTraceEvents: [event] });
    expect(scene.points.features).toEqual([]);
    expect(scene.lines.features).toEqual([]);
  });

  test("draws clickable chronological separation segments without calling them RF paths", () => {
    const first = location(42_350_000, -71_060_000, 1_700_000_000_000);
    const second = location(42_360_000, -71_050_000, 1_700_000_060_000);
    const scene = buildTransmissionMapScene({
      messageActivityEvents: [
        activity(8, 2, second, { kind: "outbound_requeued" }),
        activity(4, 1, first, { kind: "outbound_queued" }),
      ],
    });

    expect(scene.summary).toEqual({
      attemptCount: 2,
      messageLocationCount: 0,
      messageReceptionLinkCount: 0,
      observationSegmentCount: 1,
      receiverLocationCount: 0,
    });
    const segment = scene.lines.features[0];
    expect(segment?.geometry.coordinates).toEqual([
      [-71.06, 42.35],
      [-71.05, 42.36],
    ]);
    const details = scene.detailsByFeatureId[segment?.properties.id ?? ""];
    expect(details?.subtitle).toBe("Chronological field-test observation sequence");
    expect(details?.rows.at(-1)?.value).toContain("not an RF path");
  });

  test("keeps sender-attached message locations distinct from private attempt locations", () => {
    const attemptLocation = location(0, 0, 1_700_000_000_000);
    const scene = buildTransmissionMapScene({
      messageActivityEvents: [activity(4, 1, attemptLocation, { kind: "outbound_queued" })],
      locatedTimelines: [
        { peer: "34".repeat(16), peerName: "Dad", timeline: timeline(3, 0, 0, 0) },
      ],
      activityHistoryIncomplete: true,
    });

    expect(scene.points.features.map((feature) => feature.properties.kind).sort()).toEqual([
      "attempt",
      "message-location",
    ]);
    expect(scene.historyIncomplete).toBeTrue();
    expect(scene.bounds).toEqual({ east: 0, north: 0, south: 0, west: 0 });
    const sharedId = scene.points.features.find(
      (feature) => feature.properties.kind === "message-location",
    )?.properties.id;
    expect(scene.detailsByFeatureId[sharedId ?? ""]?.rows).toContainEqual({
      label: "Reported accuracy",
      value: "Unavailable",
    });
  });

  test("shows exact receiver-local final-hop signal on an inbound message location", () => {
    const received = timeline(11, 42_357_111, -71_061_924);
    received.ingress_observation = {
      interface_id: 1,
      signal: { rssi_dbm: -108, snr_db: -5 },
    };
    const scene = buildTransmissionMapScene({
      locatedTimelines: [{ peer: "34".repeat(16), peerName: "Dad", timeline: received }],
    });

    const point = scene.points.features[0];
    expect(point?.properties.label).toBe("Dad · RX -108 dBm · SNR -5 dB");
    const rows = scene.detailsByFeatureId[point?.properties.id ?? ""]?.rows;
    expect(rows).toContainEqual({ label: "Received via", value: "Interface 1" });
    expect(rows).toContainEqual({
      label: "Receiver-local signal",
      value: "RSSI -108 dBm · SNR -5 dB",
    });
    expect(rows).toContainEqual({
      label: "Signal meaning",
      value: expect.stringContaining("relay rather than the original sender"),
    });
  });

  test("draws a distinct received-message endpoint link with distance and both phone elevations", () => {
    const senderLocation = timeline(14, 42_357_111, -71_061_924).location;
    expect(senderLocation).not.toBeNull();
    const receiverLocation = location(42_367_111, -71_061_924, 1_700_000_102_000);
    const received = activity(
      18,
      1,
      null,
      { kind: "inbound_imported", message_id: "56".repeat(32) },
      {
        direction: "inbound",
        outbox_id: null,
        attempt_number: null,
        message_location: senderLocation,
        receiver_location: receiverLocation,
        ingress_observation: {
          interface_id: 1,
          signal: { rssi_dbm: -112, snr_db: -7 },
        },
      },
    );
    const scene = buildTransmissionMapScene({
      contacts: [{ destination: "34".repeat(16), name: "Dad" }],
      messageActivityEvents: [received],
      profileKey: "board-a",
    });

    expect(scene.summary).toEqual({
      attemptCount: 0,
      messageLocationCount: 1,
      messageReceptionLinkCount: 1,
      observationSegmentCount: 0,
      receiverLocationCount: 1,
    });
    expect(scene.points.features.map((feature) => feature.properties.kind)).toEqual([
      "message-location",
      "receiver-location",
    ]);
    const link = scene.lines.features[0];
    expect(link?.properties).toMatchObject({
      kind: "message-reception-link",
      label: "1.11 km · 0.69 mi · 17 m → 82 m",
      tone: "success",
    });
    expect(link?.geometry.coordinates).toEqual([
      [-71.061924, 42.357111],
      [-71.061924, 42.367111],
    ]);
    expect(scene.bounds).toEqual({
      east: -71.061924,
      north: 42.367111,
      south: 42.357111,
      west: -71.061924,
    });

    const details = scene.detailsByFeatureId[link?.properties.id ?? ""];
    expect(details?.rows).toContainEqual({
      label: "Horizontal endpoint separation",
      value: "1.11 km · 0.69 mi",
    });
    expect(details?.rows).toContainEqual({
      label: "Sender phone elevation",
      value: "17.00 m",
    });
    expect(details?.rows).toContainEqual({
      label: "Receiver phone elevation",
      value: "82.25 m ±3.5 m",
    });
    expect(details?.rows).toContainEqual({
      label: "Elevation difference",
      value: "65.25 m (receiver − sender)",
    });
    expect(details?.rows).toContainEqual({
      label: "Receiver-local final-hop signal",
      value: "RSSI -112 dBm · SNR -7 dB",
    });
    expect(details?.rows.at(-1)?.value).toContain("not the board position");
    expect(details?.rows.at(-1)?.value).toContain("relayed Reticulum route");
    const selection = selectedTransmissionMapFeatures(scene, link?.properties.id ?? null);
    expect(selection.features).toHaveLength(1);
    expect(selection.features[0]?.properties.id).toBe(link?.properties.id);
  });

  test("keeps the sender location but does not invent a reception link without a receiver fix", () => {
    const senderLocation = timeline(15, 42_357_111, -71_061_924).location;
    const scene = buildTransmissionMapScene({
      messageActivityEvents: [
        activity(
          19,
          1,
          null,
          { kind: "inbound_imported", message_id: "57".repeat(32) },
          {
            direction: "inbound",
            outbox_id: null,
            attempt_number: null,
            message_location: senderLocation,
            receiver_location: { state: "unavailable", reason: "telemetry_disabled" },
          },
        ),
      ],
    });

    expect(scene.summary.messageLocationCount).toBe(1);
    expect(scene.summary.receiverLocationCount).toBe(0);
    expect(scene.summary.messageReceptionLinkCount).toBe(0);
    expect(scene.lines.features).toEqual([]);
  });

  test("omits invalid coordinates and provides a deterministic selection and viewport", () => {
    const scene = buildTransmissionMapScene({
      messageActivityEvents: [
        activity(4, 1, location(91_000_000, 0, 1_700_000_000_000), {
          kind: "outbound_queued",
        }),
      ],
      locatedTimelines: [
        { peer: "34".repeat(16), peerName: null, timeline: timeline(9, 42_000_000, -71_000_000) },
      ],
    });
    const id = scene.points.features[0]?.properties.id ?? null;
    expect(scene.summary.attemptCount).toBe(0);
    expect(selectedTransmissionMapFeatures(scene, id).features).toHaveLength(1);
    expect(transmissionMapViewport(scene)).toEqual({ center: [-71, 42], zoom: 16 });
  });
});
