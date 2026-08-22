import { describe, expect, test } from "bun:test";
import type {
  NativeApplianceLike,
  NativeBridgeContract,
  NativePrnsNodeLike,
  NativePrnsOtaPhase,
  NativePrnsOtaSlot,
  NativeProfileStoreLike,
  NativeProfileStoreSnapshot,
  NativeProfileSummary,
} from "@reticulum/appliance-native";

import {
  type NativeApplianceBridge,
  NativeApplianceClient,
  type NativeApplianceRuntime,
  type NativePrnsBridge,
} from "./native-appliance-client.ts";
import type { NativeErrorPredicate } from "./native-error.ts";

const MANAGEMENT = "11".repeat(16);
const LXMF = "22".repeat(16);
const SECOND_MANAGEMENT = "33".repeat(16);
const SECOND_LXMF = "44".repeat(16);

const CONTRACT = {
  bridgeApiMajor: 3,
  bridgeApiMinor: 2,
  deviceApiMajor: 6,
  deviceApiMinor: 0,
  maxMessageBytes: 512,
  maxLxmfReadChunkBytes: 416,
  maxLxmfBasicTitleBytes: 295,
  maxLxmfBasicContentBytes: 295,
  maxNomadPagePathBytes: 128,
  maxNomadPageBytes: 400,
  maxNomadRequestTimestampUnixMs: 9_007_199_254_740_991n,
} satisfies NativeBridgeContract;

interface Harness {
  readonly client: NativeApplianceClient;
  readonly state: {
    active: string | undefined;
    authorized: Set<string>;
    closeCalls: number;
    destroyed: number;
    enrollCalls: string[];
    ensureCalls: number;
    openCalls: number;
    profiles: NativeProfileSummary[];
    prnsClosed: number;
    rebootCalls: string[];
    stageCalls: { destination: string; imageBytes: number; version: string }[];
  };
}

function profile(managementDestination: string, lxmfDestination: string): NativeProfileSummary {
  return {
    profileKey: managementDestination,
    managementDestination,
    lxmfDestination,
  };
}

function harness(initialProfiles: NativeProfileSummary[] = []): Harness {
  const state = {
    active: initialProfiles[0]?.profileKey,
    authorized: new Set<string>(),
    closeCalls: 0,
    destroyed: 0,
    enrollCalls: [] as string[],
    ensureCalls: 0,
    openCalls: 0,
    profiles: [...initialProfiles],
    prnsClosed: 0,
    rebootCalls: [] as string[],
    stageCalls: [] as { destination: string; imageBytes: number; version: string }[],
  };
  const store = {} as NativeProfileStoreLike;
  const snapshot = (): NativeProfileStoreSnapshot => ({
    activeProfileKey: state.active,
    profiles: [...state.profiles],
  });
  const appliance = {
    close: async () => {
      state.closeCalls += 1;
    },
    contactsJson: async () => JSON.stringify([{ destination: LXMF, name: "Field node" }]),
    ensureConnected: async () => {
      state.ensureCalls += 1;
    },
    reconnect: async () => undefined,
    snapshotJson: () =>
      JSON.stringify({
        connection: { state: "unavailable", transport: "reticulum" },
        contact_count: 0,
        imported_this_run: 0,
        last_error: null,
        local_lxmf_destination: null,
        pending_outbox: 0,
        revision: 1,
      }),
    syncNow: async () => undefined,
  } as unknown as NativeApplianceLike;
  const bridge: NativeApplianceBridge = {
    contract: CONTRACT,
    isNativeError: (() => false) as unknown as NativeErrorPredicate,
    activateProfile(_store, profileKey) {
      const found = state.profiles.find((candidate) => candidate.profileKey === profileKey);
      if (found === undefined) throw new Error("missing profile");
      state.active = found.profileKey;
      return found;
    },
    destroy() {
      state.destroyed += 1;
    },
    destroyProfileStore() {},
    forgetProfile(_store, profileKey) {
      state.profiles = state.profiles.filter((candidate) => candidate.profileKey !== profileKey);
      return snapshot();
    },
    open() {
      state.openCalls += 1;
      return appliance;
    },
    profileSnapshot: snapshot,
    rememberProfile(_store, managementDestination, lxmfDestination) {
      const existing = state.profiles.find(
        (candidate) => candidate.profileKey === managementDestination,
      );
      const remembered = existing ?? profile(managementDestination, lxmfDestination);
      if (existing === undefined) state.profiles.push(remembered);
      state.active = remembered.profileKey;
      return remembered;
    },
  };
  const prns: NativePrnsBridge = {
    node: {} as NativePrnsNodeLike,
    close() {
      state.prnsClosed += 1;
    },
    async enroll(destinationHash) {
      state.enrollCalls.push(destinationHash);
      state.authorized.add(destinationHash);
    },
    managementCandidates: () => [
      { destinationHash: MANAGEMENT, hops: 1, interfaceId: "aa".repeat(8) },
      { destinationHash: SECOND_MANAGEMENT, hops: 2, interfaceId: "bb".repeat(8) },
    ],
    async managementIdentity(destinationHash) {
      if (!state.authorized.has(destinationHash)) throw new Error("not authorized");
      return {
        managementDestination: destinationHash,
        lxmfDestination: destinationHash === MANAGEMENT ? LXMF : SECOND_LXMF,
      };
    },
    async publicIdentity(destinationHash) {
      return {
        managementDestination: destinationHash,
        lxmfDestination: destinationHash === MANAGEMENT ? LXMF : SECOND_LXMF,
      };
    },
    async rebootOta(destinationHash) {
      state.rebootCalls.push(destinationHash);
      return {
        failure: undefined,
        imageBytes: 4,
        nextChunk: 1,
        phase: 2 as NativePrnsOtaPhase,
        resourceArmed: false,
        session: "55".repeat(16),
        slot: 1 as NativePrnsOtaSlot,
        verifiedBytes: 4,
        version: "v1",
      };
    },
    async stageOta(destinationHash, image, version) {
      state.stageCalls.push({
        destination: destinationHash,
        imageBytes: image.byteLength,
        version,
      });
      return {
        failure: undefined,
        imageBytes: image.byteLength,
        nextChunk: 1,
        phase: 2 as NativePrnsOtaPhase,
        resourceArmed: false,
        session: "55".repeat(16),
        slot: 1 as NativePrnsOtaSlot,
        verifiedBytes: image.byteLength,
        version,
      };
    },
  };
  const runtime: NativeApplianceRuntime = { bridge, prns, profileStore: store };
  return { client: new NativeApplianceClient(async () => runtime), state };
}

