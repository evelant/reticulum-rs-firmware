import { View } from "react-native";

import { ApplianceSidebar } from "../../components/ApplianceSidebar.tsx";
import { styles } from "../../components/appliance-screen-styles.ts";
import { ConversationPanel } from "../../components/ConversationPanel.tsx";
import { useAppliance } from "../../lib/appliance-context.tsx";

export default function MessagesScreen() {
  const appliance = useAppliance();
  const {
    applianceLabel,
    browseNomad,
    busy,
    chooseContact,
    clearDraft,
    compact,
    contacts,
    conversations,
    exportRadioTrace,
    foreground,
    loadMessageActivity,
    loadMessageRadioTrace,
    messageLocationPreference,
    messagePane,
    nearbyReader,
    onAbandonRetainedProbe,
    onMeasurePath,
    reticulumProbeAvailable,
    reticulumProbeState,
    retryMessage,
    selectMessagePane,
    selected,
    selectedConversation,
    send,
    snapshot,
    timeline,
    upsertContact,
  } = appliance;

  const showContacts = compact && messagePane === "contacts";
  const showConversation = !compact || messagePane === "chats";

  return (
    <View style={[styles.shell, compact && styles.shellCompact]}>
      {showContacts ? (
        <ApplianceSidebar
          applianceLabel={applianceLabel}
          busy={busy}
          compact={compact}
          contacts={contacts}
          conversations={conversations}
          foreground={foreground}
          inline={compact}
          onBrowseNomad={browseNomad}
          onClose={() => selectMessagePane("chats")}
          onRefreshNearby={nearbyReader}
          onSelect={chooseContact}
          onUpsert={upsertContact}
          selected={selected}
          snapshot={snapshot}
          visible={showContacts}
        />
      ) : null}
      {showConversation ? (
        <ConversationPanel
          busy={busy}
          canMeasurePath={reticulumProbeAvailable && snapshot?.connection.state === "ready"}
          compact={compact}
          key={selectedConversation?.destination ?? "empty"}
          messageLocationDefaultEnabled={messageLocationPreference.attachByDefault}
          messageLocationPreferenceLoaded={!messageLocationPreference.loading}
          onAbandonRetainedProbe={onAbandonRetainedProbe}
          onDraftChanged={clearDraft}
          onExportRadioTrace={exportRadioTrace}
          onLoadMessageActivity={loadMessageActivity}
          onLoadRadioTrace={loadMessageRadioTrace}
          onMeasurePath={onMeasurePath}
          onRetryMessage={retryMessage}
          onSend={send}
          peer={selectedConversation}
          probeState={reticulumProbeState}
          timeline={timeline}
        />
      ) : null}
    </View>
  );
}
