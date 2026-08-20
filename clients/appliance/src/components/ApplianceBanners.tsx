import { ActivityIndicator, Text, View } from "react-native";
import { useAppliance } from "../lib/appliance-context.tsx";
import { foregroundReconnectMessage } from "../lib/foreground-reconnect.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { styles } from "./appliance-screen-styles.ts";

export function ApplianceBanners() {
  const {
    busy,
    displayedError,
    enableMessageNotifications,
    messageNotificationError,
    messageNotificationPermission,
    profileOperation,
    reconnectProgress,
  } = useAppliance();

  return (
    <>
      {displayedError === null || displayedError === undefined ? null : (
        <View accessibilityLiveRegion="assertive" style={styles.errorBanner}>
          <Text style={styles.errorText}>{displayedError}</Text>
        </View>
      )}
      {messageNotificationPermission.state === "disabled" ||
      messageNotificationPermission.state === "error" ? (
        <View accessibilityLiveRegion="polite" style={styles.notificationPermissionBanner}>
          <Text style={styles.notificationPermissionText}>
            {messageNotificationPermission.state === "error"
              ? `Phone notification setup failed: ${messageNotificationPermission.message}`
              : messageNotificationPermission.reason === "android_channel"
                ? "The Android LXMF notification channel is disabled in system settings."
                : messageNotificationPermission.canAskAgain
                  ? "Enable phone alerts for newly collected LXMF messages."
                  : "Phone alerts are disabled in system settings."}
          </Text>
          <ActionButton
            label={
              messageNotificationPermission.state === "disabled" &&
              !messageNotificationPermission.canAskAgain
                ? "Open settings"
                : "Enable"
            }
            onPress={() => void enableMessageNotifications()}
            secondary
          />
        </View>
      ) : null}
      {messageNotificationError === null ? null : (
        <View accessibilityLiveRegion="assertive" style={styles.errorBanner}>
          <Text style={styles.errorText}>{messageNotificationError}</Text>
        </View>
      )}
      {profileOperation.state === "idle" ? null : (
        <View
          accessibilityLiveRegion={profileOperation.state === "error" ? "assertive" : "polite"}
          style={[
            styles.profileOperationBanner,
            profileOperation.state === "error" && styles.errorBanner,
            profileOperation.state === "success" && styles.reconnectBanner,
          ]}
        >
          <Text
            style={[styles.reconnectText, profileOperation.state === "error" && styles.errorText]}
          >
            {profileOperation.message}
          </Text>
        </View>
      )}
      {reconnectProgress === null ? null : (
        <View accessibilityLiveRegion="polite" style={styles.reconnectBanner}>
          <Text style={styles.reconnectText}>{foregroundReconnectMessage(reconnectProgress)}</Text>
        </View>
      )}
      {busy ? <ActivityIndicator color="#91e6a7" style={styles.activity} /> : null}
    </>
  );
}