async function disposeHarness(test: Harness): Promise<void> {
  test.client.dispose();
  for (let attempt = 0; attempt < 20 && test.state.prnsClosed === 0; attempt += 1) {
    await Bun.sleep(1);
  }
  expect(test.state.prnsClosed).toBe(1);
}

describe("native PRNS appliance client", () => {
  test("bootstrap opens one offline application owner", async () => {
    const test = harness();
    await test.client.bootstrapSession();
    await test.client.bootstrapSession();
    expect(test.state.openCalls).toBe(1);
    expect((await test.client.snapshot()).connection.state).toBe("unavailable");
    await disposeHarness(test);
  });

  test("discovery returns only verified Reticulum application facts", async () => {
    const test = harness();
    await test.client.bootstrapSession();
    expect(await test.client.scanReticulumCandidates()).toEqual([
      {
        managementDestination: MANAGEMENT,
        lxmfDestination: LXMF,
        interfaceId: "aa".repeat(8),
        hops: 1,
      },
      {
        managementDestination: SECOND_MANAGEMENT,
        lxmfDestination: SECOND_LXMF,
        interfaceId: "bb".repeat(8),
        hops: 2,
      },
    ]);
    await disposeHarness(test);
  });

  test("first enrollment authorizes then remembers the management destination", async () => {
    const test = harness();
    await test.client.bootstrapSession();
    await test.client.startOnboarding({
      managementDestination: MANAGEMENT,
      lxmfDestination: LXMF,
      interfaceId: "aa".repeat(8),
      hops: 1,
    });
    expect(test.state.enrollCalls).toEqual([MANAGEMENT]);
    expect(test.state.active).toBe(MANAGEMENT);
    expect(test.state.profiles).toEqual([profile(MANAGEMENT, LXMF)]);
    expect(test.state.openCalls).toBe(2);
    expect(test.state.ensureCalls).toBe(1);
    expect((await test.client.onboarding()).lifecycle.state).toBe("ready");
    await disposeHarness(test);
  });

  test("an already authorized identity skips physical enrollment", async () => {
    const test = harness();
    test.state.authorized.add(MANAGEMENT);
    await test.client.bootstrapSession();
    await test.client.startOnboarding({
      managementDestination: MANAGEMENT,
      lxmfDestination: LXMF,
      interfaceId: "aa".repeat(8),
      hops: 1,
    });
    expect(test.state.enrollCalls).toEqual([]);
    await disposeHarness(test);
  });

  test("profile switching reopens the actor against the selected destination", async () => {
    const test = harness([profile(MANAGEMENT, LXMF), profile(SECOND_MANAGEMENT, SECOND_LXMF)]);
    await test.client.bootstrapSession();
    await test.client.activateProfile(SECOND_MANAGEMENT);
    expect(test.state.active).toBe(SECOND_MANAGEMENT);
    expect(test.state.closeCalls).toBe(1);
    expect(test.state.openCalls).toBe(2);
    await disposeHarness(test);
  });

  test("local application methods remain available through the native actor", async () => {
    const test = harness([profile(MANAGEMENT, LXMF)]);
    await test.client.bootstrapSession();
    expect(await test.client.contacts()).toEqual([{ destination: LXMF, name: "Field node" }]);
    await test.client.sync();
    await test.client.ensureConnected();
    await test.client.reconnect();
    await disposeHarness(test);
  });

  test("firmware operations use the active profile management destination", async () => {
    const test = harness([profile(MANAGEMENT, LXMF)]);
    await test.client.bootstrapSession();
    const image = new Uint8Array([0xe9, 1, 2, 3]).buffer;
    const staged = await test.client.stageFirmwareUpdate(image, "v1");
    expect(staged.phase).toBe(2);
    expect(test.state.stageCalls).toEqual([
      { destination: MANAGEMENT, imageBytes: 4, version: "v1" },
    ]);
    await test.client.rebootIntoStagedFirmware();
    expect(test.state.rebootCalls).toEqual([MANAGEMENT]);
    await disposeHarness(test);
  });

  test("dispose closes the actor before releasing PRNS ownership", async () => {
    const test = harness();
    await test.client.bootstrapSession();
    await disposeHarness(test);
    expect(test.state.closeCalls).toBe(1);
    expect(test.state.destroyed).toBe(1);
    expect(test.state.prnsClosed).toBe(1);
  });
});
