import { MAX_CONTACT_NAME_BYTES, type NearbyPeerView } from "../generated/api.ts";
import { utf8ByteLength } from "./limits.ts";
import { reticulumInterfaceIdHex } from "./reticulum-interface-id.ts";

export type { NearbyPeerView } from "../generated/api.ts";

export interface NearbyContactSummary {
  /** Canonical authenticated LXMF destination. */
  readonly destination: string;
  /** Best current metadata for contact naming and actions. */
  readonly representative: NearbyPeerView;
  /** Node-scoped observations of this destination. */
  readonly observations: readonly NearbyPeerView[];
}

export interface NearbyObserverSummary {
  readonly observerKind: NearbyPeerView["observer_kind"];
  readonly observerManagementDestination: string | null;
  /** Distinct authenticated LXMF destinations observed by this node. */
  readonly peerCount: number;
  /** Retained node/interface observations represented by the summary. */
  readonly observationCount: number;
  /** Human-readable interface labels, still scoped to this observer. */
  readonly interfaceLabels: readonly string[];
  readonly freshestObservedAgeMs: number;
}

export interface NearbyNetworkSummary {
  /** Distinct authenticated LXMF destinations, not observation-row count. */
  readonly peerCount: number;
  readonly observationCount: number;
  readonly unaddedPeerCount: number;
  readonly observerCount: number;
  readonly observers: readonly NearbyObserverSummary[];
}

/** Bounded cadence used only while the Nearby surface is visible and connected. */
export const NEARBY_FOREGROUND_POLL_INTERVAL_MS = 10_000;

function normalizedFingerprint(peer: NearbyPeerView): string {
  const identity = peer.identity_hash.trim().toLowerCase();
  if (/^[0-9a-f]{32}$/.test(identity)) return identity;
  return peer.destination.trim().toLowerCase();
}

function normalizedDestination(destination: string): string {
  return destination.trim().toLowerCase();
}

function observerKind(peer: NearbyPeerView): NearbyPeerView["observer_kind"] {
  return peer.observer_kind === "appliance" ? "appliance" : "phone";
}

function observerDestination(peer: NearbyPeerView): string | null {
  const destination = peer.observer_management_destination?.trim().toLowerCase() ?? "";
  return /^[0-9a-f]{32}$/.test(destination) ? destination : null;
}

function observerKey(peer: NearbyPeerView): string {
  return `${observerKind(peer)}\u0000${observerDestination(peer) ?? ""}`;
}

function observationKey(peer: NearbyPeerView): string {
  return `${observerKey(peer)}\u0000${reticulumInterfaceIdHex(peer.interface_id)}`;
}

function advancedObservationAge(ageMs: number, elapsedSinceFetchMs: number): number {
  const age = Number.isFinite(ageMs) ? Math.max(0, Math.floor(ageMs)) : 0;
  const elapsed = Number.isFinite(elapsedSinceFetchMs)
    ? Math.max(0, Math.floor(elapsedSinceFetchMs))
    : 0;
  return Math.min(Number.MAX_SAFE_INTEGER, age + elapsed);
}

/** Elapsed wall time to add to device-reported ages from one fetched snapshot. */
export function nearbySnapshotElapsedMs(fetchedAtMs: number | null, nowMs: number): number {
  if (fetchedAtMs === null || !Number.isFinite(fetchedAtMs) || !Number.isFinite(nowMs)) return 0;
  return Math.max(0, Math.floor(nowMs - fetchedAtMs));
}

