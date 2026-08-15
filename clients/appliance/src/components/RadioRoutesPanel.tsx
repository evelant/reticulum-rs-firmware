import { useEffect, useMemo, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";

import type {
  DiagnosticInterfaceView,
  RadioRoutesStatusView,
  RetainedRouteView,
} from "../generated/api.ts";
import {
  compactDurationLabel,
  elapsedAgeLabel,
  loraDataTxEvidenceLabel,
  loraTxSummaryLabel,
  type RadioRoutesControllerState,
  type RetainedRouteTransportFamily,
  retainedRouteFamily,
  routeExpiryLabel,
} from "../lib/radio-routes.ts";

const COLLAPSED_ROUTE_LIMIT = 4;

const ROUTE_FAMILY_TABS: readonly {
  readonly label: string;
  readonly value: RetainedRouteTransportFamily;
}[] = [
  { label: "LoRa", value: "lora" },
  { label: "TCP", value: "tcp" },
  { label: "Fallback & other", value: "other" },
];

interface RadioRoutesPanelProps {
  readonly disabled?: boolean;
  readonly onRefresh: () => void;
  readonly state: RadioRoutesControllerState;
}

function countLabel(value: number): string {
  return new Intl.NumberFormat().format(value);
}

function interfaceKindLabel(kind: DiagnosticInterfaceView["kind"]): string {
  switch (kind) {
    case "lora":
      return "LoRa";
    case "tcp":
      return "Reticulum TCP";
    case "other":
      return "Other";
  }
}

function interfaceStateLabel(state: DiagnosticInterfaceView["state"]): string {
  switch (state) {
    case "online":
      return "Online";
    case "offline":
      return "Offline";
    case "faulted":
      return "Faulted";
  }
}

function routeResolutionLabel(resolution: RetainedRouteView["resolution"]): string {
  switch (resolution) {
    case "exact_ready":
      return "Exact interface ready";
    case "exact_offline":
      return "Exact interface offline";
    case "exact_missing":
      return "Exact interface missing";
    case "broadcast_ready":
      return "Broadcast fallback ready";
    case "broadcast_unavailable":
      return "Broadcast fallback unavailable";
  }
}

function routeInterfaceLabel(route: RetainedRouteView, snapshot: RadioRoutesStatusView): string {
  if (route.retained_interface_id === null) return "broadcast fallback";
  const record = snapshot.interfaces.find(
    (candidate) => candidate.id === route.retained_interface_id,
  );
  return record === undefined
    ? `interface ${route.retained_interface_id}`
    : `${interfaceKindLabel(record.kind)} · interface ${record.id}`;
}

function familyEmptyLabel(family: RetainedRouteTransportFamily): string {
  switch (family) {
    case "lora":
      return "No LoRa routes are currently retained.";
    case "tcp":
      return "No TCP routes are currently retained.";
    case "other":
      return "No fallback or other-interface routes are currently retained.";
  }
}

function shortHash(hash: string): string {
  return `${hash.slice(0, 8)}…${hash.slice(-6)}`;
}

function bitrateLabel(bitsPerSecond: number | null): string {
  if (bitsPerSecond === null) return "bitrate not advertised";
  return bitsPerSecond >= 1_000_000
    ? `${(bitsPerSecond / 1_000_000).toFixed(1)} Mbps`
    : bitsPerSecond >= 1_000
      ? `${(bitsPerSecond / 1_000).toFixed(1)} kbps`
      : `${bitsPerSecond} bps`;
}

function Metric({ label, value }: { readonly label: string; readonly value: number | string }) {
  return (
    <View style={styles.metric}>
      <Text style={styles.metricLabel}>{label}</Text>
      <Text selectable style={styles.metricValue}>
        {typeof value === "number" ? countLabel(value) : value}
      </Text>
    </View>
  );
}

function RouteRow({
  route,
  snapshot,
}: {
  readonly route: RetainedRouteView;
  readonly snapshot: RadioRoutesStatusView;
}) {
  const unavailable =
    route.resolution === "exact_offline" ||
    route.resolution === "exact_missing" ||
    route.resolution === "broadcast_unavailable";
  return (
    <View style={styles.routeRow}>
      <View style={styles.rowHeading}>
        <Text selectable style={styles.routeHash}>
          {route.destination}
        </Text>
        <Text style={[styles.routeState, unavailable && styles.routeStateUnavailable]}>
          {routeResolutionLabel(route.resolution)}
        </Text>
      </View>
      <Text style={styles.meta}>
        {route.hops === 1 ? "direct · 1 hop" : `${route.hops} hops`} ·{" "}
        {routeInterfaceLabel(route, snapshot)}
      </Text>
      <Text selectable style={styles.meta}>
        {route.next_hop_identity === null
          ? "Direct next hop"
          : `Next-hop identity ${shortHash(route.next_hop_identity)}`}
      </Text>
      <Text style={styles.meta}>
        Learned: {elapsedAgeLabel(route.learned_age_ms)} · local LRU use:{" "}
        {elapsedAgeLabel(route.last_local_use_age_ms)} · {routeExpiryLabel(route.expires_in_ms)}
      </Text>
    </View>
  );
}

export function RadioRoutesPanel({ disabled = false, onRefresh, state }: RadioRoutesPanelProps) {
  const [expanded, setExpanded] = useState(false);
  const [routeFamily, setRouteFamily] = useState<RetainedRouteTransportFamily>("lora");
  const [showAllRoutes, setShowAllRoutes] = useState(false);
  const snapshot = state.snapshot;

  useEffect(() => {
    if (!expanded) setShowAllRoutes(false);
  }, [expanded]);

  const loraInterface = snapshot?.interfaces.find((record) => record.kind === "lora") ?? null;
  const summary =
    snapshot === null
      ? state.loadState === "loading"
        ? "Reading live node state…"
        : "Live radio and route state unavailable"
      : [
          loraInterface === null ? "LoRa not registered" : `LoRa ${loraInterface.state}`,
          snapshot.lora === null
            ? null
            : `${snapshot.lora.applied_tx_power_dbm >= 0 ? "+" : ""}${snapshot.lora.applied_tx_power_dbm} dBm applied`,
          `${snapshot.interfaces.length} interface${snapshot.interfaces.length === 1 ? "" : "s"}`,
          `${snapshot.retained_route_count} retained route${snapshot.retained_route_count === 1 ? "" : "s"}`,
          `${snapshot.observed_peer_count} observed peer${snapshot.observed_peer_count === 1 ? "" : "s"}`,
        ]
          .filter((part): part is string => part !== null)
          .join(" · ");

  const familyRoutes = useMemo(() => {
    const grouped: Record<RetainedRouteTransportFamily, RetainedRouteView[]> = {
      lora: [],
      tcp: [],
      other: [],
    };
    if (snapshot !== null) {
      for (const route of snapshot.routes) {
        grouped[retainedRouteFamily(route, snapshot)].push(route);
      }
    }
    return grouped;
  }, [snapshot]);

  const visibleRoutes =
    snapshot === null || showAllRoutes
      ? familyRoutes[routeFamily]
      : familyRoutes[routeFamily].slice(0, COLLAPSED_ROUTE_LIMIT);

  return (
    <View style={styles.panel}>
      <View style={styles.panelHeading}>
        <Pressable
          accessibilityLabel={`${expanded ? "Hide" : "Show"} radio and route diagnostics`}
          accessibilityRole="button"
          accessibilityState={{ expanded }}
          onPress={() => setExpanded((current) => !current)}
          style={({ pressed }) => [styles.headingButton, pressed && styles.pressed]}
        >
          <View style={styles.headingCopy}>
            <Text style={styles.eyebrow}>RADIO &amp; ROUTES</Text>
            <Text style={styles.title}>Live Reticulum state</Text>
            <Text numberOfLines={expanded ? undefined : 2} style={styles.summary}>
              {summary}
            </Text>
          </View>
          <Text style={styles.disclosure}>{expanded ? "Hide" : "Details"}</Text>
        </Pressable>
        <Pressable
          accessibilityLabel="Refresh radio and route diagnostics"
          accessibilityRole="button"
          disabled={disabled || state.loadState === "loading"}
          onPress={onRefresh}
          style={({ pressed }) => [
            styles.refreshButton,
            (disabled || state.loadState === "loading") && styles.disabled,
            pressed && !disabled && styles.pressed,
          ]}
        >
          {state.loadState === "loading" && snapshot === null ? (
            <ActivityIndicator color={colors.green} size="small" />
          ) : (
            <Text style={styles.refreshText}>Refresh</Text>
          )}
        </Pressable>
      </View>

      {state.error === null ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.error}>
          Live diagnostics update failed: {state.error}
          {snapshot === null ? "" : " · showing the last good snapshot"}
        </Text>
      )}

      {expanded && snapshot !== null ? (
        <View style={styles.details}>
          <View style={styles.subsection}>
            <View style={styles.subsectionHeading}>
              <View style={styles.headingCopy}>
                <Text style={styles.subsectionTitle}>LoRa interface</Text>
                {snapshot.lora === null ? (
                  <Text style={styles.help}>No LoRa-specific runtime record is available.</Text>
                ) : (
                  <Text style={styles.help}>
                    {(snapshot.lora.frequency_hz / 1_000_000).toFixed(3)} MHz · BW{" "}
                    {snapshot.lora.bandwidth_hz / 1_000} kHz · SF
                    {snapshot.lora.spreading_factor} · CR 4/
                    {snapshot.lora.coding_rate_denominator}
                  </Text>
                )}
              </View>
              {snapshot.lora === null ? null : (
                <Text style={styles.powerValue}>
                  {snapshot.lora.applied_tx_power_dbm >= 0 ? "+" : ""}
                  {snapshot.lora.applied_tx_power_dbm} dBm
                </Text>
              )}
            </View>

            {snapshot.lora === null ? null : (
              <>
                <Text style={styles.lastObservation}>
                  Last accepted RX{" "}
                  {snapshot.lora.last_rx === null
                    ? "not observed"
                    : `${elapsedAgeLabel(snapshot.lora.last_rx.age_ms)} · ${snapshot.lora.last_rx.rssi_dbm} dBm · SNR ${snapshot.lora.last_rx.snr_db} dB`}
                </Text>
                <Text style={styles.lastObservation}>
                  Last terminal TX{" "}
                  {snapshot.lora.last_tx === null
                    ? "not observed"
                    : loraTxSummaryLabel(snapshot.lora.last_tx)}
                </Text>
                {snapshot.lora.last_data_tx === null ? (
                  <Text style={styles.lastObservation}>Last DATA TX not observed</Text>
                ) : (
                  <View style={styles.dataEvidence}>
                    <Text style={styles.lastObservation}>
                      Last DATA TX {loraTxSummaryLabel(snapshot.lora.last_data_tx)}
                    </Text>
                    <Text style={styles.meta}>
                      {loraDataTxEvidenceLabel(snapshot.lora.last_data_tx) ??
                        "Prepared packet evidence unavailable"}
                    </Text>
                    {snapshot.lora.last_data_tx.data_evidence === null ? null : (
                      <Text selectable style={styles.packetHash}>
                        SHA-256 {snapshot.lora.last_data_tx.data_evidence.encoded_packet_sha256}
                      </Text>
                    )}
                    <Text style={styles.help}>
                      Prepared-packet evidence matches message details by byte length and SHA-256;
                      it does not by itself assert RF transmission.
                    </Text>
                  </View>
                )}
                <View style={styles.metricGrid}>
                  <Metric label="RX frames" value={snapshot.lora.rx_physical_frames} />
                  <Metric label="RX packets" value={snapshot.lora.rx_packets} />
                  <Metric label="RX errors" value={snapshot.lora.rx_errors} />
                  <Metric label="RX drops" value={snapshot.lora.rx_drops} />
                  <Metric label="TX jobs" value={snapshot.lora.tx_terminal_jobs} />
                  <Metric label="TX success" value={snapshot.lora.tx_successes} />
                  <Metric label="TX frames" value={snapshot.lora.tx_completed_frames} />
                  <Metric label="Access rejects" value={snapshot.lora.tx_access_rejects} />
                  <Metric label="TX failures" value={snapshot.lora.tx_failures} />
                  <Metric label="CAD clear" value={snapshot.lora.cad_clear} />
                  <Metric label="CAD busy" value={snapshot.lora.cad_busy} />
                </View>
              </>
            )}
          </View>

          <View style={styles.subsection}>
            <Text style={styles.subsectionTitle}>Interfaces</Text>
            {snapshot.interfaces.length === 0 ? (
              <Text style={styles.help}>No current interface descriptors.</Text>
            ) : (
              snapshot.interfaces.map((record) => (
                <View key={record.id} style={styles.interfaceRow}>
                  <View style={styles.headingCopy}>
                    <Text style={styles.interfaceName}>
                      {interfaceKindLabel(record.kind)} · interface {record.id}
                    </Text>
                    <Text style={styles.meta}>
                      MTU {record.logical_mtu} · {bitrateLabel(record.bitrate)} · generation{" "}
                      {record.generation}
                    </Text>
                  </View>
                  <Text
                    style={[
                      styles.interfaceState,
                      record.state === "online" && styles.stateReady,
                      record.state === "faulted" && styles.stateFaulted,
                    ]}
                  >
                    {interfaceStateLabel(record.state)}
                  </Text>
                </View>
              ))
            )}
          </View>

          <View style={styles.subsection}>
            <View style={styles.subsectionHeading}>
              <View style={styles.headingCopy}>
                <Text style={styles.subsectionTitle}>Retained routes</Text>
                <Text style={styles.help}>
                  A retained route is routing evidence, not a connected-peer or delivery guarantee.
                  “Local LRU use” is this node&apos;s route-table access, not last-heard time.
                </Text>
              </View>
              <Text style={styles.routeCount}>
                {snapshot.usable_route_count}/{snapshot.retained_route_count} usable
              </Text>
            </View>
            <View accessibilityRole="tablist" style={styles.routeTabs}>
              {ROUTE_FAMILY_TABS.map((option) => {
                const selected = routeFamily === option.value;
                return (
                  <Pressable
                    accessibilityRole="tab"
                    accessibilityState={{ selected }}
                    key={option.value}
                    onPress={() => {
                      setRouteFamily(option.value);
                      setShowAllRoutes(false);
                    }}
                    style={({ pressed }) => [
                      styles.routeTab,
                      selected && styles.routeTabSelected,
                      pressed && styles.pressed,
                    ]}
                  >
                    <Text style={[styles.routeTabText, selected && styles.routeTabTextSelected]}>
                      {option.label} ({familyRoutes[option.value].length})
                    </Text>
                  </Pressable>
                );
              })}
            </View>
            {visibleRoutes.length === 0 ? (
              <Text style={styles.help}>
                {snapshot.routes.length === 0
                  ? "The Rete path table currently retains no routes."
                  : familyEmptyLabel(routeFamily)}
              </Text>
            ) : (
              visibleRoutes.map((route) => (
                <RouteRow key={route.destination} route={route} snapshot={snapshot} />
              ))
            )}
            {familyRoutes[routeFamily].length > COLLAPSED_ROUTE_LIMIT ? (
              <Pressable
                accessibilityRole="button"
                onPress={() => setShowAllRoutes((current) => !current)}
                style={({ pressed }) => [styles.showMore, pressed && styles.pressed]}
              >
                <Text style={styles.showMoreText}>
                  {showAllRoutes
                    ? "Show fewer routes"
                    : `Show all ${familyRoutes[routeFamily].length} routes`}
                </Text>
              </Pressable>
            ) : null}
          </View>

          <View style={styles.subsection}>
            <Text style={styles.subsectionTitle}>Reticulum counters</Text>
            <View style={styles.metricGrid}>
              <Metric label="Received" value={snapshot.rns.received} />
              <Metric label="Forwarded" value={snapshot.rns.forwarded} />
              <Metric label="Duplicates" value={snapshot.rns.dedup_drops} />
              <Metric label="Invalid" value={snapshot.rns.invalid_drops} />
              <Metric label="Announces" value={snapshot.rns.announces_received} />
              <Metric label="Paths learned" value={snapshot.rns.paths_learned} />
              <Metric label="Paths expired" value={snapshot.rns.paths_expired} />
              <Metric label="Links established" value={snapshot.rns.links_established} />
              <Metric label="Links closed" value={snapshot.rns.links_closed} />
              <Metric label="Links failed" value={snapshot.rns.links_failed} />
            </View>
            <Text style={styles.help}>
              Node uptime {compactDurationLabel(snapshot.uptime_ms)} · route-table revision{" "}
              {snapshot.route_table_revision}
            </Text>
          </View>
        </View>
      ) : null}
    </View>
  );
}

