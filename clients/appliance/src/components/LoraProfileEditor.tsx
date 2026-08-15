import { useEffect, useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import type {
  LoraRadioProfileView,
  NetworkConfigMutationOutcome,
  RadioRoutesStatusView,
} from "../generated/api.ts";
import {
  applyLoraPreset,
  draftFromLoraProfile,
  formatLoraBandwidth,
  formatLoraRadioParameters,
  LORA_BANDWIDTH_OPTIONS_HZ,
  LORA_CODING_RATE_DENOMINATOR_OPTIONS,
  LORA_PROFILE_PRESETS,
  LORA_SPREADING_FACTOR_OPTIONS,
  LORA_TX_POWER_OPTIONS_DBM,
  type LoraRadioProfileDraft,
  loraProfilesEqual,
  matchingLoraPresetId,
  parseRmapReticulumConfig,
  validateLoraRadioProfileDraft,
} from "../lib/lora-radio-profile.ts";

type RunningLoraProfile = NonNullable<RadioRoutesStatusView["lora"]>;

interface LoraProfileEditorProps {
  readonly desiredProfile: LoraRadioProfileView;
  readonly disabled?: boolean;
  readonly onSave: (profile: LoraRadioProfileView) => Promise<NetworkConfigMutationOutcome | null>;
  readonly rebootRequired?: boolean;
  readonly runningProfile: RunningLoraProfile | null;
}

interface ChoiceProps {
  readonly disabled: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly selected: boolean;
}

function Choice({ disabled, label, onPress, selected }: ChoiceProps) {
  return (
    <Pressable
      accessibilityRole="radio"
      accessibilityState={{ disabled, selected }}
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.choice,
        selected && styles.choiceSelected,
        disabled && styles.disabled,
        pressed && !disabled && styles.pressed,
      ]}
    >
      <Text style={[styles.choiceText, selected && styles.choiceTextSelected]}>{label}</Text>
    </Pressable>
  );
}

function ActionButton({
  disabled,
  label,
  onPress,
  primary = false,
}: {
  readonly disabled: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly primary?: boolean;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        primary && styles.buttonPrimary,
        disabled && styles.disabled,
        pressed && !disabled && styles.pressed,
      ]}
    >
      <Text style={[styles.buttonText, primary && styles.buttonPrimaryText]}>{label}</Text>
    </Pressable>
  );
}

function runningMatchesDesired(
  running: RunningLoraProfile | null,
  desired: LoraRadioProfileView,
): boolean {
  return (
    running !== null &&
    running.frequency_hz === desired.frequency_hz &&
    running.bandwidth_hz === desired.bandwidth_hz &&
    running.spreading_factor === desired.spreading_factor &&
    running.coding_rate_denominator === desired.coding_rate_denominator &&
    running.applied_tx_power_dbm === desired.tx_power_dbm
  );
}

function runningProfileText(profile: RunningLoraProfile | null): string {
  if (profile === null) return "Live LoRa profile unavailable";
  return `${formatLoraRadioParameters(profile)} · ${profile.applied_tx_power_dbm >= 0 ? "+" : ""}${profile.applied_tx_power_dbm} dBm`;
}

function desiredProfileText(profile: LoraRadioProfileView): string {
  return `${formatLoraRadioParameters(profile)} · ${profile.tx_power_dbm >= 0 ? "+" : ""}${profile.tx_power_dbm} dBm`;
}

