import type { Feature, FeatureCollection, Geometry, LineString, Point, Position } from "geojson";

import type {
  ContactView,
  ConversationPeerView,
  MessageActivityEventView,
  MessageLocationView,
  PhoneLocationObservationView,
  RadioTraceEventView,
  RadioTraceProfileView,
  TimelineStatus,
  TimelineView,
} from "../generated/api.ts";
import {
  buildMessageActivityAliases,
  messageActivityPeerLabel,
  messageActivityStatusLabel,
} from "./message-activity.ts";

export type TransmissionMapFeatureKind =
  | "attempt"
  | "message-location"
  | "message-reception-link"
  | "observation-segment"
  | "receiver-location";
export type TransmissionMapTone = "danger" | "info" | "muted" | "success" | "warning";

export interface TransmissionMapProperties {
  readonly [key: string]: boolean | number | string | null;
  readonly id: string;
  readonly kind: TransmissionMapFeatureKind;
  readonly label: string;
  readonly timestamp_ms: number;
  readonly tone: TransmissionMapTone;
}

export interface TransmissionMapDetailRow {
  readonly label: string;
  readonly value: string;
}

export interface TransmissionMapFeatureDetails {
  readonly id: string;
  readonly kind: TransmissionMapFeatureKind;
  readonly rows: readonly TransmissionMapDetailRow[];
  readonly subtitle: string;
  readonly timelineSequence: number | null;
  readonly title: string;
  readonly tone: TransmissionMapTone;
}

export interface TransmissionMapBounds {
  readonly east: number;
  readonly north: number;
  readonly south: number;
  readonly west: number;
}

export interface TransmissionMapScene {
  readonly bounds: TransmissionMapBounds | null;
  readonly detailsByFeatureId: Readonly<Record<string, TransmissionMapFeatureDetails>>;
  readonly historyIncomplete: boolean;
  readonly lines: FeatureCollection<LineString, TransmissionMapProperties>;
  readonly points: FeatureCollection<Point, TransmissionMapProperties>;
  readonly summary: {
    readonly attemptCount: number;
    readonly messageLocationCount: number;
    readonly messageReceptionLinkCount: number;
    readonly observationSegmentCount: number;
    readonly receiverLocationCount: number;
  };
}

export interface LocatedTimeline {
  readonly peer: string;
  readonly peerName: string | null;
  readonly timeline: TimelineView;
}

export interface BuildTransmissionMapSceneInput {
  readonly activityHistoryIncomplete?: boolean;
  readonly contacts?: readonly ContactView[];
  readonly conversationPeers?: readonly ConversationPeerView[];
  readonly locatedTimelines?: readonly LocatedTimeline[];
  readonly messageActivityEvents?: readonly MessageActivityEventView[];
  readonly profileKey?: string | null;
  readonly radioTraceEvents?: readonly RadioTraceEventView[];
  readonly radioTraceHistoryIncomplete?: boolean;
}

interface AttemptAggregate {
  attemptNumber: number | null;
  latestActivity: MessageActivityEventView | null;
  location: Extract<PhoneLocationObservationView, { state: "available" }> | null;
  outboxId: number | null;
  peer: string | null;
  radioEvents: RadioTraceEventView[];
  timelineSequence: number;
}

const EMPTY_POINTS: FeatureCollection<Point, TransmissionMapProperties> = {
  type: "FeatureCollection",
  features: [],
};
const EMPTY_LINES: FeatureCollection<LineString, TransmissionMapProperties> = {
  type: "FeatureCollection",
  features: [],
};

const DATE_TIME = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "medium",
});

function safeIdPart(value: string): string {
  return encodeURIComponent(value).replaceAll("%", "_");
}

function attemptGroupKey(
  timelineSequence: number,
  outboxId: number | null,
  attemptNumber: number | null,
): string {
  return `${timelineSequence}:${outboxId ?? "none"}:${attemptNumber ?? "pending"}`;
}

function attemptFeatureId(profileKey: string, attempt: AttemptAggregate): string {
  return `attempt:${safeIdPart(profileKey)}:${attemptGroupKey(
    attempt.timelineSequence,
    attempt.outboxId,
    attempt.attemptNumber,
  )}`;
}

function validMicrodegreeCoordinates(
  location: Pick<MessageLocationView, "latitude_e6" | "longitude_e6">,
): Position | null {
  if (!Number.isFinite(location.latitude_e6) || !Number.isFinite(location.longitude_e6)) {
    return null;
  }
  if (
    location.latitude_e6 < -90_000_000 ||
    location.latitude_e6 > 90_000_000 ||
    location.longitude_e6 < -180_000_000 ||
    location.longitude_e6 > 180_000_000
  ) {
    return null;
  }
  return [location.longitude_e6 / 1_000_000, location.latitude_e6 / 1_000_000];
}

