import type { NativeProfileStoreSnapshot, NativeProfileSummary } from "@reticulum/appliance-native";

import { formatDeviceId } from "./appliance-status.ts";

export interface ApplianceProfilePresentation {
  readonly active: boolean;
  readonly advertisedName: string | null;
  readonly boardLabel: string;
  readonly bleLabel: string;
  readonly deviceId: string;
  readonly generationLabel: string;
  readonly profileKey: string;
}

export interface ApplianceProfilesPresentation {
  readonly activeProfile: ApplianceProfilePresentation | null;
  readonly profiles: readonly ApplianceProfilePresentation[];
}

function normalizedIdentity(value: string | undefined): string {
  return value?.trim().toLowerCase() ?? "";
}

/** Return the exact, nonempty BLE advertising name authorized by a profile. */
export function knownAdvertisedName(profile: NativeProfileSummary): string | null {
  const advertisedName = profile.credential.expectedBleLocalName?.trim() ?? "";
  return advertisedName === "" ? null : advertisedName;
}

/** Whether a profile can target one exact BLE advertiser without a broad scan. */
export function hasKnownAdvertisedName(profile: NativeProfileSummary): boolean {
  return knownAdvertisedName(profile) !== null;
}

/** Present one generated, secret-free native profile for an appliance selector. */
export function applianceProfilePresentation(
  profile: NativeProfileSummary,
  activeProfileKey: string | undefined,
): ApplianceProfilePresentation {
  const profileKey = profile.profileKey.trim();
  const deviceId = profile.credential.deviceId.trim();
  const advertisedName = knownAdvertisedName(profile);
  const activeKey = normalizedIdentity(activeProfileKey);
  const profileIdentity = normalizedIdentity(profileKey);

  return {
    active: activeKey !== "" && activeKey === profileIdentity,
    advertisedName,
    boardLabel: formatDeviceId(deviceId === "" ? profileKey : deviceId),
    bleLabel: advertisedName ?? "BLE name unavailable",
    deviceId,
    generationLabel: `Credential generation ${profile.credential.generation.toString()}`,
    profileKey,
  };
}

/** Project the generated store snapshot without reordering its canonical profile list. */
export function applianceProfilesPresentation(
  snapshot: NativeProfileStoreSnapshot,
): ApplianceProfilesPresentation {
  const profiles = snapshot.profiles.map((profile) =>
    applianceProfilePresentation(profile, snapshot.activeProfileKey),
  );

  return {
    activeProfile: profiles.find((profile) => profile.active) ?? null,
    profiles,
  };
}

/** Resolve an exact discovered BLE name to an already stored profile. */
export function knownProfileForAdvertisedName(
  snapshot: NativeProfileStoreSnapshot,
  advertisedName: string | undefined,
): ApplianceProfilePresentation | null {
  const normalizedName = advertisedName?.trim().toLowerCase() ?? "";
  if (normalizedName === "") return null;
  return (
    applianceProfilesPresentation(snapshot).profiles.find(
      (profile) => profile.advertisedName?.toLowerCase() === normalizedName,
    ) ?? null
  );
}