const colors = {
  background: "#101411",
  green: "#91e6a7",
  greenDark: "#173f24",
  line: "#303b33",
  muted: "#93a096",
  panel: "#171d19",
  panel2: "#1d2520",
  red: "#ff9b91",
  text: "#ecf2ea",
};

const styles = StyleSheet.create({
  panel: {
    padding: 12,
    gap: 8,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  panelHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 8,
  },
  headingButton: {
    flex: 1,
    minWidth: 0,
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
  },
  headingCopy: { flex: 1, minWidth: 0, gap: 2 },
  eyebrow: { color: colors.green, fontSize: 9, fontWeight: "800", letterSpacing: 1.2 },
  title: { color: colors.text, fontSize: 16, fontWeight: "800" },
  summary: { color: colors.muted, fontSize: 11, lineHeight: 16 },
  disclosure: { color: colors.green, fontSize: 10, fontWeight: "800" },
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
  details: { gap: 9, paddingTop: 3 },
  subsection: {
    padding: 10,
    gap: 8,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  subsectionHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 10,
  },
  subsectionTitle: { color: colors.text, fontSize: 13, fontWeight: "800" },
  help: { color: colors.muted, fontSize: 10, lineHeight: 15 },
  powerValue: {
    color: colors.green,
    fontSize: 16,
    fontWeight: "800",
    fontVariant: ["tabular-nums"],
  },
  lastObservation: { color: colors.text, fontSize: 11, lineHeight: 16 },
  dataEvidence: {
    gap: 3,
    padding: 8,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 7,
    backgroundColor: colors.background,
  },
  packetHash: {
    color: colors.text,
    fontFamily: "monospace",
    fontSize: 8,
    lineHeight: 12,
  },
  metricGrid: { flexDirection: "row", flexWrap: "wrap", gap: 6 },
  metric: {
    minWidth: 92,
    flexGrow: 1,
    flexBasis: 96,
    paddingHorizontal: 8,
    paddingVertical: 6,
    gap: 2,
    borderRadius: 7,
    backgroundColor: colors.background,
  },
  metricLabel: { color: colors.muted, fontSize: 8, fontWeight: "700", letterSpacing: 0.4 },
  metricValue: { color: colors.text, fontSize: 12, fontWeight: "800" },
  interfaceRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
    paddingVertical: 5,
    borderTopColor: colors.line,
    borderTopWidth: StyleSheet.hairlineWidth,
  },
  interfaceName: { color: colors.text, fontSize: 11, fontWeight: "700" },
  interfaceState: { color: colors.muted, fontSize: 10, fontWeight: "800" },
  stateReady: { color: colors.green },
  stateFaulted: { color: colors.red },
  meta: { color: colors.muted, fontSize: 9, lineHeight: 14 },
  routeCount: { color: colors.green, fontSize: 10, fontWeight: "800" },
  routeTabs: { flexDirection: "row", flexWrap: "wrap", gap: 6 },
  routeTab: {
    minHeight: 30,
    justifyContent: "center",
    paddingHorizontal: 9,
    paddingVertical: 5,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  routeTabSelected: { borderColor: "#5b9c69", backgroundColor: colors.greenDark },
  routeTabText: { color: colors.muted, fontSize: 9, fontWeight: "700" },
  routeTabTextSelected: { color: colors.green },
  routeRow: {
    padding: 9,
    gap: 3,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.background,
  },
  rowHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    flexWrap: "wrap",
    gap: 6,
  },
  routeHash: { flexShrink: 1, color: colors.text, fontFamily: "monospace", fontSize: 8 },
  routeState: { color: colors.green, fontSize: 9, fontWeight: "800" },
  routeStateUnavailable: { color: "#cabd98" },
  showMore: {
    alignSelf: "flex-start",
    minHeight: 30,
    justifyContent: "center",
    paddingHorizontal: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 7,
  },
  showMoreText: { color: colors.green, fontSize: 10, fontWeight: "700" },
  error: { color: colors.red, fontSize: 10, lineHeight: 15 },
  disabled: { opacity: 0.4 },
  pressed: { opacity: 0.76 },
});