function availablePhoneLocation(
  location: PhoneLocationObservationView | null,
): Extract<PhoneLocationObservationView, { state: "available" }> | null {
  if (location === null || location.state !== "available") return null;
  return validMicrodegreeCoordinates(location) === null ? null : location;
}

function newestActivity(
  current: MessageActivityEventView | null,
  candidate: MessageActivityEventView,
): MessageActivityEventView {
  return current === null || candidate.event_id > current.event_id ? candidate : current;
}

function timelineStatus(activity: MessageActivityEventView | null): {
  label: string;
  status: TimelineStatus | null;
  tone: TransmissionMapTone;
} {
  if (activity === null) return { label: "RF evidence retained", status: null, tone: "info" };
  switch (activity.activity.kind) {
    case "inbound_imported":
      return { label: "Inbound message imported", status: null, tone: "success" };
    case "outbound_queued":
      return { label: "Queued locally", status: "queued", tone: "muted" };
    case "outbound_accepted":
      return { label: "Accepted by appliance", status: "accepted", tone: "info" };
    case "outbound_requeued":
      return {
        label:
          activity.activity.trigger === "automatic"
            ? "Legacy automatic app rearm queued"
            : "Replacement submission queued",
        status: "queued",
        tone: "warning",
      };
    case "outbound_status": {
      const status = activity.activity.status;
      if (status === "delivered") {
        return { label: messageActivityStatusLabel(status), status, tone: "success" };
      }
      if (
        status === "failed_no_path" ||
        status === "failed_delivery_timeout" ||
        status === "failed_downstream_rejection" ||
        status === "failed_internal"
      ) {
        return { label: messageActivityStatusLabel(status), status, tone: "danger" };
      }
      if (status === "cancelled") {
        return { label: messageActivityStatusLabel(status), status, tone: "warning" };
      }
      return {
        label: messageActivityStatusLabel(status),
        status,
        tone: status === "awaiting_delivery" ? "warning" : "info",
      };
    }
  }
}

function traceStatus(
  events: readonly RadioTraceEventView[],
): { label: string; tone: TransmissionMapTone } | null {
  const ordered = [...events].sort((left, right) => right.event_id - left.event_id);
  const terminal = ordered.find((event) => event.event.kind === "attempt_terminal");
  if (terminal?.event.kind === "attempt_terminal") {
    if (terminal.event.outcome === "delivered") {
      return { label: "Delivery proof returned", tone: "success" };
    }
    return {
      label:
        terminal.event.outcome === "delivery_timeout"
          ? "Delivery proof timed out"
          : "Attempt ended before transmission",
      tone: "danger",
    };
  }
  const transmission = ordered.find((event) => event.event.kind === "data_tx");
  if (transmission?.event.kind === "data_tx") {
    return transmission.event.outcome === "transmitted"
      ? { label: "Physical frames reached TxDone", tone: "warning" }
      : {
          label: `Transmission failed: ${transmission.event.outcome.replaceAll("_", " ")}`,
          tone: "danger",
        };
  }
  const route = ordered.find((event) => event.event.kind === "route_selected");
  if (route?.event.kind === "route_selected") {
    const ready =
      route.event.resolution === "exact_ready" || route.event.resolution === "broadcast_ready";
    return {
      label: ready
        ? "Route selected"
        : `Route unavailable: ${route.event.resolution.replaceAll("_", " ")}`,
      tone: ready ? "info" : "danger",
    };
  }
  return null;
}

function formatDate(unixMs: number): string {
  const date = new Date(unixMs);
  return Number.isNaN(date.getTime()) ? "Invalid time" : DATE_TIME.format(date);
}

function coordinateLabel(coordinates: Position): string {
  return `${coordinates[1]?.toFixed(6)}, ${coordinates[0]?.toFixed(6)}`;
}

function accuracyLabel(location: Extract<PhoneLocationObservationView, { state: "available" }>) {
  return location.horizontal_accuracy_mm === null
    ? "Unavailable"
    : `±${(location.horizontal_accuracy_mm / 1_000).toFixed(1)} m`;
}

function messageAccuracyLabel(location: MessageLocationView): string {
  return location.accuracy_cm === 0
    ? "Unavailable"
    : `±${(location.accuracy_cm / 100).toFixed(2)} m`;
}

function messageElevationMeters(location: MessageLocationView): number | null {
  // Sideband reserves zero for unavailable optional sensor fields, so a
  // genuine zero-metre observation cannot be distinguished on the wire.
  return location.altitude_cm === 0 ? null : location.altitude_cm / 100;
}