/** Group node-scoped observations without merging their route authority. */
export function nearbyContacts(peers: readonly NearbyPeerView[]): NearbyContactSummary[] {
  const grouped = new Map<
    string,
    { representative: NearbyPeerView; observations: Map<string, NearbyPeerView> }
  >();
  for (const peer of peers) {
    const destination = normalizedDestination(peer.destination);
    if (!/^[0-9a-f]{32}$/.test(destination)) continue;
    const existing = grouped.get(destination);
    if (existing === undefined) {
      grouped.set(destination, {
        representative: peer,
        observations: new Map([[observationKey(peer), peer]]),
      });
      continue;
    }
    const key = observationKey(peer);
    const retained = existing.observations.get(key);
    if (retained === undefined || peer.observed_age_ms < retained.observed_age_ms) {
      existing.observations.set(key, peer);
    }
    const retainedHasName = existing.representative.display_name !== null;
    const candidateHasName = peer.display_name !== null;
    if (
      (candidateHasName && !retainedHasName) ||
      (candidateHasName === retainedHasName &&
        peer.observed_age_ms < existing.representative.observed_age_ms)
    ) {
      existing.representative = peer;
    }
  }
  return [...grouped.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([destination, group]) => ({
      destination,
      representative: group.representative,
      observations: [...group.observations.values()].sort((left, right) => {
        const observerOrder = observerKey(left).localeCompare(observerKey(right));
        return observerOrder === 0
          ? reticulumInterfaceIdHex(left.interface_id).localeCompare(
              reticulumInterfaceIdHex(right.interface_id),
            )
          : observerOrder;
      }),
    }));
}

/** Compact counts of contacts and the Reticulum nodes that observed them. */
export function nearbyNetworkSummary(
  peers: readonly NearbyPeerView[],
  contacts: readonly { readonly destination: string }[],
  elapsedSinceFetchMs = 0,
): NearbyNetworkSummary {
  const contactsByDestination = new Set(
    contacts.map((contact) => normalizedDestination(contact.destination)),
  );
  const groupedContacts = nearbyContacts(peers);
  const observers = new Map<
    string,
    {
      observerKind: NearbyPeerView["observer_kind"];
      observerManagementDestination: string | null;
      peers: Set<string>;
      observationCount: number;
      interfaceLabels: Set<string>;
      freshestObservedAgeMs: number;
    }
  >();
  let unaddedPeerCount = 0;

  for (const contact of groupedContacts) {
    if (!contactsByDestination.has(contact.destination)) unaddedPeerCount += 1;
    for (const peer of contact.observations) {
      const key = observerKey(peer);
      const existing = observers.get(key);
      const observedAge = advancedObservationAge(peer.observed_age_ms, elapsedSinceFetchMs);
      const interfaceLabel =
        peer.interface_name?.trim() || `Interface ${reticulumInterfaceIdHex(peer.interface_id)}`;
      if (existing === undefined) {
        observers.set(key, {
          observerKind: observerKind(peer),
          observerManagementDestination: observerDestination(peer),
          peers: new Set([contact.destination]),
          observationCount: 1,
          interfaceLabels: new Set([interfaceLabel]),
          freshestObservedAgeMs: observedAge,
        });
      } else {
        existing.peers.add(contact.destination);
        existing.observationCount += 1;
        existing.interfaceLabels.add(interfaceLabel);
        existing.freshestObservedAgeMs = Math.min(existing.freshestObservedAgeMs, observedAge);
      }
    }
  }

  const observedNodes = [...observers.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([, observer]) => ({
      observerKind: observer.observerKind,
      observerManagementDestination: observer.observerManagementDestination,
      peerCount: observer.peers.size,
      observationCount: observer.observationCount,
      interfaceLabels: [...observer.interfaceLabels].sort(),
      freshestObservedAgeMs: observer.freshestObservedAgeMs,
    }));
  return {
    peerCount: groupedContacts.length,
    observationCount: groupedContacts.reduce(
      (count, contact) => count + contact.observations.length,
      0,
    ),
    unaddedPeerCount,
    observerCount: observedNodes.length,
    observers: observedNodes,
  };
}

export function nearbyObserverLabel(
  observer: Pick<NearbyObserverSummary, "observerKind" | "observerManagementDestination">,
  applianceLabel: string | null,
): string {
  if (observer.observerKind === "phone") return "This phone";
  const label = applianceLabel?.trim();
  if (label !== undefined && label.length > 0) return label;
  const destination = observer.observerManagementDestination;
  return destination === null
    ? "Connected appliance"
    : `Appliance ${destination.slice(0, 4)} ${destination.slice(4, 8)}`;
}

