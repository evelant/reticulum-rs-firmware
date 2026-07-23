// Keep this a named import. Metro's dynamic namespace import enumerates
// react-native's deprecated lazy getters, including unavailable extracted
// modules such as PushNotificationIOS.
import { Platform } from "react-native";

export const nativePlatformOs = Platform.OS;