function receiverElevationMeters(
  location: Extract<PhoneLocationObservationView, { state: "available" }>,
): number | null {
  return location.altitude_mm === null ? null : location.altitude_mm / 1_000;
}

function elevationLabel(
  meters: number | null,
  accuracyMeters: number | null,
  zeroSentinel = false,
): string {
  if (meters === null) {
    return zeroSentinel
      ? "Unavailable (LXMF zero sentinel; exact sea level is ambiguous)"
      : "Unavailable";
  }
  const accuracy = accuracyMeters === null ? "" : ` ±${accuracyMeters.toFixed(1)} m`;
  return `${meters.toFixed(2)} m${accuracy}`;
}

function compactElevationLabel(meters: number | null): string {
  return meters === null ? "?" : `${meters.toFixed(0)} m`;
}

function shortPeer(peer: string): string {
  return peer.length <= 8 ? peer : `…${peer.slice(-8)}`;
}

function profileLabel(profile: RadioTraceProfileView): string {
  return `${(profile.frequency_hz / 1_000_000).toFixed(3)} MHz · SF${profile.spreading_factor} · BW ${(profile.bandwidth_hz / 1_000).toFixed(0)} kHz · CR 4/${profile.coding_rate_denominator} · ${profile.requested_power_dbm >= 0 ? "+" : ""}${profile.requested_power_dbm} dBm`;
}

function attemptDetailRows(
  attempt: AttemptAggregate,
  coordinates: Position,
  statusLabel: string,
): TransmissionMapDetailRow[] {
  const location = attempt.location;
  if (location === null) return [];
  const rows: TransmissionMapDetailRow[] = [
    { label: "Status", value: statusLabel },
    {
      label: "App submission",
      value: attempt.attemptNumber === null ? "Pending submission" : `${attempt.attemptNumber}`,
    },
    { label: "Message row", value: `${attempt.timelineSequence}` },
    { label: "Phone position", value: coordinateLabel(coordinates) },
    { label: "Accuracy", value: accuracyLabel(location) },
    { label: "Captured", value: formatDate(location.captured_at_unix_ms) },
    {
      label: "Position source",
      value: `${location.source.replaceAll("_", " ")} · ${location.authorization} location${location.mocked === true ? " · platform marked mocked" : ""}`,
    },
    {
      label: "Meaning",
      value:
        "Phone position when the app submission was queued; board retries reuse it; not exact RF emission or board GNSS.",
    },
  ];
  if (attempt.outboxId !== null)
    rows.splice(3, 0, { label: "Outbox", value: `${attempt.outboxId}` });

  const route = [...attempt.radioEvents]
    .sort((left, right) => right.event_id - left.event_id)
    .find((event) => event.event.kind === "route_selected");
  if (route?.event.kind === "route_selected") {
    rows.push(
      { label: "Destination", value: route.event.destination },
      {
        label: "Selected route",
        value: `${route.event.hops} hop${route.event.hops === 1 ? "" : "s"} · interface ${route.event.interface_id} · ${route.event.resolution.replaceAll("_", " ")}`,
      },
    );
  }

  const transmission = [...attempt.radioEvents]
    .sort((left, right) => right.event_id - left.event_id)
    .find((event) => event.event.kind === "data_tx");
  if (transmission?.event.kind === "data_tx") {
    rows.push({
      label: "RF transmit",
      value: `${transmission.event.outcome.replaceAll("_", " ")} · ${transmission.event.completed_physical_frames}/${transmission.event.planned_physical_frames} frames completed`,
    });
  }

  const terminal = [...attempt.radioEvents]
    .sort((left, right) => right.event_id - left.event_id)
    .find((event) => event.event.kind === "attempt_terminal");
  if (terminal?.event.kind === "attempt_terminal" && terminal.event.proof_rssi_dbm !== null) {
    rows.push({
      label: "Proof return signal",
      value: `RSSI ${terminal.event.proof_rssi_dbm} dBm${terminal.event.proof_snr_db === null ? "" : ` · SNR ${terminal.event.proof_snr_db} dB`} · final return hop only`,
    });
  }

  const newestRadio = [...attempt.radioEvents].sort(
    (left, right) => right.event_id - left.event_id,
  )[0];
  if (newestRadio !== undefined)
    rows.push({ label: "LoRa profile", value: profileLabel(newestRadio.profile) });
  return rows;
}

