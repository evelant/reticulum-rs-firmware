import { File, Paths } from "expo-file-system";

import {
  type FieldTelemetryPreferenceStore,
  parseFieldTelemetryPreference,
  serializeFieldTelemetryPreference,
} from "./field-telemetry-preference.ts";

const PREFERENCE_FILE_NAME = "reticulum-field-telemetry-v1.json";
const PREFERENCE_TEMP_FILE_NAME = "reticulum-field-telemetry-v1.tmp";

class FileFieldTelemetryPreferenceStore implements FieldTelemetryPreferenceStore {
  async load(): Promise<boolean> {
    const file = new File(Paths.document, PREFERENCE_FILE_NAME);
    if (!file.exists) return false;
    return parseFieldTelemetryPreference(await file.text()).enabled;
  }

  async save(enabled: boolean): Promise<void> {
    const target = new File(Paths.document, PREFERENCE_FILE_NAME);
    const temporary = new File(Paths.document, PREFERENCE_TEMP_FILE_NAME);
    temporary.create({ overwrite: true });
    try {
      temporary.write(serializeFieldTelemetryPreference(enabled));
      await temporary.move(target, { overwrite: true });
    } catch (error) {
      if (temporary.exists) temporary.delete();
      throw error;
    }
  }
}

/** Create the phone-local, app-private durable preference store. */
export function createFieldTelemetryPreferenceStore(): FieldTelemetryPreferenceStore {
  return new FileFieldTelemetryPreferenceStore();
}
