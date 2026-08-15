import { useMemo, useState } from "react";
import type { ViewStyle } from "react-native";
import {
  ActivityIndicator,
  Pressable,
  StyleSheet,
  Switch,
  Text,
  TextInput,
  View,
} from "react-native";

import type {
  ContactView,
  ConversationPeerView,
  MessageActivityEventView,
  MessageActivityPageView,
} from "../generated/api.ts";
import type { FieldTelemetryControllerState } from "../lib/field-telemetry.ts";
import {
  buildMessageActivityAliases,
  filterMessageActivity,
  MESSAGE_ACTIVITY_FILTERS,
  type MessageActivityFilter,
  type MessageActivityPresentation,
  messageActivityPresentation,
  sortMessageActivityNewestFirst,
} from "../lib/message-activity.ts";
import type { MessageLocationPreferenceState } from "../lib/message-location-preference.ts";

export interface ActivityEventRowProps {
  readonly aliases: ReadonlyMap<string, string>;
  readonly event: MessageActivityEventView;
}

export function ActivityEventRow({ aliases, event }: ActivityEventRowProps) {
  const presentation = messageActivityPresentation(event, aliases);
  return (
    <View
      accessibilityLabel={`${presentation.peerLabel}. ${presentation.title}. ${presentation.observedAt}`}
      style={[styles.eventRow, toneStyles[presentation.tone]]}
    >
      <View style={styles.eventHeading}>
        <View style={styles.eventHeadingCopy}>
          <Text style={styles.peerName}>{presentation.peerLabel}</Text>
          <Text
            style={[
              styles.eventTitle,
              presentation.tone === "danger" && styles.dangerText,
              presentation.tone === "success" && styles.successText,
              presentation.tone === "warning" && styles.warningText,
            ]}
          >
            {presentation.title}
          </Text>
        </View>
        <Text style={styles.attempt}>
          {event.attempt_number === null ? "" : `Attempt ${event.attempt_number}`}
        </Text>
      </View>

      <Text selectable style={styles.destination}>
        {event.peer}
      </Text>
      <Text style={styles.observedAt}>{presentation.observedAt}</Text>
      <View style={styles.metadata}>
        {presentation.metadata.map((line) => (
          <Text
            key={line}
            selectable
            style={[
              styles.metadataLine,
              (line.startsWith("Message ID ") || line.startsWith("Packet SHA-256 ")) &&
                styles.identifier,
            ]}
          >
            {line}
          </Text>
        ))}
      </View>
    </View>
  );
}

export interface ActivityEventListProps {
  readonly contacts?: readonly ContactView[];
  readonly conversationPeers?: readonly ConversationPeerView[];
  readonly emptyMessage?: string;
  readonly events: readonly MessageActivityEventView[];
}

/**
 * Reusable presentation for global activity and a message-scoped details
 * sheet. Loading, pagination, and query ownership stay with the caller.
 */
export function ActivityEventList({
  contacts = [],
  conversationPeers = [],
  emptyMessage = "No message activity has been recorded.",
  events,
}: ActivityEventListProps) {
  const aliases = useMemo(
    () => buildMessageActivityAliases(contacts, conversationPeers),
    [contacts, conversationPeers],
  );
  const ordered = useMemo(() => sortMessageActivityNewestFirst(events), [events]);

  if (ordered.length === 0) return <Text style={styles.empty}>{emptyMessage}</Text>;
  return (
    <View style={styles.eventList}>
      {ordered.map((event) => (
        <ActivityEventRow aliases={aliases} event={event} key={event.event_id} />
      ))}
    </View>
  );
}

export interface ActivityPanelProps {
  readonly contacts: readonly ContactView[];
  readonly conversationPeers: readonly ConversationPeerView[];
  readonly disabled?: boolean;
  readonly error: string | null;
  readonly fieldTelemetry?: FieldTelemetryControllerState | null;
  readonly loading: boolean;
  readonly messageLocationPreference?: MessageLocationPreferenceState | null;
  readonly onLoadOlder: () => void;
  readonly onRefresh: () => void;
  readonly onToggleFieldTelemetry?: (enabled: boolean) => void;
  readonly onToggleMessageLocationDefault?: (enabled: boolean) => void;
  /**
   * The caller may merge older pages into `events`; the page cursor must always
   * describe the next page after those currently supplied events.
   */
  readonly page: MessageActivityPageView | null;
}

