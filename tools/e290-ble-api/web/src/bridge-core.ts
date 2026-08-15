export const BRIDGE_PROTOCOL_VERSION = 1;
export const FRAME_WRITE = 1;
export const FRAME_INDICATION = 2;
export const MAX_SOCKET_BUFFER_BYTES = 64 * 1024;
export const MAX_PRE_READY_INDICATION_FRAMES = 32;
export const MAXIMUM_PROFILE_FRAGMENT_BYTES = 248;
export const MAX_PRE_READY_INDICATION_BYTES =
  MAX_PRE_READY_INDICATION_FRAMES * (MAXIMUM_PROFILE_FRAGMENT_BYTES + 2);

export type BridgeProfile = Readonly<{
  bridgeProtocol: number;
  gattProfileMajor: number;
  gattProfileMinor: number;
  serviceUuid: string;
  rxUuid: string;
  txUuid: string;
  maximumFragmentBytes: number;
  operationTimeoutMs: number;
  writeType: "with_response";
  txDelivery: "indication";
}>;

export type WriteFrame = Readonly<{
  id: number;
  value: Uint8Array;
}>;

export type CharacteristicCapabilities = Readonly<{
  writeWithResponse: boolean;
  indicate: boolean;
}>;

export interface GattWriter {
  writeValueWithResponse(value: Uint8Array): Promise<void>;
}

export interface BinarySender {
  readonly bufferedAmount: number;
  send(value: Uint8Array): void;
}

function sendBounded(sender: BinarySender, frame: Uint8Array): void {
  if (
    sender.bufferedAmount + frame.byteLength >
    MAX_SOCKET_BUFFER_BYTES
  ) {
    throw new Error("browser bridge WebSocket send buffer exceeded its bound");
  }
  sender.send(frame);
}

export class PreReadyIndicationBuffer {
  readonly #frames: Uint8Array[] = [];
  #bytes = 0;
  #ready = false;

  push(
    sender: BinarySender,
    value: DataView,
    maximumFragmentBytes: number,
  ): void {
    const frame = encodeIndicationFrame(value, maximumFragmentBytes);
    if (this.#ready) {
      sendBounded(sender, frame);
      return;
    }
    if (
      this.#frames.length >= MAX_PRE_READY_INDICATION_FRAMES ||
      this.#bytes + frame.byteLength > MAX_PRE_READY_INDICATION_BYTES
    ) {
      throw new Error("pre-ready GATT indication queue exceeded its bound");
    }
    this.#frames.push(frame);
    this.#bytes += frame.byteLength;
  }

  markReady(sender: BinarySender): void {
    if (this.#ready) {
      throw new Error("GATT indication buffer was marked ready twice");
    }
    this.#ready = true;
    for (const frame of this.#frames) {
      sendBounded(sender, frame);
    }
    this.#frames.length = 0;
    this.#bytes = 0;
  }
}

function requireInteger(
  value: unknown,
  name: string,
  minimum: number,
  maximum: number,
): number {
  if (
    typeof value !== "number" ||
    !Number.isInteger(value) ||
    value < minimum ||
    value > maximum
  ) {
    throw new Error(`${name} is outside ${minimum}..=${maximum}`);
  }
  return value;
}

function requireUuid(value: unknown, name: string): string {
  if (
    typeof value !== "string" ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu.test(
      value,
    )
  ) {
    throw new Error(`${name} is not a canonical UUID`);
  }
  return value.toLowerCase();
}

export function parseBridgeProfile(value: unknown): BridgeProfile {
  if (typeof value !== "object" || value === null) {
    throw new Error("bridge profile is not an object");
  }
  const profile = value as Record<string, unknown>;
  const bridgeProtocol = requireInteger(
    profile.bridgeProtocol,
    "bridgeProtocol",
    1,
    255,
  );
  if (bridgeProtocol !== BRIDGE_PROTOCOL_VERSION) {
    throw new Error(`unsupported bridge protocol ${bridgeProtocol}`);
  }
  const maximumFragmentBytes = requireInteger(
    profile.maximumFragmentBytes,
    "maximumFragmentBytes",
    1,
    MAXIMUM_PROFILE_FRAGMENT_BYTES,
  );
  const operationTimeoutMs = requireInteger(
    profile.operationTimeoutMs,
    "operationTimeoutMs",
    1,
    60_000,
  );
  if (profile.writeType !== "with_response") {
    throw new Error("profile does not require write-with-response");
  }
  if (profile.txDelivery !== "indication") {
    throw new Error("profile does not require TX indications");
  }
  return {
    bridgeProtocol,
    gattProfileMajor: requireInteger(
      profile.gattProfileMajor,
      "gattProfileMajor",
      1,
      65_535,
    ),
    gattProfileMinor: requireInteger(
      profile.gattProfileMinor,
      "gattProfileMinor",
      0,
      65_535,
    ),
    serviceUuid: requireUuid(profile.serviceUuid, "serviceUuid"),
    rxUuid: requireUuid(profile.rxUuid, "rxUuid"),
    txUuid: requireUuid(profile.txUuid, "txUuid"),
    maximumFragmentBytes,
    operationTimeoutMs,
    writeType: "with_response",
    txDelivery: "indication",
  };
}

export function validateCapabilities(
  capabilities: CharacteristicCapabilities,
): void {
  if (!capabilities.writeWithResponse) {
    throw new Error("RX characteristic lacks write-with-response");
  }
  if (!capabilities.indicate) {
    throw new Error("TX characteristic lacks indication support");
  }
}

export function decodeWriteFrame(
  frame: ArrayBuffer,
  maximumFragmentBytes: number,
): WriteFrame {
  const bytes = new Uint8Array(frame);
  const headerBytes = 6;
  if (
    bytes.length <= headerBytes ||
    bytes.length > headerBytes + maximumFragmentBytes
  ) {
    throw new Error("write bridge frame has an invalid size");
  }
  if (
    bytes[0] !== BRIDGE_PROTOCOL_VERSION ||
    bytes[1] !== FRAME_WRITE
  ) {
    throw new Error("write bridge frame has an invalid header");
  }
  const id = new DataView(frame).getUint32(2, false);
  return { id, value: bytes.slice(headerBytes) };
}

export function encodeIndicationFrame(
  value: DataView,
  maximumFragmentBytes: number,
): Uint8Array {
  if (
    value.byteLength === 0 ||
    value.byteLength > maximumFragmentBytes
  ) {
    throw new Error("GATT indication has an invalid size");
  }
  const frame = new Uint8Array(2 + value.byteLength);
  frame[0] = BRIDGE_PROTOCOL_VERSION;
  frame[1] = FRAME_INDICATION;
  frame.set(
    new Uint8Array(value.buffer, value.byteOffset, value.byteLength),
    2,
  );
  return frame;
}

export async function relayWrite(
  writer: GattWriter,
  sender: (control: string) => void,
  frame: ArrayBuffer,
  maximumFragmentBytes: number,
): Promise<void> {
  const write = decodeWriteFrame(frame, maximumFragmentBytes);
  try {
    await writer.writeValueWithResponse(write.value);
    sender(JSON.stringify({ type: "write_ack", id: write.id }));
  } catch (error) {
    sender(
      JSON.stringify({
        type: "write_error",
        id: write.id,
        error: boundedError(error),
      }),
    );
  }
}

export function sendIndication(
  sender: BinarySender,
  value: DataView,
  maximumFragmentBytes: number,
): void {
  sendBounded(
    sender,
    encodeIndicationFrame(value, maximumFragmentBytes),
  );
}

export function boundedError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.slice(0, 256);
}
