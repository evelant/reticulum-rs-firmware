import { File, Paths } from "expo-file-system";

import {
  type MessageLocationPreferenceStore,
  parseMessageLocationPreference,
  serializeMessageLocationPreference,
} from "./message-location-preference.ts";

const PREFERENCE_FILE_NAME = "reticulum-message-location-v1.json";
const PREFERENCE_TEMP_FILE_NAME = "reticulum-message-location-v1.tmp";

class FileMessageLocationPreferenceStore implements MessageLocationPreferenceStore {
  async load(): Promise<boolean> {
    const file = new File(Paths.document, PREFERENCE_FILE_NAME);
    if (!file.exists) return false;
    return parseMessageLocationPreference(await file.text()).attachByDefault;
  }

  async save(attachByDefault: boolean): Promise<void> {
    const target = new File(Paths.document, PREFERENCE_FILE_NAME);
    const temporary = new File(Paths.document, PREFERENCE_TEMP_FILE_NAME);
    temporary.create({ overwrite: true });
    try {
      temporary.write(serializeMessageLocationPreference(attachByDefault));
      await temporary.move(target, { overwrite: true });
    } catch (error) {
      if (temporary.exists) temporary.delete();
      throw error;
    }
  }
}

/** Create the phone-local, app-private atomic preference store. */
export function createMessageLocationPreferenceStore(): MessageLocationPreferenceStore {
  return new FileMessageLocationPreferenceStore();
}
