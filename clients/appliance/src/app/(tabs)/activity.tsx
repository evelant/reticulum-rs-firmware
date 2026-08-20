import { ActivityWorkspace } from "../../components/ActivityWorkspace.tsx";
import { useAppliance } from "../../lib/appliance-context.tsx";

export default function ActivityScreen() {
  const {
    activityError,
    activityLoading,
    activityPage,
    contacts,
    conversations,
    exportCompleteRadioTrace,
    loadActivity,
    loadRadioTrace,
    radioTraceAvailable,
    radioTraceError,
    radioTraceExportError,
    radioTraceExporting,
    radioTraceLoading,
    radioTracePage,
    ready,
  } = useAppliance();

  return (
    <ActivityWorkspace
      contacts={contacts}
      conversationPeers={conversations}
      disabled={!ready}
      activityError={activityError}
      activityLoading={activityLoading}
      activityPage={activityPage}
      onLoadOlderActivity={() => void loadActivity(true)}
      onRefreshActivity={() => void loadActivity(false)}
      radioTraceAvailable={radioTraceAvailable}
      radioTraceError={radioTraceError}
      radioTraceExportError={radioTraceExportError}
      radioTraceExporting={radioTraceExporting}
      radioTraceLoading={radioTraceLoading}
      radioTracePage={radioTracePage}
      onExportRadioTrace={(format) => void exportCompleteRadioTrace(format)}
      onLoadOlderRadioTrace={() => void loadRadioTrace(true)}
      onRefreshRadioTrace={() => void loadRadioTrace(false)}
    />
  );
}
