import type {
  DiagnosticInterfaceView,
  LoraDiagnosticsView,
  LoraRadioProfileView,
  NetworkConfigView,
  NetworkRuntimeStatusView,
  RadioRoutesStatusView,
  ReticulumTcpPeerView,
} from "../generated/api.ts";
import { networkBytesText } from "./network-config-input.ts";
import { reticulumTcpDiagnostic, reticulumTcpStateLabel } from "./tcp-runtime-diagnostics.ts";

/** Transport family of one displayed node interface or link. */
export type NodeInterfaceKind = "lora" | "tcp_client" | "tcp_server" | "wifi_station" | "other";

/** Coarse availability tone for an interface or link summary. */
export type NodeInterfaceState = "online" | "offline" | "faulted" | "unknown";

/** One labeled status line on an interface card. */
export interface NodeInterfaceMetric {
  readonly label: string;
  readonly value: string;
}

/** One interface or link rendered under its owning node. */
export interface NodeInterfaceSummary {
  readonly key: string;
  readonly kind: NodeInterfaceKind;
  readonly label: string;
  readonly enabled: boolean;
  readonly state: NodeInterfaceState;
  readonly summary: string;
  readonly metrics: readonly NodeInterfaceMetric[];
}

export interface NodeInterfaceInput {
  readonly config: NetworkConfigView | null;
  readonly runtime: NetworkRuntimeStatusView | null;
  readonly radioRoutes: RadioRoutesStatusView | null;
}

function megahertz(hz: number): string {
  return `${(hz / 1_000_000).toFixed(4)} MHz`;
}

function kilohertz(hz: number): string {
  return `${Math.round(hz / 1_000)} kHz`;
}

function tcpPeerAddress(peer: ReticulumTcpPeerView): string {
  return "ipv4_address" in peer ? peer.ipv4_address : peer.hostname;
}

function loraState(record: DiagnosticInterfaceView | undefined): NodeInterfaceState {
  if (record === undefined) return "unknown";
  switch (record.state) {
    case "online":
      return "online";
    case "offline":
      return "offline";
    case "faulted":
      return "faulted";
  }
}

function tcpState(runtime: NetworkRuntimeStatusView | null): NodeInterfaceState {
  if (runtime === null) return "unknown";
  switch (runtime.tcp_peer_state) {
    case "connected":
      return "online";
    case "faulted":
      return "faulted";
    case "disabled":
    case "waiting_for_network":
    case "connecting":
    case "backoff":
      return "offline";
  }
}

function wifiState(runtime: NetworkRuntimeStatusView | null): NodeInterfaceState {
  if (runtime === null) return "unknown";
  switch (runtime.wifi_state) {
    case "connected":
      return "online";
    case "connecting":
    case "disconnected":
    case "disabled":
      return "offline";
  }
}

function loraSummary(lora: LoraDiagnosticsView | null): string {
  if (lora === null || lora.last_rx === null) return "No packets received yet";
  const ageSeconds = Math.max(0, Math.round(lora.last_rx.age_ms / 1_000));
  return `Last RX ${ageSeconds}s ago · ${lora.last_rx.rssi_dbm} dBm · ${lora.last_rx.snr_db} dB SNR`;
}

function loraMetrics(
  profile: LoraRadioProfileView | null,
  lora: LoraDiagnosticsView | null,
): readonly NodeInterfaceMetric[] {
  const metrics: NodeInterfaceMetric[] = [];
  if (lora !== null) {
    metrics.push({ label: "Frequency", value: megahertz(lora.frequency_hz) });
    metrics.push({ label: "Bandwidth", value: kilohertz(lora.bandwidth_hz) });
    metrics.push({ label: "Spreading factor", value: String(lora.spreading_factor) });
    metrics.push({ label: "TX power", value: `${lora.applied_tx_power_dbm} dBm` });
    metrics.push({ label: "Received packets", value: String(lora.rx_packets) });
    metrics.push({ label: "TX successes", value: String(lora.tx_successes) });
  } else if (profile !== null) {
    metrics.push({ label: "Frequency", value: megahertz(profile.frequency_hz) });
    metrics.push({ label: "Bandwidth", value: kilohertz(profile.bandwidth_hz) });
    metrics.push({ label: "Spreading factor", value: String(profile.spreading_factor) });
    metrics.push({ label: "TX power", value: `${profile.tx_power_dbm} dBm` });
  }
  return metrics;
}

