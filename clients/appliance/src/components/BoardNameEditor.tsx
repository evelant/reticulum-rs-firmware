import { useEffect, useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { MAX_DEVICE_NAME_BYTES, type NetworkConfigMutationOutcome } from "../generated/api.ts";
import { utf8ByteLength } from "../lib/limits.ts";

interface BoardNameEditorProps {
  readonly disabled?: boolean;
  readonly name: string | null;
  readonly onSave: (name: string | null) => Promise<NetworkConfigMutationOutcome | null>;
}

function boardNameError(name: string): string | null {
  if (name.length === 0) return "Board name must contain at least one character";
  const bytes = utf8ByteLength(name);
  if (bytes > MAX_DEVICE_NAME_BYTES) {
    return `Board name is ${bytes} bytes; the maximum is ${MAX_DEVICE_NAME_BYTES}`;
  }
  for (const character of name) {
    const code = character.charCodeAt(0);
    if (code < 0x20 || character === "\u2028" || character === "\u2029") {
      return "Board name cannot contain control or line-separator characters";
    }
  }
  return null;
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

export function BoardNameEditor({ disabled = false, name, onSave }: BoardNameEditorProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!editing) setDraft(name ?? "");
  }, [editing, name]);

  const beginEditing = () => {
    setDraft(name ?? "");
    setLocalError(null);
    setEditing(true);
  };

  const cancel = () => {
    setLocalError(null);
    setEditing(false);
  };

  const save = async () => {
    const error = boardNameError(draft);
    if (error !== null) {
      setLocalError(error);
      return;
    }
    if (draft === (name ?? "")) {
      setLocalError(null);
      setEditing(false);
      return;
    }
    setLocalError(null);
    setSaving(true);
    try {
      const outcome = await onSave(draft);
      if (outcome?.outcome === "applied") setEditing(false);
    } catch (error) {
      setLocalError(
        `Board name could not be saved: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    setLocalError(null);
    setSaving(true);
    try {
      const outcome = await onSave(null);
      if (outcome?.outcome === "applied") {
        setDraft("");
        setEditing(false);
      }
    } catch (error) {
      setLocalError(
        `Board name could not be cleared: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setSaving(false);
    }
  };

  return (
    <View style={styles.panel}>
      <View style={styles.heading}>
        <View style={styles.headingCopy}>
          <Text style={styles.eyebrow}>BOARD NAME</Text>
          <Text style={styles.title}>Device nickname</Text>
          <Text style={styles.help}>
            Shown on the appliance screen and announced to nearby contacts. When no name is set, the
            board announces its built-in MAC-derived name.
          </Text>
        </View>
        {editing ? null : (
          <ActionButton
            disabled={disabled || saving}
            label="Set name"
            onPress={beginEditing}
            primary
          />
        )}
      </View>

      {editing ? null : (
        <View style={styles.currentRow}>
          <View style={styles.currentCopy}>
            <Text style={styles.statusLabel}>CURRENT NAME</Text>
            <Text selectable style={styles.statusValue}>
              {name ?? "Unnamed (board suffix)"}
            </Text>
          </View>
          {name === null ? null : (
            <ActionButton
              disabled={disabled || saving}
              label="Clear"
              onPress={() => void clear()}
            />
          )}
        </View>
      )}

      {editing ? (
        <View style={styles.editor}>
          <Text style={styles.label}>Name for next restart</Text>
          <TextInput
            accessibilityLabel="Board display name"
            autoCapitalize="none"
            autoCorrect={false}
            editable={!disabled && !saving}
            maxLength={MAX_DEVICE_NAME_BYTES * 4}
            onChangeText={(value) => {
              setDraft(value);
              setLocalError(null);
            }}
            placeholder="e.g. Field node"
            placeholderTextColor={colors.muted}
            selectTextOnFocus
            style={styles.input}
            value={draft}
          />
          <Text style={styles.help}>
            Up to {MAX_DEVICE_NAME_BYTES} UTF-8 bytes on one line. Changes apply after an appliance
            restart.
          </Text>
          {localError === null ? null : (
            <Text accessibilityLiveRegion="assertive" style={styles.error}>
              {localError}
            </Text>
          )}
          <View style={styles.actionRow}>
            <ActionButton
              disabled={disabled || saving}
              label={saving ? "Saving…" : "Save"}
              onPress={() => void save()}
              primary
            />
            <ActionButton disabled={disabled || saving} label="Cancel" onPress={cancel} />
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
  currentRow: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
  },
  currentCopy: { flex: 1, minWidth: 0, gap: 3 },
  statusLabel: { color: colors.muted, fontSize: 8, fontWeight: "800", letterSpacing: 1 },
  statusValue: { color: colors.text, fontSize: 12, lineHeight: 17 },
  editor: {
    padding: 11,
    gap: 8,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  label: { color: colors.text, fontSize: 11, fontWeight: "700" },
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
  error: { color: colors.red, fontSize: 10, lineHeight: 15 },
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
