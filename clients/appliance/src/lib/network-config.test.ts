import { describe, expect, test } from "bun:test";

import type {
  LoraRadioProfileView,
  NetworkConfigMutationRequest,
  NetworkConfigView,
  NetworkRuntimeStatusView,
} from "../generated/api.ts";
import {
  NetworkConfigController,
  type NetworkConfigurationClient,
  type NetworkPollScheduler,
} from "./network-config.ts";

interface Deferred<Value> {
  readonly promise: Promise<Value>;
  reject(error: unknown): void;
  resolve(value: Value): void;
}

interface ScheduledPoll {
  readonly callback: () => void;
  cancelled: boolean;
  readonly delayMs: number;
}

function deferred<Value>(): Deferred<Value> {
  let reject: (error: unknown) => void = () => undefined;
  let resolve: (value: Value) => void = () => undefined;
  const promise = new Promise<Value>((resolvePromise, rejectPromise) => {
    reject = rejectPromise;
    resolve = resolvePromise;
  });
  return { promise, reject, resolve };
}

function configuration(
  revision: number,
  loraTxPowerDbm: NetworkConfigView["lora_tx_power_dbm"] = 14,
  loraProfile: LoraRadioProfileView = {
    bandwidth_hz: 125_000,
    coding_rate_denominator: 5,
    frequency_hz: 915_000_000,
    spreading_factor: 7,
    tx_power_dbm: loraTxPowerDbm,
  },
): NetworkConfigView {
  return {
    automatic_announces_enabled: true,
    device_name: null,
    lora_profile: loraProfile,
    lora_tx_power_dbm: loraTxPowerDbm,
    revision,
    rmap_discovery_enabled: false,
    rmap_phone_location: null,
    rmap_share_location: false,
    tcp_peer: null,
    wifi_profiles: [],
    wifi_transport_enabled: true,
  };
}

function runtime(
  configuredRevision: number,
  appliedRevision = configuredRevision,
): NetworkRuntimeStatusView {
  return {
    active_wifi_profile: null,
    applied_revision: appliedRevision,
    configured_revision: configuredRevision,
    connected_ssid: null,
    dns_diagnostics: null,
    ipv4_address: null,
    last_tcp_failure: null,
    rmap_status: null,
    rssi_dbm: null,
    tcp_peer_state: "disabled",
    wifi_state: "disabled",
  };
}

