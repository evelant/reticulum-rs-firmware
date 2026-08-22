import type { NativeProfileStoreSnapshot } from "@reticulum/appliance-native";
import { useEffect, useRef, useState } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";

import { errorText } from "../lib/app-error.ts";
import { knownProfileForManagementDestination } from "../lib/appliance-profiles.ts";
import type { OnboardingView } from "../lib/onboarding.ts";
import type {
  ReticulumApplianceCandidate,
  ReticulumDiscoveryOptions,
} from "../lib/reticulum-appliance-candidate.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { styles } from "./appliance-screen-styles.ts";

interface OnboardingPanelProps {
  readonly addingAppliance: boolean;
  readonly busy: boolean;
  readonly knownProfiles: NativeProfileStoreSnapshot | null;
  readonly onboarding: OnboardingView;
  readonly onCancel: (() => Promise<void>) | null;
  readonly onMutation: (candidate: ReticulumApplianceCandidate | null) => void;
  readonly onScanCandidates:
    | ((options?: ReticulumDiscoveryOptions) => Promise<readonly ReticulumApplianceCandidate[]>)
    | null;
  readonly onSwitchKnownProfile: (profileKey: string) => void;
}

function candidateLabel(candidate: ReticulumApplianceCandidate): string {
  return `reticulum:${candidate.managementDestination.slice(-8)}`;
}

export function OnboardingPanel({
  addingAppliance,
  busy,
  knownProfiles,
  onboarding,
  onCancel,
  onMutation,
  onScanCandidates,
  onSwitchKnownProfile,
}: OnboardingPanelProps) {
  const [candidates, setCandidates] = useState<readonly ReticulumApplianceCandidate[]>([]);
  const [scanError, setScanError] = useState<string | null>(null);
  const [scanFinished, setScanFinished] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [selectedDestination, setSelectedDestination] = useState<string | null>(null);
  const scanAbort = useRef<AbortController | null>(null);
  const { lifecycle } = onboarding;
  const ready = lifecycle.state === "ready" || lifecycle.state === "unavailable";
  const working = lifecycle.state === "authorizing";
  const selected =
    candidates.find((candidate) => candidate.managementDestination === selectedDestination) ?? null;

  // A scan owns its controller across state-driven renders; only leaving this
  // screen cancels the in-flight native request.
  useEffect(
    () => () => {
      scanAbort.current?.abort(new Error("Reticulum discovery screen closed"));
      scanAbort.current = null;
    },
    [],
  );

  const scan = async () => {
    if (onScanCandidates === null || scanning || scanAbort.current !== null) return;
    const abort = new AbortController();
    scanAbort.current = abort;
    setScanning(true);
    setScanError(null);
    setScanFinished(false);
    try {
      const observed = await onScanCandidates({ signal: abort.signal });
      if (scanAbort.current !== abort) return;
      setCandidates(observed);
      setSelectedDestination((current) =>
        observed.some((candidate) => candidate.managementDestination === current) ? current : null,
      );
      setScanFinished(true);
    } catch (error) {
      if (scanAbort.current !== abort || abort.signal.aborted) return;
      setScanError(errorText(error));
    } finally {
      if (scanAbort.current === abort) {
        scanAbort.current = null;
        setScanning(false);
      }
    }
  };

  if (ready && !addingAppliance) return null;
  return (
    <ScrollView
      alwaysBounceVertical={false}
      automaticallyAdjustContentInsets={false}
      automaticallyAdjustKeyboardInsets={false}
      bounces={false}
      contentContainerStyle={styles.onboardingScrollContent}
      nestedScrollEnabled
      style={styles.onboardingScroller}
    >
      <View accessibilityLiveRegion="polite" style={styles.onboarding}>
        <Text style={styles.eyebrow}>{addingAppliance ? "ADD APPLIANCE" : "FIRST-RUN SETUP"}</Text>
        <Text style={styles.onboardingTitle}>
          {working ? "Authorizing Reticulum identity" : "Choose a Reticulum appliance"}
        </Text>
        <Text style={styles.secondaryText}>
          {working
            ? "Keep the board's enrollment window open while the app verifies its identified management request."
            : "The app verifies management announces through its own PRNS node. Saved nodes use the same Reticulum identity over any available PRNS interface."}
        </Text>
        <View style={styles.actionRow}>
          <ActionButton
            disabled={busy || scanning || working || onScanCandidates === null}
            label={scanning ? "Checking announces…" : "Refresh announces"}
            onPress={() => void scan()}
          />
        </View>
        {scanError === null ? null : (
          <Text accessibilityLiveRegion="assertive" style={styles.inlineError}>
            {scanError}
          </Text>
        )}
        {scanFinished && candidates.length === 0 ? (
          <Text style={styles.secondaryText}>
            No verified appliance management announces have been observed yet.
          </Text>
        ) : null}
        {candidates.length === 0 ? null : (
          <ScrollView
            contentContainerStyle={styles.bleCandidateList}
            nestedScrollEnabled
            style={styles.bleCandidateScroller}
          >
            {candidates.map((candidate) => {
              const isSelected =
                selected?.managementDestination === candidate.managementDestination;
              const known =
                knownProfiles === null
                  ? null
                  : knownProfileForManagementDestination(
                      knownProfiles,
                      candidate.managementDestination,
                    );
              return (
                <Pressable
                  accessibilityLabel={
                    known === null
                      ? `Select ${candidateLabel(candidate)}`
                      : `Switch to saved appliance ${known.boardLabel}`
                  }
                  accessibilityRole="button"
                  accessibilityState={{ disabled: busy || working, selected: isSelected }}
                  disabled={busy || working}
                  key={candidate.managementDestination}
                  onPress={() => {
                    if (known === null) {
                      setSelectedDestination(candidate.managementDestination);
                    } else {
                      onSwitchKnownProfile(known.profileKey);
                    }
                  }}
                  style={({ pressed }) => [
                    styles.bleCandidate,
                    isSelected && styles.bleCandidateSelected,
                    pressed && styles.buttonPressed,
                  ]}
                >
                  <View style={styles.bleCandidateHeading}>
                    <Text numberOfLines={1} style={styles.bleCandidateName}>
                      {candidateLabel(candidate)}
                    </Text>
                    <Text style={styles.bleCandidateChoice}>
                      {known === null ? (isSelected ? "Selected" : "Select") : "Switch"}
                    </Text>
                  </View>
                  <Text selectable style={styles.monospace}>
                    {candidate.managementDestination}
                  </Text>
                  <Text style={styles.secondaryText}>
                    LXMF {candidate.lxmfDestination.slice(-8)} · {candidate.hops} hop
                    {candidate.hops === 1 ? "" : "s"} · interface {candidate.interfaceId}
                  </Text>
                </Pressable>
              );
            })}
          </ScrollView>
        )}
        <View style={styles.actionRow}>
          <ActionButton
            disabled={busy || scanning || working || selected === null}
            label={working ? "Authorizing…" : "Authorize selected appliance"}
            onPress={() => onMutation(selected)}
          />
          {onCancel === null ? null : (
            <ActionButton
              disabled={busy || working}
              label="Cancel"
              onPress={() => void onCancel()}
              secondary
            />
          )}
        </View>
      </View>
    </ScrollView>
  );
}
