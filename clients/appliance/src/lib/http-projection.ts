import type {
  ApplianceSnapshot,
  ConnectionState,
  HttpApplianceSnapshot,
  HttpConnectionState,
} from "../generated/api.ts";

function connectionFromHttp(connection: HttpConnectionState): ConnectionState {
  switch (connection.state) {
    case "ready":
      return {
        state: "ready",
        transport: connection.transport,
        endpoint: connection.endpoint,
        device_label: connection.device_label,
      };
    case "starting":
    case "disconnected":
    case "connecting":
    case "backoff":
    case "faulted":
    case "stopped":
      return { state: connection.state };
  }
}

/** Adapt the HTTP snapshot to the universal application's connection model. */
export function applianceSnapshotFromHttp(snapshot: HttpApplianceSnapshot): ApplianceSnapshot {
  return {
    ...snapshot,
    connection: connectionFromHttp(snapshot.connection),
  };
}
