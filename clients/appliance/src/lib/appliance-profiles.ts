import type { NativeProfileStoreSnapshot, NativeProfileSummary } from "@reticulum/appliance-native";

export interface ApplianceProfilePresentation {
  readonly active: boolean;
  readonly boardLabel: string;
  readonly lxmfDestination: string;
  readonly managementDestination: string;
  readonly profileKey: string;
}

export interface ApplianceProfilesPresentation {
  readonly activeProfile: ApplianceProfilePresentation | null;
  readonly profiles: readonly ApplianceProfilePresentation[];
}

function normalizedIdentity(value: string | undefined): string {
  return value?.trim().toLowerCase() ?? "";
}

/** Present one saved Reticulum management application for the node selector. */
export function applianceProfilePresentation(
  profile: NativeProfileSummary,
  activeProfileKey: string | undefined,
): ApplianceProfilePresentation {
  const profileKey = profile.profileKey.trim().toLowerCase();
  const managementDestination = profile.managementDestination.trim().toLowerCase();
  const lxmfDestination = profile.lxmfDestination.trim().toLowerCase();
  const applianceLabel = profile.applianceLabel?.trim();
  return {
    active:
      normalizedIdentity(activeProfileKey) !== "" &&
      normalizedIdentity(activeProfileKey) === profileKey,
    boardLabel:
      applianceLabel === undefined || applianceLabel === ""
        ? `reticulum:${managementDestination.slice(-8)}`
        : applianceLabel,
    lxmfDestination,
    managementDestination,
    profileKey,
  };
}

/** Project the generated store snapshot without reordering its canonical list. */
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

/** Resolve one exact management destination to a saved profile. */
export function knownProfileForManagementDestination(
  snapshot: NativeProfileStoreSnapshot,
  managementDestination: string,
): ApplianceProfilePresentation | null {
  const destination = normalizedIdentity(managementDestination);
  if (destination === "") return null;
  return (
    applianceProfilesPresentation(snapshot).profiles.find(
      (profile) => profile.managementDestination === destination,
    ) ?? null
  );
}
