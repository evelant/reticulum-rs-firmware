import type { NativeProfileStoreSnapshot } from "@reticulum/appliance-native";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  AppState,
  Keyboard,
  KeyboardAvoidingView,
  Linking,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Switch,
  Text,
  TextInput,
  useWindowDimensions,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { ActivityEventList, ActivityPanel } from "../components/ActivityPanel.tsx";
import { ConnectivityPanel } from "../components/ConnectivityPanel.tsx";
import { KeyboardDoneAccessory } from "../components/KeyboardDoneAccessory";
import {
  RadioTraceEventList,
  type RadioTraceExportFormat,
  RadioTracePanel,
} from "../components/RadioTracePanel.tsx";
import { TransmissionMapPanel } from "../components/TransmissionMapPanel.tsx";
import type {
  ApplianceSnapshot,
  BytesView,
  ContactView,
  ConversationPeerView,
  MessageActivityPageView,
  MessageLocationView,
  OnboardingView,
  RadioTraceEventView,
  RadioTracePageView,
  RecoveryRequest,
  RetrySendRequest,
  SendRequest,
  TimelineView,
} from "../generated/api.ts";
import {
  MAX_CONTACT_NAME_BYTES,
  MAX_LXMF_BASIC_CONTENT_BYTES,
  MAX_LXMF_BASIC_TITLE_BYTES,
} from "../generated/api.ts";
import { ApplianceApi } from "../lib/api";
import {
  type ApplianceProfilePresentation,
  applianceProfilesPresentation,
  knownProfileForAdvertisedName,
} from "../lib/appliance-profiles.ts";
import {
  applianceStatusPresentation,
  connectionStateLabel,
  connectionTransportLabel,
} from "../lib/appliance-status.ts";
import { bleBondRepairProgressMessage } from "../lib/ble-bond-repair.ts";
import type { BleCandidate, BleScanOptions } from "../lib/ble-central-types.ts";
import { contactSaveIntent } from "../lib/contact-editor.ts";
import {
  conversationPeerLabel,
  messageRequestPeers,
  outboundOnlyUnsavedPeers,
  suggestedContactName,
} from "../lib/conversation-peers.ts";
import { ensureDraftIdentity } from "../lib/draft.ts";
import { deliverExportArtifact } from "../lib/export-artifact";
import {
  type FieldTelemetryClient,
  FieldTelemetryController,
  type FieldTelemetryControllerState,
} from "../lib/field-telemetry.ts";
import { createFieldTelemetryPreferenceStore } from "../lib/field-telemetry-preference";
import { ForegroundNearbyPoll } from "../lib/foreground-nearby-poll.ts";
import {
  ensureForegroundConnection,
  ForegroundReconnect,
  type ForegroundReconnectProgress,
  foregroundReconnectMessage,
} from "../lib/foreground-reconnect.ts";
import { keyboardLayoutPolicy } from "../lib/keyboard-layout.ts";
import { LatestRequest } from "../lib/latest-request.ts";
import { byteLimitError, utf8ByteLength } from "../lib/limits.ts";
import { directLxmfPayloadBudget, directLxmfPayloadError } from "../lib/lxmf-message-size.ts";
import {
  retryMessageCacheKey,
  retryMessageRequest,
  timelineActivityRevision,
  timelineEntryKey,
  timelineMessageCapabilities,
  timelineStatusLabel,
} from "../lib/message-actions.ts";
import { buildMessageActivityAliases, messageActivityPeerLabel } from "../lib/message-activity.ts";
import {
  captureForegroundMessageLocation,
  messageLocationPresentation,
} from "../lib/message-location.ts";
import { type DraftSubmission, prepareDraftSubmission } from "../lib/message-location-draft.ts";
import {
  createMessageLocationPreferenceStore,
  type MessageLocationPreferenceState,
} from "../lib/message-location-preference";
import {
  consumeInitialMessageNotificationTarget,
  createMessageNotificationLedgerStore,
  initializeMessageNotifications,
  type MessageNotificationPermission,
  presentInboundMessageNotification,
  requestMessageNotificationPermission,
  subscribeMessageNotificationTargets,
} from "../lib/message-notification-platform.ts";
import {
  enqueueMessageNotificationTarget,
  MESSAGE_NOTIFICATION_PAGE_SIZE,
  MessageNotificationReconciler,
  type MessageNotificationTarget,
  SupersededMessageNotificationReconciliation,
  shouldPresentInboundMessageNotification,
} from "../lib/message-notifications.ts";
import {
  type LocalMessageAcceptance,
  localMessageAcceptance,
  recordLocalMessageAcceptance,
  unreconciledLocalMessageAcceptances,
} from "../lib/message-submit-ui.ts";
import { readNativeCoreStatus } from "../lib/native-core";
import type { NativeCoreStatus } from "../lib/native-core-types.ts";
import {
  associatedNomadDestinationForLxmf,
  NEARBY_FOREGROUND_POLL_INTERVAL_MS,
  type NearbyPeerView,
  nearbyInterfaceLabel,
  nearbyInterfaceSummaryHint,
  nearbyNetworkSummary,
  nearbyPeerFingerprint,
  nearbyPeerRouteHint,
  nearbyPeerSuggestedName,
  nearbySnapshotElapsedMs,
} from "../lib/nearby-peers.ts";
import {
  NetworkConfigController,
  type NetworkConfigControllerState,
  type NetworkConfigurationClient,
} from "../lib/network-config.ts";
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
import {
  type RadioRoutesClient,
  RadioRoutesController,
  type RadioRoutesControllerState,
} from "../lib/radio-routes.ts";
import {
  collectCompleteRadioTrace,
  createRadioTraceExportDocument,
  radioTraceCsvArtifact,
  radioTraceJsonArtifact,
} from "../lib/radio-trace-export.ts";
import { randomHex } from "../lib/random.ts";
import {
  RETICULUM_PROBE_PRESENTATION_TIMEOUT_MS,
  ReticulumProbeController,
  type ReticulumProbeState,
} from "../lib/reticulum-probe.ts";
import { SettledPoll } from "../lib/settled-poll.ts";
import {
  buildTransmissionMapScene,
  type LocatedTimeline,
  type TransmissionMapFeatureDetails,
} from "../lib/transmission-map.ts";

const EMPTY_ONBOARDING: OnboardingView = { available: false, method: null, snapshot: null };
const ONBOARDING_BLE_SCAN_TIMEOUT_MS = 15_000;
const FOREGROUND_RECONNECT_DELAY_MS = 2_000;
const MESSAGE_ACTIVITY_PAGE_SIZE = 50;
const RADIO_TRACE_PAGE_SIZE = 50;
const KEYBOARD_LAYOUT = keyboardLayoutPolicy(Platform.OS);
const MESSAGE_COMPOSER_INPUT_ACCESSORY_ID = "lxmf-message-composer-keyboard";
interface QueueMessageResult {
  readonly acceptance: LocalMessageAcceptance | null;
  readonly error: string | null;
  readonly queued: boolean;
}
type Workspace = "activity" | "connectivity" | "lxmf" | "map" | "nomad";
type ProfileOperation =
  | { readonly state: "idle" }
  | { readonly message: string; readonly state: "switching" }
  | { readonly message: string; readonly state: "success" }
  | { readonly message: string; readonly state: "error" };
interface MapFeatureEvidence {
  readonly events: readonly RadioTraceEventView[];
  readonly historyIncomplete: boolean;
  readonly profileKey: string;
  readonly timelineSequence: number;
}
type ProfileConfirmation =
  | {
      readonly action: "switch";
      readonly profile: ApplianceProfilePresentation;
    }
  | {
      readonly action: "forget";
      readonly profile: ApplianceProfilePresentation;
    }
  | {
      readonly action: "repair";
      readonly profile: ApplianceProfilePresentation;
    };

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

interface ApplianceProfileManagerProps {
  readonly busy: boolean;
  readonly canAdd: boolean;
  readonly catalog: NativeProfileStoreSnapshot;
  readonly exactBleTargetRequired: boolean;
  readonly onActivate: (profileKey: string) => Promise<boolean>;
  readonly onAdd: () => void;
  readonly onClearOperation: () => void;
  readonly onClose: () => void;
  readonly onForget: ((profileKey: string) => Promise<boolean>) | null;
  readonly onReconnect: () => Promise<boolean>;
  readonly onRepairBond: (() => Promise<boolean>) | null;
  readonly operation: ProfileOperation;
  readonly visible: boolean;
}

function ApplianceProfileManager({
  busy,
  canAdd,
  catalog,
  exactBleTargetRequired,
  onActivate,
  onAdd,
  onClearOperation,
  onClose,
  onForget,
  onReconnect,
  onRepairBond,
  operation,
  visible,
}: ApplianceProfileManagerProps) {
  const [confirmation, setConfirmation] = useState<ProfileConfirmation | null>(null);
  const presentation = applianceProfilesPresentation(catalog);
  const operating = operation.state === "switching";

  useEffect(() => {
    if (!visible) setConfirmation(null);
  }, [visible]);

  const confirmOperation = async () => {
    if (confirmation === null) return;
    const completed =
      confirmation.action === "switch"
        ? await onActivate(confirmation.profile.profileKey)
        : confirmation.action === "forget"
          ? await onForget?.(confirmation.profile.profileKey)
          : await onRepairBond?.();
    if (completed) setConfirmation(null);
  };

  return (
    <Modal
      animationType="slide"
      onRequestClose={() => {
        if (!operating) onClose();
      }}
      presentationStyle="pageSheet"
      transparent={false}
      visible={visible}
    >
      <SafeAreaView style={styles.profileManagerSafeArea}>
        <View style={styles.profileManagerHeading}>
          <View style={styles.profileManagerHeadingCopy}>
            <Text style={styles.eyebrow}>APPLIANCES</Text>
            <Text style={styles.profileManagerTitle}>Choose a Reticulum node</Text>
          </View>
          <ActionButton disabled={operating} label="Done" onPress={onClose} secondary />
        </View>
        <ScrollView
          contentContainerStyle={styles.profileManagerContent}
          style={styles.profileManagerScroller}
        >
          <Text style={styles.secondaryText}>
            Each board keeps its own credential, contacts, conversations, and durable outbox.
          </Text>
          {operation.state === "idle" ? null : (
            <View
              accessibilityLiveRegion={operation.state === "error" ? "assertive" : "polite"}
              style={[
                styles.profileOperation,
                operation.state === "error" && styles.profileOperationError,
                operation.state === "success" && styles.profileOperationSuccess,
              ]}
            >
              {operating ? <ActivityIndicator color="#91e6a7" /> : null}
              <Text
                style={[
                  styles.profileOperationText,
                  operation.state === "error" && styles.profileOperationErrorText,
                ]}
              >
                {operation.message}
              </Text>
            </View>
          )}
          <View style={styles.profileList}>
            {presentation.profiles.map((profile) => {
              const incompatible =
                exactBleTargetRequired && !profile.active && profile.advertisedName === null;
              return (
                <View
                  accessibilityLabel={`${profile.active ? "Active" : "Saved"} appliance ${profile.boardLabel}`}
                  key={profile.profileKey}
                  style={[
                    styles.profileRow,
                    profile.active && styles.profileRowActive,
                    incompatible && styles.profileRowUnavailable,
                  ]}
                >
                  <View style={styles.profileRowHeading}>
                    <Text selectable style={styles.profileBoardLabel}>
                      {profile.boardLabel}
                    </Text>
                    <Text
                      style={[styles.profileBadge, profile.active && styles.profileBadgeActive]}
                    >
                      {profile.active ? "ACTIVE" : incompatible ? "NO BLE NAME" : "SAVED"}
                    </Text>
                  </View>
                  <Text selectable style={styles.monospace}>
                    {profile.bleLabel}
                  </Text>
                  <Text style={styles.profileGeneration}>{profile.generationLabel}</Text>
                  {incompatible ? (
                    <Text style={styles.profileGeneration}>
                      Re-import or repair this profile before selecting it over Bluetooth.
                    </Text>
                  ) : null}
                  {profile.active ? (
                    <>
                      <View style={styles.profileRowActions}>
                        <ActionButton
                          disabled={busy || operating}
                          label="Reconnect"
                          onPress={() => {
                            onClearOperation();
                            void onReconnect();
                          }}
                          secondary
                        />
                        {onRepairBond === null ? null : (
                          <ActionButton
                            disabled={busy || operating}
                            label="Repair Bluetooth"
                            onPress={() => {
                              onClearOperation();
                              setConfirmation({ action: "repair", profile });
                            }}
                            secondary
                          />
                        )}
                      </View>
                      <Text style={styles.profileGeneration}>
                        Switch to another appliance before forgetting this active profile.
                      </Text>
                    </>
                  ) : (
                    <View style={styles.profileRowActions}>
                      <ActionButton
                        disabled={busy || operating || incompatible}
                        label="Switch"
                        onPress={() => {
                          onClearOperation();
                          setConfirmation({ action: "switch", profile });
                        }}
                      />
                      {onForget === null ? null : (
                        <ActionButton
                          disabled={busy || operating}
                          label="Forget"
                          onPress={() => {
                            onClearOperation();
                            setConfirmation({ action: "forget", profile });
                          }}
                          secondary
                        />
                      )}
                    </View>
                  )}
                </View>
              );
            })}
          </View>
          {confirmation === null ? null : (
            <View accessibilityLiveRegion="polite" style={styles.profileConfirmation}>
              <Text style={styles.profileConfirmationTitle}>
                {confirmation.action === "switch"
                  ? `Switch to ${confirmation.profile.boardLabel}?`
                  : confirmation.action === "forget"
                    ? `Forget ${confirmation.profile.boardLabel} from this phone?`
                    : `Repair Bluetooth for ${confirmation.profile.boardLabel}?`}
              </Text>
              <Text style={styles.secondaryText}>
                {confirmation.action === "switch"
                  ? "The current connection will close. Profile-local messages and contacts stay isolated, and any unsent composer text will be discarded."
                  : confirmation.action === "forget"
                    ? "This permanently deletes this phone's local credential, messages, contacts, and outbox for this appliance. It does not revoke the board's credential or remove its Bluetooth bond."
                    : `This keeps the local credential, messages, contacts, and outbox. The alpha board retains one phone connection and one phone bond, so first force-quit this app or disable Bluetooth on the previous phone. On this phone, forget ${confirmation.profile.bleLabel} in system Bluetooth settings. Then hold GPIO21 before pressing RST, keep holding it for at least three seconds, and release it only after the board shows BLE Recovery. Choose Repair Bluetooth while that recovery screen is visible. When the app later asks for physical presence, hold GPIO21 again for about two seconds and enter the displayed code. Repair moves Bluetooth access to this phone and displaces the previous phone.`}
              </Text>
              <View style={styles.actionRow}>
                <ActionButton
                  disabled={busy || operating}
                  label={
                    operating
                      ? confirmation.action === "switch"
                        ? "Switching…"
                        : confirmation.action === "forget"
                          ? "Forgetting…"
                          : "Repairing…"
                      : confirmation.action === "switch"
                        ? "Switch appliance"
                        : confirmation.action === "forget"
                          ? "Delete local data"
                          : "Start Bluetooth repair"
                  }
                  onPress={() => void confirmOperation()}
                />
                <ActionButton
                  disabled={operating}
                  label={
                    confirmation.action === "switch"
                      ? "Keep current"
                      : confirmation.action === "forget"
                        ? "Keep appliance"
                        : "Cancel"
                  }
                  onPress={() => setConfirmation(null)}
                  secondary
                />
              </View>
            </View>
          )}
          <View style={styles.profileAddSection}>
            <Text style={styles.profileConfirmationTitle}>Another physical node</Text>
            <Text style={styles.secondaryText}>
              Find a nearby unpaired appliance and use the existing secure Bluetooth ceremony.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={busy || operating || !canAdd}
                label="Add appliance"
                onPress={() => {
                  onClearOperation();
                  onClose();
                  onAdd();
                }}
              />
            </View>
            {canAdd ? null : (
              <Text style={styles.profileGeneration}>
                Adding is unavailable on this transport; saved profiles can still be switched.
              </Text>
            )}
          </View>
        </ScrollView>
      </SafeAreaView>
    </Modal>
  );
}

interface ApplianceStatusCardProps {
  readonly busy: boolean;
  readonly canAddAppliance: boolean;
  readonly compact: boolean;
  readonly exactBleTargetRequired: boolean;
  readonly nativeCore: NativeCoreStatus | null;
  readonly onActivateProfile: (profileKey: string) => Promise<boolean>;
  readonly onAddAppliance: () => void;
  readonly onClearProfileOperation: () => void;
  readonly onForgetProfile: ((profileKey: string) => Promise<boolean>) | null;
  readonly onReconnect: () => Promise<boolean>;
  readonly onRepairBleBond: (() => Promise<boolean>) | null;
  readonly onSync: () => void;
  readonly profileOperation: ProfileOperation;
  readonly profiles: NativeProfileStoreSnapshot | null;
  readonly snapshot: ApplianceSnapshot | null;
}

