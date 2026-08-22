import { useMemo } from "react";
import { ScrollView } from "react-native";

import { useAppliance } from "../lib/appliance-context.tsx";
import { buildNodeInterfaces } from "../lib/node-interfaces.ts";
import { ApplianceLabelEditor } from "./ApplianceLabelEditor.tsx";
import { ApplianceStatusCard } from "./ApplianceStatusCard.tsx";
import { styles } from "./appliance-screen-styles.ts";
import { NodeInterfacesPanel } from "./NodeInterfacesPanel.tsx";

export function AppliancesPanel() {
  const {
    activateApplianceProfile,
    beginAddAppliance,
    busy,
    canAddAppliance,
    canForgetProfile,
    clearProfileOperation,
    applianceLabel,
    applianceLabelReady,
    forgetInactiveProfile,
    nativeCore,
    mutateApplianceLabel,
    networkDeviceKey,
    networkState,
    openNetworkSubtopic,
    profileOperation,
    profiles,
    radioRoutesState,
    reconnectActiveProfile,
    snapshot,
    sync,
  } = useAppliance();

  const interfaces = useMemo(
    () =>
      buildNodeInterfaces({
        config:
          networkState !== null && networkState.deviceKey === networkDeviceKey
            ? networkState.configuration
            : null,
        runtime:
          networkState !== null && networkState.deviceKey === networkDeviceKey
            ? networkState.runtime
            : null,
        radioRoutes: radioRoutesState?.snapshot ?? null,
      }),
    [networkDeviceKey, networkState, radioRoutesState?.snapshot],
  );

  const labelEditorReady = applianceLabelReady && mutateApplianceLabel !== null;

  return (
    <ScrollView
      contentContainerStyle={styles.pageContent}
      keyboardShouldPersistTaps="handled"
      style={styles.pageScroller}
    >
      <ApplianceStatusCard
        busy={busy}
        canAddAppliance={canAddAppliance}
        compact={false}
        applianceLabel={applianceLabel}
        nativeCore={nativeCore}
        onActivateProfile={activateApplianceProfile}
        onAddAppliance={beginAddAppliance}
        onClearProfileOperation={clearProfileOperation}
        onForgetProfile={canForgetProfile ? forgetInactiveProfile : null}
        onReconnect={reconnectActiveProfile}
        onSync={sync}
        profileOperation={profileOperation}
        profiles={profiles}
        snapshot={snapshot}
      />
      <NodeInterfacesPanel interfaces={interfaces} onConfigure={openNetworkSubtopic} />
      {labelEditorReady && mutateApplianceLabel !== null ? (
        <ApplianceLabelEditor name={applianceLabel} onSave={mutateApplianceLabel} />
      ) : null}
    </ScrollView>
  );
}
