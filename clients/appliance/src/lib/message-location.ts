import type { LocationObject } from "expo-location";

import type { MessageLocationView } from "../generated/api.ts";
import {
  ForegroundLocationPermissionError,
  type PhoneLocationSample,
  sharedPhoneLocation,
} from "./phone-location.ts";

const CENTIMETRES_PER_METRE = 100;
const CENTIDEGREES_PER_DEGREE = 100;
const U16_MAX = 0xffff;
const U32_MAX = 0xffff_ffff;
const I32_MIN = -0x8000_0000;
const I32_MAX = 0x7fff_ffff;

export interface MessageLocationPhoneSample extends PhoneLocationSample {
  readonly coords: PhoneLocationSample["coords"] & {
    readonly altitude?: number | null;
    readonly heading?: number | null;
    readonly speed?: number | null;
  };
}

export interface MessageLocationPresentation {
  readonly accuracy: string;
  readonly altitude: string;
  readonly bearing: string;
  readonly coordinates: string;
  readonly mapUrl: string;
  readonly speed: string;
  readonly summary: string;
  readonly updated: string;
}

function roundedClamped(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, Math.round(value)));
}

function optionalScaled(
  value: number | null | undefined,
  scale: number,
  minimum: number,
  maximum: number,
): number {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return 0;
  }
  return roundedClamped(value * scale, minimum, maximum);
}

/**
 * Convert a fresh phone fix into Sideband's bounded integer location fields.
 * Zero is retained as the protocol's unavailable sentinel for optional sensor
 * fields. Horizontal accuracy is the explicitly clamped unsigned-16 value.
 */
export function messageLocationFromPhoneSample(
  sample: MessageLocationPhoneSample,
): MessageLocationView {
  const shared = sharedPhoneLocation(sample, "device");
  const updatedAtSeconds = Math.floor(shared.captured_at_unix_ms / 1_000);
  if (
    !Number.isSafeInteger(updatedAtSeconds) ||
    updatedAtSeconds < 0 ||
    updatedAtSeconds > U32_MAX
  ) {
    throw new RangeError("Location time cannot be represented as unsigned Unix seconds");
  }

  const accuracyCm =
    shared.horizontal_accuracy_m === null
      ? 0
      : roundedClamped(shared.horizontal_accuracy_m * CENTIMETRES_PER_METRE, 0, U16_MAX);
  const heading = sample.coords.heading;
  const bearingCentidegrees =
    heading === null || heading === undefined || !Number.isFinite(heading) || heading < 0
      ? 0
      : Math.round((((heading % 360) + 360) % 360) * CENTIDEGREES_PER_DEGREE) % 36_000;

  return {
    latitude_e6: shared.latitude_e6,
    longitude_e6: shared.longitude_e6,
    altitude_cm: optionalScaled(sample.coords.altitude, CENTIMETRES_PER_METRE, I32_MIN, I32_MAX),
    speed_cm_per_second: optionalScaled(sample.coords.speed, CENTIMETRES_PER_METRE, 0, U32_MAX),
    bearing_centidegrees: bearingCentidegrees,
    accuracy_cm: accuracyCm,
    updated_at_unix_seconds: updatedAtSeconds,
  };
}

/** Request foreground permission and take a new high-accuracy fix for this send. */
export async function captureForegroundMessageLocation(): Promise<MessageLocationView> {
  const Location = require("expo-location") as typeof import("expo-location");
  const permission = await Location.requestForegroundPermissionsAsync();
  if (!permission.granted) {
    throw new ForegroundLocationPermissionError(permission.canAskAgain);
  }
  const sample: LocationObject = await Location.getCurrentPositionAsync({
    accuracy: Location.Accuracy.High,
  });
  return messageLocationFromPhoneSample(sample);
}

export function messageLocationMapUrl(location: MessageLocationView): string {
  const latitude = (location.latitude_e6 / 1_000_000).toFixed(6);
  const longitude = (location.longitude_e6 / 1_000_000).toFixed(6);
  return `https://www.openstreetmap.org/?mlat=${latitude}&mlon=${longitude}#map=16/${latitude}/${longitude}`;
}

/** User-facing values that preserve the protocol's zero-sentinel ambiguity. */
export function messageLocationPresentation(
  location: MessageLocationView,
): MessageLocationPresentation {
  const latitude = (location.latitude_e6 / 1_000_000).toFixed(6);
  const longitude = (location.longitude_e6 / 1_000_000).toFixed(6);
  const coordinates = `${latitude}, ${longitude}`;
  const accuracy =
    location.accuracy_cm === 0
      ? "Unavailable"
      : `±${(location.accuracy_cm / 100).toFixed(2)} m (${location.accuracy_cm} cm)`;
  return {
    accuracy,
    altitude:
      location.altitude_cm === 0
        ? "0.00 m (zero can mean unavailable)"
        : `${(location.altitude_cm / 100).toFixed(2)} m (${location.altitude_cm} cm)`,
    bearing:
      location.bearing_centidegrees === 0
        ? "0.00° (zero can mean unavailable or due north)"
        : `${(location.bearing_centidegrees / 100).toFixed(2)}° (${location.bearing_centidegrees} centidegrees)`,
    coordinates,
    mapUrl: messageLocationMapUrl(location),
    speed:
      location.speed_cm_per_second === 0
        ? "0.00 m/s (zero can mean unavailable or stationary)"
        : `${(location.speed_cm_per_second / 100).toFixed(2)} m/s (${location.speed_cm_per_second} cm/s)`,
    summary: `Attached location · ${coordinates} · ${accuracy === "Unavailable" ? "accuracy unavailable" : accuracy}`,
    updated: `${new Date(location.updated_at_unix_seconds * 1_000).toLocaleString()} (${location.updated_at_unix_seconds} Unix seconds)`,
  };
}
