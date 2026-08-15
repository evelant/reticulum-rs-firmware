import { describe, expect, test } from "bun:test";

import {
  BrowserMessageLocationPreferenceStore,
  MESSAGE_LOCATION_PREFERENCE_VERSION,
  MemoryMessageLocationPreferenceStore,
  parseMessageLocationPreference,
  serializeMessageLocationPreference,
} from "./message-location-preference.ts";

const disabled = {
  attachByDefault: false,
  version: MESSAGE_LOCATION_PREFERENCE_VERSION,
} as const;

describe("message location preference", () => {
  test("missing, malformed, incompatible, and invalid state fail closed", () => {
    expect(parseMessageLocationPreference(null)).toEqual(disabled);
    expect(parseMessageLocationPreference("not json")).toEqual(disabled);
    expect(parseMessageLocationPreference("null")).toEqual(disabled);
    expect(parseMessageLocationPreference("[]")).toEqual(disabled);
    expect(parseMessageLocationPreference('{"attachByDefault":true,"version":2}')).toEqual(
      disabled,
    );
    expect(parseMessageLocationPreference('{"attachByDefault":"yes","version":1}')).toEqual(
      disabled,
    );
  });

  test("round-trips only the durable opt-in and no coordinate data", () => {
    const raw = serializeMessageLocationPreference(true);
    expect(parseMessageLocationPreference(raw)).toEqual({
      attachByDefault: true,
      version: 1,
    });
    expect(JSON.parse(raw)).toEqual({ attachByDefault: true, version: 1 });
    for (const forbidden of ["latitude", "longitude", "accuracy", "altitude", "bearing"]) {
      expect(raw.toLowerCase()).not.toContain(forbidden);
    }
  });

  test("restores the setting across memory-backed process instances", async () => {
    const first = new MemoryMessageLocationPreferenceStore();
    await first.save(true);
    const restarted = new MemoryMessageLocationPreferenceStore(first.raw());
    expect(await restarted.load()).toBeTrue();
    await restarted.save(false);
    expect(await restarted.load()).toBeFalse();
  });

  test("uses the versioned browser localStorage key", async () => {
    const values = new Map<string, string>();
    const store = new BrowserMessageLocationPreferenceStore({
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    });
    expect(await store.load()).toBeFalse();
    await store.save(true);
    expect(await store.load()).toBeTrue();
    expect([...values.keys()]).toEqual(["reticulum.message-location.preference.v1"]);
  });
});
