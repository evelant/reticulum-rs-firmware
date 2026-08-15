import type { NativeProfileStoreSnapshot } from "@reticulum/appliance-native";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  AppState,
  Keyboard,
  KeyboardAvoidingView,
  Linking,
  Text,
  useWindowDimensions,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { ActivityWorkspace } from "../components/ActivityWorkspace.tsx";
import { ActionButton } from "../components/AppliancePrimitives.tsx";
import { ApplianceSidebar } from "../components/ApplianceSidebar.tsx";
import { ApplianceStatusCard, type ProfileOperation } from "../components/ApplianceStatusCard.tsx";
import { ApplianceTopBar, type ApplianceWorkspace } from "../components/ApplianceTopBar.tsx";
import { APPLIANCE_KEYBOARD_LAYOUT } from "../components/appliance-screen-layout.ts";
import { styles } from "../components/appliance-screen-styles.ts";
import { applianceScreenColors as colors } from "../components/appliance-screen-theme.ts";
import { ConnectivityPanel } from "../components/ConnectivityPanel.tsx";
import { ConversationPanel, type QueueMessageResult } from "../components/ConversationPanel.tsx";
import { NomadPanel } from "../components/NomadPanel.tsx";
import { OnboardingPanel } from "../components/OnboardingPanel.tsx";
import type { RadioTraceExportFormat } from "../components/RadioTracePanel.tsx";
import { TransmissionMapPanel } from "../components/TransmissionMapPanel.tsx";
import type {
  ApplianceSnapshot,
  ContactView,
  ConversationPeerView,
  MessageActivityPageView,
  OnboardingView,
  RadioTraceEventView,
  RadioTracePageView,
  RecoveryRequest,
  RetrySendRequest,
  SendRequest,
  TimelineView,
} from "../generated/api.ts";
import { ApplianceApi } from "../lib/api";
import { errorText } from "../lib/app-error.ts";
import { applianceProfilesPresentation } from "../lib/appliance-profiles.ts";
import { applianceStatusPresentation } from "../lib/appliance-status.ts";
import { bleBondRepairProgressMessage } from "../lib/ble-bond-repair.ts";
import type { BleCandidate, BleScanOptions } from "../lib/ble-central-types.ts";
import { ensureDraftIdentity } from "../lib/draft.ts";
import { deliverExportArtifact } from "../lib/export-artifact";
import {
  type FieldTelemetryClient,
  FieldTelemetryController,
  type FieldTelemetryControllerState,
} from "../lib/field-telemetry.ts";
import { createFieldTelemetryPreferenceStore } from "../lib/field-telemetry-preference";
import {
  ensureForegroundConnection,
  ForegroundReconnect,
  type ForegroundReconnectProgress,
  foregroundReconnectMessage,
} from "../lib/foreground-reconnect.ts";
import { LatestRequest } from "../lib/latest-request.ts";
import { retryMessageCacheKey, retryMessageRequest } from "../lib/message-actions.ts";
import { buildMessageActivityAliases, messageActivityPeerLabel } from "../lib/message-activity.ts";
import { captureForegroundMessageLocation } from "../lib/message-location.ts";
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
import { localMessageAcceptance } from "../lib/message-submit-ui.ts";
import { readNativeCoreStatus } from "../lib/native-core";
import type { NativeCoreStatus } from "../lib/native-core-types.ts";
import {
  NetworkConfigController,
  type NetworkConfigControllerState,
  type NetworkConfigurationClient,
} from "../lib/network-config.ts";
import { NomadBrowserController, type NomadBrowserState } from "../lib/nomad-browser.ts";
import { onboardingPresentation } from "../lib/onboarding.ts";
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
import { ReticulumProbeController, type ReticulumProbeState } from "../lib/reticulum-probe.ts";
import { SettledPoll } from "../lib/settled-poll.ts";
import {
  buildTransmissionMapScene,
  type LocatedTimeline,
  type TransmissionMapFeatureDetails,
} from "../lib/transmission-map.ts";

const EMPTY_ONBOARDING: OnboardingView = { available: false, method: null, snapshot: null };
const FOREGROUND_RECONNECT_DELAY_MS = 2_000;
const MESSAGE_ACTIVITY_PAGE_SIZE = 50;
const RADIO_TRACE_PAGE_SIZE = 50;
const KEYBOARD_LAYOUT = APPLIANCE_KEYBOARD_LAYOUT;
interface MapFeatureEvidence {
  readonly events: readonly RadioTraceEventView[];
  readonly historyIncomplete: boolean;
  readonly profileKey: string;
  readonly timelineSequence: number;
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
  const [workspace, setWorkspace] = useState<ApplianceWorkspace>("lxmf");
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
      <ApplianceSidebar
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
      <ConversationPanel
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
    <ActivityWorkspace
      contacts={contacts}
      conversationPeers={conversations}
      disabled={!ready}
      activityError={activityError}
      activityLoading={activityLoading}
      activityPage={activityPage}
      fieldTelemetry={fieldTelemetryState}
      messageLocationPreference={messageLocationPreference}
      onLoadOlderActivity={() => void loadActivity(true)}
      onRefreshActivity={() => void loadActivity(false)}
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
      radioTraceAvailable={api.radioTrace !== undefined}
      radioTraceError={radioTraceError}
      radioTraceExportError={radioTraceExportError}
      radioTraceExporting={radioTraceExporting}
      radioTraceLoading={radioTraceLoading}
      radioTracePage={radioTracePage}
      onExportRadioTrace={(format) => void exportCompleteRadioTrace(format)}
      onLoadOlderRadioTrace={() => void loadRadioTrace(true)}
      onRefreshRadioTrace={() => void loadRadioTrace(false)}
    />
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
      <ApplianceTopBar
        busy={busy}
        compact={compact}
        connectivityAvailable={connectivityAvailable}
        mobileSidebarVisible={mobileSidebarVisible}
        onOpenContacts={() => setMobileSidebarVisible(true)}
        onReconnect={() => void reconnectActiveProfile()}
        onSelectWorkspace={(nextWorkspace) => {
          if (nextWorkspace !== "lxmf") setMobileSidebarVisible(false);
          setWorkspace(nextWorkspace);
        }}
        onSync={() => void run(() => api.sync())}
        ready={ready}
        snapshot={snapshot}
        workspace={workspace}
      />
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
