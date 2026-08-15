import { describe, expect, test } from "bun:test";

import {
  collectCompleteRadioTrace,
  createRadioTraceExportDocument,
  RADIO_TRACE_EXPORT_PAGE_SIZE,
  type RadioTraceExportPageRequest,
  radioTraceCsvArtifact,
  radioTraceJsonArtifact,
} from "./radio-trace-export.ts";

const SOURCE = {
  board_label: "E290 13F88",
  device_id: "00".repeat(16),
  lxmf_delivery_destination: "11".repeat(16),
  primary_destination: "22".repeat(16),
  profile_key: "00".repeat(16),
} as const;

function traceEvent(id: number, kind = "data_tx") {
  return {
    event_id: id,
    correlation: {
      attempt_location: {
        state: "available",
        latitude_e6: 42_357_111,
        longitude_e6: -71_061_924,
        altitude_mm: 17_234,
        vertical_accuracy_mm: 3_125,
      },
      attempt_number: 2,
      timeline_sequence: 9,
    },
    event: {
      kind,
      packet_evidence: {
        encoded_packet_sha256: `hash,"${id}\nline`,
        encoded_packet_len: 211,
      },
    },
  };
}

describe("RF trace export", () => {
  test("reads every cursor page and returns chronological events", async () => {
    const requests: RadioTraceExportPageRequest[] = [];
    const collection = await collectCompleteRadioTrace(async (request) => {
      requests.push(request);
      if (request.before_event_id === null) {
        return {
          events: [traceEvent(4), traceEvent(3)],
          history_incomplete: false,
          next_before_event_id: 3,
        };
      }
      return {
        events: [traceEvent(2), traceEvent(1)],
        history_incomplete: true,
        next_before_event_id: null,
      };
    }, 9);

    expect(requests).toEqual([
      { before_event_id: null, limit: RADIO_TRACE_EXPORT_PAGE_SIZE, timeline_sequence: 9 },
      { before_event_id: 3, limit: RADIO_TRACE_EXPORT_PAGE_SIZE, timeline_sequence: 9 },
    ]);
    expect(collection.events.map((event) => event.event_id)).toEqual([1, 2, 3, 4]);
    expect(collection.historyIncomplete).toBeTrue();
  });

  test("rejects repeated and invalid cursors instead of exporting partial evidence", async () => {
    await expect(
      collectCompleteRadioTrace(
        async () => ({ events: [], history_incomplete: false, next_before_event_id: 7 }),
        null,
      ),
    ).rejects.toThrow("repeated a cursor");
    await expect(
      collectCompleteRadioTrace(
        async () => ({ events: [], history_incomplete: false, next_before_event_id: 0 }),
        null,
      ),
    ).rejects.toThrow("invalid pagination cursor");
  });

  test("preserves generated event structure and builds a scoped safe filename", () => {
    const document = createRadioTraceExportDocument({
      collection: { events: [traceEvent(1)], historyIncomplete: false },
      exportedAtUnixMs: Date.UTC(2026, 7, 1, 15, 4, 5),
      source: SOURCE,
      timelineSequence: 9,
    });
    const artifact = radioTraceJsonArtifact(document);
    const decoded = JSON.parse(artifact.contents);

    expect(artifact.filename).toBe("reticulum-rf-trace-e290-13f88-message-9-20260801T150405Z.json");
    expect(decoded.schema).toBe("org.reticulum.appliance.rf-trace");
    expect(decoded.events).toEqual([traceEvent(1)]);
    expect(decoded.scope.timeline_sequence).toBe(9);
  });

  test("flattens union-specific fields and applies RFC4180 escaping", () => {
    const inboundProof = {
      ...traceEvent(3, "inbound_proof"),
      correlation: null,
      event: {
        correlation_token: "45".repeat(32),
        dispatch_outcome: "tx_fault",
        interface_id: 1,
        kind: "inbound_proof",
        message_id: "67".repeat(32),
        packet_evidence: null,
        rssi_dbm: null,
        snr_db: null,
        stage: "physical_tx_failed",
      },
    };
    const document = createRadioTraceExportDocument({
      collection: {
        events: [traceEvent(1, "data_tx"), traceEvent(2, "logical_rx"), inboundProof],
        historyIncomplete: true,
      },
      exportedAtUnixMs: Date.UTC(2026, 7, 1),
      source: SOURCE,
      timelineSequence: null,
    });
    const artifact = radioTraceCsvArtifact(document);
    const [header, first, second, third] = artifact.contents.trim().split("\r\n");

    expect(artifact.filename).toEndWith("-all-20260801T000000Z.csv");
    expect(header).toContain("event.correlation.attempt_location.latitude_e6");
    expect(header).toContain("event.event.packet_evidence.encoded_packet_sha256");
    expect(header).toContain("event.event.correlation_token");
    expect(header).toContain("event.event.stage");
    expect(first).toContain('"hash,""1\nline"');
    expect(second).toContain("logical_rx");
    expect(third).toContain("physical_tx_failed");
    expect(third).toContain("tx_fault");
  });

  test("rejects unsafe export timestamps", () => {
    expect(() =>
      createRadioTraceExportDocument({
        collection: { events: [], historyIncomplete: false },
        exportedAtUnixMs: Number.MAX_SAFE_INTEGER + 1,
        source: SOURCE,
        timelineSequence: null,
      }),
    ).toThrow("non-negative safe integer");
  });
});
