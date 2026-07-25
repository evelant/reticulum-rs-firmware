import { describe, expect, test } from "bun:test";

import type {
  NativeApplianceError,
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBleOnboardingLike,
  NativeBridgeContract,
  NativeCredentialSummary,
  NativeProfileStoreLike,
  NativeProfileSummary,
  NativeTransport,
} from "@reticulum/appliance-native";

import {
  DEVICE_API_VERSION_MAJOR,
  DEVICE_API_VERSION_MINOR,
  MAX_LXMF_BASIC_CONTENT_BYTES,
  MAX_LXMF_BASIC_TITLE_BYTES,
  MAX_LXMF_READ_CHUNK_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_NOMAD_PAGE_BYTES,
  MAX_NOMAD_PAGE_PATH_BYTES,
  MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
} from "../generated/api.ts";
import type {
  BleCandidate,
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleScanOptions,
} from "./ble-central-types.ts";
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
  maxNomadPagePathBytes: MAX_NOMAD_PAGE_PATH_BYTES,
  maxNomadPageBytes: MAX_NOMAD_PAGE_BYTES,
  maxNomadRequestTimestampUnixMs: BigInt(MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS),
};

const E290_CREDENTIAL: NativeCredentialSummary = {
  credentialId: "cd".repeat(16),
  deviceId: "653239302d6170692d31e13e88",
  expectedBleLocalName: "reticulum-e290-e13e88",
  generation: 1n,
};
const PROFILE_STORE = {} as NativeProfileStoreLike;

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  const deadline = performance.now() + 1_000;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await Bun.sleep(1);
  }
}

