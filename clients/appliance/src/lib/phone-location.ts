import type { LocationObject, LocationPermissionResponse } from "expo-location";

import type {
  PhoneLocationAuthorizationView,
  PhoneLocationObservationView,
  PhoneLocationSourceView,
} from "../generated/api.ts";

export type PhoneLocationPrecision = "device" | "approximately_100m" | "approximately_1km";

export interface SharedPhoneLocation {
  readonly latitude_e6: number;
  readonly longitude_e6: number;
  readonly horizontal_accuracy_m: number | null;
  readonly captured_at_unix_ms: number;
  readonly precision: PhoneLocationPrecision;
}

export interface PhoneLocationCaptureOptions {
  /**
   * Balanced avoids needlessly expensive GPS fixes for a public map marker.
   * High is available for callers that explicitly need it.
   */
  readonly accuracy?: "balanced" | "high";
  /**
   * Defaults to roughly 100 metre coordinate rounding as a privacy-conscious
   * starting point. The reported sensor accuracy remains available separately.
   */
  readonly precision?: PhoneLocationPrecision;
}

export interface PhoneLocationSample {
  readonly coords: {
    readonly latitude: number;
    readonly longitude: number;
    readonly accuracy: number | null;
    /** Platform-reported ellipsoid altitude in metres, when available. */
    readonly altitude?: number | null;
    /** Platform-reported vertical accuracy in metres, when available. */
    readonly altitudeAccuracy?: number | null;
  };
  readonly timestamp: number;
  readonly mocked?: boolean;
}

export interface ForegroundPhoneLocationTelemetry {
  /** Whether a live platform watcher was installed. */
  readonly collecting: boolean;
  remove(): void;
}

export type PhoneLocationObservationSink = (
  observation: PhoneLocationObservationView,
) => void | Promise<void>;

const E6_PER_DEGREE = 1_000_000;
const MILLIMETRES_PER_METRE = 1_000;
const I32_MIN = -0x8000_0000;
const I32_MAX = 0x7fff_ffff;
const U32_MAX = 0xffff_ffff;

const PRECISION_QUANTUM_E6: Readonly<Record<PhoneLocationPrecision, number>> = {
  device: 1,
  approximately_100m: 1_000,
  approximately_1km: 10_000,
};

export class ForegroundLocationPermissionError extends Error {
  readonly canAskAgain: boolean;

  constructor(canAskAgain: boolean) {
    super(
      canAskAgain
        ? "Foreground location permission was not granted"
        : "Foreground location permission was denied in system settings",
    );
    this.name = "ForegroundLocationPermissionError";
    this.canAskAgain = canAskAgain;
  }
}

function roundedE6(value: number, quantum: number): number {
  const rounded = Math.round((value * E6_PER_DEGREE) / quantum) * quantum;
  return Object.is(rounded, -0) ? 0 : rounded;
}

function finiteCoordinate(value: number, minimum: number, maximum: number, label: string): number {
  if (!Number.isFinite(value) || value < minimum || value > maximum) {
    throw new RangeError(`${label} must be between ${minimum} and ${maximum} degrees`);
  }
  return value;
}

function optionalMillimetres(
  value: number | null | undefined,
  minimum: number,
  maximum: number,
  label: string,
): number | null {
  if (value === null || value === undefined) return null;
  if (!Number.isFinite(value)) {
    throw new RangeError(`${label} must be null or a finite number`);
  }
  const millimetres = Math.round(value * MILLIMETRES_PER_METRE);
  if (!Number.isSafeInteger(millimetres) || millimetres < minimum || millimetres > maximum) {
    throw new RangeError(`${label} cannot be represented in whole millimetres`);
  }
  return Object.is(millimetres, -0) ? 0 : millimetres;
}

/**
 * Converts an Expo location sample into the bounded integer representation
 * shared with firmware. This pure boundary is intentionally host-testable.
 */
export function sharedPhoneLocation(
  sample: PhoneLocationSample,
  precision: PhoneLocationPrecision = "approximately_100m",
): SharedPhoneLocation {
  const latitude = finiteCoordinate(sample.coords.latitude, -90, 90, "Latitude");
  const longitude = finiteCoordinate(sample.coords.longitude, -180, 180, "Longitude");
  const timestamp = Math.round(sample.timestamp);
  if (!Number.isSafeInteger(timestamp) || timestamp < 0) {
    throw new RangeError("Location timestamp must be a non-negative safe integer");
  }

  const accuracy = sample.coords.accuracy;
  if (accuracy !== null && (!Number.isFinite(accuracy) || accuracy < 0)) {
    throw new RangeError("Horizontal accuracy must be null or a non-negative finite number");
  }

  const quantum = PRECISION_QUANTUM_E6[precision];
  return {
    latitude_e6: roundedE6(latitude, quantum),
    longitude_e6: roundedE6(longitude, quantum),
    horizontal_accuracy_m: accuracy,
    captured_at_unix_ms: timestamp,
    precision,
  };
}

