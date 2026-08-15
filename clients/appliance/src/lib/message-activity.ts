import type {
  MessageActivityEventView,
  PhoneLocationObservationView,
  TimelineDirection,
  TimelineStatus,
} from "../generated/api.ts";

export type MessageActivityFilter = "all" | TimelineDirection | "attention";

export interface MessageActivityAliasSource {
  readonly destination: string;
  readonly name: string | null;
}

export interface MessageActivityPresentation {
  readonly metadata: readonly string[];
  readonly observedAt: string;
  readonly peerLabel: string;
  readonly title: string;
  readonly tone: "danger" | "muted" | "normal" | "success" | "warning";
}

export const MESSAGE_ACTIVITY_FILTERS = [
  { label: "All", value: "all" },
  { label: "Inbound", value: "inbound" },
  { label: "Outbound", value: "outbound" },
  { label: "Failures & replacement retries", value: "attention" },
] as const satisfies readonly {
  readonly label: string;
  readonly value: MessageActivityFilter;
}[];

const OBSERVED_AT_FORMAT = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "medium",
});

const LOCATION_CAPTURE_FORMAT = new Intl.DateTimeFormat(undefined, {
  dateStyle: "short",
  timeStyle: "medium",
});

const FAILED_STATUSES = new Set<TimelineStatus>([
  "failed_no_path",
  "failed_delivery_timeout",
  "failed_downstream_rejection",
  "failed_internal",
]);

export function messageActivityStatusLabel(status: TimelineStatus): string {
  switch (status) {
    case "committed":
      return "Saved locally";
    case "accepted":
      return "Accepted by appliance";
    case "queued":
      return "Queued on appliance";
    case "preparing":
      return "Pending / retrying on appliance";
    case "awaiting_delivery":
      return "Awaiting delivery proof";
    case "delivered":
      return "Delivered";
    case "failed_no_path":
      return "Failed: no path";
    case "failed_delivery_timeout":
      return "Failed: delivery timed out";
    case "failed_downstream_rejection":
      return "Failed: downstream rejected";
    case "failed_internal":
      return "Failed: internal error";
    case "cancelled":
      return "Cancelled";
  }
}

/**
 * Build a local address-book view without changing peer trust. Conversation
 * history supplies aliases first; explicit contacts win when both are present.
 */
export function buildMessageActivityAliases(
  contacts: readonly MessageActivityAliasSource[],
  conversationPeers: readonly MessageActivityAliasSource[],
): ReadonlyMap<string, string> {
  const aliases = new Map<string, string>();
  for (const peer of conversationPeers) {
    const name = peer.name?.trim();
    if (name) aliases.set(peer.destination, name);
  }
  for (const contact of contacts) {
    const name = contact.name?.trim();
    if (name) aliases.set(contact.destination, name);
  }
  return aliases;
}

export function messageActivityPeerLabel(
  event: Pick<MessageActivityEventView, "direction" | "peer">,
  aliases: ReadonlyMap<string, string>,
): string {
  const alias = aliases.get(event.peer);
  if (alias !== undefined) return alias;
  const fingerprint = event.peer.length <= 6 ? event.peer : `…${event.peer.slice(-6)}`;
  return event.direction === "inbound" ? `Unknown sender ${fingerprint}` : `Peer ${fingerprint}`;
}

/**
 * This is deliberately an observation-time label. It must not imply that the
 * timestamp is an RF transmit time or the remote peer's delivery time.
 */
export function messageActivityObservedAtLabel(observedAtUnixMs: number | null): string {
  if (observedAtUnixMs === null) return "Observed by app · time unavailable";
  const observed = new Date(observedAtUnixMs);
  if (Number.isNaN(observed.getTime())) return "Observed by app · invalid time";
  return `Observed by app · ${OBSERVED_AT_FORMAT.format(observed)}`;
}

export function isMessageActivityAttention(event: MessageActivityEventView): boolean {
  if (event.activity.kind === "outbound_requeued") return true;
  if (event.activity.kind !== "outbound_status") return false;
  return FAILED_STATUSES.has(event.activity.status) || event.activity.status === "cancelled";
}

function compactSampleAge(milliseconds: number): string {
  if (milliseconds < 0) return "phone clock ahead";
  if (milliseconds < 1_000) return "under 1s old";
  const seconds = Math.floor(milliseconds / 1_000);
  if (seconds < 60) return `${seconds}s old`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m old`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h old`;
}

