import { ActivityIndicator, Text, View } from "react-native";
import { styles } from "../../components/appliance-screen-styles.ts";
import { applianceScreenColors as colors } from "../../components/appliance-screen-theme.ts";
import { ConnectivityPanel } from "../../components/ConnectivityPanel.tsx";
import { useAppliance } from "../../lib/appliance-context.tsx";

export default function NetworkScreen() {
  const {
    consumeNetworkSubtopicHint,
    manualServiceAnnounce,
    networkController,
    networkDeviceKey,
    networkState,
    networkSubtopicHint,
    onRefreshRadioRoutes,
    radioRoutesAvailable,
    radioRoutesState,
  } = useAppliance();

  if (networkController === null || networkState === null || networkDeviceKey === null) {
    return null;
  }

  if (networkState.deviceKey !== networkDeviceKey) {
    return (
      <View style={styles.connectivityLoading}>
        <ActivityIndicator color={colors.green} />
        <Text style={styles.secondaryText}>Loading this appliance&apos;s network settings…</Text>
      </View>
    );
  }

  return (
    <ConnectivityPanel
      announceNow={manualServiceAnnounce}
      controller={networkController}
      key={networkDeviceKey}
      onRefreshRadioRoutes={onRefreshRadioRoutes}
      onSubtopicHintConsumed={consumeNetworkSubtopicHint}
      radioRoutesAvailable={radioRoutesAvailable}
      radioRoutesState={radioRoutesState}
      state={networkState}
      subtopicHint={networkSubtopicHint}
    />
  );
}
