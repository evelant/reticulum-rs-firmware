import { describe, expect, test } from "bun:test";

import type { MessageActivityEventView } from "../generated/api.ts";
import {
  buildMessageActivityAliases,
  filterMessageActivity,
  isMessageActivityAttention,
  messageActivityLocationMetadata,
  messageActivityObservedAtLabel,
  messageActivityPeerLabel,
  messageActivityPresentation,
  messageActivityStatusLabel,
  sortMessageActivityNewestFirst,
} from "./message-activity.ts";

function activityEvent(
  overrides: Partial<MessageActivityEventView> = {},
): MessageActivityEventView {
  return {
    event_id: 8,
    observed_at_unix_ms: 1_700_000_000_000,
    timeline_sequence: 4,
    peer: "00112233445566778899aabbccddeeff",
    direction: "outbound",
    outbox_id: 3,
    attempt_number: 2,
    attempt_location: null,
    ingress_observation: null,
    message_location: null,
    receiver_location: null,
    activity: {
      kind: "outbound_status",
      status: "awaiting_delivery",
      packet_evidence: {
        encoded_packet_len: 183,
        encoded_packet_sha256: "ab".repeat(32),
      },
    },
    ...overrides,
  };
}

describe("message activity presentation", () => {
  test("uses explicit contact aliases and keeps unknown inbound peers visibly unknown", () => {
    const peer = "00112233445566778899aabbccddeeff";
    const aliases = buildMessageActivityAliases(
      [{ destination: peer, name: "  Field unit  " }],
      [{ destination: peer, name: "Stale history label" }],
    );

    expect(messageActivityPeerLabel(activityEvent(), aliases)).toBe("Field unit");
    expect(
      messageActivityPeerLabel(
        activityEvent({
          direction: "inbound",
          peer: "11".repeat(16),
          outbox_id: null,
          attempt_number: null,
          activity: { kind: "inbound_imported", message_id: "22".repeat(32) },
        }),
        aliases,
      ),
    ).toBe("Unknown sender …111111");
  });

  test("describes app submission and immutable packet evidence without claiming an RF timestamp", () => {
    const presentation = messageActivityPresentation(activityEvent(), new Map());

    expect(presentation.title).toBe("Awaiting delivery proof");
    expect(presentation.observedAt).toStartWith("Observed by app · ");
    expect(presentation.metadata).toContain("App submission 2");
    expect(presentation.metadata).toContain("Encoded packet 183 bytes");
    expect(presentation.metadata).toContain(`Packet SHA-256 ${"ab".repeat(32)}`);
    expect(messageActivityObservedAtLabel(null)).toBe("Observed by app · time unavailable");
  });

  test("shows receiver-local final-hop RSSI and SNR for inbound activity", () => {
    const event = activityEvent({
      direction: "inbound",
      outbox_id: null,
      attempt_number: null,
      ingress_observation: {
        interface_id: 1,
        signal: { rssi_dbm: -103, snr_db: -2 },
      },
      activity: { kind: "inbound_imported", message_id: "fe".repeat(32) },
    });

    expect(messageActivityPresentation(event, new Map()).metadata).toContain(
      "Receiver-local final hop · interface 1 · RSSI -103 dBm · SNR -2 dB",
    );
  });

  test("shows the app-submission location reused by board retries", () => {
    const event = activityEvent({
      observed_at_unix_ms: 1_700_000_030_000,
      activity: { kind: "outbound_requeued" },
      attempt_location: {
        state: "available",
        latitude_e6: 42_357_111,
        longitude_e6: -71_061_924,
        altitude_mm: null,
        horizontal_accuracy_mm: 8_250,
        vertical_accuracy_mm: null,
        captured_at_unix_ms: 1_700_000_000_000,
        authorization: "precise",
        source: "foreground_stream",
        mocked: false,
      },
    });

    expect(messageActivityLocationMetadata(event)).toEqual([
      "Phone location 42.357111, -71.061924 · ±8.3 m",
      expect.stringContaining("30s old · precise grant · foreground stream"),
      "Phone position when app submission queued; board retries reuse it",
    ]);
    expect(messageActivityPresentation(event, new Map()).metadata).toContain(
      "Phone position when app submission queued; board retries reuse it",
    );
    expect(messageActivityPresentation(event, new Map()).title).toBe(
      "Replacement submission queued",
    );
  });

  test("labels every generated lifecycle status with product semantics", () => {
    expect(messageActivityStatusLabel("committed")).toBe("Saved locally");
    expect(messageActivityStatusLabel("preparing")).toBe("Pending / retrying on appliance");
    expect(messageActivityStatusLabel("awaiting_delivery")).toBe("Awaiting delivery proof");
    expect(messageActivityStatusLabel("failed_no_path")).toBe("Failed: no path");
    expect(messageActivityStatusLabel("delivered")).toBe("Delivered");
  });

  test("sorts by durable event order and filters direction, attention, aliases, and evidence", () => {
    const failed = activityEvent({
      event_id: 11,
      activity: {
        kind: "outbound_status",
        status: "failed_no_path",
        packet_evidence: null,
      },
    });
    const inbound = activityEvent({
      event_id: 12,
      direction: "inbound",
      peer: "99".repeat(16),
      outbox_id: null,
      attempt_number: null,
      activity: { kind: "inbound_imported", message_id: "cd".repeat(32) },
    });
    const retried = activityEvent({
      event_id: 13,
      attempt_number: 3,
      activity: { kind: "outbound_requeued" },
    });
    const aliases = new Map([[inbound.peer, "Hill relay"]]);
    const events = [failed, inbound, retried];

    expect(sortMessageActivityNewestFirst(events).map((event) => event.event_id)).toEqual([
      13, 12, 11,
    ]);
    expect(filterMessageActivity(events, "inbound", "", aliases)).toEqual([inbound]);
    expect(filterMessageActivity(events, "attention", "", aliases)).toEqual([retried, failed]);
    expect(filterMessageActivity(events, "all", "hill", aliases)).toEqual([inbound]);
    expect(filterMessageActivity(events, "all", "failed: no path", aliases)).toEqual([failed]);
    expect(isMessageActivityAttention(retried)).toBeTrue();
    expect(isMessageActivityAttention(inbound)).toBeFalse();
  });
});