function messageLocationRows(
  located: LocatedTimeline,
  coordinates: Position,
): TransmissionMapDetailRow[] {
  const location = located.timeline.location;
  if (location === null) return [];
  const rows: TransmissionMapDetailRow[] = [
    {
      label: "Direction",
      value: located.timeline.direction === "inbound" ? "Received" : "Sent",
    },
    { label: "Peer", value: located.peer },
    { label: "Attached position", value: coordinateLabel(coordinates) },
    {
      label: "Reported accuracy",
      value:
        location.accuracy_cm === 0
          ? "Unavailable"
          : `±${(location.accuracy_cm / 100).toFixed(2)} m`,
    },
    {
      label:
        located.timeline.direction === "inbound"
          ? "Sender phone elevation"
          : "Sending phone elevation",
      value: elevationLabel(messageElevationMeters(location), null, location.altitude_cm === 0),
    },
    { label: "Message time", value: formatDate(located.timeline.timestamp_ms) },
    {
      label: "Meaning",
      value:
        located.timeline.direction === "inbound"
          ? "Location attached by the remote sender to this LXMF message."
          : "Location attached by this phone to the LXMF message; not necessarily every retry position.",
    },
  ];
  if (located.timeline.direction !== "inbound") return rows;

  const ingress = located.timeline.ingress_observation;
  if (ingress === null) {
    rows.push({
      label: "Receiver-local signal",
      value: "No first-arrival interface or signal evidence was retained.",
    });
    return rows;
  }
  rows.push({
    label: "Received via",
    value: `Interface ${ingress.interface_id}`,
  });
  rows.push({
    label: "Receiver-local signal",
    value:
      ingress.signal === null
        ? "This ingress interface did not report RSSI or SNR."
        : `RSSI ${ingress.signal.rssi_dbm} dBm · SNR ${ingress.signal.snr_db} dB`,
  });
  rows.push({
    label: "Signal meaning",
    value:
      "Measured by this appliance on the final hop. On a relayed route, the transmitter may be a relay rather than the original sender.",
  });
  return rows;
}

function inboundActivityMessageRows(
  event: MessageActivityEventView,
  coordinates: Position,
): TransmissionMapDetailRow[] {
  const location = event.message_location;
  if (location === null) return [];
  const rows: TransmissionMapDetailRow[] = [
    { label: "Direction", value: "Received" },
    { label: "Peer", value: event.peer },
    { label: "Attached position", value: coordinateLabel(coordinates) },
    { label: "Reported accuracy", value: messageAccuracyLabel(location) },
    {
      label: "Sender phone elevation",
      value: elevationLabel(messageElevationMeters(location), null, location.altitude_cm === 0),
    },
    {
      label: "Sender location time",
      value: formatDate(location.updated_at_unix_seconds * 1_000),
    },
    {
      label: "Imported locally",
      value:
        event.observed_at_unix_ms === null ? "Unavailable" : formatDate(event.observed_at_unix_ms),
    },
    {
      label: "Meaning",
      value: "Location attached by the remote sender to this LXMF message.",
    },
  ];
  const ingress = event.ingress_observation;
  if (ingress === null) {
    rows.push({
      label: "Receiver-local signal",
      value: "No first-arrival interface or signal evidence was retained.",
    });
    return rows;
  }
  rows.push({ label: "Received via", value: `Interface ${ingress.interface_id}` });
  rows.push({
    label: "Receiver-local signal",
    value:
      ingress.signal === null
        ? "This ingress interface did not report RSSI or SNR."
        : `RSSI ${ingress.signal.rssi_dbm} dBm · SNR ${ingress.signal.snr_db} dB`,
  });
  rows.push({
    label: "Signal meaning",
    value:
      "Measured by this appliance on the final hop. On a relayed route, the transmitter may be a relay rather than the original sender.",
  });
  return rows;
}

function haversineMeters(start: Position, end: Position): number {
  const toRadians = (degrees: number) => (degrees * Math.PI) / 180;
  const startLatitude = toRadians(start[1] ?? 0);
  const endLatitude = toRadians(end[1] ?? 0);
  const latitudeDelta = endLatitude - startLatitude;
  const longitudeDelta = toRadians((end[0] ?? 0) - (start[0] ?? 0));
  const a =
    Math.sin(latitudeDelta / 2) ** 2 +
    Math.cos(startLatitude) * Math.cos(endLatitude) * Math.sin(longitudeDelta / 2) ** 2;
  const bounded = Math.min(1, Math.max(0, a));
  return 6_371_000 * 2 * Math.atan2(Math.sqrt(bounded), Math.sqrt(1 - bounded));
}

function distanceLabel(meters: number): string {
  return meters < 1_000
    ? `${meters.toFixed(0)} m`
    : `${(meters / 1_000).toFixed(2)} km · ${(meters / 1_609.344).toFixed(2)} mi`;
}

