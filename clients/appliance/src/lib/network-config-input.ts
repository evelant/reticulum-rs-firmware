import {
  type BytesView,
  MAX_WIFI_PASSPHRASE_BYTES,
  MAX_WIFI_SSID_BYTES,
  MIN_WIFI_PASSPHRASE_BYTES,
} from "../generated/api.ts";
import { utf8ByteLength } from "./limits.ts";

export function networkBytesText(field: BytesView): string {
  return field.encoding === "utf8" ? field.value : `hex:${field.value}`;
}

export function networkSsidInput(
  input: string,
): { readonly error: string } | { readonly value: BytesView } {
  if (input.startsWith("hex:")) {
    const hex = input.slice(4).toLowerCase();
    if (!/^(?:[0-9a-f]{2})+$/.test(hex)) {
      return { error: "Hex SSID must contain complete byte pairs after hex:" };
    }
    const bytes = hex.length / 2;
    if (bytes < 1 || bytes > MAX_WIFI_SSID_BYTES) {
      return { error: `SSID must contain 1 to ${MAX_WIFI_SSID_BYTES} bytes` };
    }
    return { value: { encoding: "hex", value: hex } };
  }

  const bytes = utf8ByteLength(input);
  if (bytes < 1 || bytes > MAX_WIFI_SSID_BYTES) {
    return { error: `SSID must contain 1 to ${MAX_WIFI_SSID_BYTES} UTF-8 bytes` };
  }
  return { value: { encoding: "utf8", value: input } };
}

export function wifiPassphraseError(passphrase: string): string | null {
  if (
    passphrase.length < MIN_WIFI_PASSPHRASE_BYTES ||
    passphrase.length > MAX_WIFI_PASSPHRASE_BYTES ||
    !/^[\x20-\x7e]+$/.test(passphrase)
  ) {
    return `Password must contain ${MIN_WIFI_PASSPHRASE_BYTES} to ${MAX_WIFI_PASSPHRASE_BYTES} printable ASCII characters`;
  }
  return null;
}

function tcpPeerPortInputError(portText: string): string | null {
  const port = Number(portText);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    return "TCP peer port must be between 1 and 65535";
  }
  return null;
}

export function tcpPeerInputError(ipv4Address: string, portText: string): string | null {
  const octets = ipv4Address.split(".");
  if (
    octets.length !== 4 ||
    octets.some(
      (octet) =>
        !/^(?:0|[1-9]\d{0,2})$/.test(octet) ||
        Number.parseInt(octet, 10) < 0 ||
        Number.parseInt(octet, 10) > 255,
    )
  ) {
    return "TCP peer must be a dotted-decimal IPv4 address";
  }
  const parsedOctets = octets.map((octet) => Number.parseInt(octet, 10));
  if (
    parsedOctets[0] === 0 ||
    parsedOctets[0] === 127 ||
    (parsedOctets[0] !== undefined && parsedOctets[0] >= 224)
  ) {
    return "TCP peer must be a routable unicast IPv4 address";
  }
  return tcpPeerPortInputError(portText);
}

/**
 * Mirrors the device API's bounded ASCII DNS-label contract. Keeping this
 * check in the app makes a typo actionable before the compare-and-swap write;
 * firmware remains authoritative.
 */
export function tcpPeerHostnameInputError(hostname: string, portText: string): string | null {
  if (
    hostname.length < 1 ||
    hostname.length > 96 ||
    [...hostname].some((character) => character.charCodeAt(0) > 0x7f)
  ) {
    return "TCP peer hostname must contain 1 to 96 ASCII characters";
  }
  const labels = hostname.split(".");
  if (
    labels.some(
      (label) =>
        label.length < 1 ||
        label.length > 63 ||
        !/^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$/.test(label),
    )
  ) {
    return "TCP peer hostname must contain valid DNS labels";
  }
  return tcpPeerPortInputError(portText);
}
