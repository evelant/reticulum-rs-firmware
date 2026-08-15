import { describe, expect, test } from "bun:test";

import {
  conversationPeerLabel,
  messageRequestPeers,
  outboundOnlyUnsavedPeers,
  suggestedContactName,
} from "./conversation-peers.ts";

describe("conversation peer presentation", () => {
  test("keeps authenticated unknown senders separate from saved contacts", () => {
    const request = {
      destination: "11".repeat(16),
      inbound_message_count: 2,
      message_count: 2,
      name: null,
    };
    const outboundOnly = {
      destination: "33".repeat(16),
      inbound_message_count: 0,
      message_count: 1,
      name: null,
    };
    const laterRequest = {
      destination: "44".repeat(16),
      inbound_message_count: 1,
      message_count: 3,
      name: null,
    };
    const peers = [
      request,
      {
        destination: "22".repeat(16),
        inbound_message_count: 1,
        message_count: 1,
        name: "Field node",
      },
      outboundOnly,
      laterRequest,
    ];

    expect(messageRequestPeers(peers)).toEqual([request, laterRequest]);
    expect(outboundOnlyUnsavedPeers(peers)).toEqual([outboundOnly]);
  });

  test("uses local aliases when present and stable fingerprints otherwise", () => {
    const destination = "00112233445566778899aabbccddeeff";

    expect(conversationPeerLabel({ destination, name: "Alice" })).toBe("Alice");
    expect(conversationPeerLabel({ destination, name: null })).toBe("Unknown …ddeeff");
    expect(suggestedContactName(destination)).toBe("Peer ddeeff");
  });
});
