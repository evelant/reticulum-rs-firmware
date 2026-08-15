import type { NativeBridgeContract } from "@reticulum/appliance-native";

import {
  DEVICE_API_VERSION_MAJOR,
  DEVICE_API_VERSION_MINOR,
  MAX_LXMF_BASIC_CONTENT_BYTES,
  MAX_LXMF_BASIC_TITLE_BYTES,
  MAX_LXMF_READ_CHUNK_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_NOMAD_PAGE_BYTES,
  MAX_NOMAD_PAGE_PATH_BYTES,
  MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
} from "../generated/api.ts";

export const NATIVE_BRIDGE_API_MAJOR = 1 as const;
export const NATIVE_BRIDGE_API_MINOR = 23 as const;

const EXPECTED_FIELDS = {
  bridgeApiMajor: NATIVE_BRIDGE_API_MAJOR,
  bridgeApiMinor: NATIVE_BRIDGE_API_MINOR,
  deviceApiMajor: DEVICE_API_VERSION_MAJOR,
  deviceApiMinor: DEVICE_API_VERSION_MINOR,
  maxMessageBytes: MAX_MESSAGE_BYTES,
  maxLxmfReadChunkBytes: MAX_LXMF_READ_CHUNK_BYTES,
  maxLxmfBasicTitleBytes: MAX_LXMF_BASIC_TITLE_BYTES,
  maxLxmfBasicContentBytes: MAX_LXMF_BASIC_CONTENT_BYTES,
  maxNomadPagePathBytes: MAX_NOMAD_PAGE_PATH_BYTES,
  maxNomadPageBytes: MAX_NOMAD_PAGE_BYTES,
  maxNomadRequestTimestampUnixMs: BigInt(MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS),
} as const satisfies NativeBridgeContract;

export function assertNativeBridgeContract(contract: NativeBridgeContract): NativeBridgeContract {
  const mismatches = Object.entries(EXPECTED_FIELDS).flatMap(([field, expected]) => {
    const observed = contract[field as keyof NativeBridgeContract];
    return observed === expected ? [] : [`${field}: expected ${expected}, observed ${observed}`];
  });
  if (mismatches.length > 0) {
    throw new Error(`native Rust bridge contract mismatch (${mismatches.join("; ")})`);
  }
  return contract;
}
