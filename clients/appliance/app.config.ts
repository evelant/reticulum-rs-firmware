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
    "expo-notifications",
    "expo-sharing",
    "expo-splash-screen",
    "@maplibre/maplibre-react-native",
    [
      "react-native-ble-manager",
      {
        bluetoothAlwaysPermission: "Connect to your Reticulum appliance over Bluetooth Low Energy.",
        isBleRequired: false,
        neverForLocation: true,
      },
    ],
    [
      "expo-location",
      {
        locationWhenInUsePermission:
          "Use your location when you choose to publish an approximate Reticulum map marker or privately record field-test message attempts.",
      },
    ],
  ],
  experiments: {
    typedRoutes: true,
  },
};

export default config;
