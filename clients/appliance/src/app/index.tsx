import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  AppState,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  useWindowDimensions,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import type {
  ApplianceSnapshot,
  BytesView,
  ConnectionState,
  ConnectionTransport,
  ContactView,
  OnboardingView,
  RecoveryRequest,
  SendRequest,
  TimelineView,
} from "../generated/api.ts";
import {
  MAX_CONTACT_NAME_BYTES,
  MAX_LXMF_BASIC_CONTENT_BYTES,
  MAX_LXMF_BASIC_TITLE_BYTES,
} from "../generated/api.ts";
import { ApplianceApi } from "../lib/api";
import type { BleCandidate, BleScanOptions } from "../lib/ble-central-types.ts";
import { type DraftIdentity, ensureDraftIdentity } from "../lib/draft.ts";
import {
  ForegroundReconnect,
  foregroundReconnectMessage,
  type ForegroundReconnectProgress,
} from "../lib/foreground-reconnect.ts";
import { keyboardLayoutPolicy } from "../lib/keyboard-layout.ts";
import { LatestRequest } from "../lib/latest-request.ts";
import { byteLimitError, utf8ByteLength } from "../lib/limits.ts";
import { readNativeCoreStatus } from "../lib/native-core";
import type { NativeCoreStatus } from "../lib/native-core-types.ts";
import {
  associatedNomadDestinationForLxmf,
  type NearbyPeerView,
  nearbyPeerFingerprint,
  nearbyPeerRouteHint,
  nearbyPeerSuggestedName,
} from "../lib/nearby-peers.ts";
import {
  DEFAULT_NOMAD_PAGE_PATH,
  NOMAD_PRESENTATION_TIMEOUT_MS,
  NomadBrowserController,
  type NomadBrowserState,
  nomadDestinationHintApplication,
  nomadFetchInputError,
  nomadRequestProvenance,
} from "../lib/nomad-browser.ts";
import {
  BLE_SECURITY_CONTINUE_LABEL,
  bleCandidateDetails,
  bleCandidateName,
  bleDiscoveryPresentation,
  onboardingPresentation,
  selectedBleCandidate,
} from "../lib/onboarding.ts";
import { randomHex } from "../lib/random.ts";

const EMPTY_ONBOARDING: OnboardingView = { available: false, method: null, snapshot: null };
const ONBOARDING_BLE_SCAN_TIMEOUT_MS = 5_000;
const FOREGROUND_RECONNECT_DELAY_MS = 2_000;
const KEYBOARD_LAYOUT = keyboardLayoutPolicy(Platform.OS);
type Workspace = "lxmf" | "nomad";

function connectionLabel(connection: ConnectionState | undefined): string {
  return connection?.state.replaceAll("_", " ") ?? "starting";
}

function transportLabel(transport: ConnectionTransport): string {
  return typeof transport === "string"
    ? transport.replaceAll("_", " ")
    : transport.other.replaceAll("_", " ");
}

function bytesText(field: BytesView): string {
  return field.encoding === "utf8" ? field.value : `hex:${field.value}`;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface ActionButtonProps {
  readonly disabled?: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly secondary?: boolean;
}

function ActionButton({ disabled = false, label, onPress, secondary = false }: ActionButtonProps) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        secondary && styles.buttonSecondary,
        disabled && styles.buttonDisabled,
        pressed && !disabled && styles.buttonPressed,
      ]}
    >
      <Text style={[styles.buttonText, secondary && styles.buttonSecondaryText]}>{label}</Text>
    </Pressable>
  );
}

function nomadPhaseLabel(phase: string | null): string {
  switch (phase) {
    case null:
      return "Accepted; waiting for the first device update";
    case "path_lookup":
      return "Looking up a Reticulum path";
    case "link_establishment":
      return "Establishing a Reticulum Link";
    case "request_preparation":
      return "Preparing the anonymous page request";
    case "awaiting_dispatch_confirmation":
      return "Waiting for radio dispatch confirmation";
    case "awaiting_response":
      return "Waiting for the remote page";
    default:
      return phase.replaceAll("_", " ");
  }
}

function nomadFailureLabel(failure: string): string {
  switch (failure) {
    case "no_path":
      return "No usable Reticulum path was found.";
    case "link":
      return "The Reticulum Link could not be established or retained.";
    case "request":
      return "The page request could not be prepared, sent, or processed.";
    case "timeout":
      return "The remote node did not answer before its request timeout.";
    case "page_too_large":
      return "The page exceeds this appliance profile's bounded response size.";
    case "invalid_utf8":
      return "The remote response is not valid UTF-8.";
    case "internal":
      return "The appliance stopped this fetch after an internal invariant or backend failure.";
    default:
      return failure.replaceAll("_", " ");
  }
}

function nomadInitialInput(state: NomadBrowserState): {
  readonly destination: string;
  readonly path: string;
} {
  if ("request" in state) {
    return { destination: state.request.destination, path: state.request.path };
  }
  if (state.status === "input_error") {
    return { destination: state.destination, path: state.path };
  }
  return { destination: "", path: DEFAULT_NOMAD_PAGE_PATH };
}

interface NomadPanelProps {
  readonly connected: boolean;
  readonly controller: NomadBrowserController;
  readonly destinationHint: string | null;
  readonly onDestinationHintConsumed: () => void;
  readonly state: NomadBrowserState;
}

