import type {
  NetworkRuntimeStatusView,
  RmapDeferredReasonView,
  RmapRuntimeStatusView,
} from "../generated/api.ts";

export interface RmapRuntimePresentation {
  readonly headline: string;
  readonly rows: readonly string[];
  readonly tone: "error" | "neutral" | "success" | "warning";
}

function compactSeconds(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86_400)
    return `${Math.floor(seconds / 3_600)}h ${Math.floor((seconds % 3_600) / 60)}m`;
  return `${Math.floor(seconds / 86_400)}d ${Math.floor((seconds % 86_400) / 3_600)}h`;
}

function deferredReasonLabel(reason: RmapDeferredReasonView): string {
  switch (reason) {
    case "discovery_model_invalid":
      return "Discovery data is invalid";
    case "payload_encoding_failed":
      return "Discovery payload could not be encoded";
    case "stamp_initialization_failed":
      return "Stamp search could not start";
    case "destination_activation_failed":
      return "Discovery destination could not be activated";
    case "stamp_search_exhausted":
      return "Stamp search was exhausted";
    case "initial_tcp_not_ready":
      return "Public TCP interface is not ready";
    case "announce_payload_too_large":
      return "Discovery announce is too large";
    case "announce_queue_full":
      return "Native announce queue is full";
    case "announce_construction_rejected":
      return "Native announce construction was rejected";
    case "ordinary_queue_rejected":
      return "Transmit coordinator is busy";
  }
}

function stampLabel(status: RmapRuntimeStatusView): string {
  switch (status.stamp_phase) {
    case "disabled":
      return "Disabled";
    case "searching":
      return `Searching (${status.stamp_attempts} attempts)`;
    case "ready":
      return `Ready (${status.stamp_attempts} attempts)`;
    case "exhausted":
      return `Exhausted (${status.stamp_attempts} attempts)`;
    case "faulted":
      return "Faulted";
  }
}

function gateLabel(status: RmapRuntimeStatusView): string {
  switch (status.initial_tcp_gate) {
    case "not_required":
      return "No public TCP target required";
    case "waiting":
      return "Waiting for public TCP";
    case "open":
      return "Public TCP ready";
  }
}

function queueLabel(status: RmapRuntimeStatusView): string {
  const suffix =
    status.last_queue_attempt_at_uptime_seconds === null
      ? ""
      : ` at ${compactSeconds(status.last_queue_attempt_at_uptime_seconds)} uptime`;
  switch (status.last_queue_outcome) {
    case "not_attempted":
      return "No publication attempted";
    case "accepted":
      return `Accepted ${status.queued_count} publication${status.queued_count === 1 ? "" : "s"}${suffix}`;
    case "announce_admission_deferred":
      return `Native announce admission deferred${suffix}`;
    case "ordinary_admission_deferred":
      return `Transmit admission deferred${suffix}`;
  }
}

function egressLabel(status: RmapRuntimeStatusView): string | null {
  switch (status.egress_confirmation) {
    case "not_applicable":
      return null;
    case "not_observed":
      return "Accepted for transmission; radio or link completion is not tracked";
    case "confirmed":
      return "Physical egress confirmed";
  }
}

/**
 * Explain the board-owned RMAP publisher without inferring success from a
 * desired configuration or from cadence alone.
 */
export function rmapRuntimePresentation(
  runtime: NetworkRuntimeStatusView | null,
  desiredEnabled: boolean,
): RmapRuntimePresentation {
  const status = runtime?.rmap_status;
  if (status === null || status === undefined) {
    return {
      headline: desiredEnabled ? "Status unavailable" : "Disabled",
      rows: desiredEnabled
        ? ["The connected firmware did not report RMAP publication state."]
        : ["RMAP publication is not enabled."],
      tone: "neutral",
    };
  }

  const rows = [`Stamp ${stampLabel(status)}`, gateLabel(status), queueLabel(status)];
  const egress = egressLabel(status);
  if (egress !== null) rows.push(egress);
  if (status.next_due_in_seconds !== null) {
    rows.push(`Next publication due in ${compactSeconds(status.next_due_in_seconds)}`);
  }
  if (status.deferred_reason !== null) {
    rows.push(deferredReasonLabel(status.deferred_reason));
  }

  if (!status.config_applied) {
    return { headline: "Restart required", rows, tone: "warning" };
  }
  if (status.stamp_phase === "faulted" || status.stamp_phase === "exhausted") {
    return { headline: "Publication blocked", rows, tone: "error" };
  }
  if (status.stamp_phase === "disabled") {
    return { headline: "Disabled", rows, tone: "neutral" };
  }
  if (status.initial_tcp_gate === "waiting") {
    return { headline: "Waiting for public TCP", rows, tone: "warning" };
  }
  if (
    status.last_queue_outcome === "announce_admission_deferred" ||
    status.last_queue_outcome === "ordinary_admission_deferred" ||
    status.deferred_reason !== null
  ) {
    return { headline: "Publication deferred", rows, tone: "warning" };
  }
  if (status.stamp_phase === "searching") {
    return { headline: "Preparing signed marker", rows, tone: "neutral" };
  }
  if (status.last_queue_outcome === "accepted") {
    return { headline: "Publication accepted", rows, tone: "success" };
  }
  return { headline: "Ready to publish", rows, tone: "neutral" };
}
