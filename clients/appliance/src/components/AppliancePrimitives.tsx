import { Pressable, Text, View } from "react-native";

import { styles } from "./appliance-screen-styles.ts";

interface ActionButtonProps {
  readonly disabled?: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly secondary?: boolean;
}

export function ActionButton({
  disabled = false,
  label,
  onPress,
  secondary = false,
}: ActionButtonProps) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        secondary && styles.buttonSecondary,
        disabled && styles.buttonDisabled,
        pressed && !disabled && styles.buttonPressed,
      ]}
    >
      <Text style={[styles.buttonText, secondary && styles.buttonSecondaryText]}>{label}</Text>
    </Pressable>
  );
}

export function MetaRow({ label, value }: { readonly label: string; readonly value: string }) {
  return (
    <View style={styles.metaRow}>
      <Text style={styles.metaLabel}>{label}</Text>
      <Text selectable style={styles.metaValue}>
        {value}
      </Text>
    </View>
  );
}