function ApplianceStatusCard({
  busy,
  canAddAppliance,
  compact,
  exactBleTargetRequired,
  nativeCore,
  onActivateProfile,
  onAddAppliance,
  onClearProfileOperation,
  onForgetProfile,
  onReconnect,
  onRepairBleBond,
  onSync,
  profileOperation,
  profiles,
  snapshot,
}: ApplianceStatusCardProps) {
  const [showDetails, setShowDetails] = useState(false);
  const [showProfiles, setShowProfiles] = useState(false);
  const presentation = applianceStatusPresentation(snapshot);
  const activeProfile =
    profiles === null ? null : applianceProfilesPresentation(profiles).activeProfile;
  const connectionReady = snapshot?.connection.state === "ready";
  const nativeApiLabel =
    nativeCore?.label ?? (Platform.OS === "web" ? "Web client" : "Checking native bridge");

  return (
    <View style={[styles.applianceStatusCard, compact && styles.applianceStatusCardCompact]}>
      <View
        style={[styles.applianceStatusHeading, compact && styles.applianceStatusHeadingCompact]}
      >
        <View
          style={[styles.applianceStatusIdentity, compact && styles.applianceStatusIdentityCompact]}
        >
          {compact ? null : <Text style={styles.eyebrow}>APPLIANCE STATUS</Text>}
          <Text
            numberOfLines={compact ? 1 : undefined}
            selectable
            style={[styles.applianceStatusBoard, compact && styles.applianceStatusBoardCompact]}
          >
            {activeProfile?.boardLabel ?? presentation.boardLabel}
          </Text>
          <Text
            accessibilityLiveRegion="polite"
            style={[
              styles.applianceStatusConnection,
              compact && styles.applianceStatusConnectionCompact,
              presentation.tone === "ready" && styles.applianceStatusConnectionReady,
              presentation.tone === "faulted" && styles.applianceStatusConnectionFaulted,
            ]}
          >
            {compact ? connectionStateLabel(snapshot?.connection) : presentation.connectionLabel}
          </Text>
        </View>
        <View
          style={[styles.applianceStatusActions, compact && styles.applianceStatusActionsCompact]}
        >
          {compact && !connectionReady ? (
            <Pressable
              accessibilityLabel="Reconnect to active appliance"
              accessibilityRole="button"
              accessibilityState={{ disabled: busy }}
              disabled={busy}
              hitSlop={7}
              onPress={() => void onReconnect()}
              style={({ pressed }) => [
                styles.statusDetailsButton,
                styles.statusDetailsButtonCompact,
                busy && styles.buttonDisabled,
                pressed && !busy && styles.buttonPressed,
              ]}
            >
              <Text style={styles.statusDetailsButtonText}>Reconnect</Text>
            </Pressable>
          ) : null}
          {profiles === null ? null : (
            <Pressable
              accessibilityLabel="Manage saved appliances"
              accessibilityRole="button"
              accessibilityState={{ disabled: busy }}
              disabled={busy}
              hitSlop={7}
              onPress={() => {
                onClearProfileOperation();
                setShowProfiles(true);
              }}
              style={({ pressed }) => [
                styles.statusDetailsButton,
                compact && styles.statusDetailsButtonCompact,
                busy && styles.buttonDisabled,
                pressed && !busy && styles.buttonPressed,
              ]}
            >
              <Text style={styles.statusDetailsButtonText}>{compact ? "Nodes" : "Appliances"}</Text>
            </Pressable>
          )}
          <Pressable
            accessibilityLabel={`${showDetails ? "Hide" : "Show"} appliance diagnostics`}
            accessibilityRole="button"
            accessibilityState={{ expanded: showDetails }}
            hitSlop={7}
            onPress={() => setShowDetails((visible) => !visible)}
            style={({ pressed }) => [
              styles.statusDetailsButton,
              compact && styles.statusDetailsButtonCompact,
              pressed && styles.buttonPressed,
            ]}
          >
            <Text style={styles.statusDetailsButtonText}>
              {showDetails ? (compact ? "Less" : "Hide details") : "Details"}
            </Text>
          </Pressable>
        </View>
      </View>
      {!compact || showDetails ? (
        <>
          <View style={styles.applianceActivity}>
            <Text style={styles.applianceActivityItem}>{presentation.pendingOutboxLabel}</Text>
            <Text style={styles.applianceActivitySeparator}>·</Text>
            <Text style={styles.applianceActivityItem}>{presentation.importedThisRunLabel}</Text>
            <Text style={styles.applianceActivitySeparator}>·</Text>
            <Text style={styles.applianceActivityItem}>{presentation.contactCountLabel}</Text>
          </View>
          <View style={styles.applianceDestination}>
            <Text style={styles.applianceDestinationLabel}>LOCAL LXMF</Text>
            <Text selectable style={styles.applianceDestinationValue}>
              {presentation.lxmfDestination ?? "Not available"}
            </Text>
          </View>
        </>
      ) : null}
      {showDetails ? (
        <View style={styles.applianceStatusDetails}>
          <MetaRow label="Endpoint" value={presentation.endpoint ?? "Not connected"} />
          <MetaRow label="Primary" value={presentation.primaryDestination ?? "Not available"} />
          <MetaRow label="Device ID" value={presentation.deviceId ?? "Not available"} />
          <MetaRow label="API" value={nativeApiLabel} />
          {compact ? (
            <View style={styles.statusUtilityActions}>
              <ActionButton disabled={busy} label="Sync" onPress={onSync} secondary />
              <ActionButton
                disabled={busy}
                label="Reconnect"
                onPress={() => void onReconnect()}
                secondary
              />
            </View>
          ) : null}
        </View>
      ) : null}
      {profiles === null ? null : (
        <ApplianceProfileManager
          busy={busy}
          canAdd={canAddAppliance}
          catalog={profiles}
          exactBleTargetRequired={exactBleTargetRequired}
          onActivate={onActivateProfile}
          onAdd={onAddAppliance}
          onClearOperation={onClearProfileOperation}
          onClose={() => {
            setShowProfiles(false);
            onClearProfileOperation();
          }}
          onForget={onForgetProfile}
          onReconnect={onReconnect}
          onRepairBond={onRepairBleBond}
          operation={profileOperation}
          visible={showProfiles}
        />
      )}
    </View>
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

function OnboardingPanel({
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

interface NearbyPanelProps {
  readonly active: boolean;
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
  readonly snapshotFetchedAtMs: number | null;
}

function NearbyPanel({
  active,
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
  snapshotFetchedAtMs,
}: NearbyPanelProps) {
  const [addingDestination, setAddingDestination] = useState<string | null>(null);
  const [ageClockMs, setAgeClockMs] = useState(() => Date.now());
  useEffect(() => {
    setAgeClockMs(Date.now());
    if (!active) return;
    const timer = setInterval(() => setAgeClockMs(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, [active]);
  const elapsedSinceFetchMs = nearbySnapshotElapsedMs(snapshotFetchedAtMs, ageClockMs);
  const networkSummary = nearbyNetworkSummary(peers, contacts, elapsedSinceFetchMs);

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
        <Text style={styles.nearbyStatus}>{nearbyPeerRouteHint(peer, elapsedSinceFetchMs)}</Text>
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
          <Text style={styles.nearbyCaption}>
            {loaded
              ? `${networkSummary.peerCount} authenticated · ${networkSummary.unaddedPeerCount} not in contacts`
              : "Authenticated LXMF announces"}
          </Text>
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
          No authenticated LXMF announces received yet. Leave both nodes powered, then refresh.
        </Text>
      ) : (
        <>
          <View style={styles.nearbyInterfaces}>
            <Text style={styles.applianceDestinationLabel}>
              OBSERVED INTERFACES ({networkSummary.interfaceCount})
            </Text>
            {networkSummary.interfaces.map((observedInterface) => (
              <View key={observedInterface.interfaceId} style={styles.nearbyInterfaceRow}>
                <Text style={styles.nearbyInterfaceName}>
                  {nearbyInterfaceLabel(observedInterface)}
                </Text>
                <Text style={styles.nearbyStatus}>
                  {nearbyInterfaceSummaryHint(observedInterface)}
                </Text>
              </View>
            ))}
          </View>
          {compact ? (
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
        </>
      )}
    </View>
  );
}

interface SidebarProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly contacts: ContactView[];
  readonly conversations: ConversationPeerView[];
  readonly foreground: boolean;
  readonly onBrowseNomad: (destination: string) => void;
  readonly onClose: () => void;
  readonly onRefreshNearby: (() => Promise<NearbyPeerView[]>) | null;
  readonly onSelect: (destination: string) => void;
  readonly onUpsert: (
    destination: string,
    name: string,
    selectAfterSave?: boolean,
  ) => Promise<boolean>;
  readonly selected: string | null;
  readonly snapshot: ApplianceSnapshot | null;
  readonly visible: boolean;
}

function Sidebar({
  busy,
  compact,
  contacts,
  conversations,
  foreground,
  onBrowseNomad,
  onClose,
  onRefreshNearby,
  onSelect,
  onUpsert,
  selected,
  snapshot,
  visible,
}: SidebarProps) {
  const [showForm, setShowForm] = useState(false);
  const [showNearby, setShowNearby] = useState(false);
  const [name, setName] = useState("");
  const [destination, setDestination] = useState("");
  const [editingDestination, setEditingDestination] = useState<string | null>(null);
  const [requestDestination, setRequestDestination] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [nearbyPeers, setNearbyPeers] = useState<NearbyPeerView[]>([]);
  const [nearbyLoadError, setNearbyLoadError] = useState<string | null>(null);
  const [nearbyLoaded, setNearbyLoaded] = useState(false);
  const [nearbyLoading, setNearbyLoading] = useState(false);
  const [nearbySnapshotFetchedAtMs, setNearbySnapshotFetchedAtMs] = useState<number | null>(null);
  const nearbyRequests = useRef(new LatestRequest());
  const nearbyRefreshInFlight = useRef(false);
  const drawerScroller = useRef<ScrollView | null>(null);
  const readyConnection = snapshot?.connection.state === "ready" ? snapshot.connection : undefined;
  const nearbyConnectionKey =
    readyConnection === undefined
      ? null
      : [
          snapshot?.device?.device_id ?? "",
          connectionTransportLabel(readyConnection.transport),
          readyConnection.endpoint,
          readyConnection.device_label,
        ].join("\u0000");
  const nearbyConnectionKeyRef = useRef(nearbyConnectionKey);

  const resetContactForm = () => {
    setShowForm(false);
    setName("");
    setDestination("");
    setEditingDestination(null);
    setRequestDestination(null);
    setFormError(null);
  };

  const revealContactForm = () => {
    requestAnimationFrame(() => drawerScroller.current?.scrollTo({ animated: true, y: 0 }));
  };

  const beginAddingContact = () => {
    setShowNearby(false);
    setName("");
    setDestination("");
    setEditingDestination(null);
    setRequestDestination(null);
    setFormError(null);
    setShowForm(true);
    revealContactForm();
  };

  const beginEditingContact = (contact: ContactView) => {
    setShowNearby(false);
    setName(contact.name);
    setDestination(contact.destination);
    setEditingDestination(contact.destination);
    setRequestDestination(null);
    setFormError(null);
    setShowForm(true);
    revealContactForm();
  };

  const beginSavingUnsavedPeer = (peer: ConversationPeerView) => {
    setShowNearby(false);
    setName(suggestedContactName(peer.destination));
    setDestination(peer.destination);
    setEditingDestination(null);
    setRequestDestination(peer.destination);
    setFormError(null);
    setShowForm(true);
    revealContactForm();
  };

  const selectContact = (selectedDestination: string) => {
    resetContactForm();
    onSelect(selectedDestination);
    if (compact) onClose();
  };

  const upsertContact = async (
    selectedDestination: string,
    selectedName: string,
    selectAfterSave = true,
  ) => {
    const saved = await onUpsert(selectedDestination, selectedName, selectAfterSave);
    if (saved && compact) onClose();
    return saved;
  };

  const refreshNearby = useCallback(async () => {
    const source = nearbyConnectionKey;
    if (onRefreshNearby === null || source === null || nearbyRefreshInFlight.current) return;

    nearbyRefreshInFlight.current = true;
    const request = nearbyRequests.current.begin();
    setNearbyLoading(true);
    setNearbyLoadError(null);
    try {
      const discovered = await onRefreshNearby();
      if (!nearbyRequests.current.accepts(request) || nearbyConnectionKeyRef.current !== source) {
        return;
      }
      setNearbyPeers(discovered);
      setNearbySnapshotFetchedAtMs(Date.now());
      setNearbyLoaded(true);
    } catch (nextError) {
      if (!nearbyRequests.current.accepts(request) || nearbyConnectionKeyRef.current !== source) {
        return;
      }
      setNearbyLoadError(errorText(nextError));
      setNearbyLoaded(true);
    } finally {
      nearbyRefreshInFlight.current = false;
      if (nearbyRequests.current.accepts(request) && nearbyConnectionKeyRef.current === source) {
        setNearbyLoading(false);
      }
    }
  }, [nearbyConnectionKey, onRefreshNearby]);

  useEffect(() => {
    nearbyConnectionKeyRef.current = nearbyConnectionKey;
    nearbyRequests.current.invalidate();
    setNearbyPeers([]);
    setNearbyLoadError(null);
    setNearbyLoaded(false);
    setNearbyLoading(false);
    setNearbySnapshotFetchedAtMs(null);

    return () => nearbyRequests.current.invalidate();
  }, [nearbyConnectionKey]);

  useEffect(() => {
    if (visible) return;
    setShowForm(false);
    setName("");
    setDestination("");
    setEditingDestination(null);
    setRequestDestination(null);
    setFormError(null);
  }, [visible]);

  useEffect(() => {
    if (
      busy ||
      !foreground ||
      !showNearby ||
      !visible ||
      nearbyConnectionKey === null ||
      onRefreshNearby === null
    ) {
      return;
    }

    const poll = new ForegroundNearbyPoll(refreshNearby, NEARBY_FOREGROUND_POLL_INTERVAL_MS);
    poll.start();
    return () => poll.stop();
  }, [busy, foreground, nearbyConnectionKey, onRefreshNearby, refreshNearby, showNearby, visible]);

  const save = async () => {
    const intent =
      requestDestination === null
        ? contactSaveIntent(name, destination, editingDestination)
        : {
            destination: requestDestination,
            name,
            selectAfterSave: false,
          };
    const nameError = byteLimitError(name, MAX_CONTACT_NAME_BYTES, "Name");
    if (nameError !== null) {
      setFormError(nameError);
      return;
    }
    if (name.trim().length === 0) {
      setFormError("Name is required");
      return;
    }
    if (!/^[0-9a-f]{32}$/.test(intent.destination)) {
      setFormError("LXMF destination must be exactly 32 hexadecimal characters");
      return;
    }
    setFormError(null);
    if (!(await upsertContact(intent.destination, intent.name, intent.selectAfterSave))) return;
    resetContactForm();
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
          onPress={() => selectContact(contact.destination)}
          style={({ pressed }) => [styles.contactSelection, pressed && styles.contactPressed]}
        >
          <Text numberOfLines={1} style={styles.contactName}>
            {displayName}
          </Text>
          <Text selectable style={styles.monospace}>
            {contact.destination}
          </Text>
        </Pressable>
        <View style={styles.contactActions}>
          <Pressable
            accessibilityHint="Changes this phone's local name for the contact"
            accessibilityLabel={`Rename ${displayName}`}
            accessibilityRole="button"
            disabled={busy}
            onPress={() => beginEditingContact(contact)}
            style={({ pressed }) => [
              styles.nearbyPeerButton,
              busy && styles.buttonDisabled,
              pressed && !busy && styles.contactPressed,
            ]}
          >
            <Text style={styles.nearbyPeerAction}>Edit</Text>
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
                busy && styles.buttonDisabled,
                pressed && !busy && styles.contactPressed,
              ]}
            >
              <Text style={styles.nearbyPeerAction}>Browse</Text>
            </Pressable>
          )}
        </View>
      </View>
    );
  });

  const unsavedPeerRow = (peer: ConversationPeerView, inboundRequest: boolean) => {
    const displayName = conversationPeerLabel(peer);
    const lastMessage = peer.last_message;
    const preview =
      lastMessage === null
        ? `${peer.message_count} stored message${peer.message_count === 1 ? "" : "s"}`
        : `${lastMessage.direction === "inbound" ? "Received" : "Sent"} · ${
            bytesText(lastMessage.content) || "Empty message"
          }`;
    return (
      <View
        key={peer.destination}
        style={[styles.contact, selected === peer.destination && styles.contactActive]}
      >
        <Pressable
          accessibilityLabel={
            inboundRequest
              ? `Open message request from ${displayName}, ${peer.inbound_message_count} message${peer.inbound_message_count === 1 ? "" : "s"}`
              : `Open unsaved conversation with ${displayName}, ${peer.message_count} message${peer.message_count === 1 ? "" : "s"}`
          }
          accessibilityRole="button"
          onPress={() => selectContact(peer.destination)}
          style={({ pressed }) => [styles.contactSelection, pressed && styles.contactPressed]}
        >
          <Text numberOfLines={1} style={styles.contactName}>
            {displayName}
          </Text>
          <Text numberOfLines={1} style={styles.messageRequestPreview}>
            {preview}
          </Text>
          <Text selectable style={styles.monospace}>
            {peer.destination}
          </Text>
        </Pressable>
        <View style={styles.contactActions}>
          <Pressable
            accessibilityHint="Adds a local name without changing the authenticated destination"
            accessibilityLabel={`Save ${displayName} as a contact`}
            accessibilityRole="button"
            disabled={busy}
            onPress={() => beginSavingUnsavedPeer(peer)}
            style={({ pressed }) => [
              styles.nearbyPeerButton,
              busy && styles.buttonDisabled,
              pressed && !busy && styles.contactPressed,
            ]}
          >
            <Text style={styles.nearbyPeerAction}>Save</Text>
          </Pressable>
        </View>
      </View>
    );
  };
  const requestRows = messageRequestPeers(conversations).map((peer) => unsavedPeerRow(peer, true));
  const outboundUnsavedRows = outboundOnlyUnsavedPeers(conversations).map((peer) =>
    unsavedPeerRow(peer, false),
  );

  const conversationRows = (
    <>
      {requestRows.length === 0 ? null : (
        <View style={styles.messageRequestsSection}>
          <Text style={styles.applianceDestinationLabel}>
            MESSAGE REQUESTS ({requestRows.length})
          </Text>
          <Text style={styles.nearbyStatus}>
            Authenticated inbound senders that are not saved as contacts.
          </Text>
          <View style={styles.contacts}>{requestRows}</View>
        </View>
      )}
      {outboundUnsavedRows.length === 0 ? null : (
        <View style={styles.messageRequestsSection}>
          <Text style={styles.applianceDestinationLabel}>
            UNSAVED CONVERSATIONS ({outboundUnsavedRows.length})
          </Text>
          <Text style={styles.nearbyStatus}>
            Outbound history for destinations that are not saved as contacts.
          </Text>
          <View style={styles.contacts}>{outboundUnsavedRows}</View>
        </View>
      )}
      <View style={styles.contacts}>{contactRows}</View>
    </>
  );

  const sidebarContents = (
    <>
      <View style={styles.sectionHeading}>
        <Text style={styles.heading}>Contacts</Text>
        <View style={styles.sectionActions}>
          <Pressable
            accessibilityLabel={showNearby ? "Hide nearby peers" : "Find nearby peers"}
            accessibilityRole="button"
            onPress={() => {
              resetContactForm();
              setShowNearby((visible) => !visible);
            }}
            style={[styles.smallButton, showNearby && styles.smallButtonActive]}
          >
            <Text style={styles.smallButtonText}>Nearby</Text>
          </Pressable>
          <Pressable
            accessibilityLabel="Add contact manually"
            accessibilityRole="button"
            onPress={beginAddingContact}
            style={styles.addButton}
          >
            <Text style={styles.addButtonText}>+</Text>
          </Pressable>
        </View>
      </View>
      {showNearby ? (
        <NearbyPanel
          active={foreground && visible}
          busy={busy}
          compact={compact}
          connected={readyConnection !== undefined}
          contacts={contacts}
          loadError={nearbyLoadError}
          loaded={nearbyLoaded}
          loading={nearbyLoading}
          onBrowseNomad={onBrowseNomad}
          onRefresh={onRefreshNearby === null ? null : refreshNearby}
          onSelect={selectContact}
          onUpsert={upsertContact}
          peers={nearbyPeers}
          snapshotFetchedAtMs={nearbySnapshotFetchedAtMs}
        />
      ) : null}
      {showForm ? (
        <View style={styles.contactForm}>
          <Text style={styles.contactName}>
            {editingDestination !== null
              ? "Rename contact"
              : requestDestination !== null
                ? "Save conversation peer"
                : "Add contact"}
          </Text>
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
            accessibilityLabel={
              editingDestination === null && requestDestination === null
                ? "LXMF destination"
                : "LXMF destination, fixed for this contact"
            }
            accessibilityState={{
              disabled: busy || editingDestination !== null || requestDestination !== null,
            }}
            autoCapitalize="none"
            autoCorrect={false}
            editable={!busy && editingDestination === null && requestDestination === null}
            maxLength={32}
            onChangeText={setDestination}
            selectTextOnFocus={editingDestination !== null || requestDestination !== null}
            style={[
              styles.input,
              styles.monospaceInput,
              (editingDestination !== null || requestDestination !== null) && styles.inputReadOnly,
            ]}
            value={destination}
          />
          {formError === null ? null : <Text style={styles.inlineError}>{formError}</Text>}
          <View style={styles.actionRow}>
            <ActionButton
              disabled={busy}
              label={
                editingDestination !== null
                  ? "Save name"
                  : requestDestination !== null
                    ? "Add contact"
                    : "Save"
              }
              onPress={() => void save()}
            />
            <ActionButton label="Cancel" onPress={resetContactForm} secondary />
          </View>
        </View>
      ) : null}
      {compact ? (
        conversationRows
      ) : (
        <ScrollView
          contentContainerStyle={styles.contacts}
          keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
          keyboardShouldPersistTaps="handled"
        >
          {conversationRows}
        </ScrollView>
      )}
    </>
  );

  if (compact) {
    return (
      <Modal animationType="slide" onRequestClose={onClose} transparent visible={visible}>
        <KeyboardAvoidingView
          behavior={KEYBOARD_LAYOUT.avoidingBehavior}
          enabled={KEYBOARD_LAYOUT.avoidingEnabled}
          style={styles.sidebarDrawerBackdrop}
        >
          <Pressable
            accessibilityLabel="Close contacts"
            accessibilityRole="button"
            onPress={onClose}
            style={styles.sidebarDrawerDismiss}
          />
          <SafeAreaView style={styles.sidebarDrawer}>
            <View style={styles.sidebarDrawerHeading}>
              <View>
                <Text style={styles.eyebrow}>MESSAGES</Text>
                <Text style={styles.profileManagerTitle}>Contacts</Text>
              </View>
              <ActionButton label="Done" onPress={onClose} secondary />
            </View>
            <ScrollView
              automaticallyAdjustKeyboardInsets={Platform.OS === "ios"}
              contentContainerStyle={styles.sidebarCompactContent}
              keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
              keyboardShouldPersistTaps="handled"
              ref={drawerScroller}
              style={styles.sidebarCompactScroller}
            >
              {sidebarContents}
            </ScrollView>
          </SafeAreaView>
        </KeyboardAvoidingView>
      </Modal>
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

function AttachedMessageLocation({ location }: { readonly location: MessageLocationView }) {
  const presentation = messageLocationPresentation(location);
  return (
    <View accessibilityLabel={presentation.summary} style={styles.messageLocationChip}>
      <Text style={styles.messageLocationChipLabel}>ATTACHED MESSAGE LOCATION</Text>
      <Text numberOfLines={2} selectable style={styles.messageLocationChipText}>
        {presentation.coordinates} · {presentation.accuracy}
      </Text>
    </View>
  );
}

interface ConversationProps {
  readonly busy: boolean;
  readonly canMeasurePath: boolean;
  readonly compact: boolean;
  readonly messageLocationDefaultEnabled: boolean;
  readonly messageLocationPreferenceLoaded: boolean;
  readonly onAbandonRetainedProbe: () => void;
  readonly onMeasurePath: (destination: string) => Promise<void>;
  readonly onDraftChanged: () => void;
  readonly onLoadMessageActivity: (
    timelineSequence: number,
    beforeEventId: number | null,
  ) => Promise<MessageActivityPageView>;
  readonly onLoadRadioTrace: (
    timelineSequence: number,
    beforeEventId: number | null,
  ) => Promise<RadioTracePageView>;
  readonly onExportRadioTrace: (
    timelineSequence: number,
    format: RadioTraceExportFormat,
  ) => Promise<void>;
  readonly onRetryMessage: (entry: TimelineView) => Promise<boolean>;
  readonly onSend: (
    title: string,
    content: string,
    attachLocation: boolean,
  ) => Promise<QueueMessageResult>;
  readonly peer: ConversationPeerView | undefined;
  readonly probeState: ReticulumProbeState;
  readonly timeline: TimelineView[];
}

function Conversation({
  busy,
  canMeasurePath,
  compact,
  messageLocationDefaultEnabled,
  messageLocationPreferenceLoaded,
  onAbandonRetainedProbe,
  onMeasurePath,
  onDraftChanged,
  onLoadMessageActivity,
  onLoadRadioTrace,
  onExportRadioTrace,
  onRetryMessage,
  onSend,
  peer,
  probeState,
  timeline,
}: ConversationProps) {
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);
  const [attachLocation, setAttachLocation] = useState(
    messageLocationPreferenceLoaded && messageLocationDefaultEnabled,
  );
  const [queueing, setQueueing] = useState(false);
  const [localAcceptances, setLocalAcceptances] = useState<readonly LocalMessageAcceptance[]>([]);
  const [localAcceptanceScrollGeneration, setLocalAcceptanceScrollGeneration] = useState(0);
  const [messageLocationActionError, setMessageLocationActionError] = useState<string | null>(null);
  const [selectedActionKey, setSelectedActionKey] = useState<string | null>(null);
  const [messageActivityPage, setMessageActivityPage] = useState<MessageActivityPageView | null>(
    null,
  );
  const [messageActivityLoading, setMessageActivityLoading] = useState(false);
  const [messageActivityError, setMessageActivityError] = useState<string | null>(null);
  const [messageRadioTracePage, setMessageRadioTracePage] = useState<RadioTracePageView | null>(
    null,
  );
  const [messageRadioTraceLoading, setMessageRadioTraceLoading] = useState(false);
  const [messageRadioTraceError, setMessageRadioTraceError] = useState<string | null>(null);
  const [messageRadioTraceExporting, setMessageRadioTraceExporting] =
    useState<RadioTraceExportFormat | null>(null);
  const draftVersion = useRef(0);
  const timelineScroller = useRef<ScrollView | null>(null);
  const messageLocationPreferenceApplied = useRef(messageLocationPreferenceLoaded);
  const messageActivityGeneration = useRef(0);
  const messageRadioTraceGeneration = useRef(0);
  const selectedAction =
    selectedActionKey === null
      ? null
      : (timeline.find((entry) => timelineEntryKey(entry) === selectedActionKey) ?? null);
  const selectedActionSequence = selectedAction?.sequence ?? null;
  const selectedActionActivityRevision =
    selectedAction === null ? null : timelineActivityRevision(selectedAction);
  const activePeerDestination = peer?.destination ?? null;
  const visibleLocalAcceptances = unreconciledLocalMessageAcceptances(
    localAcceptances,
    timeline,
    activePeerDestination,
  );

  useEffect(() => {
    if (selectedActionKey !== null && selectedAction === null) setSelectedActionKey(null);
  }, [selectedAction, selectedActionKey]);

  useEffect(() => {
    if (!messageLocationPreferenceLoaded || messageLocationPreferenceApplied.current) return;
    messageLocationPreferenceApplied.current = true;
    setAttachLocation(messageLocationDefaultEnabled);
  }, [messageLocationDefaultEnabled, messageLocationPreferenceLoaded]);

  useEffect(() => {
    setLocalAcceptances((current) =>
      unreconciledLocalMessageAcceptances(current, timeline, activePeerDestination),
    );
  }, [activePeerDestination, timeline]);

  useEffect(() => {
    if (localAcceptanceScrollGeneration === 0) return;
    const frame = requestAnimationFrame(() => {
      timelineScroller.current?.scrollToEnd({ animated: true });
    });
    return () => cancelAnimationFrame(frame);
  }, [localAcceptanceScrollGeneration]);

  useEffect(() => {
    const generation = messageActivityGeneration.current + 1;
    messageActivityGeneration.current = generation;
    setMessageActivityPage(null);
    setMessageActivityError(null);
    if (selectedActionSequence === null || selectedActionActivityRevision === null) {
      setMessageActivityLoading(false);
      return;
    }
    setMessageActivityLoading(true);
    void onLoadMessageActivity(selectedActionSequence, null)
      .then((page) => {
        if (messageActivityGeneration.current !== generation) return;
        setMessageActivityPage(page);
      })
      .catch((nextError) => {
        if (messageActivityGeneration.current !== generation) return;
        setMessageActivityError(errorText(nextError));
      })
      .finally(() => {
        if (messageActivityGeneration.current === generation) setMessageActivityLoading(false);
      });
  }, [onLoadMessageActivity, selectedActionActivityRevision, selectedActionSequence]);

  useEffect(() => {
    const generation = messageRadioTraceGeneration.current + 1;
    messageRadioTraceGeneration.current = generation;
    setMessageRadioTracePage(null);
    setMessageRadioTraceError(null);
    if (selectedActionSequence === null || selectedActionActivityRevision === null) {
      setMessageRadioTraceLoading(false);
      return;
    }
    setMessageRadioTraceLoading(true);
    void onLoadRadioTrace(selectedActionSequence, null)
      .then((page) => {
        if (messageRadioTraceGeneration.current !== generation) return;
        setMessageRadioTracePage(page);
      })
      .catch((nextError) => {
        if (messageRadioTraceGeneration.current !== generation) return;
        setMessageRadioTraceError(errorText(nextError));
      })
      .finally(() => {
        if (messageRadioTraceGeneration.current === generation) {
          setMessageRadioTraceLoading(false);
        }
      });
  }, [onLoadRadioTrace, selectedActionActivityRevision, selectedActionSequence]);

  if (peer === undefined) {
    return (
      <View style={[styles.emptyState, compact && styles.conversationCompact]}>
        <Text style={styles.emptyTitle}>
          Select a contact, message request, or unsaved conversation to begin.
        </Text>
        <Text style={styles.secondaryText}>
          The node continues receiving and routing while this app is closed.
        </Text>
      </View>
    );
  }

  const probeDestination =
    probeState.status === "idle"
      ? null
      : probeState.status === "input_error"
        ? probeState.destination
        : probeState.status === "starting" || probeState.status === "error"
          ? probeState.request.destination
          : probeState.destination;
  const visibleProbeState = probeDestination === peer.destination ? probeState : null;
  const probeRunning =
    visibleProbeState?.status === "starting" || visibleProbeState?.status === "pending";
  const probeRetainedForResume =
    visibleProbeState?.status === "timed_out" ||
    (visibleProbeState?.status === "error" && visibleProbeState.stage === "poll");
  const probeOwnsDeviceSlot =
    probeState.status === "starting" ||
    probeState.status === "pending" ||
    probeState.status === "timed_out" ||
    (probeState.status === "error" && probeState.stage === "poll");
  const probeOwnedByAnotherPeer =
    probeOwnsDeviceSlot && probeDestination !== null && probeDestination !== peer.destination;
  const probeButtonDisabled = busy || !canMeasurePath || probeRunning || probeOwnedByAnotherPeer;

  const titleBytes = utf8ByteLength(title);
  const contentBytes = utf8ByteLength(content);
  const directPayloadBudget = directLxmfPayloadBudget(titleBytes, contentBytes, attachLocation);
  const actionCapabilities =
    selectedAction === null ? null : timelineMessageCapabilities(selectedAction);
  const selectedMessageLocation =
    selectedAction?.location === null || selectedAction?.location === undefined
      ? null
      : messageLocationPresentation(selectedAction.location);
  const openSelectedMessageLocation = async () => {
    if (selectedMessageLocation === null) return;
    setMessageLocationActionError(null);
    try {
      await Linking.openURL(selectedMessageLocation.mapUrl);
    } catch (nextError) {
      setMessageLocationActionError(`Could not open map: ${errorText(nextError)}`);
    }
  };
  const send = async () => {
    const error =
      byteLimitError(title, MAX_LXMF_BASIC_TITLE_BYTES, "Title") ??
      byteLimitError(content, MAX_LXMF_BASIC_CONTENT_BYTES, "Message") ??
      directLxmfPayloadError(titleBytes, contentBytes, attachLocation) ??
      (content.length === 0 ? "Message is required" : null);
    setValidationError(error);
    if (error !== null) return;
    const submittedVersion = draftVersion.current;
    setQueueing(true);
    try {
      const result = await onSend(title, content, attachLocation);
      if (!result.queued) {
        setValidationError(result.error ?? "The message could not be queued");
        return;
      }
      const acceptance = result.acceptance;
      if (acceptance !== null) {
        setLocalAcceptances((current) => recordLocalMessageAcceptance(current, acceptance));
        setLocalAcceptanceScrollGeneration((generation) => generation + 1);
      }
      if (draftVersion.current === submittedVersion) {
        setTitle("");
        setContent("");
        setAttachLocation(messageLocationDefaultEnabled);
      }
    } finally {
      setQueueing(false);
    }
  };
  const populateDraft = (entry: TimelineView) => {
    const capabilities = timelineMessageCapabilities(entry);
    if (!capabilities.canUseAsDraft) return;
    draftVersion.current += 1;
    setTitle(entry.title.value);
    setContent(entry.content.value);
    setAttachLocation(messageLocationDefaultEnabled);
    setValidationError(null);
    onDraftChanged();
    setSelectedActionKey(null);
  };
  const retryMessage = async (entry: TimelineView) => {
    const capabilities = timelineMessageCapabilities(entry);
    if (!capabilities.canRetry) return;
    if (await onRetryMessage(entry)) setSelectedActionKey(null);
  };
  const loadOlderMessageActivity = async () => {
    if (
      selectedAction === null ||
      messageActivityLoading ||
      messageActivityPage?.next_before_event_id === null ||
      messageActivityPage?.next_before_event_id === undefined
    ) {
      return;
    }
    const generation = messageActivityGeneration.current;
    setMessageActivityLoading(true);
    setMessageActivityError(null);
    try {
      const older = await onLoadMessageActivity(
        selectedAction.sequence,
        messageActivityPage.next_before_event_id,
      );
      if (messageActivityGeneration.current !== generation) return;
      setMessageActivityPage({
        events: [...messageActivityPage.events, ...older.events],
        next_before_event_id: older.next_before_event_id,
        history_incomplete: messageActivityPage.history_incomplete || older.history_incomplete,
      });
    } catch (nextError) {
      if (messageActivityGeneration.current === generation) {
        setMessageActivityError(errorText(nextError));
      }
    } finally {
      if (messageActivityGeneration.current === generation) setMessageActivityLoading(false);
    }
  };
  const loadOlderMessageRadioTrace = async () => {
    if (
      selectedAction === null ||
      messageRadioTraceLoading ||
      messageRadioTracePage?.next_before_event_id === null ||
      messageRadioTracePage?.next_before_event_id === undefined
    ) {
      return;
    }
    const generation = messageRadioTraceGeneration.current;
    setMessageRadioTraceLoading(true);
    setMessageRadioTraceError(null);
    try {
      const older = await onLoadRadioTrace(
        selectedAction.sequence,
        messageRadioTracePage.next_before_event_id,
      );
      if (messageRadioTraceGeneration.current !== generation) return;
      setMessageRadioTracePage({
        events: [...messageRadioTracePage.events, ...older.events],
        next_before_event_id: older.next_before_event_id,
        history_incomplete: messageRadioTracePage.history_incomplete || older.history_incomplete,
      });
    } catch (nextError) {
      if (messageRadioTraceGeneration.current === generation) {
        setMessageRadioTraceError(errorText(nextError));
      }
    } finally {
      if (messageRadioTraceGeneration.current === generation) {
        setMessageRadioTraceLoading(false);
      }
    }
  };
  const exportMessageRadioTrace = async (format: RadioTraceExportFormat) => {
    if (selectedAction === null || messageRadioTraceExporting !== null) return;
    setMessageRadioTraceExporting(format);
    setMessageRadioTraceError(null);
    try {
      await onExportRadioTrace(selectedAction.sequence, format);
    } catch (nextError) {
      setMessageRadioTraceError(`Export failed: ${errorText(nextError)}`);
    } finally {
      setMessageRadioTraceExporting(null);
    }
  };

  return (
    <View style={[styles.conversation, compact && styles.conversationCompact]}>
      <View style={[styles.conversationHeading, compact && styles.conversationHeadingCompact]}>
        <View style={styles.conversationIdentity}>
          <Text style={styles.heading}>{conversationPeerLabel(peer)}</Text>
          {peer.name === null ? (
            <Text style={styles.messageRequestBadge}>
              {peer.inbound_message_count > 0
                ? "MESSAGE REQUEST · NOT SAVED"
                : "UNSAVED CONVERSATION"}
            </Text>
          ) : null}
        </View>
        <Text selectable style={styles.monospace}>
          {peer.destination}
        </Text>
        <Pressable
          accessibilityHint="Sends one bounded Reticulum path-and-proof measurement"
          accessibilityLabel={`Measure Reticulum path to ${conversationPeerLabel(peer)}`}
          accessibilityRole="button"
          disabled={probeButtonDisabled}
          onPress={() => void onMeasurePath(peer.destination)}
          style={({ pressed }) => [
            styles.measurePathButton,
            probeButtonDisabled && styles.buttonDisabled,
            pressed && !probeButtonDisabled && styles.buttonPressed,
          ]}
        >
          <Text style={styles.measurePathButtonText}>
            {probeRunning
              ? "Measuring…"
              : probeOwnedByAnotherPeer
                ? "Measurement active"
                : probeRetainedForResume
                  ? "Resume measurement"
                  : "Measure path"}
          </Text>
        </Pressable>
      </View>
      {visibleProbeState === null || visibleProbeState.status === "idle" ? null : (
        <View
          accessibilityLiveRegion={
            visibleProbeState.status === "failed" ||
            visibleProbeState.status === "error" ||
            visibleProbeState.status === "input_error" ||
            visibleProbeState.status === "timed_out"
              ? "assertive"
              : "polite"
          }
          style={styles.probeResult}
        >
          <Text style={styles.probeResultTitle}>
            {visibleProbeState.status === "starting"
              ? "Starting path measurement…"
              : visibleProbeState.status === "pending"
                ? visibleProbeState.phase === null
                  ? "Probe accepted; waiting for node status…"
                  : visibleProbeState.phase === "path_lookup"
                    ? "Looking up a Reticulum path…"
                    : visibleProbeState.phase === "awaiting_dispatch"
                      ? "Waiting for probe dispatch…"
                      : "Probe sent; awaiting proof…"
                : visibleProbeState.status === "succeeded"
                  ? `${visibleProbeState.result.round_trip_ms} ms round trip · ${visibleProbeState.result.hops} route ${visibleProbeState.result.hops === 1 ? "hop" : "hops"}`
                  : visibleProbeState.status === "failed"
                    ? `Measurement failed: ${visibleProbeState.failure.replaceAll("_", " ")}`
                    : visibleProbeState.status === "timed_out"
                      ? `No terminal result after ${RETICULUM_PROBE_PRESENTATION_TIMEOUT_MS / 60_000} minutes`
                      : visibleProbeState.error}
          </Text>
          {visibleProbeState.status === "succeeded" ? (
            <>
              <Text selectable style={styles.probeResultValue}>
                Return interface {visibleProbeState.result.ingress_observation.interface_id}
                {visibleProbeState.result.ingress_observation.signal === null
                  ? " · no radio signal values"
                  : ` · ${visibleProbeState.result.ingress_observation.signal.rssi_dbm} dBm RSSI · ${visibleProbeState.result.ingress_observation.signal.snr_db} dB SNR`}
              </Text>
              <Text style={styles.probeResultHelp}>
                Signal is receiver-local at this appliance on the proof&apos;s final return hop. A
                relay may be the transmitter; this is not the remote receiver&apos;s request RSSI.
              </Text>
            </>
          ) : null}
          {probeRetainedForResume ? (
            <Pressable
              accessibilityHint="Forgets this local probe ID after a device reboot or unrecoverable poll failure"
              accessibilityLabel="Clear retained path measurement"
              accessibilityRole="button"
              onPress={onAbandonRetainedProbe}
              style={({ pressed }) => [styles.probeResultAction, pressed && styles.buttonPressed]}
            >
              <Text style={styles.probeResultActionText}>Clear retained measurement</Text>
            </Pressable>
          ) : null}
        </View>
      )}
      <ScrollView
        contentContainerStyle={[styles.timeline, compact && styles.timelineCompact]}
        keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
        keyboardShouldPersistTaps="handled"
        ref={timelineScroller}
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
            <View style={styles.messageHeading}>
              <Text numberOfLines={compact ? 2 : undefined} style={styles.messageTitle}>
                {bytesText(entry.title) || "Untitled"}
              </Text>
              <Pressable
                accessibilityLabel={`Actions for ${bytesText(entry.title) || "untitled message"}`}
                accessibilityRole="button"
                hitSlop={8}
                onPress={() => {
                  Keyboard.dismiss();
                  setMessageLocationActionError(null);
                  setSelectedActionKey(timelineEntryKey(entry));
                }}
                style={({ pressed }) => [
                  styles.messageActionsButton,
                  pressed && styles.buttonPressed,
                ]}
              >
                <Text style={styles.messageActionsButtonText}>•••</Text>
              </Pressable>
            </View>
            <Text selectable style={styles.messageContent}>
              {bytesText(entry.content)}
            </Text>
            {entry.location ? <AttachedMessageLocation location={entry.location} /> : null}
            <Text style={styles.messageFooter}>
              {new Date(entry.timestamp_ms).toLocaleString()}
              {entry.status === null ? "" : ` · ${timelineStatusLabel(entry)}`}
              {entry.direction !== "inbound" ||
              entry.ingress_observation === null ||
              entry.ingress_observation.signal === null
                ? ""
                : ` · RX ${entry.ingress_observation.signal.rssi_dbm} dBm · SNR ${entry.ingress_observation.signal.snr_db} dB`}
            </Text>
          </View>
        ))}
        {visibleLocalAcceptances.map((acceptance) => (
          <View
            key={`local:${acceptance.outboxId}`}
            style={[styles.message, styles.messageOutbound]}
          >
            <View style={styles.messageHeading}>
              <Text numberOfLines={compact ? 2 : undefined} style={styles.messageTitle}>
                {acceptance.title || "Untitled"}
              </Text>
              <Text style={styles.localAcceptanceBadge}>QUEUED</Text>
            </View>
            <Text selectable style={styles.messageContent}>
              {acceptance.content}
            </Text>
            {acceptance.location ? (
              <AttachedMessageLocation location={acceptance.location} />
            ) : null}
            <Text style={styles.messageFooter}>
              {new Date(acceptance.timestampMs).toLocaleString()} · Queued on appliance
            </Text>
          </View>
        ))}
      </ScrollView>
      <ScrollView
        contentContainerStyle={[styles.compose, compact && styles.composeCompact]}
        keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
        keyboardShouldPersistTaps="handled"
        nestedScrollEnabled
        showsVerticalScrollIndicator={compact}
        style={[styles.composeScroller, compact && styles.composeScrollerCompact]}
      >
        <TextInput
          accessibilityLabel="Message title"
          editable={!busy}
          inputAccessoryViewID={
            KEYBOARD_LAYOUT.inputAccessoryEnabled ? MESSAGE_COMPOSER_INPUT_ACCESSORY_ID : undefined
          }
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
          inputAccessoryViewID={
            KEYBOARD_LAYOUT.inputAccessoryEnabled ? MESSAGE_COMPOSER_INPUT_ACCESSORY_ID : undefined
          }
          multiline
          onChangeText={(value) => {
            draftVersion.current += 1;
            setContent(value);
            onDraftChanged();
          }}
          placeholder="Message"
          placeholderTextColor="#748078"
          style={[styles.input, styles.messageInput, compact && styles.messageInputCompact]}
          value={content}
        />
        {validationError === null ? null : (
          <Text style={styles.inlineError}>{validationError}</Text>
        )}
        <View
          style={[styles.composerLocationToggle, compact && styles.composerLocationToggleCompact]}
        >
          <View
            style={[styles.composerLocationCopy, compact && styles.composerLocationCopyCompact]}
          >
            <Text style={styles.composerLocationTitle}>
              {compact ? "Location" : "Attach phone location"}
            </Text>
            <Text numberOfLines={compact ? 1 : undefined} style={styles.composerLocationHelp}>
              {messageLocationPreferenceLoaded
                ? attachLocation
                  ? compact
                    ? "On · fresh phone fix at send"
                    : "A fresh high-accuracy fix will be requested when you queue this message. Queueing fails if the fix is unavailable."
                  : compact
                    ? "Off"
                    : "No sender location will be included in this message."
                : "Loading this phone's saved default…"}
            </Text>
            {compact ? null : (
              <Text style={styles.composerLocationCaveat}>
                Recipient-visible message metadata · not board or RF position
              </Text>
            )}
          </View>
          <Switch
            accessibilityLabel="Attach phone location to this message"
            disabled={busy || queueing || !messageLocationPreferenceLoaded}
            onValueChange={(enabled) => {
              draftVersion.current += 1;
              setAttachLocation(enabled);
              setValidationError(null);
              onDraftChanged();
            }}
            trackColor={{ false: colors.line, true: "#496d8f" }}
            value={attachLocation}
          />
        </View>
        <View style={styles.composeFooter}>
          <View style={styles.composerBudget}>
            <Text style={[styles.counter, !directPayloadBudget.fits && styles.counterOverLimit]}>
              {compact
                ? `T ${titleBytes}/${MAX_LXMF_BASIC_TITLE_BYTES} · M ${contentBytes}/${MAX_LXMF_BASIC_CONTENT_BYTES} · Payload ${directPayloadBudget.payloadBytes}/${directPayloadBudget.maximumPayloadBytes}`
                : `Title ${titleBytes} / ${MAX_LXMF_BASIC_TITLE_BYTES} · Message ${contentBytes} / ${MAX_LXMF_BASIC_CONTENT_BYTES} · Direct payload ${directPayloadBudget.payloadBytes} / ${directPayloadBudget.maximumPayloadBytes}`}
            </Text>
            {compact ? null : (
              <Text
                style={[
                  styles.composerBudgetHelp,
                  !directPayloadBudget.fits && styles.counterOverLimit,
                ]}
              >
                {attachLocation
                  ? `Includes a conservative ${directPayloadBudget.fieldsEncodedBytes}-byte location-fields reservation and exact bin8/bin16 title and message headers.`
                  : `Includes the exact ${directPayloadBudget.fieldsEncodedBytes}-byte empty fields map and bin8/bin16 title and message headers.`}
              </Text>
            )}
          </View>
          {queueing ? <ActivityIndicator color={colors.green} size="small" /> : null}
          <ActionButton
            disabled={busy || queueing || !messageLocationPreferenceLoaded}
            label={
              queueing ? (attachLocation ? "Getting location…" : "Queueing…") : "Queue message"
            }
            onPress={() => void send()}
          />
        </View>
      </ScrollView>
      {KEYBOARD_LAYOUT.inputAccessoryEnabled ? (
        <KeyboardDoneAccessory nativeID={MESSAGE_COMPOSER_INPUT_ACCESSORY_ID} />
      ) : null}
      <Modal
        animationType="fade"
        onRequestClose={() => setSelectedActionKey(null)}
        transparent
        visible={selectedAction !== null}
      >
        <View style={styles.messageActionsBackdrop}>
          <Pressable
            accessibilityLabel="Close message actions"
            accessibilityRole="button"
            onPress={() => setSelectedActionKey(null)}
            style={styles.messageActionsDismiss}
          />
          {selectedAction === null || actionCapabilities === null ? null : (
            <SafeAreaView style={styles.messageActionsSheet}>
              <View style={styles.messageActionsHeading}>
                <View style={styles.messageActionsHeadingCopy}>
                  <Text style={styles.eyebrow}>MESSAGE</Text>
                  <Text style={styles.profileManagerTitle}>Details and actions</Text>
                </View>
                <ActionButton label="Done" onPress={() => setSelectedActionKey(null)} secondary />
              </View>
              <ScrollView
                contentContainerStyle={styles.messageActionsContent}
                keyboardShouldPersistTaps="handled"
              >
                <MetaRow
                  label="Direction"
                  value={selectedAction.direction === "inbound" ? "Received" : "Sent"}
                />
                <MetaRow label="Status" value={timelineStatusLabel(selectedAction)} />
                <MetaRow
                  label="Time"
                  value={new Date(selectedAction.timestamp_ms).toLocaleString()}
                />
                <MetaRow label="Title" value={bytesText(selectedAction.title) || "Untitled"} />
                <MetaRow label="Message" value={bytesText(selectedAction.content) || "Empty"} />
                <MetaRow label="Message ID" value={selectedAction.message_id ?? "Not assigned"} />
                <MetaRow
                  label="Outbox ID"
                  value={
                    selectedAction.outbox_id === null
                      ? "Not applicable"
                      : String(selectedAction.outbox_id)
                  }
                />
                <MetaRow
                  label="Submission"
                  value={
                    selectedAction.submission_id === null
                      ? "Not assigned"
                      : String(selectedAction.submission_id)
                  }
                />
                <MetaRow
                  label="App submission #"
                  value={
                    selectedAction.current_attempt_number === null
                      ? "Not applicable"
                      : String(selectedAction.current_attempt_number)
                  }
                />
                <MetaRow
                  label="Legacy app rearms"
                  value={
                    selectedAction.automatic_retry_count === null
                      ? "Not applicable"
                      : String(selectedAction.automatic_retry_count)
                  }
                />
                <MetaRow label="Sequence" value={String(selectedAction.sequence)} />
                {selectedAction.packet_evidence === null ? null : (
                  <>
                    <MetaRow
                      label="Packet bytes"
                      value={String(selectedAction.packet_evidence.encoded_packet_len)}
                    />
                    <MetaRow
                      label="Packet SHA"
                      value={selectedAction.packet_evidence.encoded_packet_sha256}
                    />
                  </>
                )}
                {selectedMessageLocation === null ? (
                  <MetaRow label="Attached location" value="None" />
                ) : (
                  <View style={styles.messageAttachedLocationDetails}>
                    <Text style={styles.applianceDestinationLabel}>ATTACHED MESSAGE LOCATION</Text>
                    <MetaRow label="Coordinates" value={selectedMessageLocation.coordinates} />
                    <MetaRow label="Horizontal accuracy" value={selectedMessageLocation.accuracy} />
                    <MetaRow label="Altitude" value={selectedMessageLocation.altitude} />
                    <MetaRow label="Speed" value={selectedMessageLocation.speed} />
                    <MetaRow label="Bearing" value={selectedMessageLocation.bearing} />
                    <MetaRow label="Location updated" value={selectedMessageLocation.updated} />
                    <Text style={styles.secondaryText}>
                      {selectedAction.direction === "outbound"
                        ? "This app attached a fresh foreground phone fix when the message was queued. "
                        : "The remote sender supplied this authenticated Sideband location snapshot. "}
                      It does not prove board position, route position, or the exact location or
                      time of RF transmission.
                    </Text>
                    {messageLocationActionError === null ? null : (
                      <Text accessibilityLiveRegion="polite" style={styles.inlineError}>
                        {messageLocationActionError}
                      </Text>
                    )}
                    <ActionButton
                      label="Open Map"
                      onPress={() => void openSelectedMessageLocation()}
                      secondary
                    />
                  </View>
                )}
                <View style={styles.messageRadioDetails}>
                  <Text style={styles.applianceDestinationLabel}>RECEIVER-LOCAL FINAL HOP</Text>
                  {selectedAction.direction === "outbound" ? (
                    <Text style={styles.secondaryText}>
                      Receiver-local ingress evidence is not available on this sender. Nearby
                      announce signal readings are not substituted.
                    </Text>
                  ) : selectedAction.ingress_observation === null ? (
                    <Text style={styles.secondaryText}>
                      No first-arrival evidence was retained for this received message. Nearby
                      announce signal readings are not substituted.
                    </Text>
                  ) : (
                    <>
                      <MetaRow
                        label="Interface ID at receipt"
                        value={String(selectedAction.ingress_observation.interface_id)}
                      />
                      {selectedAction.ingress_observation.signal === null ? (
                        <Text style={styles.secondaryText}>
                          This ingress interface did not report physical signal values.
                        </Text>
                      ) : (
                        <>
                          <MetaRow
                            label="RSSI"
                            value={`${selectedAction.ingress_observation.signal.rssi_dbm} dBm`}
                          />
                          <MetaRow
                            label="SNR"
                            value={`${selectedAction.ingress_observation.signal.snr_db} dB`}
                          />
                        </>
                      )}
                      <Text style={styles.secondaryText}>
                        Recorded by this appliance on first arrival. These values describe only the
                        final hop into this appliance; the final-hop transmitter may be a relay. The
                        numeric interface ID is historical and is not reinterpreted using current
                        interface settings.
                      </Text>
                    </>
                  )}
                </View>
                <View style={styles.messageActivityDetails}>
                  <Text style={styles.applianceDestinationLabel}>ATTEMPT HISTORY</Text>
                  <Text style={styles.secondaryText}>
                    Durable app-observed transitions are retained across retries. Intermediate
                    states may be absent when the appliance advanced between status reads.
                  </Text>
                  {messageActivityError === null ? null : (
                    <Text accessibilityLiveRegion="polite" style={styles.inlineError}>
                      Could not read message history: {messageActivityError}
                    </Text>
                  )}
                  {messageActivityLoading && messageActivityPage === null ? (
                    <ActivityIndicator color={colors.green} size="small" />
                  ) : (
                    <ActivityEventList
                      conversationPeers={[peer]}
                      emptyMessage="No retained activity is available for this message."
                      events={messageActivityPage?.events ?? []}
                    />
                  )}
                  {messageActivityPage?.history_incomplete ? (
                    <Text style={styles.secondaryText}>
                      Earlier activity predates this journal or was removed by bounded retention.
                    </Text>
                  ) : null}
                  {messageActivityPage?.next_before_event_id === null ||
                  messageActivityPage?.next_before_event_id === undefined ? null : (
                    <ActionButton
                      disabled={messageActivityLoading}
                      label={messageActivityLoading ? "Loading…" : "Load older history"}
                      onPress={() => void loadOlderMessageActivity()}
                      secondary
                    />
                  )}
                </View>
                <View style={styles.messageActivityDetails}>
                  <Text style={styles.applianceDestinationLabel}>PACKET-CORRELATED RF TRACE</Text>
                  <Text style={styles.secondaryText}>
                    Board-local route, TxDone, receive and proof evidence correlated to this message
                    row. Board times are monotonic; import time and queued phone location are not
                    exact RF timestamps or positions.
                  </Text>
                  {messageRadioTraceError === null ? null : (
                    <Text accessibilityLiveRegion="polite" style={styles.inlineError}>
                      Could not read RF trace: {messageRadioTraceError}
                    </Text>
                  )}
                  {messageRadioTraceLoading && messageRadioTracePage === null ? (
                    <ActivityIndicator color={colors.green} size="small" />
                  ) : (
                    <RadioTraceEventList
                      emptyMessage="No RF evidence is correlated to this message yet."
                      events={messageRadioTracePage?.events ?? []}
                    />
                  )}
                  {messageRadioTracePage?.history_incomplete ? (
                    <Text style={styles.secondaryText}>
                      Earlier observations were overwritten by a board&apos;s bounded trace ring.
                    </Text>
                  ) : null}
                  <Text style={styles.secondaryText}>
                    Exports can contain precise phone coordinates, peer identities, packet hashes
                    and radio timing. Message bodies and credentials are excluded.
                  </Text>
                  <View style={styles.messageTraceActions}>
                    {(["json", "csv"] as const).map((format) => (
                      <ActionButton
                        disabled={messageRadioTraceExporting !== null}
                        key={format}
                        label={
                          messageRadioTraceExporting === format
                            ? `Exporting ${format.toUpperCase()}…`
                            : `Export ${format.toUpperCase()}`
                        }
                        onPress={() => void exportMessageRadioTrace(format)}
                        secondary
                      />
                    ))}
                    {messageRadioTracePage?.next_before_event_id === null ||
                    messageRadioTracePage?.next_before_event_id === undefined ? null : (
                      <ActionButton
                        disabled={messageRadioTraceLoading}
                        label={messageRadioTraceLoading ? "Loading…" : "Load older RF events"}
                        onPress={() => void loadOlderMessageRadioTrace()}
                        secondary
                      />
                    )}
                  </View>
                </View>
                <View style={styles.messageActionUtilities}>
                  {actionCapabilities.canRetry ? (
                    <View style={styles.messageSendAgainNotice}>
                      <Text style={styles.secondaryText}>
                        Current appliances keep accepted LXMF messages pending and retry on the
                        board without the app. This action is for a legacy or permanently terminal
                        row: it keeps the outbox row and signed LXMF identity, but creates a
                        replacement durable device submission with a fresh request key.
                      </Text>
                      <ActionButton
                        disabled={busy}
                        label="Retry now"
                        onPress={() => void retryMessage(selectedAction)}
                      />
                    </View>
                  ) : null}
                  {actionCapabilities.canUseAsDraft ? (
                    <ActionButton
                      disabled={busy}
                      label="Use as draft"
                      onPress={() => populateDraft(selectedAction)}
                      secondary
                    />
                  ) : (
                    <Text style={styles.secondaryText}>
                      Binary message fields cannot be copied into the UTF-8 composer.
                    </Text>
                  )}
                </View>
              </ScrollView>
            </SafeAreaView>
          )}
        </View>
      </Modal>
    </View>
  );
}

