import { describe, expect, test } from "bun:test";

import type {
  NativeApplianceError,
  NativeApplianceLike,
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
import { NativeApplianceClient, type NativeApplianceRuntime } from "./native-appliance-client.ts";
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
      inner: { transport: 2, reason: "BLE connector is not implemented" },
    } as unknown as NativeApplianceError;
    const appliance: NativeApplianceLike = {
      async close(): Promise<void> {
        events.push("close");
      },
      async contactsJson(): Promise<string> {
        return JSON.stringify([{ destination: "ab".repeat(16), name: "Field node" }]);
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
          connection: { state: "unavailable", transport: "bluetooth_low_energy" },
          device: null,
          pending_outbox: 0,
          contact_count: 1,
          imported_this_run: 0,
          last_error: "BLE connector is not implemented",
        });
      },
      async syncNow(): Promise<void> {},
      async timelineJson(): Promise<string> {
        return "[]";
      },
      transport(): NativeTransport {
        return 2 as NativeTransport;
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
      transport: "bluetooth_low_energy",
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
      "Native appliance transport unavailable: BLE connector is not implemented",
    );

    client.dispose();
    await destroyed;
    expect(events).toEqual(["open /app/reticulum-lxmf-chat.sqlite3", "close", "destroy"]);
  });
});
