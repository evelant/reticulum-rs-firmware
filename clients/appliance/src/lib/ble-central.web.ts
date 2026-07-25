import type {
  BleCandidate,
  BleCentral,
  BleConnection,
  BleConnectOptions,
  BleGattProfile,
  BleScanOptions,
} from "./ble-central-types.ts";

export type {
  BleCandidate,
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleDisconnectEvent,
  BleGattProfile,
  BleScanOptions,
} from "./ble-central-types.ts";

class UnsupportedWebBleCentral implements BleCentral {
  readonly supported = false;

  scan(_serviceUuid: string, _options?: BleScanOptions): Promise<readonly BleCandidate[]> {
    return Promise.reject(
      new Error(
        "The appliance BLE central is available only in iOS and Android development builds",
      ),
    );
  }

  connect(_profile: BleGattProfile, _options?: BleConnectOptions): Promise<BleConnection> {
    return Promise.reject(
      new Error(
        "The appliance BLE central is available only in iOS and Android development builds",
      ),
    );
  }

  dispose(): Promise<void> {
    return Promise.resolve();
  }
}

export function createBleCentral(): BleCentral {
  return new UnsupportedWebBleCentral();
}
