import { MAX_CONTACT_NAME_BYTES, type NearbyPeerView } from "../generated/api.ts";
import { utf8ByteLength } from "./limits.ts";

export type { NearbyPeerView } from "../generated/api.ts";

function normalizedFingerprint(peer: NearbyPeerView): string {
  const identity = peer.identity_hash.trim().toLowerCase();
  if (/^[0-9a-f]{32}$/.test(identity)) return identity;
  return peer.destination.trim().toLowerCase();
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

export function nearbyPeerAge(ageMs: number): string {
  const safeAge = Number.isFinite(ageMs) ? Math.max(0, Math.floor(ageMs)) : 0;
  if (safeAge < 5_000) return "just now";
  if (safeAge < 60_000) return `${Math.floor(safeAge / 1_000)}s ago`;
  if (safeAge < 3_600_000) return `${Math.floor(safeAge / 60_000)}m ago`;
  if (safeAge < 86_400_000) return `${Math.floor(safeAge / 3_600_000)}h ago`;
  return `${Math.floor(safeAge / 86_400_000)}d ago`;
}

export function nearbyPeerRouteHint(peer: NearbyPeerView): string {
  const interfaceName = peer.interface_name?.trim();
  const parts = [
    peer.hops === 0 ? "direct" : `${peer.hops} ${peer.hops === 1 ? "hop" : "hops"}`,
    interfaceName === undefined || interfaceName.length === 0
      ? `interface ${peer.interface_id}`
      : interfaceName,
    nearbyPeerAge(peer.observed_age_ms),
  ];
  if (peer.rssi_dbm !== null) parts.push(`${peer.rssi_dbm} dBm`);
  if (peer.snr_db !== null) parts.push(`${peer.snr_db} dB SNR`);
  return parts.join(" · ");
}
