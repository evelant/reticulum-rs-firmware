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
  plugins: ["expo-router"],
  experiments: {
    typedRoutes: true,
  },
};

export default config;
