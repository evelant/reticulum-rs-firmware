import type { ExpoConfig } from "expo/config";
import { type ConfigPlugin, withGradleProperties } from "expo/config-plugins";

const withPrnsAndroidArchitectures: ConfigPlugin = (config) =>
  withGradleProperties(config, (config) => {
    config.modResults = config.modResults.filter(
      (item) => item.type !== "property" || item.key !== "reactNativeArchitectures",
    );
    config.modResults.push(
      {
        type: "comment",
        value: "PRNS and the appliance native module support 64-bit Android targets.",
      },
      {
        type: "property",
        key: "reactNativeArchitectures",
        value: "arm64-v8a,x86_64",
      },
    );
    return config;
  });

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
      UIBackgroundModes: ["bluetooth-central", "bluetooth-peripheral"],
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

export default withPrnsAndroidArchitectures(config);
