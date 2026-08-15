import { useEffect, useMemo, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import type {
  TransmissionMapFeatureDetails,
  TransmissionMapScene,
  TransmissionMapTone,
} from "../lib/transmission-map.ts";
import { TransmissionMap } from "./TransmissionMap";

export interface TransmissionMapPanelProps {
  readonly compact?: boolean;
  readonly disabled?: boolean;
  readonly evidenceError?: string | null;
  readonly evidenceLoading?: boolean;
  readonly error: string | null;
  readonly hasOlder: boolean;
  readonly loading: boolean;
  readonly onLoadOlder: () => void;
  readonly onRefresh: () => void;
  readonly onSelectFeature?: (details: TransmissionMapFeatureDetails | null) => void;
  readonly scene: TransmissionMapScene;
}

function MapButton({
  disabled,
  label,
  onPress,
}: {
  readonly disabled: boolean;
  readonly label: string;
  readonly onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      hitSlop={7}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        disabled && styles.disabled,
        pressed && !disabled && styles.pressed,
      ]}
    >
      <Text style={styles.buttonText}>{label}</Text>
    </Pressable>
  );
}

function LegendItem({
  label,
  tone,
}: {
  readonly label: string;
  readonly tone: TransmissionMapTone;
}) {
  return (
    <View style={styles.legendItem}>
      <View style={[styles.legendDot, toneStyles[tone]]} />
      <Text style={styles.legendText}>{label}</Text>
    </View>
  );
}

function FeatureDetails({
  candidateCount,
  candidatePosition,
  details,
  evidenceError,
  evidenceLoading,
  onClose,
  onNext,
  onNextHere,
  onPrevious,
}: {
  readonly candidateCount: number;
  readonly candidatePosition: number;
  readonly details: TransmissionMapFeatureDetails;
  readonly evidenceError: string | null;
  readonly evidenceLoading: boolean;
  readonly onClose: () => void;
  readonly onNext: () => void;
  readonly onNextHere: () => void;
  readonly onPrevious: () => void;
}) {
  return (
    <View accessibilityLiveRegion="polite" style={styles.detailsCard}>
      <View style={styles.detailsHeading}>
        <View style={styles.detailsHeadingCopy}>
          <Text style={styles.detailsTitle}>{details.title}</Text>
          <Text style={styles.detailsSubtitle}>{details.subtitle}</Text>
        </View>
        <Pressable
          accessibilityLabel="Close map feature details"
          accessibilityRole="button"
          hitSlop={10}
          onPress={onClose}
          style={({ pressed }) => [styles.closeButton, pressed && styles.pressed]}
        >
          <Text style={styles.closeButtonText}>Close</Text>
        </Pressable>
      </View>
      <ScrollView contentContainerStyle={styles.detailRows} nestedScrollEnabled>
        {candidateCount > 1 ? (
          <View style={styles.detailRow}>
            <Text style={styles.detailLabel}>At this position</Text>
            <Text style={styles.detailValue}>
              Observation {candidatePosition + 1} of {candidateCount}
            </Text>
          </View>
        ) : null}
        {evidenceLoading ? (
          <View style={styles.evidenceStatus}>
            <ActivityIndicator color={colors.green} size="small" />
            <Text style={styles.detailValue}>Loading complete message-scoped RF evidence…</Text>
          </View>
        ) : null}
        {evidenceError === null ? null : (
          <Text accessibilityLiveRegion="polite" style={styles.errorText}>
            RF evidence update failed: {evidenceError}
          </Text>
        )}
        {details.rows.map((row) => (
          <View key={`${row.label}:${row.value}`} style={styles.detailRow}>
            <Text style={styles.detailLabel}>{row.label}</Text>
            <Text selectable style={styles.detailValue}>
              {row.value}
            </Text>
          </View>
        ))}
        <View style={styles.detailActions}>
          <MapButton disabled={false} label="Previous" onPress={onPrevious} />
          <MapButton disabled={false} label="Next" onPress={onNext} />
          {candidateCount > 1 ? (
            <MapButton disabled={false} label="Next here" onPress={onNextHere} />
          ) : null}
        </View>
      </ScrollView>
    </View>
  );
}