function receptionRows(
  event: MessageActivityEventView,
  senderCoordinates: Position,
  receiverCoordinates: Position,
  receiver: Extract<PhoneLocationObservationView, { state: "available" }>,
  horizontalDistanceMeters: number,
): TransmissionMapDetailRow[] {
  const sender = event.message_location;
  if (sender === null) return [];
  const senderElevation = messageElevationMeters(sender);
  const receiverElevation = receiverElevationMeters(receiver);
  const elevationDelta =
    senderElevation === null || receiverElevation === null
      ? null
      : receiverElevation - senderElevation;
  const slantDistance =
    elevationDelta === null ? null : Math.hypot(horizontalDistanceMeters, Math.abs(elevationDelta));
  const ingress = event.ingress_observation;
  const rows: TransmissionMapDetailRow[] = [
    { label: "Message row", value: `${event.timeline_sequence}` },
    { label: "Peer", value: event.peer },
    { label: "Sender phone position", value: coordinateLabel(senderCoordinates) },
    { label: "Sender horizontal accuracy", value: messageAccuracyLabel(sender) },
    {
      label: "Sender phone elevation",
      value: elevationLabel(senderElevation, null, sender.altitude_cm === 0),
    },
    {
      label: "Sender location time",
      value: formatDate(sender.updated_at_unix_seconds * 1_000),
    },
    { label: "Receiver phone position", value: coordinateLabel(receiverCoordinates) },
    { label: "Receiver horizontal accuracy", value: accuracyLabel(receiver) },
    {
      label: "Receiver phone elevation",
      value: elevationLabel(
        receiverElevation,
        receiver.vertical_accuracy_mm === null ? null : receiver.vertical_accuracy_mm / 1_000,
      ),
    },
    { label: "Receiver location captured", value: formatDate(receiver.captured_at_unix_ms) },
    { label: "Horizontal endpoint separation", value: distanceLabel(horizontalDistanceMeters) },
    {
      label: "Elevation difference",
      value:
        elevationDelta === null
          ? "Unavailable"
          : `${elevationDelta.toFixed(2)} m (receiver − sender)`,
    },
    {
      label: "Straight-line endpoint separation",
      value: slantDistance === null ? "Unavailable" : distanceLabel(slantDistance),
    },
  ];
  if (ingress !== null) {
    rows.push({ label: "Received via", value: `Interface ${ingress.interface_id}` });
    rows.push({
      label: "Receiver-local final-hop signal",
      value:
        ingress.signal === null
          ? "This ingress interface did not report RSSI or SNR."
          : `RSSI ${ingress.signal.rssi_dbm} dBm · SNR ${ingress.signal.snr_db} dB`,
    });
  }
  rows.push({
    label: "Meaning",
    value:
      "Phone-to-phone endpoint separation using the sender-attached fix and the receiver phone fix retained when the app imported the message. It is not the board position, exact RF reception position, traveled route, or a measured RF path. On a relayed Reticulum route, final-hop RSSI and SNR describe the relay-to-receiver hop, not this full line.",
  });
  return rows;
}

function sceneBounds(
  points: readonly Feature<Point, TransmissionMapProperties>[],
): TransmissionMapBounds | null {
  if (points.length === 0) return null;
  let west = Number.POSITIVE_INFINITY;
  let east = Number.NEGATIVE_INFINITY;
  let south = Number.POSITIVE_INFINITY;
  let north = Number.NEGATIVE_INFINITY;
  for (const point of points) {
    const [longitude, latitude] = point.geometry.coordinates;
    if (longitude === undefined || latitude === undefined) continue;
    west = Math.min(west, longitude);
    east = Math.max(east, longitude);
    south = Math.min(south, latitude);
    north = Math.max(north, latitude);
  }
  return Number.isFinite(west) ? { east, north, south, west } : null;
}

