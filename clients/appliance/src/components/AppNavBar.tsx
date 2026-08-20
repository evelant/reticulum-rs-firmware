import Ionicons from "@expo/vector-icons/Ionicons";
import { Pressable, Text, View } from "react-native";

import type { ApplianceWorkspace } from "../lib/navigation.ts";
import { WORKSPACE_DESTINATIONS } from "../lib/navigation.ts";
import { styles } from "./appliance-screen-styles.ts";
import { applianceScreenColors as colors } from "./appliance-screen-theme.ts";

const WORKSPACE_ICONS: Record<ApplianceWorkspace, keyof typeof Ionicons.glyphMap> = {
  lxmf: "chatbubbles-outline",
  nomad: "compass-outline",
  activity: "pulse-outline",
  map: "map-outline",
  connectivity: "radio-outline",
};

interface AppNavBarProps {
  readonly connectivityAvailable: boolean;
  readonly nodesPanelVisible: boolean;
  readonly onNavigate: (workspace: ApplianceWorkspace) => void;
  readonly onOpenAppliances: () => void;
  readonly onOpenSettings: () => void;
  readonly onTogglePeople: () => void;
  readonly peoplePanelVisible: boolean;
  readonly showSidebar: boolean;
  readonly workspace: ApplianceWorkspace;
}

export function AppNavBar({
  connectivityAvailable,
  nodesPanelVisible,
  onNavigate,
  onOpenAppliances,
  onOpenSettings,
  onTogglePeople,
  peoplePanelVisible,
  showSidebar,
  workspace,
}: AppNavBarProps) {
  const destinations = WORKSPACE_DESTINATIONS.filter(
    (destination) => destination.workspace !== "connectivity" || connectivityAvailable,
  );

  if (showSidebar) {
    return (
      <View style={styles.navRail}>
        {destinations.map((destination) => {
          const selected = destination.workspace === workspace;
          return (
            <Pressable
              accessibilityLabel={destination.labelWide}
              accessibilityRole="tab"
              accessibilityState={{ selected }}
              key={destination.workspace}
              onPress={() => onNavigate(destination.workspace)}
              style={[styles.navRailButton, selected && styles.navRailButtonActive]}
            >
              <Ionicons
                color={selected ? colors.green : colors.muted}
                name={WORKSPACE_ICONS[destination.workspace]}
                size={20}
              />
            </Pressable>
          );
        })}
        <View style={styles.navRailDivider} />
        <Pressable
          accessibilityLabel={peoplePanelVisible ? "Hide contacts" : "Show contacts"}
          accessibilityRole="button"
          accessibilityState={{ selected: peoplePanelVisible }}
          onPress={onTogglePeople}
          style={[styles.navRailButton, peoplePanelVisible && styles.navRailButtonActive]}
        >
          <Ionicons
            color={peoplePanelVisible ? colors.green : colors.muted}
            name="people-outline"
            size={20}
          />
        </Pressable>
        <View style={styles.navRailSpacer} />
        <Pressable
          accessibilityLabel={nodesPanelVisible ? "Hide appliances" : "Show appliances"}
          accessibilityRole="button"
          accessibilityState={{ selected: nodesPanelVisible }}
          onPress={onOpenAppliances}
          style={[styles.navRailButton, nodesPanelVisible && styles.navRailButtonActive]}
        >
          <Ionicons
            color={nodesPanelVisible ? colors.green : colors.muted}
            name="hardware-chip-outline"
            size={20}
          />
        </Pressable>
        <Pressable
          accessibilityLabel="Settings"
          accessibilityRole="button"
          onPress={onOpenSettings}
          style={styles.navRailButton}
        >
          <Ionicons color={colors.muted} name="settings-outline" size={20} />
        </Pressable>
      </View>
    );
  }

  return (
    <View accessibilityRole="tablist" style={styles.bottomBar}>
      {destinations.map((destination) => {
        const selected = destination.workspace === workspace;
        return (
          <Pressable
            accessibilityRole="tab"
            accessibilityState={{ selected }}
            key={destination.workspace}
            onPress={() => onNavigate(destination.workspace)}
            style={[styles.bottomTab, selected && styles.bottomTabActive]}
          >
            {selected ? <View style={styles.bottomTabIndicator} /> : null}
            <Ionicons
              color={selected ? colors.green : colors.muted}
              name={WORKSPACE_ICONS[destination.workspace]}
              size={20}
            />
            <Text style={[styles.bottomTabLabel, selected && styles.bottomTabLabelActive]}>
              {destination.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
