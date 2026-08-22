import type {
  PacketEvidenceView,
  RadioTraceEventKindView,
  RadioTraceEventView,
  RadioTraceRouteResolutionView,
  RadioTraceTxOutcomeView,
} from "../generated/api.ts";
import { phoneLocationMetadata } from "./message-activity.ts";
import { reticulumInterfaceIdHex } from "./reticulum-interface-id.ts";

export type RadioTraceFilter =
  | "all"
  | "correlated"
  | "route"
  | "tx"
  | "rx"
  | "terminal"
  | "attention";

export interface RadioTracePresentation {
  readonly metadata: readonly string[];
  readonly observedAt: string;
  readonly title: string;
  readonly tone: "danger" | "muted" | "normal" | "success" | "warning";
}

export const RADIO_TRACE_FILTERS = [
  { label: "All", value: "all" },
  { label: "Messages", value: "correlated" },
  { label: "Routes", value: "route" },
  { label: "TX", value: "tx" },
  { label: "RX", value: "rx" },
  { label: "Proofs", value: "terminal" },
  { label: "Attention", value: "attention" },
] as const satisfies readonly { readonly label: string; readonly value: RadioTraceFilter }[];

const IMPORTED_AT_FORMAT = new Intl.DateTimeFormat(undefined, {
  dateStyle: "short",
  timeStyle: "medium",
});

function sentence(value: string): string {
  const words = value.replaceAll("_", " ");
  return `${words[0]?.toUpperCase() ?? ""}${words.slice(1)}`;
}

function signed(value: number): string {
  return `${value >= 0 ? "+" : ""}${value}`;
}

function profileMetadata(event: RadioTraceEventView): string[] {
  const profile = event.profile;
  return [
    `${(profile.frequency_hz / 1_000_000).toFixed(3)} MHz · BW ${(profile.bandwidth_hz / 1_000).toFixed(0)} kHz · SF${profile.spreading_factor} · CR 4/${profile.coding_rate_denominator} · requested ${signed(profile.requested_power_dbm)} dBm`,
    `Preamble ${profile.preamble_symbols} · ${profile.explicit_header ? "explicit" : "implicit"} header · CRC ${profile.crc ? "on" : "off"} · IQ ${profile.iq_inverted ? "inverted" : "normal"}`,
    `Profile fingerprint ${profile.fingerprint}`,
  ];
}

function packetEvidenceMetadata(packet: PacketEvidenceView): string[] {
  return [
    `Encoded packet ${packet.encoded_packet_len} bytes`,
    `Packet SHA-256 ${packet.encoded_packet_sha256}`,
  ];
}

function packetMetadata(
  event: Extract<RadioTraceEventKindView, { kind: "data_tx" | "logical_rx" | "route_selected" }>,
): string[] {
  return packetEvidenceMetadata(event.packet_evidence);
}

function tokenMetadata(event: RadioTraceEventKindView): string[] {
  if (event.kind === "logical_rx") {
    return event.rns_packet_hash === null ? [] : [`RNS packet hash ${event.rns_packet_hash}`];
  }
  if (event.kind === "inbound_proof") {
    return [`Inbound DATA correlation token ${event.correlation_token}`];
  }
  return [`RNS attempt token ${event.rns_attempt_token}`];
}

function routeResolutionLabel(resolution: RadioTraceRouteResolutionView): string {
  switch (resolution) {
    case "exact_ready":
      return "Exact retained route ready";
    case "exact_offline":
      return "Exact route interface offline";
    case "exact_missing":
      return "Exact route incomplete";
    case "broadcast_ready":
      return "Broadcast fallback ready";
    case "broadcast_unavailable":
      return "No usable exact route or broadcast fallback";
  }
}

