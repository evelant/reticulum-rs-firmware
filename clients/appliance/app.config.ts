import type { ExpoConfig } from "expo/config";

const config: ExpoConfig = {
  name: "Reticulum Appliance",
  slug: "reticulum-appliance",
  version: "0.0.1",
  orientation: "default",
  scheme: "reticulum-appliance",
  userInterfaceStyle: "dark",
  ios: {
    bundleIdentifier: "org.reticulum.appliance",
    infoPlist: {
      NSBluetoothAlwaysUsageDescription:
        "Connect to your Reticulum appliance over Bluetooth Low Energy.",
      NSLocalNetworkUsageDescription:
        "Connect to your Reticulum appliance over its local Wi-Fi network.",
    },
    supportsTablet: true,
  },
  android: {
    package: "org.reticulum.appliance",
  },
  web: {
    bundler: "metro",
    name: "Reticulum LXMF",
    output: "single",
  },
  plugins: [
    "expo-router",
    [
      "react-native-ble-manager",
      {
        bluetoothAlwaysPermission: "Connect to your Reticulum appliance over Bluetooth Low Energy.",
        isBleRequired: false,
        neverForLocation: true,
      },
    ],
  ],
  experiments: {
    typedRoutes: true,
  },
};

export default config;
