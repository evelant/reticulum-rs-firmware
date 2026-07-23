import type {
  BleCentral,
  BleConnection,
  BleConnectOptions,
  BleGattProfile,
} from "./ble-central-types.ts";

export type {
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleDisconnectEvent,
  BleGattProfile,
} from "./ble-central-types.ts";

class UnsupportedWebBleCentral implements BleCentral {
  readonly supported = false;

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
