import { Pressable, ScrollView, Text, View } from "react-native";

import type { ApplianceSnapshot } from "../generated/api.ts";
import { connectionStateLabel } from "../lib/appliance-status.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { styles } from "./appliance-screen-styles.ts";

export type ApplianceWorkspace = "activity" | "connectivity" | "lxmf" | "map" | "nomad";

interface ApplianceTopBarProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly connectivityAvailable: boolean;
  readonly mobileSidebarVisible: boolean;
  readonly onOpenContacts: () => void;
  readonly onReconnect: () => void;
  readonly onSelectWorkspace: (workspace: ApplianceWorkspace) => void;
  readonly onSync: () => void;
  readonly ready: boolean;
  readonly snapshot: ApplianceSnapshot | null;
  readonly workspace: ApplianceWorkspace;
}

function workspaceTitle(workspace: ApplianceWorkspace): string {
  switch (workspace) {
    case "lxmf":
      return "LXMF";
    case "nomad":
      return "NomadNet";
    case "activity":
      return "Activity";
    case "map":
      return "Map";
    case "connectivity":
      return "Connectivity";
  }
}

interface WorkspaceTabProps {
  readonly compact: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly selected: boolean;
}

function WorkspaceTab({ compact, label, onPress, selected }: WorkspaceTabProps) {
  return (
    <Pressable
      accessibilityRole="tab"
      accessibilityState={{ selected }}
      onPress={onPress}
      style={[
        styles.workspaceTab,
        compact && styles.workspaceTabCompact,
        selected && styles.workspaceTabActive,
      ]}
    >
      <Text style={[styles.workspaceTabText, compact && styles.workspaceTabTextCompact]}>
        {label}
      </Text>
    </Pressable>
  );
}

export function ApplianceTopBar({
  busy,
  compact,
  connectivityAvailable,
  mobileSidebarVisible,
  onOpenContacts,
  onReconnect,
  onSelectWorkspace,
  onSync,
  ready,
  snapshot,
  workspace,
}: ApplianceTopBarProps) {
  return (
    <View style={[styles.topbar, compact && styles.topbarCompact]}>
      <View style={[styles.brandCluster, compact && styles.brandClusterCompact]}>
        {compact ? (
          workspace === "lxmf" ? (
            <Pressable
              accessibilityLabel="Open contacts"
              accessibilityRole="button"
              accessibilityState={{ expanded: mobileSidebarVisible }}
              onPress={onOpenContacts}
              style={({ pressed }) => [
                styles.mobileContactsButton,
                pressed && styles.buttonPressed,
              ]}
            >
              <Text style={styles.mobileContactsButtonText}>Contacts</Text>
            </Pressable>
          ) : (
            <Text style={styles.mobileBrand}>Reticulum</Text>
          )
        ) : (
          <View>
            <Text style={styles.eyebrow}>RETICULUM APPLIANCE</Text>
            <Text style={styles.title}>{workspaceTitle(workspace)}</Text>
          </View>
        )}
        <View
          accessibilityRole="tablist"
          style={[styles.workspaceSwitcher, compact && styles.workspaceSwitcherCompact]}
        >
          <ScrollView
            contentContainerStyle={styles.workspaceSwitcherContent}
            horizontal
            keyboardShouldPersistTaps="handled"
            showsHorizontalScrollIndicator={false}
          >
            <WorkspaceTab
              compact={compact}
              label={compact ? "Chat" : "Messages"}
              onPress={() => onSelectWorkspace("lxmf")}
              selected={workspace === "lxmf"}
            />
            <WorkspaceTab
              compact={compact}
              label="Browse"
              onPress={() => onSelectWorkspace("nomad")}
              selected={workspace === "nomad"}
            />
            <WorkspaceTab
              compact={compact}
              label="Activity"
              onPress={() => onSelectWorkspace("activity")}
              selected={workspace === "activity"}
            />
            <WorkspaceTab
              compact={compact}
              label="Map"
              onPress={() => onSelectWorkspace("map")}
              selected={workspace === "map"}
            />
            {connectivityAvailable ? (
              <WorkspaceTab
                compact={compact}
                label={compact ? "Net" : "Network"}
                onPress={() => onSelectWorkspace("connectivity")}
                selected={workspace === "connectivity"}
              />
            ) : null}
          </ScrollView>
        </View>
      </View>
      {compact ? null : (
        <View style={styles.statusCluster}>
          <View
            style={[
              styles.pill,
              snapshot?.connection.state === "ready" && styles.pillReady,
              snapshot?.connection.state === "faulted" && styles.pillFaulted,
            ]}
          >
            <Text style={styles.pillText}>
              {ready ? connectionStateLabel(snapshot?.connection) : "setup required"}
            </Text>
          </View>
          <ActionButton disabled={!ready || busy} label="Sync" onPress={onSync} secondary />
          <ActionButton
            disabled={!ready || busy}
            label="Reconnect"
            onPress={onReconnect}
            secondary
          />
        </View>
      )}
    </View>
  );
}