function NomadPanel({
  connected,
  controller,
  destinationHint,
  onDestinationHintConsumed,
  state,
}: NomadPanelProps) {
  const slotOwned =
    state.status === "starting" ||
    state.status === "pending" ||
    state.status === "poll_error" ||
    state.status === "timed_out";
  const initial = nomadInitialInput(state);
  const [destination, setDestination] = useState(
    slotOwned ? initial.destination : (destinationHint ?? initial.destination),
  );
  const [path, setPath] = useState(initial.path);
  const [formError, setFormError] = useState<string | null>(null);
  const fetchLabel =
    state.status === "starting" || state.status === "pending"
      ? "Fetching…"
      : state.status === "poll_error" || state.status === "timed_out"
        ? "Resume current fetch"
        : "Fetch page";
  const fetchId = "id" in state ? state.id : null;
  const requestProvenance = nomadRequestProvenance(state);

  useEffect(() => {
    const application = nomadDestinationHintApplication(destination, destinationHint, slotOwned);
    if (!application.consumed) return;
    setDestination(application.destination);
    setFormError(null);
    onDestinationHintConsumed();
  }, [destination, destinationHint, onDestinationHintConsumed, slotOwned]);

  const fetchPage = () => {
    const normalizedDestination = destination.trim().toLowerCase();
    const validationError = nomadFetchInputError(normalizedDestination, path);
    setFormError(validationError);
    if (validationError === null) void controller.start(normalizedDestination, path);
  };

  const result = (() => {
    switch (state.status) {
      case "idle":
      case "input_error":
        return (
          <Text style={styles.nomadHint}>
            Enter the peer&apos;s distinct Nomad node destination, or choose Browse beside an
            authenticated nearby peer. Its LXMF/contact destination is a different address.
          </Text>
        );
      case "starting":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadStatus}>
            <ActivityIndicator color="#91e6a7" />
            <Text style={styles.secondaryText}>Submitting the bounded page request…</Text>
          </View>
        );
      case "start_error":
        return (
          <View accessibilityLiveRegion="assertive" style={styles.nomadResult}>
            <Text style={styles.inlineError}>{state.error}</Text>
            <Text style={styles.nomadHint}>
              The appliance may have accepted the request. Retry preserves its exact timestamp and
              idempotency key.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={!connected}
                label="Retry same start"
                onPress={() => void controller.retryStart()}
                secondary
              />
            </View>
          </View>
        );
      case "pending":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadStatus}>
            <ActivityIndicator color="#91e6a7" />
            <View style={styles.nomadStatusCopy}>
              <Text style={styles.nomadResultTitle}>Fetching page</Text>
              <Text style={styles.secondaryText}>{nomadPhaseLabel(state.phase)}</Text>
            </View>
          </View>
        );
      case "poll_error":
        return (
          <View accessibilityLiveRegion="assertive" style={styles.nomadResult}>
            <Text style={styles.inlineError}>{state.error}</Text>
            <Text style={styles.nomadHint}>
              The fetch ID is retained. Resume after reconnecting. If the board rebooted and made
              this boot-scoped ID stale, explicitly abandon it before starting another fetch.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={!connected}
                label="Resume polling"
                onPress={() => void controller.resumePolling()}
                secondary
              />
              <ActionButton
                label="Abandon fetch ID"
                onPress={() => controller.abandonRetainedFetch()}
                secondary
              />
            </View>
          </View>
        );
      case "timed_out":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadResult}>
            <Text style={styles.nomadResultTitle}>Still pending</Text>
            <Text style={styles.secondaryText}>
              Local polling paused after {NOMAD_PRESENTATION_TIMEOUT_MS / 1_000} seconds. The
              boot-scoped fetch ID is retained. Resume it, or explicitly abandon it after a board
              reset to permit a fresh request.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={!connected}
                label="Resume polling"
                onPress={() => void controller.resumePolling()}
                secondary
              />
              <ActionButton
                label="Abandon fetch ID"
                onPress={() => controller.abandonRetainedFetch()}
                secondary
              />
            </View>
          </View>
        );
      case "failed":
        return (
          <View accessibilityLiveRegion="assertive" style={styles.nomadResult}>
            <Text style={styles.nomadResultTitle}>Fetch failed</Text>
            <Text style={styles.inlineError}>{nomadFailureLabel(state.failure)}</Text>
          </View>
        );
      case "ready":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadResult}>
            <Text style={styles.nomadResultTitle}>Raw Micron page</Text>
            <Text style={styles.nomadHint}>
              Rendering is deliberately deferred; this first browser surface shows the exact bounded
              UTF-8 response.
            </Text>
            <View style={styles.nomadRawPage}>
              <Text selectable style={styles.nomadRawText}>
                {state.page}
              </Text>
            </View>
          </View>
        );
    }
  })();

  const contents = (
    <View style={styles.nomadCard}>
      <View style={styles.nomadHeading}>
        <View style={styles.nomadHeadingCopy}>
          <Text style={styles.eyebrow}>NOMADNET</Text>
          <Text style={styles.heading}>Browse a bounded page</Text>
        </View>
        <View style={[styles.pill, connected && styles.pillReady]}>
          <Text style={styles.pillText}>{connected ? "appliance ready" : "disconnected"}</Text>
        </View>
      </View>
      <Text style={styles.label}>Nomad node destination</Text>
      <TextInput
        accessibilityLabel="Nomad node destination"
        autoCapitalize="none"
        autoCorrect={false}
        editable={!slotOwned}
        maxLength={32}
        onChangeText={setDestination}
        placeholder="32 hexadecimal characters"
        placeholderTextColor="#748078"
        style={[styles.input, styles.monospaceInput]}
        value={destination}
      />
      <Text style={styles.nomadHint}>
        Use the distinct Nomad node destination. An LXMF/contact destination will not resolve here.
      </Text>
      <Text style={styles.label}>Page path</Text>
      <TextInput
        accessibilityLabel="Nomad page path"
        autoCapitalize="none"
        autoCorrect={false}
        editable={!slotOwned}
        onChangeText={setPath}
        placeholder={DEFAULT_NOMAD_PAGE_PATH}
        placeholderTextColor="#748078"
        style={[styles.input, styles.monospaceInput]}
        value={path}
      />
      {formError === null ? null : <Text style={styles.inlineError}>{formError}</Text>}
      <View style={styles.nomadFetchRow}>
        <ActionButton disabled={!connected || slotOwned} label={fetchLabel} onPress={fetchPage} />
        <Text style={styles.nomadHint}>
          Anonymous request · one complete page · no Resource yet
        </Text>
      </View>
      {fetchId === null && requestProvenance === null ? null : (
        <View style={styles.nomadFetchMeta}>
          {fetchId === null ? null : (
            <>
              <Text style={styles.metaLabel}>Fetch ID</Text>
              <Text selectable style={styles.monospace}>
                {fetchId}
              </Text>
            </>
          )}
          {"outcome" in state ? (
            <Text style={styles.nomadHint}>Start {state.outcome.replaceAll("_", " ")}</Text>
          ) : null}
          {requestProvenance === null ? null : (
            <>
              <Text style={styles.metaLabel}>Request destination</Text>
              <Text selectable style={styles.monospace}>
                {requestProvenance.destination}
              </Text>
              <Text style={styles.metaLabel}>Request path</Text>
              <Text selectable style={styles.monospace}>
                {requestProvenance.path}
              </Text>
            </>
          )}
        </View>
      )}
      {result}
    </View>
  );

  return (
    <ScrollView
      automaticallyAdjustKeyboardInsets={Platform.OS === "ios"}
      contentContainerStyle={styles.nomadContent}
      keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
      keyboardShouldPersistTaps="handled"
      style={styles.nomadScroller}
    >
      {contents}
    </ScrollView>
  );
}

interface OnboardingPanelProps {
  readonly busy: boolean;
  readonly onboarding: OnboardingView;
  readonly onCancel: (() => Promise<void>) | null;
  readonly onMutation: (
    path: "start" | "continue" | "refresh" | RecoveryRequest["action"],
    candidate: BleCandidate | null,
  ) => void;
  readonly onScanBleCandidates:
    | ((options?: BleScanOptions) => Promise<readonly BleCandidate[]>)
    | null;
}

function OnboardingPanel({
  busy,
  onboarding,
  onCancel,
  onMutation,
  onScanBleCandidates,
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
    lifecycle?.state === "working" &&
    lifecycle.stage !== "activating";

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
        <Text style={styles.eyebrow}>FIRST-RUN SETUP</Text>
        <Text style={styles.onboardingTitle}>{presentation.title}</Text>
        <Text style={styles.secondaryText}>{presentation.instruction}</Text>
        {discovery.available || presentation.identifierLabel === null ? null : (
          <View style={styles.serialRow}>
            <Text style={styles.metaLabel}>{presentation.identifierLabel}</Text>
            <Text selectable style={styles.monospace}>
              {onboarding.snapshot?.usb_serial ?? "—"}
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
                  return (
                    <Pressable
                      accessibilityLabel={`Select ${bleCandidateName(candidate)}`}
                      accessibilityRole="button"
                      accessibilityState={{ selected }}
                      disabled={busy || lifecycle?.state === "working"}
                      key={candidate.peripheralId}
                      onPress={() => setSelectedPeripheralId(candidate.peripheralId)}
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
                          {selected ? "Selected" : "Select"}
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
              label={cancelling ? "Cancelling…" : "Cancel secure pairing"}
              onPress={() => void cancelBleOnboarding()}
              secondary
            />
          ) : null}
        </View>
      </View>
    </ScrollView>
  );
}

interface NearbyPanelProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly connected: boolean;
  readonly contacts: ContactView[];
  readonly loadError: string | null;
  readonly loaded: boolean;
  readonly loading: boolean;
  readonly onBrowseNomad: (destination: string) => void;
  readonly onRefresh: (() => Promise<void>) | null;
  readonly onSelect: (destination: string) => void;
  readonly onUpsert: (destination: string, name: string) => Promise<boolean>;
  readonly peers: readonly NearbyPeerView[];
}

