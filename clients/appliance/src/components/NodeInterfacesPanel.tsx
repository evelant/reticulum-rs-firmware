import { StyleSheet, Text, View } from "react-native";

import type { NetworkSubtopic } from "../lib/navigation.ts";
import type {
  NodeInterfaceKind,
  NodeInterfaceState,
  NodeInterfaceSummary,
} from "../lib/node-interfaces.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { styles as shared } from "./appliance-screen-styles.ts";
import { applianceScreenColors as colors } from "./appliance-screen-theme.ts";

function stateLabel(state: NodeInterfaceState): string {
  switch (state) {
    case "online":
      return "ONLINE";
    case "offline":
      return "OFFLINE";
    case "faulted":
      return "FAULTED";
    case "unknown":
      return "UNKNOWN";
  }
}

function configuredSubtopic(kind: NodeInterfaceKind): NetworkSubtopic | null {
  switch (kind) {
    case "lora":
      return "radio";
    case "tcp_client":
    case "tcp_server":
      return "peers";
    case "wifi_station":
      return "overview";
    case "bluetooth":
    case "other":
      return null;
  }
}

interface NodeInterfacesPanelProps {
  readonly interfaces: readonly NodeInterfaceSummary[];
  readonly onConfigure?: (subtopic: NetworkSubtopic) => void;
}

export function NodeInterfacesPanel({ interfaces, onConfigure }: NodeInterfacesPanelProps) {
  if (interfaces.length === 0) return null;

  return (
    <View style={styles.section}>
      <View style={styles.heading}>
        <Text style={shared.eyebrow}>INTERFACES</Text>
        <Text style={styles.count}>
          {interfaces.filter((iface) => iface.state === "online").length}/{interfaces.length} online
        </Text>
      </View>
      {interfaces.map((iface) => {
        const subtopic = configuredSubtopic(iface.kind);
        return (
          <View key={iface.key} style={styles.card}>
            <View style={styles.cardHeading}>
              <View style={styles.cardCopy}>
                <Text style={styles.label}>{iface.label}</Text>
                <Text numberOfLines={2} style={styles.summary}>
                  {iface.summary}
                </Text>
              </View>
              <View style={styles.cardActions}>
                <Text
                  style={[
                    styles.pill,
                    iface.state === "online" && styles.pillOnline,
                    iface.state === "faulted" && styles.pillFaulted,
                  ]}
                >
                  {stateLabel(iface.state)}
                </Text>
                {subtopic === null || onConfigure === undefined ? null : (
                  <ActionButton label="Configure" onPress={() => onConfigure(subtopic)} secondary />
                )}
              </View>
            </View>
            {iface.metrics.length === 0 ? null : (
              <View style={styles.metrics}>
                {iface.metrics.map((metric) => (
                  <View key={metric.label} style={styles.metricRow}>
                    <Text style={styles.metricLabel}>{metric.label}</Text>
                    <Text selectable style={styles.metricValue}>
                      {metric.value}
                    </Text>
                  </View>
                ))}
              </View>
            )}
          </View>
        );
      })}
    </View>
  );
}

const styles = StyleSheet.create({
  section: { gap: 9 },
  heading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  count: { color: colors.muted, fontSize: 11 },
  card: {
    padding: 12,
    gap: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  cardHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 10,
  },
  cardCopy: { flex: 1, minWidth: 0, gap: 3 },
  cardActions: { flexDirection: "row", alignItems: "center", gap: 8 },
  label: { color: colors.text, fontSize: 15, fontWeight: "800" },
  summary: { color: colors.muted, fontSize: 11, lineHeight: 16 },
  pill: {
    paddingHorizontal: 8,
    paddingVertical: 3,
    borderRadius: 999,
    borderColor: colors.line,
    borderWidth: 1,
    color: colors.muted,
    fontSize: 9,
    fontWeight: "800",
  },
  pillOnline: { borderColor: "#356344", backgroundColor: colors.greenDark, color: colors.green },
  pillFaulted: { borderColor: "#70413d", backgroundColor: "#321d1b", color: colors.red },
  metrics: {
    flexDirection: "row",
    flexWrap: "wrap",
    gap: 6,
    paddingTop: 9,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  metricRow: {
    flexGrow: 1,
    flexBasis: 130,
    minWidth: 0,
    gap: 1,
    paddingHorizontal: 8,
    paddingVertical: 6,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel2,
  },
  metricLabel: {
    color: colors.muted,
    fontSize: 8,
    fontWeight: "800",
    letterSpacing: 1,
    textTransform: "uppercase",
  },
  metricValue: { color: colors.text, fontSize: 11 },
});
