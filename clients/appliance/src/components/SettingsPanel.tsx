import { Pressable, ScrollView, StyleSheet, Switch, Text, View } from "react-native";

import { useAppliance } from "../lib/appliance-context.tsx";
import type { FieldTelemetryControllerState } from "../lib/field-telemetry.ts";
import type { MessageLocationPreferenceState } from "../lib/message-location-preference.ts";
import { styles as shared } from "./appliance-screen-styles.ts";

function fieldTelemetryLabel(state: FieldTelemetryControllerState): string {
  if (state.error !== null) return `Location error: ${state.error}`;
  if (!state.enabled && state.runState === "starting") {
    return "Loading saved location preference…";
  }
  if (!state.enabled) return "Off · future attempts record that telemetry was disabled";
  if (state.runState === "starting") return "Starting foreground location…";
  if (state.runState === "inactive") return "Paused while the app is not in the foreground";
  const observation = state.observation;
  if (observation === null) return "Waiting for local runtime state…";
  if (observation.state === "unavailable") {
    return `Location unavailable · ${observation.reason.replaceAll("_", " ")}`;
  }
  const accuracy =
    observation.horizontal_accuracy_mm === null
      ? "accuracy unknown"
      : `±${(observation.horizontal_accuracy_mm / 1_000).toFixed(1)} m`;
  return `${(observation.latitude_e6 / 1_000_000).toFixed(6)}, ${(observation.longitude_e6 / 1_000_000).toFixed(6)} · ${accuracy}`;
}

function messageLocationPreferenceLabel(state: MessageLocationPreferenceState): string {
  if (state.error !== null) return `Preference error: ${state.error}`;
  if (state.loading) return "Loading saved message-location preference…";
  if (state.saving) return "Saving default…";
  return state.attachByDefault
    ? "On · new composers start with location enabled"
    : "Off · new composers start without location";
}

export function SettingsPanel() {
  const {
    busy,
    enableMessageNotifications,
    fieldTelemetryState,
    messageNotificationPermission,
    messageLocationPreference,
    onToggleFieldTelemetry,
    setMessageLocationDefault,
  } = useAppliance();

  return (
    <ScrollView contentContainerStyle={shared.pageContent} style={shared.pageScroller}>
      <View style={shared.pageHeading}>
        <Text style={shared.eyebrow}>SETTINGS</Text>
        <Text style={shared.title}>Preferences</Text>
      </View>

      <View style={styles.card}>
        <Text style={styles.cardTitle}>Messages</Text>
        <View style={styles.row}>
          <View style={styles.copy}>
            <Text style={styles.title}>Attach location to new messages</Text>
            <Text style={styles.help}>
              Set the initial state of each new composer&apos;s location toggle. When enabled for a
              message, the app requests a fresh high-accuracy foreground phone fix while queueing
              and includes it in the LXMF message for its recipient. Each draft can override this
              default without changing the saved setting.
            </Text>
            <Text style={styles.state}>
              {messageLocationPreferenceLabel(messageLocationPreference)}
            </Text>
          </View>
          <Switch
            accessibilityLabel="Attach location to new messages by default"
            disabled={messageLocationPreference.loading || messageLocationPreference.saving}
            onValueChange={(enabled) => void setMessageLocationDefault(enabled)}
            trackColor={{ false: colors.line, true: "#496d8f" }}
            value={messageLocationPreference.attachByDefault}
          />
        </View>
      </View>

      <View style={styles.card}>
        <Text style={styles.cardTitle}>Location</Text>
        {fieldTelemetryState === null || onToggleFieldTelemetry === undefined ? (
          <Text style={styles.help}>Field telemetry is unavailable for this client.</Text>
        ) : (
          <View style={styles.row}>
            <View style={styles.copy}>
              <Text style={styles.title}>Field location telemetry</Text>
              <Text style={styles.help}>
                Record the phone&apos;s high-accuracy foreground position with every new send and
                retry. Coordinates stay in this profile&apos;s local activity database and are not
                added to the message or RMAP. This phone remembers the setting across app restarts
                and appliance switches until you turn it off.
              </Text>
              <Text style={styles.state}>{fieldTelemetryLabel(fieldTelemetryState)}</Text>
            </View>
            <Switch
              accessibilityLabel="Record private field telemetry"
              disabled={fieldTelemetryState.runState === "starting"}
              onValueChange={onToggleFieldTelemetry}
              trackColor={{ false: colors.line, true: "#39764a" }}
              value={fieldTelemetryState.enabled}
            />
          </View>
        )}
      </View>

      <View style={styles.card}>
        <Text style={styles.cardTitle}>Notifications</Text>
        <View style={styles.copy}>
          <Text style={styles.title}>Incoming LXMF message alerts</Text>
          <Text style={styles.help}>
            {messageNotificationPermission.state === "enabled"
              ? "Phone alerts are enabled for newly collected LXMF messages."
              : messageNotificationPermission.state === "checking"
                ? "Checking phone notification permission…"
                : "Phone alerts are disabled or unavailable. Messages still sync when the app is open."}
          </Text>
        </View>
        {messageNotificationPermission.state === "disabled" ? (
          <Pressable
            accessibilityRole="button"
            disabled={busy}
            onPress={() => void enableMessageNotifications()}
            style={({ pressed }) => [
              shared.button,
              busy && shared.buttonDisabled,
              pressed && !busy && shared.buttonPressed,
            ]}
          >
            <Text style={shared.buttonText}>
              {messageNotificationPermission.canAskAgain ? "Enable alerts" : "Open system settings"}
            </Text>
          </Pressable>
        ) : null}
      </View>
    </ScrollView>
  );
}

const colors = {
  background: "#101411",
  green: "#91e6a7",
  greenDark: "#173f24",
  line: "#303b33",
  muted: "#93a096",
  panel: "#171d19",
  panel2: "#1d2520",
  text: "#ecf2ea",
} as const;

const styles = StyleSheet.create({
  card: {
    padding: 14,
    gap: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  cardTitle: {
    color: colors.muted,
    fontSize: 10,
    fontWeight: "800",
    letterSpacing: 1.4,
    textTransform: "uppercase",
  },
  row: { flexDirection: "row", alignItems: "flex-start", gap: 12 },
  copy: { flex: 1, minWidth: 0, gap: 4 },
  title: { color: colors.text, fontSize: 14, fontWeight: "700" },
  help: { color: colors.muted, fontSize: 11, lineHeight: 16 },
  state: { color: colors.green, fontSize: 10, fontWeight: "700", lineHeight: 15 },
});
