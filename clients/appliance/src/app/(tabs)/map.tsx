import { TransmissionMapPanel } from "../../components/TransmissionMapPanel.tsx";
import { useAppliance } from "../../lib/appliance-context.tsx";

export default function MapScreen() {
  const {
    activityError,
    activityLoading,
    activityPage,
    compact,
    loadActivity,
    loadRadioTrace,
    mapFeatureEvidenceError,
    mapFeatureEvidenceLoading,
    radioTraceError,
    radioTraceLoading,
    radioTracePage,
    ready,
    selectMapFeature,
    transmissionMapScene,
  } = useAppliance();

  const mapDataError = [activityError, radioTraceError]
    .filter((message): message is string => message !== null)
    .join(" · ");

  return (
    <TransmissionMapPanel
      compact={compact}
      disabled={!ready}
      evidenceError={mapFeatureEvidenceError}
      evidenceLoading={mapFeatureEvidenceLoading}
      error={mapDataError.length === 0 ? null : mapDataError}
      hasOlder={
        (activityPage?.next_before_event_id !== null &&
          activityPage?.next_before_event_id !== undefined) ||
        (radioTracePage?.next_before_event_id !== null &&
          radioTracePage?.next_before_event_id !== undefined)
      }
      loading={activityLoading || radioTraceLoading}
      onLoadOlder={() => {
        void Promise.all([loadActivity(true), loadRadioTrace(true)]);
      }}
      onRefresh={() => {
        void Promise.all([loadActivity(false), loadRadioTrace(false)]);
      }}
      onSelectFeature={selectMapFeature}
      scene={transmissionMapScene}
    />
  );
}