function txOutcomeLabel(outcome: RadioTraceTxOutcomeView): string {
  switch (outcome) {
    case "transmitted":
      return "Every physical frame reached TxDone";
    case "access_rejected":
      return "Initial channel access rejected";
    case "permit_denied":
      return "Exact transmit permit denied";
    case "authorization_expired":
      return "Transmit authorization expired";
    case "post_grant_access_rejected":
      return "Post-grant channel access rejected";
    case "airtime_rejected":
      return "Airtime calculation or admission rejected";
    case "deadline_conversion_overflow":
      return "Dispatch deadline could not be represented";
    case "radio_inactive":
      return "Selected radio inactive";
    case "interface_configuration_mismatch":
      return "Router and radio interface configuration mismatch";
    case "radio_configuration_changed_before_permit":
      return "Radio profile changed before permit";
    case "radio_configuration_changed_after_permit":
      return "Radio profile changed after permit";
    case "cad_fault":
      return "Channel-activity detection fault";
    case "tx_fault":
      return "Physical transmit fault";
    case "control_plane_recovery":
      return "Transmit control-plane recovery";
    case "frame_invariant_recovery":
      return "Authorized frame invariant recovery";
    case "cancelled_radio_operation":
      return "Cancelled radio operation reconciled";
  }
}

function monotonicTimeLabel(microseconds: number): string {
  if (microseconds < 1_000) return `${microseconds} us since boot`;
  if (microseconds < 1_000_000) return `${(microseconds / 1_000).toFixed(1)} ms since boot`;
  return `${(microseconds / 1_000_000).toFixed(3)} s since boot`;
}

function frameCompletionMetadata(event: Extract<RadioTraceEventKindView, { kind: "data_tx" }>) {
  const completed = [event.frame_0_completed_at_us, event.frame_1_completed_at_us].filter(
    (value): value is number => value !== null,
  );
  return completed.map(
    (timestamp, index) => `Frame ${index + 1} TxDone · ${monotonicTimeLabel(timestamp)}`,
  );
}

function importedAtLabel(unixMs: number): string {
  const date = new Date(unixMs);
  return Number.isNaN(date.getTime())
    ? "Imported by app · invalid time"
    : `Imported by app · ${IMPORTED_AT_FORMAT.format(date)}`;
}

export function isRadioTraceAttention(event: RadioTraceEventView): boolean {
  switch (event.event.kind) {
    case "route_selected":
      return (
        event.event.resolution !== "exact_ready" && event.event.resolution !== "broadcast_ready"
      );
    case "data_tx":
      return event.event.outcome !== "transmitted";
    case "logical_rx":
      return false;
    case "attempt_terminal":
      return event.event.outcome !== "delivered";
    case "inbound_proof":
      return event.event.stage === "physical_tx_failed";
  }
}