/** Private phone-position metadata retained with one app-created submission. */
export function phoneLocationMetadata(
  location: PhoneLocationObservationView | null,
  observedAtUnixMs: number | null,
): string[] {
  if (location === null) return [];
  if (location.state === "unavailable") {
    return [`Phone location unavailable · ${location.reason.replaceAll("_", " ")}`];
  }

  const latitude = (location.latitude_e6 / 1_000_000).toFixed(6);
  const longitude = (location.longitude_e6 / 1_000_000).toFixed(6);
  const accuracy =
    location.horizontal_accuracy_mm === null
      ? "accuracy unknown"
      : `±${(location.horizontal_accuracy_mm / 1_000).toFixed(1)} m`;
  const captured = new Date(location.captured_at_unix_ms);
  const capturedLabel = Number.isNaN(captured.getTime())
    ? "invalid capture time"
    : LOCATION_CAPTURE_FORMAT.format(captured);
  const sampleAge =
    observedAtUnixMs === null
      ? "sample age unknown"
      : compactSampleAge(observedAtUnixMs - location.captured_at_unix_ms);
  const authorization =
    location.authorization === "precise" ? "precise grant" : `${location.authorization} grant`;
  const source = location.source.replaceAll("_", " ");
  const mocked = location.mocked === true ? " · platform marked mocked" : "";
  return [
    `Phone location ${latitude}, ${longitude} · ${accuracy}`,
    `Captured ${capturedLabel} · ${sampleAge} · ${authorization} · ${source}${mocked}`,
    "Phone position when app submission queued; board retries reuse it",
  ];
}

/** Private phone-position metadata retained by an app submission-begin event. */
export function messageActivityLocationMetadata(event: MessageActivityEventView): string[] {
  return phoneLocationMetadata(event.attempt_location, event.observed_at_unix_ms);
}

/** Receiver-local first-arrival evidence associated with an inbound message. */
export function messageActivityIngressMetadata(event: MessageActivityEventView): string[] {
  const ingress = event.ingress_observation;
  if (ingress === null) return [];
  if (ingress.signal === null) {
    return [`Receiver-local final hop · interface ${ingress.interface_id} · no RF signal values`];
  }
  return [
    `Receiver-local final hop · interface ${ingress.interface_id} · RSSI ${ingress.signal.rssi_dbm} dBm · SNR ${ingress.signal.snr_db} dB`,
  ];
}

export function messageActivityPresentation(
  event: MessageActivityEventView,
  aliases: ReadonlyMap<string, string>,
): MessageActivityPresentation {
  const metadata = [
    `${event.direction === "inbound" ? "Inbound" : "Outbound"} · event ${event.event_id} · message row ${event.timeline_sequence}`,
  ];
  if (event.outbox_id !== null) metadata.push(`Outbox ${event.outbox_id}`);
  if (event.attempt_number !== null) metadata.push(`App submission ${event.attempt_number}`);

  let title: string;
  let tone: MessageActivityPresentation["tone"] = "normal";
  switch (event.activity.kind) {
    case "inbound_imported":
      title = "Inbound message imported";
      tone = "success";
      metadata.push(`Message ID ${event.activity.message_id}`);
      break;
    case "outbound_queued":
      title = "Outbound message saved locally";
      tone = "muted";
      break;
    case "outbound_accepted":
      title = "Appliance accepted outbound submission";
      metadata.push(`Submission ${event.activity.submission_id}`);
      metadata.push(`Message ID ${event.activity.message_id}`);
      break;
    case "outbound_status": {
      title = messageActivityStatusLabel(event.activity.status);
      if (event.activity.status === "delivered") tone = "success";
      else if (FAILED_STATUSES.has(event.activity.status)) tone = "danger";
      else if (event.activity.status === "cancelled") tone = "warning";
      const evidence = event.activity.packet_evidence;
      if (evidence !== null) {
        metadata.push(`Encoded packet ${evidence.encoded_packet_len} bytes`);
        metadata.push(`Packet SHA-256 ${evidence.encoded_packet_sha256}`);
      }
      break;
    }
    case "outbound_requeued":
      title = "Replacement submission queued";
      tone = "warning";
      break;
  }
  metadata.push(...messageActivityIngressMetadata(event));
  metadata.push(...messageActivityLocationMetadata(event));

  return {
    metadata,
    observedAt: messageActivityObservedAtLabel(event.observed_at_unix_ms),
    peerLabel: messageActivityPeerLabel(event, aliases),
    title,
    tone,
  };
}

export function sortMessageActivityNewestFirst(
  events: readonly MessageActivityEventView[],
): MessageActivityEventView[] {
  return [...events].sort((left, right) => right.event_id - left.event_id);
}

export function filterMessageActivity(
  events: readonly MessageActivityEventView[],
  filter: MessageActivityFilter,
  query: string,
  aliases: ReadonlyMap<string, string>,
): MessageActivityEventView[] {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  return sortMessageActivityNewestFirst(events).filter((event) => {
    if (filter === "inbound" && event.direction !== "inbound") return false;
    if (filter === "outbound" && event.direction !== "outbound") return false;
    if (filter === "attention" && !isMessageActivityAttention(event)) return false;
    if (normalizedQuery.length === 0) return true;

    const presentation = messageActivityPresentation(event, aliases);
    return [
      presentation.title,
      presentation.peerLabel,
      presentation.observedAt,
      event.peer,
      ...presentation.metadata,
    ]
      .join(" ")
      .toLocaleLowerCase()
      .includes(normalizedQuery);
  });
}
