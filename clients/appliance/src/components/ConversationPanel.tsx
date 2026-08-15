import { useEffect, useRef, useState } from "react";
import {
  ActivityIndicator,
  Keyboard,
  Linking,
  Modal,
  Pressable,
  ScrollView,
  Switch,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import type {
  ConversationPeerView,
  MessageActivityPageView,
  MessageLocationView,
  RadioTracePageView,
  TimelineView,
} from "../generated/api.ts";
import { MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES } from "../generated/api.ts";
import { errorText } from "../lib/app-error.ts";
import { bytesText } from "../lib/bytes-text.ts";
import { conversationPeerLabel } from "../lib/conversation-peers.ts";
import { byteLimitError, utf8ByteLength } from "../lib/limits.ts";
import { directLxmfPayloadBudget, directLxmfPayloadError } from "../lib/lxmf-message-size.ts";
import {
  timelineActivityRevision,
  timelineEntryKey,
  timelineMessageCapabilities,
  timelineStatusLabel,
} from "../lib/message-actions.ts";
import { messageLocationPresentation } from "../lib/message-location.ts";
import {
  type LocalMessageAcceptance,
  recordLocalMessageAcceptance,
  unreconciledLocalMessageAcceptances,
} from "../lib/message-submit-ui.ts";
import {
  RETICULUM_PROBE_PRESENTATION_TIMEOUT_MS,
  type ReticulumProbeState,
} from "../lib/reticulum-probe.ts";
import { ActivityEventList } from "./ActivityPanel.tsx";
import { ActionButton, MetaRow } from "./AppliancePrimitives.tsx";
import { APPLIANCE_KEYBOARD_LAYOUT } from "./appliance-screen-layout.ts";
import { styles } from "./appliance-screen-styles.ts";
import { applianceScreenColors as colors } from "./appliance-screen-theme.ts";
import { KeyboardDoneAccessory } from "./KeyboardDoneAccessory";
import { RadioTraceEventList, type RadioTraceExportFormat } from "./RadioTracePanel.tsx";

const KEYBOARD_LAYOUT = APPLIANCE_KEYBOARD_LAYOUT;
const MESSAGE_COMPOSER_INPUT_ACCESSORY_ID = "lxmf-message-composer-keyboard";

export interface QueueMessageResult {
  readonly acceptance: LocalMessageAcceptance | null;
  readonly error: string | null;
  readonly queued: boolean;
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

interface ConversationPanelProps {
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

export function ConversationPanel({
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
}: ConversationPanelProps) {
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
                        board without the app. This action is for a retryable terminal row: it keeps
                        the outbox row and signed LXMF identity, but creates a replacement durable
                        device submission with a fresh request key.
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
