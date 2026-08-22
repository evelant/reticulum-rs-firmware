/** Opaque PRNS interface identity exposed at the product boundary. */
export type ReticulumInterfaceId = [number, number, number, number, number, number, number, number];

/** Stable lowercase representation for display, keys, and equality. */
export function reticulumInterfaceIdHex(interfaceId: readonly number[]): string {
  return interfaceId.map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function sameReticulumInterfaceId(
  left: readonly number[],
  right: readonly number[],
): boolean {
  return reticulumInterfaceIdHex(left) === reticulumInterfaceIdHex(right);
}

/** PRNS interface-kind discriminant carried by the self-describing id. */
export function reticulumInterfaceKind(interfaceId: readonly number[]): number | null {
  const kind = interfaceId[0];
  return Number.isInteger(kind) && kind !== undefined && kind >= 0 && kind <= 255 ? kind : null;
}

/** Stable PRNS interface-kind name, retaining unknown future discriminants. */
export function reticulumInterfaceKindName(interfaceId: readonly number[]): string {
  const kind = reticulumInterfaceKind(interfaceId);
  const names: Readonly<Record<number, string>> = {
    0: "Loopback",
    1: "TCP client",
    2: "TCP server",
    3: "UDP",
    4: "Serial",
    5: "USB Auto host",
    6: "USB Auto device",
    7: "Auto Wi-Fi",
    8: "Wi-Fi peer",
    9: "Local server",
    10: "Local client",
    11: "TCP server peer",
    12: "Bluetooth Auto",
    13: "Bluetooth peer",
    14: "LoRa",
    15: "KISS",
    16: "AX.25 KISS",
    17: "Pipe",
    18: "RNode",
    19: "Backbone server",
    20: "Backbone server peer",
    21: "Backbone client",
    22: "ESP-NOW",
    23: "WebSocket client",
    24: "WebSocket server",
    25: "WebSocket server peer",
    26: "Wi-Fi Direct",
    27: "Wi-Fi Direct peer",
    28: "Wi-Fi Aware",
    29: "Wi-Fi Aware peer",
    30: "I2P",
    31: "I2P peer",
    32: "Weave",
    33: "Weave peer",
  };
  return kind === null ? "Unknown interface" : (names[kind] ?? `Interface kind ${kind}`);
}

export type ReticulumInterfaceFamily = "bluetooth" | "lora" | "tcp" | "other";

/** Coarse UI grouping derived from PRNS's exact interface kind. */
export function reticulumInterfaceFamily(interfaceId: readonly number[]): ReticulumInterfaceFamily {
  switch (reticulumInterfaceKind(interfaceId)) {
    case 1:
    case 2:
    case 11:
    case 19:
    case 20:
    case 21:
    case 23:
    case 24:
    case 25:
      return "tcp";
    case 12:
    case 13:
      return "bluetooth";
    case 14:
      return "lora";
    default:
      return "other";
  }
}

/** Deterministic full-width interface identity for test fixtures. */
export function syntheticReticulumInterfaceId(slot: number): ReticulumInterfaceId {
  return [0, 0, 0, 0, 0, 0, 0, slot];
}
