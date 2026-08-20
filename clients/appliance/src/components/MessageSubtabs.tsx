import { Pressable, Text, View } from "react-native";

import { useAppliance } from "../lib/appliance-context.tsx";
import { styles } from "./appliance-screen-styles.ts";

const OPTIONS = [
  { label: "Chats", value: "chats" },
  { label: "Contacts", value: "contacts" },
] as const;

/** Secondary Chats/Contacts tabs rendered directly against the bottom nav. */
export function MessageSubtabs() {
  const { messagePane, selectMessagePane } = useAppliance();

  return (
    <View accessibilityRole="tablist" style={styles.messageSubtabsBar}>
      {OPTIONS.map((option) => {
        const selected = messagePane === option.value;
        return (
          <Pressable
            accessibilityRole="tab"
            accessibilityState={{ selected }}
            key={option.value}
            onPress={() => selectMessagePane(option.value)}
            style={styles.messageSubtab}
          >
            {selected ? <View style={styles.messageSubtabIndicator} /> : null}
            <Text style={[styles.messageSubtabLabel, selected && styles.messageSubtabLabelActive]}>
              {option.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
