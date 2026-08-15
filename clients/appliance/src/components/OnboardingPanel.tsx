import type { NativeProfileStoreSnapshot } from "@reticulum/appliance-native";
import { useEffect, useRef, useState } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";

import type { OnboardingView, RecoveryRequest } from "../generated/api.ts";
import { errorText } from "../lib/app-error.ts";
import { knownProfileForAdvertisedName } from "../lib/appliance-profiles.ts";
import type { BleCandidate, BleScanOptions } from "../lib/ble-central-types.ts";
import {
  BLE_SECURITY_CONTINUE_LABEL,
  bleCandidateDetails,
  bleCandidateName,
  bleDiscoveryPresentation,
  onboardingPresentation,
  selectedBleCandidate,
} from "../lib/onboarding.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { styles } from "./appliance-screen-styles.ts";

const ONBOARDING_BLE_SCAN_TIMEOUT_MS = 15_000;

interface OnboardingPanelProps {
  readonly addingAppliance: boolean;
  readonly busy: boolean;
  readonly knownProfiles: NativeProfileStoreSnapshot | null;
  readonly onboarding: OnboardingView;
  readonly onCancel: (() => Promise<void>) | null;
  readonly onMutation: (
    path: "start" | "continue" | "refresh" | RecoveryRequest["action"],
    candidate: BleCandidate | null,
  ) => void;
  readonly onScanBleCandidates:
    | ((options?: BleScanOptions) => Promise<readonly BleCandidate[]>)
    | null;
  readonly onSwitchKnownProfile: (profileKey: string) => void;
}

