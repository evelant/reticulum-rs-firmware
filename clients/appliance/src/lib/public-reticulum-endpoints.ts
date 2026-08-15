import type { ReticulumTcpPeerView } from "../generated/api.ts";

export interface PublicReticulumTcpEndpoint {
  readonly id: string;
  readonly label: string;
  readonly hostname: string;
  readonly port: number;
  /**
   * The transport identity advertised by the endpoint when it was verified.
   *
   * This is diagnostic metadata, not a cryptographic pin: public Reticulum
   * operators may legitimately rotate their transport identity.
   */
  readonly expectedTransportId: string;
  readonly sourceUrl: string;
  readonly verifiedOn: `${number}-${number}-${number}`;
}

/**
 * Small, deliberately curated bootstrap list for the appliance's one active
 * outbound TCP client. Hostnames are retained so firmware can resolve them on
 * every reconnect instead of persisting a stale address.
 */
export const PUBLIC_RETICULUM_TCP_ENDPOINTS = [
  {
    id: "rmap-world",
    label: "RMAP World",
    hostname: "rmap.world",
    port: 4242,
    expectedTransportId: "682e34edf6dd0daa867831ebc9b4e204",
    sourceUrl: "https://rmap.world/info.html",
    verifiedOn: "2026-07-26",
  },
  {
    id: "reticulumnet-nl",
    label: "ReticulumNet.nl",
    hostname: "node.reticulumnet.nl",
    port: 4242,
    expectedTransportId: "8a2c0d3c3fee8bea4a8172dc6f4d7ea6",
    sourceUrl: "https://www.reticulumnet.nl/en/get-started/",
    verifiedOn: "2026-07-26",
  },
  {
    id: "mcswain-dev",
    label: "McSwain Reticulum",
    hostname: "reticulum.mcswain.dev",
    port: 4242,
    expectedTransportId: "72d389bca0703e185155f2d2c3eace57",
    sourceUrl: "https://rmap.world/?json=1",
    verifiedOn: "2026-07-26",
  },
] as const satisfies readonly PublicReticulumTcpEndpoint[];

export type PublicReticulumEndpointId = (typeof PUBLIC_RETICULUM_TCP_ENDPOINTS)[number]["id"];

export function publicReticulumEndpoint(
  id: PublicReticulumEndpointId,
): (typeof PUBLIC_RETICULUM_TCP_ENDPOINTS)[number] {
  const endpoint = PUBLIC_RETICULUM_TCP_ENDPOINTS.find((candidate) => candidate.id === id);
  if (endpoint === undefined) {
    // `id` is closed over the catalog at compile time. Keep a runtime guard for
    // untyped persistence or navigation data crossing into the app.
    throw new Error(`Unknown public Reticulum endpoint: ${id}`);
  }
  return endpoint;
}

/** Whether an enabled hostname peer exactly selects this catalog entry. */
export function isPublicReticulumEndpointSelected(
  peer: ReticulumTcpPeerView | null,
  endpoint: PublicReticulumTcpEndpoint,
): boolean {
  return (
    peer?.enabled === true &&
    "hostname" in peer &&
    peer.hostname.toLowerCase() === endpoint.hostname &&
    peer.port === endpoint.port
  );
}