function NearbyPanel({
  busy,
  compact,
  connected,
  contacts,
  loadError,
  loaded,
  loading,
  onBrowseNomad,
  onRefresh,
  onSelect,
  onUpsert,
  peers,
}: NearbyPanelProps) {
  const [addingDestination, setAddingDestination] = useState<string | null>(null);

  const choosePeer = async (peer: NearbyPeerView, alreadyAdded: boolean) => {
    if (alreadyAdded) {
      onSelect(peer.destination);
      return;
    }
    setAddingDestination(peer.destination);
    try {
      await onUpsert(peer.destination, nearbyPeerSuggestedName(peer));
    } finally {
      setAddingDestination(null);
    }
  };

  const peerRows = peers.map((peer) => {
    const existing = contacts.some((contact) => contact.destination === peer.destination);
    const adding = addingDestination === peer.destination;
    return (
      <View
        key={peer.destination}
        style={[
          styles.nearbyPeer,
          existing && styles.nearbyPeerAdded,
          busy && styles.buttonDisabled,
        ]}
      >
        <View style={styles.nearbyPeerHeading}>
          <Text numberOfLines={1} style={styles.nearbyPeerName}>
            {nearbyPeerSuggestedName(peer)}
          </Text>
          <View style={styles.nearbyPeerButtons}>
            <Pressable
              accessibilityHint="Opens this peer's associated Nomad node in the browser"
              accessibilityLabel={`Browse ${nearbyPeerSuggestedName(peer)} on NomadNet`}
              accessibilityRole="button"
              disabled={busy}
              onPress={() => onBrowseNomad(peer.associated_nomad_destination)}
              style={({ pressed }) => [
                styles.nearbyPeerButton,
                pressed && !busy && styles.contactPressed,
              ]}
            >
              <Text style={styles.nearbyPeerAction}>Browse</Text>
            </Pressable>
            <Pressable
              accessibilityHint={
                existing
                  ? "Opens the existing conversation"
                  : "Adds this authenticated peer as a contact"
              }
              accessibilityLabel={`${existing ? "Open" : "Add"} ${nearbyPeerSuggestedName(peer)}`}
              accessibilityRole="button"
              disabled={busy}
              onPress={() => void choosePeer(peer, existing)}
              style={({ pressed }) => [
                styles.nearbyPeerButton,
                pressed && !busy && styles.contactPressed,
              ]}
            >
              <Text style={styles.nearbyPeerAction}>
                {adding ? "Adding…" : existing ? "Open" : "Add"}
              </Text>
            </Pressable>
          </View>
        </View>
        <Text style={styles.nearbyStatus}>{nearbyPeerRouteHint(peer)}</Text>
        <Text selectable style={styles.monospace}>
          ID {nearbyPeerFingerprint(peer)}
        </Text>
      </View>
    );
  });

  return (
    <View style={styles.nearbyPanel}>
      <View style={styles.nearbyHeading}>
        <View style={styles.nearbyTitle}>
          <Text style={styles.contactName}>Nearby</Text>
          <Text style={styles.nearbyCaption}>Authenticated LXMF announces</Text>
        </View>
        <Pressable
          accessibilityLabel="Refresh nearby peers"
          accessibilityRole="button"
          disabled={busy || loading || !connected || onRefresh === null}
          onPress={() => void onRefresh?.()}
          style={({ pressed }) => [
            styles.smallButton,
            (busy || loading || !connected || onRefresh === null) && styles.buttonDisabled,
            pressed && styles.buttonPressed,
          ]}
        >
          <Text style={styles.smallButtonText}>{loading ? "Scanning…" : "Refresh"}</Text>
        </Pressable>
      </View>
      {onRefresh === null ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          Nearby discovery is not included in this app build yet.
        </Text>
      ) : !connected ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          Connect to the appliance to read peers it has heard.
        </Text>
      ) : loading && !loaded ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          Reading authenticated announces from the appliance…
        </Text>
      ) : loadError !== null ? (
        <View accessibilityLiveRegion="assertive" style={styles.nearbyError}>
          <Text style={styles.inlineError}>{loadError}</Text>
          <Text style={styles.nearbyStatus}>Tap Refresh to try again.</Text>
        </View>
      ) : loaded && peers.length === 0 ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          No LXMF peers heard yet. Leave both nodes powered, then refresh.
        </Text>
      ) : compact ? (
        <View style={styles.nearbyList}>{peerRows}</View>
      ) : (
        <ScrollView
          contentContainerStyle={styles.nearbyList}
          keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
          keyboardShouldPersistTaps="handled"
          nestedScrollEnabled
          style={styles.nearbyScroller}
        >
          {peerRows}
        </ScrollView>
      )}
    </View>
  );
}

interface SidebarProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly contacts: ContactView[];
  readonly onBrowseNomad: (destination: string) => void;
  readonly onRefreshNearby: (() => Promise<NearbyPeerView[]>) | null;
  readonly onSelect: (destination: string) => void;
  readonly onUpsert: (destination: string, name: string) => Promise<boolean>;
  readonly selected: string | null;
  readonly snapshot: ApplianceSnapshot | null;
}

