import { describe, expect, test } from "bun:test";

import type { NearbyPeerView } from "./nearby-peers.ts";
import {
  nearbyPeerAge,
  nearbyPeerFingerprint,
  nearbyPeerRouteHint,
  nearbyPeerSuggestedName,
} from "./nearby-peers.ts";

function peer(overrides: Partial<NearbyPeerView> = {}): NearbyPeerView {
  return {
    destination: "ab".repeat(16),
    display_name: "Field node",
    hops: 1,
    identity_hash: "1234567890abcdef".repeat(2),
    interface_id: 0,
    interface_name: "LoRa",
    observed_age_ms: 12_400,
    rssi_dbm: -91,
    snr_db: 7,
    ...overrides,
  };
}

describe("nearby peer presentation", () => {
  test("uses the Rust-decoded display name for one-tap contact creation", () => {
    expect(nearbyPeerSuggestedName(peer())).toBe("Field node");
    expect(nearbyPeerSuggestedName(peer({ display_name: "  Ridge relay  " }))).toBe("Ridge relay");
  });

  test("falls back to a stable short public identity fingerprint", () => {
    const unnamed = peer({ display_name: null });
    expect(nearbyPeerFingerprint(unnamed)).toBe("1234 5678 90ab");
    expect(nearbyPeerSuggestedName(unnamed)).toBe("Peer 1234 5678 90ab");
  });

  test("rejects an overlong announced name at the existing contact boundary", () => {
    const overlong = peer({ display_name: "🛰️".repeat(100) });
    expect(nearbyPeerSuggestedName(overlong)).toBe("Peer 1234 5678 90ab");
  });

  test("falls back to the destination when a defensive test double lacks an identity hash", () => {
    expect(nearbyPeerFingerprint(peer({ identity_hash: "" }))).toBe("abab abab abab");
  });

  test("formats fresh, minute, hour, and day age bands", () => {
    expect(nearbyPeerAge(0)).toBe("just now");
    expect(nearbyPeerAge(59_999)).toBe("59s ago");
    expect(nearbyPeerAge(120_000)).toBe("2m ago");
    expect(nearbyPeerAge(7_200_000)).toBe("2h ago");
    expect(nearbyPeerAge(172_800_000)).toBe("2d ago");
  });

  test("presents transport-neutral route and optional signal hints", () => {
    expect(nearbyPeerRouteHint(peer())).toBe("1 hop · LoRa · 12s ago · -91 dBm · 7 dB SNR");
    expect(
      nearbyPeerRouteHint(
        peer({
          hops: 0,
          interface_id: 3,
          interface_name: null,
          observed_age_ms: 0,
          rssi_dbm: null,
          snr_db: null,
        }),
      ),
    ).toBe("direct · interface 3 · just now");
  });
});