export function radioTracePresentation(event: RadioTraceEventView): RadioTracePresentation {
  const evidence = event.event;
  const metadata = [
    `Trace ${event.event_id} · boot ${event.boot_id} · sequence ${event.event_sequence}`,
    `Board observation · ${monotonicTimeLabel(event.observed_at_us)}`,
  ];
  let title: string;
  let tone: RadioTracePresentation["tone"] = "normal";

  switch (evidence.kind) {
    case "route_selected":
      title = routeResolutionLabel(evidence.resolution);
      if (evidence.resolution === "broadcast_unavailable") tone = "danger";
      else if (evidence.resolution === "exact_offline" || evidence.resolution === "exact_missing") {
        tone = "warning";
      }
      metadata.push(`Destination ${evidence.destination}`);
      metadata.push(
        `${evidence.hops === 1 ? "Direct · 1 hop" : `${evidence.hops} hops`} · interface ${reticulumInterfaceIdHex(evidence.interface_id)}`,
      );
      metadata.push(
        evidence.next_hop_identity === null
          ? "Direct or broadcast next hop"
          : `Next-hop identity ${evidence.next_hop_identity}`,
      );
      metadata.push(`Device submission ${evidence.submission_id}`);
      metadata.push(...packetMetadata(evidence), ...tokenMetadata(evidence));
      break;
    case "data_tx":
      title =
        evidence.outcome === "transmitted"
          ? "LoRa DATA reached TxDone"
          : `LoRa DATA did not complete · ${sentence(evidence.outcome)}`;
      tone = evidence.outcome === "transmitted" ? "success" : "danger";
      metadata.push(txOutcomeLabel(evidence.outcome));
      metadata.push(`Interface ${reticulumInterfaceIdHex(evidence.interface_id)}`);
      metadata.push(
        `${evidence.completed_physical_frames}/${evidence.planned_physical_frames} physical frames completed · authorized frame ${evidence.authorized_frame_observed ? "observed" : "not observed"}`,
      );
      metadata.push(...frameCompletionMetadata(evidence));
      metadata.push(...packetMetadata(evidence), ...tokenMetadata(evidence));
      break;
    case "logical_rx":
      title = "LoRa logical packet received";
      tone = "success";
      metadata.push(
        `Interface ${reticulumInterfaceIdHex(evidence.interface_id)} · RSSI ${evidence.rssi_dbm} dBm · SNR ${evidence.snr_db} dB`,
      );
      metadata.push(...packetMetadata(evidence), ...tokenMetadata(evidence));
      break;
    case "attempt_terminal":
      title =
        evidence.outcome === "delivered"
          ? "Delivery proof completed attempt"
          : evidence.outcome === "delivery_timeout"
            ? "Delivery proof timed out"
            : "Attempt ended without final-hop transmission";
      tone = evidence.outcome === "delivered" ? "success" : "danger";
      if (evidence.proof_interface_id === null) {
        metadata.push("No local proof ingress was retained");
      } else {
        metadata.push(
          `Proof returned on interface ${reticulumInterfaceIdHex(evidence.proof_interface_id)}`,
        );
        metadata.push(
          evidence.proof_rssi_dbm === null || evidence.proof_snr_db === null
            ? "Proof interface did not report physical signal"
            : `Proof final hop · RSSI ${evidence.proof_rssi_dbm} dBm · SNR ${evidence.proof_snr_db} dB`,
        );
      }
      metadata.push(...tokenMetadata(evidence));
      break;
    case "inbound_proof":
      switch (evidence.stage) {
        case "data_logical_rx":
          title = "Inbound DATA reconstructed";
          break;
        case "ordinary_queued":
          title = "Delivery proof accepted for transmission";
          break;
        case "physical_tx_done":
          title = "Delivery proof reached TxDone";
          tone = "success";
          break;
        case "physical_tx_failed":
          title = "Delivery proof did not reach TxDone";
          tone = "danger";
          break;
      }
      if (evidence.message_id !== null) metadata.push(`LXMF message ${evidence.message_id}`);
      if (evidence.interface_id !== null) {
        const interfaceId = reticulumInterfaceIdHex(evidence.interface_id);
        metadata.push(
          evidence.rssi_dbm === null || evidence.snr_db === null
            ? `Interface ${interfaceId} · no physical signal retained`
            : `Inbound DATA · interface ${interfaceId} · RSSI ${evidence.rssi_dbm} dBm · SNR ${evidence.snr_db} dB`,
        );
      }
      if (evidence.packet_evidence !== null) {
        metadata.push(...packetEvidenceMetadata(evidence.packet_evidence));
      }
      if (evidence.dispatch_outcome !== null) {
        metadata.push(txOutcomeLabel(evidence.dispatch_outcome));
      }
      metadata.push(...tokenMetadata(evidence));
      break;
  }

  if (event.correlation !== null) {
    metadata.push(
      `Message row ${event.correlation.timeline_sequence} · outbox ${event.correlation.outbox_id} · attempt ${event.correlation.attempt_number}`,
    );
    metadata.push(
      ...phoneLocationMetadata(event.correlation.attempt_location, event.imported_at_unix_ms),
    );
  } else {
    metadata.push("Not correlated to a local outbound message attempt");
  }
  metadata.push(...profileMetadata(event));

  return { metadata, observedAt: importedAtLabel(event.imported_at_unix_ms), title, tone };
}

export function filterRadioTrace(
  events: readonly RadioTraceEventView[],
  filter: RadioTraceFilter,
  query: string,
): RadioTraceEventView[] {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  return [...events]
    .sort((left, right) => right.event_id - left.event_id)
    .filter((event) => {
      if (filter === "correlated" && event.correlation === null) return false;
      if (filter === "route" && event.event.kind !== "route_selected") return false;
      if (filter === "tx" && event.event.kind !== "data_tx") return false;
      if (filter === "rx" && event.event.kind !== "logical_rx") return false;
      if (
        filter === "terminal" &&
        event.event.kind !== "attempt_terminal" &&
        event.event.kind !== "inbound_proof"
      ) {
        return false;
      }
      if (filter === "attention" && !isRadioTraceAttention(event)) return false;
      if (normalizedQuery === "") return true;
      const presentation = radioTracePresentation(event);
      return [presentation.title, presentation.observedAt, ...presentation.metadata]
        .join(" ")
        .toLocaleLowerCase()
        .includes(normalizedQuery);
    });
}