export function TransmissionMapPanel({
  compact = false,
  disabled = false,
  evidenceError = null,
  evidenceLoading = false,
  error,
  hasOlder,
  loading,
  onLoadOlder,
  onRefresh,
  onSelectFeature,
  scene,
}: TransmissionMapPanelProps) {
  const [mapError, setMapError] = useState<string | null>(null);
  const [mapReady, setMapReady] = useState(false);
  const [selectedFeatureId, setSelectedFeatureId] = useState<string | null>(null);
  const [selectionCandidateIds, setSelectionCandidateIds] = useState<readonly string[]>([]);
  const [viewportRevision, setViewportRevision] = useState(0);
  const selectedDetails =
    selectedFeatureId === null ? undefined : scene.detailsByFeatureId[selectedFeatureId];
  const controlsDisabled = disabled || loading;
  const empty = scene.points.features.length === 0;
  const orderedFeatureIds = useMemo(
    () =>
      [...scene.points.features, ...scene.lines.features].map((feature) => feature.properties.id),
    [scene.lines.features, scene.points.features],
  );
  const availableCandidateIds = selectionCandidateIds.filter(
    (featureId) => scene.detailsByFeatureId[featureId] !== undefined,
  );
  const candidatePosition =
    selectedFeatureId === null ? -1 : availableCandidateIds.indexOf(selectedFeatureId);

  const chooseFeature = (featureId: string, candidates: readonly string[]) => {
    const details = scene.detailsByFeatureId[featureId];
    if (details === undefined) return;
    setSelectionCandidateIds(candidates);
    setSelectedFeatureId(featureId);
    onSelectFeature?.(details);
  };
  const clearSelection = () => {
    setSelectionCandidateIds([]);
    setSelectedFeatureId(null);
    onSelectFeature?.(null);
  };
  const selectFeatures = (featureIds: readonly string[]) => {
    const candidates = [
      ...new Set(
        featureIds.filter((featureId) => scene.detailsByFeatureId[featureId] !== undefined),
      ),
    ];
    if (candidates.length === 0) {
      clearSelection();
      return;
    }
    const selectedCandidate =
      selectedFeatureId === null ? -1 : candidates.indexOf(selectedFeatureId);
    const next = candidates[(selectedCandidate + 1) % candidates.length];
    if (next !== undefined) chooseFeature(next, candidates);
  };
  const selectAdjacentFeature = (offset: -1 | 1) => {
    if (orderedFeatureIds.length === 0) return;
    const current = selectedFeatureId === null ? -1 : orderedFeatureIds.indexOf(selectedFeatureId);
    const nextIndex =
      current === -1
        ? offset === 1
          ? 0
          : orderedFeatureIds.length - 1
        : (current + offset + orderedFeatureIds.length) % orderedFeatureIds.length;
    const next = orderedFeatureIds[nextIndex];
    if (next !== undefined) chooseFeature(next, [next]);
  };
  const selectNextCandidate = () => {
    if (availableCandidateIds.length < 2) return;
    const next = availableCandidateIds[(candidatePosition + 1) % availableCandidateIds.length];
    if (next !== undefined) chooseFeature(next, availableCandidateIds);
  };

  useEffect(() => {
    if (selectedFeatureId === null || selectedDetails !== undefined) return;
    setSelectionCandidateIds([]);
    setSelectedFeatureId(null);
    onSelectFeature?.(null);
  }, [onSelectFeature, selectedDetails, selectedFeatureId]);

  return (
    <View style={styles.panel}>
      <View style={[styles.toolbar, compact && styles.toolbarCompact]}>
        <View style={styles.headingCopy}>
          <Text style={styles.eyebrow}>FIELD TELEMETRY MAP</Text>
          <Text style={styles.title}>
            {compact ? "Transmission map" : "Transmission and shared-message locations"}
          </Text>
          {compact ? null : (
            <Text numberOfLines={3} style={styles.help}>
              Pins are retained phone observations and sender-attached LXMF locations. Solid green
              lines show phone-to-phone reception endpoint separation; dashed blue lines connect
              queue observations chronologically. Neither is necessarily an RF path or Reticulum
              route.
            </Text>
          )}
        </View>
        <View style={styles.actions}>
          <MapButton
            disabled={empty}
            label="Fit"
            onPress={() => setViewportRevision((revision) => revision + 1)}
          />
          <MapButton disabled={empty} label="Browse" onPress={() => selectAdjacentFeature(1)} />
          {hasOlder ? (
            <MapButton disabled={controlsDisabled} label="Older" onPress={onLoadOlder} />
          ) : null}
          <MapButton disabled={controlsDisabled} label="Refresh" onPress={onRefresh} />
        </View>
      </View>

      <View style={styles.summary}>
        <Text style={styles.summaryText}>{scene.summary.attemptCount} attempt locations</Text>
        <Text style={styles.summaryText}>
          {scene.summary.messageLocationCount} shared locations
        </Text>
        <Text style={styles.summaryText}>
          {scene.summary.messageReceptionLinkCount} reception links
        </Text>
        <Text style={styles.summaryText}>
          {scene.summary.observationSegmentCount} observation segments
        </Text>
      </View>

      {error === null ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.error}>
          Map data update failed: {error}
        </Text>
      )}
      {scene.historyIncomplete ? (
        <Text accessibilityLiveRegion="polite" style={styles.warning}>
          Earlier activity or RF evidence has already rolled out of this profile&apos;s retained
          history.
        </Text>
      ) : null}

      <View style={[styles.mapFrame, compact && styles.mapFrameCompact]}>
        <TransmissionMap
          onMapError={setMapError}
          onMapReady={() => setMapReady(true)}
          onSelectFeatures={selectFeatures}
          scene={scene}
          selectedFeatureId={selectedFeatureId}
          viewportRevision={viewportRevision}
        />
        <View pointerEvents="none" style={styles.legend}>
          <LegendItem label="Delivered / receiver" tone="success" />
          <LegendItem label="Pending / TxDone" tone="warning" />
          <LegendItem label="Failed" tone="danger" />
          <LegendItem label="Info / shared" tone="info" />
          <LegendItem label="Queued / sent" tone="muted" />
        </View>
        {loading && empty ? (
          <View pointerEvents="none" style={styles.mapNotice}>
            <ActivityIndicator color={colors.green} />
            <Text style={styles.noticeText}>Loading retained locations…</Text>
          </View>
        ) : empty && !loading ? (
          <View pointerEvents="none" style={styles.mapNotice}>
            <Text style={styles.noticeTitle}>No mapped observations yet</Text>
            <Text style={styles.noticeText}>
              Enable field location telemetry and send a message, or receive an LXMF message with an
              attached location.
            </Text>
          </View>
        ) : null}
        {!mapReady && mapError === null ? (
          <View pointerEvents="none" style={styles.loadingBadge}>
            <ActivityIndicator color={colors.green} size="small" />
            <Text style={styles.loadingBadgeText}>Loading map…</Text>
          </View>
        ) : null}
        {mapError === null ? null : (
          <View
            accessibilityLiveRegion="assertive"
            accessibilityRole="alert"
            pointerEvents="none"
            style={styles.mapErrorBadge}
          >
            <Text style={styles.mapErrorText}>{mapError}</Text>
            <Text style={styles.mapErrorHint}>
              Location records remain available; the online basemap needs a network connection.
            </Text>
          </View>
        )}
        {selectedDetails === undefined ? null : (
          <FeatureDetails
            candidateCount={availableCandidateIds.length}
            candidatePosition={Math.max(0, candidatePosition)}
            details={selectedDetails}
            evidenceError={evidenceError}
            evidenceLoading={evidenceLoading}
            onClose={clearSelection}
            onNext={() => selectAdjacentFeature(1)}
            onNextHere={selectNextCandidate}
            onPrevious={() => selectAdjacentFeature(-1)}
          />
        )}
      </View>
    </View>
  );
}