function buildLora(
  profile: LoraRadioProfileView | null,
  lora: LoraDiagnosticsView | null,
  record: DiagnosticInterfaceView | undefined,
): NodeInterfaceSummary {
  return {
    key: "lora",
    kind: "lora",
    label: "LoRa radio",
    enabled: true,
    state: loraState(record),
    summary: loraSummary(lora),
    metrics: loraMetrics(profile, lora),
  };
}

function buildTcp(
  peer: ReticulumTcpPeerView | null,
  runtime: NetworkRuntimeStatusView | null,
  record: DiagnosticInterfaceView | undefined,
): NodeInterfaceSummary {
  const kind: NodeInterfaceKind = record?.kind === "tcp_server" ? "tcp_server" : "tcp_client";
  const diagnostic = reticulumTcpDiagnostic(runtime);
  const metrics: NodeInterfaceMetric[] = [];
  if (peer !== null) {
    metrics.push({ label: "Peer", value: `${tcpPeerAddress(peer)}:${peer.port}` });
    metrics.push({ label: "Enabled", value: peer.enabled ? "yes" : "no" });
  } else {
    metrics.push({ label: "Peer", value: "not configured" });
  }
  return {
    key: kind,
    kind,
    label: "Reticulum TCP",
    enabled: peer?.enabled ?? false,
    state: tcpState(runtime),
    summary:
      diagnostic ??
      `${reticulumTcpStateLabel(runtime?.tcp_peer_state ?? null)}${peer === null ? " · no peer configured" : ""}`,
    metrics,
  };
}

function buildWifi(
  config: NetworkConfigView,
  runtime: NetworkRuntimeStatusView | null,
): NodeInterfaceSummary {
  const metrics: NodeInterfaceMetric[] = [];
  if (runtime?.connected_ssid !== null && runtime?.connected_ssid !== undefined) {
    metrics.push({ label: "Network", value: networkBytesText(runtime.connected_ssid) });
  }
  if (runtime?.ipv4_address !== null && runtime?.ipv4_address !== undefined) {
    metrics.push({ label: "IPv4", value: runtime.ipv4_address });
  }
  if (runtime?.rssi_dbm !== null && runtime?.rssi_dbm !== undefined) {
    metrics.push({ label: "RSSI", value: `${runtime.rssi_dbm} dBm` });
  }
  metrics.push({ label: "Saved networks", value: String(config.wifi_profiles.length) });

  const summary =
    runtime?.connected_ssid !== null && runtime?.connected_ssid !== undefined
      ? `${networkBytesText(runtime.connected_ssid)}${runtime?.ipv4_address !== null && runtime?.ipv4_address !== undefined ? ` · ${runtime.ipv4_address}` : ""}`
      : "No associated network";

  return {
    key: "wifi_station",
    kind: "wifi_station",
    label: "Wi-Fi station",
    enabled: config.wifi_transport_enabled,
    state: wifiState(runtime),
    summary,
    metrics,
  };
}

/**
 * Project the board's configured interfaces and links into one generic list.
 *
 * The diagnostic interface registry is the authority for which packet
 * interfaces exist; each record is enriched with kind-specific configuration
 * and runtime status. The Wi-Fi station is a link-layer service that the TCP
 * interface depends on, so it is appended as a sibling rather than modelled as
 * a Reticulum interface.
 */
export function buildNodeInterfaces(input: NodeInterfaceInput): NodeInterfaceSummary[] {
  const { config, runtime, radioRoutes } = input;
  const records = radioRoutes?.interfaces ?? [];
  const loraRecord = records.find((record) => record.kind === "lora");
  const tcpRecord = records.find(
    (record) => record.kind === "tcp_client" || record.kind === "tcp_server",
  );

  const summaries: NodeInterfaceSummary[] = [];

  if (config !== null || radioRoutes !== null) {
    summaries.push(buildLora(config?.lora_profile ?? null, radioRoutes?.lora ?? null, loraRecord));
  }

  const wifiEnabled = config?.wifi_transport_enabled === true;
  const tcpConfigured = config?.tcp_peer != null;
  if (tcpConfigured || wifiEnabled) {
    summaries.push(buildTcp(config?.tcp_peer ?? null, runtime, tcpRecord));
  }

  if (config !== null && wifiEnabled) {
    summaries.push(buildWifi(config, runtime));
  }

  return summaries;
}
