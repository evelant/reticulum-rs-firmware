import { describe, expect, test } from "bun:test";

import type {
  NativeApplianceError,
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBridgeContract,
  NativeCredentialSummary,
  NativeTransport,
} from "@reticulum/appliance-native";

import {
  DEVICE_API_VERSION_MAJOR,
  DEVICE_API_VERSION_MINOR,
  MAX_LXMF_BASIC_CONTENT_BYTES,
  MAX_LXMF_BASIC_TITLE_BYTES,
  MAX_LXMF_READ_CHUNK_BYTES,
  MAX_MESSAGE_BYTES,
} from "../generated/api.ts";
import type { BleCentral, BleConnectOptions } from "./ble-central-types.ts";
import {
  bleGattProfileFromNative,
  cleanupPickerOwnedCredential,
  NativeApplianceClient,
  type NativeApplianceRuntime,
  type NativeCredentialPicker,
  type NativeCredentialState,
  normalizeBlePeripheralName,
} from "./native-appliance-client.ts";
import { NATIVE_BRIDGE_API_MAJOR, NATIVE_BRIDGE_API_MINOR } from "./native-contract.ts";

const CONTRACT: NativeBridgeContract = {
  bridgeApiMajor: NATIVE_BRIDGE_API_MAJOR,
  bridgeApiMinor: NATIVE_BRIDGE_API_MINOR,
  deviceApiMajor: DEVICE_API_VERSION_MAJOR,
  deviceApiMinor: DEVICE_API_VERSION_MINOR,
  maxMessageBytes: MAX_MESSAGE_BYTES,
  maxLxmfReadChunkBytes: MAX_LXMF_READ_CHUNK_BYTES,
  maxLxmfBasicTitleBytes: MAX_LXMF_BASIC_TITLE_BYTES,
  maxLxmfBasicContentBytes: MAX_LXMF_BASIC_CONTENT_BYTES,
};

const E290_CREDENTIAL: NativeCredentialSummary = {
  credentialId: "cd".repeat(16),
  deviceId: "653239302d6170692d31e13e88",
  expectedBleLocalName: "reticulum-e290-e13e88",
  generation: 1n,
};

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  const deadline = performance.now() + 1_000;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await Bun.sleep(1);
  }
}

function offlineBleAppliance(): NativeApplianceLike {
  return {
    bleDisconnected(): void {},
    bleIngestIndication(): void {},
    bleLinkConnected(): bigint {
      return 1n;
    },
    async bleNextPlatformCommand(): Promise<undefined> {
      return undefined;
    },
    bleWriteFailed(): void {},
    bleWriteSucceeded(): void {},
    async close(): Promise<void> {},
    async contactsJson(): Promise<string> {
      return "[]";
    },
    credentialStatus(): never {
      throw new Error("test bridge owns credential projection");
    },
    async ensureConnected(): Promise<void> {},
    importActivatedCredential(): never {
      throw new Error("test bridge owns credential import");
    },
    async nearbyPeersJson(): Promise<string> {
      return "[]";
    },
    async reconnect(): Promise<void> {},
    async sendMessageJson(): Promise<string> {
      return JSON.stringify({ outbox_id: 1, outcome: "inserted" });
    },
    snapshotJson(): string {
      return JSON.stringify({
        revision: 0,
        connection: { state: "disconnected" },
        device: null,
        pending_outbox: 0,
        contact_count: 0,
        imported_this_run: 0,
        last_error: null,
      });
    },
    async syncNow(): Promise<void> {},
    async timelineJson(): Promise<string> {
      return "[]";
    },
    transport(): NativeTransport {
      return 2 as NativeTransport;
    },
    async upsertContactJson(): Promise<string> {
      return JSON.stringify({ outcome: "unchanged" });
    },
  };
}

