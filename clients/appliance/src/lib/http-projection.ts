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
        transport: "usb_serial",
        endpoint: connection.port,
        device_label: connection.usb_serial,
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

/**
 * Adapt the frozen HTTP-v1 USB field names to the transport-neutral model used
 * by the universal application and native Rust bridge.
 */
export function applianceSnapshotFromHttp(snapshot: HttpApplianceSnapshot): ApplianceSnapshot {
  return {
    ...snapshot,
    connection: connectionFromHttp(snapshot.connection),
  };
}
