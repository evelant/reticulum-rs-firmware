import { describe, expect, test } from "bun:test";
import type { NativeProfileSummary } from "@reticulum/appliance-native";

import {
  applianceProfilePresentation,
  applianceProfilesPresentation,
  knownProfileForManagementDestination,
} from "./appliance-profiles.ts";

const FIRST: NativeProfileSummary = {
  profileKey: "11".repeat(16),
  managementDestination: "11".repeat(16),
  lxmfDestination: "21".repeat(16),
};
const SECOND: NativeProfileSummary = {
  profileKey: "12".repeat(16),
  managementDestination: "12".repeat(16),
  lxmfDestination: "22".repeat(16),
};

describe("Reticulum appliance profiles", () => {
  test("profiles are keyed and labeled by management destination", () => {
    expect(applianceProfilePresentation(FIRST, FIRST.profileKey)).toEqual({
      active: true,
      boardLabel: "reticulum:11111111",
      lxmfDestination: FIRST.lxmfDestination,
      managementDestination: FIRST.managementDestination,
      profileKey: FIRST.profileKey,
    });
  });

  test("a cached product label replaces only the profile presentation fallback", () => {
    expect(
      applianceProfilePresentation({ ...FIRST, applianceLabel: "North node" }, FIRST.profileKey)
        .boardLabel,
    ).toBe("North node");
  });

  test("the active profile is selected without reordering the catalog", () => {
    const presented = applianceProfilesPresentation({
      activeProfileKey: SECOND.profileKey,
      profiles: [FIRST, SECOND],
    });
    expect(presented.profiles.map((profile) => profile.profileKey)).toEqual([
      FIRST.profileKey,
      SECOND.profileKey,
    ]);
    expect(presented.activeProfile?.profileKey).toBe(SECOND.profileKey);
  });

  test("a verified announce resolves only by exact management destination", () => {
    const snapshot = { activeProfileKey: FIRST.profileKey, profiles: [FIRST, SECOND] };
    expect(
      knownProfileForManagementDestination(
        snapshot,
        `  ${SECOND.managementDestination.toUpperCase()}  `,
      )?.profileKey,
    ).toBe(SECOND.profileKey);
    expect(knownProfileForManagementDestination(snapshot, "ff".repeat(16))).toBeNull();
  });
});
