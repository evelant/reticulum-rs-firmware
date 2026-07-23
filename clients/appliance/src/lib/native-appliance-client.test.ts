import { describe, expect, test } from "bun:test";

import type {
  NativeApplianceError,
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBridgeContract,
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
import type { BleCentral } from "./ble-central-types.ts";
import {
  bleGattProfileFromNative,
  NativeApplianceClient,
  type NativeApplianceRuntime,
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

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  const deadline = performance.now() + 1_000;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await Bun.sleep(1);
  }
}

describe("native appliance adapter loading", () => {
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
      async contactsJson(): Promise<string> {
        return JSON.stringify([{ destination: "ab".repeat(16), name: "Field node" }]);
      },
      async ensureConnected(): Promise<void> {},
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
        destroy(value): void {
          expect(value).toBe(appliance);
          events.push("destroy");
          finishDestroy();
        },
        isNativeError: (value): value is NativeApplianceError => value === bridgeError,
        open(databasePath): NativeApplianceLike {
          events.push(`open ${databasePath}`);
          return appliance;
        },
      },
      databasePath: "/app/reticulum-lxmf-chat.sqlite3",
    };
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    expect((await client.snapshot()).connection).toEqual({
      state: "unavailable",
      transport: "usb_serial",
    });
    expect(await client.contacts()).toEqual([{ destination: "ab".repeat(16), name: "Field node" }]);
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
    expect(events).toEqual(["open /app/reticulum-lxmf-chat.sqlite3", "close", "destroy"]);
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
      async contactsJson(): Promise<string> {
        return JSON.stringify([{ destination: "ef".repeat(16), name: "Offline contact" }]);
      },
      async ensureConnected(): Promise<void> {},
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
        destroy(): void {
          destroyed = true;
        },
        isNativeError: (_value): _value is NativeApplianceError => false,
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
