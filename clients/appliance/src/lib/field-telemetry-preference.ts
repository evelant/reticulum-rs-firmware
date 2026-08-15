export const FIELD_TELEMETRY_PREFERENCE_VERSION = 1;

export interface FieldTelemetryPreference {
  readonly enabled: boolean;
  readonly version: typeof FIELD_TELEMETRY_PREFERENCE_VERSION;
}

export interface FieldTelemetryPreferenceStore {
  load(): Promise<boolean>;
  save(enabled: boolean): Promise<void>;
}

const STORAGE_KEY = "reticulum.field-telemetry.preference.v1";
const DEFAULT_PREFERENCE: FieldTelemetryPreference = {
  enabled: false,
  version: FIELD_TELEMETRY_PREFERENCE_VERSION,
};

/**
 * Decode app-owned preference state. Missing, corrupt, or future state fails
 * closed to disabled without affecting message or trace data.
 */
export function parseFieldTelemetryPreference(raw: string | null): FieldTelemetryPreference {
  if (raw === null) return DEFAULT_PREFERENCE;
  let candidate: unknown;
  try {
    candidate = JSON.parse(raw);
  } catch {
    return DEFAULT_PREFERENCE;
  }
  if (
    typeof candidate !== "object" ||
    candidate === null ||
    Array.isArray(candidate) ||
    !("version" in candidate) ||
    candidate.version !== FIELD_TELEMETRY_PREFERENCE_VERSION ||
    !("enabled" in candidate) ||
    typeof candidate.enabled !== "boolean"
  ) {
    return DEFAULT_PREFERENCE;
  }
  return {
    enabled: candidate.enabled,
    version: FIELD_TELEMETRY_PREFERENCE_VERSION,
  };
}

/** Serialize only the durable opt-in; coordinates and observations never enter this file. */
export function serializeFieldTelemetryPreference(enabled: boolean): string {
  return JSON.stringify({ enabled, version: FIELD_TELEMETRY_PREFERENCE_VERSION });
}

/** Host-testable store that can also model two app processes over shared state. */
export class MemoryFieldTelemetryPreferenceStore implements FieldTelemetryPreferenceStore {
  #raw: string | null;

  constructor(raw: string | null = null) {
    this.#raw = raw;
  }

  async load(): Promise<boolean> {
    return parseFieldTelemetryPreference(this.#raw).enabled;
  }

  async save(enabled: boolean): Promise<void> {
    this.#raw = serializeFieldTelemetryPreference(enabled);
  }

  raw(): string | null {
    return this.#raw;
  }
}

class BrowserFieldTelemetryPreferenceStore implements FieldTelemetryPreferenceStore {
  async load(): Promise<boolean> {
    return parseFieldTelemetryPreference(globalThis.localStorage.getItem(STORAGE_KEY)).enabled;
  }

  async save(enabled: boolean): Promise<void> {
    globalThis.localStorage.setItem(STORAGE_KEY, serializeFieldTelemetryPreference(enabled));
  }
}

/** Create persistent browser storage, with an in-memory fallback for non-DOM hosts. */
export function createFieldTelemetryPreferenceStore(): FieldTelemetryPreferenceStore {
  return typeof globalThis.localStorage === "undefined"
    ? new MemoryFieldTelemetryPreferenceStore()
    : new BrowserFieldTelemetryPreferenceStore();
}
