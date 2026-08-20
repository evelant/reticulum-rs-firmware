import Ionicons from "@expo/vector-icons/Ionicons";
import { Pressable, Text, View } from "react-native";

import type { ApplianceSnapshot } from "../generated/api.ts";
import type { ApplianceWorkspace } from "../lib/navigation.ts";
import { workspaceTitle } from "../lib/navigation.ts";
import { ApplianceChip } from "./ApplianceChip.tsx";
import { styles } from "./appliance-screen-styles.ts";
import { applianceScreenColors as colors } from "./appliance-screen-theme.ts";

interface AppTopBarProps {
  readonly busy: boolean;
  readonly chipLabel: string;
  readonly compact: boolean;
  readonly onOpenAppliances: () => void;
  readonly onOpenSettings: () => void;
  readonly ready: boolean;
  readonly snapshot: ApplianceSnapshot | null;
  readonly workspace: ApplianceWorkspace;
}

export function AppTopBar({
  busy,
  chipLabel,
  compact,
  onOpenAppliances,
  onOpenSettings,
  ready,
  snapshot,
  workspace,
}: AppTopBarProps) {
  const settingsButton = (
    <Pressable
      accessibilityLabel="Open settings"
      accessibilityRole="button"
      onPress={onOpenSettings}
      style={({ pressed }) => [
        styles.topBarIconButton,
        compact && styles.topBarIconButtonCompact,
        pressed && styles.buttonPressed,
      ]}
    >
      <Ionicons color={colors.muted} name="settings-outline" size={compact ? 18 : 16} />
      {compact ? null : <Text style={styles.topBarIconText}>Settings</Text>}
    </Pressable>
  );

  return (
    <View style={[styles.topbar, compact && styles.topbarCompact]}>
      {compact ? (
        <View style={styles.brandClusterCompact}>
          <View style={styles.topBarActions}>
            <ApplianceChip
              busy={busy}
              compact
              label={chipLabel}
              onPress={onOpenAppliances}
              ready={ready}
              snapshot={snapshot}
            />
            {settingsButton}
          </View>
        </View>
      ) : (
        <>
          <View>
            <Text style={styles.eyebrow}>RETICULUM APPLIANCE</Text>
            <Text style={styles.title}>{workspaceTitle(workspace)}</Text>
          </View>
          <View style={styles.topBarActions}>
            <ApplianceChip
              busy={busy}
              compact={false}
              label={chipLabel}
              onPress={onOpenAppliances}
              ready={ready}
              snapshot={snapshot}
            />
            {settingsButton}
          </View>
        </>
      )}
    </View>
  );
}