/**
 * Requests foreground permission at the point of user action and obtains one
 * current fix. It does not subscribe to updates or request background access.
 */
export async function captureForegroundPhoneLocation(
  options: PhoneLocationCaptureOptions = {},
): Promise<SharedPhoneLocation> {
  // Keep Expo's native module out of Bun host-test evaluation while giving
  // Metro a synchronous dependency it can retain in the appliance's required
  // single-file embedded SPA bundle.
  const Location = require("expo-location") as typeof import("expo-location");
  const permission = await Location.requestForegroundPermissionsAsync();
  if (!permission.granted) {
    throw new ForegroundLocationPermissionError(permission.canAskAgain);
  }

  const location: LocationObject = await Location.getCurrentPositionAsync({
    accuracy: options.accuracy === "high" ? Location.Accuracy.High : Location.Accuracy.Balanced,
  });
  return sharedPhoneLocation(location, options.precision);
}

/** Project Expo's platform-specific precision grant into the durable vocabulary. */
export function phoneLocationAuthorization(
  permission: Pick<LocationPermissionResponse, "android" | "ios">,
): PhoneLocationAuthorizationView {
  if (permission.ios?.accuracy === "full" || permission.android?.accuracy === "fine") {
    return "precise";
  }
  if (permission.ios?.accuracy === "reduced" || permission.android?.accuracy === "coarse") {
    return "approximate";
  }
  return "unknown";
}

/**
 * Convert one high-accuracy phone fix into a private app-submission sample.
 * Unlike the RMAP helper, this retains device E6 precision and capture metadata.
 */
export function attemptPhoneLocationObservation(
  sample: PhoneLocationSample,
  authorization: PhoneLocationAuthorizationView,
  source: PhoneLocationSourceView,
): PhoneLocationObservationView {
  const location = sharedPhoneLocation(sample, "device");
  const accuracyMm = optionalMillimetres(
    location.horizontal_accuracy_m,
    0,
    U32_MAX,
    "Horizontal accuracy",
  );
  const altitudeMm = optionalMillimetres(sample.coords.altitude, I32_MIN, I32_MAX, "Altitude");
  const verticalAccuracyMm = optionalMillimetres(
    sample.coords.altitudeAccuracy,
    0,
    U32_MAX,
    "Vertical accuracy",
  );
  return {
    state: "available",
    latitude_e6: location.latitude_e6,
    longitude_e6: location.longitude_e6,
    horizontal_accuracy_mm: accuracyMm,
    altitude_mm: altitudeMm,
    vertical_accuracy_mm: verticalAccuracyMm,
    captured_at_unix_ms: location.captured_at_unix_ms,
    authorization,
    source,
    mocked: sample.mocked ?? null,
  };
}

function unavailable(
  reason: Extract<PhoneLocationObservationView, { state: "unavailable" }>["reason"],
): PhoneLocationObservationView {
  return { state: "unavailable", reason };
}

/**
 * Start private, high-accuracy foreground field telemetry.
 *
 * This never requests background location and never publishes a coordinate.
 * Provider failures are represented explicitly so sending remains available.
 */
export async function startForegroundPhoneLocationTelemetry(
  onObservation: PhoneLocationObservationSink,
): Promise<ForegroundPhoneLocationTelemetry> {
  const Location = require("expo-location") as typeof import("expo-location");
  if (!(await Location.hasServicesEnabledAsync())) {
    await onObservation(unavailable("services_disabled"));
    return { collecting: false, remove() {} };
  }

  const permission = await Location.requestForegroundPermissionsAsync();
  if (!permission.granted) {
    await onObservation(unavailable("permission_denied"));
    return { collecting: false, remove() {} };
  }
  const authorization = phoneLocationAuthorization(permission);
  await onObservation(unavailable("no_fix_yet"));

  const lastKnown = await Location.getLastKnownPositionAsync({ maxAge: 5 * 60_000 });
  if (lastKnown !== null) {
    await onObservation(attemptPhoneLocationObservation(lastKnown, authorization, "last_known"));
  }

  const subscription = await Location.watchPositionAsync(
    {
      accuracy: Location.Accuracy.High,
      distanceInterval: 1,
      timeInterval: 1_000,
    },
    (sample) => {
      void onObservation(
        attemptPhoneLocationObservation(sample, authorization, "foreground_stream"),
      );
    },
    () => {
      void onObservation(unavailable("provider_error"));
    },
  );
  return { collecting: true, remove: () => subscription.remove() };
}
