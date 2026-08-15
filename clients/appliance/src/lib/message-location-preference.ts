export const MESSAGE_LOCATION_PREFERENCE_VERSION = 1;

export interface MessageLocationPreference {
  readonly attachByDefault: boolean;
  readonly version: typeof MESSAGE_LOCATION_PREFERENCE_VERSION;
}

export interface MessageLocationPreferenceStore {
  load(): Promise<boolean>;
  save(attachByDefault: boolean): Promise<void>;
}

export interface MessageLocationPreferenceState {
  readonly attachByDefault: boolean;
  readonly error: string | null;
  readonly loading: boolean;
  readonly saving: boolean;
}

interface StringStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const STORAGE_KEY = "reticulum.message-location.preference.v1";
const DEFAULT_PREFERENCE: MessageLocationPreference = {
  attachByDefault: false,
  version: MESSAGE_LOCATION_PREFERENCE_VERSION,
};

/** Missing, corrupt, or future state fails closed to not sharing location. */
export function parseMessageLocationPreference(raw: string | null): MessageLocationPreference {
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
    candidate.version !== MESSAGE_LOCATION_PREFERENCE_VERSION ||
    !("attachByDefault" in candidate) ||
    typeof candidate.attachByDefault !== "boolean"
  ) {
    return DEFAULT_PREFERENCE;
  }
  return {
    attachByDefault: candidate.attachByDefault,
    version: MESSAGE_LOCATION_PREFERENCE_VERSION,
  };
}

/** Persist only the opt-in setting; message coordinates never enter this file. */
export function serializeMessageLocationPreference(attachByDefault: boolean): string {
  return JSON.stringify({
    attachByDefault,
    version: MESSAGE_LOCATION_PREFERENCE_VERSION,
  });
}

/** Host-testable store that can model a process restart over shared state. */
export class MemoryMessageLocationPreferenceStore implements MessageLocationPreferenceStore {
  #raw: string | null;

  constructor(raw: string | null = null) {
    this.#raw = raw;
  }

  async load(): Promise<boolean> {
    return parseMessageLocationPreference(this.#raw).attachByDefault;
  }

  async save(attachByDefault: boolean): Promise<void> {
    this.#raw = serializeMessageLocationPreference(attachByDefault);
  }

  raw(): string | null {
    return this.#raw;
  }
}

export class BrowserMessageLocationPreferenceStore implements MessageLocationPreferenceStore {
  readonly #storage: StringStorage;

  constructor(storage: StringStorage = globalThis.localStorage) {
    this.#storage = storage;
  }

  async load(): Promise<boolean> {
    return parseMessageLocationPreference(this.#storage.getItem(STORAGE_KEY)).attachByDefault;
  }

  async save(attachByDefault: boolean): Promise<void> {
    this.#storage.setItem(STORAGE_KEY, serializeMessageLocationPreference(attachByDefault));
  }
}

/** Create persistent browser storage, with an in-memory fallback for non-DOM hosts. */
export function createMessageLocationPreferenceStore(): MessageLocationPreferenceStore {
  return typeof globalThis.localStorage === "undefined"
    ? new MemoryMessageLocationPreferenceStore()
    : new BrowserMessageLocationPreferenceStore();
}
