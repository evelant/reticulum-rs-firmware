import { describe, expect, test } from "bun:test";

import {
  attemptPhoneLocationObservation,
  phoneLocationAuthorization,
  sharedPhoneLocation,
} from "./phone-location.ts";

const I32_ALTITUDE_OVERFLOW_METRES = 2_147_483.648;

const sample = {
  coords: {
    latitude: 42.357_111_4,
    longitude: -71.061_923_7,
    accuracy: 8.25,
    altitude: 17.234_4,
    altitudeAccuracy: 3.125_4,
  },
  timestamp: 1_785_084_000_123.4,
};

describe("phone location sharing boundary", () => {
  test("uses privacy-conscious 100 metre rounding by default", () => {
    expect(sharedPhoneLocation(sample)).toEqual({
      latitude_e6: 42_357_000,
      longitude_e6: -71_062_000,
      horizontal_accuracy_m: 8.25,
      captured_at_unix_ms: 1_785_084_000_123,
      precision: "approximately_100m",
    });
  });

  test("supports device E6 precision and approximately one kilometre rounding", () => {
    expect(sharedPhoneLocation(sample, "device")).toMatchObject({
      latitude_e6: 42_357_111,
      longitude_e6: -71_061_924,
      precision: "device",
    });
    expect(sharedPhoneLocation(sample, "approximately_1km")).toMatchObject({
      latitude_e6: 42_360_000,
      longitude_e6: -71_060_000,
      precision: "approximately_1km",
    });
  });

  test("normalizes negative zero and retains an unavailable accuracy value", () => {
    expect(
      sharedPhoneLocation({
        coords: { latitude: -0, longitude: -0, accuracy: null },
        timestamp: 0,
      }),
    ).toEqual({
      latitude_e6: 0,
      longitude_e6: 0,
      horizontal_accuracy_m: null,
      captured_at_unix_ms: 0,
      precision: "approximately_100m",
    });
  });

  test("rejects invalid native samples before they cross the firmware boundary", () => {
    expect(() =>
      sharedPhoneLocation({
        coords: { latitude: 91, longitude: 0, accuracy: 1 },
        timestamp: 1,
      }),
    ).toThrow("Latitude must be between -90 and 90");
    expect(() =>
      sharedPhoneLocation({
        coords: { latitude: 0, longitude: 0, accuracy: -1 },
        timestamp: 1,
      }),
    ).toThrow("Horizontal accuracy");
    expect(() =>
      sharedPhoneLocation({
        coords: { latitude: 0, longitude: 0, accuracy: 1 },
        timestamp: Number.POSITIVE_INFINITY,
      }),
    ).toThrow("Location timestamp");
  });

  test("retains device precision, elevation, accuracy, capture time, source, and mock status for attempts", () => {
    expect(
      attemptPhoneLocationObservation({ ...sample, mocked: false }, "precise", "foreground_stream"),
    ).toEqual({
      state: "available",
      latitude_e6: 42_357_111,
      longitude_e6: -71_061_924,
      horizontal_accuracy_mm: 8_250,
      altitude_mm: 17_234,
      vertical_accuracy_mm: 3_125,
      captured_at_unix_ms: 1_785_084_000_123,
      authorization: "precise",
      source: "foreground_stream",
      mocked: false,
    });
  });

  test("preserves unavailable elevation without inventing a zero-altitude fix", () => {
    expect(
      attemptPhoneLocationObservation(
        {
          ...sample,
          coords: { ...sample.coords, altitude: null, altitudeAccuracy: null },
        },
        "precise",
        "foreground_stream",
      ),
    ).toMatchObject({ altitude_mm: null, vertical_accuracy_mm: null });
  });

  test("accepts signed elevation and rejects invalid or unrepresentable vertical values", () => {
    expect(
      attemptPhoneLocationObservation(
        {
          ...sample,
          coords: { ...sample.coords, altitude: -12.345_6, altitudeAccuracy: 1.5 },
        },
        "precise",
        "foreground_stream",
      ),
    ).toMatchObject({ altitude_mm: -12_346, vertical_accuracy_mm: 1_500 });

    expect(() =>
      attemptPhoneLocationObservation(
        {
          ...sample,
          coords: { ...sample.coords, altitude: Number.POSITIVE_INFINITY },
        },
        "precise",
        "foreground_stream",
      ),
    ).toThrow("Altitude must be null or a finite number");
    expect(() =>
      attemptPhoneLocationObservation(
        {
          ...sample,
          coords: { ...sample.coords, altitude: I32_ALTITUDE_OVERFLOW_METRES },
        },
        "precise",
        "foreground_stream",
      ),
    ).toThrow("Altitude cannot be represented in whole millimetres");
    expect(() =>
      attemptPhoneLocationObservation(
        {
          ...sample,
          coords: { ...sample.coords, altitudeAccuracy: -1 },
        },
        "precise",
        "foreground_stream",
      ),
    ).toThrow("Vertical accuracy cannot be represented in whole millimetres");
  });

  test("preserves platform-reported precise, approximate, and unknown authorization", () => {
    expect(phoneLocationAuthorization({ ios: { accuracy: "full", scope: "whenInUse" } })).toBe(
      "precise",
    );
    expect(phoneLocationAuthorization({ android: { accuracy: "coarse" } })).toBe("approximate");
    expect(phoneLocationAuthorization({})).toBe("unknown");
  });
});