function Sidebar({
  busy,
  compact,
  contacts,
  onBrowseNomad,
  onRefreshNearby,
  onSelect,
  onUpsert,
  selected,
  snapshot,
}: SidebarProps) {
  const [showForm, setShowForm] = useState(false);
  const [showNearby, setShowNearby] = useState(false);
  const [name, setName] = useState("");
  const [destination, setDestination] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [nearbyPeers, setNearbyPeers] = useState<NearbyPeerView[]>([]);
  const [nearbyLoadError, setNearbyLoadError] = useState<string | null>(null);
  const [nearbyLoaded, setNearbyLoaded] = useState(false);
  const [nearbyLoading, setNearbyLoading] = useState(false);
  const nearbyRequests = useRef(new LatestRequest());
  const readyConnection = snapshot?.connection.state === "ready" ? snapshot.connection : undefined;
  const nearbyConnectionKey =
    readyConnection === undefined
      ? null
      : [
          snapshot?.device?.device_id ?? "",
          transportLabel(readyConnection.transport),
          readyConnection.endpoint,
          readyConnection.device_label,
        ].join("\u0000");
  const nearbyConnectionKeyRef = useRef(nearbyConnectionKey);
  nearbyConnectionKeyRef.current = nearbyConnectionKey;

  const refreshNearby = useCallback(async () => {
    const source = nearbyConnectionKey;
    if (onRefreshNearby === null || source === null) return;

    const request = nearbyRequests.current.begin();
    setNearbyLoading(true);
    setNearbyLoadError(null);
    try {
      const discovered = await onRefreshNearby();
      if (!nearbyRequests.current.accepts(request) || nearbyConnectionKeyRef.current !== source) {
        return;
      }
      setNearbyPeers(discovered);
      setNearbyLoaded(true);
    } catch (nextError) {
      if (!nearbyRequests.current.accepts(request) || nearbyConnectionKeyRef.current !== source) {
        return;
      }
      setNearbyLoadError(errorText(nextError));
      setNearbyLoaded(true);
    } finally {
      if (nearbyRequests.current.accepts(request) && nearbyConnectionKeyRef.current === source) {
        setNearbyLoading(false);
      }
    }
  }, [nearbyConnectionKey, onRefreshNearby]);

  useEffect(() => {
    nearbyRequests.current.invalidate();
    setNearbyPeers([]);
    setNearbyLoadError(null);
    setNearbyLoaded(false);
    setNearbyLoading(false);
    if (nearbyConnectionKey !== null && onRefreshNearby !== null) void refreshNearby();

    return () => nearbyRequests.current.invalidate();
  }, [nearbyConnectionKey, onRefreshNearby, refreshNearby]);

  const save = async () => {
    const normalizedDestination = destination.trim().toLowerCase();
    const nameError = byteLimitError(name, MAX_CONTACT_NAME_BYTES, "Name");
    if (nameError !== null) {
      setFormError(nameError);
      return;
    }
    if (name.trim().length === 0) {
      setFormError("Name is required");
      return;
    }
    if (!/^[0-9a-f]{32}$/.test(normalizedDestination)) {
      setFormError("LXMF destination must be exactly 32 hexadecimal characters");
      return;
    }
    setFormError(null);
    if (!(await onUpsert(normalizedDestination, name))) return;
    setName("");
    setDestination("");
    setShowForm(false);
  };

  const contactRows = contacts.map((contact) => {
    const nomadDestination = associatedNomadDestinationForLxmf(nearbyPeers, contact.destination);
    const displayName = contact.name || "Unnamed contact";
    return (
      <View
        key={contact.destination}
        style={[styles.contact, selected === contact.destination && styles.contactActive]}
      >
        <Pressable
          accessibilityLabel={`Open ${displayName}`}
          accessibilityRole="button"
          onPress={() => onSelect(contact.destination)}
          style={({ pressed }) => [styles.contactSelection, pressed && styles.contactPressed]}
        >
          <Text numberOfLines={1} style={styles.contactName}>
            {displayName}
          </Text>
          <Text selectable style={styles.monospace}>
            {contact.destination}
          </Text>
        </Pressable>
        {nomadDestination === null ? null : (
          <Pressable
            accessibilityHint="Uses the distinct Nomad destination authenticated in this peer's nearby announce"
            accessibilityLabel={`Browse ${displayName} on NomadNet`}
            accessibilityRole="button"
            disabled={busy}
            onPress={() => onBrowseNomad(nomadDestination)}
            style={({ pressed }) => [
              styles.nearbyPeerButton,
              styles.contactBrowseButton,
              busy && styles.buttonDisabled,
              pressed && !busy && styles.contactPressed,
            ]}
          >
            <Text style={styles.nearbyPeerAction}>Browse</Text>
          </Pressable>
        )}
      </View>
    );
  });

  const sidebarContents = (
    <>
      <View style={styles.sectionHeading}>
        <Text style={styles.heading}>Contacts</Text>
        <View style={styles.sectionActions}>
          <Pressable
            accessibilityLabel={showNearby ? "Hide nearby peers" : "Find nearby peers"}
            accessibilityRole="button"
            onPress={() => {
              setShowForm(false);
              setShowNearby((visible) => !visible);
            }}
            style={[styles.smallButton, showNearby && styles.smallButtonActive]}
          >
            <Text style={styles.smallButtonText}>Nearby</Text>
          </Pressable>
          <Pressable
            accessibilityLabel="Add contact manually"
            accessibilityRole="button"
            onPress={() => {
              setShowNearby(false);
              setShowForm(true);
            }}
            style={styles.addButton}
          >
            <Text style={styles.addButtonText}>+</Text>
          </Pressable>
        </View>
      </View>
      {showNearby ? (
        <NearbyPanel
          busy={busy}
          compact={compact}
          connected={readyConnection !== undefined}
          contacts={contacts}
          loadError={nearbyLoadError}
          loaded={nearbyLoaded}
          loading={nearbyLoading}
          onBrowseNomad={onBrowseNomad}
          onRefresh={onRefreshNearby === null ? null : refreshNearby}
          onSelect={onSelect}
          onUpsert={onUpsert}
          peers={nearbyPeers}
        />
      ) : null}
      {showForm ? (
        <View style={styles.contactForm}>
          <Text style={styles.label}>Name</Text>
          <TextInput
            accessibilityLabel="Contact name"
            autoCapitalize="none"
            editable={!busy}
            onChangeText={setName}
            style={styles.input}
            value={name}
          />
          <Text style={styles.label}>LXMF destination</Text>
          <TextInput
            accessibilityLabel="LXMF destination"
            autoCapitalize="none"
            autoCorrect={false}
            editable={!busy}
            maxLength={32}
            onChangeText={setDestination}
            style={[styles.input, styles.monospaceInput]}
            value={destination}
          />
          {formError === null ? null : <Text style={styles.inlineError}>{formError}</Text>}
          <View style={styles.actionRow}>
            <ActionButton disabled={busy} label="Save" onPress={() => void save()} />
            <ActionButton label="Cancel" onPress={() => setShowForm(false)} secondary />
          </View>
        </View>
      ) : null}
      {compact ? (
        <View style={styles.contacts}>{contactRows}</View>
      ) : (
        <ScrollView
          contentContainerStyle={styles.contacts}
          keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
          keyboardShouldPersistTaps="handled"
        >
          {contactRows}
        </ScrollView>
      )}
      <View style={styles.deviceMeta}>
        <MetaRow label="Connection" value={connectionLabel(snapshot?.connection)} />
        <MetaRow
          label="Transport"
          value={readyConnection === undefined ? "—" : transportLabel(readyConnection.transport)}
        />
        <MetaRow label="Endpoint" value={readyConnection?.endpoint ?? "—"} />
        <MetaRow label="Device" value={readyConnection?.device_label ?? "—"} />
        <MetaRow label="Pending" value={String(snapshot?.pending_outbox ?? 0)} />
        <MetaRow label="Imported" value={String(snapshot?.imported_this_run ?? 0)} />
        <MetaRow label="Local LXMF" value={snapshot?.device?.lxmf_delivery_destination ?? "—"} />
      </View>
    </>
  );

  if (compact) {
    return (
      <ScrollView
        contentContainerStyle={styles.sidebarCompactContent}
        keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
        keyboardShouldPersistTaps="handled"
        nestedScrollEnabled
        style={styles.sidebarCompactScroller}
      >
        {sidebarContents}
      </ScrollView>
    );
  }

  return <View style={styles.sidebar}>{sidebarContents}</View>;
}

function MetaRow({ label, value }: { readonly label: string; readonly value: string }) {
  return (
    <View style={styles.metaRow}>
      <Text style={styles.metaLabel}>{label}</Text>
      <Text selectable style={styles.metaValue}>
        {value}
      </Text>
    </View>
  );
}

interface ConversationProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly contact: ContactView | undefined;
  readonly onDraftChanged: () => void;
  readonly onSend: (title: string, content: string) => Promise<boolean>;
  readonly timeline: TimelineView[];
}

