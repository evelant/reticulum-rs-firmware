import {
  BASIC_LXMF_SELECTION_OVERHEAD_BYTES,
  EMPTY_LXMF_FIELDS_ENCODED_BYTES,
  MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES,
  MAX_LXMF_DIRECT_CONTENT_BYTES,
} from "../generated/api.ts";

/** Array-of-four marker plus the LXMF float64 timestamp. */
const BASIC_LXMF_ARRAY_AND_TIMESTAMP_BYTES = 10;
const MSGPACK_BIN8_MAX_BYTES = 0xff;
const MSGPACK_BIN16_MAX_BYTES = 0xffff;

export interface DirectLxmfPayloadBudget {
  readonly fieldsEncodedBytes: number;
  readonly fits: boolean;
  readonly maximumPayloadBytes: number;
  readonly overByBytes: number;
  readonly payloadBytes: number;
}

/** Exact MessagePack binary size within the app's generated title/content bounds. */
export function encodedMessagePackBinaryBytes(valueBytes: number): number {
  if (!Number.isSafeInteger(valueBytes) || valueBytes < 0) {
    throw new RangeError("MessagePack binary length must be a non-negative safe integer");
  }
  if (valueBytes <= MSGPACK_BIN8_MAX_BYTES) return valueBytes + 2;
  if (valueBytes <= MSGPACK_BIN16_MAX_BYTES) return valueBytes + 3;
  throw new RangeError("Composer binary length exceeds MessagePack bin16");
}

/**
 * Preflight Python LXMF's direct-lane selection bound.
 *
 * Unlocated messages are exact. A located draft conservatively reserves the
 * generated maximum Sideband fields size because the fresh fix does not exist
 * until the user presses Queue.
 */
export function directLxmfPayloadBudget(
  titleBytes: number,
  contentBytes: number,
  attachLocation: boolean,
): DirectLxmfPayloadBudget {
  const fieldsEncodedBytes = attachLocation
    ? MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES
    : EMPTY_LXMF_FIELDS_ENCODED_BYTES;
  const payloadBytes =
    BASIC_LXMF_ARRAY_AND_TIMESTAMP_BYTES +
    encodedMessagePackBinaryBytes(titleBytes) +
    encodedMessagePackBinaryBytes(contentBytes) +
    fieldsEncodedBytes;
  const maximumPayloadBytes = BASIC_LXMF_SELECTION_OVERHEAD_BYTES + MAX_LXMF_DIRECT_CONTENT_BYTES;
  return {
    fieldsEncodedBytes,
    fits: payloadBytes <= maximumPayloadBytes,
    maximumPayloadBytes,
    overByBytes: Math.max(0, payloadBytes - maximumPayloadBytes),
    payloadBytes,
  };
}

export function directLxmfPayloadError(
  titleBytes: number,
  contentBytes: number,
  attachLocation: boolean,
): string | null {
  const budget = directLxmfPayloadBudget(titleBytes, contentBytes, attachLocation);
  if (budget.fits) return null;
  return `Combined LXMF payload is ${budget.overByBytes} ${budget.overByBytes === 1 ? "byte" : "bytes"} too large${attachLocation ? " with attached location" : ""} (${budget.payloadBytes} / ${budget.maximumPayloadBytes} encoded bytes); shorten the title or message`;
}