export default function ApplianceScreen() {
  const api = useMemo(() => new ApplianceApi(), []);
  const manualServiceAnnounce = useMemo(() => {
    const announce = api.manualServiceAnnounce;
    return announce === undefined ? undefined : () => announce.call(api);
  }, [api]);
  const networkClient = useMemo<NetworkConfigurationClient | null>(() => {
    const mutateNetworkConfig = api.mutateNetworkConfig;
    const networkConfig = api.networkConfig;
    const networkStatus = api.networkStatus;
    if (
      mutateNetworkConfig === undefined ||
      networkConfig === undefined ||
      networkStatus === undefined
    ) {
      return null;
    }
    return {
      mutateNetworkConfig: (request) => mutateNetworkConfig.call(api, request),
      networkConfig: () => networkConfig.call(api),
      networkStatus: () => networkStatus.call(api),
    };
  }, [api]);
  const networkController = useMemo(
    () =>
      networkClient === null
        ? null
        : new NetworkConfigController(networkClient, {
            createIdempotencyKey: () => randomHex(16),
          }),
    [networkClient],
  );
  const radioRoutesClient = useMemo<RadioRoutesClient | null>(() => {
    const radioRoutesStatus = api.radioRoutesStatus;
    return radioRoutesStatus === undefined
      ? null
      : { radioRoutesStatus: () => radioRoutesStatus.call(api) };
  }, [api]);
  const radioRoutesController = useMemo(
    () => (radioRoutesClient === null ? null : new RadioRoutesController(radioRoutesClient)),
    [radioRoutesClient],
  );
  const fieldTelemetryClient = useMemo<FieldTelemetryClient | null>(() => {
    const observation = api.phoneLocationObservation;
    const update = api.updatePhoneLocationObservation;
    if (observation === undefined || update === undefined) return null;
    return {
      phoneLocationObservation: () => observation.call(api),
      updatePhoneLocationObservation: (next) => update.call(api, next),
    };
  }, [api]);
  const fieldTelemetryPreferenceStore = useMemo(() => createFieldTelemetryPreferenceStore(), []);
  const messageLocationPreferenceStore = useMemo(() => createMessageLocationPreferenceStore(), []);
  const fieldTelemetryController = useMemo(
    () =>
      fieldTelemetryClient === null
        ? null
        : new FieldTelemetryController(
            fieldTelemetryClient,
            undefined,
            fieldTelemetryPreferenceStore,
          ),
    [fieldTelemetryClient, fieldTelemetryPreferenceStore],
  );
  const nomadBrowser = useMemo(
    () =>
      new NomadBrowserController(api, {
        createIdempotencyKey: () => randomHex(16),
      }),
    [api],
  );
  const reticulumProbe = useMemo(
    () =>
      new ReticulumProbeController(api, {
        createIdempotencyKey: () => randomHex(16),
      }),
    [api],
  );
  const { height, width } = useWindowDimensions();
  const compact = width < 760 || height < 640;
  const [bootstrapped, setBootstrapped] = useState(false);
  const [nativeCore, setNativeCore] = useState<NativeCoreStatus | null>(null);
  const [snapshot, setSnapshot] = useState<ApplianceSnapshot | null>(null);
  const [onboarding, setOnboarding] = useState<OnboardingView>(EMPTY_ONBOARDING);
  const [profiles, setProfiles] = useState<NativeProfileStoreSnapshot | null>(null);
  const [addingAppliance, setAddingAppliance] = useState(false);
  const [contacts, setContacts] = useState<ContactView[]>([]);
  const [conversations, setConversations] = useState<ConversationPeerView[]>([]);
  const [activityPage, setActivityPage] = useState<MessageActivityPageView | null>(null);
  const [activityLoading, setActivityLoading] = useState(false);
  const [activityError, setActivityError] = useState<string | null>(null);
  const [radioTracePage, setRadioTracePage] = useState<RadioTracePageView | null>(null);
  const [radioTraceLoading, setRadioTraceLoading] = useState(false);
  const [radioTraceError, setRadioTraceError] = useState<string | null>(null);
  const [radioTraceExportError, setRadioTraceExportError] = useState<string | null>(null);
  const [radioTraceExporting, setRadioTraceExporting] = useState<RadioTraceExportFormat | null>(
    null,
  );
  const [mapFeatureEvidence, setMapFeatureEvidence] = useState<MapFeatureEvidence | null>(null);
  const [mapFeatureEvidenceLoading, setMapFeatureEvidenceLoading] = useState(false);
  const [mapFeatureEvidenceError, setMapFeatureEvidenceError] = useState<string | null>(null);
  const [foreground, setForeground] = useState(
    AppState.currentState === null || AppState.currentState === "active",
  );
  const [keyboardVisible, setKeyboardVisible] = useState(false);
  const [reconnectRetry, setReconnectRetry] = useState(0);
  const [reconnectProgress, setReconnectProgress] = useState<ForegroundReconnectProgress | null>(
    null,
  );
  const [selected, setSelected] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<TimelineView[]>([]);
  const [workspace, setWorkspace] = useState<Workspace>("lxmf");
  const [mobileSidebarVisible, setMobileSidebarVisible] = useState(false);
  const [nomadDestinationHint, setNomadDestinationHint] = useState<string | null>(null);
  const [nomadState, setNomadState] = useState<NomadBrowserState>(nomadBrowser.state);
  const [reticulumProbeState, setReticulumProbeState] = useState<ReticulumProbeState>(
    reticulumProbe.state,
  );
  const [networkState, setNetworkState] = useState<NetworkConfigControllerState | null>(
    networkController?.state ?? null,
  );
  const [radioRoutesState, setRadioRoutesState] = useState<RadioRoutesControllerState | null>(
    radioRoutesController?.state ?? null,
  );
  const [fieldTelemetryState, setFieldTelemetryState] =
    useState<FieldTelemetryControllerState | null>(fieldTelemetryController?.state ?? null);
  const [messageLocationPreference, setMessageLocationPreference] =
    useState<MessageLocationPreferenceState>({
      attachByDefault: false,
      error: null,
      loading: true,
      saving: false,
    });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [profileOperation, setProfileOperation] = useState<ProfileOperation>({ state: "idle" });
  const [messageNotificationPermission, setMessageNotificationPermission] =
    useState<MessageNotificationPermission>({ state: "checking" });
  const [messageNotificationPermissionCheckedEpoch, setMessageNotificationPermissionCheckedEpoch] =
    useState<number | null>(null);
  const [messageNotificationError, setMessageNotificationError] = useState<string | null>(null);
  const [messageNotificationTargets, setMessageNotificationTargets] = useState<
    readonly MessageNotificationTarget[]
  >([]);
  const addingApplianceRef = useRef(false);
  const draft = useRef<DraftSubmission | null>(null);
  const retryMessageRequests = useRef(new Map<string, RetrySendRequest>());
  const activityPageRef = useRef<MessageActivityPageView | null>(null);
  const activityRequests = useRef(new LatestRequest());
  const activityReadInFlight = useRef<number | null>(null);
  const radioTracePageRef = useRef<RadioTracePageView | null>(null);
  const radioTraceRequests = useRef(new LatestRequest());
  const radioTraceReadInFlight = useRef<number | null>(null);
  const mapFeatureEvidenceRequests = useRef(new LatestRequest());
  const mapProfileKeyRef = useRef<string | null>(null);
  const mutationInFlight = useRef(false);
  const sendTimelineRefreshesInFlight = useRef(0);
  const refreshRequests = useRef(new LatestRequest());
  const selectedRef = useRef<string | null>(null);
  const timelineRequests = useRef(new LatestRequest());
  const notificationNavigationInFlight = useRef(false);
  const messageNotificationProfileEpoch = useRef(0);
  const messageNotificationPermissionEpoch = useRef(0);
  const messageNotificationProfileKeyRef = useRef<string | null>(null);
  const notificationActivateProfile = useRef<(profileKey: string) => Promise<boolean>>(
    async () => false,
  );
  const notificationChooseContact = useRef<(destination: string) => void>(() => undefined);
  const foregroundRef = useRef(foreground);
  const mobileSidebarVisibleRef = useRef(mobileSidebarVisible);
  const workspaceRef = useRef(workspace);
  const messageNotificationReconciler = useMemo(
    () => new MessageNotificationReconciler(createMessageNotificationLedgerStore()),
    [],
  );
  const automaticReconnect = useMemo(
    () =>
      new ForegroundReconnect(
        () => setReconnectRetry((generation) => generation + 1),
        FOREGROUND_RECONNECT_DELAY_MS,
      ),
    [],
  );
  foregroundRef.current = foreground;
  mobileSidebarVisibleRef.current = mobileSidebarVisible;
  workspaceRef.current = workspace;

  const ready = onboardingPresentation(onboarding).ready;
  // Missing credentials can make the dormant connector report an expected
  // local error. The onboarding panel owns that state until setup is ready.
  const displayedError =
    error ??
    (ready && (reconnectProgress === null || snapshot?.connection.state === "faulted")
      ? snapshot?.last_error
      : null);
  const selectedConversation = conversations.find((peer) => peer.destination === selected);
  const networkDeviceKey = profiles?.activeProfileKey ?? snapshot?.device?.device_id ?? null;
  mapProfileKeyRef.current = networkDeviceKey;
  const mapLocatedTimelines = useMemo<LocatedTimeline[]>(() => {
    const located: LocatedTimeline[] = [];
    for (const conversation of conversations) {
      if (conversation.last_message !== null) {
        located.push({
          peer: conversation.destination,
          peerName: conversation.name,
          timeline: conversation.last_message,
        });
      }
    }
    return located;
  }, [conversations]);
  const scopedMapFeatureEvidence =
    mapFeatureEvidence?.profileKey === networkDeviceKey ? mapFeatureEvidence : null;
  const mapRadioTraceEvents = useMemo(
    () => [
      ...new Map(
        [...(radioTracePage?.events ?? []), ...(scopedMapFeatureEvidence?.events ?? [])].map(
          (event) => [event.event_id, event] as const,
        ),
      ).values(),
    ],
    [radioTracePage, scopedMapFeatureEvidence],
  );
  const transmissionMapScene = useMemo(
    () =>
      buildTransmissionMapScene({
        activityHistoryIncomplete: activityPage?.history_incomplete ?? false,
        contacts,
        conversationPeers: conversations,
        locatedTimelines: mapLocatedTimelines,
        messageActivityEvents: activityPage?.events ?? [],
        profileKey: networkDeviceKey,
        radioTraceEvents: mapRadioTraceEvents,
        radioTraceHistoryIncomplete:
          (radioTracePage?.history_incomplete ?? false) ||
          (scopedMapFeatureEvidence?.historyIncomplete ?? false),
      }),
    [
      activityPage,
      contacts,
      conversations,
      mapLocatedTimelines,
      mapRadioTraceEvents,
      networkDeviceKey,
      radioTracePage?.history_incomplete,
      scopedMapFeatureEvidence?.historyIncomplete,
    ],
  );
  const messageNotificationProfileKey = networkDeviceKey;
  messageNotificationProfileKeyRef.current = messageNotificationProfileKey;
  const messageNotificationBoardLabel =
    (profiles === null
      ? null
      : applianceProfilesPresentation(profiles).activeProfile?.boardLabel) ??
    applianceStatusPresentation(snapshot).boardLabel;
  const connectivityAvailable =
    networkController !== null &&
    networkDeviceKey !== null &&
    snapshot?.connection.state === "ready";
  const canManageProfiles = api.profiles !== undefined && api.activateProfile !== undefined;
  const hasSavedProfiles = (profiles?.profiles.length ?? 0) > 0;
  const canAddAppliance =
    api.beginAddAppliance !== undefined && (api.supportsAdditionalBleOnboarding?.() ?? true);
  const exactBleTargetRequired = api.supportsBleCandidateDiscovery?.() ?? false;
  const nearbyReader = useMemo(() => {
    const read = api.nearbyPeers;
    return read === undefined ? null : () => read.call(api);
  }, [api]);
  const resetActivity = useCallback(() => {
    activityRequests.current.invalidate();
    activityReadInFlight.current = null;
    activityPageRef.current = null;
    setActivityPage(null);
    setActivityError(null);
    setActivityLoading(false);
  }, []);
  const loadActivity = useCallback(
    async (older = false) => {
      if (activityReadInFlight.current !== null) return;
      const retained = activityPageRef.current;
      const beforeEventId = older ? retained?.next_before_event_id : null;
      if (older && (beforeEventId === null || beforeEventId === undefined)) return;

      const request = activityRequests.current.begin();
      activityReadInFlight.current = request;
      setActivityLoading(true);
      setActivityError(null);
      try {
        const next = await api.messageActivity({
          before_event_id: beforeEventId ?? null,
          limit: MESSAGE_ACTIVITY_PAGE_SIZE,
          timeline_sequence: null,
        });
        if (!activityRequests.current.accepts(request)) return;
        const merged =
          older && retained !== null
            ? {
                events: [...retained.events, ...next.events],
                next_before_event_id: next.next_before_event_id,
                history_incomplete: retained.history_incomplete || next.history_incomplete,
              }
            : next;
        activityPageRef.current = merged;
        setActivityPage(merged);
      } catch (nextError) {
        if (activityRequests.current.accepts(request)) setActivityError(errorText(nextError));
      } finally {
        if (activityReadInFlight.current === request) activityReadInFlight.current = null;
        if (activityRequests.current.accepts(request)) setActivityLoading(false);
      }
    },
    [api],
  );
  const loadMessageActivity = useCallback(
    (timelineSequence: number, beforeEventId: number | null) =>
      api.messageActivity({
        before_event_id: beforeEventId,
        limit: MESSAGE_ACTIVITY_PAGE_SIZE,
        timeline_sequence: timelineSequence,
      }),
    [api],
  );
  const resetRadioTrace = useCallback(() => {
    radioTraceRequests.current.invalidate();
    radioTraceReadInFlight.current = null;
    radioTracePageRef.current = null;
    setRadioTracePage(null);
    setRadioTraceError(null);
    setRadioTraceLoading(false);
    setRadioTraceExportError(null);
    setRadioTraceExporting(null);
  }, []);
  const loadRadioTrace = useCallback(
    async (older = false) => {
      const read = api.radioTrace;
      if (read === undefined || radioTraceReadInFlight.current !== null) return;
      const retained = radioTracePageRef.current;
      const beforeEventId = older ? retained?.next_before_event_id : null;
      if (older && (beforeEventId === null || beforeEventId === undefined)) return;

      const request = radioTraceRequests.current.begin();
      radioTraceReadInFlight.current = request;
      setRadioTraceLoading(true);
      setRadioTraceError(null);
      try {
        const next = await read.call(api, {
          before_event_id: beforeEventId ?? null,
          limit: RADIO_TRACE_PAGE_SIZE,
          timeline_sequence: null,
        });
        if (!radioTraceRequests.current.accepts(request)) return;
        const merged =
          older && retained !== null
            ? {
                events: [...retained.events, ...next.events],
                next_before_event_id: next.next_before_event_id,
                history_incomplete: retained.history_incomplete || next.history_incomplete,
              }
            : next;
        radioTracePageRef.current = merged;
        setRadioTracePage(merged);
      } catch (nextError) {
        if (radioTraceRequests.current.accepts(request)) {
          setRadioTraceError(errorText(nextError));
        }
      } finally {
        if (radioTraceReadInFlight.current === request) radioTraceReadInFlight.current = null;
        if (radioTraceRequests.current.accepts(request)) setRadioTraceLoading(false);
      }
    },
    [api],
  );
  const loadMessageRadioTrace = useCallback(
    (timelineSequence: number, beforeEventId: number | null) => {
      const read = api.radioTrace;
      if (read === undefined) throw new Error("Durable RF trace is unavailable in this client");
      return read.call(api, {
        before_event_id: beforeEventId,
        limit: RADIO_TRACE_PAGE_SIZE,
        timeline_sequence: timelineSequence,
      });
    },
    [api],
  );
  const selectMapFeature = useCallback(
    (details: TransmissionMapFeatureDetails | null) => {
      const request = mapFeatureEvidenceRequests.current.begin();
      setMapFeatureEvidence(null);
      setMapFeatureEvidenceError(null);
      if (
        details?.kind !== "attempt" ||
        details.timelineSequence === null ||
        api.radioTrace === undefined ||
        networkDeviceKey === null
      ) {
        setMapFeatureEvidenceLoading(false);
        return;
      }

      const profileKey = networkDeviceKey;
      const timelineSequence = details.timelineSequence;
      const read = api.radioTrace;
      setMapFeatureEvidenceLoading(true);
      void collectCompleteRadioTrace((pageRequest) => read.call(api, pageRequest), timelineSequence)
        .then((collection) => {
          if (
            !mapFeatureEvidenceRequests.current.accepts(request) ||
            mapProfileKeyRef.current !== profileKey
          ) {
            return;
          }
          setMapFeatureEvidence({
            events: collection.events,
            historyIncomplete: collection.historyIncomplete,
            profileKey,
            timelineSequence,
          });
        })
        .catch((nextError) => {
          if (
            mapFeatureEvidenceRequests.current.accepts(request) &&
            mapProfileKeyRef.current === profileKey
          ) {
            setMapFeatureEvidenceError(errorText(nextError));
          }
        })
        .finally(() => {
          if (
            mapFeatureEvidenceRequests.current.accepts(request) &&
            mapProfileKeyRef.current === profileKey
          ) {
            setMapFeatureEvidenceLoading(false);
          }
        });
    },
    [api, networkDeviceKey],
  );
  const exportRadioTrace = useCallback(
    async (timelineSequence: number | null, format: RadioTraceExportFormat) => {
      const read = api.radioTrace;
      if (read === undefined) throw new Error("Durable RF trace is unavailable in this client");
      const collection = await collectCompleteRadioTrace(
        (request) => read.call(api, request),
        timelineSequence,
      );
      const document = createRadioTraceExportDocument({
        collection,
        exportedAtUnixMs: Date.now(),
        source: {
          board_label: messageNotificationBoardLabel,
          device_id: snapshot?.device?.device_id ?? null,
          lxmf_delivery_destination: snapshot?.device?.lxmf_delivery_destination ?? null,
          primary_destination: snapshot?.device?.primary_destination ?? null,
          profile_key: profiles?.activeProfileKey ?? null,
        },
        timelineSequence,
      });
      await deliverExportArtifact(
        format === "json" ? radioTraceJsonArtifact(document) : radioTraceCsvArtifact(document),
      );
    },
    [api, messageNotificationBoardLabel, profiles?.activeProfileKey, snapshot?.device],
  );
  const exportCompleteRadioTrace = useCallback(
    async (format: RadioTraceExportFormat) => {
      if (radioTraceExporting !== null) return;
      setRadioTraceExporting(format);
      setRadioTraceExportError(null);
      try {
        await exportRadioTrace(null, format);
      } catch (nextError) {
        setRadioTraceExportError(errorText(nextError));
      } finally {
        setRadioTraceExporting(null);
      }
    },
    [exportRadioTrace, radioTraceExporting],
  );
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

  useEffect(() => {
    let active = true;
    const unsubscribe = subscribeMessageNotificationTargets((target) => {
      if (active) {
        setMessageNotificationTargets((queue) => enqueueMessageNotificationTarget(queue, target));
      }
    });
    const initialTarget = consumeInitialMessageNotificationTarget();
    if (initialTarget !== null) {
      setMessageNotificationTargets((queue) =>
        enqueueMessageNotificationTarget(queue, initialTarget),
      );
    }
    return () => {
      active = false;
      unsubscribe();
    };
  }, []);

  useEffect(() => {
    if (!foreground) return;
    const permissionEpoch = messageNotificationPermissionEpoch.current + 1;
    messageNotificationPermissionEpoch.current = permissionEpoch;
    setMessageNotificationPermission({ state: "checking" });
    setMessageNotificationPermissionCheckedEpoch(null);
    let active = true;
    void initializeMessageNotifications().then((permission) => {
      if (active && messageNotificationPermissionEpoch.current === permissionEpoch) {
        setMessageNotificationPermission(permission);
        setMessageNotificationPermissionCheckedEpoch(permissionEpoch);
      }
    });
    return () => {
      active = false;
    };
  }, [foreground]);

  useEffect(() => () => api.dispose(), [api]);

  useEffect(() => {
    const unsubscribe = nomadBrowser.subscribe(setNomadState);
    return () => {
      unsubscribe();
      nomadBrowser.dispose();
    };
  }, [nomadBrowser]);

  useEffect(() => {
    const unsubscribe = reticulumProbe.subscribe(setReticulumProbeState);
    return () => {
      unsubscribe();
      reticulumProbe.dispose();
    };
  }, [reticulumProbe]);

  useEffect(() => {
    if (networkController === null) {
      setNetworkState(null);
      return;
    }
    const unsubscribe = networkController.subscribe(setNetworkState);
    return () => {
      unsubscribe();
      networkController.dispose();
    };
  }, [networkController]);

  useEffect(() => {
    if (radioRoutesController === null) {
      setRadioRoutesState(null);
      return;
    }
    const unsubscribe = radioRoutesController.subscribe(setRadioRoutesState);
    return () => {
      unsubscribe();
      radioRoutesController.dispose();
    };
  }, [radioRoutesController]);

  useEffect(() => {
    if (fieldTelemetryController === null) {
      setFieldTelemetryState(null);
      return;
    }
    const unsubscribe = fieldTelemetryController.subscribe(setFieldTelemetryState);
    return () => {
      unsubscribe();
      fieldTelemetryController.dispose();
    };
  }, [fieldTelemetryController]);

  useEffect(() => {
    let active = true;
    void messageLocationPreferenceStore
      .load()
      .then((attachByDefault) => {
        if (!active) return;
        setMessageLocationPreference({
          attachByDefault,
          error: null,
          loading: false,
          saving: false,
        });
      })
      .catch((nextError) => {
        if (!active) return;
        setMessageLocationPreference({
          attachByDefault: false,
          error: `Saved default could not be loaded: ${errorText(nextError)}`,
          loading: false,
          saving: false,
        });
      });
    return () => {
      active = false;
    };
  }, [messageLocationPreferenceStore]);

  const setMessageLocationDefault = useCallback(
    async (attachByDefault: boolean) => {
      setMessageLocationPreference((current) => ({
        ...current,
        error: null,
        saving: true,
      }));
      try {
        await messageLocationPreferenceStore.save(attachByDefault);
        setMessageLocationPreference({
          attachByDefault,
          error: null,
          loading: false,
          saving: false,
        });
      } catch (nextError) {
        setMessageLocationPreference((current) => ({
          ...current,
          error: `Default was not saved: ${errorText(nextError)}`,
          loading: false,
          saving: false,
        }));
      }
    },
    [messageLocationPreferenceStore],
  );

  useEffect(() => {
    if (
      networkController !== null &&
      workspace === "connectivity" &&
      connectivityAvailable &&
      foreground &&
      networkDeviceKey !== null
    ) {
      void networkController.activate(networkDeviceKey);
      return;
    }
    networkController?.suspend();
  }, [connectivityAvailable, foreground, networkController, networkDeviceKey, workspace]);

  useEffect(() => {
    if (
      radioRoutesController !== null &&
      workspace === "connectivity" &&
      connectivityAvailable &&
      foreground &&
      networkDeviceKey !== null
    ) {
      void radioRoutesController.activate(networkDeviceKey);
      return;
    }
    radioRoutesController?.suspend();
  }, [connectivityAvailable, foreground, networkDeviceKey, radioRoutesController, workspace]);

  useEffect(() => {
    if (
      fieldTelemetryController !== null &&
      bootstrapped &&
      foreground &&
      networkDeviceKey !== null
    ) {
      void fieldTelemetryController.activate(networkDeviceKey);
      return;
    }
    fieldTelemetryController?.suspend();
  }, [bootstrapped, fieldTelemetryController, foreground, networkDeviceKey]);

  useEffect(() => {
    if (workspace === "connectivity" && !connectivityAvailable) setWorkspace("lxmf");
  }, [connectivityAvailable, workspace]);

  useEffect(() => {
    // A boot-scoped probe identifier must never survive an appliance switch.
    void networkDeviceKey;
    reticulumProbe.reset();
  }, [networkDeviceKey, reticulumProbe]);

  useEffect(() => {
    // Selection evidence is profile-local and must not survive an appliance switch.
    void networkDeviceKey;
    mapFeatureEvidenceRequests.current.invalidate();
    setMapFeatureEvidence(null);
    setMapFeatureEvidenceError(null);
    setMapFeatureEvidenceLoading(false);
  }, [networkDeviceKey]);

  useEffect(() => {
    if (
      bootstrapped &&
      ready &&
      networkDeviceKey !== null &&
      (workspace === "activity" || workspace === "map")
    ) {
      void loadActivity(false);
      void loadRadioTrace(false);
    }
  }, [bootstrapped, loadActivity, loadRadioTrace, networkDeviceKey, ready, workspace]);

  useEffect(() => {
    const subscription = AppState.addEventListener("change", (state) => {
      const active = state === "active";
      foregroundRef.current = active;
      setForeground(active);
    });
    return () => subscription.remove();
  }, []);

  useEffect(() => {
    if (!KEYBOARD_LAYOUT.avoidingEnabled) return;
    const show = Keyboard.addListener("keyboardDidShow", () => setKeyboardVisible(true));
    const hide = Keyboard.addListener("keyboardDidHide", () => setKeyboardVisible(false));
    return () => {
      show.remove();
      hide.remove();
    };
  }, []);

  useEffect(() => () => automaticReconnect.suspend(), [automaticReconnect]);

  const refresh = useCallback(async () => {
    const refreshRequest = refreshRequests.current.begin();
    try {
      const [nextOnboarding, nextProfiles] = await Promise.all([
        api.onboarding(),
        api.profiles?.() ?? Promise.resolve(null),
      ]);
      if (!refreshRequests.current.accepts(refreshRequest)) return;
      setOnboarding(nextOnboarding);
      setProfiles(nextProfiles);
      const nextReady = onboardingPresentation(nextOnboarding).ready;
      const completingAdditionalAppliance = addingApplianceRef.current && nextReady;
      if (addingApplianceRef.current && !nextReady) {
        timelineRequests.current.invalidate();
        return;
      }
      if (completingAdditionalAppliance) {
        addingApplianceRef.current = false;
        setAddingAppliance(false);
        timelineRequests.current.invalidate();
        selectedRef.current = null;
        draft.current = null;
        retryMessageRequests.current.clear();
        setSelected(null);
        setTimeline([]);
        setContacts([]);
        setConversations([]);
        resetActivity();
        resetRadioTrace();
        nomadBrowser.reset();
      }

      if (!nextReady) {
        const nextSnapshot = await api.snapshot();
        if (!refreshRequests.current.accepts(refreshRequest)) return;
        setSnapshot(nextSnapshot);
        timelineRequests.current.invalidate();
        return;
      }

      // Queue all ready-state local reads in one foreground burst. The native
      // actor drains already-queued commands before beginning another blocking
      // BLE exchange; staging timeline as a later wave could add a complete
      // device-request timeout to an otherwise local UI refresh.
      const selectedDestination = selectedRef.current;
      const timelineRequest =
        selectedDestination === null ? null : timelineRequests.current.begin();
      type TimelineRead =
        | { readonly ok: true; readonly value: TimelineView[] }
        | { readonly error: unknown; readonly ok: false };
      const timelineRead: Promise<TimelineRead | null> =
        selectedDestination === null
          ? Promise.resolve(null)
          : api.timeline(selectedDestination).then(
              (value) => ({ ok: true, value }),
              (nextError: unknown) => ({ error: nextError, ok: false }),
            );
      const [nextSnapshot, nextContacts, nextConversations, nextTimelineRead] = await Promise.all([
        api.snapshot(),
        api.contacts(),
        api.conversationPeers(),
        timelineRead,
      ]);
      if (!refreshRequests.current.accepts(refreshRequest)) return;
      setSnapshot(nextSnapshot);
      setContacts(nextContacts);
      setConversations(nextConversations);

      if (selectedDestination !== null) {
        const timelineStillCurrent =
          timelineRequest !== null &&
          timelineRequests.current.accepts(timelineRequest) &&
          selectedRef.current === selectedDestination;
        if (!nextConversations.some((peer) => peer.destination === selectedDestination)) {
          if (timelineStillCurrent) {
            timelineRequests.current.invalidate();
            selectedRef.current = null;
            setSelected(null);
            setTimeline([]);
          }
        } else if (timelineStillCurrent && nextTimelineRead?.ok) {
          setTimeline(nextTimelineRead.value);
        } else if (timelineStillCurrent && nextTimelineRead !== null && !nextTimelineRead.ok) {
          throw nextTimelineRead.error;
        }
      }
      setError(null);
    } catch (nextError) {
      if (refreshRequests.current.accepts(refreshRequest)) throw nextError;
    }
  }, [api, nomadBrowser, resetActivity, resetRadioTrace]);

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
    if (!bootstrapped || profileOperation.state === "switching") return;
    const unsubscribe = api.subscribeInvalidations(
      () => void refresh().catch((nextError) => setError(errorText(nextError))),
      () => setError("Event stream reconnecting"),
    );
    const poll =
      !ready || unsubscribe === null
        ? new SettledPoll(
            () => refresh().catch((nextError) => setError(errorText(nextError))),
            ready ? 2_000 : 500,
            () => mutationInFlight.current || sendTimelineRefreshesInFlight.current > 0,
          )
        : null;
    poll?.start();
    return () => {
      unsubscribe?.();
      poll?.stop();
    };
  }, [api, bootstrapped, profileOperation.state, ready, refresh]);

  useEffect(() => {
    if (
      addingAppliance ||
      !bootstrapped ||
      !foreground ||
      !ready ||
      profileOperation.state === "switching" ||
      messageNotificationPermission.state !== "enabled" ||
      messageNotificationPermissionCheckedEpoch !== messageNotificationPermissionEpoch.current ||
      messageNotificationProfileKey === null
    ) {
      return;
    }
    const profileEpoch = messageNotificationProfileEpoch.current;
    const permissionEpoch = messageNotificationPermissionEpoch.current;
    const profileKey = messageNotificationProfileKey.trim().toLowerCase();
    const profileIsCurrent = () =>
      messageNotificationProfileEpoch.current === profileEpoch &&
      messageNotificationPermissionEpoch.current === permissionEpoch &&
      foregroundRef.current &&
      messageNotificationProfileKeyRef.current?.trim().toLowerCase() === profileKey;
    const aliases = buildMessageActivityAliases(contacts, conversations);
    void messageNotificationReconciler
      .reconcile({
        isCurrent: profileIsCurrent,
        loadPage: (beforeEventId) =>
          api.messageActivity({
            before_event_id: beforeEventId,
            limit: MESSAGE_NOTIFICATION_PAGE_SIZE,
            timeline_sequence: null,
          }),
        notify: async (notification) => {
          if (
            !shouldPresentInboundMessageNotification(notification.peer, {
              foreground: foregroundRef.current,
              navigationOverlayVisible: mobileSidebarVisibleRef.current,
              selectedDestination: selectedRef.current,
              workspace: workspaceRef.current,
            })
          ) {
            return;
          }
          await presentInboundMessageNotification({
            boardLabel: messageNotificationBoardLabel,
            notification,
            peerLabel: messageActivityPeerLabel(
              { direction: "inbound", peer: notification.peer },
              aliases,
            ),
            profileKey,
          });
        },
        profileKey,
      })
      .then(() => setMessageNotificationError(null))
      .catch((nextError) => {
        if (nextError instanceof SupersededMessageNotificationReconciliation) return;
        setMessageNotificationError(`Message notification failed: ${errorText(nextError)}`);
      });
  }, [
    addingAppliance,
    api,
    bootstrapped,
    contacts,
    conversations,
    foreground,
    messageNotificationBoardLabel,
    messageNotificationPermissionCheckedEpoch,
    messageNotificationPermission.state,
    messageNotificationProfileKey,
    messageNotificationReconciler,
    profileOperation.state,
    ready,
  ]);

  useEffect(() => {
    if (
      addingAppliance ||
      busy ||
      !foreground ||
      !onboarding.available ||
      !ready ||
      snapshot?.connection.state === "ready"
    ) {
      automaticReconnect.suspend();
      setReconnectProgress(null);
      return;
    }
    if (!automaticReconnect.begin(reconnectRetry)) return;

    let active = true;
    setReconnectProgress({ state: "attempting" });
    void ensureForegroundConnection(api)
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
    addingAppliance,
    automaticReconnect,
    busy,
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

  const profileLabel = (profileKey: string | undefined): string => {
    if (profileKey === undefined || profiles === null) return "active appliance";
    const normalizedProfileKey = profileKey.trim().toLowerCase();
    return (
      applianceProfilesPresentation(profiles).profiles.find(
        (profile) => profile.profileKey.toLowerCase() === normalizedProfileKey,
      )?.boardLabel ?? "active appliance"
    );
  };

  const runActiveProfileOperation = async (
    startedMessage: string,
    successMessage: string,
    operation: () => Promise<unknown>,
  ): Promise<boolean> => {
    if (mutationInFlight.current) return false;
    mutationInFlight.current = true;
    automaticReconnect.suspend();
    setReconnectProgress(null);
    setBusy(true);
    setError(null);
    setProfileOperation({ message: startedMessage, state: "switching" });

    let operationFailure: unknown;
    try {
      await operation();
    } catch (nextError) {
      operationFailure = nextError;
    }

    let authorityFailure: unknown;
    try {
      const authoritativeProfiles = await api.profiles?.();
      if (authoritativeProfiles !== undefined) setProfiles(authoritativeProfiles);
    } catch (nextError) {
      authorityFailure = nextError;
    }

    let refreshFailure: unknown;
    try {
      await refresh();
    } catch (nextError) {
      refreshFailure = nextError;
    }

    const failure = operationFailure ?? authorityFailure ?? refreshFailure;
    if (failure === undefined) {
      setProfileOperation({ message: successMessage, state: "success" });
    } else {
      const prefix =
        operationFailure === undefined
          ? `${successMessage} The action completed, but the authoritative display refresh failed:`
          : "The appliance operation failed:";
      setProfileOperation({ message: `${prefix} ${errorText(failure)}`, state: "error" });
    }

    mutationInFlight.current = false;
    setBusy(false);
    return failure === undefined;
  };

  const reconnectActiveProfile = (): Promise<boolean> => {
    const label = profileLabel(profiles?.activeProfileKey);
    automaticReconnect.allow();
    return runActiveProfileOperation(`Reconnecting to ${label}…`, `Reconnected to ${label}.`, () =>
      api.reconnect(),
    );
  };

  const repairActiveBleBond = async (): Promise<boolean> => {
    const repair = api.repairBleBond;
    if (repair === undefined) {
      setProfileOperation({
        message: "Bluetooth bond repair is unavailable for this client.",
        state: "error",
      });
      return false;
    }
    const label = profileLabel(profiles?.activeProfileKey);
    automaticReconnect.inhibit();
    let repairSucceeded = false;
    const completed = await runActiveProfileOperation(
      `Finding ${label}… The board must already show BLE Recovery from a reset-time GPIO21 hold. Keep GPIO21 released during discovery; hold it again for about two seconds only when the app asks for physical presence.`,
      `Bluetooth bond repaired for ${label}; the saved appliance data was retained.`,
      async () => {
        await repair.call(api, (stage) => {
          setProfileOperation({
            message: bleBondRepairProgressMessage(stage, label),
            state: "switching",
          });
        });
        repairSucceeded = true;
      },
    );
    if (repairSucceeded) automaticReconnect.allow();
    return completed;
  };

  const forgetInactiveProfile = async (profileKey: string): Promise<boolean> => {
    const forget = api.forgetProfile;
    const normalizedProfileKey = profileKey.trim().toLowerCase();
    const label = profileLabel(profileKey);
    if (forget === undefined) {
      setProfileOperation({
        message: "Forgetting saved appliance profiles is unavailable for this client.",
        state: "error",
      });
      return false;
    }
    if (profiles?.activeProfileKey?.trim().toLowerCase() === normalizedProfileKey) {
      setProfileOperation({
        message: `Switch to another appliance before forgetting ${label}.`,
        state: "error",
      });
      return false;
    }
    if (mutationInFlight.current) return false;

    mutationInFlight.current = true;
    automaticReconnect.suspend();
    setReconnectProgress(null);
    setBusy(true);
    setError(null);
    setProfileOperation({ message: `Deleting local data for ${label}…`, state: "switching" });

    let forgetFailure: unknown;
    try {
      await forget.call(api, profileKey);
    } catch (nextError) {
      forgetFailure = nextError;
    }

    let authoritativeProfiles: NativeProfileStoreSnapshot | null = null;
    let authorityFailure: unknown;
    try {
      authoritativeProfiles = (await api.profiles?.()) ?? null;
      if (authoritativeProfiles !== null) setProfiles(authoritativeProfiles);
    } catch (nextError) {
      authorityFailure = nextError;
    }

    let refreshFailure: unknown;
    try {
      await refresh();
    } catch (nextError) {
      refreshFailure = nextError;
    }

    const profileRemoved =
      authoritativeProfiles !== null &&
      !authoritativeProfiles.profiles.some(
        (profile) => profile.profileKey.trim().toLowerCase() === normalizedProfileKey,
      );
    let notificationLedgerFailure: unknown;
    if (profileRemoved) {
      try {
        await messageNotificationReconciler.forgetProfile(profileKey);
      } catch (nextError) {
        notificationLedgerFailure = nextError;
      }
    }
    const failure =
      profileRemoved && forgetFailure !== undefined
        ? (authorityFailure ?? refreshFailure ?? notificationLedgerFailure)
        : (forgetFailure ??
          authorityFailure ??
          refreshFailure ??
          notificationLedgerFailure ??
          new Error("the authoritative profile store still lists this appliance"));
    if (failure === undefined) {
      setProfileOperation({
        message: `Deleted ${label}'s local credential, messages, contacts, and outbox. The board credential and Bluetooth bond were not revoked.`,
        state: "success",
      });
    } else {
      setProfileOperation({
        message: `Could not forget ${label}: ${errorText(failure)}`,
        state: "error",
      });
    }

    mutationInFlight.current = false;
    setBusy(false);
    return failure === undefined;
  };

  const activateProfileWithAuthority = async (profileKey: string): Promise<boolean> => {
    const activate = api.activateProfile;
    if (activate === undefined) return false;
    messageNotificationProfileEpoch.current += 1;
    const normalizedProfileKey = profileKey.trim().toLowerCase();
    const targetLabel =
      (profiles === null
        ? null
        : applianceProfilesPresentation(profiles).profiles.find(
            (profile) => profile.profileKey.toLowerCase() === normalizedProfileKey,
          )?.boardLabel) ?? profileKey;

    automaticReconnect.allow();
    automaticReconnect.suspend();
    setWorkspace("lxmf");
    setReconnectProgress(null);
    refreshRequests.current.invalidate();
    timelineRequests.current.invalidate();
    selectedRef.current = null;
    draft.current = null;
    retryMessageRequests.current.clear();
    setSelected(null);
    setTimeline([]);
    setContacts([]);
    setConversations([]);
    resetActivity();
    resetRadioTrace();
    nomadBrowser.reset();
    setError(null);
    setProfileOperation({ message: `Switching to ${targetLabel}…`, state: "switching" });

    let activationFailure: unknown;
    try {
      await activate.call(api, profileKey);
    } catch (nextError) {
      activationFailure = nextError;
    }

    let authoritativeProfiles: NativeProfileStoreSnapshot | null = null;
    let authorityFailure: unknown;
    try {
      authoritativeProfiles = (await api.profiles?.()) ?? null;
      if (authoritativeProfiles !== null) setProfiles(authoritativeProfiles);
    } catch (nextError) {
      authorityFailure = nextError;
    }

    let refreshFailure: unknown;
    try {
      await refresh();
    } catch (nextError) {
      refreshFailure = nextError;
    }

    if (authoritativeProfiles === null && api.profiles !== undefined) {
      try {
        authoritativeProfiles = await api.profiles();
        setProfiles(authoritativeProfiles);
        authorityFailure = undefined;
      } catch (nextError) {
        authorityFailure = nextError;
      }
    }

    const activeProfileKey = authoritativeProfiles?.activeProfileKey?.toLowerCase();
    const targetIsActive = activeProfileKey === normalizedProfileKey;
    if (targetIsActive) {
      if (
        activationFailure !== undefined ||
        authorityFailure !== undefined ||
        refreshFailure !== undefined
      ) {
        const failure = activationFailure ?? authorityFailure ?? refreshFailure;
        const message =
          `${targetLabel} is now active, but the switch needs attention: ` +
          `${errorText(failure)}. Close Appliances and use Reconnect if needed.`;
        setProfileOperation({
          message,
          state: "error",
        });
      } else {
        setProfileOperation({ message: `Switched to ${targetLabel}.`, state: "success" });
      }
      return true;
    }

    const failure = activationFailure ?? authorityFailure ?? refreshFailure;
    const authority =
      activeProfileKey === undefined
        ? "The authoritative active profile could not be confirmed."
        : "A different appliance profile remains active.";
    const message =
      `Could not switch to ${targetLabel}. ${authority}` +
      (failure === undefined ? "" : ` ${errorText(failure)}`);
    setProfileOperation({
      message,
      state: "error",
    });
    return false;
  };

  const activateApplianceProfile = async (profileKey: string): Promise<boolean> => {
    if (mutationInFlight.current) return false;
    mutationInFlight.current = true;
    setBusy(true);
    try {
      return await activateProfileWithAuthority(profileKey);
    } finally {
      mutationInFlight.current = false;
      setBusy(false);
    }
  };

  const beginAddAppliance = () => {
    const begin = api.beginAddAppliance;
    if (begin === undefined || addingApplianceRef.current) return;
    messageNotificationProfileEpoch.current += 1;
    automaticReconnect.allow();
    automaticReconnect.suspend();
    setWorkspace("lxmf");
    setReconnectProgress(null);
    refreshRequests.current.invalidate();
    timelineRequests.current.invalidate();
    nomadBrowser.reset();
    addingApplianceRef.current = true;
    setAddingAppliance(true);
    void run(async () => {
      try {
        await begin.call(api);
      } catch (nextError) {
        addingApplianceRef.current = false;
        setAddingAppliance(false);
        throw nextError;
      }
    });
  };

  const switchToKnownProfile = (profileKey: string) => {
    if (
      mutationInFlight.current ||
      api.cancelOnboarding === undefined ||
      api.activateProfile === undefined
    ) {
      return;
    }
    const targetLabel =
      (profiles === null
        ? null
        : applianceProfilesPresentation(profiles).profiles.find(
            (profile) => profile.profileKey === profileKey,
          )?.boardLabel) ?? profileKey;
    automaticReconnect.allow();
    mutationInFlight.current = true;
    setBusy(true);
    setError(null);
    setProfileOperation({
      message: `Closing discovery and switching to saved appliance ${targetLabel}…`,
      state: "switching",
    });
    void (async () => {
      try {
        try {
          await api.cancelOnboarding?.();
        } catch (nextError) {
          try {
            await refresh();
          } catch {
            // The cancellation failure remains the useful recovery message.
          }
          const message = `Could not leave Add appliance safely: ${errorText(nextError)}`;
          setProfileOperation({
            message,
            state: "error",
          });
          return;
        }
        addingApplianceRef.current = false;
        setAddingAppliance(false);
        await activateProfileWithAuthority(profileKey);
      } finally {
        mutationInFlight.current = false;
        setBusy(false);
      }
    })();
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
          await refresh();
        };

  const upsertContact = (
    destination: string,
    name: string,
    selectAfterSave = true,
  ): Promise<boolean> =>
    run(async () => {
      await api.upsertContact(destination, { name });
      if (!selectAfterSave) return;
      if (selectedRef.current !== destination) draft.current = null;
      selectedRef.current = destination;
      timelineRequests.current.invalidate();
      setSelected(destination);
      setTimeline([]);
    });

  const send = async (
    title: string,
    content: string,
    attachLocation: boolean,
  ): Promise<QueueMessageResult> => {
    if (selected === null) {
      return { acceptance: null, error: "Select a recipient first", queued: false };
    }
    if (mutationInFlight.current) {
      return {
        acceptance: null,
        error: "Another appliance action is already in progress",
        queued: false,
      };
    }

    const destination = selected;
    mutationInFlight.current = true;
    try {
      const submission = await prepareDraftSubmission(
        draft.current,
        attachLocation,
        () => ensureDraftIdentity(null, () => randomHex(16), Date.now),
        captureForegroundMessageLocation,
      );
      draft.current = submission;
      const request: SendRequest = {
        destination,
        timestamp_ms: submission.identity.timestampMs,
        idempotency_key: submission.identity.idempotencyKey,
        title,
        content,
        location: submission.location,
      };
      const response = await api.send(request);
      if (draft.current === submission) draft.current = null;

      // The successful response is the durable SQLite acceptance boundary.
      // Reconcile the exact sequence/status in the background without keeping
      // the composer or global navigation busy.
      if (selectedRef.current === destination) {
        const timelineRequest = timelineRequests.current.begin();
        sendTimelineRefreshesInFlight.current += 1;
        void api
          .timeline(destination)
          .then((nextTimeline) => {
            if (
              timelineRequests.current.accepts(timelineRequest) &&
              selectedRef.current === destination
            ) {
              setTimeline(nextTimeline);
            }
          })
          .catch(() => {
            // The local acceptance remains visible. The settled full refresh
            // poll will retry this projection without changing send success.
          })
          .finally(() => {
            sendTimelineRefreshesInFlight.current = Math.max(
              0,
              sendTimelineRefreshesInFlight.current - 1,
            );
          });
      }

      return {
        acceptance: localMessageAcceptance(request, response),
        error: null,
        queued: true,
      };
    } catch (nextError) {
      // draft.current intentionally retains an ambiguous request's exact
      // identity and captured location for the next explicit retry.
      return { acceptance: null, error: errorText(nextError), queued: false };
    } finally {
      mutationInFlight.current = false;
    }
  };
  const retryMessage = async (entry: TimelineView): Promise<boolean> => {
    if (selected === null) return false;
    const cacheKey = retryMessageCacheKey(selected, entry);
    let request = retryMessageRequests.current.get(cacheKey);
    if (request === undefined) {
      const nextRequest = retryMessageRequest(entry, randomHex(16));
      if (nextRequest === null) return false;
      request = nextRequest;
      retryMessageRequests.current.set(cacheKey, request);
    }
    const accepted = await run(() => api.retryMessage(request));
    if (accepted) retryMessageRequests.current.delete(cacheKey);
    return accepted;
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

  const enableMessageNotifications = async (): Promise<void> => {
    setMessageNotificationError(null);
    if (
      messageNotificationPermission.state === "disabled" &&
      !messageNotificationPermission.canAskAgain
    ) {
      try {
        await Linking.openSettings();
      } catch (nextError) {
        setMessageNotificationError(
          `Could not open notification settings: ${errorText(nextError)}`,
        );
      }
      return;
    }
    setMessageNotificationPermission({ state: "checking" });
    const permission = await requestMessageNotificationPermission();
    setMessageNotificationPermission(permission);
    setMessageNotificationPermissionCheckedEpoch(messageNotificationPermissionEpoch.current);
  };

  notificationActivateProfile.current = activateApplianceProfile;
  notificationChooseContact.current = chooseContact;
  const messageNotificationTarget = messageNotificationTargets[0] ?? null;

  useEffect(() => {
    if (
      messageNotificationTarget === null ||
      !bootstrapped ||
      !ready ||
      busy ||
      notificationNavigationInFlight.current
    ) {
      return;
    }
    const target = messageNotificationTarget;
    notificationNavigationInFlight.current = true;
    void (async () => {
      try {
        const activeProfileKey = profiles?.activeProfileKey?.trim().toLowerCase() ?? "";
        if (activeProfileKey !== target.profileKey) {
          const activated = await notificationActivateProfile.current(target.profileKey);
          if (!activated) {
            throw new Error("the appliance attached to this notification could not be activated");
          }
        }
        setMobileSidebarVisible(false);
        setWorkspace("lxmf");
        notificationChooseContact.current(target.destination);
      } catch (nextError) {
        setError(`Could not open the message notification: ${errorText(nextError)}`);
      } finally {
        notificationNavigationInFlight.current = false;
        setMessageNotificationTargets((queue) => (queue[0] === target ? queue.slice(1) : queue));
      }
    })();
  }, [bootstrapped, busy, messageNotificationTarget, profiles?.activeProfileKey, ready]);

  const browseNomad = useCallback((destination: string) => {
    setMobileSidebarVisible(false);
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
        conversations={conversations}
        foreground={foreground}
        onBrowseNomad={browseNomad}
        onClose={() => setMobileSidebarVisible(false)}
        onRefreshNearby={nearbyReader}
        onSelect={chooseContact}
        onUpsert={upsertContact}
        selected={selected}
        snapshot={snapshot}
        visible={!compact || mobileSidebarVisible}
      />
      <Conversation
        busy={busy}
        canMeasurePath={snapshot?.connection.state === "ready"}
        compact={compact}
        key={selectedConversation?.destination ?? "empty"}
        messageLocationDefaultEnabled={messageLocationPreference.attachByDefault}
        messageLocationPreferenceLoaded={!messageLocationPreference.loading}
        onAbandonRetainedProbe={() => reticulumProbe.abandonRetainedProbe()}
        onDraftChanged={() => {
          draft.current = null;
        }}
        onSend={send}
        onRetryMessage={retryMessage}
        onLoadMessageActivity={loadMessageActivity}
        onLoadRadioTrace={loadMessageRadioTrace}
        onExportRadioTrace={(timelineSequence, format) =>
          exportRadioTrace(timelineSequence, format)
        }
        onMeasurePath={(destination) => reticulumProbe.measure(destination)}
        peer={selectedConversation}
        probeState={reticulumProbeState}
        timeline={timeline}
      />
    </View>
  );
  const activityShell = (
    <ScrollView
      contentContainerStyle={styles.activityContent}
      keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
      keyboardShouldPersistTaps="handled"
      style={styles.activityScroller}
    >
      <ActivityPanel
        contacts={contacts}
        conversationPeers={conversations}
        disabled={!ready}
        error={activityError}
        fieldTelemetry={fieldTelemetryState}
        loading={activityLoading}
        messageLocationPreference={messageLocationPreference}
        onLoadOlder={() => void loadActivity(true)}
        onRefresh={() => void loadActivity(false)}
        onToggleFieldTelemetry={
          fieldTelemetryController === null
            ? undefined
            : (enabled) => {
                void fieldTelemetryController.setEnabled(enabled);
              }
        }
        onToggleMessageLocationDefault={(enabled) => {
          void setMessageLocationDefault(enabled);
        }}
        page={activityPage}
      />
      {api.radioTrace === undefined ? (
        <Text style={styles.secondaryText}>
          Durable packet-correlated RF tracing is unavailable in this client build.
        </Text>
      ) : (
        <RadioTracePanel
          disabled={!ready}
          error={radioTraceError}
          exportError={radioTraceExportError}
          exporting={radioTraceExporting}
          loading={radioTraceLoading}
          onExport={(format) => void exportCompleteRadioTrace(format)}
          onLoadOlder={() => void loadRadioTrace(true)}
          onRefresh={() => void loadRadioTrace(false)}
          page={radioTracePage}
        />
      )}
    </ScrollView>
  );
  const mapDataError = [activityError, radioTraceError]
    .filter((message): message is string => message !== null)
    .join(" · ");
  const mapShell = (
    <TransmissionMapPanel
      compact={compact}
      disabled={!ready}
      evidenceError={mapFeatureEvidenceError}
      evidenceLoading={mapFeatureEvidenceLoading}
      error={mapDataError.length === 0 ? null : mapDataError}
      hasOlder={
        (activityPage?.next_before_event_id !== null &&
          activityPage?.next_before_event_id !== undefined) ||
        (radioTracePage?.next_before_event_id !== null &&
          radioTracePage?.next_before_event_id !== undefined)
      }
      loading={activityLoading || radioTraceLoading}
      onLoadOlder={() => {
        void Promise.all([loadActivity(true), loadRadioTrace(true)]);
      }}
      onRefresh={() => {
        void Promise.all([loadActivity(false), loadRadioTrace(false)]);
      }}
      onSelectFeature={selectMapFeature}
      scene={transmissionMapScene}
    />
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
  const connectivityShell =
    networkController === null ||
    networkState === null ||
    networkDeviceKey === null ? null : networkState.deviceKey === networkDeviceKey ? (
      <ConnectivityPanel
        announceNow={manualServiceAnnounce}
        controller={networkController}
        key={networkDeviceKey}
        onRefreshRadioRoutes={
          radioRoutesController === null
            ? undefined
            : () => {
                void radioRoutesController.refresh();
              }
        }
        radioRoutesState={radioRoutesState}
        state={networkState}
      />
    ) : (
      <View style={styles.connectivityLoading}>
        <ActivityIndicator color={colors.green} />
        <Text style={styles.secondaryText}>Loading this appliance&apos;s network settings…</Text>
      </View>
    );

  return (
    <SafeAreaView style={styles.safeArea}>
      <View style={[styles.topbar, compact && styles.topbarCompact]}>
        <View style={[styles.brandCluster, compact && styles.brandClusterCompact]}>
          {compact ? (
            workspace === "lxmf" ? (
              <Pressable
                accessibilityLabel="Open contacts"
                accessibilityRole="button"
                accessibilityState={{ expanded: mobileSidebarVisible }}
                onPress={() => setMobileSidebarVisible(true)}
                style={({ pressed }) => [
                  styles.mobileContactsButton,
                  pressed && styles.buttonPressed,
                ]}
              >
                <Text style={styles.mobileContactsButtonText}>Contacts</Text>
              </Pressable>
            ) : (
              <Text style={styles.mobileBrand}>Reticulum</Text>
            )
          ) : (
            <View>
              <Text style={styles.eyebrow}>RETICULUM APPLIANCE</Text>
              <Text style={styles.title}>
                {workspace === "lxmf"
                  ? "LXMF"
                  : workspace === "nomad"
                    ? "NomadNet"
                    : workspace === "activity"
                      ? "Activity"
                      : workspace === "map"
                        ? "Map"
                        : "Connectivity"}
              </Text>
            </View>
          )}
          <View
            accessibilityRole="tablist"
            style={[styles.workspaceSwitcher, compact && styles.workspaceSwitcherCompact]}
          >
            <ScrollView
              contentContainerStyle={styles.workspaceSwitcherContent}
              horizontal
              keyboardShouldPersistTaps="handled"
              showsHorizontalScrollIndicator={false}
            >
              <Pressable
                accessibilityRole="tab"
                accessibilityState={{ selected: workspace === "lxmf" }}
                onPress={() => {
                  setWorkspace("lxmf");
                }}
                style={[
                  styles.workspaceTab,
                  compact && styles.workspaceTabCompact,
                  workspace === "lxmf" && styles.workspaceTabActive,
                ]}
              >
                <Text style={[styles.workspaceTabText, compact && styles.workspaceTabTextCompact]}>
                  {compact ? "Chat" : "Messages"}
                </Text>
              </Pressable>
              <Pressable
                accessibilityRole="tab"
                accessibilityState={{ selected: workspace === "nomad" }}
                onPress={() => {
                  setMobileSidebarVisible(false);
                  setWorkspace("nomad");
                }}
                style={[
                  styles.workspaceTab,
                  compact && styles.workspaceTabCompact,
                  workspace === "nomad" && styles.workspaceTabActive,
                ]}
              >
                <Text style={[styles.workspaceTabText, compact && styles.workspaceTabTextCompact]}>
                  Browse
                </Text>
              </Pressable>
              <Pressable
                accessibilityRole="tab"
                accessibilityState={{ selected: workspace === "activity" }}
                onPress={() => {
                  setMobileSidebarVisible(false);
                  setWorkspace("activity");
                }}
                style={[
                  styles.workspaceTab,
                  compact && styles.workspaceTabCompact,
                  workspace === "activity" && styles.workspaceTabActive,
                ]}
              >
                <Text style={[styles.workspaceTabText, compact && styles.workspaceTabTextCompact]}>
                  Activity
                </Text>
              </Pressable>
              <Pressable
                accessibilityRole="tab"
                accessibilityState={{ selected: workspace === "map" }}
                onPress={() => {
                  setMobileSidebarVisible(false);
                  setWorkspace("map");
                }}
                style={[
                  styles.workspaceTab,
                  compact && styles.workspaceTabCompact,
                  workspace === "map" && styles.workspaceTabActive,
                ]}
              >
                <Text style={[styles.workspaceTabText, compact && styles.workspaceTabTextCompact]}>
                  Map
                </Text>
              </Pressable>
              {connectivityAvailable ? (
                <Pressable
                  accessibilityRole="tab"
                  accessibilityState={{ selected: workspace === "connectivity" }}
                  onPress={() => {
                    setMobileSidebarVisible(false);
                    setWorkspace("connectivity");
                  }}
                  style={[
                    styles.workspaceTab,
                    compact && styles.workspaceTabCompact,
                    workspace === "connectivity" && styles.workspaceTabActive,
                  ]}
                >
                  <Text
                    style={[styles.workspaceTabText, compact && styles.workspaceTabTextCompact]}
                  >
                    {compact ? "Net" : "Network"}
                  </Text>
                </Pressable>
              ) : null}
            </ScrollView>
          </View>
        </View>
        {compact ? null : (
          <View style={styles.statusCluster}>
            <View
              style={[
                styles.pill,
                snapshot?.connection.state === "ready" && styles.pillReady,
                snapshot?.connection.state === "faulted" && styles.pillFaulted,
              ]}
            >
              <Text style={styles.pillText}>
                {ready ? connectionStateLabel(snapshot?.connection) : "setup required"}
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
                  onPress={() => void reconnectActiveProfile()}
                  secondary
                />
              </>
            )}
          </View>
        )}
      </View>
      {(ready || (canManageProfiles && hasSavedProfiles)) &&
      !(compact && workspace === "lxmf" && keyboardVisible) ? (
        <ApplianceStatusCard
          busy={busy}
          canAddAppliance={canAddAppliance}
          compact={compact}
          exactBleTargetRequired={exactBleTargetRequired}
          nativeCore={nativeCore}
          onActivateProfile={activateApplianceProfile}
          onAddAppliance={beginAddAppliance}
          onClearProfileOperation={() => setProfileOperation({ state: "idle" })}
          onForgetProfile={api.forgetProfile === undefined ? null : forgetInactiveProfile}
          onReconnect={reconnectActiveProfile}
          onRepairBleBond={api.repairBleBond === undefined ? null : repairActiveBleBond}
          onSync={() => void run(() => api.sync())}
          profileOperation={profileOperation}
          profiles={canManageProfiles ? profiles : null}
          snapshot={snapshot}
        />
      ) : null}
      {displayedError === null || displayedError === undefined ? null : (
        <View accessibilityLiveRegion="assertive" style={styles.errorBanner}>
          <Text style={styles.errorText}>{displayedError}</Text>
        </View>
      )}
      {ready &&
      (messageNotificationPermission.state === "disabled" ||
        messageNotificationPermission.state === "error") ? (
        <View accessibilityLiveRegion="polite" style={styles.notificationPermissionBanner}>
          <Text style={styles.notificationPermissionText}>
            {messageNotificationPermission.state === "error"
              ? `Phone notification setup failed: ${messageNotificationPermission.message}`
              : messageNotificationPermission.reason === "android_channel"
                ? "The Android LXMF notification channel is disabled in system settings."
                : messageNotificationPermission.canAskAgain
                  ? "Enable phone alerts for newly collected LXMF messages."
                  : "Phone alerts are disabled in system settings."}
          </Text>
          <ActionButton
            label={
              messageNotificationPermission.state === "disabled" &&
              !messageNotificationPermission.canAskAgain
                ? "Open settings"
                : "Enable"
            }
            onPress={() => void enableMessageNotifications()}
            secondary
          />
        </View>
      ) : null}
      {messageNotificationError === null ? null : (
        <View accessibilityLiveRegion="assertive" style={styles.errorBanner}>
          <Text style={styles.errorText}>{messageNotificationError}</Text>
        </View>
      )}
      {profileOperation.state === "idle" ? null : (
        <View
          accessibilityLiveRegion={profileOperation.state === "error" ? "assertive" : "polite"}
          style={[
            styles.profileOperationBanner,
            profileOperation.state === "error" && styles.errorBanner,
            profileOperation.state === "success" && styles.reconnectBanner,
          ]}
        >
          <Text
            style={[styles.reconnectText, profileOperation.state === "error" && styles.errorText]}
          >
            {profileOperation.message}
          </Text>
        </View>
      )}
      {reconnectProgress === null ? null : (
        <View accessibilityLiveRegion="polite" style={styles.reconnectBanner}>
          <Text style={styles.reconnectText}>{foregroundReconnectMessage(reconnectProgress)}</Text>
        </View>
      )}
      {busy ? <ActivityIndicator color="#91e6a7" style={styles.activity} /> : null}
      <OnboardingPanel
        addingAppliance={addingAppliance}
        busy={busy}
        knownProfiles={profiles}
        onboarding={onboarding}
        onCancel={cancelOnboarding}
        onMutation={onboardingMutation}
        onScanBleCandidates={bleCandidateScanner}
        onSwitchKnownProfile={switchToKnownProfile}
      />
      {ready ? (
        <KeyboardAvoidingView
          behavior={KEYBOARD_LAYOUT.avoidingBehavior}
          enabled={KEYBOARD_LAYOUT.avoidingEnabled}
          style={styles.keyboardAvoiding}
        >
          {workspace === "nomad"
            ? nomadShell
            : workspace === "activity"
              ? activityShell
              : workspace === "map"
                ? mapShell
                : workspace === "connectivity"
                  ? connectivityShell
                  : applianceShell}
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
  connectivityLoading: {
    flex: 1,
    minHeight: 0,
    alignItems: "center",
    justifyContent: "center",
    gap: 10,
  },
  activityScroller: { flex: 1, minHeight: 0 },
  activityContent: {
    width: "100%",
    maxWidth: 900,
    alignSelf: "center",
    gap: 14,
    padding: 18,
    paddingBottom: 44,
  },
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
  topbarCompact: {
    minHeight: 48,
    paddingHorizontal: 12,
    paddingVertical: 7,
    flexWrap: "nowrap",
    gap: 8,
  },
  brandCluster: { flexDirection: "row", alignItems: "center", flexWrap: "wrap", gap: 18 },
  brandClusterCompact: {
    flex: 1,
    minWidth: 0,
    justifyContent: "space-between",
    flexWrap: "nowrap",
    gap: 8,
  },
  mobileBrand: { color: colors.text, fontSize: 14, fontWeight: "800" },
  mobileContactsButton: {
    minHeight: 34,
    justifyContent: "center",
    paddingHorizontal: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel,
  },
  mobileContactsButtonText: { color: colors.text, fontSize: 12, fontWeight: "700" },
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
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel,
    overflow: "hidden",
  },
  workspaceSwitcherCompact: { flexShrink: 1, minWidth: 0 },
  workspaceSwitcherContent: { flexDirection: "row", padding: 3 },
  workspaceTab: {
    minHeight: 30,
    justifyContent: "center",
    paddingHorizontal: 11,
    borderRadius: 6,
  },
  workspaceTabCompact: { paddingHorizontal: 5 },
  workspaceTabActive: { backgroundColor: colors.greenDark },
  workspaceTabText: { color: "#dfe8df", fontSize: 11, fontWeight: "700" },
  workspaceTabTextCompact: { fontSize: 9 },
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
  applianceStatusCard: {
    marginHorizontal: 28,
    marginTop: 14,
    padding: 14,
    gap: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 12,
    backgroundColor: colors.panel,
  },
  applianceStatusCardCompact: {
    marginHorizontal: 12,
    marginTop: 8,
    padding: 8,
    gap: 7,
  },
  applianceStatusHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    flexWrap: "wrap",
    gap: 12,
  },
  applianceStatusHeadingCompact: { alignItems: "center", flexWrap: "nowrap", gap: 8 },
  applianceStatusIdentity: { flex: 1, minWidth: 0 },
  applianceStatusIdentityCompact: { justifyContent: "center" },
  applianceStatusBoard: {
    color: colors.text,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 15,
    fontWeight: "700",
    letterSpacing: 0.4,
  },
  applianceStatusBoardCompact: { fontSize: 13 },
  applianceStatusConnection: { marginTop: 4, color: colors.muted, fontSize: 12 },
  applianceStatusConnectionCompact: { marginTop: 1, fontSize: 11 },
  applianceStatusConnectionReady: { color: colors.green },
  applianceStatusConnectionFaulted: { color: colors.red },
  applianceStatusActions: {
    flexDirection: "row",
    alignItems: "center",
    flexWrap: "wrap",
    flexShrink: 1,
    justifyContent: "flex-end",
    gap: 7,
  },
  applianceStatusActionsCompact: { flexWrap: "nowrap", flexShrink: 0, gap: 5 },
  statusDetailsButton: {
    minHeight: 44,
    justifyContent: "center",
    paddingHorizontal: 10,
    paddingVertical: 5,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 999,
  },
  statusDetailsButtonCompact: { minHeight: 34, paddingHorizontal: 8, paddingVertical: 3 },
  statusDetailsButtonText: { color: "#dfe8df", fontSize: 11, fontWeight: "700" },
  applianceActivity: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 7 },
  applianceActivityItem: { color: colors.text, fontSize: 12, fontWeight: "600" },
  applianceActivitySeparator: { color: colors.muted, fontSize: 12 },
  applianceDestination: {
    flexDirection: "row",
    alignItems: "baseline",
    flexWrap: "wrap",
    gap: 8,
  },
  applianceDestinationLabel: {
    color: colors.muted,
    fontSize: 10,
    fontWeight: "700",
    letterSpacing: 0.8,
  },
  applianceDestinationValue: {
    flexShrink: 1,
    color: colors.text,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 11,
    lineHeight: 16,
  },
  applianceStatusDetails: {
    paddingTop: 10,
    gap: 8,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  statusUtilityActions: { flexDirection: "row", flexWrap: "wrap", gap: 8 },
  profileManagerSafeArea: { flex: 1, backgroundColor: colors.background },
  profileManagerHeading: {
    minHeight: 76,
    paddingLeft: 22,
    paddingRight: 132,
    paddingVertical: 14,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 14,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  profileManagerHeadingCopy: { flex: 1, minWidth: 0 },
  profileManagerTitle: { color: colors.text, fontSize: 21, fontWeight: "800" },
  profileManagerScroller: { flex: 1, minHeight: 0 },
  profileManagerContent: {
    width: "100%",
    maxWidth: 720,
    alignSelf: "center",
    padding: 20,
    paddingBottom: 44,
    gap: 16,
  },
  profileOperation: {
    minHeight: 48,
    padding: 12,
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  profileOperationError: { borderColor: "#70413d", backgroundColor: "#321d1b" },
  profileOperationSuccess: { borderColor: "#356344", backgroundColor: colors.greenDark },
  profileOperationText: { flex: 1, color: colors.muted, lineHeight: 19 },
  profileOperationErrorText: { color: colors.red },
  profileList: { gap: 10 },
  profileRow: {
    minHeight: 92,
    padding: 14,
    gap: 5,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  profileRowActive: { borderColor: "#4c8d5b", backgroundColor: colors.greenDark },
  profileRowUnavailable: { opacity: 0.62 },
  profileRowHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  profileBoardLabel: {
    flexShrink: 1,
    color: colors.text,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 16,
    fontWeight: "700",
  },
  profileBadge: {
    paddingHorizontal: 8,
    paddingVertical: 4,
    overflow: "hidden",
    color: colors.muted,
    fontSize: 9,
    fontWeight: "800",
    letterSpacing: 0.8,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 999,
  },
  profileBadgeActive: { color: colors.green, borderColor: "#4c8d5b" },
  profileGeneration: { color: colors.muted, fontSize: 11 },
  profileRowActions: {
    marginTop: 7,
    flexDirection: "row",
    flexWrap: "wrap",
    alignItems: "center",
    gap: 8,
  },
  profileConfirmation: {
    padding: 16,
    gap: 10,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel2,
  },
  profileConfirmationTitle: { color: colors.text, fontSize: 15, fontWeight: "700" },
  profileAddSection: {
    paddingTop: 16,
    gap: 9,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
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
  notificationPermissionBanner: {
    marginHorizontal: 28,
    marginTop: 10,
    paddingHorizontal: 12,
    paddingVertical: 8,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.greenDark,
  },
  notificationPermissionText: { flex: 1, color: colors.text, fontSize: 12 },
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
  profileOperationBanner: {
    marginHorizontal: 28,
    marginTop: 14,
    padding: 12,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel2,
  },
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
  sidebarDrawerBackdrop: {
    flex: 1,
    flexDirection: "row",
    backgroundColor: "#00000099",
  },
  sidebarDrawerDismiss: {
    position: "absolute",
    top: 0,
    right: 0,
    bottom: 0,
    left: 0,
  },
  sidebarDrawer: {
    width: "88%",
    maxWidth: 420,
    height: "100%",
    borderRightColor: colors.line,
    borderRightWidth: 1,
    backgroundColor: colors.panel,
  },
  sidebarDrawerHeading: {
    minHeight: 64,
    paddingHorizontal: 18,
    paddingVertical: 10,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  sidebarCompactScroller: { flex: 1, minHeight: 0 },
  sidebarCompactContent: { padding: 18, paddingBottom: 36 },
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
  nearbyInterfaces: {
    gap: 6,
    padding: 8,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel,
  },
  nearbyInterfaceRow: { gap: 1 },
  nearbyInterfaceName: { color: "#dfe8df", fontSize: 11, fontWeight: "700" },
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
  inputReadOnly: { color: colors.muted },
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
  contactActions: {
    flexDirection: "row",
    alignItems: "center",
    gap: 5,
    marginRight: 8,
  },
  contactActive: { borderColor: colors.line, backgroundColor: colors.panel2 },
  contactPressed: { opacity: 0.8 },
  contactName: { color: "#dfe8df", fontWeight: "700" },
  messageRequestsSection: {
    gap: 7,
    marginBottom: 12,
    paddingBottom: 12,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  messageRequestPreview: { color: colors.muted, fontSize: 11 },
  monospace: {
    color: colors.muted,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 11,
    lineHeight: 16,
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
  conversationCompact: { minHeight: 0 },
  conversationHeading: {
    minHeight: 72,
    justifyContent: "center",
    gap: 4,
    paddingHorizontal: 22,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  conversationHeadingCompact: { minHeight: 48, paddingLeft: 14, paddingRight: 124 },
  conversationIdentity: { gap: 3 },
  messageRequestBadge: { color: colors.green, fontSize: 9, fontWeight: "800", letterSpacing: 0.7 },
  measurePathButton: {
    position: "absolute",
    top: 12,
    right: 14,
    minHeight: 30,
    justifyContent: "center",
    paddingHorizontal: 9,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 8,
  },
  measurePathButtonText: { color: colors.green, fontSize: 10, fontWeight: "800" },
  probeResult: {
    gap: 3,
    paddingHorizontal: 14,
    paddingVertical: 9,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
    backgroundColor: colors.greenDark,
  },
  probeResultTitle: { color: colors.text, fontSize: 11, fontWeight: "700" },
  probeResultValue: { color: colors.green, fontSize: 11, fontWeight: "700" },
  probeResultHelp: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  probeResultAction: {
    alignSelf: "flex-start",
    marginTop: 3,
    paddingHorizontal: 7,
    paddingVertical: 4,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 6,
  },
  probeResultActionText: { color: colors.green, fontSize: 9, fontWeight: "800" },
  timelineScroller: { flex: 1 },
  timeline: { flexGrow: 1, gap: 12, padding: 22, justifyContent: "flex-end" },
  timelineCompact: { gap: 9, padding: 12 },
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
  messageHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 8,
  },
  messageTitle: { flex: 1, marginBottom: 5, color: colors.text, fontWeight: "700" },
  localAcceptanceBadge: {
    color: colors.green,
    fontSize: 8,
    fontWeight: "800",
    letterSpacing: 0.8,
  },
  messageActionsButton: {
    minWidth: 28,
    minHeight: 24,
    alignItems: "center",
    justifyContent: "center",
    borderRadius: 999,
  },
  messageActionsButtonText: {
    color: colors.muted,
    fontSize: 12,
    fontWeight: "800",
    letterSpacing: 1,
  },
  messageContent: { color: colors.text, lineHeight: 21 },
  messageLocationChip: {
    marginTop: 9,
    paddingHorizontal: 9,
    paddingVertical: 7,
    gap: 2,
    borderColor: "#506b88",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#15212b",
  },
  messageLocationChipLabel: {
    color: "#9ac9f4",
    fontSize: 8,
    fontWeight: "800",
    letterSpacing: 0.9,
  },
  messageLocationChipText: { color: "#c9def0", fontSize: 9, lineHeight: 13 },
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
  composeScroller: {
    flexGrow: 0,
    flexShrink: 1,
    minHeight: 0,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    backgroundColor: colors.panel,
  },
  composeScrollerCompact: { maxHeight: "60%" },
  compose: { gap: 10, padding: 16 },
  composeCompact: { gap: 6, padding: 9 },
  messageInput: { minHeight: 78, textAlignVertical: "top" },
  messageInputCompact: { minHeight: 48, maxHeight: 96 },
  composerLocationToggle: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 10,
    padding: 9,
    borderColor: "#506b88",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: "#151e27",
  },
  composerLocationToggleCompact: { alignItems: "center", padding: 7 },
  composerLocationCopy: { flex: 1, minWidth: 0, gap: 2 },
  composerLocationCopyCompact: { flexDirection: "row", alignItems: "center", gap: 7 },
  composerLocationTitle: { color: "#b5d9fa", fontSize: 10, fontWeight: "800" },
  composerLocationHelp: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  composerLocationCaveat: { color: "#829bb1", fontSize: 8, lineHeight: 12 },
  composeFooter: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  composerBudget: { flex: 1, minWidth: 0, gap: 2 },
  counter: { color: colors.muted, fontSize: 10 },
  composerBudgetHelp: { color: "#748078", fontSize: 8, lineHeight: 12 },
  counterOverLimit: { color: colors.red },
  messageActionsBackdrop: {
    flex: 1,
    justifyContent: "flex-end",
    alignItems: "center",
    backgroundColor: "#00000099",
  },
  messageActionsDismiss: {
    position: "absolute",
    top: 0,
    right: 0,
    bottom: 0,
    left: 0,
  },
  messageActionsSheet: {
    width: "100%",
    maxWidth: 680,
    maxHeight: "88%",
    borderTopColor: colors.line,
    borderTopWidth: 1,
    borderTopLeftRadius: 18,
    borderTopRightRadius: 18,
    backgroundColor: colors.panel,
  },
  messageActionsHeading: {
    paddingHorizontal: 18,
    paddingVertical: 12,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  messageActionsHeadingCopy: { flex: 1, minWidth: 0 },
  messageActionsContent: { padding: 18, paddingBottom: 30, gap: 12 },
  messageRadioDetails: {
    gap: 5,
    paddingTop: 12,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  messageAttachedLocationDetails: {
    gap: 7,
    padding: 11,
    borderColor: "#506b88",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: "#151e27",
  },
  messageActivityDetails: {
    gap: 8,
    paddingTop: 12,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  messageTraceActions: { flexDirection: "row", flexWrap: "wrap", gap: 8 },
  messageActionUtilities: {
    gap: 10,
    paddingTop: 12,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  messageSendAgainNotice: { gap: 10 },
});