function Conversation({
  busy,
  compact,
  contact,
  onDraftChanged,
  onSend,
  timeline,
}: ConversationProps) {
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);
  const draftVersion = useRef(0);

  if (contact === undefined) {
    return (
      <View style={[styles.emptyState, compact && styles.conversationCompact]}>
        <Text style={styles.emptyTitle}>Select or add a contact to begin.</Text>
        <Text style={styles.secondaryText}>
          The node continues receiving and routing while this app is closed.
        </Text>
      </View>
    );
  }

  const titleBytes = utf8ByteLength(title);
  const contentBytes = utf8ByteLength(content);
  const send = async () => {
    const error =
      byteLimitError(title, MAX_LXMF_BASIC_TITLE_BYTES, "Title") ??
      byteLimitError(content, MAX_LXMF_BASIC_CONTENT_BYTES, "Message") ??
      (content.length === 0 ? "Message is required" : null);
    setValidationError(error);
    if (error !== null) return;
    const submittedVersion = draftVersion.current;
    if (!(await onSend(title, content))) return;
    if (draftVersion.current === submittedVersion) {
      setTitle("");
      setContent("");
    }
  };

  return (
    <View style={[styles.conversation, compact && styles.conversationCompact]}>
      <View style={styles.conversationHeading}>
        <Text style={styles.heading}>{contact.name || "Unnamed contact"}</Text>
        <Text selectable style={styles.monospace}>
          {contact.destination}
        </Text>
      </View>
      <ScrollView
        contentContainerStyle={styles.timeline}
        keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
        keyboardShouldPersistTaps="handled"
        nestedScrollEnabled={compact}
        style={styles.timelineScroller}
      >
        {timeline.map((entry) => (
          <View
            key={`${entry.sequence}:${entry.direction}`}
            style={[
              styles.message,
              entry.direction === "outbound" ? styles.messageOutbound : styles.messageInbound,
            ]}
          >
            <Text style={styles.messageTitle}>{bytesText(entry.title) || "Untitled"}</Text>
            <Text selectable style={styles.messageContent}>
              {bytesText(entry.content)}
            </Text>
            <Text style={styles.messageFooter}>
              {new Date(entry.timestamp_ms).toLocaleString()}
              {entry.status === null ? "" : ` · ${entry.status.replaceAll("_", " ")}`}
            </Text>
          </View>
        ))}
      </ScrollView>
      <View style={styles.compose}>
        <TextInput
          accessibilityLabel="Message title"
          editable={!busy}
          onChangeText={(value) => {
            draftVersion.current += 1;
            setTitle(value);
            onDraftChanged();
          }}
          placeholder="Title (optional)"
          placeholderTextColor="#748078"
          style={styles.input}
          value={title}
        />
        <TextInput
          accessibilityLabel="Message"
          editable={!busy}
          multiline
          onChangeText={(value) => {
            draftVersion.current += 1;
            setContent(value);
            onDraftChanged();
          }}
          placeholder="Message"
          placeholderTextColor="#748078"
          style={[styles.input, styles.messageInput]}
          value={content}
        />
        {validationError === null ? null : (
          <Text style={styles.inlineError}>{validationError}</Text>
        )}
        <View style={styles.composeFooter}>
          <Text style={styles.counter}>
            Title {titleBytes} / {MAX_LXMF_BASIC_TITLE_BYTES} · Message {contentBytes} /{" "}
            {MAX_LXMF_BASIC_CONTENT_BYTES}
          </Text>
          <ActionButton disabled={busy} label="Queue message" onPress={() => void send()} />
        </View>
      </View>
    </View>
  );
}

