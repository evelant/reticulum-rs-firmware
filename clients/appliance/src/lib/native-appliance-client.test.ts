import { describe, expect, test } from "bun:test";

import type {
  NativeApplianceError,
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBleOnboardingLike,
  NativeBridgeContract,
  NativeCredentialSummary,
  NativeProfileStoreLike,
  NativeProfileStoreSnapshot,
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
  expectedBleRecoveryLocalName: "reticulum-pair-e13e88",
  generation: 1n,
};
const E290_PROFILE: NativeProfileSummary = {
  credential: E290_CREDENTIAL,
  profileKey: E290_CREDENTIAL.deviceId,
};
const PROFILE_STORE = {} as NativeProfileStoreLike;

function e290ProfileSnapshot() {
  return {
    activeProfileKey: E290_PROFILE.profileKey,
    profiles: [E290_PROFILE],
  };
}

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

function networkApiStubs() {
  return {
    async phoneLocationObservationJson(): Promise<string> {
      return JSON.stringify({ state: "unavailable", reason: "not_observed" });
    },
    async updatePhoneLocationObservationJson(observationJson: string): Promise<string> {
      return observationJson;
    },
    async conversationPeersJson(): Promise<string> {
      return "[]";
    },
    async messageActivityJson(_requestJson: string): Promise<string> {
      return JSON.stringify({
        events: [],
        next_before_event_id: null,
        history_incomplete: false,
      });
    },
    async manualServiceAnnounceJson(): Promise<string> {
      return JSON.stringify("queued");
    },
    async mutateNetworkConfigJson(): Promise<string> {
      return JSON.stringify({ outcome: "applied", reboot_required: false, revision: 0 });
    },
    async networkConfigJson(): Promise<string> {
      return JSON.stringify({
        automatic_announces_enabled: true,
        lora_profile: {
          bandwidth_hz: 125_000,
          coding_rate_denominator: 5,
          frequency_hz: 915_000_000,
          spreading_factor: 7,
          tx_power_dbm: 14,
        },
        lora_tx_power_dbm: 14,
        revision: 0,
        rmap_discovery_enabled: false,
        rmap_phone_location: null,
        rmap_share_location: false,
        tcp_peer: null,
        wifi_profiles: [],
        wifi_transport_enabled: true,
      });
    },
    async networkStatusJson(): Promise<string> {
      return JSON.stringify({
        active_wifi_profile: null,
        applied_revision: 0,
        configured_revision: 0,
        connected_ssid: null,
        dns_diagnostics: null,
        ipv4_address: null,
        last_tcp_failure: null,
        rssi_dbm: null,
        tcp_peer_state: "disabled",
        wifi_state: "disabled",
      });
    },
    async radioRoutesStatusJson(): Promise<string> {
      return JSON.stringify({
        interfaces: [],
        lora: null,
        observed_peer_count: 0,
        retained_route_count: 0,
        rns: {
          announces_received: 0,
          dedup_drops: 0,
          forwarded: 0,
          invalid_drops: 0,
          links_closed: 0,
          links_established: 0,
          links_failed: 0,
          paths_expired: 0,
          paths_learned: 0,
          received: 0,
        },
        route_table_revision: 0,
        routes: [],
        uptime_ms: 0,
        usable_route_count: 0,
      });
    },
    async radioTraceJson(_requestJson: string): Promise<string> {
      return JSON.stringify({
        events: [],
        next_before_event_id: null,
        history_incomplete: false,
      });
    },
    async retryMessageJson(requestJson: string): Promise<string> {
      const request = JSON.parse(requestJson) as { outbox_id: number };
      return JSON.stringify({ outbox_id: request.outbox_id, outcome: "requeued" });
    },
    async reticulumProbeStartJson(): Promise<string> {
      return JSON.stringify({ id: "44".repeat(16), outcome: "accepted" });
    },
    async reticulumProbePollJson(): Promise<string> {
      return JSON.stringify({ state: "pending", phase: "path_lookup" });
    },
  };
}