export function ActivityPanel({
  contacts,
  conversationPeers,
  disabled = false,
  error,
  fieldTelemetry = null,
  loading,
  messageLocationPreference = null,
  onLoadOlder,
  onRefresh,
  onToggleFieldTelemetry,
  onToggleMessageLocationDefault,
  page,
}: ActivityPanelProps) {
  const [filter, setFilter] = useState<MessageActivityFilter>("all");
  const [query, setQuery] = useState("");
  const aliases = useMemo(
    () => buildMessageActivityAliases(contacts, conversationPeers),
    [contacts, conversationPeers],
  );
  const events = page?.events ?? [];
  const visibleEvents = useMemo(
    () => filterMessageActivity(events, filter, query, aliases),
    [aliases, events, filter, query],
  );
  const hasOlder = page?.next_before_event_id !== null && page?.next_before_event_id !== undefined;
  const controlsDisabled = disabled || loading;

  return (
    <View style={styles.panel}>
      <View style={styles.panelHeading}>
        <View style={styles.headingCopy}>
          <Text style={styles.eyebrow}>MESSAGE ACTIVITY</Text>
          <Text style={styles.panelTitle}>Delivery and retry history</Text>
          <Text style={styles.help}>
            Times show when this app&apos;s durable store observed a change. They are not RF
            transmission times or timestamps reported by the remote peer. Inbound RSSI and SNR are
            measured by this appliance on the final hop; a relay may be the transmitter.
          </Text>
        </View>
        <Pressable
          accessibilityLabel="Refresh message activity"
          accessibilityRole="button"
          disabled={controlsDisabled}
          onPress={onRefresh}
          style={({ pressed }) => [
            styles.refreshButton,
            controlsDisabled && styles.disabled,
            pressed && !controlsDisabled && styles.pressed,
          ]}
        >
          {loading && page === null ? (
            <ActivityIndicator color={colors.green} size="small" />
          ) : (
            <Text style={styles.refreshText}>Refresh</Text>
          )}
        </Pressable>
      </View>

      {messageLocationPreference === null || onToggleMessageLocationDefault === undefined ? null : (
        <View style={styles.messageLocationCard}>
          <View style={styles.telemetryCopy}>
            <Text style={styles.messageLocationTitle}>Attach location to new messages</Text>
            <Text style={styles.help}>
              Set the initial state of each new composer&apos;s location toggle. When enabled for a
              message, the app requests a fresh high-accuracy foreground phone fix while queueing
              and includes it in the LXMF message for its recipient. Each draft can override this
              default without changing the saved setting.
            </Text>
            <Text style={styles.messageLocationState}>
              {messageLocationPreferenceLabel(messageLocationPreference)}
            </Text>
            <Text style={styles.telemetryCaveat}>
              This is sender-attached phone location, not board GNSS, route position, or the exact
              location of an RF emission. It is separate from private field telemetry below.
            </Text>
          </View>
          <Switch
            accessibilityLabel="Attach location to new messages by default"
            disabled={
              disabled || messageLocationPreference.loading || messageLocationPreference.saving
            }
            onValueChange={onToggleMessageLocationDefault}
            trackColor={{ false: colors.line, true: "#496d8f" }}
            value={messageLocationPreference.attachByDefault}
          />
        </View>
      )}

      {fieldTelemetry === null || onToggleFieldTelemetry === undefined ? null : (
        <View style={styles.telemetryCard}>
          <View style={styles.telemetryCopy}>
            <Text style={styles.telemetryTitle}>Field location telemetry</Text>
            <Text style={styles.help}>
              Record the phone&apos;s high-accuracy foreground position with every new send and
              retry. Coordinates stay in this profile&apos;s local activity database and are not
              added to the message or RMAP. This phone remembers the setting across app restarts and
              appliance switches until you turn it off.
            </Text>
            <Text style={styles.telemetryState}>{fieldTelemetryLabel(fieldTelemetry)}</Text>
            <Text style={styles.telemetryCaveat}>
              A stamp is the phone position when the attempt was queued, not exact RF emission or
              board GNSS.
            </Text>
          </View>
          <Switch
            accessibilityLabel="Record private field telemetry"
            disabled={disabled || fieldTelemetry.runState === "starting"}
            onValueChange={onToggleFieldTelemetry}
            trackColor={{ false: colors.line, true: "#39764a" }}
            value={fieldTelemetry.enabled}
          />
        </View>
      )}

      {page?.history_incomplete ? (
        <View accessibilityLiveRegion="polite" style={styles.incompleteNotice}>
          <Text style={styles.incompleteTitle}>Earlier history is incomplete</Text>
          <Text style={styles.incompleteText}>
            Some activity predates this journal or was removed by bounded retention. Current message
            state remains available.
          </Text>
        </View>
      ) : null}

      {error === null ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.error}>
          Activity update failed: {error}
          {page === null ? "" : " · showing retained results"}
        </Text>
      )}

      <TextInput
        accessibilityLabel="Search message activity"
        autoCapitalize="none"
        autoCorrect={false}
        editable={!disabled}
        onChangeText={setQuery}
        placeholder="Filter by contact, status, destination, or packet hash"
        placeholderTextColor={colors.muted}
        style={styles.search}
        value={query}
      />

      <View accessibilityRole="tablist" style={styles.filters}>
        {MESSAGE_ACTIVITY_FILTERS.map((option) => {
          const selected = filter === option.value;
          return (
            <Pressable
              accessibilityRole="tab"
              accessibilityState={{ selected }}
              key={option.value}
              onPress={() => setFilter(option.value)}
              style={({ pressed }) => [
                styles.filter,
                selected && styles.filterSelected,
                pressed && styles.pressed,
              ]}
            >
              <Text style={[styles.filterText, selected && styles.filterTextSelected]}>
                {option.label}
              </Text>
            </Pressable>
          );
        })}
      </View>

      <Text style={styles.resultCount}>
        {visibleEvents.length} of {events.length} retained event{events.length === 1 ? "" : "s"} ·
        newest first
      </Text>

      <ActivityEventList
        contacts={contacts}
        conversationPeers={conversationPeers}
        emptyMessage={
          events.length === 0
            ? loading
              ? "Reading durable activity…"
              : "No message activity has been recorded."
            : "No activity matches these filters."
        }
        events={visibleEvents}
      />

      {loading && page !== null ? (
        <View accessibilityLiveRegion="polite" style={styles.loading}>
          <ActivityIndicator color={colors.green} size="small" />
          <Text style={styles.help}>Updating activity…</Text>
        </View>
      ) : null}

      {hasOlder ? (
        <Pressable
          accessibilityRole="button"
          disabled={controlsDisabled}
          onPress={onLoadOlder}
          style={({ pressed }) => [
            styles.loadOlder,
            controlsDisabled && styles.disabled,
            pressed && !controlsDisabled && styles.pressed,
          ]}
        >
          <Text style={styles.loadOlderText}>Load older activity</Text>
        </Pressable>
      ) : events.length === 0 ? null : (
        <Text style={styles.endOfHistory}>End of retained activity</Text>
      )}
    </View>
  );
}

