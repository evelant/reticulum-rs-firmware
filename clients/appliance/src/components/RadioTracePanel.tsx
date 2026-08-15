import { useMemo, useState } from "react";
import type { ViewStyle } from "react-native";
import { ActivityIndicator, Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import type { RadioTraceEventView, RadioTracePageView } from "../generated/api.ts";
import {
  filterRadioTrace,
  RADIO_TRACE_FILTERS,
  type RadioTraceFilter,
  type RadioTracePresentation,
  radioTracePresentation,
} from "../lib/radio-trace.ts";

export type RadioTraceExportFormat = "csv" | "json";

export interface RadioTraceEventListProps {
  readonly emptyMessage?: string;
  readonly events: readonly RadioTraceEventView[];
}

function RadioTraceEventRow({ event }: { readonly event: RadioTraceEventView }) {
  const presentation = radioTracePresentation(event);
  return (
    <View
      accessibilityLabel={`${presentation.title}. ${presentation.observedAt}`}
      style={[styles.eventRow, toneStyles[presentation.tone]]}
    >
      <View style={styles.eventHeading}>
        <View style={styles.headingCopy}>
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
          <Text style={styles.observedAt}>{presentation.observedAt}</Text>
        </View>
        <Text style={styles.eventId}>#{event.event_id}</Text>
      </View>
      <View style={styles.metadata}>
        {presentation.metadata.map((line) => (
          <Text
            key={line}
            selectable
            style={[
              styles.metadataLine,
              (line.startsWith("Packet SHA-256 ") ||
                line.startsWith("RNS attempt token ") ||
                line.startsWith("RNS packet hash ") ||
                line.startsWith("Profile fingerprint ")) &&
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

export function RadioTraceEventList({
  emptyMessage = "No RF trace events have been retained.",
  events,
}: RadioTraceEventListProps) {
  if (events.length === 0) return <Text style={styles.empty}>{emptyMessage}</Text>;
  return (
    <View style={styles.eventList}>
      {events.map((event) => (
        <RadioTraceEventRow event={event} key={event.event_id} />
      ))}
    </View>
  );
}

export interface RadioTracePanelProps {
  readonly disabled?: boolean;
  readonly error: string | null;
  readonly exportError: string | null;
  readonly exporting: RadioTraceExportFormat | null;
  readonly loading: boolean;
  readonly onExport: (format: RadioTraceExportFormat) => void;
  readonly onLoadOlder: () => void;
  readonly onRefresh: () => void;
  readonly page: RadioTracePageView | null;
}

export function RadioTracePanel({
  disabled = false,
  error,
  exportError,
  exporting,
  loading,
  onExport,
  onLoadOlder,
  onRefresh,
  page,
}: RadioTracePanelProps) {
  const [filter, setFilter] = useState<RadioTraceFilter>("all");
  const [query, setQuery] = useState("");
  const events = page?.events ?? [];
  const visible = useMemo(() => filterRadioTrace(events, filter, query), [events, filter, query]);
  const hasOlder = page?.next_before_event_id !== null && page?.next_before_event_id !== undefined;
  const controlsDisabled = disabled || loading || exporting !== null;

  return (
    <View style={styles.panel}>
      <View style={styles.panelHeading}>
        <View style={styles.headingCopy}>
          <Text style={styles.eyebrow}>PACKET-CORRELATED RF TRACE</Text>
          <Text style={styles.panelTitle}>Route, radio and proof evidence</Text>
          <Text style={styles.help}>
            Board monotonic times, TxDone evidence and receiver-local signal are retained separately
            from app import time. A successful TX is not itself proof of delivery.
          </Text>
        </View>
        <Pressable
          accessibilityLabel="Refresh RF trace"
          accessibilityRole="button"
          disabled={controlsDisabled}
          onPress={onRefresh}
          style={({ pressed }) => [
            styles.compactButton,
            controlsDisabled && styles.disabled,
            pressed && !controlsDisabled && styles.pressed,
          ]}
        >
          {loading && page === null ? (
            <ActivityIndicator color={colors.green} size="small" />
          ) : (
            <Text style={styles.buttonText}>Refresh</Text>
          )}
        </Pressable>
      </View>

      <View style={styles.privacyNotice}>
        <Text style={styles.privacyTitle}>Private diagnostic data</Text>
        <Text style={styles.help}>
          Exports can contain precise phone coordinates, peer identities, packet hashes and radio
          timing. Share them deliberately. Message bodies and appliance credentials are excluded.
        </Text>
        <View style={styles.exportActions}>
          {(["json", "csv"] as const).map((format) => (
            <Pressable
              accessibilityLabel={`Export complete RF trace as ${format.toUpperCase()}`}
              accessibilityRole="button"
              disabled={controlsDisabled}
              key={format}
              onPress={() => onExport(format)}
              style={({ pressed }) => [
                styles.exportButton,
                controlsDisabled && styles.disabled,
                pressed && !controlsDisabled && styles.pressed,
              ]}
            >
              {exporting === format ? (
                <ActivityIndicator color={colors.green} size="small" />
              ) : (
                <Text style={styles.buttonText}>Export {format.toUpperCase()}</Text>
              )}
            </Pressable>
          ))}
        </View>
      </View>

      {page?.history_incomplete ? (
        <View style={styles.incompleteNotice}>
          <Text style={styles.incompleteTitle}>Earlier RF history is incomplete</Text>
          <Text style={styles.help}>
            At least one board boot reported that its bounded trace ring had already overwritten
            earlier observations.
          </Text>
        </View>
      ) : null}
      {error === null ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.error}>
          RF trace update failed: {error}
          {page === null ? "" : " · showing retained results"}
        </Text>
      )}
      {exportError === null ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.error}>
          RF trace export failed: {exportError}
        </Text>
      )}

      <TextInput
        accessibilityLabel="Search RF trace"
        autoCapitalize="none"
        autoCorrect={false}
        editable={!disabled}
        onChangeText={setQuery}
        placeholder="Filter by outcome, destination, packet hash, token, or profile"
        placeholderTextColor={colors.muted}
        style={styles.search}
        value={query}
      />
      <View accessibilityRole="tablist" style={styles.filters}>
        {RADIO_TRACE_FILTERS.map((option) => {
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
        {visible.length} of {events.length} retained event{events.length === 1 ? "" : "s"} · newest
        first
      </Text>
      <RadioTraceEventList
        emptyMessage={
          events.length === 0
            ? loading
              ? "Reading durable RF trace…"
              : "No RF trace events have been imported yet."
            : "No RF trace events match these filters."
        }
        events={visible}
      />
      {loading && page !== null ? (
        <View style={styles.loading}>
          <ActivityIndicator color={colors.green} size="small" />
          <Text style={styles.help}>Updating RF trace…</Text>
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
          <Text style={styles.loadOlderText}>Load older RF events</Text>
        </Pressable>
      ) : events.length === 0 ? null : (
        <Text style={styles.endOfHistory}>End of retained RF trace</Text>
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

const styles = StyleSheet.create({
  panel: {
    padding: 12,
    gap: 10,
    borderColor: "#506b88",
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  panelHeading: { flexDirection: "row", alignItems: "flex-start", gap: 9 },
  headingCopy: { flex: 1, minWidth: 0, gap: 2 },
  eyebrow: { color: "#9ac9f4", fontSize: 9, fontWeight: "800", letterSpacing: 1.1 },
  panelTitle: { color: colors.text, fontSize: 16, fontWeight: "800" },
  help: { color: colors.muted, fontSize: 10, lineHeight: 15 },
  compactButton: {
    minHeight: 32,
    minWidth: 62,
    alignItems: "center",
    justifyContent: "center",
    paddingHorizontal: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  buttonText: { color: colors.text, fontSize: 10, fontWeight: "700" },
  privacyNotice: {
    padding: 9,
    gap: 6,
    borderColor: "#675d88",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#211e2c",
  },
  privacyTitle: { color: "#d2c3fa", fontSize: 11, fontWeight: "800" },
  exportActions: { flexDirection: "row", flexWrap: "wrap", gap: 7 },
  exportButton: {
    minHeight: 32,
    minWidth: 92,
    alignItems: "center",
    justifyContent: "center",
    paddingHorizontal: 9,
    borderColor: "#675d88",
    borderWidth: 1,
    borderRadius: 8,
  },
  incompleteNotice: {
    padding: 9,
    gap: 2,
    borderColor: "#7f7348",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#2a271b",
  },
  incompleteTitle: { color: colors.warning, fontSize: 11, fontWeight: "800" },
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
  filterSelected: { borderColor: "#668daf", backgroundColor: "#172a3c" },
  filterText: { color: colors.muted, fontSize: 9, fontWeight: "700" },
  filterTextSelected: { color: "#9ac9f4" },
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
  eventToneNormal: { borderLeftColor: "#668daf" },
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
  eventTitle: { color: colors.text, fontSize: 11, fontWeight: "800" },
  observedAt: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  eventId: { color: colors.muted, fontSize: 9, fontWeight: "800" },
  dangerText: { color: colors.red },
  successText: { color: colors.green },
  warningText: { color: colors.warning },
  metadata: {
    marginTop: 2,
    paddingTop: 5,
    gap: 2,
    borderTopColor: colors.line,
    borderTopWidth: StyleSheet.hairlineWidth,
  },
  metadataLine: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  identifier: { fontFamily: "monospace", fontSize: 8 },
  empty: { paddingVertical: 18, color: colors.muted, fontSize: 10, textAlign: "center" },
  loading: { flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 7 },
  loadOlder: {
    minHeight: 36,
    alignItems: "center",
    justifyContent: "center",
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  loadOlderText: { color: "#9ac9f4", fontSize: 10, fontWeight: "800" },
  endOfHistory: { color: colors.muted, fontSize: 9, textAlign: "center" },
  disabled: { opacity: 0.4 },
  pressed: { opacity: 0.76 },
});

const toneStyles: Record<RadioTracePresentation["tone"], ViewStyle> = {
  danger: styles.eventToneDanger,
  muted: styles.eventToneMuted,
  normal: styles.eventToneNormal,
  success: styles.eventToneSuccess,
  warning: styles.eventToneWarning,
};
