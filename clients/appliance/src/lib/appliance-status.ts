import type { ApplianceSnapshot, ConnectionState, ConnectionTransport } from "../generated/api.ts";

export type ApplianceStatusTone = "neutral" | "ready" | "faulted";

export interface ApplianceStatusPresentation {
  readonly boardLabel: string;
  readonly connectionLabel: string;
  readonly contactCountLabel: string;
  readonly deviceId: string | null;
  readonly endpoint: string | null;
  readonly importedThisRunLabel: string;
  readonly lxmfDestination: string | null;
  readonly pendingOutboxLabel: string;
  readonly primaryDestination: string | null;
  readonly tone: ApplianceStatusTone;
}

const E290_DEVICE_API_ID_PREFIX_HEX = "653239302d6170692d31";

function nonempty(value: string | null | undefined): string | null {
  const normalized = value?.trim() ?? "";
  return normalized.length === 0 ? null : normalized;
}

/** Friendly label for the PRNS-owned connection network. */
export function connectionTransportLabel(_transport: ConnectionTransport): string {
  return "Reticulum";
}

/** Compact lifecycle wording for surfaces that do not show connection metadata. */
export function connectionStateLabel(connection: ConnectionState | undefined): string {
  switch (connection?.state) {
    case undefined:
    case "starting":
      return "Starting";
    case "disconnected":
      return "Disconnected";
    case "connecting":
      return "Connecting";
    case "unavailable":
      return `${connectionTransportLabel(connection.transport)} unavailable`;
    case "ready":
      return "Ready";
    case "backoff":
      return "Waiting to reconnect";
    case "faulted":
      return "Connection fault";
    case "stopped":
      return "Stopped";
  }
}

/**
 * Format an EUI-48 or namespaced E290 device-API identifier without changing
 * identifiers from other device families.
 */
export function formatDeviceId(value: string): string {
  const normalized = value.trim();
  const compact = normalized.replaceAll(":", "").replaceAll("-", "");
  const eui48 = /^[0-9a-f]{12}$/i.test(compact)
    ? compact
    : compact.length === 32 &&
        compact.toLowerCase().startsWith(E290_DEVICE_API_ID_PREFIX_HEX) &&
        /^[0-9a-f]{32}$/i.test(compact)
      ? compact.slice(E290_DEVICE_API_ID_PREFIX_HEX.length)
      : null;
  if (eui48 !== null) {
    return eui48.toUpperCase().match(/.{2}/g)?.join(":") ?? normalized;
  }
  return normalized;
}

function countLabel(count: number, singular: string, plural: string): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

/** Derive truthful user-facing appliance status exclusively from the public snapshot. */
export function applianceStatusPresentation(
  snapshot: ApplianceSnapshot | null,
): ApplianceStatusPresentation {
  const connection = snapshot?.connection;
  const readyConnection = connection?.state === "ready" ? connection : null;
  const deviceId = nonempty(snapshot?.device?.device_id);
  const deviceLabel = nonempty(readyConnection?.device_label);
  const boardIdentity = deviceId ?? deviceLabel;

  let connectionLabel = connectionStateLabel(connection);
  let tone: ApplianceStatusTone = "neutral";
  if (readyConnection !== null) {
    connectionLabel = `Connected through ${connectionTransportLabel(readyConnection.transport)}`;
    tone = "ready";
  } else if (connection?.state === "faulted" || connection?.state === "stopped") {
    tone = "faulted";
  }

  const pendingOutbox = snapshot?.pending_outbox ?? 0;
  const importedThisRun = snapshot?.imported_this_run ?? 0;
  return {
    boardLabel: boardIdentity === null ? "Appliance" : formatDeviceId(boardIdentity),
    connectionLabel,
    contactCountLabel: countLabel(snapshot?.contact_count ?? 0, "contact", "contacts"),
    deviceId,
    endpoint: nonempty(readyConnection?.endpoint),
    importedThisRunLabel: `${importedThisRun} imported since app start`,
    lxmfDestination: nonempty(snapshot?.device?.lxmf_delivery_destination),
    pendingOutboxLabel: `${pendingOutbox} outbound pending`,
    primaryDestination: nonempty(snapshot?.device?.primary_destination),
    tone,
  };
}
