import { InputAccessoryView, Keyboard, Pressable, StyleSheet, Text, View } from "react-native";

import type { KeyboardDoneAccessoryProps } from "./KeyboardDoneAccessory";

export function KeyboardDoneAccessory({ nativeID }: KeyboardDoneAccessoryProps) {
  return (
    <InputAccessoryView nativeID={nativeID}>
      <View style={styles.toolbar}>
        <Text style={styles.label}>Message</Text>
        <Pressable
          accessibilityLabel="Dismiss message keyboard"
          accessibilityRole="button"
          hitSlop={8}
          onPress={Keyboard.dismiss}
          style={({ pressed }) => [styles.doneButton, pressed && styles.doneButtonPressed]}
          testID="message-keyboard-done"
        >
          <Text style={styles.doneText}>Done</Text>
        </Pressable>
      </View>
    </InputAccessoryView>
  );
}

const styles = StyleSheet.create({
  toolbar: {
    minHeight: 44,
    paddingHorizontal: 14,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    borderTopColor: "#303b33",
    borderTopWidth: StyleSheet.hairlineWidth,
    backgroundColor: "#171d19",
  },
  label: { color: "#93a096", fontSize: 12, fontWeight: "600" },
  doneButton: {
    minHeight: 34,
    justifyContent: "center",
    paddingHorizontal: 12,
    borderRadius: 8,
  },
  doneButtonPressed: { backgroundColor: "#1d2520" },
  doneText: { color: "#91e6a7", fontSize: 15, fontWeight: "800" },
});
