import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";

import { ApplianceProvider } from "../lib/appliance-context.tsx";

export default function RootLayout() {
  return (
    <ApplianceProvider>
      <StatusBar style="light" />
      <Stack screenOptions={{ headerShown: false }}>
        <Stack.Screen name="(tabs)" />
        <Stack.Screen name="appliances" options={{ presentation: "modal" }} />
        <Stack.Screen name="settings" options={{ presentation: "modal" }} />
      </Stack>
    </ApplianceProvider>
  );
}