function credentialRuntime(options: {
  readonly central: BleCentral;
  readonly importCredential: (stagingPath: string) => NativeCredentialSummary;
  readonly isNativeError?: (value: unknown) => value is NativeApplianceError;
  readonly peripheralName?: string | null;
  readonly pickCredential?: NativeCredentialPicker;
  readonly status: () => NativeCredentialState;
}): NativeApplianceRuntime {
  const appliance = offlineBleAppliance();
  return {
    ble: {
      central: options.central,
      decodeCommand: () => {
        throw new Error("no command expected");
      },
      // A wrong diagnostic fallback makes it observable that the
      // credential-derived E290 name takes precedence.
      peripheralName:
        options.peripheralName === null
          ? undefined
          : (options.peripheralName ?? "reticulum-e290-wrong"),
      profile: {
        serviceUuid: "generated-service",
        writeCharacteristicUuid: "generated-rx",
        indicateCharacteristicUuid: "generated-tx",
        maximumWriteValueBytes: 20,
      },
    },
    bridge: {
      contract: CONTRACT,
      credentialStatus: options.status,
      destroy(): void {},
      isNativeError: options.isNativeError ?? ((_value): _value is NativeApplianceError => false),
      importCredential(_appliance, stagingPath): NativeCredentialSummary {
        return options.importCredential(stagingPath);
      },
      open(): NativeApplianceLike {
        return appliance;
      },
    },
    databasePath: "/app/credential-onboarding.sqlite3",
    pickCredential: options.pickCredential,
  };
}

