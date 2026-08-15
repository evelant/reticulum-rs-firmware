import { File, Paths } from "expo-file-system";
import * as Notifications from "expo-notifications";
import { Platform } from "react-native";
import {
  type MessageNotificationPermission,
  projectMessageNotificationPermission,
} from "./message-notification-permission.ts";
import {
  type InboundMessageNotification,
  MESSAGE_NOTIFICATION_LEDGER_VERSION,
  type MessageNotificationLedger,
  type MessageNotificationLedgerStore,
  type MessageNotificationTarget,
  messageNotificationIdentifier,
  parseMessageNotificationLedger,
  parseMessageNotificationTarget,
} from "./message-notifications.ts";

export type { MessageNotificationPermission } from "./message-notification-permission.ts";

const ANDROID_CHANNEL_ID = "lxmf-messages";
const LEDGER_FILE_NAME = "reticulum-message-notifications-v1.json";
const LEDGER_TEMP_FILE_NAME = "reticulum-message-notifications-v1.tmp";

export interface PresentInboundMessageInput {
  readonly boardLabel: string;
  readonly notification: InboundMessageNotification;
  readonly peerLabel: string;
  readonly profileKey: string;
}

let platformInitialized = false;

function supportsLocalNotifications(): boolean {
  return Platform.OS === "ios" || Platform.OS === "android";
}

function permissionPresentation(
  permission: Notifications.NotificationPermissionsStatus,
  channel: Notifications.NotificationChannel | null,
): MessageNotificationPermission {
  return projectMessageNotificationPermission({
    androidChannelBlocked:
      Platform.OS === "android" && channel?.importance === Notifications.AndroidImportance.NONE,
    applicationCanAskAgain: permission.canAskAgain,
    applicationGranted: permission.granted,
  });
}

async function configureAndroidChannel(): Promise<Notifications.NotificationChannel | null> {
  if (Platform.OS !== "android") return null;
  return await Notifications.setNotificationChannelAsync(ANDROID_CHANNEL_ID, {
    description: "New LXMF messages collected from Reticulum appliances",
    enableVibrate: true,
    importance: Notifications.AndroidImportance.HIGH,
    name: "LXMF messages",
    showBadge: true,
    sound: "default",
    vibrationPattern: [0, 180, 120, 180],
  });
}

export async function initializeMessageNotifications(): Promise<MessageNotificationPermission> {
  if (!supportsLocalNotifications()) return { state: "unsupported" };
  try {
    if (!platformInitialized) {
      Notifications.setNotificationHandler({
        handleNotification: async () => ({
          priority: Notifications.AndroidNotificationPriority.HIGH,
          shouldPlaySound: true,
          shouldSetBadge: false,
          shouldShowBanner: true,
          shouldShowList: true,
        }),
      });
      platformInitialized = true;
    }
    const channel = await configureAndroidChannel();
    return permissionPresentation(await Notifications.getPermissionsAsync(), channel);
  } catch (error) {
    return {
      message: error instanceof Error ? error.message : String(error),
      state: "error",
    };
  }
}

export async function requestMessageNotificationPermission(): Promise<MessageNotificationPermission> {
  if (!supportsLocalNotifications()) return { state: "unsupported" };
  try {
    const channel = await configureAndroidChannel();
    const permission = await Notifications.requestPermissionsAsync({
      ios: {
        allowAlert: true,
        allowBadge: true,
        allowSound: true,
      },
    });
    return permissionPresentation(permission, channel);
  } catch (error) {
    return {
      message: error instanceof Error ? error.message : String(error),
      state: "error",
    };
  }
}

export async function presentInboundMessageNotification({
  boardLabel,
  notification,
  peerLabel,
  profileKey,
}: PresentInboundMessageInput): Promise<void> {
  if (!supportsLocalNotifications()) return;
  const permission = permissionPresentation(
    await Notifications.getPermissionsAsync(),
    await configureAndroidChannel(),
  );
  if (permission.state !== "enabled") {
    throw new Error(
      permission.state === "disabled" && permission.reason === "android_channel"
        ? "the Android LXMF notification channel is disabled"
        : "phone notification permission is disabled",
    );
  }
  await Notifications.scheduleNotificationAsync({
    identifier: messageNotificationIdentifier(profileKey, notification.messageId),
    content: {
      autoDismiss: true,
      body: `From ${peerLabel} · ${boardLabel}`,
      data: {
        destination: notification.peer,
        kind: "lxmf_message",
        messageId: notification.messageId,
        profileKey,
      },
      priority: Notifications.AndroidNotificationPriority.HIGH,
      sound: "default",
      title: "New Reticulum message",
    },
    trigger: Platform.OS === "android" ? { channelId: ANDROID_CHANNEL_ID } : null,
  });
}

function targetFromResponse(
  response: Notifications.NotificationResponse | null,
): MessageNotificationTarget | null {
  const data = response?.notification.request.content.data;
  return data === undefined ? null : parseMessageNotificationTarget(data);
}

export function consumeInitialMessageNotificationTarget(): MessageNotificationTarget | null {
  if (!supportsLocalNotifications()) return null;
  const target = targetFromResponse(Notifications.getLastNotificationResponse());
  if (target !== null) Notifications.clearLastNotificationResponse();
  return target;
}

export function subscribeMessageNotificationTargets(
  listener: (target: MessageNotificationTarget) => void,
): () => void {
  if (!supportsLocalNotifications()) return () => undefined;
  const subscription = Notifications.addNotificationResponseReceivedListener((response) => {
    const target = targetFromResponse(response);
    if (target !== null) listener(target);
  });
  return () => subscription.remove();
}

class FileMessageNotificationLedgerStore implements MessageNotificationLedgerStore {
  async load(): Promise<MessageNotificationLedger> {
    const file = new File(Paths.document, LEDGER_FILE_NAME);
    if (!file.exists) {
      return { profiles: {}, version: MESSAGE_NOTIFICATION_LEDGER_VERSION };
    }
    return parseMessageNotificationLedger(await file.text());
  }

  async save(ledger: MessageNotificationLedger): Promise<void> {
    const target = new File(Paths.document, LEDGER_FILE_NAME);
    const temporary = new File(Paths.document, LEDGER_TEMP_FILE_NAME);
    temporary.create({ overwrite: true });
    try {
      temporary.write(JSON.stringify(ledger));
      await temporary.move(target, { overwrite: true });
    } catch (error) {
      if (temporary.exists) temporary.delete();
      throw error;
    }
  }
}

class MemoryMessageNotificationLedgerStore implements MessageNotificationLedgerStore {
  #ledger: MessageNotificationLedger = {
    profiles: {},
    version: MESSAGE_NOTIFICATION_LEDGER_VERSION,
  };

  async load(): Promise<MessageNotificationLedger> {
    return this.#ledger;
  }

  async save(ledger: MessageNotificationLedger): Promise<void> {
    this.#ledger = ledger;
  }
}

export function createMessageNotificationLedgerStore(): MessageNotificationLedgerStore {
  return supportsLocalNotifications()
    ? new FileMessageNotificationLedgerStore()
    : new MemoryMessageNotificationLedgerStore();
}
