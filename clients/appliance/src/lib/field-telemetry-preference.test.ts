import { describe, expect, test } from "bun:test";

import {
  FIELD_TELEMETRY_PREFERENCE_VERSION,
  MemoryFieldTelemetryPreferenceStore,
  parseFieldTelemetryPreference,
  serializeFieldTelemetryPreference,
} from "./field-telemetry-preference.ts";

const disabled = {
  enabled: false,
  version: FIELD_TELEMETRY_PREFERENCE_VERSION,
} as const;

describe("field telemetry preference", () => {
  test("missing, malformed, incompatible, and invalid state fail closed", () => {
    expect(parseFieldTelemetryPreference(null)).toEqual(disabled);
    expect(parseFieldTelemetryPreference("not json")).toEqual(disabled);
    expect(parseFieldTelemetryPreference("null")).toEqual(disabled);
    expect(parseFieldTelemetryPreference("[]")).toEqual(disabled);
    expect(parseFieldTelemetryPreference('{"enabled":true,"version":2}')).toEqual(disabled);
    expect(parseFieldTelemetryPreference('{"enabled":"yes","version":1}')).toEqual(disabled);
  });

  test("round-trips the versioned boolean without retaining location or identity", () => {
    const raw = serializeFieldTelemetryPreference(true);
    expect(parseFieldTelemetryPreference(raw)).toEqual({ enabled: true, version: 1 });
    expect(JSON.parse(raw)).toEqual({ enabled: true, version: 1 });
    for (const forbidden of [
      "latitude",
      "longitude",
      "observation",
      "profile",
      "device",
      "appliance",
    ]) {
      expect(raw.toLowerCase()).not.toContain(forbidden);
    }
  });

  test("shared backing state models a preference restored by a new controller", async () => {
    const first = new MemoryFieldTelemetryPreferenceStore();
    await first.save(true);
    const restarted = new MemoryFieldTelemetryPreferenceStore(first.raw());
    expect(await restarted.load()).toBeTrue();
    await restarted.save(false);
    expect(await restarted.load()).toBeFalse();
  });
});
