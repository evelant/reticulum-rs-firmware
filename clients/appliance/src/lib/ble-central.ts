import { PermissionsAndroid, Platform } from "react-native";
import BleManager, {
  type BleDisconnectPeripheralEvent,
  type BleManagerDidUpdateValueForCharacteristicEvent,
  BleState,
  type Peripheral,
  type PeripheralInfo,
} from "react-native-ble-manager";

import {
  type BleCentralDriver,
  type BleDiscoveredPeripheral,
  type BleDriverDisconnectEvent,
  type BleDriverIndicationEvent,
  type BleGattDiscovery,
  ForegroundBleCentral,
  MINIMUM_WRITE_WITH_RESPONSE_BYTES,
} from "./ble-central-core.ts";
import type { BleCentral } from "./ble-central-types.ts";

export type {
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleDisconnectEvent,
  BleGattProfile,
} from "./ble-central-types.ts";

const PREFERRED_ANDROID_ATT_MTU = 247;

let managerStarted: Promise<void> | undefined;

async function startManager(): Promise<void> {
  if (managerStarted === undefined) {
    managerStarted = (async () => {
      if (!(await BleManager.isStarted())) {
        await BleManager.start({ showAlert: true });
      }
    })().catch((error: unknown) => {
      managerStarted = undefined;
      throw error;
    });
  }
  await managerStarted;
}

function androidApiLevel(): number {
  return typeof Platform.Version === "number"
    ? Platform.Version
    : Number.parseInt(String(Platform.Version), 10);
}

async function requireAndroidPermissions(): Promise<void> {
  if (Platform.OS !== "android") return;

  const apiLevel = androidApiLevel();
  const permissions =
    apiLevel >= 31
      ? [
          PermissionsAndroid.PERMISSIONS.BLUETOOTH_SCAN,
          PermissionsAndroid.PERMISSIONS.BLUETOOTH_CONNECT,
        ]
      : apiLevel >= 23
        ? [PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION]
        : [];
  if (permissions.length === 0) return;

  const results = await PermissionsAndroid.requestMultiple(permissions);
  const denied = permissions.filter(
    (permission) => results[permission] !== PermissionsAndroid.RESULTS.GRANTED,
  );
  if (denied.length > 0) {
    throw new Error(`Bluetooth permissions were denied: ${denied.join(", ")}`);
  }
}

async function requirePoweredBluetooth(): Promise<void> {
  let state = await BleManager.checkState();
  if (state === BleState.Off && Platform.OS === "android") {
    await BleManager.enableBluetooth();
    state = await BleManager.checkState();
  }
  if (state !== BleState.On) {
    throw new Error(`Bluetooth is not ready (${state})`);
  }
}

function peripheralInfoDiscovery(info: PeripheralInfo): BleGattDiscovery {
  return {
    serviceUuids: [
      ...(info.serviceUUIDs ?? []),
      ...(info.services ?? []).map((service) => service.uuid),
    ],
    characteristics: (info.characteristics ?? []).map((characteristic) => ({
      serviceUuid: characteristic.service,
      characteristicUuid: characteristic.characteristic,
      canWriteWithResponse: characteristic.properties.Write === "Write",
      canIndicate: characteristic.properties.Indicate === "Indicate",
    })),
  };
}

class ReactNativeBleManagerDriver implements BleCentralDriver {
  async prepare(): Promise<void> {
    await requireAndroidPermissions();
    await startManager();
    await requirePoweredBluetooth();
  }

  onDiscovered(listener: (peripheral: BleDiscoveredPeripheral) => void): () => void {
    const subscription = BleManager.onDiscoverPeripheral((peripheral: Peripheral) => {
      listener({ id: peripheral.id, name: peripheral.name ?? peripheral.advertising.localName });
    });
    return () => subscription.remove();
  }

  onDisconnected(listener: (event: BleDriverDisconnectEvent) => void): () => void {
    const subscription = BleManager.onDisconnectPeripheral(
      (event: BleDisconnectPeripheralEvent) => {
        listener({
          peripheralId: event.peripheral,
          status: event.status,
          domain: event.domain,
          code: event.code,
        });
      },
    );
    return () => subscription.remove();
  }

  onIndication(listener: (event: BleDriverIndicationEvent) => void): () => void {
    const subscription = BleManager.onDidUpdateValueForCharacteristic(
      (event: BleManagerDidUpdateValueForCharacteristicEvent) => {
        listener({
          peripheralId: event.peripheral,
          serviceUuid: event.service,
          characteristicUuid: event.characteristic,
          bytes: Uint8Array.from(event.value),
        });
      },
    );
    return () => subscription.remove();
  }

  async startScan(serviceUuid: string): Promise<void> {
    await BleManager.scan({
      serviceUUIDs: [serviceUuid],
      seconds: 0,
      allowDuplicates: false,
    });
  }

  async stopScan(): Promise<void> {
    await BleManager.stopScan();
  }

  async connect(peripheralId: string): Promise<void> {
    await BleManager.connect(peripheralId, { autoconnect: false });
  }

  async discover(peripheralId: string, serviceUuid: string): Promise<BleGattDiscovery> {
    const info = await BleManager.retrieveServices(peripheralId, [serviceUuid]);
    return peripheralInfoDiscovery(info);
  }

  async startIndications(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void> {
    await BleManager.startNotification(peripheralId, serviceUuid, characteristicUuid);
  }

  async stopIndications(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void> {
    await BleManager.stopNotification(peripheralId, serviceUuid, characteristicUuid);
  }

  async maximumWriteWithResponseBytes(peripheralId: string): Promise<number> {
    try {
      if (Platform.OS === "ios") {
        return await BleManager.getMaximumWriteValueLengthForWithResponse(peripheralId);
      }
      if (Platform.OS === "android" && androidApiLevel() >= 21) {
        const mtu = await BleManager.requestMTU(peripheralId, PREFERRED_ANDROID_ATT_MTU);
        return mtu - 3;
      }
    } catch {
      // The default ATT MTU always leaves a 20-byte attribute payload.
    }
    return MINIMUM_WRITE_WITH_RESPONSE_BYTES;
  }

  async writeWithResponse(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
    chunk: Uint8Array,
    maximumChunkBytes: number,
  ): Promise<void> {
    await BleManager.write(
      peripheralId,
      serviceUuid,
      characteristicUuid,
      Array.from(chunk),
      maximumChunkBytes,
    );
  }

  async disconnect(peripheralId: string): Promise<void> {
    await BleManager.disconnect(peripheralId);
  }
}

export function createBleCentral(): BleCentral {
  return new ForegroundBleCentral(new ReactNativeBleManagerDriver());
}
