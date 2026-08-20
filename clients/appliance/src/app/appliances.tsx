import { useRouter } from "expo-router";
import { Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { ActionButton } from "../components/AppliancePrimitives.tsx";
import { AppliancesPanel } from "../components/AppliancesPanel.tsx";
import { styles } from "../components/appliance-screen-styles.ts";

export default function AppliancesScreen() {
  const router = useRouter();
  return (
    <SafeAreaView style={styles.overlayScreen}>
      <View style={styles.overlayHeader}>
        <View>
          <Text style={styles.eyebrow}>APPLIANCES</Text>
          <Text style={styles.title}>Reticulum nodes</Text>
        </View>
        <ActionButton label="Close" onPress={() => router.back()} secondary />
      </View>
      <AppliancesPanel />
    </SafeAreaView>
  );
}
