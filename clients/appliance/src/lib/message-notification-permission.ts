export type MessageNotificationPermission =
  | { readonly state: "checking" }
  | { readonly state: "unsupported" }
  | {
      readonly canAskAgain: boolean;
      readonly reason: "application" | "android_channel";
      readonly state: "disabled";
    }
  | { readonly state: "enabled" }
  | { readonly message: string; readonly state: "error" };

export interface MessageNotificationPermissionInput {
  readonly androidChannelBlocked: boolean;
  readonly applicationCanAskAgain: boolean;
  readonly applicationGranted: boolean;
}

export function projectMessageNotificationPermission({
  androidChannelBlocked,
  applicationCanAskAgain,
  applicationGranted,
}: MessageNotificationPermissionInput): MessageNotificationPermission {
  if (!applicationGranted) {
    return {
      canAskAgain: applicationCanAskAgain,
      reason: "application",
      state: "disabled",
    };
  }
  if (androidChannelBlocked) {
    return {
      canAskAgain: false,
      reason: "android_channel",
      state: "disabled",
    };
  }
  return { state: "enabled" };
}