export function LoraProfileEditor({
  desiredProfile,
  disabled = false,
  onSave,
  rebootRequired = false,
  runningProfile,
}: LoraProfileEditorProps) {
  const [draft, setDraft] = useState<LoraRadioProfileDraft>(() =>
    draftFromLoraProfile(desiredProfile),
  );
  const [editBaseProfile, setEditBaseProfile] = useState(desiredProfile);
  const [editing, setEditing] = useState(false);
  const [importText, setImportText] = useState("");
  const [importVisible, setImportVisible] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const [localNotice, setLocalNotice] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (editing) return;
    setDraft(draftFromLoraProfile(desiredProfile));
    setEditBaseProfile(desiredProfile);
  }, [desiredProfile, editing]);

  const validatedDraft = validateLoraRadioProfileDraft(draft);
  const selectedPresetId = validatedDraft.ok ? matchingLoraPresetId(validatedDraft.profile) : null;
  const radioProfilePending =
    runningProfile === null
      ? rebootRequired
      : !runningMatchesDesired(runningProfile, desiredProfile);
  const savedProfileChangedWhileEditing =
    editing && !loraProfilesEqual(editBaseProfile, desiredProfile);
  const editorDisabled = disabled || saving;

  const beginEditing = () => {
    setDraft(draftFromLoraProfile(desiredProfile));
    setEditBaseProfile(desiredProfile);
    setImportText("");
    setImportVisible(false);
    setLocalError(null);
    setLocalNotice(null);
    setEditing(true);
  };

  const cancelEditing = () => {
    setDraft(draftFromLoraProfile(desiredProfile));
    setEditBaseProfile(desiredProfile);
    setImportText("");
    setImportVisible(false);
    setLocalError(null);
    setLocalNotice(null);
    setEditing(false);
  };

  const saveDraft = async () => {
    const validation = validateLoraRadioProfileDraft(draft);
    if (!validation.ok) {
      setLocalError(validation.error);
      return;
    }
    if (loraProfilesEqual(validation.profile, desiredProfile)) {
      setLocalError(null);
      setLocalNotice(null);
      setEditing(false);
      return;
    }

    setLocalError(null);
    setLocalNotice(null);
    setSaving(true);
    try {
      const outcome = await onSave(validation.profile);
      if (outcome?.outcome === "applied") {
        setImportText("");
        setImportVisible(false);
        setEditing(false);
      }
    } catch (error) {
      setLocalError(
        `Radio profile could not be saved: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setSaving(false);
    }
  };

  const previewRmapConfig = () => {
    const result = parseRmapReticulumConfig(importText, draft.txPowerDbm);
    if (!result.ok) {
      setLocalNotice(null);
      setLocalError(result.error);
      return;
    }
    setDraft(draftFromLoraProfile(result.profile));
    setImportVisible(false);
    setLocalError(null);
    setLocalNotice(`Imported “${result.sectionName}” into the unsaved draft. Review and save.`);
  };

  return (
    <View style={styles.panel}>
      <View style={styles.heading}>
        <View style={styles.headingCopy}>
          <Text style={styles.eyebrow}>LORA PROFILE</Text>
          <Text style={styles.title}>Radio compatibility</Text>
          <Text style={styles.help}>
            Frequency, bandwidth, spreading factor, and coding rate must match on every LoRa node
            that should communicate directly.
          </Text>
        </View>
        {editing ? null : (
          <ActionButton
            disabled={editorDisabled}
            label="Configure"
            onPress={beginEditing}
            primary
          />
        )}
      </View>

      <View style={styles.profileStatusRow}>
        <View style={styles.profileStatus}>
          <Text style={styles.statusLabel}>RUNNING</Text>
          <Text selectable style={styles.statusValue}>
            {runningProfileText(runningProfile)}
          </Text>
        </View>
        <View style={[styles.profileStatus, radioProfilePending && styles.profileStatusPending]}>
          <View style={styles.statusHeading}>
            <Text style={styles.statusLabel}>AFTER RESTART</Text>
            {radioProfilePending ? <Text style={styles.pendingBadge}>PENDING</Text> : null}
          </View>
          <Text selectable style={styles.statusValue}>
            {desiredProfileText(desiredProfile)}
          </Text>
        </View>
      </View>

      {radioProfilePending ? (
        <Text accessibilityLiveRegion="polite" style={styles.restartText}>
          {runningProfile === null
            ? "One or more saved changes await restart; live LoRa diagnostics are unavailable for comparison."
            : "The saved LoRa profile does not match the running radio. Restart the appliance to apply it."}
        </Text>
      ) : null}

      {editing ? (
        <View style={styles.editor}>
          <Text style={styles.editorTitle}>Profile for next restart</Text>

          <Text style={styles.label}>Start from a known profile</Text>
          <View style={styles.presetRow}>
            <ActionButton
              disabled={editorDisabled}
              label="Current saved"
              onPress={() => {
                setDraft(draftFromLoraProfile(desiredProfile));
                setEditBaseProfile(desiredProfile);
                setLocalError(null);
                setLocalNotice(null);
              }}
            />
            {LORA_PROFILE_PRESETS.map((preset) => (
              <Pressable
                accessibilityRole="button"
                accessibilityState={{
                  disabled: editorDisabled,
                  selected: selectedPresetId === preset.id,
                }}
                disabled={editorDisabled}
                key={preset.id}
                onPress={() => {
                  setDraft((current) => applyLoraPreset(current, preset));
                  setLocalError(null);
                  setLocalNotice(`${preset.label} loaded as an unsaved draft.`);
                }}
                style={({ pressed }) => [
                  styles.preset,
                  selectedPresetId === preset.id && styles.presetSelected,
                  editorDisabled && styles.disabled,
                  pressed && !editorDisabled && styles.pressed,
                ]}
              >
                <Text
                  style={[
                    styles.presetLabel,
                    selectedPresetId === preset.id && styles.presetLabelSelected,
                  ]}
                >
                  {preset.label}
                </Text>
                <Text
                  style={[
                    styles.presetDescription,
                    selectedPresetId === preset.id && styles.presetDescriptionSelected,
                  ]}
                >
                  {preset.description}
                </Text>
              </Pressable>
            ))}
          </View>
          <Text style={styles.help}>
            Presets are project convenience profiles, not Reticulum-wide standards. Power is kept
            separate and is never raised by selecting a preset.
          </Text>

          <Text style={styles.label}>Center frequency (MHz)</Text>
          <TextInput
            accessibilityLabel="LoRa center frequency in megahertz"
            autoCapitalize="none"
            autoCorrect={false}
            editable={!editorDisabled}
            keyboardType="decimal-pad"
            onChangeText={(frequencyMhz) => {
              setDraft({ ...draft, frequencyMhz });
              setLocalError(null);
              setLocalNotice(null);
            }}
            placeholder="915"
            placeholderTextColor={colors.muted}
            selectTextOnFocus
            style={styles.input}
            value={draft.frequencyMhz}
          />

          <Text style={styles.label}>Bandwidth</Text>
          <View accessibilityRole="radiogroup" style={styles.choiceGrid}>
            {LORA_BANDWIDTH_OPTIONS_HZ.map((bandwidthHz) => (
              <Choice
                disabled={editorDisabled}
                key={bandwidthHz}
                label={formatLoraBandwidth(bandwidthHz)}
                onPress={() => {
                  setDraft({ ...draft, bandwidthHz });
                  setLocalError(null);
                  setLocalNotice(null);
                }}
                selected={draft.bandwidthHz === bandwidthHz}
              />
            ))}
          </View>

          <Text style={styles.label}>Spreading factor</Text>
          <View accessibilityRole="radiogroup" style={styles.choiceGrid}>
            {LORA_SPREADING_FACTOR_OPTIONS.map((spreadingFactor) => (
              <Choice
                disabled={editorDisabled}
                key={spreadingFactor}
                label={`SF${spreadingFactor}`}
                onPress={() => {
                  setDraft({ ...draft, spreadingFactor });
                  setLocalError(null);
                  setLocalNotice(null);
                }}
                selected={draft.spreadingFactor === spreadingFactor}
              />
            ))}
          </View>

          <Text style={styles.label}>Coding rate</Text>
          <View accessibilityRole="radiogroup" style={styles.choiceGrid}>
            {LORA_CODING_RATE_DENOMINATOR_OPTIONS.map((codingRateDenominator) => (
              <Choice
                disabled={editorDisabled}
                key={codingRateDenominator}
                label={`4/${codingRateDenominator}`}
                onPress={() => {
                  setDraft({ ...draft, codingRateDenominator });
                  setLocalError(null);
                  setLocalNotice(null);
                }}
                selected={draft.codingRateDenominator === codingRateDenominator}
              />
            ))}
          </View>

          <Text style={styles.label}>Requested radio output</Text>
          <View accessibilityRole="radiogroup" style={styles.choiceGrid}>
            {LORA_TX_POWER_OPTIONS_DBM.map((txPowerDbm) => (
              <Choice
                disabled={editorDisabled}
                key={txPowerDbm}
                label={`+${txPowerDbm} dBm`}
                onPress={() => {
                  setDraft({ ...draft, txPowerDbm });
                  setLocalError(null);
                  setLocalNotice(null);
                }}
                selected={draft.txPowerDbm === txPowerDbm}
              />
            ))}
          </View>
          <Text style={styles.help}>
            This is requested chip output, not measured conducted power or antenna EIRP.
          </Text>

          <View style={styles.importHeading}>
            <View style={styles.headingCopy}>
              <Text style={styles.label}>RMAP.world Reticulum config</Text>
              <Text style={styles.help}>
                Paste one copied RNodeInterface block. Import only previews values in this draft.
              </Text>
            </View>
            <ActionButton
              disabled={editorDisabled}
              label={importVisible ? "Hide importer" : "Paste config"}
              onPress={() => {
                setImportVisible((visible) => !visible);
                setLocalError(null);
              }}
            />
          </View>
          {importVisible ? (
            <View style={styles.importer}>
              <TextInput
                accessibilityLabel="RMAP Reticulum RNodeInterface configuration"
                autoCapitalize="none"
                autoCorrect={false}
                editable={!editorDisabled}
                multiline
                onChangeText={(value) => {
                  setImportText(value);
                  setLocalError(null);
                  setLocalNotice(null);
                }}
                placeholder={
                  "[[My LoRa Interface]]\ntype = RNodeInterface\nfrequency = 915000000\nbandwidth = 125000\nspreadingfactor = 8\ncodingrate = 5"
                }
                placeholderTextColor={colors.muted}
                style={[styles.input, styles.importInput]}
                textAlignVertical="top"
                value={importText}
              />
              <ActionButton
                disabled={editorDisabled || importText.trim().length === 0}
                label="Preview imported values"
                onPress={previewRmapConfig}
              />
            </View>
          ) : null}

          {localError === null ? null : (
            <Text accessibilityLiveRegion="assertive" style={styles.error}>
              {localError}
            </Text>
          )}
          {localNotice === null ? null : (
            <Text accessibilityLiveRegion="polite" style={styles.notice}>
              {localNotice}
            </Text>
          )}

          {savedProfileChangedWhileEditing ? (
            <Text accessibilityLiveRegion="polite" style={styles.staleText}>
              The board&apos;s saved LoRa profile changed while this editor was open. Review the
              current saved profile above, or choose Current saved to reload it before saving.
            </Text>
          ) : null}

          <Text style={styles.regulatoryText}>
            The app checks this board&apos;s fitted radio range, not local spectrum authorization.
            Confirm frequency, bandwidth, duty cycle, and EIRP are legal where the appliance will
            transmit.
          </Text>

          <View style={styles.actionRow}>
            <ActionButton
              disabled={editorDisabled}
              label={saving ? "Saving…" : "Save for next restart"}
              onPress={() => void saveDraft()}
              primary
            />
            <ActionButton disabled={editorDisabled} label="Cancel" onPress={cancelEditing} />
          </View>
        </View>
      ) : null}
    </View>
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
  red: "#ff9b91",
  text: "#ecf2ea",
} as const;

const styles = StyleSheet.create({
  panel: {
    padding: 12,
    gap: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  heading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 10,
  },
  headingCopy: { flex: 1, minWidth: 0, gap: 3 },
  eyebrow: { color: colors.green, fontSize: 9, fontWeight: "800", letterSpacing: 1.2 },
  title: { color: colors.text, fontSize: 16, fontWeight: "800" },
  help: { color: colors.muted, fontSize: 10, lineHeight: 15 },
  profileStatusRow: { flexDirection: "row", flexWrap: "wrap", gap: 7 },
  profileStatus: {
    flexGrow: 1,
    flexBasis: 260,
    minWidth: 0,
    padding: 9,
    gap: 4,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.background,
  },
  profileStatusPending: { borderColor: "#9a7937", backgroundColor: "#332a18" },
  statusHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 6,
  },
  statusLabel: { color: colors.muted, fontSize: 8, fontWeight: "800", letterSpacing: 1 },
  statusValue: { color: colors.text, fontSize: 10, lineHeight: 15 },
  pendingBadge: { color: "#f2d58c", fontSize: 8, fontWeight: "800" },
  restartText: { color: "#f2d58c", fontSize: 10, lineHeight: 15 },
  editor: {
    padding: 11,
    gap: 8,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  editorTitle: { color: colors.text, fontSize: 14, fontWeight: "800" },
  label: { color: colors.text, fontSize: 11, fontWeight: "700" },
  presetRow: { flexDirection: "row", flexWrap: "wrap", alignItems: "stretch", gap: 7 },
  preset: {
    minWidth: 150,
    flexGrow: 1,
    flexBasis: 180,
    justifyContent: "center",
    paddingHorizontal: 10,
    paddingVertical: 7,
    gap: 2,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  presetSelected: { borderColor: "#5b9c69", backgroundColor: colors.greenDark },
  presetLabel: { color: colors.text, fontSize: 11, fontWeight: "800" },
  presetLabelSelected: { color: colors.green },
  presetDescription: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  presetDescriptionSelected: { color: "#b9d9c0" },
  input: {
    minHeight: 40,
    paddingHorizontal: 10,
    paddingVertical: 7,
    color: colors.text,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.background,
  },
  choiceGrid: { flexDirection: "row", flexWrap: "wrap", gap: 6 },
  choice: {
    minHeight: 32,
    minWidth: 72,
    flexGrow: 1,
    flexBasis: 76,
    justifyContent: "center",
    paddingHorizontal: 7,
    paddingVertical: 5,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  choiceSelected: { borderColor: "#5b9c69", backgroundColor: colors.green },
  choiceText: { color: colors.text, fontSize: 10, fontWeight: "700", textAlign: "center" },
  choiceTextSelected: { color: "#0d1b11" },
  importHeading: {
    marginTop: 3,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
  },
  importer: { gap: 7 },
  importInput: { minHeight: 150, fontFamily: "monospace" },
  error: { color: colors.red, fontSize: 10, lineHeight: 15 },
  notice: { color: colors.green, fontSize: 10, lineHeight: 15 },
  staleText: { color: "#f2d58c", fontSize: 10, lineHeight: 15 },
  regulatoryText: { color: "#cabd98", fontSize: 9, lineHeight: 14 },
  actionRow: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 7 },
  button: {
    minHeight: 34,
    justifyContent: "center",
    paddingHorizontal: 10,
    paddingVertical: 6,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  buttonPrimary: { borderColor: "#5b9c69", backgroundColor: colors.green },
  buttonText: { color: colors.text, fontSize: 10, fontWeight: "700", textAlign: "center" },
  buttonPrimaryText: { color: "#0d1b11" },
  disabled: { opacity: 0.4 },
  pressed: { opacity: 0.76 },
});