export function OnboardingPanel({
  addingAppliance,
  busy,
  knownProfiles,
  onboarding,
  onCancel,
  onMutation,
  onScanBleCandidates,
  onSwitchKnownProfile,
}: OnboardingPanelProps) {
  const [bleCandidates, setBleCandidates] = useState<readonly BleCandidate[]>([]);
  const [bleScanError, setBleScanError] = useState<string | null>(null);
  const [bleScanFinished, setBleScanFinished] = useState(false);
  const [bleScanning, setBleScanning] = useState(false);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [selectedPeripheralId, setSelectedPeripheralId] = useState<string | null>(null);
  const scanAbort = useRef<AbortController | null>(null);
  const presentation = onboardingPresentation(onboarding);
  const discovery = bleDiscoveryPresentation(onboarding, onScanBleCandidates !== null);
  const selectedCandidate = selectedBleCandidate(bleCandidates, selectedPeripheralId);
  const lifecycle = onboarding.snapshot?.lifecycle;
  const lifecycleStage = lifecycle?.state === "working" ? lifecycle.stage : null;
  const scrollEpoch =
    `${lifecycle?.state ?? "unavailable"}:${lifecycleStage ?? "idle"}:` +
    (bleScanning ? "scanning" : "settled");
  const canRetryBle =
    discovery.available &&
    onboarding.method === "managed_pairing" &&
    lifecycle?.state === "faulted" &&
    lifecycle.reason !== "invalid_credential_artifact";
  const canContinueBle =
    lifecycle?.state === "working" && lifecycle.stage === "waiting_for_ble_security";
  const canCancelBle =
    onCancel !== null &&
    onboarding.method === "managed_pairing" &&
    ((addingAppliance && !(lifecycle?.state === "working" && lifecycle.stage === "activating")) ||
      (lifecycle?.state === "working" && lifecycle.stage !== "activating"));

  useEffect(
    () => () => {
      scanAbort.current?.abort(new Error("BLE discovery screen closed"));
      scanAbort.current = null;
    },
    [],
  );

  useEffect(() => {
    if (discovery.available) return;
    scanAbort.current?.abort(new Error("BLE discovery is no longer available"));
    scanAbort.current = null;
    setBleCandidates([]);
    setBleScanError(null);
    setBleScanFinished(false);
    setBleScanning(false);
    setSelectedPeripheralId(null);
  }, [discovery.available]);

  const scanBleCandidates = async () => {
    if (onScanBleCandidates === null || bleScanning || scanAbort.current !== null) return;
    const abort = new AbortController();
    scanAbort.current = abort;
    setBleScanning(true);
    setBleCandidates([]);
    setBleScanError(null);
    setBleScanFinished(false);
    setSelectedPeripheralId(null);
    try {
      const candidates = await onScanBleCandidates({
        scanTimeoutMs: ONBOARDING_BLE_SCAN_TIMEOUT_MS,
        signal: abort.signal,
      });
      if (scanAbort.current !== abort) return;
      setBleCandidates(candidates);
      setSelectedPeripheralId(
        (current) => selectedBleCandidate(candidates, current)?.peripheralId ?? null,
      );
      setBleScanFinished(true);
    } catch (scanError) {
      if (scanAbort.current !== abort || abort.signal.aborted) return;
      setBleScanError(errorText(scanError));
    } finally {
      if (scanAbort.current === abort) {
        scanAbort.current = null;
        setBleScanning(false);
      }
    }
  };

  const cancelBleOnboarding = async () => {
    if (onCancel === null || cancelling) return;
    setCancelling(true);
    setCancelError(null);
    try {
      await onCancel();
    } catch (cancelError) {
      setCancelError(errorText(cancelError));
    } finally {
      setCancelling(false);
    }
  };

  if (presentation.ready) return null;
  return (
    <ScrollView
      alwaysBounceVertical={false}
      automaticallyAdjustContentInsets={false}
      automaticallyAdjustKeyboardInsets={false}
      bounces={false}
      contentContainerStyle={styles.onboardingScrollContent}
      key={scrollEpoch}
      nestedScrollEnabled
      style={styles.onboardingScroller}
    >
      <View accessibilityLiveRegion="polite" style={styles.onboarding}>
        <Text style={styles.eyebrow}>{addingAppliance ? "ADD APPLIANCE" : "FIRST-RUN SETUP"}</Text>
        <Text style={styles.onboardingTitle}>{presentation.title}</Text>
        <Text style={styles.secondaryText}>{presentation.instruction}</Text>
        {discovery.available || presentation.identifierLabel === null ? null : (
          <View style={styles.serialRow}>
            <Text style={styles.metaLabel}>{presentation.identifierLabel}</Text>
            <Text selectable style={styles.monospace}>
              {onboarding.snapshot?.device_label ?? "—"}
            </Text>
          </View>
        )}
        {discovery.available ? (
          <View style={styles.bleDiscovery}>
            <Text style={styles.bleDiscoveryTitle}>{discovery.title}</Text>
            <Text style={styles.secondaryText}>{discovery.instruction}</Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={busy || bleScanning || lifecycle?.state === "working"}
                label={bleScanning ? "Finding nearby boards…" : "Find nearby boards"}
                onPress={() => void scanBleCandidates()}
              />
            </View>
            {bleScanError === null ? null : (
              <Text accessibilityLiveRegion="assertive" style={styles.inlineError}>
                {bleScanError}
              </Text>
            )}
            {cancelError === null ? null : (
              <Text accessibilityLiveRegion="assertive" style={styles.inlineError}>
                {cancelError}
              </Text>
            )}
            {bleScanFinished && bleCandidates.length === 0 ? (
              <Text style={styles.secondaryText}>No nearby appliances were found.</Text>
            ) : null}
            {bleCandidates.length === 0 ? null : (
              <ScrollView
                contentContainerStyle={styles.bleCandidateList}
                nestedScrollEnabled
                style={styles.bleCandidateScroller}
              >
                {bleCandidates.map((candidate) => {
                  const selected = selectedCandidate?.peripheralId === candidate.peripheralId;
                  const knownProfile =
                    knownProfiles === null
                      ? null
                      : knownProfileForAdvertisedName(knownProfiles, candidate.peripheralName);
                  return (
                    <Pressable
                      accessibilityLabel={
                        knownProfile === null
                          ? `Select ${bleCandidateName(candidate)}`
                          : `Switch to saved appliance ${knownProfile.boardLabel}`
                      }
                      accessibilityRole="button"
                      accessibilityState={{
                        disabled: busy || lifecycle?.state === "working",
                        selected,
                      }}
                      disabled={busy || lifecycle?.state === "working"}
                      key={candidate.peripheralId}
                      onPress={() => {
                        if (knownProfile === null) {
                          setSelectedPeripheralId(candidate.peripheralId);
                        } else {
                          onSwitchKnownProfile(knownProfile.profileKey);
                        }
                      }}
                      style={({ pressed }) => [
                        styles.bleCandidate,
                        selected && styles.bleCandidateSelected,
                        pressed && styles.buttonPressed,
                      ]}
                    >
                      <View style={styles.bleCandidateHeading}>
                        <Text numberOfLines={1} style={styles.bleCandidateName}>
                          {bleCandidateName(candidate)}
                        </Text>
                        <Text style={styles.bleCandidateChoice}>
                          {knownProfile === null ? (selected ? "Selected" : "Select") : "Switch"}
                        </Text>
                      </View>
                      <Text selectable style={styles.monospace}>
                        {bleCandidateDetails(candidate)}
                      </Text>
                    </Pressable>
                  );
                })}
              </ScrollView>
            )}
            {selectedCandidate === null ? null : (
              <Text accessibilityLiveRegion="polite" style={styles.bleSelectionStatus}>
                {lifecycle?.state === "working"
                  ? "Secure pairing is using this exact selected BLE peripheral."
                  : "Selected for the upcoming secure pairing step. No connection has been made."}
              </Text>
            )}
          </View>
        ) : null}
        <View style={styles.actionRow}>
          {presentation.canStart ? (
            <ActionButton
              disabled={busy || bleScanning || (discovery.available && selectedCandidate === null)}
              label={presentation.startLabel}
              onPress={() => onMutation("start", selectedCandidate)}
              secondary={discovery.available}
            />
          ) : null}
          {canRetryBle ? (
            <ActionButton
              disabled={busy || bleScanning || selectedCandidate === null}
              label="Retry secure pairing"
              onPress={() => onMutation("start", selectedCandidate)}
            />
          ) : null}
          {canContinueBle ? (
            <ActionButton
              disabled={busy}
              label={BLE_SECURITY_CONTINUE_LABEL}
              onPress={() => onMutation("continue", selectedCandidate)}
            />
          ) : null}
          {presentation.canResume ? (
            <ActionButton
              disabled={busy || bleScanning || (discovery.available && selectedCandidate === null)}
              label="Resume pairing"
              onPress={() => onMutation("resume_known_pending", selectedCandidate)}
            />
          ) : null}
          {presentation.canAbort ? (
            <ActionButton
              disabled={busy || bleScanning || (discovery.available && selectedCandidate === null)}
              label="Abort pending state"
              onPress={() => onMutation("abort_orphan", selectedCandidate)}
              secondary
            />
          ) : null}
          {presentation.canRefresh ? (
            <ActionButton
              disabled={busy || bleScanning}
              label="Recheck local state"
              onPress={() => onMutation("refresh", selectedCandidate)}
              secondary
            />
          ) : null}
          {canCancelBle ? (
            <ActionButton
              disabled={cancelling}
              label={
                cancelling
                  ? "Cancelling…"
                  : addingAppliance
                    ? "Cancel adding appliance"
                    : "Cancel secure pairing"
              }
              onPress={() => void cancelBleOnboarding()}
              secondary
            />
          ) : null}
        </View>
      </View>
    </ScrollView>
  );
}