const colors = {
  background: "#101411",
  green: "#91e6a7",
  line: "#303b33",
  muted: "#93a096",
  panel: "#171d19",
  panel2: "#1d2520",
  red: "#ff9b91",
  text: "#ecf2ea",
  warning: "#e5cf8f",
};

function fieldTelemetryLabel(state: FieldTelemetryControllerState): string {
  if (state.error !== null) return `Location error: ${state.error}`;
  if (!state.enabled && state.runState === "starting") {
    return "Loading saved location preference…";
  }
  if (!state.enabled) return "Off · future attempts record that telemetry was disabled";
  if (state.runState === "starting") return "Starting foreground location…";
  if (state.runState === "inactive") return "Paused while the app is not in the foreground";
  const observation = state.observation;
  if (observation === null) return "Waiting for local runtime state…";
  if (observation.state === "unavailable") {
    return `Location unavailable · ${observation.reason.replaceAll("_", " ")}`;
  }
  const accuracy =
    observation.horizontal_accuracy_mm === null
      ? "accuracy unknown"
      : `±${(observation.horizontal_accuracy_mm / 1_000).toFixed(1)} m`;
  return `${(observation.latitude_e6 / 1_000_000).toFixed(6)}, ${(observation.longitude_e6 / 1_000_000).toFixed(6)} · ${accuracy}`;
}

function messageLocationPreferenceLabel(state: MessageLocationPreferenceState): string {
  if (state.error !== null) return `Preference error: ${state.error}`;
  if (state.loading) return "Loading saved message-location preference…";
  if (state.saving) return "Saving default…";
  return state.attachByDefault
    ? "On · new composers start with location enabled"
    : "Off · new composers start without location";
}