function emptyBleScan(): Promise<readonly BleCandidate[]> {
  return Promise.resolve([]);
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
    async nomadFetchStartJson(): Promise<string> {
      return JSON.stringify({ id: `${"33".repeat(8)}0000000000000001`, outcome: "accepted" });
    },
    async nomadFetchPollJson(): Promise<string> {
      return JSON.stringify({ state: "pending", phase: "path_lookup" });
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
    createBle: () => ({
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
        securityConfirmationCharacteristicUuid: "generated-security-confirmation",
        securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
        maximumWriteValueBytes: 20,
      },
    }),
    bridge: {
      contract: CONTRACT,
      credentialStatus: options.status,
      destroy(): void {},
      destroyProfileStore(): void {},
      isNativeError: options.isNativeError ?? ((_value): _value is NativeApplianceError => false),
      importCredential(_appliance, stagingPath): NativeCredentialSummary {
        return options.importCredential(stagingPath);
      },
      open(): NativeApplianceLike {
        return appliance;
      },
    },
    pickCredential: options.pickCredential,
    profileStore: PROFILE_STORE,
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
            associated_nomad_destination: "ce".repeat(16),
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
      async nomadFetchStartJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({
          id: `${"33".repeat(8)}0000000000000001`,
          outcome: "accepted",
        });
      },
      async nomadFetchPollJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({ state: "ready", page: ">Metalbeard" });
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
        destroyProfileStore(value): void {
          expect(value).toBe(PROFILE_STORE);
        },
        isNativeError: (value): value is NativeApplianceError => value === bridgeError,
        importCredential: () => E290_CREDENTIAL,
        open(profileStore): NativeApplianceLike {
          expect(profileStore).toBe(PROFILE_STORE);
          events.push("open profile");
          return appliance;
        },
      },
      profileStore: PROFILE_STORE,
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
        associated_nomad_destination: "ce".repeat(16),
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
    const fetchId = `${"33".repeat(8)}0000000000000001`;
    expect(
      await client.nomadFetchStart({
        destination: "de".repeat(16),
        path: "/page/index.mu",
        timestamp_unix_ms: 2,
        idempotency_key: "ef".repeat(16),
      }),
    ).toEqual({ id: fetchId, outcome: "accepted" });
    expect(await client.nomadFetchPoll({ id: fetchId })).toEqual({
      state: "ready",
      page: ">Metalbeard",
    });
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
      {
        destination: "de".repeat(16),
        path: "/page/index.mu",
        timestamp_unix_ms: 2,
        idempotency_key: "ef".repeat(16),
      },
      { id: fetchId },
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
    expect(events).toEqual(["open profile", "close", "destroy"]);
  });

  test("maps the generated native GATT profile without duplicating or swapping values", () => {
    const generated: NativeBleGattProfile = {
      major: 7,
      minor: 8,
      serviceUuid: "generated-service",
      rxUuid: "generated-rx",
      txUuid: "generated-tx",
      securityConfirmationUuid: "generated-security-confirmation",
      securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31).buffer,
      initialAttValueBytes: 41,
    };

    expect(bleGattProfileFromNative(generated)).toEqual({
      serviceUuid: "generated-service",
      writeCharacteristicUuid: "generated-rx",
      indicateCharacteristicUuid: "generated-tx",
      maximumWriteValueBytes: 41,
      securityConfirmationCharacteristicUuid: "generated-security-confirmation",
      securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
    });
  });

  test("keeps the durable client available while the background BLE attempt fails", async () => {
    let connectCount = 0;
    let disposeCount = 0;
    let destroyed = false;
    const central: BleCentral = {
      supported: true,
      scan: emptyBleScan,
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
      async nomadFetchStartJson(): Promise<string> {
        return JSON.stringify({ id: `${"33".repeat(8)}0000000000000001`, outcome: "accepted" });
      },
      async nomadFetchPollJson(): Promise<string> {
        return JSON.stringify({ state: "pending", phase: "path_lookup" });
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
      createBle: () => ({
        central,
        decodeCommand: () => {
          throw new Error("no command expected");
        },
        profile: {
          serviceUuid: "generated-service",
          writeCharacteristicUuid: "generated-rx",
          indicateCharacteristicUuid: "generated-tx",
          securityConfirmationCharacteristicUuid: "generated-security-confirmation",
          securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
          maximumWriteValueBytes: 20,
        },
      }),
      bridge: {
        contract: CONTRACT,
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {
          destroyed = true;
        },
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          return appliance;
        },
      },
      profileStore: PROFILE_STORE,
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

describe("fileless native BLE onboarding", () => {
  test("continues explicitly when a retained iOS bond produces no passkey callback", async () => {
    const selectedConnections: BleConnectOptions[] = [];
    let credentialState: NativeCredentialState = { state: "missing" };
    let connectionCloses = 0;
    let nativeDestroys = 0;
    let ordinaryConnectAttempts = 0;
    let ordinaryBleRegistrations = 0;
    let onboardingBleRegistrations = 0;
    let pairCalls = 0;
    let pickerCalls = 0;
    let securityConfirmationReads = 0;
    let writes = 0;
    let phase: "idle" | "link_ready" | "waiting_for_begin_presence" | "complete" = "idle";
    let finishPair = () => {};
    const pairBarrier = new Promise<void>((resolve) => {
      finishPair = resolve;
    });
    let observer: BleConnectionObserver | null = null;
    const connection: BleConnection = {
      maxWriteWithResponseBytes: 20,
      peripheralId: "board-b",
      observe(nextObserver): () => void {
        observer = nextObserver;
        return () => {
          if (observer === nextObserver) observer = null;
        };
      },
      async write(): Promise<void> {
        writes += 1;
      },
      async read(): Promise<Uint8Array> {
        expect(pairCalls).toBe(0);
        securityConfirmationReads += 1;
        return Uint8Array.of(0x52, 0x44, 0x59, 0x31);
      },
      async close(): Promise<void> {
        connectionCloses += 1;
        if (connectionCloses === 1) throw new Error("simulated GATT close failure");
      },
    };
    const ordinaryConnection: BleConnection = {
      maxWriteWithResponseBytes: 20,
      name: E290_CREDENTIAL.expectedBleLocalName,
      peripheralId: "board-b",
      observe(): () => void {
        return () => {};
      },
      async write(): Promise<void> {},
      async read(): Promise<Uint8Array> {
        return Uint8Array.of(0x52, 0x44, 0x59, 0x31);
      },
      async close(): Promise<void> {},
    };
    const central: BleCentral = {
      supported: true,
      scan: emptyBleScan,
      async connect(_profile, options): Promise<BleConnection> {
        selectedConnections.push(options ?? {});
        if (options?.peripheralId === "board-b") return connection;
        ordinaryConnectAttempts += 1;
        if (ordinaryConnectAttempts === 1) {
          throw new Error("board has not resumed advertising yet");
        }
        return ordinaryConnection;
      },
      async dispose(): Promise<void> {},
    };
    const appliance: NativeApplianceLike = {
      ...offlineBleAppliance(),
      bleLinkConnected(): bigint {
        ordinaryBleRegistrations += 1;
        return 99n;
      },
      async bleNextPlatformCommand(_generation, asyncOptions): Promise<undefined> {
        await new Promise<void>((_resolve, reject) => {
          asyncOptions?.signal.addEventListener("abort", () => reject(asyncOptions.signal.reason), {
            once: true,
          });
        });
        return undefined;
      },
    };
    const completedProfile: NativeProfileSummary = {
      credential: E290_CREDENTIAL,
      profileKey: E290_CREDENTIAL.deviceId,
    };
    const onboarding: NativeBleOnboardingLike = {
      async abortCurrent(): Promise<void> {},
      bleDisconnected(): void {},
      bleIngestIndication(): void {},
      bleLinkConnected(peripheralId): bigint {
        expect(peripheralId).toBe("board-b");
        onboardingBleRegistrations += 1;
        phase = "link_ready";
        return 7n;
      },
      async bleNextPlatformCommand(_generation, asyncOptions): Promise<undefined> {
        await new Promise<void>((_resolve, reject) => {
          asyncOptions?.signal.addEventListener("abort", () => reject(asyncOptions.signal.reason), {
            once: true,
          });
        });
        return undefined;
      },
      bleWriteFailed(): void {},
      bleWriteSucceeded(): void {},
      async pair(): Promise<NativeProfileSummary> {
        pairCalls += 1;
        phase = "waiting_for_begin_presence";
        await pairBarrier;
        credentialState = { state: "active", summary: E290_CREDENTIAL };
        phase = "complete";
        return completedProfile;
      },
      async resume(): Promise<NativeProfileSummary> {
        throw new Error("resume not expected");
      },
      snapshot() {
        throw new Error("test bridge owns the coarse snapshot projection");
      },
    };
    const runtime: NativeApplianceRuntime = {
      bleOnboarding: {
        destroy(): void {
          nativeDestroys += 1;
        },
        open(profileStore): NativeBleOnboardingLike {
          expect(profileStore).toBe(PROFILE_STORE);
          return onboarding;
        },
        snapshot() {
          return {
            completedProfile: phase === "complete" ? completedProfile : undefined,
            phase,
            revision: phase === "idle" ? 0n : phase === "complete" ? 3n : 2n,
          };
        },
      },
      bridge: {
        contract: CONTRACT,
        credentialStatus: () => credentialState,
        destroy(): void {},
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential(): NativeCredentialSummary {
          throw new Error("file import must not run during BLE onboarding");
        },
        open(): NativeApplianceLike {
          return appliance;
        },
      },
      createBle: () => ({
        central,
        decodeCommand: () => {
          throw new Error("no platform command expected");
        },
        peripheralName: "diagnostic-name-must-not-select-onboarding",
        profile: {
          indicateCharacteristicUuid: "generated-tx",
          maximumWriteValueBytes: 20,
          securityConfirmationCharacteristicUuid: "generated-security-confirmation",
          securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
          serviceUuid: "generated-service",
          writeCharacteristicUuid: "generated-rx",
        },
      }),
      async pickCredential() {
        pickerCalls += 1;
        return null;
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    expect(await client.onboarding()).toMatchObject({
      method: "managed_pairing",
      snapshot: { lifecycle: { state: "needs_pairing" } },
    });
    await client.startOnboarding({
      peripheralId: "board-b",
      peripheralName: "Reticulum B",
      rssi: -42,
    });

    expect(await client.onboarding()).toMatchObject({
      method: "managed_pairing",
      snapshot: {
        lifecycle: { state: "working", stage: "waiting_for_ble_security" },
        usb_serial: "board-b",
      },
    });
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "working", stage: "waiting_for_ble_security" } },
    });
    expect(selectedConnections[0]?.peripheralId).toBe("board-b");
    expect(selectedConnections[0]?.peripheralName).toBeUndefined();
    expect(onboardingBleRegistrations).toBe(1);
    expect(ordinaryBleRegistrations).toBe(0);
    expect(pairCalls).toBe(0);
    expect(pickerCalls).toBe(0);
    expect(writes).toBe(0);
    expect(connectionCloses).toBe(0);
    expect(nativeDestroys).toBe(0);

    await expect(
      client.startOnboarding({
        peripheralId: "board-a",
        peripheralName: "Reticulum A",
      }),
    ).rejects.toThrow("another BLE onboarding operation is already active");

    await expect(client.cancelOnboarding()).rejects.toThrow("simulated GATT close failure");
    expect(nativeDestroys).toBe(1);
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "needs_pairing" } },
    });

    await client.startOnboarding({
      peripheralId: "board-b",
      peripheralName: "Reticulum B",
    });
    (observer as BleConnectionObserver | null)?.onDisconnect({
      peripheralId: "board-b",
      reason: "radio link disappeared before the Bluetooth code",
    });
    expect(await client.onboarding()).toMatchObject({
      snapshot: {
        lifecycle: { state: "faulted", reason: "device_unavailable" },
      },
    });
    await expect(client.continueOnboarding()).rejects.toThrow(
      "open a selected BLE appliance before continuing secure pairing",
    );
    expect(pairCalls).toBe(0);
    expect(connectionCloses).toBe(1);
    expect(nativeDestroys).toBe(2);

    await client.startOnboarding({
      peripheralId: "board-b",
      peripheralName: "Reticulum B",
    });
    // This fixture deliberately has no platform passkey or bond event. iOS can
    // silently reuse a retained bond, so the explicit UI action is the only
    // portable continuation signal available to the native pairing owner.
    const pairing = client.continueOnboarding();
    await waitFor(
      () => phase === "waiting_for_begin_presence",
      "native physical-presence progress",
    );
    await expect(client.continueOnboarding()).rejects.toThrow(
      "retained BLE onboarding operation has already started",
    );
    expect(pairCalls).toBe(1);
    expect(securityConfirmationReads).toBe(1);
    expect(selectedConnections).toHaveLength(3);
    expect(onboardingBleRegistrations).toBe(3);

    finishPair();
    await pairing;
    expect(connectionCloses).toBe(2);
    expect(nativeDestroys).toBe(3);
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "credential_ready" } },
    });
    await waitFor(() => ordinaryConnectAttempts === 1, "first post-pair ordinary BLE scan");
    await Bun.sleep(0);
    await expect(client.reconnect()).resolves.toBeUndefined();
    expect(ordinaryConnectAttempts).toBe(2);
    expect(ordinaryBleRegistrations).toBe(1);
    expect(selectedConnections.at(-1)?.peripheralName).toBe(E290_CREDENTIAL.expectedBleLocalName);
    expect(pairCalls).toBe(1);
    client.dispose();
  });
});

