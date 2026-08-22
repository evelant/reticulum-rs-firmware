import { describe, expect, test } from "bun:test";

import type { NearbyPeerView } from "./nearby-peers.ts";
import {
  associatedNomadDestinationForLxmf,
  nearbyContacts,
  nearbyNetworkSummary,
  nearbyObserverLabel,
  nearbyObserverSummaryHint,
  nearbyPeerAge,
  nearbyPeerFingerprint,
  nearbyPeerObservationHint,
  nearbyPeerSuggestedName,
  nearbySnapshotElapsedMs,
} from "./nearby-peers.ts";
import { syntheticReticulumInterfaceId } from "./reticulum-interface-id.ts";

function peer(overrides: Partial<NearbyPeerView> = {}): NearbyPeerView {
  return {
    associated_nomad_destination: "cd".repeat(16),
    destination: "ab".repeat(16),
    display_name: "Field node",
    hops: 1,
    identity_hash: "1234567890abcdef".repeat(2),
    interface_id: syntheticReticulumInterfaceId(0),
    interface_name: "LoRa",
    observer_kind: "phone",
    observer_management_destination: null,
    observed_age_ms: 12_400,
    ...overrides,
  };
}

describe("nearby peer presentation", () => {
  test("deduplicates contacts while keeping phone and appliance observations separate", () => {
    const summary = nearbyNetworkSummary(
      [
        peer({ destination: "aa".repeat(16), hops: 1, observed_age_ms: 21_000 }),
        peer({
          destination: "aa".repeat(16),
          hops: 2,
          interface_id: syntheticReticulumInterfaceId(3),
          interface_name: "LoRa",
          observer_kind: "appliance",
          observer_management_destination: "11".repeat(16),
          observed_age_ms: 7_000,
        }),
        peer({
          destination: "bb".repeat(16),
          hops: 3,
          interface_id: syntheticReticulumInterfaceId(3),
          interface_name: "LoRa",
          observer_kind: "appliance",
          observer_management_destination: "11".repeat(16),
          observed_age_ms: 2_000,
        }),
        peer({ destination: "cc".repeat(16), hops: 2, observed_age_ms: 5_000 }),
      ],
      [{ destination: ` ${"AA".repeat(16)} ` }],
    );

    expect(summary).toEqual({
      peerCount: 3,
      observationCount: 4,
      unaddedPeerCount: 2,
      observerCount: 2,
      observers: [
        {
          observerKind: "appliance",
          observerManagementDestination: "11".repeat(16),
          peerCount: 2,
          observationCount: 2,
          interfaceLabels: ["LoRa"],
          freshestObservedAgeMs: 2_000,
        },
        {
          observerKind: "phone",
          observerManagementDestination: null,
          peerCount: 2,
          observationCount: 2,
          interfaceLabels: ["LoRa"],
          freshestObservedAgeMs: 5_000,
        },
      ],
    });
    const [alice] = nearbyContacts([
      peer({ destination: "aa".repeat(16), display_name: null }),
      peer({
        destination: "aa".repeat(16),
        display_name: "Alice",
        observer_kind: "appliance",
        observer_management_destination: "11".repeat(16),
      }),
    ]);
    expect(alice?.representative.display_name).toBe("Alice");
    expect(alice?.observations).toHaveLength(2);
  });

  test("returns an empty bounded summary before any authenticated announce is retained", () => {
    expect(nearbyNetworkSummary([], [{ destination: "aa".repeat(16) }])).toEqual({
      peerCount: 0,
      observationCount: 0,
      unaddedPeerCount: 0,
      observerCount: 0,
      observers: [],
    });
  });

  test("labels observer nodes without treating one node's interface as the other's", () => {
    expect(
      nearbyObserverLabel({ observerKind: "phone", observerManagementDestination: null }, "Ridge"),
    ).toBe("This phone");
    expect(
      nearbyObserverLabel(
        { observerKind: "appliance", observerManagementDestination: "11".repeat(16) },
        "Ridge",
      ),
    ).toBe("Ridge");
    expect(
      nearbyObserverSummaryHint({
        observerKind: "appliance",
        observerManagementDestination: "11".repeat(16),
        peerCount: 2,
        observationCount: 2,
        interfaceLabels: ["LoRa"],
        freshestObservedAgeMs: 63_000,
      }),
    ).toBe("2 peers · 2 observations · LoRa · last announce 1m ago");
    expect(
      nearbyObserverLabel(
        { observerKind: "appliance", observerManagementDestination: "12345678".padEnd(32, "0") },
        null,
      ),
    ).toBe("Appliance 1234 5678");
  });

  test("deduplicates repeated rows from one observer and interface", () => {
    const contacts = nearbyContacts([
      peer({ observed_age_ms: 20_000 }),
      peer({ observed_age_ms: 2_000 }),
    ]);
    expect(contacts).toHaveLength(1);
    expect(contacts[0]?.observations).toHaveLength(1);
    expect(contacts[0]?.observations[0]?.observed_age_ms).toBe(2_000);
  });

  test("preserves an opaque future interface identity in observation text", () => {
    expect(
      nearbyPeerObservationHint(
        peer({
          hops: 2,
          interface_id: syntheticReticulumInterfaceId(7),
          interface_name: null,
          observed_age_ms: 63_000,
        }),
        null,
      ),
    ).toBe("This phone · 2 hops · interface 0000000000000007 · announced 1m ago");
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

  test("presents node-scoped observation hints without claiming a direct route", () => {
    expect(nearbyPeerObservationHint(peer(), null)).toBe(
      "This phone · 1 hop · LoRa · announced 12s ago",
    );
    expect(
      nearbyPeerObservationHint(
        peer({
          hops: 2,
          interface_id: syntheticReticulumInterfaceId(3),
          interface_name: null,
          observer_kind: "appliance",
          observer_management_destination: "11".repeat(16),
          observed_age_ms: 0,
        }),
        "Ridge",
      ),
    ).toBe("Ridge · 2 hops · interface 0000000000000003 · announced just now");
  });

  test("ages a retained snapshot without losing its transport-neutral interface summary", () => {
    const summary = nearbyNetworkSummary(
      [
        peer({
          interface_id: syntheticReticulumInterfaceId(2),
          interface_name: "Reticulum TCP",
          observed_age_ms: 3_000,
        }),
      ],
      [],
      61_000,
    );

    const [observer] = summary.observers;
    if (observer === undefined) throw new Error("the fixture must expose one observer");
    expect(observer.freshestObservedAgeMs).toBe(64_000);
    expect(nearbyObserverSummaryHint(observer)).toContain("last announce 1m ago");
    expect(nearbyPeerObservationHint(peer({ observed_age_ms: 3_000 }), null, 61_000)).toContain(
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