export function nearbyObserverSummaryHint(summary: NearbyObserverSummary): string {
  const peerLabel = summary.peerCount === 1 ? "peer" : "peers";
  const observationLabel = summary.observationCount === 1 ? "observation" : "observations";
  return [
    `${summary.peerCount} ${peerLabel}`,
    `${summary.observationCount} ${observationLabel}`,
    summary.interfaceLabels.join(", "),
    `last announce ${nearbyPeerAge(summary.freshestObservedAgeMs)}`,
  ].join(" · ");
}

/** A compact public fingerprint for recognition, never authentication. */
export function nearbyPeerFingerprint(peer: NearbyPeerView): string {
  const compact = normalizedFingerprint(peer).slice(0, 12).padEnd(12, "0");
  return `${compact.slice(0, 4)} ${compact.slice(4, 8)} ${compact.slice(8, 12)}`;
}

/**
 * Contact name proposed by a one-tap add.
 *
 * Rust bounds and decodes the announce display name. This final UI guard keeps
 * the proposed value inside the existing durable contact-name contract.
 */
export function nearbyPeerSuggestedName(peer: NearbyPeerView): string {
  const announcedName = peer.display_name?.trim() ?? "";
  if (announcedName.length > 0 && utf8ByteLength(announcedName) <= MAX_CONTACT_NAME_BYTES) {
    return announcedName;
  }
  return `Peer ${nearbyPeerFingerprint(peer)}`;
}

/**
 * Returns the distinct Nomad node destination authenticated alongside an LXMF
 * announce. A contact destination alone is intentionally insufficient: the two
 * application destinations cannot be substituted for or derived from each
 * other without the announcing identity.
 */
export function associatedNomadDestinationForLxmf(
  peers: readonly NearbyPeerView[],
  lxmfDestination: string,
): string | null {
  const normalizedLxmf = lxmfDestination.trim().toLowerCase();
  if (!/^[0-9a-f]{32}$/.test(normalizedLxmf)) return null;

  for (const peer of peers) {
    if (peer.destination.trim().toLowerCase() !== normalizedLxmf) continue;
    const nomadDestination = peer.associated_nomad_destination.trim().toLowerCase();
    if (/^[0-9a-f]{32}$/.test(nomadDestination) && nomadDestination !== normalizedLxmf) {
      return nomadDestination;
    }
  }
  return null;
}

export function nearbyPeerAge(ageMs: number): string {
  const safeAge = Number.isFinite(ageMs) ? Math.max(0, Math.floor(ageMs)) : 0;
  if (safeAge < 5_000) return "just now";
  if (safeAge < 60_000) return `${Math.floor(safeAge / 1_000)}s ago`;
  if (safeAge < 3_600_000) return `${Math.floor(safeAge / 60_000)}m ago`;
  if (safeAge < 86_400_000) return `${Math.floor(safeAge / 3_600_000)}h ago`;
  return `${Math.floor(safeAge / 86_400_000)}d ago`;
}

export function nearbyPeerObservationHint(
  peer: NearbyPeerView,
  applianceLabel: string | null,
  elapsedSinceFetchMs = 0,
): string {
  const interfaceName = peer.interface_name?.trim();
  const observer = nearbyObserverLabel(
    {
      observerKind: observerKind(peer),
      observerManagementDestination: observerDestination(peer),
    },
    applianceLabel,
  );
  const parts = [
    observer,
    `${peer.hops} ${peer.hops === 1 ? "hop" : "hops"}`,
    interfaceName === undefined || interfaceName.length === 0
      ? `interface ${reticulumInterfaceIdHex(peer.interface_id)}`
      : interfaceName,
    `announced ${nearbyPeerAge(advancedObservationAge(peer.observed_age_ms, elapsedSinceFetchMs))}`,
  ];
  return parts.join(" · ");
}