function recordingScheduler(scheduled: ScheduledPoll[]): NetworkPollScheduler {
  return (callback, delayMs) => {
    const poll = { callback, cancelled: false, delayMs };
    scheduled.push(poll);
    return () => {
      poll.cancelled = true;
    };
  };
}

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("network configuration controller", () => {
  test("loads and polls only while one appliance's Connectivity workspace is active", async () => {
    const scheduled: ScheduledPoll[] = [];
    let statusReads = 0;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig() {
        throw new Error("not used");
      },
      async networkConfig() {
        return configuration(3);
      },
      async networkStatus() {
        statusReads += 1;
        return runtime(3);
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "11".repeat(16),
      schedule: recordingScheduler(scheduled),
    });

    expect(statusReads).toBe(0);
    await controller.activate("board-a");
    expect(controller.state).toMatchObject({
      deviceKey: "board-a",
      loadState: "ready",
      rebootRequired: false,
    });
    expect(statusReads).toBe(1);
    expect(scheduled).toHaveLength(1);
    expect(scheduled[0]?.delayMs).toBe(2_000);

    scheduled[0]?.callback();
    await flushPromises();
    expect(statusReads).toBe(2);
    expect(scheduled).toHaveLength(2);

    controller.suspend();
    expect(controller.state.loadState).toBe("ready");
    expect(scheduled[1]?.cancelled).toBe(true);
    scheduled[1]?.callback();
    await flushPromises();
    expect(statusReads).toBe(2);

    await controller.activate("board-a");
    expect(statusReads).toBe(3);
    expect(scheduled).toHaveLength(3);
  });

  test("retains the exact secret-bearing CAS request for an ambiguous retry", async () => {
    const requests: NetworkConfigMutationRequest[] = [];
    let revision = 7;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig(request) {
        requests.push(request);
        if (requests.length === 1) throw new Error("BLE disconnected after write");
        revision = 8;
        return { outcome: "applied", reboot_required: true, revision };
      },
      async networkConfig() {
        return configuration(revision);
      },
      async networkStatus() {
        return runtime(revision, 7);
      },
    };
    let identityCreations = 0;
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => {
        identityCreations += 1;
        return "22".repeat(16);
      },
    });
    await controller.activate("board-a");

    await controller.mutate({
      credential: { kind: "replace", passphrase: "alpha-demo-secret" },
      enabled: true,
      kind: "upsert_wifi",
      priority: 200,
      profile_id: "33".repeat(16),
      ssid: { encoding: "utf8", value: "Field Mesh" },
    });
    expect(controller.state.mutation).toEqual({
      error: "BLE disconnected after write",
      state: "retryable_error",
    });

    controller.suspend();
    await controller.activate("board-a");
    await controller.retryMutation();

    expect(requests).toHaveLength(2);
    expect(requests[1]).toBe(requests[0] as NetworkConfigMutationRequest);
    expect(requests[0]).toMatchObject({
      expected_revision: 7,
      idempotency_key: "22".repeat(16),
    });
    expect(identityCreations).toBe(1);
    expect(controller.state).toMatchObject({
      configuration: { revision: 8 },
      mutation: { revision: 8, state: "applied" },
      rebootRequired: true,
    });
  });

  test("treats a typed revision conflict as definitive and refreshes authority", async () => {
    let configReads = 0;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig() {
        return { current_revision: 9, outcome: "revision_conflict" };
      },
      async networkConfig() {
        configReads += 1;
        return configuration(configReads === 1 ? 7 : 9);
      },
      async networkStatus() {
        return runtime(configReads === 0 ? 7 : 9);
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "44".repeat(16),
    });
    await controller.activate("board-a");

    await controller.mutate({
      kind: "replace_tcp_peer",
      peer: { enabled: true, ipv4_address: "192.0.2.10", port: 4242 },
    });

    expect(configReads).toBe(2);
    expect(controller.state).toMatchObject({
      configuration: { revision: 9 },
      mutation: { currentRevision: 9, state: "revision_conflict" },
    });
    expect(await controller.retryMutation()).toBeNull();
  });

  test("rejects stale load results after the active board changes", async () => {
    const configurations = [deferred<NetworkConfigView>(), deferred<NetworkConfigView>()];
    const statuses = [deferred<NetworkRuntimeStatusView>(), deferred<NetworkRuntimeStatusView>()];
    let configRead = 0;
    let statusRead = 0;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig() {
        throw new Error("not used");
      },
      networkConfig() {
        const result = configurations[configRead];
        configRead += 1;
        if (result === undefined) throw new Error("unexpected configuration read");
        return result.promise;
      },
      networkStatus() {
        const result = statuses[statusRead];
        statusRead += 1;
        if (result === undefined) throw new Error("unexpected status read");
        return result.promise;
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "55".repeat(16),
    });

    const boardA = controller.activate("board-a");
    const boardB = controller.activate("board-b");
    configurations[1]?.resolve(configuration(20));
    statuses[1]?.resolve(runtime(20));
    await boardB;
    expect(controller.state).toMatchObject({
      configuration: { revision: 20 },
      deviceKey: "board-b",
    });

    configurations[0]?.resolve(configuration(10));
    statuses[0]?.resolve(runtime(10));
    await boardA;
    expect(controller.state).toMatchObject({
      configuration: { revision: 20 },
      deviceKey: "board-b",
    });
  });

  test("allows only one mutation at a time", async () => {
    const pending = deferred<{
      outcome: "applied";
      reboot_required: boolean;
      revision: number;
    }>();
    let writes = 0;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig() {
        writes += 1;
        return pending.promise;
      },
      async networkConfig() {
        return configuration(1);
      },
      async networkStatus() {
        return runtime(1);
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "66".repeat(16),
    });
    await controller.activate("board-a");

    const first = controller.mutate({ kind: "replace_tcp_peer", peer: null });
    const second = await controller.mutate({ kind: "remove_wifi", profile_id: "77".repeat(16) });
    expect(second).toBeNull();
    expect(writes).toBe(1);
    pending.resolve({ outcome: "applied", reboot_required: false, revision: 2 });
    await first;
  });

  test("persists an exact qualified LoRa power selection for the next restart", async () => {
    const requests: NetworkConfigMutationRequest[] = [];
    let revision = 4;
    let loraTxPowerDbm: NetworkConfigView["lora_tx_power_dbm"] = 14;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig(request) {
        requests.push(request);
        if (request.mutation.kind !== "set_lora_tx_power") {
          throw new Error("unexpected mutation");
        }
        loraTxPowerDbm = request.mutation.lora_tx_power_dbm;
        revision += 1;
        return { outcome: "applied", reboot_required: true, revision };
      },
      async networkConfig() {
        return configuration(revision, loraTxPowerDbm);
      },
      async networkStatus() {
        return runtime(revision, 4);
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "88".repeat(16),
    });
    await controller.activate("board-a");

    await controller.mutate({ kind: "set_lora_tx_power", lora_tx_power_dbm: 22 });

    expect(requests).toEqual([
      {
        expected_revision: 4,
        idempotency_key: "88".repeat(16),
        mutation: { kind: "set_lora_tx_power", lora_tx_power_dbm: 22 },
      },
    ]);
    expect(controller.state).toMatchObject({
      configuration: { lora_tx_power_dbm: 22, revision: 5 },
      mutation: { rebootRequired: true, revision: 5, state: "applied" },
      rebootRequired: true,
    });
  });

  test("persists one complete LoRa profile atomically for the next restart", async () => {
    const requests: NetworkConfigMutationRequest[] = [];
    const target: LoraRadioProfileView = {
      bandwidth_hz: 250_000,
      coding_rate_denominator: 8,
      frequency_hz: 916_000_000,
      spreading_factor: 10,
      tx_power_dbm: 22,
    };
    let loraProfile: LoraRadioProfileView = configuration(9).lora_profile;
    let revision = 9;
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig(request) {
        requests.push(request);
        if (request.mutation.kind !== "set_lora_profile") {
          throw new Error("unexpected mutation");
        }
        loraProfile = request.mutation.profile;
        revision += 1;
        return { outcome: "applied", reboot_required: true, revision };
      },
      async networkConfig() {
        return configuration(revision, loraProfile.tx_power_dbm, loraProfile);
      },
      async networkStatus() {
        return runtime(revision, 9);
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "99".repeat(16),
    });
    await controller.activate("board-a");

    await controller.mutate({ kind: "set_lora_profile", profile: target });

    expect(requests).toEqual([
      {
        expected_revision: 9,
        idempotency_key: "99".repeat(16),
        mutation: { kind: "set_lora_profile", profile: target },
      },
    ]);
    expect(controller.state).toMatchObject({
      configuration: { lora_profile: target, revision: 10 },
      mutation: { rebootRequired: true, revision: 10, state: "applied" },
      rebootRequired: true,
    });
  });

  test("disables the Wi-Fi radio without changing announce policy or saved connectivity", async () => {
    const requests: NetworkConfigMutationRequest[] = [];
    const wifiProfile = {
      credential_configured: true,
      enabled: true,
      priority: 180,
      profile_id: "aa".repeat(16),
      ssid: { encoding: "utf8" as const, value: "Field Mesh" },
    };
    const tcpPeer = {
      enabled: true,
      hostname: "peer.example.org",
      port: 4242,
    };
    let current: NetworkConfigView = {
      ...configuration(12),
      automatic_announces_enabled: false,
      tcp_peer: tcpPeer,
      wifi_profiles: [wifiProfile],
    };
    const client: NetworkConfigurationClient = {
      async mutateNetworkConfig(request) {
        requests.push(request);
        current = { ...current, revision: 13, wifi_transport_enabled: false };
        return { outcome: "applied", reboot_required: true, revision: 13 };
      },
      async networkConfig() {
        return current;
      },
      async networkStatus() {
        return runtime(current.revision, 12);
      },
    };
    const controller = new NetworkConfigController(client, {
      createIdempotencyKey: () => "bb".repeat(16),
    });
    await controller.activate("board-a");

    await controller.mutate({
      automatic_announces_enabled: false,
      kind: "set_gateway_policy",
      wifi_transport_enabled: false,
    });

    expect(requests).toEqual([
      {
        expected_revision: 12,
        idempotency_key: "bb".repeat(16),
        mutation: {
          automatic_announces_enabled: false,
          kind: "set_gateway_policy",
          wifi_transport_enabled: false,
        },
      },
    ]);
    expect(controller.state).toMatchObject({
      configuration: {
        automatic_announces_enabled: false,
        revision: 13,
        tcp_peer: tcpPeer,
        wifi_profiles: [wifiProfile],
        wifi_transport_enabled: false,
      },
      mutation: { rebootRequired: true, revision: 13, state: "applied" },
      rebootRequired: true,
    });
  });
});
