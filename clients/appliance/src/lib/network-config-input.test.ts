import { describe, expect, test } from "bun:test";

import {
  networkBytesText,
  networkSsidInput,
  tcpPeerHostnameInputError,
  tcpPeerInputError,
  wifiPassphraseError,
} from "./network-config-input.ts";

describe("network configuration inputs", () => {
  test("round-trips binary SSIDs without silently interpreting them as text", () => {
    const display = networkBytesText({ encoding: "hex", value: "6669656c64ff" });
    expect(display).toBe("hex:6669656c64ff");
    expect(networkSsidInput(display)).toEqual({
      value: { encoding: "hex", value: "6669656c64ff" },
    });
    expect(networkSsidInput("hex:abc")).toEqual({
      error: "Hex SSID must contain complete byte pairs after hex:",
    });
  });

  test("bounds SSIDs by UTF-8 bytes rather than JavaScript characters", () => {
    expect(networkSsidInput("mesh")).toEqual({
      value: { encoding: "utf8", value: "mesh" },
    });
    expect(networkSsidInput("é".repeat(16))).toEqual({
      value: { encoding: "utf8", value: "é".repeat(16) },
    });
    expect(networkSsidInput("é".repeat(17))).toEqual({
      error: "SSID must contain 1 to 32 UTF-8 bytes",
    });
  });

  test("accepts only the firmware's printable WPA2 passphrase range", () => {
    expect(wifiPassphraseError("12345678")).toBeNull();
    expect(wifiPassphraseError("short")).toContain("8 to 63");
    expect(wifiPassphraseError("line\nbreak")).toContain("printable ASCII");
  });

  test("validates the bounded unicast IPv4 peer and TCP port", () => {
    expect(tcpPeerInputError("192.0.2.10", "4242")).toBeNull();
    expect(tcpPeerInputError("0.1.2.3", "4242")).toContain("routable unicast");
    expect(tcpPeerInputError("127.0.0.1", "4242")).toContain("routable unicast");
    expect(tcpPeerInputError("239.1.2.3", "4242")).toContain("unicast");
    expect(tcpPeerInputError("240.1.2.3", "4242")).toContain("routable unicast");
    expect(tcpPeerInputError("255.255.255.255", "4242")).toContain("unicast");
    expect(tcpPeerInputError("192.0.2.999", "4242")).toContain("dotted-decimal");
    expect(tcpPeerInputError("192.0.2.10", "0")).toContain("1 and 65535");
  });

  test("validates the firmware's bounded DNS hostname shape", () => {
    expect(tcpPeerHostnameInputError("rmap.world", "4242")).toBeNull();
    expect(tcpPeerHostnameInputError("node.reticulumnet.nl", "4242")).toBeNull();
    expect(tcpPeerHostnameInputError("-invalid.example", "4242")).toContain("DNS labels");
    expect(tcpPeerHostnameInputError("invalid..example", "4242")).toContain("DNS labels");
    expect(tcpPeerHostnameInputError("x".repeat(97), "4242")).toContain("1 to 96");
    expect(tcpPeerHostnameInputError("rmap.world", "65536")).toContain("1 and 65535");
  });
});
