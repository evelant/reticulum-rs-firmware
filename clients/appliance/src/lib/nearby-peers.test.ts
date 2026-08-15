import { describe, expect, test } from "bun:test";

import type { NearbyPeerView } from "./nearby-peers.ts";
import {
  associatedNomadDestinationForLxmf,
  nearbyInterfaceLabel,
  nearbyInterfaceSummaryHint,
  nearbyNetworkSummary,
  nearbyPeerAge,
  nearbyPeerFingerprint,
  nearbyPeerRouteHint,
  nearbyPeerSuggestedName,
  nearbySnapshotElapsedMs,
} from "./nearby-peers.ts";

function peer(overrides: Partial<NearbyPeerView> = {}): NearbyPeerView {
  return {
    associated_nomad_destination: "cd".repeat(16),
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
  test("summarizes unadded peers and observing interfaces without inventing route state", () => {
    const summary = nearbyNetworkSummary(
      [
        peer({ destination: "aa".repeat(16), hops: 1, observed_age_ms: 21_000 }),
        peer({
          destination: "bb".repeat(16),
          hops: 2,
          interface_id: 3,
          interface_name: null,
          observed_age_ms: 7_000,
          rssi_dbm: null,
          snr_db: null,
        }),
        peer({
          destination: "cc".repeat(16),
          hops: 3,
          interface_id: 0,
          interface_name: " LoRa ",
          observed_age_ms: 2_000,
        }),
      ],
      [{ destination: ` ${"AA".repeat(16)} ` }],
    );

    expect(summary).toEqual({
      peerCount: 3,
      unaddedPeerCount: 2,
      interfaceCount: 2,
      interfaces: [
        {
          interfaceId: 0,
          interfaceName: "LoRa",
          peerCount: 2,
          unaddedPeerCount: 1,
          directPeerCount: 1,
          freshestObservedAgeMs: 2_000,
        },
        {
          interfaceId: 3,
          interfaceName: null,
          peerCount: 1,
          unaddedPeerCount: 1,
          directPeerCount: 0,
          freshestObservedAgeMs: 7_000,
        },
      ],
    });
  });

  test("returns an empty bounded summary before any authenticated announce is retained", () => {
    expect(nearbyNetworkSummary([], [{ destination: "aa".repeat(16) }])).toEqual({
      peerCount: 0,
      unaddedPeerCount: 0,
      interfaceCount: 0,
      interfaces: [],
    });
  });

  test("labels known and future interface slots without assuming a transport", () => {
    const [known, future] = nearbyNetworkSummary(
      [
        peer({ hops: 1, interface_id: 1, interface_name: "LoRa", observed_age_ms: 2_000 }),
        peer({
          destination: "ef".repeat(16),
          hops: 2,
          interface_id: 7,
          interface_name: null,
          observed_age_ms: 63_000,
        }),
      ],
      [],
    ).interfaces;

    if (known === undefined || future === undefined) {
      throw new Error("the fixture must project both observing interfaces");
    }
    expect(nearbyInterfaceLabel(known)).toBe("LoRa · interface 1");
    expect(nearbyInterfaceSummaryHint(known)).toBe(
      "1 peer · 1 not in contacts · 1 direct peer · last announce just now",
    );
    expect(nearbyInterfaceLabel(future)).toBe("Interface 7");
    expect(nearbyInterfaceSummaryHint(future)).toBe(
      "1 peer · 1 not in contacts · 0 direct peers · last announce 1m ago",
    );
  });

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

  test("cross-references an LXMF contact with its separately authenticated Nomad destination", () => {
    expect(associatedNomadDestinationForLxmf([peer()], `  ${"AB".repeat(16)}  `)).toBe(
      "cd".repeat(16),
    );
  });

  test("does not mistake or derive an LXMF contact hash for a Nomad destination", () => {
    expect(associatedNomadDestinationForLxmf([], "ab".repeat(16))).toBeNull();
    expect(
      associatedNomadDestinationForLxmf(
        [peer({ associated_nomad_destination: "not-a-destination" })],
        "ab".repeat(16),
      ),
    ).toBeNull();
    expect(
      associatedNomadDestinationForLxmf(
        [peer({ associated_nomad_destination: "ab".repeat(16) })],
        "ab".repeat(16),
      ),
    ).toBeNull();
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
    expect(nearbyPeerRouteHint(peer())).toBe(
      "direct · LoRa · announced 12s ago · RX -91 dBm · SNR 7 dB",
    );
    expect(
      nearbyPeerRouteHint(
        peer({
          hops: 2,
          interface_id: 3,
          interface_name: null,
          observed_age_ms: 0,
          rssi_dbm: null,
          snr_db: null,
        }),
      ),
    ).toBe("2 hops · interface 3 · announced just now");
  });

  test("ages a retained snapshot without losing its transport-neutral interface summary", () => {
    const summary = nearbyNetworkSummary(
      [peer({ interface_id: 2, interface_name: "Reticulum TCP", observed_age_ms: 3_000 })],
      [],
      61_000,
    );

    const [observedInterface] = summary.interfaces;
    if (observedInterface === undefined) throw new Error("the fixture must expose one interface");
    expect(observedInterface.freshestObservedAgeMs).toBe(64_000);
    expect(nearbyInterfaceSummaryHint(observedInterface)).toContain("last announce 1m ago");
    expect(nearbyPeerRouteHint(peer({ observed_age_ms: 3_000 }), 61_000)).toContain(
      "announced 1m ago",
    );
  });

  test("derives only forward elapsed time from the successful snapshot fetch", () => {
    expect(nearbySnapshotElapsedMs(1_000, 4_250)).toBe(3_250);
    expect(nearbySnapshotElapsedMs(5_000, 4_000)).toBe(0);
    expect(nearbySnapshotElapsedMs(null, 4_000)).toBe(0);
    expect(nearbySnapshotElapsedMs(Number.NaN, 4_000)).toBe(0);
  });
});
