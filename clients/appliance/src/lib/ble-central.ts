import { PermissionsAndroid, Platform } from "react-native";
import BleManager, {
  type BleDisconnectPeripheralEvent,
  type BleManagerDidUpdateValueForCharacteristicEvent,
  BleState,
  type Peripheral,
  type PeripheralInfo,
} from "react-native-ble-manager";

import {
  advertisedPeripheralName,
  type BleCentralDriver,
  type BleDiscoveredPeripheral,
  type BleDriverDisconnectEvent,
  type BleDriverIndicationEvent,
  type BleGattDiscovery,
  conservativeSingleWriteBytes,
  ForegroundBleCentral,
  MINIMUM_WRITE_WITH_RESPONSE_BYTES,
} from "./ble-central-core.ts";
import type { BleCentral } from "./ble-central-types.ts";

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

const PREFERRED_ANDROID_ATT_MTU = 251;

let managerStarted: Promise<void> | undefined;

function disconnectDescription(event: BleDisconnectPeripheralEvent): string | undefined {
  // react-native-ble-manager 12.5 emits CoreBluetooth's localized description
  // on iOS even though its public TypeScript event omits that runtime field.
  const description = (
    event as BleDisconnectPeripheralEvent & {
      readonly description?: unknown;
    }
  ).description;
  return typeof description === "string" && description.trim().length > 0
    ? description.trim()
    : undefined;
}

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
      canRead: characteristic.properties.Read === "Read",
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

  async connectedPeripherals(serviceUuid: string): Promise<readonly BleDiscoveredPeripheral[]> {
    let peripherals = await BleManager.getConnectedPeripherals([serviceUuid]);
    if (Platform.OS === "ios" && peripherals.length === 0) {
      // react-native-ble-manager 12.5 inserts CoreBluetooth peripherals first
      // retrieved from another native owner into its cache without returning
      // them from that call. A second lookup returns the now-retained entries.
      peripherals = await BleManager.getConnectedPeripherals([serviceUuid]);
    }
    return peripherals.map((peripheral) => ({
      id: peripheral.id,
      name: advertisedPeripheralName(peripheral.advertising?.localName, peripheral.name),
      rssi: peripheral.rssi,
    }));
  }

  onDiscovered(listener: (peripheral: BleDiscoveredPeripheral) => void): () => void {
    const subscription = BleManager.onDiscoverPeripheral((peripheral: Peripheral) => {
      listener({
        id: peripheral.id,
        name: advertisedPeripheralName(peripheral.advertising?.localName, peripheral.name),
        rssi: peripheral.rssi,
      });
    });
    return () => subscription.remove();
  }

  onDisconnected(listener: (event: BleDriverDisconnectEvent) => void): () => void {
    const subscription = BleManager.onDisconnectPeripheral(
      (event: BleDisconnectPeripheralEvent) => {
        listener({
          peripheralId: event.peripheral,
          status: event.status,
          description: disconnectDescription(event),
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

  async startScan(serviceUuid: string, allowDuplicates: boolean): Promise<void> {
    await BleManager.scan({
      serviceUUIDs: [serviceUuid],
      seconds: 0,
      allowDuplicates,
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
        const [withResponse, withoutResponse] = await Promise.all([
          BleManager.getMaximumWriteValueLengthForWithResponse(peripheralId),
          BleManager.getMaximumWriteValueLengthForWithoutResponse(peripheralId),
        ]);
        return conservativeSingleWriteBytes([withResponse, withoutResponse]);
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

  async read(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<Uint8Array> {
    return Uint8Array.from(await BleManager.read(peripheralId, serviceUuid, characteristicUuid));
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
