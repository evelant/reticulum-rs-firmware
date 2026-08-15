import type {
  MessageLocationView,
  SendRequest,
  SendResponse,
  TimelineView,
} from "../generated/api.ts";

/**
 * Exact user-visible fields retained after the local SQLite outbox accepts a
 * submission but before a follow-up timeline read has projected that row.
 */
export interface LocalMessageAcceptance {
  readonly content: string;
  readonly destination: string;
  readonly location: MessageLocationView | null;
  readonly outboxId: number;
  readonly timestampMs: number;
  readonly title: string;
}

/** Project the successful send response without inventing a timeline sequence. */
export function localMessageAcceptance(
  request: SendRequest,
  response: SendResponse,
): LocalMessageAcceptance {
  return {
    content: request.content,
    destination: request.destination,
    location: request.location ?? null,
    outboxId: response.outbox_id,
    timestampMs: request.timestamp_ms,
    title: request.title,
  };
}

/**
 * Retain at most one local presentation per durable outbox row. Replayed
 * idempotent submissions update the same presentation instead of duplicating
 * the message.
 */
export function recordLocalMessageAcceptance(
  current: readonly LocalMessageAcceptance[],
  acceptance: LocalMessageAcceptance,
): readonly LocalMessageAcceptance[] {
  const existing = current.findIndex((candidate) => candidate.outboxId === acceptance.outboxId);
  if (existing < 0) return [...current, acceptance];
  if (current[existing] === acceptance) return current;
  return current.map((candidate, index) => (index === existing ? acceptance : candidate));
}

/** Hide local placeholders as soon as the authoritative timeline includes them. */
export function unreconciledLocalMessageAcceptances(
  current: readonly LocalMessageAcceptance[],
  timeline: readonly TimelineView[],
  destination: string | null,
): readonly LocalMessageAcceptance[] {
  if (current.length === 0) return current;
  const scoped =
    destination === null
      ? []
      : current.filter((acceptance) => acceptance.destination === destination);
  if (scoped.length === 0 || timeline.length === 0) {
    return scoped.length === current.length ? current : scoped;
  }
  const projectedOutboxIds = new Set(
    timeline.flatMap((entry) => (entry.outbox_id === null ? [] : [entry.outbox_id])),
  );
  const remaining = scoped.filter((acceptance) => !projectedOutboxIds.has(acceptance.outboxId));
  return remaining.length === current.length ? current : remaining;
}
