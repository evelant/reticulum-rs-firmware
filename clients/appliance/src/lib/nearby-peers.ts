import { MAX_CONTACT_NAME_BYTES, type NearbyPeerView } from "../generated/api.ts";
import { utf8ByteLength } from "./limits.ts";

export type { NearbyPeerView } from "../generated/api.ts";

export interface NearbyInterfaceSummary {
  /** Product-owned observing-interface slot. */
  readonly interfaceId: number;
  /** Firmware-projected label, when the slot is known to this product. */
  readonly interfaceName: string | null;
  /** Distinct authenticated peers most recently observed on this interface. */
  readonly peerCount: number;
  /** Observed peers that are not present in the app's contact store. */
  readonly unaddedPeerCount: number;
  /** Peers whose latest announce was received directly (one Reticulum hop). */
  readonly directPeerCount: number;
  /** Freshest retained observation age for this interface. */
  readonly freshestObservedAgeMs: number;
}

export interface NearbyNetworkSummary {
  readonly peerCount: number;
  readonly unaddedPeerCount: number;
  readonly interfaceCount: number;
  readonly interfaces: readonly NearbyInterfaceSummary[];
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

/**
 * Project the authenticated nearby-announcement view into compact,
 * transport-neutral network state.
 *
 * This deliberately reports observed peers and observing interfaces, not a
 * route-table size or interface-health claim. The current device API exposes
 * neither an enumerable route table nor an authoritative interface registry.
 */
export function nearbyNetworkSummary(
  peers: readonly NearbyPeerView[],
  contacts: readonly { readonly destination: string }[],
  elapsedSinceFetchMs = 0,
): NearbyNetworkSummary {
  const contactsByDestination = new Set(
    contacts.map((contact) => normalizedDestination(contact.destination)),
  );
  const interfaces = new Map<number, NearbyInterfaceSummary>();
  let unaddedPeerCount = 0;

  for (const peer of peers) {
    const unadded = !contactsByDestination.has(normalizedDestination(peer.destination));
    if (unadded) unaddedPeerCount += 1;

    const existing = interfaces.get(peer.interface_id);
    const interfaceName = peer.interface_name?.trim() || existing?.interfaceName || null;
    const observedAge = advancedObservationAge(peer.observed_age_ms, elapsedSinceFetchMs);
    interfaces.set(peer.interface_id, {
      interfaceId: peer.interface_id,
      interfaceName,
      peerCount: (existing?.peerCount ?? 0) + 1,
      unaddedPeerCount: (existing?.unaddedPeerCount ?? 0) + Number(unadded),
      directPeerCount: (existing?.directPeerCount ?? 0) + Number(peer.hops === 1),
      freshestObservedAgeMs:
        existing === undefined
          ? observedAge
          : Math.min(existing.freshestObservedAgeMs, observedAge),
    });
  }

  const observedInterfaces = [...interfaces.values()].sort(
    (left, right) => left.interfaceId - right.interfaceId,
  );
  return {
    peerCount: peers.length,
    unaddedPeerCount,
    interfaceCount: observedInterfaces.length,
    interfaces: observedInterfaces,
  };
}

export function nearbyInterfaceLabel(summary: NearbyInterfaceSummary): string {
  const name = summary.interfaceName?.trim();
  return name === undefined || name.length === 0
    ? `Interface ${summary.interfaceId}`
    : `${name} · interface ${summary.interfaceId}`;
}

export function nearbyInterfaceSummaryHint(summary: NearbyInterfaceSummary): string {
  const peerLabel = summary.peerCount === 1 ? "peer" : "peers";
  const directLabel = summary.directPeerCount === 1 ? "direct peer" : "direct peers";
  return [
    `${summary.peerCount} ${peerLabel}`,
    `${summary.unaddedPeerCount} not in contacts`,
    `${summary.directPeerCount} ${directLabel}`,
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

export function nearbyPeerRouteHint(peer: NearbyPeerView, elapsedSinceFetchMs = 0): string {
  const interfaceName = peer.interface_name?.trim();
  const parts = [
    peer.hops === 1 ? "direct" : `${peer.hops} hops`,
    interfaceName === undefined || interfaceName.length === 0
      ? `interface ${peer.interface_id}`
      : interfaceName,
    `announced ${nearbyPeerAge(advancedObservationAge(peer.observed_age_ms, elapsedSinceFetchMs))}`,
  ];
  if (peer.rssi_dbm !== null) parts.push(`RX ${peer.rssi_dbm} dBm`);
  if (peer.snr_db !== null) parts.push(`SNR ${peer.snr_db} dB`);
  return parts.join(" · ");
}
