import { describe, expect, test } from "bun:test";

import type {
  DiagnosticInterfaceView,
  LoraDiagnosticsView,
  NetworkConfigView,
  NetworkRuntimeStatusView,
  RadioRoutesStatusView,
  ReticulumTcpPeerView,
} from "../generated/api.ts";
import { buildNodeInterfaces } from "./node-interfaces.ts";

function lora(overrides: Partial<LoraDiagnosticsView> = {}): LoraDiagnosticsView {
  return {
    applied_tx_power_dbm: 22,
    bandwidth_hz: 125_000,
    cad_busy: 0,
    cad_clear: 0,
    coding_rate_denominator: 5,
    frequency_hz: 915_000_000,
    last_data_tx: null,
    last_rx: { age_ms: 2_000, rssi_dbm: -102, snr_db: 7 },
    last_tx: null,
    rx_drops: 0,
    rx_errors: 0,
    rx_packets: 12,
    rx_physical_frames: 12,
    spreading_factor: 7,
    tx_access_rejects: 0,
    tx_completed_frames: 5,
    tx_failures: 0,
    tx_successes: 5,
    tx_terminal_jobs: 5,
    ...overrides,
  };
}

function record(
  id: number,
  kind: DiagnosticInterfaceView["kind"],
  state: DiagnosticInterfaceView["state"] = "online",
): DiagnosticInterfaceView {
  return { id, kind, state, generation: 1, logical_mtu: 500, bitrate: 5_470 };
}

function radioRoutes(
  interfaces: DiagnosticInterfaceView[],
  loraDiagnostics: LoraDiagnosticsView | null = lora(),
): RadioRoutesStatusView {
  return {
    interfaces,
    lora: loraDiagnostics,
    observed_peer_count: 0,
    retained_route_count: 0,
    rns: {
      announces_received: 0,
      dedup_drops: 0,
      forwarded: 0,
      invalid_drops: 0,
      links_closed: 0,
      links_established: 0,
      links_failed: 0,
      paths_expired: 0,
      paths_learned: 0,
      received: 0,
    },
    route_table_revision: 0,
    routes: [],
    uptime_ms: 0,
    usable_route_count: 0,
  };
}

function configuration(overrides: Partial<NetworkConfigView> = {}): NetworkConfigView {
  return {
    automatic_announces_enabled: true,
    device_name: null,
    lora_profile: {
      bandwidth_hz: 125_000,
      coding_rate_denominator: 5,
      frequency_hz: 915_000_000,
      spreading_factor: 7,
      tx_power_dbm: 22,
    },
    lora_tx_power_dbm: 22,
    revision: 3,
    rmap_discovery_enabled: false,
    rmap_phone_location: null,
    rmap_share_location: false,
    tcp_peer: null,
    wifi_profiles: [],
    wifi_transport_enabled: true,
    ...overrides,
  };
}

function runtime(overrides: Partial<NetworkRuntimeStatusView> = {}): NetworkRuntimeStatusView {
  return {
    active_wifi_profile: null,
    applied_revision: 3,
    configured_revision: 3,
    connected_ssid: null,
    dns_diagnostics: null,
    ipv4_address: null,
    last_tcp_failure: null,
    rmap_status: null,
    rssi_dbm: null,
    tcp_peer_state: "disabled",
    wifi_state: "disabled",
    ...overrides,
  };
}

function tcpPeer(): ReticulumTcpPeerView {
  return { enabled: true, ipv4_address: "192.0.2.10", port: 4242 };
}

describe("buildNodeInterfaces", () => {
  test("lists LoRa, TCP client, and the Wi-Fi link for a gateway node", () => {
    const summaries = buildNodeInterfaces({
      config: configuration({ tcp_peer: tcpPeer() }),
      radioRoutes: radioRoutes([record(1, "lora"), record(2, "tcp_client")]),
      runtime: runtime({
        ipv4_address: "192.168.1.20",
        tcp_peer_state: "connected",
        wifi_state: "connected",
      }),
    });

    expect(summaries.map((summary) => summary.kind)).toEqual([
      "lora",
      "tcp_client",
      "wifi_station",
    ]);
    expect(summaries.map((summary) => summary.state)).toEqual(["online", "online", "online"]);
    expect(summaries[0]?.summary).toBe("Last RX 2s ago · -102 dBm · 7 dB SNR");
    expect(summaries[0]?.metrics.map((metric) => metric.label)).toContain("Frequency");
    expect(summaries[1]?.metrics).toContainEqual({ label: "Peer", value: "192.0.2.10:4242" });
  });

  test("reports only LoRa when no network configuration is available", () => {
    const summaries = buildNodeInterfaces({
      config: null,
      radioRoutes: radioRoutes([record(1, "lora")]),
      runtime: null,
    });

    expect(summaries).toHaveLength(1);
    expect(summaries[0]).toMatchObject({ kind: "lora", enabled: true, state: "online" });
  });

  test("shows the TCP interface offline when no peer is configured", () => {
    const summaries = buildNodeInterfaces({
      config: configuration(),
      radioRoutes: radioRoutes([record(1, "lora"), record(2, "tcp_client", "offline")]),
      runtime: runtime({ tcp_peer_state: "disabled", wifi_state: "disconnected" }),
    });

    const tcp = summaries.find((summary) => summary.kind === "tcp_client");
    expect(tcp).toMatchObject({ enabled: false, state: "offline" });
    expect(tcp?.summary).toContain("no peer configured");
  });

  test("maps faulted and non-connected states to coarse tones", () => {
    const summaries = buildNodeInterfaces({
      config: configuration({ tcp_peer: tcpPeer() }),
      radioRoutes: radioRoutes([record(1, "lora", "faulted"), record(2, "tcp_client")]),
      runtime: runtime({ tcp_peer_state: "faulted", wifi_state: "connecting" }),
    });

    expect(summaries[0]?.state).toBe("faulted");
    expect(summaries[1]?.state).toBe("faulted");
    expect(summaries[2]?.state).toBe("offline");
  });

  test("falls back to desired LoRa profile when no applied diagnostics exist", () => {
    const summaries = buildNodeInterfaces({
      config: configuration(),
      radioRoutes: radioRoutes([record(1, "lora")], null),
      runtime: runtime(),
    });

    const loraSummary = summaries.find((summary) => summary.kind === "lora");
    expect(loraSummary?.summary).toBe("No packets received yet");
    expect(loraSummary?.metrics).toContainEqual({ label: "Frequency", value: "915.0000 MHz" });
  });
});
