import { describe, expect, test } from "bun:test";

import type { NativeProfileStoreSnapshot, NativeProfileSummary } from "@reticulum/appliance-native";

import {
  applianceProfilePresentation,
  applianceProfilesPresentation,
  hasKnownAdvertisedName,
  knownAdvertisedName,
  knownProfileForAdvertisedName,
} from "./appliance-profiles.ts";

function profile(
  overrides: {
    readonly deviceId?: string;
    readonly expectedBleLocalName?: string;
    readonly generation?: bigint;
    readonly profileKey?: string;
  } = {},
): NativeProfileSummary {
  return {
    credential: {
      credentialId: "11".repeat(16),
      deviceId: overrides.deviceId ?? "653239302d6170692d31aca704e13f88",
      expectedBleLocalName: overrides.expectedBleLocalName,
      generation: overrides.generation ?? 7n,
    },
    profileKey: overrides.profileKey ?? "653239302d6170692d31aca704e13f88",
  };
}

describe("appliance profile presentation", () => {
  test("uses the same readable E290 identity as the appliance status card", () => {
    expect(
      applianceProfilePresentation(
        profile({ expectedBleLocalName: "reticulum-e290-e13f88" }),
        "653239302D6170692D31ACA704E13F88",
      ),
    ).toEqual({
      active: true,
      advertisedName: "reticulum-e290-e13f88",
      boardLabel: "AC:A7:04:E1:3F:88",
      bleLabel: "reticulum-e290-e13f88",
      deviceId: "653239302d6170692d31aca704e13f88",
      generationLabel: "Credential generation 7",
      profileKey: "653239302d6170692d31aca704e13f88",
    });
  });

  test("keeps an exact bigint generation lossless in user-facing text", () => {
    const generation = 18_446_744_073_709_551_615n;

    expect(applianceProfilePresentation(profile({ generation }), undefined).generationLabel).toBe(
      "Credential generation 18446744073709551615",
    );
  });

  test("preserves opaque device identities and falls back to the profile key when empty", () => {
    expect(
      applianceProfilePresentation(
        profile({ deviceId: "field-node-alpha", profileKey: "profile-a" }),
        undefined,
      ).boardLabel,
    ).toBe("field-node-alpha");
    expect(
      applianceProfilePresentation(
        profile({ deviceId: "  ", profileKey: "profile-fallback" }),
        undefined,
      ).boardLabel,
    ).toBe("profile-fallback");
  });

  test("distinguishes an exact advertised name from missing or blank metadata", () => {
    const known = profile({ expectedBleLocalName: "  reticulum-e290-e13f88  " });
    const missing = profile();
    const blank = profile({ expectedBleLocalName: "   " });

    expect(knownAdvertisedName(known)).toBe("reticulum-e290-e13f88");
    expect(hasKnownAdvertisedName(known)).toBeTrue();
    expect(knownAdvertisedName(missing)).toBeNull();
    expect(hasKnownAdvertisedName(missing)).toBeFalse();
    expect(hasKnownAdvertisedName(blank)).toBeFalse();
    expect(applianceProfilePresentation(blank, undefined).bleLabel).toBe("BLE name unavailable");
  });
});

describe("appliance profile store presentation", () => {
  test("marks only the generated active profile and preserves canonical ordering", () => {
    const first = profile({
      deviceId: "653239302d6170692d31aca704e13e88",
      profileKey: "653239302d6170692d31aca704e13e88",
    });
    const second = profile({
      deviceId: "653239302d6170692d31aca704e13f88",
      profileKey: "653239302d6170692d31aca704e13f88",
    });
    const snapshot: NativeProfileStoreSnapshot = {
      activeProfileKey: second.profileKey,
      profiles: [first, second],
    };

    const presentation = applianceProfilesPresentation(snapshot);

    expect(presentation.profiles.map(({ profileKey }) => profileKey)).toEqual([
      first.profileKey,
      second.profileKey,
    ]);
    expect(presentation.profiles.map(({ active }) => active)).toEqual([false, true]);
    expect(presentation.activeProfile?.boardLabel).toBe("AC:A7:04:E1:3F:88");
  });

  test("does not invent an active profile for an empty or inconsistent snapshot", () => {
    expect(applianceProfilesPresentation({ profiles: [] }).activeProfile).toBeNull();
    expect(
      applianceProfilesPresentation({
        activeProfileKey: "missing",
        profiles: [profile()],
      }).activeProfile,
    ).toBeNull();
  });

  test("routes an exact discovered BLE name to its saved profile", () => {
    const saved = profile({ expectedBleLocalName: "reticulum-e290-e13f88" });
    const snapshot: NativeProfileStoreSnapshot = {
      activeProfileKey: saved.profileKey,
      profiles: [saved],
    };

    expect(knownProfileForAdvertisedName(snapshot, "  RETICULUM-E290-E13F88  ")?.profileKey).toBe(
      saved.profileKey,
    );
    expect(knownProfileForAdvertisedName(snapshot, "reticulum-e290-unknown")).toBeNull();
    expect(knownProfileForAdvertisedName(snapshot, "   ")).toBeNull();
    expect(knownProfileForAdvertisedName(snapshot, undefined)).toBeNull();
  });
});