export function buildTransmissionMapScene({
  activityHistoryIncomplete = false,
  contacts = [],
  conversationPeers = [],
  locatedTimelines = [],
  messageActivityEvents = [],
  profileKey = null,
  radioTraceEvents = [],
  radioTraceHistoryIncomplete = false,
}: BuildTransmissionMapSceneInput): TransmissionMapScene {
  const profile = profileKey ?? "active";
  const aliases = buildMessageActivityAliases(contacts, conversationPeers);
  const attempts = new Map<string, AttemptAggregate>();

  for (const event of messageActivityEvents) {
    if (event.direction !== "outbound") continue;
    const key = attemptGroupKey(event.timeline_sequence, event.outbox_id, event.attempt_number);
    const current = attempts.get(key) ?? {
      attemptNumber: event.attempt_number,
      latestActivity: null,
      location: null,
      outboxId: event.outbox_id,
      peer: event.peer,
      radioEvents: [],
      timelineSequence: event.timeline_sequence,
    };
    current.latestActivity = newestActivity(current.latestActivity, event);
    current.location = availablePhoneLocation(event.attempt_location) ?? current.location;
    current.peer = event.peer || current.peer;
    current.outboxId = event.outbox_id ?? current.outboxId;
    current.attemptNumber = event.attempt_number ?? current.attemptNumber;
    attempts.set(key, current);
  }

  for (const event of radioTraceEvents) {
    const correlation = event.correlation;
    if (correlation === null) continue;
    const key = attemptGroupKey(
      correlation.timeline_sequence,
      correlation.outbox_id,
      correlation.attempt_number,
    );
    const current = attempts.get(key) ?? {
      attemptNumber: correlation.attempt_number,
      latestActivity: null,
      location: null,
      outboxId: correlation.outbox_id,
      peer: null,
      radioEvents: [],
      timelineSequence: correlation.timeline_sequence,
    };
    current.location = availablePhoneLocation(correlation.attempt_location) ?? current.location;
    current.radioEvents.push(event);
    if (event.event.kind === "route_selected") current.peer = event.event.destination;
    attempts.set(key, current);
  }

  const detailsByFeatureId: Record<string, TransmissionMapFeatureDetails> = {};
  const attemptPoints: Feature<Point, TransmissionMapProperties>[] = [];
  for (const attempt of attempts.values()) {
    if (attempt.location === null) continue;
    const coordinates = validMicrodegreeCoordinates(attempt.location);
    if (coordinates === null) continue;
    const activityState = timelineStatus(attempt.latestActivity);
    const radioState = traceStatus(attempt.radioEvents);
    const state =
      activityState.status === null && radioState !== null
        ? radioState
        : activityState.status === "delivered" || activityState.tone === "danger"
          ? activityState
          : (radioState ?? activityState);
    const peer = attempt.peer ?? attempt.latestActivity?.peer ?? "unknown peer";
    const peerLabel =
      attempt.latestActivity === null
        ? (aliases.get(peer) ?? shortPeer(peer))
        : messageActivityPeerLabel(attempt.latestActivity, aliases);
    const id = attemptFeatureId(profile, attempt);
    const attemptLabel = attempt.attemptNumber === null ? "queued" : `#${attempt.attemptNumber}`;
    const properties: TransmissionMapProperties = {
      id,
      kind: "attempt",
      label: `${peerLabel} · ${attemptLabel} · ${state.label}`,
      timestamp_ms: attempt.location.captured_at_unix_ms,
      tone: state.tone,
    };
    attemptPoints.push({
      type: "Feature",
      id,
      geometry: { type: "Point", coordinates },
      properties,
    });
    detailsByFeatureId[id] = {
      id,
      kind: "attempt",
      rows: [
        { label: "Peer", value: `${peerLabel} · ${peer}` },
        ...attemptDetailRows(attempt, coordinates, state.label),
      ],
      subtitle: state.label,
      timelineSequence: attempt.timelineSequence,
      title: `${peerLabel} · attempt ${attempt.attemptNumber ?? "pending"}`,
      tone: state.tone,
    };
  }

  const messagePoints: Feature<Point, TransmissionMapProperties>[] = [];
  const seenTimelines = new Set<number>();
  for (const located of locatedTimelines) {
    const timeline = located.timeline;
    if (timeline.location === null || seenTimelines.has(timeline.sequence)) continue;
    const coordinates = validMicrodegreeCoordinates(timeline.location);
    if (coordinates === null) continue;
    seenTimelines.add(timeline.sequence);
    const peerLabel =
      located.peerName?.trim() || aliases.get(located.peer) || shortPeer(located.peer);
    const id = `message-location:${safeIdPart(profile)}:${timeline.sequence}`;
    const tone: TransmissionMapTone = timeline.direction === "inbound" ? "info" : "muted";
    const ingressSignal =
      timeline.direction === "inbound" ? timeline.ingress_observation?.signal : null;
    messagePoints.push({
      type: "Feature",
      id,
      geometry: { type: "Point", coordinates },
      properties: {
        id,
        kind: "message-location",
        label:
          ingressSignal === null || ingressSignal === undefined
            ? `${peerLabel} · shared`
            : `${peerLabel} · RX ${ingressSignal.rssi_dbm} dBm · SNR ${ingressSignal.snr_db} dB`,
        timestamp_ms: timeline.timestamp_ms,
        tone,
      },
    });
    detailsByFeatureId[id] = {
      id,
      kind: "message-location",
      rows: messageLocationRows(located, coordinates),
      subtitle:
        timeline.direction === "inbound"
          ? "Remote sender-attached LXMF location"
          : "Location attached to an outbound LXMF message",
      timelineSequence: timeline.sequence,
      title: `${peerLabel} · shared location`,
      tone,
    };
  }

  const receiverPoints: Feature<Point, TransmissionMapProperties>[] = [];
  const receptionLines: Feature<LineString, TransmissionMapProperties>[] = [];
  const seenReceptions = new Set<number>();
  for (const event of messageActivityEvents) {
    if (
      event.direction !== "inbound" ||
      event.message_location === null ||
      seenReceptions.has(event.timeline_sequence)
    ) {
      continue;
    }
    const senderCoordinates = validMicrodegreeCoordinates(event.message_location);
    if (senderCoordinates === null) continue;
    seenReceptions.add(event.timeline_sequence);

    const peerLabel = messageActivityPeerLabel(event, aliases);
    const messagePointId = `message-location:${safeIdPart(profile)}:${event.timeline_sequence}`;
    const observedAt =
      event.observed_at_unix_ms ?? event.message_location.updated_at_unix_seconds * 1_000;
    if (!seenTimelines.has(event.timeline_sequence)) {
      const ingressSignal = event.ingress_observation?.signal;
      messagePoints.push({
        type: "Feature",
        id: messagePointId,
        geometry: { type: "Point", coordinates: senderCoordinates },
        properties: {
          id: messagePointId,
          kind: "message-location",
          label:
            ingressSignal === null || ingressSignal === undefined
              ? `${peerLabel} · shared`
              : `${peerLabel} · RX ${ingressSignal.rssi_dbm} dBm · SNR ${ingressSignal.snr_db} dB`,
          timestamp_ms: observedAt,
          tone: "info",
        },
      });
      detailsByFeatureId[messagePointId] = {
        id: messagePointId,
        kind: "message-location",
        rows: inboundActivityMessageRows(event, senderCoordinates),
        subtitle: "Remote sender-attached LXMF location",
        timelineSequence: event.timeline_sequence,
        title: `${peerLabel} · shared location`,
        tone: "info",
      };
      seenTimelines.add(event.timeline_sequence);
    }

    const receiver = availablePhoneLocation(event.receiver_location);
    if (receiver === null) continue;
    const receiverCoordinates = validMicrodegreeCoordinates(receiver);
    if (receiverCoordinates === null) continue;
    const distance = haversineMeters(senderCoordinates, receiverCoordinates);
    const senderElevation = messageElevationMeters(event.message_location);
    const receiverElevation = receiverElevationMeters(receiver);
    const lineLabel = `${distanceLabel(distance)} · ${compactElevationLabel(senderElevation)} → ${compactElevationLabel(receiverElevation)}`;
    const rows = receptionRows(event, senderCoordinates, receiverCoordinates, receiver, distance);
    const receiverId = `receiver-location:${safeIdPart(profile)}:${event.timeline_sequence}`;
    receiverPoints.push({
      type: "Feature",
      id: receiverId,
      geometry: { type: "Point", coordinates: receiverCoordinates },
      properties: {
        id: receiverId,
        kind: "receiver-location",
        label: `Local receiver · ${compactElevationLabel(receiverElevation)}`,
        timestamp_ms: receiver.captured_at_unix_ms,
        tone: "success",
      },
    });
    detailsByFeatureId[receiverId] = {
      id: receiverId,
      kind: "receiver-location",
      rows,
      subtitle: "Receiver phone fix retained at local import",
      timelineSequence: event.timeline_sequence,
      title: `${peerLabel} · receiver endpoint`,
      tone: "success",
    };

    const linkId = `message-reception-link:${safeIdPart(profile)}:${event.timeline_sequence}`;
    receptionLines.push({
      type: "Feature",
      id: linkId,
      geometry: {
        type: "LineString",
        coordinates: [senderCoordinates, receiverCoordinates],
      },
      properties: {
        id: linkId,
        kind: "message-reception-link",
        label: lineLabel,
        timestamp_ms: observedAt,
        tone: "success",
      },
    });
    detailsByFeatureId[linkId] = {
      id: linkId,
      kind: "message-reception-link",
      rows,
      subtitle: "Phone-to-phone endpoint separation for one received LXMF message",
      timelineSequence: event.timeline_sequence,
      title: `${peerLabel} · reception · ${distanceLabel(distance)}`,
      tone: "success",
    };
  }

  attemptPoints.sort(
    (left, right) =>
      left.properties.timestamp_ms - right.properties.timestamp_ms ||
      left.properties.id.localeCompare(right.properties.id),
  );
  messagePoints.sort(
    (left, right) =>
      left.properties.timestamp_ms - right.properties.timestamp_ms ||
      left.properties.id.localeCompare(right.properties.id),
  );
  receiverPoints.sort(
    (left, right) =>
      left.properties.timestamp_ms - right.properties.timestamp_ms ||
      left.properties.id.localeCompare(right.properties.id),
  );

  const observationLines: Feature<LineString, TransmissionMapProperties>[] = [];
  for (let index = 1; index < attemptPoints.length; index += 1) {
    const start = attemptPoints[index - 1];
    const end = attemptPoints[index];
    if (start === undefined || end === undefined) continue;
    if (
      start.geometry.coordinates[0] === end.geometry.coordinates[0] &&
      start.geometry.coordinates[1] === end.geometry.coordinates[1]
    ) {
      continue;
    }
    const distance = haversineMeters(start.geometry.coordinates, end.geometry.coordinates);
    const id = `observation-segment:${safeIdPart(profile)}:${start.properties.id}:${end.properties.id}`;
    observationLines.push({
      type: "Feature",
      id,
      geometry: {
        type: "LineString",
        coordinates: [start.geometry.coordinates, end.geometry.coordinates],
      },
      properties: {
        id,
        kind: "observation-segment",
        label: distanceLabel(distance),
        timestamp_ms: end.properties.timestamp_ms,
        tone: "info",
      },
    });
    detailsByFeatureId[id] = {
      id,
      kind: "observation-segment",
      rows: [
        { label: "From", value: start.properties.label },
        { label: "To", value: end.properties.label },
        { label: "Geographic separation", value: distanceLabel(distance) },
        { label: "Start sample", value: formatDate(start.properties.timestamp_ms) },
        { label: "End sample", value: formatDate(end.properties.timestamp_ms) },
        {
          label: "Meaning",
          value:
            "Chronological connection between two phone queue-position observations; not an RF path, route, or traveled track.",
        },
      ],
      subtitle: "Chronological field-test observation sequence",
      timelineSequence: null,
      title: `Observation separation · ${distanceLabel(distance)}`,
      tone: "info",
    };
  }

  const points = [...attemptPoints, ...messagePoints, ...receiverPoints];
  const lines = [...observationLines, ...receptionLines];
  return {
    bounds: sceneBounds(points),
    detailsByFeatureId,
    historyIncomplete: activityHistoryIncomplete || radioTraceHistoryIncomplete,
    lines: { type: "FeatureCollection", features: lines },
    points: { type: "FeatureCollection", features: points },
    summary: {
      attemptCount: attemptPoints.length,
      messageLocationCount: messagePoints.length,
      messageReceptionLinkCount: receptionLines.length,
      observationSegmentCount: observationLines.length,
      receiverLocationCount: receiverPoints.length,
    },
  };
}