const styles = StyleSheet.create({
  panel: {
    padding: 12,
    gap: 10,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  panelHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 9,
  },
  telemetryCard: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 10,
    padding: 10,
    borderColor: "#436c4c",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: "#142018",
  },
  messageLocationCard: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 10,
    padding: 10,
    borderColor: "#506b88",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: "#151e27",
  },
  telemetryCopy: { flex: 1, minWidth: 0, gap: 3 },
  telemetryTitle: { color: colors.text, fontSize: 11, fontWeight: "800" },
  messageLocationTitle: { color: "#b5d9fa", fontSize: 11, fontWeight: "800" },
  messageLocationState: { color: "#9ac9f4", fontSize: 9, fontWeight: "700", lineHeight: 14 },
  telemetryState: { color: colors.green, fontSize: 9, fontWeight: "700", lineHeight: 14 },
  telemetryCaveat: { color: colors.muted, fontSize: 8, lineHeight: 12 },
  headingCopy: { flex: 1, minWidth: 0, gap: 2 },
  eyebrow: { color: colors.green, fontSize: 9, fontWeight: "800", letterSpacing: 1.2 },
  panelTitle: { color: colors.text, fontSize: 16, fontWeight: "800" },
  help: { color: colors.muted, fontSize: 10, lineHeight: 15 },
  refreshButton: {
    minHeight: 32,
    minWidth: 62,
    alignItems: "center",
    justifyContent: "center",
    paddingHorizontal: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  refreshText: { color: colors.text, fontSize: 10, fontWeight: "700" },
  incompleteNotice: {
    padding: 9,
    gap: 2,
    borderColor: "#7f7348",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#2a271b",
  },
  incompleteTitle: { color: colors.warning, fontSize: 11, fontWeight: "800" },
  incompleteText: { color: "#cfc6a8", fontSize: 9, lineHeight: 14 },
  error: { color: colors.red, fontSize: 10, lineHeight: 15 },
  search: {
    minHeight: 38,
    paddingHorizontal: 10,
    paddingVertical: 7,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.background,
    color: colors.text,
    fontSize: 11,
  },
  filters: { flexDirection: "row", flexWrap: "wrap", gap: 6 },
  filter: {
    minHeight: 30,
    justifyContent: "center",
    paddingHorizontal: 9,
    paddingVertical: 5,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  filterSelected: { borderColor: "#5b9c69", backgroundColor: "#173f24" },
  filterText: { color: colors.muted, fontSize: 9, fontWeight: "700" },
  filterTextSelected: { color: colors.green },
  resultCount: { color: colors.muted, fontSize: 9, fontWeight: "700" },
  eventList: { gap: 7 },
  eventRow: {
    padding: 10,
    gap: 4,
    borderColor: colors.line,
    borderLeftWidth: 3,
    borderRightWidth: 1,
    borderTopWidth: 1,
    borderBottomWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  eventToneNormal: { borderLeftColor: "#5b9c69" },
  eventToneMuted: { borderLeftColor: "#58635b" },
  eventToneSuccess: { borderLeftColor: colors.green },
  eventToneWarning: { borderLeftColor: colors.warning },
  eventToneDanger: { borderLeftColor: colors.red },
  eventHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 8,
  },
  eventHeadingCopy: { flex: 1, minWidth: 0, gap: 1 },
  peerName: { color: colors.text, fontSize: 12, fontWeight: "800" },
  eventTitle: { color: colors.text, fontSize: 10, fontWeight: "700" },
  dangerText: { color: colors.red },
  successText: { color: colors.green },
  warningText: { color: colors.warning },
  attempt: { color: colors.muted, fontSize: 9, fontWeight: "800" },
  destination: { color: colors.muted, fontFamily: "monospace", fontSize: 8 },
  observedAt: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  metadata: {
    marginTop: 2,
    paddingTop: 5,
    gap: 2,
    borderTopColor: colors.line,
    borderTopWidth: StyleSheet.hairlineWidth,
  },
  metadataLine: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  identifier: { fontFamily: "monospace", fontSize: 8 },
  empty: {
    paddingVertical: 18,
    color: colors.muted,
    fontSize: 10,
    lineHeight: 15,
    textAlign: "center",
  },
  loading: { flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 7 },
  loadOlder: {
    minHeight: 36,
    alignItems: "center",
    justifyContent: "center",
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  loadOlderText: { color: colors.green, fontSize: 10, fontWeight: "800" },
  endOfHistory: { color: colors.muted, fontSize: 9, textAlign: "center" },
  disabled: { opacity: 0.4 },
  pressed: { opacity: 0.76 },
});

const toneStyles: Record<MessageActivityPresentation["tone"], ViewStyle> = {
  danger: styles.eventToneDanger,
  muted: styles.eventToneMuted,
  normal: styles.eventToneNormal,
  success: styles.eventToneSuccess,
  warning: styles.eventToneWarning,
};
