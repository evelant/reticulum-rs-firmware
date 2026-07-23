import type { NativeApplianceError } from "@reticulum/appliance-native";

export type NativeErrorPredicate = (value: unknown) => value is NativeApplianceError;

function errorReason(error: NativeApplianceError): string | null {
  if (!("inner" in error)) return null;
  const inner = error.inner;
  if (typeof inner !== "object" || inner === null || !("reason" in inner)) return null;
  return typeof inner.reason === "string" ? inner.reason : null;
}

function errorLabel(tag: string): string {
  return tag.replace(/([a-z0-9])([A-Z])/g, "$1 $2").toLowerCase();
}

export function nativeApplianceErrorMessage(error: NativeApplianceError): string {
  const label = `Native appliance ${errorLabel(String(error.tag))}`;
  const reason = errorReason(error);
  return reason === null ? label : `${label}: ${reason}`;
}

export function normalizeNativeError(error: unknown, isNativeError: NativeErrorPredicate): Error {
  if (error !== null && error !== undefined && isNativeError(error)) {
    return new Error(nativeApplianceErrorMessage(error), { cause: error });
  }
  return error instanceof Error ? error : new Error(String(error));
}