export function emptyTransmissionMapScene(): TransmissionMapScene {
  return {
    bounds: null,
    detailsByFeatureId: {},
    historyIncomplete: false,
    lines: EMPTY_LINES,
    points: EMPTY_POINTS,
    summary: {
      attemptCount: 0,
      messageLocationCount: 0,
      messageReceptionLinkCount: 0,
      observationSegmentCount: 0,
      receiverLocationCount: 0,
    },
  };
}

export function selectedTransmissionMapFeatures(
  scene: TransmissionMapScene,
  selectedFeatureId: string | null,
): FeatureCollection<Geometry, TransmissionMapProperties> {
  if (selectedFeatureId === null) return { type: "FeatureCollection", features: [] };
  const feature = [...scene.points.features, ...scene.lines.features].find(
    (candidate) => candidate.properties.id === selectedFeatureId,
  );
  return {
    type: "FeatureCollection",
    features: feature === undefined ? [] : [feature],
  };
}

export function transmissionMapViewport(scene: TransmissionMapScene): {
  readonly center: readonly [number, number];
  readonly zoom: number;
} {
  const bounds = scene.bounds;
  if (bounds === null) return { center: [0, 20], zoom: 1.5 };
  const longitudeSpan = Math.max(0.000_1, bounds.east - bounds.west);
  const latitudeSpan = Math.max(0.000_1, bounds.north - bounds.south);
  const span = Math.max(longitudeSpan, latitudeSpan * 1.7);
  return {
    center: [(bounds.west + bounds.east) / 2, (bounds.south + bounds.north) / 2],
    zoom: Math.max(1.5, Math.min(16, Math.log2(360 / span) - 1.2)),
  };
}