export default function ApplianceScreen() {
  const api = useMemo(() => new ApplianceApi(), []);
  const nomadBrowser = useMemo(
    () =>
      new NomadBrowserController(api, {
        createIdempotencyKey: () => randomHex(16),
      }),
    [api],
  );
  const { width } = useWindowDimensions();
  const compact = width < 760;
  const [bootstrapped, setBootstrapped] = useState(false);
  const [nativeCore, setNativeCore] = useState<NativeCoreStatus | null>(null);
  const [snapshot, setSnapshot] = useState<ApplianceSnapshot | null>(null);
  const [onboarding, setOnboarding] = useState<OnboardingView>(EMPTY_ONBOARDING);
  const [contacts, setContacts] = useState<ContactView[]>([]);
  const [foreground, setForeground] = useState(
    AppState.currentState === null || AppState.currentState === "active",
  );
  const [reconnectRetry, setReconnectRetry] = useState(0);
  const [reconnectProgress, setReconnectProgress] =
    useState<ForegroundReconnectProgress | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<TimelineView[]>([]);
  const [workspace, setWorkspace] = useState<Workspace>("lxmf");
  const [nomadDestinationHint, setNomadDestinationHint] = useState<string | null>(null);
  const [nomadState, setNomadState] = useState<NomadBrowserState>(nomadBrowser.state);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const draft = useRef<DraftIdentity | null>(null);
  const mutationInFlight = useRef(false);
  const refreshRequests = useRef(new LatestRequest());
  const selectedRef = useRef<string | null>(null);
  const timelineRequests = useRef(new LatestRequest());
  const automaticReconnect = useMemo(
    () =>
      new ForegroundReconnect(
        () => setReconnectRetry((generation) => generation + 1),
        FOREGROUND_RECONNECT_DELAY_MS,
      ),
    [],
  );

  const ready = onboardingPresentation(onboarding).ready;
  // Missing credentials can make the dormant connector report an expected
  // local error. The onboarding panel owns that state until setup is ready.
  const displayedError =
    error ??
    (ready &&
    (reconnectProgress === null || snapshot?.connection.state === "faulted")
      ? snapshot?.last_error
      : null);
  const selectedContact = contacts.find((contact) => contact.destination === selected);
  const nearbyReader = useMemo(() => {
    const read = api.nearbyPeers;
    return read === undefined ? null : () => read.call(api);
  }, [api]);
  const bleCandidateScanner = useMemo(() => {
    if (!bootstrapped) return null;
    const scan = api.scanBleCandidates;
    const supported = api.supportsBleCandidateDiscovery?.() ?? scan !== undefined;
    return scan === undefined || !supported
      ? null
      : (options?: BleScanOptions) => scan.call(api, options);
  }, [api, bootstrapped]);

  useEffect(() => {
    let active = true;
    void readNativeCoreStatus().then((status) => {
      if (active) setNativeCore(status);
    });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => () => api.dispose(), [api]);

  useEffect(() => {
    const unsubscribe = nomadBrowser.subscribe(setNomadState);
    return () => {
      unsubscribe();
      nomadBrowser.dispose();
    };
  }, [nomadBrowser]);

  useEffect(() => {
    const subscription = AppState.addEventListener("change", (state) => {
      setForeground(state === "active");
    });
    return () => subscription.remove();
  }, []);

  useEffect(() => () => automaticReconnect.suspend(), [automaticReconnect]);

  const refresh = useCallback(async () => {
    const refreshRequest = refreshRequests.current.begin();
    try {
      const [nextSnapshot, nextOnboarding] = await Promise.all([api.snapshot(), api.onboarding()]);
      if (!refreshRequests.current.accepts(refreshRequest)) return;
      setSnapshot(nextSnapshot);
      setOnboarding(nextOnboarding);
      const nextReady = onboardingPresentation(nextOnboarding).ready;
      if (!nextReady) {
        timelineRequests.current.invalidate();
        return;
      }
      const nextContacts = await api.contacts();
      if (!refreshRequests.current.accepts(refreshRequest)) return;
      setContacts(nextContacts);
      const selectedDestination = selectedRef.current;
      if (selectedDestination !== null) {
        if (nextContacts.some((contact) => contact.destination === selectedDestination)) {
          const timelineRequest = timelineRequests.current.begin();
          let nextTimeline: TimelineView[];
          try {
            nextTimeline = await api.timeline(selectedDestination);
          } catch (nextError) {
            if (
              refreshRequests.current.accepts(refreshRequest) &&
              timelineRequests.current.accepts(timelineRequest) &&
              selectedRef.current === selectedDestination
            ) {
              throw nextError;
            }
            return;
          }
          if (
            !refreshRequests.current.accepts(refreshRequest) ||
            !timelineRequests.current.accepts(timelineRequest) ||
            selectedRef.current !== selectedDestination
          ) {
            return;
          }
          setTimeline(nextTimeline);
        } else {
          timelineRequests.current.invalidate();
          selectedRef.current = null;
          setSelected(null);
          setTimeline([]);
        }
      }
      setError(null);
    } catch (nextError) {
      if (refreshRequests.current.accepts(refreshRequest)) throw nextError;
    }
  }, [api]);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        await api.bootstrapSession();
        if (!active) return;
        setBootstrapped(true);
        await refresh();
      } catch (nextError) {
        if (active) setError(errorText(nextError));
      }
    })();
    return () => {
      active = false;
    };
  }, [api, refresh]);

  useEffect(() => {
    if (!bootstrapped) return;
    const unsubscribe = api.subscribeInvalidations(
      () => void refresh().catch((nextError) => setError(errorText(nextError))),
      () => setError("Event stream reconnecting"),
    );
    const interval =
      !ready || unsubscribe === null
        ? setInterval(
            () => void refresh().catch((nextError) => setError(errorText(nextError))),
            ready ? 2_000 : 500,
          )
        : null;
    return () => {
      unsubscribe?.();
      if (interval !== null) clearInterval(interval);
    };
  }, [api, bootstrapped, ready, refresh]);

  useEffect(() => {
    if (!foreground || !onboarding.available || !ready || snapshot?.connection.state === "ready") {
      automaticReconnect.suspend();
      setReconnectProgress(null);
      return;
    }
    if (!automaticReconnect.begin(reconnectRetry)) return;

    let active = true;
    setReconnectProgress({ state: "attempting" });
    void api
      .reconnect()
      .then(refresh)
      .then(() => {
        if (active) setReconnectProgress(null);
      })
      .catch((nextError) => {
        if (active) {
          setReconnectProgress({
            state: "waiting_retry",
            reason: errorText(nextError),
          });
        }
      })
      .finally(() => automaticReconnect.settle());
    return () => {
      active = false;
    };
  }, [
    api,
    automaticReconnect,
    foreground,
    onboarding.available,
    ready,
    reconnectRetry,
    refresh,
    snapshot?.connection.state,
  ]);

  const run = async (operation: () => Promise<unknown>): Promise<boolean> => {
    if (mutationInFlight.current) return false;
    mutationInFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      try {
        await operation();
      } catch (nextError) {
        setError(errorText(nextError));
        return false;
      }
      try {
        await refresh();
      } catch (nextError) {
        setError(`Action completed, but refreshing the display failed: ${errorText(nextError)}`);
      }
      return true;
    } finally {
      mutationInFlight.current = false;
      setBusy(false);
    }
  };

  const onboardingMutation = (
    action: "start" | "continue" | "refresh" | RecoveryRequest["action"],
    candidate: BleCandidate | null,
  ) => {
    void run(() => {
      if (action === "start") return api.startOnboarding(candidate ?? undefined);
      if (action === "continue") {
        if (api.continueOnboarding === undefined) {
          throw new Error("This client cannot continue a retained BLE pairing ceremony.");
        }
        return api.continueOnboarding();
      }
      if (action === "refresh") return api.refreshOnboarding();
      return api.recoverOnboarding({ action }, candidate ?? undefined);
    });
  };

  const cancelOnboarding =
    api.cancelOnboarding === undefined
      ? null
      : async (): Promise<void> => {
          await api.cancelOnboarding?.();
        };

  const upsertContact = (destination: string, name: string): Promise<boolean> =>
    run(async () => {
      await api.upsertContact(destination, { name });
      if (selectedRef.current !== destination) draft.current = null;
      selectedRef.current = destination;
      timelineRequests.current.invalidate();
      setSelected(destination);
      setTimeline([]);
    });

  const send = async (title: string, content: string): Promise<boolean> => {
    if (selected === null) return false;
    return run(async () => {
      const identity = ensureDraftIdentity(draft.current, () => randomHex(16), Date.now);
      draft.current = identity;
      const request: SendRequest = {
        destination: selected,
        timestamp_ms: identity.timestampMs,
        idempotency_key: identity.idempotencyKey,
        title,
        content,
      };
      await api.send(request);
      if (draft.current === identity) draft.current = null;
    });
  };

  const chooseContact = (destination: string) => {
    if (selectedRef.current !== destination) draft.current = null;
    selectedRef.current = destination;
    const timelineRequest = timelineRequests.current.begin();
    setSelected(destination);
    setTimeline([]);
    void api
      .timeline(destination)
      .then((nextTimeline) => {
        if (
          timelineRequests.current.accepts(timelineRequest) &&
          selectedRef.current === destination
        ) {
          setTimeline(nextTimeline);
          setError(null);
        }
      })
      .catch((nextError) => {
        if (
          timelineRequests.current.accepts(timelineRequest) &&
          selectedRef.current === destination
        ) {
          setError(errorText(nextError));
        }
      });
  };

  const browseNomad = useCallback((destination: string) => {
    setNomadDestinationHint(destination);
    setWorkspace("nomad");
  }, []);
  const consumeNomadDestinationHint = useCallback(() => {
    setNomadDestinationHint(null);
  }, []);

  const applianceShell = (
    <View style={[styles.shell, compact && styles.shellCompact]}>
      <Sidebar
        busy={busy}
        compact={compact}
        contacts={contacts}
        onBrowseNomad={browseNomad}
        onRefreshNearby={nearbyReader}
        onSelect={chooseContact}
        onUpsert={upsertContact}
        selected={selected}
        snapshot={snapshot}
      />
      <Conversation
        busy={busy}
        compact={compact}
        contact={selectedContact}
        key={selectedContact?.destination ?? "empty"}
        onDraftChanged={() => {
          draft.current = null;
        }}
        onSend={send}
        timeline={timeline}
      />
    </View>
  );
  const nomadShell = (
    <NomadPanel
      connected={snapshot?.connection.state === "ready"}
      controller={nomadBrowser}
      destinationHint={nomadDestinationHint}
      onDestinationHintConsumed={consumeNomadDestinationHint}
      state={nomadState}
    />
  );

  return (
    <SafeAreaView style={styles.safeArea}>
      <View style={styles.topbar}>
        <View style={styles.brandCluster}>
          <View>
            <Text style={styles.eyebrow}>RETICULUM APPLIANCE</Text>
            <Text style={styles.title}>{workspace === "lxmf" ? "LXMF" : "NomadNet"}</Text>
          </View>
          <View accessibilityRole="tablist" style={styles.workspaceSwitcher}>
            <Pressable
              accessibilityRole="tab"
              accessibilityState={{ selected: workspace === "lxmf" }}
              onPress={() => setWorkspace("lxmf")}
              style={[styles.workspaceTab, workspace === "lxmf" && styles.workspaceTabActive]}
            >
              <Text style={styles.workspaceTabText}>Messages</Text>
            </Pressable>
            <Pressable
              accessibilityRole="tab"
              accessibilityState={{ selected: workspace === "nomad" }}
              onPress={() => setWorkspace("nomad")}
              style={[styles.workspaceTab, workspace === "nomad" && styles.workspaceTabActive]}
            >
              <Text style={styles.workspaceTabText}>Browse</Text>
            </Pressable>
          </View>
        </View>
        <View style={styles.statusCluster}>
          {nativeCore === null ? null : (
            <View
              style={[
                styles.pill,
                nativeCore.state === "ready" ? styles.pillReady : styles.pillFaulted,
              ]}
            >
              <Text style={styles.pillText}>{nativeCore.label}</Text>
            </View>
          )}
          <View
            style={[
              styles.pill,
              snapshot?.connection.state === "ready" && styles.pillReady,
              snapshot?.connection.state === "faulted" && styles.pillFaulted,
            ]}
          >
            <Text style={styles.pillText}>
              {ready ? connectionLabel(snapshot?.connection) : "setup required"}
            </Text>
          </View>
          {compact ? null : (
            <>
              <ActionButton
                disabled={!ready || busy}
                label="Sync"
                onPress={() => void run(() => api.sync())}
                secondary
              />
              <ActionButton
                disabled={!ready || busy}
                label="Reconnect"
                onPress={() => void run(() => api.reconnect())}
                secondary
              />
            </>
          )}
        </View>
      </View>
      {displayedError === null || displayedError === undefined ? null : (
        <View accessibilityLiveRegion="assertive" style={styles.errorBanner}>
          <Text style={styles.errorText}>{displayedError}</Text>
        </View>
      )}
      {reconnectProgress === null ? null : (
        <View accessibilityLiveRegion="polite" style={styles.reconnectBanner}>
          <Text style={styles.reconnectText}>
            {foregroundReconnectMessage(reconnectProgress)}
          </Text>
        </View>
      )}
      {busy ? <ActivityIndicator color="#91e6a7" style={styles.activity} /> : null}
      <OnboardingPanel
        busy={busy}
        onboarding={onboarding}
        onCancel={cancelOnboarding}
        onMutation={onboardingMutation}
        onScanBleCandidates={bleCandidateScanner}
      />
      {ready ? (
        <KeyboardAvoidingView
          behavior={KEYBOARD_LAYOUT.avoidingBehavior}
          enabled={KEYBOARD_LAYOUT.avoidingEnabled}
          style={styles.keyboardAvoiding}
        >
          {workspace === "nomad" ? (
            nomadShell
          ) : compact ? (
            <ScrollView
              automaticallyAdjustKeyboardInsets={Platform.OS === "ios"}
              contentContainerStyle={styles.compactContent}
              keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
              keyboardShouldPersistTaps="handled"
              nestedScrollEnabled
              style={styles.compactScroller}
            >
              {applianceShell}
            </ScrollView>
          ) : (
            applianceShell
          )}
          {compact ? (
            <View style={styles.mobileActions}>
              <ActionButton
                disabled={busy}
                label="Sync"
                onPress={() => void run(() => api.sync())}
                secondary
              />
              <ActionButton
                disabled={busy}
                label="Reconnect"
                onPress={() => void run(() => api.reconnect())}
                secondary
              />
            </View>
          ) : null}
        </KeyboardAvoidingView>
      ) : null}
    </SafeAreaView>
  );
}

