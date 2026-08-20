import { type ReactNode, useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import type {
  ContactView,
  ConversationPeerView,
  MessageActivityPageView,
  RadioTracePageView,
} from "../generated/api.ts";
import { ActivityPanel } from "./ActivityPanel.tsx";
import { APPLIANCE_KEYBOARD_LAYOUT } from "./appliance-screen-layout.ts";
import { styles } from "./appliance-screen-styles.ts";
import { type RadioTraceExportFormat, RadioTracePanel } from "./RadioTracePanel.tsx";

type ActivitySubtopic = "messages" | "trace";

const ACTIVITY_SUBTOPICS = [
  { label: "Messages", value: "messages" },
  { label: "RF trace", value: "trace" },
] as const;

export interface ActivityWorkspaceProps {
  readonly contacts: readonly ContactView[];
  readonly conversationPeers: readonly ConversationPeerView[];
  readonly disabled: boolean;
  readonly activityError: string | null;
  readonly activityLoading: boolean;
  readonly activityPage: MessageActivityPageView | null;
  readonly onLoadOlderActivity: () => void;
  readonly onRefreshActivity: () => void;
  readonly radioTraceAvailable: boolean;
  readonly radioTraceError: string | null;
  readonly radioTraceExportError: string | null;
  readonly radioTraceExporting: RadioTraceExportFormat | null;
  readonly radioTraceLoading: boolean;
  readonly radioTracePage: RadioTracePageView | null;
  readonly onExportRadioTrace: (format: RadioTraceExportFormat) => void;
  readonly onLoadOlderRadioTrace: () => void;
  readonly onRefreshRadioTrace: () => void;
}

/**
 * The Activity workspace keeps message activity and packet-correlated RF
 * tracing as sibling subtopics instead of stacking both lists in one scroll.
 * Loading, pagination, and query ownership stay with the route.
 */
export function ActivityWorkspace({
  contacts,
  conversationPeers,
  disabled,
  activityError,
  activityLoading,
  activityPage,
  onLoadOlderActivity,
  onRefreshActivity,
  radioTraceAvailable,
  radioTraceError,
  radioTraceExportError,
  radioTraceExporting,
  radioTraceLoading,
  radioTracePage,
  onExportRadioTrace,
  onLoadOlderRadioTrace,
  onRefreshRadioTrace,
}: ActivityWorkspaceProps) {
  const [subtopic, setSubtopic] = useState<ActivitySubtopic>("messages");

  const scroller = (panel: ReactNode) => (
    <ScrollView
      contentContainerStyle={styles.activityContent}
      key={subtopic}
      keyboardDismissMode={APPLIANCE_KEYBOARD_LAYOUT.dismissMode}
      keyboardShouldPersistTaps="handled"
      style={styles.activityScroller}
    >
      {panel}
    </ScrollView>
  );

  if (!radioTraceAvailable) {
    return scroller(
      <ActivityPanel
        contacts={contacts}
        conversationPeers={conversationPeers}
        disabled={disabled}
        error={activityError}
        loading={activityLoading}
        onLoadOlder={onLoadOlderActivity}
        onRefresh={onRefreshActivity}
        page={activityPage}
      />,
    );
  }

  return (
    <View style={localStyles.workspace}>
      <View style={localStyles.switcherWrap}>
        <View accessibilityRole="tablist" style={styles.workspaceSwitcher}>
          <View style={styles.workspaceSwitcherContent}>
            {ACTIVITY_SUBTOPICS.map((option) => {
              const selected = subtopic === option.value;
              return (
                <Pressable
                  accessibilityRole="tab"
                  accessibilityState={{ selected }}
                  key={option.value}
                  onPress={() => setSubtopic(option.value)}
                  style={[styles.workspaceTab, selected && styles.workspaceTabActive]}
                >
                  <Text style={styles.workspaceTabText}>{option.label}</Text>
                </Pressable>
              );
            })}
          </View>
        </View>
      </View>
      {scroller(
        subtopic === "messages" ? (
          <ActivityPanel
            contacts={contacts}
            conversationPeers={conversationPeers}
            disabled={disabled}
            error={activityError}
            loading={activityLoading}
            onLoadOlder={onLoadOlderActivity}
            onRefresh={onRefreshActivity}
            page={activityPage}
          />
        ) : (
          <RadioTracePanel
            disabled={disabled}
            error={radioTraceError}
            exportError={radioTraceExportError}
            exporting={radioTraceExporting}
            loading={radioTraceLoading}
            onExport={onExportRadioTrace}
            onLoadOlder={onLoadOlderRadioTrace}
            onRefresh={onRefreshRadioTrace}
            page={radioTracePage}
          />
        ),
      )}
    </View>
  );
}

const localStyles = StyleSheet.create({
  workspace: { flex: 1, minHeight: 0 },
  switcherWrap: {
    width: "100%",
    maxWidth: 900,
    alignSelf: "center",
    paddingHorizontal: 18,
    paddingTop: 14,
  },
});
