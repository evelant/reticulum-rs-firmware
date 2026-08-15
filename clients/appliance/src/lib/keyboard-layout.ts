export interface KeyboardLayoutPolicy {
  readonly avoidingBehavior: "height" | "padding";
  readonly avoidingEnabled: boolean;
  readonly dismissMode: "interactive" | "on-drag";
  readonly inputAccessoryEnabled: boolean;
}

export function keyboardLayoutPolicy(platform: string): KeyboardLayoutPolicy {
  return {
    avoidingBehavior: platform === "ios" ? "padding" : "height",
    avoidingEnabled: platform === "ios" || platform === "android",
    dismissMode: platform === "ios" ? "interactive" : "on-drag",
    inputAccessoryEnabled: platform === "ios",
  };
}