const colors = {
  background: "#101411",
  panel: "#171d19",
  panel2: "#1d2520",
  line: "#303b33",
  text: "#ecf2ea",
  muted: "#93a096",
  green: "#91e6a7",
  greenDark: "#173f24",
  red: "#ff9b91",
} as const;

const styles = StyleSheet.create({
  safeArea: { flex: 1, minHeight: "100%", backgroundColor: colors.background },
  keyboardAvoiding: { flex: 1, minHeight: 0 },
  topbar: {
    minHeight: 84,
    paddingHorizontal: 28,
    paddingVertical: 18,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    flexWrap: "wrap",
    gap: 12,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
    backgroundColor: "#101411f2",
  },
  brandCluster: { flexDirection: "row", alignItems: "center", flexWrap: "wrap", gap: 18 },
  eyebrow: {
    marginBottom: 3,
    color: colors.green,
    fontSize: 10,
    fontWeight: "700",
    letterSpacing: 2,
  },
  title: { color: colors.text, fontSize: 24, fontWeight: "800" },
  heading: { color: colors.text, fontSize: 17, fontWeight: "700" },
  workspaceSwitcher: {
    flexDirection: "row",
    padding: 3,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel,
  },
  workspaceTab: {
    minHeight: 30,
    justifyContent: "center",
    paddingHorizontal: 11,
    borderRadius: 6,
  },
  workspaceTabActive: { backgroundColor: colors.greenDark },
  workspaceTabText: { color: "#dfe8df", fontSize: 11, fontWeight: "700" },
  statusCluster: { flexDirection: "row", alignItems: "center", flexWrap: "wrap", gap: 10 },
  pill: {
    paddingHorizontal: 11,
    paddingVertical: 7,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 999,
  },
  pillReady: { borderColor: "#356344", backgroundColor: colors.greenDark },
  pillFaulted: { borderColor: "#70413d", backgroundColor: "#321d1b" },
  pillText: { color: colors.muted, fontSize: 12 },
  button: {
    minHeight: 36,
    justifyContent: "center",
    paddingHorizontal: 13,
    paddingVertical: 8,
    borderRadius: 8,
    borderColor: "#5b9c69",
    borderWidth: 1,
    backgroundColor: colors.green,
  },
  buttonPressed: { opacity: 0.8 },
  buttonDisabled: { opacity: 0.4 },
  buttonSecondary: { borderColor: colors.line, backgroundColor: "transparent" },
  buttonText: { color: "#0d1b11", fontWeight: "700", textAlign: "center" },
  buttonSecondaryText: { color: "#dfe8df" },
  errorBanner: {
    marginHorizontal: 28,
    marginTop: 14,
    padding: 12,
    borderColor: "#70413d",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#321d1b",
  },
  errorText: { color: colors.red },
  reconnectBanner: {
    marginHorizontal: 28,
    marginTop: 14,
    padding: 12,
    borderColor: "#356344",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.greenDark,
  },
  reconnectText: { color: colors.muted },
  inlineError: { color: colors.red, fontSize: 12 },
  activity: { position: "absolute", top: 92, right: 16, zIndex: 2 },
  onboardingScroller: { flex: 1, minHeight: 0 },
  onboardingScrollContent: { paddingBottom: 32 },
  onboarding: {
    width: "92%",
    maxWidth: 620,
    alignSelf: "center",
    marginTop: 48,
    padding: 24,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 14,
    backgroundColor: colors.panel,
  },
  onboardingTitle: { marginBottom: 12, color: colors.text, fontSize: 22, fontWeight: "700" },
  secondaryText: { color: colors.muted, lineHeight: 22 },
  bleDiscovery: {
    marginVertical: 20,
    paddingVertical: 16,
    gap: 10,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  bleDiscoveryTitle: { color: colors.text, fontSize: 16, fontWeight: "700" },
  bleCandidateScroller: { maxHeight: 220 },
  bleCandidateList: { gap: 7 },
  bleCandidate: {
    gap: 4,
    padding: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel2,
  },
  bleCandidateSelected: { borderColor: "#5b9c69", backgroundColor: colors.greenDark },
  bleCandidateHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
  },
  bleCandidateName: { flex: 1, color: "#dfe8df", fontWeight: "700" },
  bleCandidateChoice: { color: colors.green, fontSize: 11, fontWeight: "700" },
  bleSelectionStatus: { color: colors.green, fontSize: 11, lineHeight: 16 },
  serialRow: {
    marginVertical: 20,
    paddingVertical: 13,
    gap: 8,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  actionRow: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 10 },
  compactScroller: { flex: 1 },
  compactContent: { flexGrow: 1 },
  shell: { flex: 1, flexDirection: "row", minHeight: 0 },
  shellCompact: { flexDirection: "column" },
  sidebar: {
    width: 320,
    maxWidth: "100%",
    padding: 22,
    borderRightColor: colors.line,
    borderRightWidth: 1,
    backgroundColor: colors.panel,
  },
  sidebarCompactScroller: {
    width: "100%",
    maxHeight: 320,
    borderRightWidth: 0,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
    backgroundColor: colors.panel,
  },
  sidebarCompactContent: { padding: 22 },
  sectionHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    marginBottom: 14,
  },
  sectionActions: { flexDirection: "row", alignItems: "center", gap: 8 },
  addButton: {
    width: 34,
    height: 34,
    alignItems: "center",
    justifyContent: "center",
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 17,
  },
  addButtonText: { color: colors.text, fontSize: 22 },
  smallButton: {
    minHeight: 32,
    justifyContent: "center",
    paddingHorizontal: 10,
    paddingVertical: 6,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 999,
  },
  smallButtonActive: { borderColor: "#356344", backgroundColor: colors.greenDark },
  smallButtonText: { color: "#dfe8df", fontSize: 11, fontWeight: "700" },
  nearbyPanel: {
    gap: 10,
    marginBottom: 16,
    padding: 12,
    borderColor: "#356344",
    borderWidth: 1,
    borderRadius: 10,
    backgroundColor: colors.greenDark,
  },
  nearbyHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
  },
  nearbyTitle: { flex: 1, gap: 2 },
  nearbyCaption: { color: colors.muted, fontSize: 10 },
  nearbyStatus: { color: colors.muted, fontSize: 11, lineHeight: 16 },
  nearbyError: { gap: 3 },
  nearbyScroller: { maxHeight: 230 },
  nearbyList: { gap: 7 },
  nearbyPeer: {
    gap: 4,
    padding: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel,
  },
  nearbyPeerAdded: { borderColor: "#5b9c69" },
  nearbyPeerHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
  },
  nearbyPeerName: { flex: 1, color: "#dfe8df", fontWeight: "700" },
  nearbyPeerButtons: { flexDirection: "row", alignItems: "center", gap: 5 },
  nearbyPeerButton: {
    minHeight: 28,
    justifyContent: "center",
    paddingHorizontal: 8,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 999,
  },
  nearbyPeerAction: { color: colors.green, fontSize: 11, fontWeight: "700" },
  contactForm: {
    gap: 9,
    marginBottom: 18,
    padding: 14,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 10,
    backgroundColor: colors.panel2,
  },
  label: { color: colors.muted, fontSize: 12 },
  input: {
    width: "100%",
    minHeight: 42,
    paddingHorizontal: 11,
    paddingVertical: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    color: "#f3f7f2",
    backgroundColor: "#0d110e",
  },
  monospaceInput: {
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
  },
  contacts: { gap: 6 },
  contact: {
    flexDirection: "row",
    alignItems: "center",
    gap: 6,
    borderColor: "transparent",
    borderWidth: 1,
    borderRadius: 8,
  },
  contactSelection: { flex: 1, minWidth: 0, gap: 3, padding: 11 },
  contactBrowseButton: { marginRight: 8 },
  contactActive: { borderColor: colors.line, backgroundColor: colors.panel2 },
  contactPressed: { opacity: 0.8 },
  contactName: { color: "#dfe8df", fontWeight: "700" },
  monospace: {
    color: colors.muted,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 11,
    lineHeight: 16,
  },
  deviceMeta: {
    marginTop: 26,
    paddingTop: 16,
    gap: 8,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  metaRow: { flexDirection: "row", gap: 10 },
  metaLabel: {
    width: 78,
    color: colors.muted,
    fontSize: 11,
    fontWeight: "600",
    letterSpacing: 0.8,
    textTransform: "uppercase",
  },
  metaValue: { flex: 1, color: colors.text, fontSize: 12 },
  emptyState: { flex: 1, alignItems: "center", justifyContent: "center", padding: 32 },
  emptyTitle: { marginBottom: 8, color: colors.text, fontSize: 17, fontWeight: "600" },
  conversation: { flex: 1, minWidth: 0 },
  conversationCompact: { minHeight: 420 },
  conversationHeading: {
    minHeight: 72,
    justifyContent: "center",
    gap: 4,
    paddingHorizontal: 22,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  timelineScroller: { flex: 1 },
  timeline: { flexGrow: 1, gap: 12, padding: 22, justifyContent: "flex-end" },
  message: {
    maxWidth: "78%",
    padding: 13,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 12,
  },
  messageInbound: { alignSelf: "flex-start", backgroundColor: colors.panel2 },
  messageOutbound: {
    alignSelf: "flex-end",
    borderColor: "#356344",
    backgroundColor: colors.greenDark,
  },
  messageTitle: { marginBottom: 5, color: colors.text, fontWeight: "700" },
  messageContent: { color: colors.text, lineHeight: 21 },
  messageFooter: { marginTop: 8, color: colors.muted, fontSize: 10 },
  nomadScroller: { flex: 1 },
  nomadContent: { flexGrow: 1, padding: 22 },
  nomadCard: {
    width: "100%",
    maxWidth: 860,
    alignSelf: "center",
    gap: 10,
    padding: 22,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 14,
    backgroundColor: colors.panel,
  },
  nomadHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    flexWrap: "wrap",
    gap: 12,
    marginBottom: 6,
  },
  nomadHeadingCopy: { gap: 2 },
  nomadHint: { flexShrink: 1, color: colors.muted, fontSize: 11, lineHeight: 17 },
  nomadFetchRow: {
    flexDirection: "row",
    alignItems: "center",
    flexWrap: "wrap",
    gap: 12,
    marginTop: 4,
  },
  nomadFetchMeta: {
    gap: 5,
    padding: 11,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel2,
  },
  nomadStatus: {
    flexDirection: "row",
    alignItems: "center",
    gap: 12,
    marginTop: 8,
    padding: 14,
    borderColor: "#356344",
    borderWidth: 1,
    borderRadius: 10,
    backgroundColor: colors.greenDark,
  },
  nomadStatusCopy: { flex: 1, gap: 3 },
  nomadResult: {
    gap: 10,
    marginTop: 8,
    padding: 14,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 10,
    backgroundColor: colors.panel2,
  },
  nomadResultTitle: { color: colors.text, fontSize: 15, fontWeight: "700" },
  nomadRawPage: {
    padding: 14,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#0d110e",
  },
  nomadRawText: {
    color: colors.text,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 13,
    lineHeight: 20,
  },
  compose: {
    gap: 10,
    padding: 16,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    backgroundColor: colors.panel,
  },
  messageInput: { minHeight: 78, textAlignVertical: "top" },
  composeFooter: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  counter: { flex: 1, color: colors.muted, fontSize: 10 },
  mobileActions: {
    flexDirection: "row",
    justifyContent: "flex-end",
    gap: 8,
    padding: 10,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    backgroundColor: colors.panel,
  },
});
