import { describe, expect, test } from "bun:test";

import {
  messageLocationFromPhoneSample,
  messageLocationMapUrl,
  messageLocationPresentation,
} from "./message-location.ts";

const sample = {
  coords: {
    latitude: 42.357_111_4,
    longitude: -71.061_923_7,
    accuracy: 8.255,
    altitude: 17.234,
    speed: 3.456,
    heading: 359.999,
  },
  timestamp: 1_785_084_000_999,
};

describe("Sideband message location", () => {
  test("maps a fresh phone fix into exact bounded Sideband units", () => {
    expect(messageLocationFromPhoneSample(sample)).toEqual({
      latitude_e6: 42_357_111,
      longitude_e6: -71_061_924,
      altitude_cm: 1_723,
      speed_cm_per_second: 346,
      bearing_centidegrees: 0,
      accuracy_cm: 826,
      updated_at_unix_seconds: 1_785_084_000,
    });
  });

  test("uses zero for unavailable motion fields and clamps accuracy to u16", () => {
    expect(
      messageLocationFromPhoneSample({
        coords: {
          latitude: 0,
          longitude: 0,
          accuracy: 9_999,
          altitude: null,
          speed: -1,
          heading: -1,
        },
        timestamp: 0,
      }),
    ).toEqual({
      latitude_e6: 0,
      longitude_e6: 0,
      altitude_cm: 0,
      speed_cm_per_second: 0,
      bearing_centidegrees: 0,
      accuracy_cm: 65_535,
      updated_at_unix_seconds: 0,
    });
  });

  test("rejects an unrepresentable location timestamp", () => {
    expect(() =>
      messageLocationFromPhoneSample({
        ...sample,
        timestamp: (0xffff_ffff + 1) * 1_000,
      }),
    ).toThrow("unsigned Unix seconds");
  });

  test("builds a cross-platform map URL and honest location labels", () => {
    const location = messageLocationFromPhoneSample({
      ...sample,
      coords: { ...sample.coords, accuracy: null, altitude: null, speed: null, heading: null },
    });
    expect(messageLocationMapUrl(location)).toBe(
      "https://www.openstreetmap.org/?mlat=42.357111&mlon=-71.061924#map=16/42.357111/-71.061924",
    );
    expect(messageLocationPresentation(location)).toMatchObject({
      accuracy: "Unavailable",
      altitude: "0.00 m (zero can mean unavailable)",
      bearing: "0.00° (zero can mean unavailable or due north)",
      coordinates: "42.357111, -71.061924",
      speed: "0.00 m/s (zero can mean unavailable or stationary)",
      summary: "Attached location · 42.357111, -71.061924 · accuracy unavailable",
    });
  });
});
