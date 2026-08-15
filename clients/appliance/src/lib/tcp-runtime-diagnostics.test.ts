import { describe, expect, test } from "bun:test";

import type { NetworkRuntimeStatusView } from "../generated/api.ts";
import {
  reticulumDnsDiagnosticDetails,
  reticulumDnsRawOutcomeLabel,
  reticulumTcpDiagnostic,
  reticulumTcpFailureLabel,
  reticulumTcpStateLabel,
} from "./tcp-runtime-diagnostics.ts";

function status(
  tcpPeerState: NetworkRuntimeStatusView["tcp_peer_state"],
  lastTcpFailure: NetworkRuntimeStatusView["last_tcp_failure"],
): NetworkRuntimeStatusView {
  return {
    active_wifi_profile: null,
    applied_revision: 3,
    configured_revision: 3,
    connected_ssid: null,
    dns_diagnostics: null,
    ipv4_address: "192.0.2.10",
    last_tcp_failure: lastTcpFailure,
    rssi_dbm: -70,
    tcp_peer_state: tcpPeerState,
    wifi_state: "connected",
  };
}

describe("Reticulum TCP runtime diagnostics", () => {
  test("distinguishes a bounded retry delay from an active connection attempt", () => {
    expect(reticulumTcpStateLabel("connecting")).toBe("Connecting");
    expect(reticulumTcpStateLabel("backoff")).toBe("Retrying");
  });

  test("renders every typed recoverable failure without exposing raw transport text", () => {
    expect(reticulumTcpFailureLabel("dns_timeout")).toBe("DNS lookup timed out");
    expect(reticulumTcpFailureLabel("dns_lookup_failed")).toBe("DNS lookup failed");
    expect(reticulumTcpFailureLabel("dns_no_ipv4_result")).toBe("DNS returned no IPv4 address");
    expect(reticulumTcpFailureLabel("connect_invalid_state")).toBe(
      "TCP stack rejected the connection state",
    );
    expect(reticulumTcpFailureLabel("connect_reset")).toBe("Connection was reset");
    expect(reticulumTcpFailureLabel("connect_timeout")).toBe("Connection attempt timed out");
    expect(reticulumTcpFailureLabel("connect_no_route")).toBe("No route to the peer");
    expect(reticulumTcpFailureLabel("socket_closed")).toBe("Connected socket closed");
    expect(reticulumTcpFailureLabel("transmit_failed")).toBe("Reticulum frame transmission failed");
  });

  test("explains automatic retry and keeps the preceding failure visible while reconnecting", () => {
    expect(reticulumTcpDiagnostic(status("backoff", "dns_timeout"))).toBe(
      "DNS lookup timed out. Retrying automatically.",
    );
    expect(reticulumTcpDiagnostic(status("connecting", "connect_reset"))).toBe(
      "Last failure: Connection was reset.",
    );
    expect(reticulumTcpDiagnostic(status("backoff", null))).toBe(
      "Retry delay active. The appliance will try again automatically.",
    );
    expect(reticulumTcpDiagnostic(status("connected", null))).toBeNull();
  });

  test("renders sparse resolver diagnostics with explicit typed sources", () => {
    const observed: NetworkRuntimeStatusView = {
      ...status("backoff", "dns_lookup_failed"),
      dns_diagnostics: {
        dhcp_servers: ["192.168.50.1", null, null],
        gateway_ipv4: "192.168.50.1",
        primary_outcome: "lookup_failed",
        raw_attempts: [
          {
            outcome: { kind: "timeout" },
            server: "192.168.50.1",
            source: "dhcp",
          },
          null,
          {
            outcome: { kind: "response_code", code: 3 },
            server: "1.1.1.1",
            source: "public",
          },
          {
            outcome: { kind: "awaiting_response" },
            server: "9.9.9.9",
            source: "public",
          },
          null,
        ],
        raw_setup_state: "ready",
        resolution: null,
      },
    };

    expect(reticulumDnsDiagnosticDetails(observed)).toEqual({
      context: "Gateway 192.168.50.1 · DHCP DNS 192.168.50.1 · Raw socket ready",
      resolution: null,
      rows: [
        {
          key: "system",
          label: "System DNS",
          outcome: "Lookup failed",
          tone: "failure",
        },
        {
          key: "raw-0-192.168.50.1",
          label: "DHCP 192.168.50.1",
          outcome: "Timed out",
          tone: "failure",
        },
        {
          key: "raw-2-1.1.1.1",
          label: "Public 1.1.1.1",
          outcome: "DNS response code 3",
          tone: "failure",
        },
        {
          key: "raw-3-9.9.9.9",
          label: "Public 9.9.9.9",
          outcome: "Awaiting response…",
          tone: "neutral",
        },
      ],
    });
    expect(reticulumDnsRawOutcomeLabel({ kind: "malformed" })).toBe("Malformed response");
  });

  test("shows the exact successful resolver path", () => {
    const observed: NetworkRuntimeStatusView = {
      ...status("connecting", "dns_lookup_failed"),
      dns_diagnostics: {
        dhcp_servers: [],
        gateway_ipv4: null,
        primary_outcome: "lookup_failed",
        raw_attempts: [],
        raw_setup_state: "ready",
        resolution: {
          address: "217.154.9.220",
          resolver: "9.9.9.9",
          source: "raw_public",
        },
      },
    };

    expect(reticulumDnsDiagnosticDetails(observed)?.resolution).toBe(
      "Resolved 217.154.9.220 via Public 9.9.9.9",
    );
  });
});