function offlineBleAppliance(): NativeApplianceLike {
  return {
    ...networkApiStubs(),
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
      activateProfile(): NativeProfileSummary {
        return E290_PROFILE;
      },
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
      profileSnapshot: e290ProfileSnapshot,
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
      ...networkApiStubs(),
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
      async conversationPeersJson(): Promise<string> {
        return JSON.stringify([
          {
            destination: "ab".repeat(16),
            name: "Field node",
            message_count: 1,
            inbound_message_count: 1,
            last_message: {
              sequence: 1,
              direction: "inbound",
              timestamp_ms: 1_000,
              message_id: "cd".repeat(32),
              outbox_id: null,
              submission_id: null,
              current_attempt_number: null,
              automatic_retry_count: null,
              packet_evidence: null,
              ingress_observation: {
                interface_id: 7,
                signal: { rssi_dbm: -97, snr_db: 4 },
              },
              receiver_location: null,
              location: {
                latitude_e6: 42_357_111,
                longitude_e6: -71_061_924,
                altitude_cm: 0,
                speed_cm_per_second: 0,
                bearing_centidegrees: 0,
                accuracy_cm: 825,
                updated_at_unix_seconds: 1_785_084_000,
              },
              status: null,
              title: { encoding: "utf8", value: "hello" },
              content: { encoding: "utf8", value: "from the field" },
            },
          },
        ]);
      },
      async messageActivityJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({
          events: [
            {
              event_id: 4,
              observed_at_unix_ms: 1_250,
              timeline_sequence: 1,
              peer: "ab".repeat(16),
              direction: "outbound",
              outbox_id: 7,
              attempt_number: 1,
              attempt_location: null,
              ingress_observation: null,
              message_location: null,
              receiver_location: null,
              activity: { kind: "outbound_queued" },
            },
          ],
          next_before_event_id: null,
          history_incomplete: false,
        });
      },
      async radioTraceJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({
          events: [],
          next_before_event_id: null,
          history_incomplete: false,
        });
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
      async networkConfigJson(): Promise<string> {
        return JSON.stringify({
          automatic_announces_enabled: true,
          lora_profile: {
            bandwidth_hz: 125_000,
            coding_rate_denominator: 5,
            frequency_hz: 915_000_000,
            spreading_factor: 8,
            tx_power_dbm: 22,
          },
          lora_tx_power_dbm: 22,
          revision: 4,
          rmap_discovery_enabled: false,
          rmap_phone_location: null,
          rmap_share_location: false,
          tcp_peer: null,
          wifi_profiles: [
            {
              credential_configured: true,
              enabled: true,
              priority: 180,
              profile_id: "12".repeat(16),
              ssid: { encoding: "utf8", value: "Field Mesh" },
            },
          ],
          wifi_transport_enabled: true,
        });
      },
      async networkStatusJson(): Promise<string> {
        return JSON.stringify({
          active_wifi_profile: "12".repeat(16),
          applied_revision: 3,
          configured_revision: 4,
          connected_ssid: null,
          dns_diagnostics: null,
          ipv4_address: null,
          last_tcp_failure: "dns_timeout",
          rssi_dbm: null,
          tcp_peer_state: "waiting_for_network",
          wifi_state: "connecting",
        });
      },
      async mutateNetworkConfigJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({ outcome: "applied", reboot_required: true, revision: 5 });
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
      async reticulumProbeStartJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({ id: "44".repeat(16), outcome: "accepted" });
      },
      async reticulumProbePollJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({
          state: "succeeded",
          result: {
            round_trip_ms: 1_234,
            hops: 2,
            ingress_observation: {
              interface_id: 7,
              signal: { rssi_dbm: -91, snr_db: 7 },
            },
          },
        });
      },
      async reconnect(): Promise<void> {
        throw bridgeError;
      },
      async retryMessageJson(requestJson): Promise<string> {
        requests.push(requestJson);
        return JSON.stringify({ outbox_id: 7, outcome: "requeued" });
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
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
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
        profileSnapshot: e290ProfileSnapshot,
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
    expect(await client.conversationPeers()).toEqual([
      {
        destination: "ab".repeat(16),
        name: "Field node",
        message_count: 1,
        inbound_message_count: 1,
        last_message: {
          sequence: 1,
          direction: "inbound",
          timestamp_ms: 1_000,
          message_id: "cd".repeat(32),
          outbox_id: null,
          submission_id: null,
          current_attempt_number: null,
          automatic_retry_count: null,
          packet_evidence: null,
          ingress_observation: {
            interface_id: 7,
            signal: { rssi_dbm: -97, snr_db: 4 },
          },
          receiver_location: null,
          location: {
            latitude_e6: 42_357_111,
            longitude_e6: -71_061_924,
            altitude_cm: 0,
            speed_cm_per_second: 0,
            bearing_centidegrees: 0,
            accuracy_cm: 825,
            updated_at_unix_seconds: 1_785_084_000,
          },
          status: null,
          title: { encoding: "utf8", value: "hello" },
          content: { encoding: "utf8", value: "from the field" },
        },
      },
    ]);
    expect(
      await client.messageActivity({
        before_event_id: null,
        limit: 20,
        timeline_sequence: 1,
      }),
    ).toEqual({
      events: [
        {
          event_id: 4,
          observed_at_unix_ms: 1_250,
          timeline_sequence: 1,
          peer: "ab".repeat(16),
          direction: "outbound",
          outbox_id: 7,
          attempt_number: 1,
          attempt_location: null,
          ingress_observation: null,
          message_location: null,
          receiver_location: null,
          activity: { kind: "outbound_queued" },
        },
      ],
      next_before_event_id: null,
      history_incomplete: false,
    });
    expect(await client.phoneLocationObservation()).toEqual({
      state: "unavailable",
      reason: "not_observed",
    });
    expect(
      await client.radioTrace({
        before_event_id: null,
        limit: 20,
        timeline_sequence: 1,
      }),
    ).toEqual({ events: [], next_before_event_id: null, history_incomplete: false });
    expect(
      await client.updatePhoneLocationObservation({
        state: "available",
        latitude_e6: 42_357_111,
        longitude_e6: -71_061_924,
        altitude_mm: 17_234,
        horizontal_accuracy_mm: 8_250,
        vertical_accuracy_mm: 12_500,
        captured_at_unix_ms: 1_785_084_000_123,
        authorization: "precise",
        source: "foreground_stream",
        mocked: false,
      }),
    ).toEqual({
      state: "available",
      latitude_e6: 42_357_111,
      longitude_e6: -71_061_924,
      altitude_mm: 17_234,
      horizontal_accuracy_mm: 8_250,
      vertical_accuracy_mm: 12_500,
      captured_at_unix_ms: 1_785_084_000_123,
      authorization: "precise",
      source: "foreground_stream",
      mocked: false,
    });
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
    expect(await client.manualServiceAnnounce()).toBe("queued");
    expect(await client.networkConfig()).toMatchObject({
      lora_profile: {
        bandwidth_hz: 125_000,
        coding_rate_denominator: 5,
        frequency_hz: 915_000_000,
        spreading_factor: 8,
        tx_power_dbm: 22,
      },
      lora_tx_power_dbm: 22,
      revision: 4,
      wifi_profiles: [{ priority: 180, ssid: { encoding: "utf8", value: "Field Mesh" } }],
    });
    expect(await client.networkStatus()).toMatchObject({
      applied_revision: 3,
      configured_revision: 4,
      dns_diagnostics: null,
      last_tcp_failure: "dns_timeout",
      tcp_peer_state: "waiting_for_network",
      wifi_state: "connecting",
    });
    expect(
      await client.mutateNetworkConfig({
        expected_revision: 4,
        idempotency_key: "13".repeat(16),
        mutation: {
          credential: { kind: "keep" },
          enabled: true,
          kind: "upsert_wifi",
          priority: 200,
          profile_id: "12".repeat(16),
          ssid: { encoding: "utf8", value: "Field Mesh" },
        },
      }),
    ).toEqual({ outcome: "applied", reboot_required: true, revision: 5 });
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
    const probeId = "44".repeat(16);
    expect(
      await client.reticulumProbeStart({
        destination: "de".repeat(16),
        idempotency_key: "f0".repeat(16),
      }),
    ).toEqual({ id: probeId, outcome: "accepted" });
    expect(await client.reticulumProbePoll({ id: probeId })).toEqual({
      state: "succeeded",
      result: {
        round_trip_ms: 1_234,
        hops: 2,
        ingress_observation: {
          interface_id: 7,
          signal: { rssi_dbm: -91, snr_db: 7 },
        },
      },
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
        location: {
          latitude_e6: 42_357_111,
          longitude_e6: -71_061_924,
          altitude_cm: 1_723,
          speed_cm_per_second: 346,
          bearing_centidegrees: 12_345,
          accuracy_cm: 826,
          updated_at_unix_seconds: 1_785_084_000,
        },
      }),
    ).toEqual({ outbox_id: 7, outcome: "inserted" });
    expect(
      await client.retryMessage({
        outbox_id: 7,
        idempotency_key: "ce".repeat(16),
      }),
    ).toEqual({ outbox_id: 7, outcome: "requeued" });
    expect(requests.map((request) => JSON.parse(request))).toEqual([
      {
        before_event_id: null,
        limit: 20,
        timeline_sequence: 1,
      },
      {
        before_event_id: null,
        limit: 20,
        timeline_sequence: 1,
      },
      {
        expected_revision: 4,
        idempotency_key: "13".repeat(16),
        mutation: {
          credential: { kind: "keep" },
          enabled: true,
          kind: "upsert_wifi",
          priority: 200,
          profile_id: "12".repeat(16),
          ssid: { encoding: "utf8", value: "Field Mesh" },
        },
      },
      {
        destination: "de".repeat(16),
        path: "/page/index.mu",
        timestamp_unix_ms: 2,
        idempotency_key: "ef".repeat(16),
      },
      { id: fetchId },
      {
        destination: "de".repeat(16),
        idempotency_key: "f0".repeat(16),
      },
      { id: probeId },
      { name: "Updated field node" },
      {
        destination: "ab".repeat(16),
        timestamp_ms: 1,
        idempotency_key: "cd".repeat(16),
        title: "",
        content: "hello",
        location: {
          latitude_e6: 42_357_111,
          longitude_e6: -71_061_924,
          altitude_cm: 1_723,
          speed_cm_per_second: 346,
          bearing_centidegrees: 12_345,
          accuracy_cm: 826,
          updated_at_unix_seconds: 1_785_084_000,
        },
      },
      {
        outbox_id: 7,
        idempotency_key: "ce".repeat(16),
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
      maximumAttValueBytes: 41,
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
      ...networkApiStubs(),
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
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
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
        profileSnapshot: e290ProfileSnapshot,
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

describe("native appliance profile management", () => {
  const SECOND_CREDENTIAL: NativeCredentialSummary = {
    credentialId: "ef".repeat(16),
    deviceId: "653239302d6170692d31aca704e13f88",
    expectedBleLocalName: "reticulum-e290-e13f88",
    expectedBleRecoveryLocalName: "reticulum-pair-e13f88",
    generation: 2n,
  };
  const SECOND_PROFILE: NativeProfileSummary = {
    credential: SECOND_CREDENTIAL,
    profileKey: SECOND_CREDENTIAL.deviceId,
  };
  const THIRD_CREDENTIAL: NativeCredentialSummary = {
    credentialId: "f0".repeat(16),
    deviceId: "653239302d6170692d31aca704e14088",
    expectedBleLocalName: "reticulum-e290-e14088",
    expectedBleRecoveryLocalName: "reticulum-pair-e14088",
    generation: 3n,
  };
  const THIRD_PROFILE: NativeProfileSummary = {
    credential: THIRD_CREDENTIAL,
    profileKey: THIRD_CREDENTIAL.deviceId,
  };

  test("lists generated profiles and closes the old owner before activating and opening another", async () => {
    const events: string[] = [];
    let activeProfileKey = E290_PROFILE.profileKey;
    const labels = new Map<NativeApplianceLike, string>();
    const profileFor = (profileKey: string) =>
      profileKey === E290_PROFILE.profileKey ? E290_PROFILE : SECOND_PROFILE;
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(_store, profileKey): NativeProfileSummary {
          events.push(`activate ${profileKey}`);
          activeProfileKey = profileKey;
          return profileFor(profileKey);
        },
        credentialStatus(appliance): NativeCredentialState {
          return {
            state: "active",
            summary: labels.get(appliance) === "A" ? E290_CREDENTIAL : SECOND_CREDENTIAL,
          };
        },
        destroy(appliance): void {
          events.push(`destroy ${labels.get(appliance)}`);
        },
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          const label = activeProfileKey === E290_PROFILE.profileKey ? "A" : "B";
          const appliance = {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              events.push(`close ${label}`);
            },
          };
          labels.set(appliance, label);
          events.push(`open ${label}`);
          return appliance;
        },
        profileSnapshot: () => ({
          activeProfileKey,
          profiles: [E290_PROFILE, SECOND_PROFILE],
        }),
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);

    await client.bootstrapSession();
    expect(await client.profiles()).toEqual({
      activeProfileKey: E290_PROFILE.profileKey,
      profiles: [E290_PROFILE, SECOND_PROFILE],
    });
    await client.activateProfile(SECOND_PROFILE.profileKey);

    expect(events).toEqual([
      "open A",
      "close A",
      "destroy A",
      `activate ${SECOND_PROFILE.profileKey}`,
      "open B",
    ]);
    expect((await client.profiles()).activeProfileKey).toBe(SECOND_PROFILE.profileKey);

    client.dispose();
  });

  test("forgets only an inactive profile without closing the active appliance owner", async () => {
    const events: string[] = [];
    let profiles = [E290_PROFILE, SECOND_PROFILE];
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          throw new Error("profile activation must not run while forgetting an inactive profile");
        },
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {
          events.push("destroy");
        },
        destroyProfileStore(): void {},
        forgetProfile(_store, profileKey): NativeProfileStoreSnapshot {
          events.push(`forget ${profileKey}`);
          profiles = profiles.filter((profile) => profile.profileKey !== profileKey);
          return {
            activeProfileKey: E290_PROFILE.profileKey,
            profiles,
          };
        },
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          events.push("open");
          return {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              events.push("close");
            },
          };
        },
        profileSnapshot: () => ({
          activeProfileKey: E290_PROFILE.profileKey,
          profiles,
        }),
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    await client.forgetProfile(SECOND_PROFILE.profileKey);

    expect(events).toEqual(["open", `forget ${SECOND_PROFILE.profileKey}`]);
    expect(await client.profiles()).toEqual({
      activeProfileKey: E290_PROFILE.profileKey,
      profiles: [E290_PROFILE],
    });
    client.dispose();
  });

  test("rejects forgetting the active, unknown, or unsupported profile before mutation", async () => {
    let forgetCalls = 0;
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {},
        destroyProfileStore(): void {},
        forgetProfile(): NativeProfileStoreSnapshot {
          forgetCalls += 1;
          return e290ProfileSnapshot();
        },
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open: offlineBleAppliance,
        profileSnapshot: () => ({
          activeProfileKey: E290_PROFILE.profileKey,
          profiles: [E290_PROFILE, SECOND_PROFILE],
        }),
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    await expect(client.forgetProfile(E290_PROFILE.profileKey)).rejects.toThrow(
      "switch to another appliance",
    );
    await expect(client.forgetProfile("ff".repeat(16))).rejects.toThrow(
      "selected appliance profile no longer exists",
    );
    expect(forgetCalls).toBe(0);
    client.dispose();

    const unsupported = new NativeApplianceClient(async () => ({
      ...runtime,
      bridge: { ...runtime.bridge, forgetProfile: undefined },
    }));
    await unsupported.bootstrapSession();
    await expect(unsupported.forgetProfile(SECOND_PROFILE.profileKey)).rejects.toThrow(
      "installed native bridge cannot forget appliance profiles",
    );
    unsupported.dispose();
  });

  test("validates before teardown and restores the prior owner after activation failure", async () => {
    const events: string[] = [];
    let opens = 0;
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(): never {
          events.push("activate B");
          throw new Error("metadata publication failed");
        },
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {
          events.push("destroy A");
        },
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          opens += 1;
          events.push("open A");
          return {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              events.push("close A");
            },
          };
        },
        profileSnapshot: () => ({
          activeProfileKey: E290_PROFILE.profileKey,
          profiles: [E290_PROFILE, SECOND_PROFILE],
        }),
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    await client.activateProfile(E290_PROFILE.profileKey);
    await expect(client.activateProfile("ff".repeat(16))).rejects.toThrow(
      "selected appliance profile no longer exists",
    );
    expect(events).toEqual(["open A"]);

    await expect(client.activateProfile(SECOND_PROFILE.profileKey)).rejects.toThrow(
      "metadata publication failed",
    );
    expect(opens).toBe(2);
    expect(events).toEqual(["open A", "close A", "destroy A", "activate B", "open A"]);

    client.dispose();
  });

  test("serializes validation so a failed queued switch rolls back to its immediate predecessor", async () => {
    const activations: string[] = [];
    let activeProfileKey = E290_PROFILE.profileKey;
    const profileFor = (profileKey: string): NativeProfileSummary => {
      if (profileKey === E290_PROFILE.profileKey) return E290_PROFILE;
      if (profileKey === SECOND_PROFILE.profileKey) return SECOND_PROFILE;
      return THIRD_PROFILE;
    };
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(_store, profileKey): NativeProfileSummary {
          activations.push(profileKey);
          activeProfileKey = profileKey;
          return profileFor(profileKey);
        },
        credentialStatus: () => ({
          state: "active",
          summary: profileFor(activeProfileKey).credential,
        }),
        destroy(): void {},
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          if (activeProfileKey === THIRD_PROFILE.profileKey) {
            throw new Error("third profile database is unavailable");
          }
          return offlineBleAppliance();
        },
        profileSnapshot: () => ({
          activeProfileKey,
          profiles: [E290_PROFILE, SECOND_PROFILE, THIRD_PROFILE],
        }),
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    const first = client.activateProfile(SECOND_PROFILE.profileKey);
    const second = client.activateProfile(THIRD_PROFILE.profileKey);
    await first;
    await expect(second).rejects.toThrow(
      "The selected appliance could not open; the previous profile was restored.",
    );

    expect(activations).toEqual([
      SECOND_PROFILE.profileKey,
      THIRD_PROFILE.profileKey,
      SECOND_PROFILE.profileKey,
    ]);
    expect((await client.profiles()).activeProfileKey).toBe(SECOND_PROFILE.profileKey);
    await expect(client.snapshot()).resolves.toMatchObject({ revision: 0 });
    client.dispose();
  });

  test("fails closed with an explicit owner fault when teardown cannot be confirmed", async () => {
    let activations = 0;
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          activations += 1;
          return SECOND_PROFILE;
        },
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {},
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          return {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              throw new Error("actor shutdown acknowledgement was lost");
            },
          };
        },
        profileSnapshot: () => ({
          activeProfileKey: E290_PROFILE.profileKey,
          profiles: [E290_PROFILE, SECOND_PROFILE],
        }),
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    await expect(client.activateProfile(SECOND_PROFILE.profileKey)).rejects.toThrow(
      "could not close cleanly",
    );
    expect(activations).toBe(0);
    await expect(client.snapshot()).rejects.toThrow("no replacement appliance owner");
    await expect(client.bootstrapSession()).rejects.toThrow("no replacement appliance owner");

    client.dispose();
  });

  test("does not reopen an appliance when add-device quiescence cannot confirm actor shutdown", async () => {
    const credentialWithoutBleName: NativeCredentialSummary = {
      credentialId: E290_CREDENTIAL.credentialId,
      deviceId: E290_CREDENTIAL.deviceId,
      generation: E290_CREDENTIAL.generation,
    };
    let bleOwners = 0;
    let opens = 0;
    const runtime: NativeApplianceRuntime = {
      bleOnboarding: {
        destroy(): void {},
        open(): never {
          throw new Error("onboarding must not open after teardown fails");
        },
        snapshot(): never {
          throw new Error("onboarding must not open after teardown fails");
        },
      },
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
        credentialStatus: () => ({ state: "active", summary: credentialWithoutBleName }),
        destroy(): void {},
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          opens += 1;
          return {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              throw new Error("actor shutdown acknowledgement was lost");
            },
          };
        },
        profileSnapshot: e290ProfileSnapshot,
      },
      createBle: () => {
        bleOwners += 1;
        return {
          central: {
            supported: true,
            scan: emptyBleScan,
            async connect(): Promise<never> {
              throw new Error("no BLE connection expected");
            },
            async dispose(): Promise<void> {},
          },
          decodeCommand: () => {
            throw new Error("no platform command expected");
          },
          profile: {
            indicateCharacteristicUuid: "generated-tx",
            maximumWriteValueBytes: 20,
            securityConfirmationCharacteristicUuid: "generated-security-confirmation",
            securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
            serviceUuid: "generated-service",
            writeCharacteristicUuid: "generated-rx",
          },
        };
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    await expect(client.beginAddAppliance()).rejects.toThrow("no replacement appliance owner");
    expect(opens).toBe(1);
    expect(bleOwners).toBe(1);
    await expect(client.snapshot()).rejects.toThrow("no replacement appliance owner");
    await expect(client.beginAddAppliance()).rejects.toThrow("no replacement appliance owner");
    expect(opens).toBe(1);
    expect(bleOwners).toBe(1);

    client.dispose();
  });

  test("does not replace an appliance when credential-import teardown is unconfirmed", async () => {
    let cleanupCalls = 0;
    let credentialImported = false;
    let opens = 0;
    const runtime: NativeApplianceRuntime = {
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
        credentialStatus: () =>
          credentialImported ? { state: "active", summary: E290_CREDENTIAL } : { state: "missing" },
        destroy(): void {},
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential(): NativeCredentialSummary {
          credentialImported = true;
          return E290_CREDENTIAL;
        },
        open(): NativeApplianceLike {
          opens += 1;
          return {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              throw new Error("actor shutdown acknowledgement was lost");
            },
          };
        },
        profileSnapshot: e290ProfileSnapshot,
      },
      async pickCredential() {
        return {
          stagingPath: "/app-private/staged-device-credential.rdpkey",
          cleanup(): void {
            cleanupCalls += 1;
          },
        };
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();

    await expect(client.startOnboarding()).rejects.toThrow("no replacement appliance owner");
    expect(credentialImported).toBeTrue();
    expect(cleanupCalls).toBe(1);
    expect(opens).toBe(1);
    await expect(client.snapshot()).rejects.toThrow("no replacement appliance owner");
    await expect(client.bootstrapSession()).rejects.toThrow("no replacement appliance owner");
    expect(opens).toBe(1);

    client.dispose();
  });

  test("quiesces the current BLE owner, reuses one scanner, blocks known re-pairing, and restores on cancel", async () => {
    const events: string[] = [];
    let centralCount = 0;
    let onboardingOpens = 0;
    const runtime: NativeApplianceRuntime = {
      bleOnboarding: {
        destroy(): void {},
        open(): NativeBleOnboardingLike {
          onboardingOpens += 1;
          throw new Error("known appliance must not open onboarding");
        },
        snapshot(): never {
          throw new Error("no onboarding projection expected");
        },
      },
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
        credentialStatus: () => ({ state: "active", summary: E290_CREDENTIAL }),
        destroy(): void {
          events.push("destroy A");
        },
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          events.push("open A");
          return {
            ...offlineBleAppliance(),
            async close(): Promise<void> {
              events.push("close A");
            },
          };
        },
        profileSnapshot: e290ProfileSnapshot,
      },
      createBle: () => {
        centralCount += 1;
        const centralId = centralCount;
        events.push(`create central ${centralId}`);
        const central: BleCentral = {
          supported: true,
          async scan(): Promise<readonly BleCandidate[]> {
            events.push(`scan central ${centralId}`);
            return [
              {
                peripheralId: "known-board",
                peripheralName: E290_CREDENTIAL.expectedBleRecoveryLocalName,
              },
            ];
          },
          async connect(): Promise<never> {
            throw new Error("ordinary board is offline");
          },
          async dispose(): Promise<void> {
            events.push(`dispose central ${centralId}`);
          },
        };
        return {
          central,
          decodeCommand: () => {
            throw new Error("no platform command expected");
          },
          profile: {
            indicateCharacteristicUuid: "generated-tx",
            maximumWriteValueBytes: 20,
            securityConfirmationCharacteristicUuid: "generated-security-confirmation",
            securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
            serviceUuid: "generated-service",
            writeCharacteristicUuid: "generated-rx",
          },
        };
      },
      profileStore: PROFILE_STORE,
    };
    const client = new NativeApplianceClient(async () => runtime);
    await client.bootstrapSession();
    expect(client.supportsAdditionalBleOnboarding()).toBeTrue();

    await client.beginAddAppliance();
    const candidates = await client.scanBleCandidates();
    expect(candidates).toHaveLength(1);
    await expect(client.startOnboarding(candidates[0])).rejects.toThrow("already stored");
    expect(onboardingOpens).toBe(0);
    expect(await client.onboarding()).toMatchObject({
      method: "managed_pairing",
      snapshot: { lifecycle: { state: "needs_pairing" } },
    });

    await client.cancelOnboarding();
    expect(events.indexOf("dispose central 1")).toBeLessThan(events.indexOf("close A"));
    expect(events.indexOf("close A")).toBeLessThan(events.indexOf("create central 2"));
    expect(events).toContain("scan central 2");
    expect(events.indexOf("dispose central 2")).toBeLessThan(events.lastIndexOf("open A"));
    expect(centralCount).toBe(3);
    expect((await client.onboarding()).snapshot?.lifecycle.state).toBe("credential_ready");

    client.dispose();
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
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
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
        profileSnapshot: e290ProfileSnapshot,
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