describe("native credential import onboarding", () => {
  test("forwards advertisement-only discovery while credentials are missing without starting BLE", async () => {
    const candidates = [
      { peripheralId: "board-a", peripheralName: "Reticulum A", rssi: -48 },
    ] as const;
    const scans: Array<{ readonly options?: BleScanOptions; readonly serviceUuid: string }> = [];
    let connectCalls = 0;
    const central: BleCentral = {
      supported: true,
      async scan(serviceUuid, options): Promise<readonly BleCandidate[]> {
        scans.push({ serviceUuid, options });
        return candidates;
      },
      async connect(): Promise<never> {
        connectCalls += 1;
        throw new Error("unexpected BLE connection");
      },
      async dispose(): Promise<void> {},
    };
    const runtime = credentialRuntime({
      central,
      importCredential: () => E290_CREDENTIAL,
      status: () => ({ state: "missing" }),
    });
    const client = new NativeApplianceClient(async () => runtime);
    const abort = new AbortController();

    expect(client.supportsBleCandidateDiscovery()).toBeFalse();
    await client.bootstrapSession();
    expect(client.supportsBleCandidateDiscovery()).toBeTrue();
    await expect(
      client.scanBleCandidates({ scanTimeoutMs: 25, signal: abort.signal }),
    ).resolves.toEqual(candidates);

    expect(scans).toEqual([
      {
        serviceUuid: "generated-service",
        options: { scanTimeoutMs: 25, signal: abort.signal },
      },
    ]);
    expect(connectCalls).toBe(0);
    client.dispose();
  });

  test("refuses credential-free discovery for active and invalid credential states", async () => {
    for (const state of [
      { state: "active", summary: E290_CREDENTIAL },
      { state: "invalid", reason: "bad credential" },
    ] satisfies NativeCredentialState[]) {
      let scanCalls = 0;
      const central: BleCentral = {
        supported: true,
        async scan(): Promise<readonly BleCandidate[]> {
          scanCalls += 1;
          return [];
        },
        async connect(): Promise<never> {
          throw new Error("offline active credential");
        },
        async dispose(): Promise<void> {},
      };
      const runtime = credentialRuntime({
        central,
        importCredential: () => E290_CREDENTIAL,
        status: () => state,
      });
      const client = new NativeApplianceClient(async () => runtime);

      await client.bootstrapSession();
      await expect(client.scanBleCandidates({ scanTimeoutMs: 1 })).rejects.toThrow(
        "available only before credential setup",
      );
      expect(scanCalls).toBe(0);
      client.dispose();
    }
  });

  test("keeps BLE stopped for a missing credential and treats picker cancel as a no-op", async () => {
    const connections: BleConnectOptions[] = [];
    let pickerCalls = 0;
    let importCalls = 0;
    const central: BleCentral = {
      supported: true,
      scan: emptyBleScan,
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
      scan: emptyBleScan,
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
      scan: emptyBleScan,
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
      scan: emptyBleScan,
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

  test("closes the unconfigured owner before opening the imported device profile", async () => {
    const events: string[] = [];
    let active = false;
    let opened = 0;
    const appliances = [offlineBleAppliance(), offlineBleAppliance()].map((base, index) => ({
      ...base,
      async close(): Promise<void> {
        events.push(`close ${index + 1}`);
      },
    }));
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        credentialStatus: () =>
          active ? { state: "active", summary: E290_CREDENTIAL } : { state: "missing" },
        destroy(appliance): void {
          events.push(`destroy ${appliance === appliances[0] ? 1 : 2}`);
        },
        destroyProfileStore(): void {
          events.push("destroy profile store");
        },
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential(): NativeCredentialSummary {
          events.push("import");
          active = true;
          return E290_CREDENTIAL;
        },
        open(profileStore): NativeApplianceLike {
          expect(profileStore).toBe(PROFILE_STORE);
          events.push(`open ${opened + 1}`);
          return appliances[opened++] as NativeApplianceLike;
        },
      },
      pickCredential: async () => ({
        stagingPath: "/app/cache/profile-import.rdpkey",
        cleanup(): void {
          events.push("cleanup staging");
        },
      }),
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    await client.startOnboarding();

    expect(events).toEqual([
      "open 1",
      "import",
      "cleanup staging",
      "close 1",
      "destroy 1",
      "open 2",
    ]);
    client.dispose();
    await waitFor(() => events.includes("destroy profile store"), "profile store cleanup");
    expect(events.slice(-3)).toEqual(["close 2", "destroy 2", "destroy profile store"]);
  });

  test("removes staging and leaves BLE stopped when native validation rejects the import", async () => {
    let cleaned = 0;
    let connectCalls = 0;
    const central: BleCentral = {
      supported: true,
      scan: emptyBleScan,
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
      scan: emptyBleScan,
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
      scan: emptyBleScan,
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
      scan: emptyBleScan,
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

  test("accepts a generic Wi-Fi credential and reopens its active device profile", async () => {
    const genericCredential: NativeCredentialSummary = {
      ...E290_CREDENTIAL,
      deviceId: "ab".repeat(16),
      expectedBleLocalName: undefined,
    };
    let state: NativeCredentialState = { state: "missing" };
    let opens = 0;
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
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential(_appliance, stagingPath): NativeCredentialSummary {
          expect(stagingPath).toBe("/app/cache/wifi.rdpkey");
          state = { state: "active", summary: genericCredential };
          return genericCredential;
        },
        open(): NativeApplianceLike {
          opens += 1;
          return appliance;
        },
      },
      pickCredential: async () => ({
        stagingPath: "/app/cache/wifi.rdpkey",
        cleanup(): void {
          cleaned += 1;
        },
      }),
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);

    expect(client.supportsBleCandidateDiscovery()).toBeFalse();
    await client.bootstrapSession();
    expect(client.supportsBleCandidateDiscovery()).toBeFalse();
    await client.startOnboarding();

    expect(opens).toBe(2);
    expect(reconnects).toBe(0);
    expect(cleaned).toBe(1);
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "credential_ready" } },
    });
    client.dispose();
  });
});