describe("native appliance adapter loading", () => {
  test("normalizes an optional exact BLE advertised name", () => {
    expect(normalizeBlePeripheralName(undefined)).toBeUndefined();
    expect(normalizeBlePeripheralName("")).toBeUndefined();
    expect(normalizeBlePeripheralName("   ")).toBeUndefined();
    expect(normalizeBlePeripheralName("  reticulum-e290-e13e88  ")).toBe("reticulum-e290-e13e88");
    expect(normalizeBlePeripheralName("RETICULUM-E290-E13E88")).toBe("RETICULUM-E290-E13E88");
  });

  test("deletes Expo's iOS picker copy without deleting an Android provider source", () => {
    let iosDeletes = 0;
    let androidDeletes = 0;
    cleanupPickerOwnedCredential("ios", {
      exists: true,
      delete: () => {
        iosDeletes += 1;
      },
    });
    cleanupPickerOwnedCredential("android", {
      exists: true,
      delete: () => {
        androidDeletes += 1;
      },
    });

    expect(iosDeletes).toBe(1);
    expect(androidDeletes).toBe(0);
  });

  test("constructs without eagerly requiring the custom TurboModule", () => {
    const client = new NativeApplianceClient();
    expect(client).toBeInstanceOf(NativeApplianceClient);
    client.dispose();
  });

  test("forwards generated DTO JSON and closes before destroying its native owner", async () => {
    const events: string[] = [];
    const requests: string[] = [];
    let finishDestroy = () => {};
    const destroyed = new Promise<void>((resolve) => {
      finishDestroy = resolve;
    });
    const bridgeError = {
      tag: "TransportUnavailable",
      inner: { transport: 0, reason: "USB serial adapter is unavailable" },
    } as unknown as NativeApplianceError;
    const appliance: NativeApplianceLike = {
      bleDisconnected(): void {},
      bleIngestIndication(): void {},
      bleLinkConnected(): bigint {
        return 1n;
      },
      async bleNextPlatformCommand(): Promise<undefined> {
        return undefined;
      },
      bleWriteFailed(): void {},
      bleWriteSucceeded(): void {},
      async close(): Promise<void> {
        events.push("close");
      },
      credentialStatus(): never {
        throw new Error("test bridge owns credential projection");
      },
      async contactsJson(): Promise<string> {
        return JSON.stringify([{ destination: "ab".repeat(16), name: "Field node" }]);
      },
      async ensureConnected(): Promise<void> {},
      importActivatedCredential(): NativeCredentialSummary {
        return E290_CREDENTIAL;
      },
      async nearbyPeersJson(): Promise<string> {
        return JSON.stringify([
          {
            destination: "bc".repeat(16),
            display_name: "Ridge relay",
            hops: 1,
            identity_hash: "cd".repeat(16),
            interface_id: 0,
            interface_name: "LoRa",
            observed_age_ms: 125,
            rssi_dbm: -91,
            snr_db: 7,
          },
        ]);
      },
      async reconnect(): Promise<void> {
        throw bridgeError;
      },
      async sendMessageJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({ outbox_id: 7, outcome: "inserted" });
      },
      snapshotJson(): string {
        return JSON.stringify({
          revision: 1,
          connection: { state: "unavailable", transport: "usb_serial" },
          device: null,
          pending_outbox: 0,
          contact_count: 1,
          imported_this_run: 0,
          last_error: "USB serial adapter is unavailable",
        });
      },
      async syncNow(): Promise<void> {},
      async timelineJson(): Promise<string> {
        return "[]";
      },
      transport(): NativeTransport {
        return 0 as NativeTransport;
      },
      async upsertContactJson(_destination, requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({ outcome: "inserted" });
      },
    };
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(value): void {
          expect(value).toBe(appliance);
          events.push("destroy");
          finishDestroy();
        },
        isNativeError: (value): value is NativeApplianceError => value === bridgeError,
        importCredential: () => E290_CREDENTIAL,
        open(databasePath): NativeApplianceLike {
          events.push(`open ${databasePath}`);
          return appliance;
        },
      },
      databasePath: "/app/reticulum-lxmf-chat-alpha-schema3.sqlite3",
    };
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    expect((await client.snapshot()).connection).toEqual({
      state: "unavailable",
      transport: "usb_serial",
    });
    expect(await client.contacts()).toEqual([{ destination: "ab".repeat(16), name: "Field node" }]);
    expect(await client.nearbyPeers()).toEqual([
      {
        destination: "bc".repeat(16),
        display_name: "Ridge relay",
        hops: 1,
        identity_hash: "cd".repeat(16),
        interface_id: 0,
        interface_name: "LoRa",
        observed_age_ms: 125,
        rssi_dbm: -91,
        snr_db: 7,
      },
    ]);
    expect(await client.upsertContact("ab".repeat(16), { name: "Updated field node" })).toEqual({
      outcome: "inserted",
    });
    expect(
      await client.send({
        destination: "ab".repeat(16),
        timestamp_ms: 1,
        idempotency_key: "cd".repeat(16),
        title: "",
        content: "hello",
      }),
    ).toEqual({ outbox_id: 7, outcome: "inserted" });
    expect(requests.map((request) => JSON.parse(request))).toEqual([
      { name: "Updated field node" },
      {
        destination: "ab".repeat(16),
        timestamp_ms: 1,
        idempotency_key: "cd".repeat(16),
        title: "",
        content: "hello",
      },
    ]);
    await expect(client.reconnect()).rejects.toThrow(
      "Native appliance transport unavailable: USB serial adapter is unavailable",
    );

    client.dispose();
    await destroyed;
    expect(events).toEqual([
      "open /app/reticulum-lxmf-chat-alpha-schema3.sqlite3",
      "close",
      "destroy",
    ]);
  });

  test("maps the generated native GATT profile without duplicating or swapping values", () => {
    const generated: NativeBleGattProfile = {
      major: 7,
      minor: 8,
      serviceUuid: "generated-service",
      rxUuid: "generated-rx",
      txUuid: "generated-tx",
      initialAttValueBytes: 41,
    };

    expect(bleGattProfileFromNative(generated)).toEqual({
      serviceUuid: "generated-service",
      writeCharacteristicUuid: "generated-rx",
      indicateCharacteristicUuid: "generated-tx",
      maximumWriteValueBytes: 41,
    });
  });

  test("keeps the durable client available while the background BLE attempt fails", async () => {
    let connectCount = 0;
    let disposeCount = 0;
    let destroyed = false;
    const central: BleCentral = {
      supported: true,
      async connect(): Promise<never> {
        connectCount += 1;
        throw new Error("no nearby appliance");
      },
      async dispose(): Promise<void> {
        disposeCount += 1;
      },
    };
    const appliance: NativeApplianceLike = {
      bleDisconnected(): void {},
      bleIngestIndication(): void {},
      bleLinkConnected(): bigint {
        return 1n;
      },
      async bleNextPlatformCommand(): Promise<undefined> {
        return undefined;
      },
      bleWriteFailed(): void {},
      bleWriteSucceeded(): void {},
      async close(): Promise<void> {},
      credentialStatus(): never {
        throw new Error("test bridge owns credential projection");
      },
      async contactsJson(): Promise<string> {
        return JSON.stringify([{ destination: "ef".repeat(16), name: "Offline contact" }]);
      },
      async ensureConnected(): Promise<void> {},
      importActivatedCredential(): NativeCredentialSummary {
        return E290_CREDENTIAL;
      },
      async nearbyPeersJson(): Promise<string> {
        return "[]";
      },
      async reconnect(): Promise<void> {},
      async sendMessageJson(): Promise<string> {
        return JSON.stringify({ outbox_id: 1, outcome: "inserted" });
      },
      snapshotJson(): string {
        return JSON.stringify({
          revision: 0,
          connection: { state: "disconnected" },
          device: null,
          pending_outbox: 0,
          contact_count: 1,
          imported_this_run: 0,
          last_error: null,
        });
      },
      async syncNow(): Promise<void> {},
      async timelineJson(): Promise<string> {
        return "[]";
      },
      transport(): NativeTransport {
        return 2 as NativeTransport;
      },
      async upsertContactJson(): Promise<string> {
        return JSON.stringify({ outcome: "unchanged" });
      },
    };
    const runtime: NativeApplianceRuntime = {
      ble: {
        central,
        decodeCommand: () => {
          throw new Error("no command expected");
        },
        profile: {
          serviceUuid: "generated-service",
          writeCharacteristicUuid: "generated-rx",
          indicateCharacteristicUuid: "generated-tx",
          maximumWriteValueBytes: 20,
        },
      },
      bridge: {
        contract: CONTRACT,
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {
          destroyed = true;
        },
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          return appliance;
        },
      },
      databasePath: "/app/offline.sqlite3",
    };
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    expect(await client.contacts()).toEqual([
      { destination: "ef".repeat(16), name: "Offline contact" },
    ]);
    await waitFor(() => connectCount > 0, "background BLE attempt");
    await expect(client.reconnect()).rejects.toThrow("no nearby appliance");
    expect(connectCount).toBe(2);

    client.dispose();
    await waitFor(() => destroyed, "native BLE cleanup");
    expect(disposeCount).toBe(1);
  });
});