describe("native BLE profile-publication reconciliation", () => {
  const PUBLISHED_CREDENTIAL: NativeCredentialSummary = {
    credentialId: "a5".repeat(16),
    deviceId: "653239302d6170692d31aca704e15088",
    expectedBleLocalName: "reticulum-e290-e15088",
    expectedBleRecoveryLocalName: "reticulum-pair-e15088",
    generation: 4n,
  };
  const PUBLISHED_PROFILE: NativeProfileSummary = {
    credential: PUBLISHED_CREDENTIAL,
    profileKey: PUBLISHED_CREDENTIAL.deviceId,
  };
  const SELECTED_BOARD: BleCandidate = {
    peripheralId: "board-publication-recovery",
    peripheralName: PUBLISHED_CREDENTIAL.expectedBleLocalName,
    rssi: -47,
  };

  function publicationFailureFixture(outcome: "already_finalized" | "finalized" | "rejected") {
    const previousCredentialWithoutBleName: NativeCredentialSummary = {
      credentialId: E290_CREDENTIAL.credentialId,
      deviceId: E290_CREDENTIAL.deviceId,
      generation: E290_CREDENTIAL.generation,
    };
    const state: {
      activeProfileKey: string;
      bleOwners: number;
      connectionCloses: number;
      nativeDestroys: number;
      opens: string[];
      reconcileCalls: number;
    } = {
      activeProfileKey: E290_PROFILE.profileKey,
      bleOwners: 0,
      connectionCloses: 0,
      nativeDestroys: 0,
      opens: [],
      reconcileCalls: 0,
    };
    let onboardingPhase: "idle" | "link_ready" | "failed" = "idle";
    const selectedConnection: BleConnection = {
      maxWriteWithResponseBytes: 20,
      name: SELECTED_BOARD.peripheralName,
      peripheralId: SELECTED_BOARD.peripheralId,
      observe(): () => void {
        return () => {};
      },
      async read(): Promise<Uint8Array> {
        return Uint8Array.of(0x52, 0x44, 0x59, 0x31);
      },
      async write(): Promise<void> {},
      async close(): Promise<void> {
        state.connectionCloses += 1;
      },
    };
    const onboarding: NativeBleOnboardingLike = {
      async abortCurrent(): Promise<void> {},
      bleDisconnected(): void {},
      bleIngestIndication(): void {},
      bleLinkConnected(peripheralId): bigint {
        expect(peripheralId).toBe(SELECTED_BOARD.peripheralId);
        onboardingPhase = "link_ready";
        return 11n;
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
      async pair(): Promise<never> {
        onboardingPhase = "failed";
        throw new Error("simulated profile publication failure");
      },
      async resume(): Promise<never> {
        throw new Error("resume not expected");
      },
      snapshot(): never {
        throw new Error("test bridge owns the coarse onboarding projection");
      },
    };
    const runtime: NativeApplianceRuntime = {
      bleOnboarding: {
        destroy(): void {
          state.nativeDestroys += 1;
        },
        open(): NativeBleOnboardingLike {
          return onboarding;
        },
        snapshot() {
          return {
            failure:
              onboardingPhase === "failed" ? ("profile_publication_failure" as const) : undefined,
            phase: onboardingPhase,
            revision: onboardingPhase === "idle" ? 0n : 1n,
          };
        },
      },
      bridge: {
        contract: CONTRACT,
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
        credentialStatus: () => ({
          state: "active",
          summary:
            state.activeProfileKey === PUBLISHED_PROFILE.profileKey
              ? PUBLISHED_CREDENTIAL
              : previousCredentialWithoutBleName,
        }),
        destroy(): void {},
        destroyProfileStore(): void {},
        isNativeError: (_value): _value is NativeApplianceError => false,
        importCredential: () => E290_CREDENTIAL,
        open(): NativeApplianceLike {
          const label =
            state.activeProfileKey === PUBLISHED_PROFILE.profileKey ? "published" : "previous";
          state.opens.push(label);
          return offlineBleAppliance();
        },
        profileSnapshot: () => ({
          activeProfileKey: state.activeProfileKey,
          profiles:
            state.activeProfileKey === PUBLISHED_PROFILE.profileKey
              ? [E290_PROFILE, PUBLISHED_PROFILE]
              : [E290_PROFILE],
        }),
        reconcileOnboardingPublication() {
          state.reconcileCalls += 1;
          if (outcome === "rejected") {
            throw new Error(`simulated reconciliation rejection ${state.reconcileCalls}`);
          }
          state.activeProfileKey = PUBLISHED_PROFILE.profileKey;
          return {
            activeProfile: PUBLISHED_PROFILE,
            finalizedActiveArtifact: outcome === "finalized",
          };
        },
      },
      createBle: () => {
        state.bleOwners += 1;
        return {
          central: {
            supported: true,
            async scan(): Promise<readonly BleCandidate[]> {
              return [SELECTED_BOARD];
            },
            async connect(_profile, options): Promise<BleConnection> {
              if (options?.peripheralId === SELECTED_BOARD.peripheralId) {
                return selectedConnection;
              }
              throw new Error("ordinary authenticated BLE target is offline");
            },
            async dispose(): Promise<void> {},
          },
          decodeCommand: () => {
            throw new Error("no platform command expected");
          },
          profile: {
            indicateCharacteristicUuid: "generated-tx",
            maximumWriteValueBytes: 20,
            securityConfirmationCharacteristicUuid: "generated-security-confirmation",
            securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
            serviceUuid: "generated-service",
            writeCharacteristicUuid: "generated-rx",
          },
        };
      },
      profileStore: PROFILE_STORE,
    };
    return {
      client: new NativeApplianceClient(async () => runtime),
      state,
    };
  }

  async function beginSelectedAdditionalAppliance(client: NativeApplianceClient): Promise<void> {
    await client.bootstrapSession();
    await client.beginAddAppliance();
    const candidates = await client.scanBleCandidates();
    expect(candidates).toEqual([SELECTED_BOARD]);
    await client.startOnboarding(candidates[0]);
  }

  test("treats a finalized Active artifact as success and reopens its authoritative owner", async () => {
    const { client, state } = publicationFailureFixture("finalized");
    await beginSelectedAdditionalAppliance(client);

    await expect(client.continueOnboarding()).resolves.toBeUndefined();

    expect(state.reconcileCalls).toBe(1);
    expect(state.connectionCloses).toBe(1);
    expect(state.nativeDestroys).toBe(1);
    expect(state.opens).toEqual(["previous", "published"]);
    expect((await client.profiles()).activeProfileKey).toBe(PUBLISHED_PROFILE.profileKey);
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "credential_ready" } },
    });

    client.dispose();
  });

  test("accepts authoritative readback after the scratch artifact was already removed", async () => {
    const { client, state } = publicationFailureFixture("already_finalized");
    await beginSelectedAdditionalAppliance(client);

    await expect(client.continueOnboarding()).resolves.toBeUndefined();

    expect(state.reconcileCalls).toBe(1);
    expect(state.opens).toEqual(["previous", "published"]);
    expect((await client.profiles()).activeProfileKey).toBe(PUBLISHED_PROFILE.profileKey);
    expect(await client.onboarding()).toMatchObject({
      snapshot: { lifecycle: { state: "credential_ready" } },
    });

    client.dispose();
  });

  test("releases onboarding, restores the prior add owner, and surfaces reconciliation rejection", async () => {
    const { client, state } = publicationFailureFixture("rejected");
    await beginSelectedAdditionalAppliance(client);

    let failure: unknown;
    try {
      await client.continueOnboarding();
    } catch (error) {
      failure = error;
    }

    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).message).toBe(
      "Secure pairing activated a credential, but profile publication recovery failed.",
    );
    expect((failure as AggregateError).errors[0]).toMatchObject({
      message: "simulated profile publication failure",
    });
    expect((failure as AggregateError).errors[1]).toMatchObject({
      message:
        "The paired credential was activated, but its profile publication could not be reconciled.",
    });
    expect(state.reconcileCalls).toBe(2);
    expect(state.connectionCloses).toBe(1);
    expect(state.nativeDestroys).toBe(1);
    expect(state.opens).toEqual(["previous", "previous"]);
    expect((await client.profiles()).activeProfileKey).toBe(E290_PROFILE.profileKey);
    await expect(client.snapshot()).resolves.toMatchObject({ revision: 0 });
    expect(await client.onboarding()).toMatchObject({
      snapshot: {
        lifecycle: { state: "faulted", reason: "protocol_or_persistence_failure" },
      },
    });

    await client.cancelOnboarding();
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
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
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
        profileSnapshot: e290ProfileSnapshot,
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
        activateProfile(): NativeProfileSummary {
          return E290_PROFILE;
        },
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
        profileSnapshot: e290ProfileSnapshot,
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
