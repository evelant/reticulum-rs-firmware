import { Platform } from "react-native";

import { keyboardLayoutPolicy } from "../lib/keyboard-layout.ts";

/** Platform keyboard behavior shared by the appliance route and its panels. */
export const APPLIANCE_KEYBOARD_LAYOUT = keyboardLayoutPolicy(Platform.OS);