describe("native credential import onboarding", () => {
  test("keeps BLE stopped for a missing credential and treats picker cancel as a no-op", async () => {
    const connections: BleConnectOptions[] = [];
    let pickerCalls = 0;
    let importCalls = 0;
    const central: BleCentral = {
      supported: true,
      async connect(_profile, options): Promise<never> {
        connections.push(options ?? {});
        throw new Error("unexpected BLE scan");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => {
        importCalls += 1;
        return E290_CREDENTIAL;
      },
      async pickCredential() {
        pickerCalls += 1;
        return null;
      },
      status: () => ({ state: "missing" }),
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await Bun.sleep(0);
    expect(connections).toEqual([]);
    expect(await client.onboarding()).toEqual({
      available: true,
      method: "credential_import",
      snapshot: {
        lifecycle: { state: "needs_pairing" },
        revision: 0,
        usb_serial: "",
      },
    });
    await client.startOnboarding();
    await expect(client.reconnect()).rejects.toThrow(
      "Import an activated device credential before connecting",
    );

    expect(pickerCalls).toBe(1);
    expect(importCalls).toBe(0);
    expect(connections).toEqual([]);
    client.dispose();
  });

  test("projects an active BLE credential without any exact target as faulted and never scans", async () => {
    let connectCalls = 0;
    const untargetedCredential: NativeCredentialSummary = {
      ...E290_CREDENTIAL,
      expectedBleLocalName: undefined,
    };
    const central: BleCentral = {
      supported: true,
      async connect(): Promise<never> {
        connectCalls += 1;
        throw new Error("unexpected untargeted scan");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => untargetedCredential,
      peripheralName: null,
      status: () => ({ state: "active", summary: untargetedCredential }),
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await Bun.sleep(0);

    expect(await client.onboarding()).toMatchObject({
      method: "credential_import",
      snapshot: {
        lifecycle: { state: "faulted", reason: "unsupported_device" },
      },
    });
    await expect(client.reconnect()).rejects.toThrow("refusing an untargeted scan");
    expect(connectCalls).toBe(0);
    client.dispose();
  });

  test("applies a credential that appears after bootstrap before explicit reconnect scans", async () => {
    const connections: BleConnectOptions[] = [];
    let state: NativeCredentialState = { state: "missing" };
    const central: BleCentral = {
      supported: true,
      async connect(_profile, options): Promise<never> {
        connections.push(options ?? {});
        throw new Error("offline test");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => E290_CREDENTIAL,
      peripheralName: null,
      status: () => state,
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    expect(connections).toEqual([]);

    state = { state: "active", summary: E290_CREDENTIAL };
    await expect(client.reconnect()).rejects.toThrow("offline test");

    expect(connections).toHaveLength(1);
    expect(connections[0]?.peripheralName).toBe(E290_CREDENTIAL.expectedBleLocalName);
    client.dispose();
  });

  test("imports from app-owned staging, always removes it, and targets the derived board", async () => {
    const connections: BleConnectOptions[] = [];
    let state: NativeCredentialState = { state: "missing" };
    let cleaned = 0;
    const central: BleCentral = {
      supported: true,
      async connect(_profile, options): Promise<never> {
        connections.push(options ?? {});
        throw new Error("offline test");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential(stagingPath) {
        expect(stagingPath).toBe("/app/cache/import.rdpkey");
        state = { state: "active", summary: E290_CREDENTIAL };
        return E290_CREDENTIAL;
      },
      pickCredential: async () => ({
        stagingPath: "/app/cache/import.rdpkey",
        async cleanup(): Promise<void> {
          cleaned += 1;
        },
      }),
      status: () => state,
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    expect(connections).toEqual([]);
    await client.startOnboarding();
    await waitFor(() => connections.length === 1, "credential-targeted BLE scan");

    expect(cleaned).toBe(1);
    expect(connections[0]?.peripheralName).toBe(E290_CREDENTIAL.expectedBleLocalName);
    expect(await client.onboarding()).toMatchObject({
      available: true,
      method: "credential_import",
      snapshot: { lifecycle: { state: "credential_ready" }, usb_serial: "" },
    });
    client.dispose();
  });

  test("removes staging and leaves BLE stopped when native validation rejects the import", async () => {
    let cleaned = 0;
    let connectCalls = 0;
    const central: BleCentral = {
      supported: true,
      async connect(): Promise<never> {
        connectCalls += 1;
        throw new Error("unexpected BLE scan");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => {
        throw new Error("credential length is invalid");
      },
      pickCredential: async () => ({
        stagingPath: "/app/cache/bad.rdpkey",
        cleanup(): void {
          cleaned += 1;
        },
      }),
      status: () => ({ state: "missing" }),
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await expect(client.startOnboarding()).rejects.toThrow("credential length is invalid");

    expect(cleaned).toBe(1);
    expect(connectCalls).toBe(0);
    client.dispose();
  });

  test("reconciles an import error after atomic publication before starting BLE", async () => {
    const connections: BleConnectOptions[] = [];
    let state: NativeCredentialState = { state: "missing" };
    let cleaned = 0;
    const publicationUncertain = {
      tag: "CredentialPublicationUncertain",
      inner: { reason: "directory fsync failed after publication" },
    } as unknown as NativeApplianceError;
    const central: BleCentral = {
      supported: true,
      async connect(_profile, options): Promise<never> {
        connections.push(options ?? {});
        throw new Error("offline test");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => {
        state = { state: "active", summary: E290_CREDENTIAL };
        throw publicationUncertain;
      },
      isNativeError: (value): value is NativeApplianceError => value === publicationUncertain,
      pickCredential: async () => ({
        stagingPath: "/app/cache/reconcile.rdpkey",
        cleanup(): void {
          cleaned += 1;
        },
      }),
      status: () => state,
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await client.startOnboarding();
    await waitFor(() => connections.length === 1, "reconciled credential BLE scan");

    expect(cleaned).toBe(1);
    expect(connections[0]?.peripheralName).toBe(E290_CREDENTIAL.expectedBleLocalName);
    client.dispose();
  });

  test("never reconciles an exact-readback rejection even when changed bytes remain active", async () => {
    let state: NativeCredentialState = { state: "missing" };
    let cleaned = 0;
    let connectCalls = 0;
    const readbackRejected = {
      tag: "Storage",
      inner: {
        reason: "installed credential bytes changed during publication or readback",
      },
    } as unknown as NativeApplianceError;
    const central: BleCentral = {
      supported: true,
      async connect(): Promise<never> {
        connectCalls += 1;
        throw new Error("unexpected BLE scan");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => {
        // The altered PSK remains structurally canonical and therefore
        // inspectable as Active, but it is not the exact selected authority.
        state = { state: "active", summary: E290_CREDENTIAL };
        throw readbackRejected;
      },
      isNativeError: (value): value is NativeApplianceError => value === readbackRejected,
      pickCredential: async () => ({
        stagingPath: "/app/cache/readback-mismatch.rdpkey",
        cleanup(): void {
          cleaned += 1;
        },
      }),
      status: () => state,
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await expect(client.startOnboarding()).rejects.toThrow(
      "installed credential bytes changed during publication or readback",
    );

    expect(cleaned).toBe(1);
    expect(connectCalls).toBe(0);
    client.dispose();
  });

  test("configures the installed credential before surfacing a staging cleanup failure", async () => {
    const connections: BleConnectOptions[] = [];
    let state: NativeCredentialState = { state: "missing" };
    const central: BleCentral = {
      supported: true,
      async connect(_profile, options): Promise<never> {
        connections.push(options ?? {});
        throw new Error("offline test");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => {
        state = { state: "active", summary: E290_CREDENTIAL };
        return E290_CREDENTIAL;
      },
      pickCredential: async () => ({
        stagingPath: "/app/cache/residual.rdpkey",
        cleanup(): never {
          throw new Error("cache unlink failed");
        },
      }),
      status: () => state,
    });
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await expect(client.startOnboarding()).rejects.toThrow(
      "app-private staging copy could not be removed",
    );
    await waitFor(() => connections.length === 1, "targeted BLE scan after cleanup failure");

    expect(connections[0]?.peripheralName).toBe(E290_CREDENTIAL.expectedBleLocalName);
    client.dispose();
  });

  test("accepts a generic Wi-Fi credential and reconnects its dormant native owner", async () => {
    const genericCredential: NativeCredentialSummary = {
      ...E290_CREDENTIAL,
      deviceId: "ab".repeat(16),
      expectedBleLocalName: undefined,
    };
    let state: NativeCredentialState = { state: "missing" };
    let reconnects = 0;
    let cleaned = 0;
    const appliance: NativeApplianceLike = {
      ...offlineBleAppliance(),
      async reconnect(): Promise<void> {
        reconnects += 1;
      },
    };
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        credentialStatus: () => state,
        destroy(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential(_appliance, stagingPath): NativeCredentialSummary {
          expect(stagingPath).toBe("/app/cache/wifi.rdpkey");
          state = { state: "active", summary: genericCredential };
          return genericCredential;
        },
        open(): NativeApplianceLike {
          return appliance;
        },
      },
      databasePath: "/app/wifi-onboarding.sqlite3",
      pickCredential: async () => ({
        stagingPath: "/app/cache/wifi.rdpkey",
        cleanup(): void {
          cleaned += 1;
        },
      }),
    };
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await client.startOnboarding();

    expect(reconnects).toBe(1);
    expect(cleaned).toBe(1);
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "credential_ready" } },
    });
    client.dispose();
  });
});
