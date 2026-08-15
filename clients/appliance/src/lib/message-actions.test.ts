import { describe, expect, test } from "bun:test";

import type { TimelineView } from "../generated/api.ts";
import {
  retryMessageCacheKey,
  retryMessageRequest,
  timelineActivityRevision,
  timelineEntryKey,
  timelineMessageCapabilities,
  timelineStatusLabel,
} from "./message-actions.ts";

function message(overrides: Partial<TimelineView> = {}): TimelineView {
  return {
    sequence: 7,
    direction: "outbound",
    timestamp_ms: 1_700_000_000_000,
    message_id: "ab".repeat(16),
    outbox_id: 3,
    submission_id: 4,
    current_attempt_number: 1,
    automatic_retry_count: 0,
    packet_evidence: null,
    ingress_observation: null,
    receiver_location: null,
    location: null,
    status: "failed_delivery_timeout",
    title: { encoding: "utf8", value: "Hello" },
    content: { encoding: "utf8", value: "World" },
    ...overrides,
  };
}

describe("timeline message actions", () => {
  test("offers exact retry for retryable terminal outbound messages", () => {
    expect(timelineMessageCapabilities(message())).toEqual({
      canRetry: true,
      canUseAsDraft: true,
    });
  });

  test("does not misrepresent active, inbound, cancelled, or binary rows as retryable", () => {
    expect(
      timelineMessageCapabilities(message({ status: "awaiting_delivery" })).canRetry,
    ).toBeFalse();
    expect(
      timelineMessageCapabilities(message({ direction: "inbound", status: null })).canRetry,
    ).toBeFalse();
    expect(timelineMessageCapabilities(message({ status: "cancelled" })).canRetry).toBeFalse();
    expect(
      timelineMessageCapabilities(message({ status: "failed_downstream_rejection" })).canRetry,
    ).toBeFalse();
    expect(
      timelineMessageCapabilities(message({ content: { encoding: "hex", value: "ff00" } })),
    ).toEqual({
      canRetry: true,
      canUseAsDraft: false,
    });
  });

  test("formats generated status values and inbound rows for the details sheet", () => {
    expect(timelineStatusLabel(message())).toBe("Failed Delivery Timeout");
    expect(timelineStatusLabel(message({ status: "preparing" }))).toBe(
      "Pending / retrying on appliance",
    );
    expect(timelineStatusLabel(message({ direction: "inbound", status: null }))).toBe("Received");
  });

  test("retains row and retry-cache identity across lifecycle refreshes", () => {
    const failed = message();
    const delivered = message({ status: "delivered" });

    expect(timelineEntryKey(failed)).toBe(timelineEntryKey(delivered));
    expect(retryMessageCacheKey("cd".repeat(16), failed)).toBe(
      retryMessageCacheKey("cd".repeat(16), delivered),
    );
  });

  test("keeps activity pagination across equivalent poll objects and refreshes on transitions", () => {
    const failed = message();
    const equivalent = message();
    const retried = message({
      status: "preparing",
      submission_id: 5,
      current_attempt_number: 2,
      automatic_retry_count: 1,
      packet_evidence: {
        encoded_packet_len: 218,
        encoded_packet_sha256: "cd".repeat(32),
      },
    });

    expect(timelineActivityRevision(equivalent)).toBe(timelineActivityRevision(failed));
    expect(timelineActivityRevision(retried)).not.toBe(timelineActivityRevision(failed));
  });

  test("refreshes details when receiver-local ingress evidence appears without replacing the row", () => {
    const received = message({
      direction: "inbound",
      outbox_id: null,
      submission_id: null,
      current_attempt_number: null,
      automatic_retry_count: null,
      status: null,
    });
    const observed = message({
      ...received,
      ingress_observation: {
        interface_id: 7,
        signal: {
          rssi_dbm: -97,
          snr_db: 4,
        },
      },
    });

    expect(timelineEntryKey(observed)).toBe(timelineEntryKey(received));
    expect(timelineActivityRevision(observed)).not.toBe(timelineActivityRevision(received));
  });

  test("replaces the terminal device submission with only a fresh request key", () => {
    const failed = message();

    expect(retryMessageRequest(failed, "ef".repeat(16))).toEqual({
      outbox_id: 3,
      idempotency_key: "ef".repeat(16),
    });
    expect(
      retryMessageRequest(message({ status: "awaiting_delivery" }), "ef".repeat(16)),
    ).toBeNull();
  });
});
