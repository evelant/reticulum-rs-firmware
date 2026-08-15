import { describe, expect, test } from "bun:test";

import type { SendRequest, SendResponse, TimelineView } from "../generated/api.ts";
import {
  localMessageAcceptance,
  recordLocalMessageAcceptance,
  unreconciledLocalMessageAcceptances,
} from "./message-submit-ui.ts";

const request: SendRequest = {
  content: "message",
  destination: "11".repeat(16),
  idempotency_key: "22".repeat(16),
  location: null,
  timestamp_ms: 1_786_741_200_000,
  title: "title",
};

const response: SendResponse = { outbox_id: 41, outcome: "inserted" };

function timeline(outboxId: number): TimelineView {
  return {
    content: { encoding: "utf8", value: "message" },
    current_attempt_number: 1,
    direction: "outbound",
    ingress_observation: null,
    location: null,
    message_id: null,
    outbox_id: outboxId,
    packet_evidence: null,
    receiver_location: null,
    sequence: 9,
    status: "committed",
    submission_id: null,
    timestamp_ms: request.timestamp_ms,
    title: { encoding: "utf8", value: "title" },
  };
}

describe("local message acceptance presentation", () => {
  test("projects only fields known at durable acceptance", () => {
    expect(localMessageAcceptance(request, response)).toEqual({
      content: "message",
      destination: "11".repeat(16),
      location: null,
      outboxId: 41,
      timestampMs: 1_786_741_200_000,
      title: "title",
    });
  });

  test("coalesces an idempotent replay by durable outbox id", () => {
    const first = localMessageAcceptance(request, response);
    const once = recordLocalMessageAcceptance([], first);
    expect(recordLocalMessageAcceptance(once, first)).toBe(once);

    const replay = localMessageAcceptance(request, { outbox_id: 41, outcome: "existing" });
    expect(recordLocalMessageAcceptance(once, replay)).toEqual([replay]);
  });

  test("removes only acceptances present in an authoritative timeline", () => {
    const first = localMessageAcceptance(request, response);
    const second = { ...first, outboxId: 42 };
    const pending = [first, second];

    expect(
      unreconciledLocalMessageAcceptances(pending, [timeline(41)], request.destination),
    ).toEqual([second]);
    expect(unreconciledLocalMessageAcceptances(pending, [timeline(99)], request.destination)).toBe(
      pending,
    );
  });

  test("never carries a local placeholder into another conversation", () => {
    const first = localMessageAcceptance(request, response);
    expect(unreconciledLocalMessageAcceptances([first], [], "33".repeat(16))).toEqual([]);
    expect(unreconciledLocalMessageAcceptances([first], [], null)).toEqual([]);
  });
});