const colors = {
  background: "#101411",
  blue: "#62a9e8",
  green: "#91e6a7",
  line: "#303b33",
  muted: "#93a096",
  panel: "#171d19",
  panel2: "#1d2520",
  red: "#ff6f61",
  text: "#ecf2ea",
  warning: "#e8c766",
} as const;

const toneStyles: Record<TransmissionMapTone, { backgroundColor: string }> = {
  danger: { backgroundColor: colors.red },
  info: { backgroundColor: colors.blue },
  muted: { backgroundColor: colors.muted },
  success: { backgroundColor: "#50d890" },
  warning: { backgroundColor: colors.warning },
};

const styles = StyleSheet.create({
  actions: { flexDirection: "row", flexWrap: "wrap", gap: 6, justifyContent: "flex-end" },
  button: {
    backgroundColor: colors.panel2,
    borderColor: colors.line,
    borderRadius: 7,
    borderWidth: 1,
    paddingHorizontal: 10,
    paddingVertical: 7,
  },
  buttonText: { color: colors.green, fontSize: 11, fontWeight: "700" },
  closeButton: { paddingHorizontal: 6, paddingVertical: 4 },
  closeButtonText: { color: colors.green, fontSize: 11, fontWeight: "700" },
  detailLabel: { color: colors.muted, fontSize: 10, fontWeight: "700", textTransform: "uppercase" },
  detailActions: { flexDirection: "row", flexWrap: "wrap", gap: 6, paddingTop: 3 },
  detailRow: { gap: 2 },
  detailRows: { gap: 9, paddingBottom: 2 },
  detailValue: { color: colors.text, fontSize: 12, lineHeight: 17 },
  detailsCard: {
    backgroundColor: "rgba(16, 20, 17, 0.96)",
    borderColor: "#506b88",
    borderRadius: 10,
    borderWidth: 1,
    bottom: 10,
    left: 10,
    maxHeight: "48%",
    padding: 12,
    position: "absolute",
    right: 10,
  },
  detailsHeading: {
    flexDirection: "row",
    gap: 10,
    justifyContent: "space-between",
    marginBottom: 9,
  },
  detailsHeadingCopy: { flex: 1, gap: 2 },
  detailsSubtitle: { color: colors.muted, fontSize: 11 },
  detailsTitle: { color: colors.text, fontSize: 15, fontWeight: "800" },
  disabled: { opacity: 0.45 },
  errorText: { color: colors.red, fontSize: 11, lineHeight: 15 },
  error: { color: colors.red, fontSize: 11, paddingHorizontal: 12, paddingBottom: 7 },
  evidenceStatus: { alignItems: "center", flexDirection: "row", gap: 7 },
  eyebrow: { color: colors.blue, fontSize: 10, fontWeight: "800", letterSpacing: 0.8 },
  headingCopy: { flex: 1, gap: 3, minWidth: 190 },
  help: { color: colors.muted, fontSize: 11, lineHeight: 15 },
  legend: {
    backgroundColor: "rgba(16, 20, 17, 0.86)",
    borderColor: colors.line,
    borderRadius: 8,
    borderWidth: 1,
    flexDirection: "row",
    flexWrap: "wrap",
    gap: 8,
    left: 9,
    paddingHorizontal: 8,
    paddingVertical: 6,
    position: "absolute",
    top: 9,
  },
  legendDot: { borderRadius: 4, height: 8, width: 8 },
  legendItem: { alignItems: "center", flexDirection: "row", gap: 4 },
  legendText: { color: colors.text, fontSize: 9 },
  loadingBadge: {
    alignItems: "center",
    backgroundColor: "rgba(16, 20, 17, 0.9)",
    borderRadius: 8,
    flexDirection: "row",
    gap: 7,
    left: "50%",
    paddingHorizontal: 10,
    paddingVertical: 8,
    position: "absolute",
    top: "50%",
    transform: [{ translateX: -60 }, { translateY: -18 }],
  },
  loadingBadgeText: { color: colors.text, fontSize: 11 },
  mapErrorBadge: {
    backgroundColor: "rgba(44, 18, 17, 0.94)",
    borderColor: colors.red,
    borderRadius: 8,
    borderWidth: 1,
    left: 12,
    padding: 10,
    position: "absolute",
    right: 12,
    top: 52,
  },
  mapErrorHint: { color: colors.muted, fontSize: 10, marginTop: 3 },
  mapErrorText: { color: colors.red, fontSize: 11, fontWeight: "700" },
  mapFrame: { backgroundColor: "#17202a", flex: 1, minHeight: 220, position: "relative" },
  mapFrameCompact: { minHeight: 140 },
  mapNotice: {
    alignItems: "center",
    backgroundColor: "rgba(16, 20, 17, 0.88)",
    borderColor: colors.line,
    borderRadius: 10,
    borderWidth: 1,
    gap: 6,
    left: "15%",
    padding: 14,
    position: "absolute",
    right: "15%",
    top: "40%",
  },
  noticeText: { color: colors.muted, fontSize: 11, lineHeight: 16, textAlign: "center" },
  noticeTitle: { color: colors.text, fontSize: 14, fontWeight: "800" },
  panel: { backgroundColor: colors.background, flex: 1, minHeight: 0 },
  pressed: { opacity: 0.72 },
  summary: {
    backgroundColor: colors.panel2,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
    flexDirection: "row",
    flexWrap: "wrap",
    gap: 12,
    paddingHorizontal: 12,
    paddingVertical: 6,
  },
  summaryText: { color: colors.muted, fontSize: 10 },
  title: { color: colors.text, fontSize: 15, fontWeight: "800" },
  toolbar: {
    alignItems: "center",
    backgroundColor: colors.panel,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
    flexDirection: "row",
    flexWrap: "wrap",
    gap: 10,
    padding: 10,
  },
  toolbarCompact: { paddingHorizontal: 9, paddingVertical: 7 },
  warning: { color: colors.warning, fontSize: 11, paddingHorizontal: 12, paddingVertical: 6 },
});
