import { Pressable, Text, View } from "react-native";

import type { ApplianceSnapshot } from "../generated/api.ts";
import { connectionStateLabel } from "../lib/appliance-status.ts";
import { styles } from "./appliance-screen-styles.ts";

interface ApplianceChipProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly ready: boolean;
  readonly snapshot: ApplianceSnapshot | null;
}

export function ApplianceChip({
  busy,
  compact,
  label,
  onPress,
  ready,
  snapshot,
}: ApplianceChipProps) {
  const connection = snapshot?.connection.state;
  return (
    <Pressable
      accessibilityLabel={`Manage appliance ${label}`}
      accessibilityRole="button"
      accessibilityState={{ disabled: busy }}
      disabled={busy}
      onPress={onPress}
      style={({ pressed }) => [
        styles.applianceChip,
        compact && styles.applianceChipCompact,
        pressed && styles.buttonPressed,
      ]}
    >
      <View
        style={[
          styles.applianceChipDot,
          connection === "ready" && styles.applianceChipDotReady,
          connection === "faulted" && styles.applianceChipDotFaulted,
        ]}
      />
      <View style={styles.applianceChipCopy}>
        <Text numberOfLines={1} style={styles.applianceChipLabel}>
          {label}
        </Text>
        <Text numberOfLines={1} style={styles.applianceChipSub}>
          {ready ? connectionStateLabel(snapshot?.connection) : "setup required"}
        </Text>
      </View>
    </Pressable>
  );
}
