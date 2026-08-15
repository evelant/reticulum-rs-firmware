import type { RetrySendRequest, TimelineStatus, TimelineView } from "../generated/api.ts";

const REPLACEMENT_RETRYABLE_STATUSES = new Set<TimelineStatus>([
  "failed_no_path",
  "failed_delivery_timeout",
  "failed_internal",
]);

export interface TimelineMessageCapabilities {
  readonly canRetry: boolean;
  readonly canUseAsDraft: boolean;
}

/** Stable identity for a row while later timeline refreshes update its fields. */
export function timelineEntryKey(entry: TimelineView): string {
  return `${entry.sequence}:${entry.direction}`;
}

/**
 * Stable activity-query revision for a row projected repeatedly by polling.
 *
 * Object identity is deliberately excluded so an unchanged native timeline
 * refresh cannot clear message-history pagination. Fields that can correspond
 * to a new durable activity event remain part of the revision.
 */
export function timelineActivityRevision(entry: TimelineView): string {
  return [
    entry.message_id ?? "no-message",
    entry.status ?? "no-status",
    entry.submission_id ?? "no-submission",
    entry.current_attempt_number ?? "no-attempt",
    entry.automatic_retry_count ?? "no-retry-count",
    entry.packet_evidence?.encoded_packet_len ?? "no-packet-length",
    entry.packet_evidence?.encoded_packet_sha256 ?? "no-packet-hash",
    entry.ingress_observation?.interface_id ?? "no-ingress-interface",
    entry.ingress_observation?.signal?.rssi_dbm ?? "no-ingress-rssi",
    entry.ingress_observation?.signal?.snr_db ?? "no-ingress-snr",
  ].join(":");
}

/** Stable owner for an ambiguous send-again request during this app session. */
export function retryMessageCacheKey(destination: string, entry: TimelineView): string {
  return [
    destination,
    timelineEntryKey(entry),
    entry.outbox_id ?? "no-outbox",
    entry.message_id ?? "no-message",
  ].join(":");
}

/** User-facing lifecycle label without changing the generated protocol type. */
export function timelineStatusLabel(entry: TimelineView): string {
  if (entry.status === null) return entry.direction === "inbound" ? "Received" : "Unknown";
  if (entry.direction === "outbound" && entry.status === "preparing") {
    return "Pending / retrying on appliance";
  }
  return entry.status
    .split("_")
    .map((word) => `${word[0]?.toUpperCase() ?? ""}${word.slice(1)}`)
    .join(" ");
}

/**
 * Actions that can faithfully round-trip through the app's UTF-8 composer.
 *
 * Explicit replacement is available only for legacy or permanently terminal
 * failures that preserve the original semantic LXMF message. Current
 * board-owned delivery remains `preparing` and needs no app rearm. Explicit
 * downstream rejection is not a lossy-network condition and is not retryable.
 */
export function timelineMessageCapabilities(entry: TimelineView): TimelineMessageCapabilities {
  const composerCompatible = entry.title.encoding === "utf8" && entry.content.encoding === "utf8";
  return {
    canRetry:
      entry.direction === "outbound" &&
      entry.outbox_id !== null &&
      entry.status !== null &&
      REPLACEMENT_RETRYABLE_STATUSES.has(entry.status),
    canUseAsDraft: composerCompatible,
  };
}

/**
 * Build an exact-message replacement request for one existing outbox row.
 *
 * This is not another carrier attempt within the current board-owned delivery
 * loop. The fresh key creates a replacement durable device submission for a
 * legacy or permanently terminal row. Destination, timestamp, title, content,
 * timeline sequence, and LXMF message identity remain owned by the existing
 * outbox row.
 */
export function retryMessageRequest(
  entry: TimelineView,
  idempotencyKey: string,
): RetrySendRequest | null {
  if (!timelineMessageCapabilities(entry).canRetry || entry.outbox_id === null) return null;
  return {
    outbox_id: entry.outbox_id,
    idempotency_key: idempotencyKey,
  };
}
