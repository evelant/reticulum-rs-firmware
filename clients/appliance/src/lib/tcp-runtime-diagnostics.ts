import type {
  NetworkRuntimeStatusView,
  ReticulumDnsPrimaryOutcomeView,
  ReticulumDnsRawOutcomeView,
  ReticulumDnsRawSetupStateView,
  ReticulumDnsResolutionView,
  ReticulumTcpFailureView,
  ReticulumTcpPeerStateView,
} from "../generated/api.ts";

export interface ReticulumDnsDiagnosticRow {
  readonly key: string;
  readonly label: string;
  readonly outcome: string;
  readonly tone: "failure" | "neutral" | "success";
}

export interface ReticulumDnsDiagnosticDetails {
  readonly context: string;
  readonly resolution: string | null;
  readonly rows: readonly ReticulumDnsDiagnosticRow[];
}

export function reticulumTcpStateLabel(state: ReticulumTcpPeerStateView | null): string {
  switch (state) {
    case null:
      return "Unknown";
    case "disabled":
      return "Disabled";
    case "waiting_for_network":
      return "Waiting for Wi-Fi";
    case "connecting":
      return "Connecting";
    case "backoff":
      return "Retrying";
    case "connected":
      return "Connected";
    case "faulted":
      return "Faulted";
  }
}

export function reticulumTcpFailureLabel(failure: ReticulumTcpFailureView): string {
  switch (failure) {
    case "dns_timeout":
      return "DNS lookup timed out";
    case "dns_lookup_failed":
      return "DNS lookup failed";
    case "dns_no_ipv4_result":
      return "DNS returned no IPv4 address";
    case "connect_invalid_state":
      return "TCP stack rejected the connection state";
    case "connect_reset":
      return "Connection was reset";
    case "connect_timeout":
      return "Connection attempt timed out";
    case "connect_no_route":
      return "No route to the peer";
    case "socket_closed":
      return "Connected socket closed";
    case "transmit_failed":
      return "Reticulum frame transmission failed";
  }
}

export function reticulumDnsPrimaryOutcomeLabel(outcome: ReticulumDnsPrimaryOutcomeView): string {
  switch (outcome) {
    case "not_started":
      return "Not started";
    case "resolving":
      return "Resolving…";
    case "resolved":
      return "Resolved";
    case "no_servers":
      return "No DHCP resolvers";
    case "timeout":
      return "Timed out";
    case "lookup_failed":
      return "Lookup failed";
    case "no_ipv4_result":
      return "No IPv4 answer";
  }
}

export function reticulumDnsRawSetupStateLabel(state: ReticulumDnsRawSetupStateView): string {
  switch (state) {
    case "not_started":
      return "not started";
    case "binding":
      return "binding";
    case "ready":
      return "ready";
    case "bind_failed":
      return "bind failed";
    case "encode_failed":
      return "query encoding failed";
  }
}

export function reticulumDnsRawOutcomeLabel(outcome: ReticulumDnsRawOutcomeView): string {
  switch (outcome.kind) {
    case "not_started":
      return "Not started";
    case "skipped_duplicate":
      return "Skipped duplicate";
    case "skipped_local_name":
      return "Skipped for local name";
    case "sending":
      return "Sending…";
    case "awaiting_response":
      return "Awaiting response…";
    case "resolved":
      return "Resolved";
    case "send_failed":
      return "Send failed";
    case "timeout":
      return "Timed out";
    case "not_a_response":
      return "Not a DNS response";
    case "truncated":
      return "Truncated response";
    case "response_code":
      return `DNS response code ${outcome.code}`;
    case "question_mismatch":
      return "Question mismatch";
    case "malformed":
      return "Malformed response";
    case "no_ipv4_result":
      return "No IPv4 answer";
  }
}

function primaryOutcomeTone(
  outcome: ReticulumDnsPrimaryOutcomeView,
): ReticulumDnsDiagnosticRow["tone"] {
  switch (outcome) {
    case "resolved":
      return "success";
    case "no_servers":
    case "timeout":
    case "lookup_failed":
    case "no_ipv4_result":
      return "failure";
    case "not_started":
    case "resolving":
      return "neutral";
  }
}

function rawOutcomeTone(outcome: ReticulumDnsRawOutcomeView): ReticulumDnsDiagnosticRow["tone"] {
  switch (outcome.kind) {
    case "resolved":
      return "success";
    case "send_failed":
    case "timeout":
    case "not_a_response":
    case "truncated":
    case "response_code":
    case "question_mismatch":
    case "malformed":
    case "no_ipv4_result":
      return "failure";
    case "not_started":
    case "skipped_duplicate":
    case "skipped_local_name":
    case "sending":
    case "awaiting_response":
      return "neutral";
  }
}

function resolutionLabel(resolution: ReticulumDnsResolutionView): string {
  switch (resolution.source) {
    case "system_dns":
      return `Resolved ${resolution.address} via System DNS`;
    case "raw_dhcp":
      return `Resolved ${resolution.address} via DHCP ${resolution.resolver ?? "resolver"}`;
    case "raw_public":
      return `Resolved ${resolution.address} via Public ${resolution.resolver ?? "resolver"}`;
  }
}

export function reticulumDnsDiagnosticDetails(
  status: NetworkRuntimeStatusView | null,
): ReticulumDnsDiagnosticDetails | null {
  const diagnostics = status?.dns_diagnostics;
  if (diagnostics === null || diagnostics === undefined) return null;

  const rows: ReticulumDnsDiagnosticRow[] = [
    {
      key: "system",
      label: "System DNS",
      outcome: reticulumDnsPrimaryOutcomeLabel(diagnostics.primary_outcome),
      tone: primaryOutcomeTone(diagnostics.primary_outcome),
    },
  ];
  for (const [index, attempt] of diagnostics.raw_attempts.entries()) {
    if (attempt === null) continue;
    const source = attempt.source === "dhcp" ? "DHCP" : "Public";
    rows.push({
      key: `raw-${index}-${attempt.server}`,
      label: `${source} ${attempt.server}`,
      outcome: reticulumDnsRawOutcomeLabel(attempt.outcome),
      tone: rawOutcomeTone(attempt.outcome),
    });
  }

  const dhcpServers = diagnostics.dhcp_servers.filter(
    (server): server is string => server !== null,
  );
  const context = [
    `Gateway ${diagnostics.gateway_ipv4 ?? "unavailable"}`,
    `DHCP DNS ${dhcpServers.length === 0 ? "none" : dhcpServers.join(", ")}`,
    `Raw socket ${reticulumDnsRawSetupStateLabel(diagnostics.raw_setup_state)}`,
  ].join(" · ");

  return {
    context,
    resolution: diagnostics.resolution === null ? null : resolutionLabel(diagnostics.resolution),
    rows,
  };
}

/**
 * User-facing retry detail for the board-owned TCP actor.
 *
 * A failure remains visible while the next connection attempt is active. It
 * is cleared when the actor loses its station network without a more specific
 * DNS, TCP, or stream failure.
 */
export function reticulumTcpDiagnostic(status: NetworkRuntimeStatusView | null): string | null {
  if (status === null) return null;
  if (status.last_tcp_failure !== null) {
    const failure = reticulumTcpFailureLabel(status.last_tcp_failure);
    return status.tcp_peer_state === "backoff"
      ? `${failure}. Retrying automatically.`
      : `Last failure: ${failure}.`;
  }
  return status.tcp_peer_state === "backoff"
    ? "Retry delay active. The appliance will try again automatically."
    : null;
}
