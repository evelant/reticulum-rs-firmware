import { useMemo } from "react";
import { ScrollView } from "react-native";

import { useAppliance } from "../lib/appliance-context.tsx";
import { buildNodeInterfaces } from "../lib/node-interfaces.ts";
import { ApplianceStatusCard } from "./ApplianceStatusCard.tsx";
import { styles } from "./appliance-screen-styles.ts";
import { BoardNameEditor } from "./BoardNameEditor.tsx";
import { NodeInterfacesPanel } from "./NodeInterfacesPanel.tsx";

export function AppliancesPanel() {
  const {
    activateApplianceProfile,
    beginAddAppliance,
    busy,
    canAddAppliance,
    canForgetProfile,
    canRepairBond,
    clearProfileOperation,
    deviceName,
    exactBleTargetRequired,
    forgetInactiveProfile,
    nativeCore,
    networkController,
    networkDeviceKey,
    networkState,
    openNetworkSubtopic,
    profileOperation,
    profiles,
    radioRoutesState,
    reconnectActiveProfile,
    repairActiveBleBond,
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

  const nameEditorReady =
    networkController !== null &&
    networkState !== null &&
    networkState.deviceKey === networkDeviceKey &&
    networkState.loadState === "ready";
  const mutating = networkState?.mutation.state === "running";

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
        deviceName={deviceName}
        exactBleTargetRequired={exactBleTargetRequired}
        nativeCore={nativeCore}
        onActivateProfile={activateApplianceProfile}
        onAddAppliance={beginAddAppliance}
        onClearProfileOperation={clearProfileOperation}
        onForgetProfile={canForgetProfile ? forgetInactiveProfile : null}
        onReconnect={reconnectActiveProfile}
        onRepairBleBond={canRepairBond ? repairActiveBleBond : null}
        onSync={sync}
        profileOperation={profileOperation}
        profiles={profiles}
        snapshot={snapshot}
      />
      <NodeInterfacesPanel interfaces={interfaces} onConfigure={openNetworkSubtopic} />
      {nameEditorReady && networkController !== null ? (
        <BoardNameEditor
          disabled={mutating}
          name={deviceName}
          onSave={(name) => networkController.mutate({ kind: "set_device_name", name })}
        />
      ) : null}
    </ScrollView>
  );
}
