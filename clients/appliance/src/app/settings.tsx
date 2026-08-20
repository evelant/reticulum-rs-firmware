import { useRouter } from "expo-router";
import { Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { ActionButton } from "../components/AppliancePrimitives.tsx";
import { styles } from "../components/appliance-screen-styles.ts";
import { SettingsPanel } from "../components/SettingsPanel.tsx";

export default function SettingsScreen() {
  const router = useRouter();
  return (
    <SafeAreaView style={styles.overlayScreen}>
      <View style={styles.overlayHeader}>
        <View>
          <Text style={styles.eyebrow}>SETTINGS</Text>
          <Text style={styles.title}>Preferences</Text>
        </View>
        <ActionButton label="Close" onPress={() => router.back()} secondary />
      </View>
      <SettingsPanel />
    </SafeAreaView>
  );
}
