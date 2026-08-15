import { describe, expect, spyOn, test } from "bun:test";

import type { NativeApplianceLike, NativeBlePlatformCommand } from "@reticulum/appliance-native";

import type {
  BleCandidate,
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleGattProfile,
  BleScanOptions,
} from "./ble-central-types.ts";
import {
  ensureForegroundConnection,
  ForegroundReconnect,
  type RetryScheduler,
} from "./foreground-reconnect.ts";
import {
  type DecodedNativeBlePlatformCommand,
  NativeBleOnboardingTransport,
  NativeBleTransport,
  PLATFORM_BLE_BOND_REPAIR_TIMEOUT_MS,
} from "./native-ble-transport.ts";

const PROFILE: BleGattProfile = {
  indicateCharacteristicUuid: "tx",
  maximumWriteValueBytes: 20,
  securityConfirmationCharacteristicUuid: "security-confirmation",
  securityConfirmationReadyValue: Uint8Array.of(0x52, 0x44, 0x59, 0x31),
  serviceUuid: "service",
  writeCharacteristicUuid: "rx",
};

function deferred<T>(): {
  readonly promise: Promise<T>;
  readonly reject: (error: unknown) => void;
  readonly resolve: (value: T) => void;
} {
  let resolve = (_value: T) => {};
  let reject = (_error: unknown) => {};
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, reject, resolve };
}

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  const deadline = performance.now() + 1_000;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await Bun.sleep(1);
  }
}

function encoded(command: DecodedNativeBlePlatformCommand): NativeBlePlatformCommand {
  return command as unknown as NativeBlePlatformCommand;
}

class FakeBleConnection implements BleConnection {
  readonly maxWriteWithResponseBytes = 20;
  readonly name = "E290";
  readonly peripheralId: string;

  closeCount = 0;
  closeBehaviors: Array<() => Promise<void>> = [];
  observer: BleConnectionObserver | null = null;
  defaultReadValue = new Uint8Array(PROFILE.securityConfirmationReadyValue);
  readBehaviors: Array<() => Promise<Uint8Array>> = [];
  reads: string[] = [];
  writeBehaviors: Array<(chunk: Uint8Array) => Promise<void>> = [];
  writes: number[][] = [];

  constructor(peripheralId = "peripheral-1") {
    this.peripheralId = peripheralId;
  }

  observe(observer: BleConnectionObserver): () => void {
    this.observer = observer;
    return () => {
      if (this.observer === observer) this.observer = null;
    };
  }

  async write(chunk: Uint8Array): Promise<void> {
    this.writes.push([...chunk]);
    const behavior = this.writeBehaviors.shift();
    await behavior?.(chunk);
  }

  async read(characteristicUuid: string): Promise<Uint8Array> {
    this.reads.push(characteristicUuid);
    return (await this.readBehaviors.shift()?.()) ?? new Uint8Array(this.defaultReadValue);
  }

  async close(): Promise<void> {
    this.closeCount += 1;
    await this.closeBehaviors.shift()?.();
  }

  emitBytes(...bytes: number[]): void {
    this.observer?.onBytes(Uint8Array.from(bytes));
  }

  emitDisconnect(reason: string): void {
    this.observer?.onDisconnect({ peripheralId: this.peripheralId, reason });
  }
}

class FakeBleCentral implements BleCentral {
  readonly supported = true;

  connectCount = 0;
  connectOptions: BleConnectOptions[] = [];
  disposeCount = 0;
  profiles: BleGattProfile[] = [];
  results: Array<(options?: BleConnectOptions) => Promise<BleConnection>> = [];
  scanOptions: BleScanOptions[] = [];
  scanResults: Array<
    (serviceUuid: string, options?: BleScanOptions) => Promise<readonly BleCandidate[]>
  > = [];
  scannedServiceUuids: string[] = [];

  async scan(serviceUuid: string, options?: BleScanOptions): Promise<readonly BleCandidate[]> {
    this.scannedServiceUuids.push(serviceUuid);
    this.scanOptions.push(options ?? {});
    const result = this.scanResults.shift();
    if (result === undefined) throw new Error("fake BLE central has no scan result");
    return result(serviceUuid, options);
  }

  async connect(profile: BleGattProfile, options?: BleConnectOptions): Promise<BleConnection> {
    this.connectCount += 1;
    this.connectOptions.push(options ?? {});
    this.profiles.push(profile);
    const result = this.results.shift();
    if (result === undefined) throw new Error("fake BLE central has no connection result");
    return result(options);
  }

  async dispose(): Promise<void> {
    this.disposeCount += 1;
  }
}

type NativeBleAppliance = Pick<
  NativeApplianceLike,
  | "bleDisconnected"
  | "bleIngestIndication"
  | "bleLinkConnected"
  | "bleNextPlatformCommand"
  | "bleWriteFailed"
  | "bleWriteSucceeded"
  | "ensureConnected"
  | "reconnect"
>;

class FakeNativeBleAppliance implements NativeBleAppliance {
  acknowledgements: Array<
    | { readonly generation: bigint; readonly outcome: "succeeded"; readonly token: bigint }
    | {
        readonly generation: bigint;
        readonly outcome: "failed";
        readonly reason: string;
        readonly token: bigint;
      }
  > = [];
  commands: NativeBlePlatformCommand[] = [];
  disconnected: Array<{ readonly generation: bigint; readonly reason: string }> = [];
  events: string[] = [];
  indications: Array<{ readonly bytes: number[]; readonly generation: bigint }> = [];
  nextGeneration = 1n;
  reconnectError: Error | null = null;

  #waiter:
    | {
        readonly reject: (error: unknown) => void;
        readonly resolve: (command: NativeBlePlatformCommand | undefined) => void;
      }
    | undefined;

  bleDisconnected(generation: bigint, reason: string): void {
    this.disconnected.push({ generation, reason });
    this.events.push(`disconnected ${generation}`);
  }

  bleIngestIndication(generation: bigint, bytes: ArrayBuffer): void {
    this.indications.push({ generation, bytes: [...new Uint8Array(bytes)] });
  }

  bleLinkConnected(peripheralId: string, maxWriteBytes: number): bigint {
    const generation = this.nextGeneration;
    this.nextGeneration += 1n;
    this.events.push(`link ${generation} ${peripheralId} ${maxWriteBytes}`);
    return generation;
  }

  bleNextPlatformCommand(
    _generation: bigint,
    asyncOptions?: { signal: AbortSignal },
  ): Promise<NativeBlePlatformCommand | undefined> {
    const queued = this.commands.shift();
    if (queued !== undefined) return Promise.resolve(queued);

    const waiting = deferred<NativeBlePlatformCommand | undefined>();
    const onAbort = () => waiting.reject(new Error("aborted"));
    asyncOptions?.signal.addEventListener("abort", onAbort, { once: true });
    this.#waiter = {
      reject: waiting.reject,
      resolve: (command) => {
        asyncOptions?.signal.removeEventListener("abort", onAbort);
        waiting.resolve(command);
      },
    };
    return waiting.promise;
  }

  bleWriteFailed(generation: bigint, token: bigint, reason: string): void {
    this.acknowledgements.push({ generation, token, outcome: "failed", reason });
  }

  bleWriteSucceeded(generation: bigint, token: bigint): void {
    this.acknowledgements.push({ generation, token, outcome: "succeeded" });
  }

  async reconnect(): Promise<void> {
    this.events.push("reconnect");
    if (this.reconnectError !== null) throw this.reconnectError;
  }

  async ensureConnected(): Promise<void> {
    this.events.push("ensure connected");
  }

  queue(command: DecodedNativeBlePlatformCommand): void {
    const next = encoded(command);
    const waiter = this.#waiter;
    this.#waiter = undefined;
    if (waiter === undefined) {
      this.commands.push(next);
    } else {
      waiter.resolve(next);
    }
  }
}

function transport(
  appliance: FakeNativeBleAppliance,
  central: FakeBleCentral,
  writeTimeoutMs = 100,
  peripheralName?: string,
  recoveryPeripheralName?: string,
): NativeBleTransport {
  return new NativeBleTransport(
    appliance,
    {
      central,
      decodeCommand: (command) => command as unknown as DecodedNativeBlePlatformCommand,
      peripheralName,
      profile: PROFILE,
      recoveryPeripheralName,
    },
    writeTimeoutMs,
  );
}

function onboardingTransport(
  onboarding: FakeNativeBleAppliance,
  central: FakeBleCentral,
): NativeBleOnboardingTransport {
  return new NativeBleOnboardingTransport(onboarding, {
    central,
    decodeCommand: (command) => command as unknown as DecodedNativeBlePlatformCommand,
    // The onboarding transport must ignore diagnostic name targeting and use
    // only the candidate explicitly selected by platform identifier.
    peripheralName: "must-not-be-used",
    profile: PROFILE,
  });
}

describe("native BLE onboarding transport orchestration", () => {
  test("connects only the selected peripheral without waking the ordinary actor", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("board-b");
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);

    await owner.connectSelected("board-b");

    expect(central.connectOptions).toHaveLength(1);
    expect(central.connectOptions[0]?.peripheralId).toBe("board-b");
    expect(central.connectOptions[0]?.peripheralName).toBeUndefined();
    expect(onboarding.events).toEqual(["link 1 board-b 20"]);
    expect(owner.usable).toBeTrue();
    expect(owner.failureReason).toBeNull();
    await owner.dispose();
  });

  test("retries authenticated readiness without emitting protocol bytes", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("board-b");
    connection.readBehaviors.push(
      () => Promise.reject(new Error("insufficient authentication")),
      () => Promise.resolve(Uint8Array.of(0x57, 0x41, 0x49, 0x54)),
      () => Promise.resolve(new Uint8Array(PROFILE.securityConfirmationReadyValue)),
    );
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);
    await owner.connectSelected("board-b");

    await owner.confirmAuthenticated(100, 1);

    expect(connection.reads).toEqual([
      PROFILE.securityConfirmationCharacteristicUuid,
      PROFILE.securityConfirmationCharacteristicUuid,
      PROFILE.securityConfirmationCharacteristicUuid,
    ]);
    expect(connection.writes).toEqual([]);
    expect(onboarding.acknowledgements).toEqual([]);
    await owner.dispose();
  });

  test("does not impose an app deadline while firmware still owns physical presence", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("board-b");
    let simulatedNow = 1_000;
    connection.readBehaviors.push(
      async () => {
        simulatedNow += 60_000;
        return Uint8Array.of(0x57, 0x41, 0x49, 0x54);
      },
      () => Promise.resolve(new Uint8Array(PROFILE.securityConfirmationReadyValue)),
    );
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);
    await owner.connectSelected("board-b");
    const clock = spyOn(Date, "now").mockImplementation(() => simulatedNow);

    try {
      await owner.confirmAuthenticated(undefined, 1);
    } finally {
      clock.mockRestore();
    }

    expect(connection.reads).toEqual([
      PROFILE.securityConfirmationCharacteristicUuid,
      PROFILE.securityConfirmationCharacteristicUuid,
    ]);
    expect(connection.writes).toEqual([]);
    expect(onboarding.acknowledgements).toEqual([]);
    await owner.dispose();
  });

  test("times out a pending readiness marker without emitting or failing a protocol write", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("board-b");
    connection.defaultReadValue = Uint8Array.of(0x57, 0x41, 0x49, 0x54);
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);
    await owner.connectSelected("board-b");

    await expect(owner.confirmAuthenticated(5, 1)).rejects.toThrow(
      "did not become application-ready",
    );

    expect(connection.reads.length).toBeGreaterThan(0);
    expect(connection.writes).toEqual([]);
    expect(onboarding.acknowledgements).toEqual([]);
    await owner.dispose();
  });

  test("surfaces a disconnect during readiness without emitting protocol bytes", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("board-b");
    connection.readBehaviors.push(async () => {
      connection.emitDisconnect("board left during Bluetooth security");
      throw new Error("read cancelled by disconnect");
    });
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);
    await owner.connectSelected("board-b");

    await expect(owner.confirmAuthenticated(100, 1)).rejects.toThrow(
      "disconnected during security confirmation",
    );

    expect(connection.writes).toEqual([]);
    expect(onboarding.acknowledgements).toEqual([]);
    await owner.dispose();
  });

  test("relays opaque indication and write bytes without interpreting their contents", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("selected-board");
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);
    await owner.connectSelected("selected-board");

    connection.emitBytes(0xa5, 0x00, 0xff);
    onboarding.queue({
      bytes: Uint8Array.of(0x72, 0x64, 0x61, 0x31).buffer,
      generation: 1n,
      kind: "write",
      token: 8n,
    });
    await waitFor(() => onboarding.acknowledgements.length === 1, "onboarding write");

    expect(onboarding.indications).toEqual([{ bytes: [0xa5, 0x00, 0xff], generation: 1n }]);
    expect(connection.writes).toEqual([[0x72, 0x64, 0x61, 0x31]]);
    expect(onboarding.acknowledgements).toEqual([
      { generation: 1n, outcome: "succeeded", token: 8n },
    ]);
    await owner.dispose();
  });

  test("retains a secret-free platform reason when the onboarding write fails", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("selected-board");
    connection.writeBehaviors.push(() => Promise.reject(new Error("insufficient authentication")));
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);
    await owner.connectSelected("selected-board");

    onboarding.queue({
      bytes: Uint8Array.of(1).buffer,
      generation: 1n,
      kind: "write",
      token: 3n,
    });
    await waitFor(() => onboarding.acknowledgements.length === 1, "failed onboarding write");

    expect(owner.failureReason).toBe("BLE GATT write failed: insufficient authentication");
    await owner.dispose();
  });

  test("retains the platform reason when the selected link disconnects during setup", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("selected-board");
    connection.observe = (observer): (() => void) => {
      connection.observer = observer;
      queueMicrotask(() => {
        observer.onDisconnect({
          peripheralId: connection.peripheralId,
          reason: "selected board disconnected during subscription",
        });
      });
      return () => {
        if (connection.observer === observer) connection.observer = null;
      };
    };
    central.results.push(() => Promise.resolve(connection));
    const owner = onboardingTransport(onboarding, central);

    await expect(owner.connectSelected("selected-board")).rejects.toThrow(
      "disconnected during onboarding setup",
    );

    expect(owner.failureReason).toBe("selected board disconnected during subscription");
    expect(onboarding.disconnected).toEqual([
      { generation: 1n, reason: "selected board disconnected during subscription" },
    ]);
    await owner.dispose();
  });

  test("cancels a pending exact-candidate connection without registering a generation", async () => {
    const onboarding = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    central.results.push(
      (options) =>
        new Promise<BleConnection>((_resolve, reject) => {
          options?.signal?.addEventListener("abort", () => reject(options.signal?.reason), {
            once: true,
          });
        }),
    );
    const owner = onboardingTransport(onboarding, central);
    const connecting = owner.connectSelected("board-c");
    await waitFor(() => central.connectCount === 1, "onboarding connection");

    await owner.disconnect("cancelled from setup UI");
    await expect(connecting).rejects.toThrow("cancelled from setup UI");

    expect(onboarding.events).toEqual([]);
    await owner.dispose();
  });
});

describe("native BLE transport orchestration", () => {
  test("forwards credential-free scans without starting a link or invoking the native actor", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const candidates = [
      { peripheralId: "board-a", peripheralName: "Reticulum A", rssi: -54 },
    ] as const;
    central.scanResults.push(() => Promise.resolve(candidates));
    const owner = transport(appliance, central);
    const abort = new AbortController();

    await expect(owner.scan({ scanTimeoutMs: 25, signal: abort.signal })).resolves.toEqual(
      candidates,
    );

    expect(central.scannedServiceUuids).toEqual([PROFILE.serviceUuid]);
    expect(central.scanOptions).toEqual([{ scanTimeoutMs: 25, signal: abort.signal }]);
    expect(central.connectCount).toBe(0);
    expect(appliance.events).toEqual([]);
    await owner.dispose();
  });

  test("does not permit discovery after the authenticated transport has started", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    central.results.push(() => Promise.reject(new Error("offline")));
    const owner = transport(appliance, central);

    owner.start();
    await waitFor(() => central.connectCount === 1, "initial BLE attempt");

    await expect(owner.scan({ scanTimeoutMs: 1 })).rejects.toThrow(
      "unavailable after the authenticated transport starts",
    );
    expect(central.scannedServiceUuids).toEqual([]);
    await owner.dispose();
  });

  test("keeps initial failure offline and explicit reconnect retries the whole BLE link", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    central.results.push(
      () => Promise.reject(new Error("Bluetooth unavailable")),
      () => Promise.resolve(connection),
    );
    const owner = transport(appliance, central);

    expect(owner.start()).toBeUndefined();
    await waitFor(() => central.connectCount === 1, "initial BLE attempt");
    await Bun.sleep(0);
    await owner.reconnect();

    expect(central.connectCount).toBe(2);
    expect(central.profiles).toEqual([PROFILE, PROFILE]);
    expect(appliance.events).toEqual(["reconnect", "link 1 peripheral-1 20", "ensure connected"]);
    await owner.dispose();
  });

  test("forwards the configured advertised name on initial and explicit reconnects", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    central.results.push(
      () => Promise.resolve(new FakeBleConnection("first")),
      () => Promise.resolve(new FakeBleConnection("second")),
    );
    const owner = transport(appliance, central, 100, "reticulum-e290-e13e88");

    owner.start();
    await waitFor(() => appliance.events.includes("ensure connected"), "initial actor wake");
    await owner.reconnect();

    expect(central.connectOptions.map(({ peripheralName }) => peripheralName)).toEqual([
      "reticulum-e290-e13e88",
      "reticulum-e290-e13e88",
    ]);
    expect(central.connectOptions[0]?.signal).toBeDefined();
    expect(central.connectOptions[1]?.signal).toBeDefined();
    expect(central.connectOptions[0]?.signal).not.toBe(central.connectOptions[1]?.signal);
    await owner.dispose();
  });

  test("accepts a credential-derived target before start and rejects live retargeting", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    central.results.push(() => Promise.resolve(new FakeBleConnection("credential-device")));
    const owner = transport(appliance, central, 100, "environment-fallback");

    owner.configurePeripheralName("reticulum-e290-e13e88");
    owner.start();
    await waitFor(() => appliance.events.includes("ensure connected"), "credential-targeted link");

    expect(central.connectOptions[0]?.peripheralName).toBe("reticulum-e290-e13e88");
    expect(() => owner.configurePeripheralName("reticulum-e290-e13e88")).not.toThrow();
    expect(() => owner.configurePeripheralName("reticulum-e290-e13f88")).toThrow(
      "must be selected before the transport starts",
    );
    await owner.dispose();
  });

  test("destructively reconnects before registration, then non-destructively wakes the actor", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    central.results.push(() => Promise.resolve(connection));
    const owner = transport(appliance, central);

    await owner.reconnect();
    connection.emitBytes(1, 2, 3);

    expect(appliance.events).toEqual(["reconnect", "link 1 peripheral-1 20", "ensure connected"]);
    expect(appliance.indications).toEqual([{ generation: 1n, bytes: [1, 2, 3] }]);
    await owner.dispose();
  });

  test("repairs a stale bond before registering or sending authenticated protocol bytes", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const securityConnection = new FakeBleConnection("bond-repair-board");
    const authenticatedConnection = new FakeBleConnection("bond-repair-board");
    const authenticated = deferred<Uint8Array>();
    securityConnection.readBehaviors.push(() => authenticated.promise);
    central.results.push(
      () => Promise.resolve(securityConnection),
      () => Promise.resolve(authenticatedConnection),
    );
    const owner = transport(
      appliance,
      central,
      100,
      "reticulum-e290-e13f88",
      "recovery-name-from-rust",
    );

    const repairStages: string[] = [];
    const repair = owner.repairBond((stage) => repairStages.push(stage));
    await waitFor(
      () => securityConnection.reads.length === 1,
      "public BLE security confirmation read",
    );

    expect(repairStages).toEqual([
      "searching_recovery_advertisement",
      "waiting_for_physical_presence",
    ]);
    expect(securityConnection.reads).toEqual([PROFILE.securityConfirmationCharacteristicUuid]);
    expect(appliance.events).toEqual(["reconnect"]);
    authenticated.resolve(new Uint8Array(PROFILE.securityConfirmationReadyValue));
    await repair;

    expect(repairStages).toEqual([
      "searching_recovery_advertisement",
      "waiting_for_physical_presence",
      "reopening_authenticated_link",
    ]);
    expect(securityConnection.closeCount).toBe(1);
    expect(appliance.events).toEqual([
      "reconnect",
      "link 1 bond-repair-board 20",
      "ensure connected",
    ]);
    expect(central.connectOptions).toHaveLength(2);
    expect(central.connectOptions.map(({ peripheralName }) => peripheralName)).toEqual([
      "recovery-name-from-rust",
      "recovery-name-from-rust",
    ]);
    expect(central.connectOptions[0]?.scanTimeoutMs).toBe(PLATFORM_BLE_BOND_REPAIR_TIMEOUT_MS);
    expect(central.connectOptions[0]?.peripheralId).toBeUndefined();
    expect(central.connectOptions[0]?.peripheralNameAliases).toEqual(["reticulum-e290-e13f88"]);
    expect(central.connectOptions[0]?.reclaimConnectedPeripheral).toBeFalse();
    expect(central.connectOptions[1]?.connectionTimeoutMs).toBe(
      PLATFORM_BLE_BOND_REPAIR_TIMEOUT_MS,
    );
    expect(central.connectOptions[1]?.peripheralId).toBe("bond-repair-board");
    await owner.dispose();
  });

  test("explicit bond repair supersedes an in-flight ordinary reconnect", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const securityConnection = new FakeBleConnection("repair-security-link");
    const repairedConnection = new FakeBleConnection("repair-after-background-attempt");
    central.results.push(
      (options) =>
        new Promise<BleConnection>((_resolve, reject) => {
          options?.signal?.addEventListener("abort", () => reject(options.signal?.reason), {
            once: true,
          });
        }),
      () => Promise.resolve(securityConnection),
      () => Promise.resolve(repairedConnection),
    );
    const owner = transport(
      appliance,
      central,
      100,
      "reticulum-e290-e13f88",
      "recovery-name-from-rust",
    );

    owner.start();
    await waitFor(() => central.connectCount === 1, "ordinary background BLE attempt");
    await owner.repairBond();

    expect(central.connectCount).toBe(3);
    expect(central.connectOptions[0]?.scanTimeoutMs).toBeUndefined();
    expect(central.connectOptions[1]?.scanTimeoutMs).toBe(PLATFORM_BLE_BOND_REPAIR_TIMEOUT_MS);
    expect(central.connectOptions[2]?.scanTimeoutMs).toBe(PLATFORM_BLE_BOND_REPAIR_TIMEOUT_MS);
    expect(central.connectOptions.map(({ peripheralName }) => peripheralName)).toEqual([
      "reticulum-e290-e13f88",
      "recovery-name-from-rust",
      "recovery-name-from-rust",
    ]);
    expect(central.connectOptions[1]?.peripheralId).toBeUndefined();
    expect(central.connectOptions[2]?.peripheralId).toBe("repair-security-link");
    expect(securityConnection.reads).toEqual([PROFILE.securityConfirmationCharacteristicUuid]);
    expect(securityConnection.closeCount).toBe(1);
    expect(appliance.events).toEqual([
      "reconnect",
      "link 1 repair-after-background-attempt 20",
      "ensure connected",
    ]);
    await owner.dispose();
  });

  test("repairs through a fresh platform identifier instead of reusing the previous iOS identifier", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const previous = new FakeBleConnection("stale-ios-peripheral-id");
    const securityConnection = new FakeBleConnection("fresh-ios-peripheral-id");
    const repairedConnection = new FakeBleConnection("fresh-ios-peripheral-id");
    central.results.push(
      () => Promise.resolve(previous),
      () => Promise.resolve(securityConnection),
      () => Promise.resolve(repairedConnection),
    );
    const owner = transport(
      appliance,
      central,
      100,
      "reticulum-e290-e13f88",
      "recovery-name-from-rust",
    );

    owner.start();
    await waitFor(() => appliance.events.includes("ensure connected"), "initial BLE link");
    await owner.repairBond();

    expect(previous.closeCount).toBe(1);
    expect(securityConnection.closeCount).toBe(1);
    expect(central.connectOptions[1]?.peripheralName).toBe("recovery-name-from-rust");
    expect(central.connectOptions[1]?.peripheralId).toBeUndefined();
    expect(central.connectOptions[2]?.peripheralName).toBe("recovery-name-from-rust");
    expect(central.connectOptions[2]?.peripheralId).toBe("fresh-ios-peripheral-id");
    expect(appliance.events).toEqual([
      "link 1 stale-ios-peripheral-id 20",
      "ensure connected",
      "disconnected 1",
      "reconnect",
      "link 2 fresh-ios-peripheral-id 20",
      "ensure connected",
    ]);
    await owner.dispose();
  });

  test("requires explicit distinct recovery targeting before disrupting an ordinary owner", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    expect(() => transport(appliance, central, 100, "same-advertiser", "same-advertiser")).toThrow(
      "normal and recovery BLE peripheral names must be distinct",
    );
    const owner = transport(appliance, central, 100, "reticulum-e290-e13f88");

    await expect(owner.repairBond()).rejects.toThrow(
      "requires exact normal and recovery BLE advertising names",
    );

    expect(central.connectCount).toBe(0);
    expect(appliance.events).toEqual([]);
    await owner.dispose();
  });

  test("configures explicit normal and recovery names without deriving either in TypeScript", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const ordinary = new FakeBleConnection("ordinary-board");
    const security = new FakeBleConnection("recovery-board");
    const repaired = new FakeBleConnection("ordinary-board-after-repair");
    central.results.push(
      () => Promise.resolve(ordinary),
      () => Promise.resolve(security),
      () => Promise.resolve(repaired),
    );
    const owner = transport(appliance, central);

    owner.configurePeripheralNames("normal-name-from-rust", "recovery-name-from-rust");
    owner.start();
    await waitFor(() => appliance.events.includes("ensure connected"), "ordinary named link");
    await owner.repairBond();

    expect(central.connectOptions.map(({ peripheralName }) => peripheralName)).toEqual([
      "normal-name-from-rust",
      "recovery-name-from-rust",
      "recovery-name-from-rust",
    ]);
    expect(() => owner.configurePeripheralNames("other-normal", "other-recovery")).toThrow(
      "must be selected before the transport starts",
    );
    await owner.dispose();
  });

  test("explicit reconnect closes and reports the old generation before registering a replacement", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const first = new FakeBleConnection("first");
    const second = new FakeBleConnection("second");
    central.results.push(
      () => Promise.resolve(first),
      () => Promise.resolve(second),
    );
    const owner = transport(appliance, central);

    await owner.reconnect();
    await owner.reconnect();

    expect(first.closeCount).toBe(1);
    expect(appliance.events).toEqual([
      "reconnect",
      "link 1 first 20",
      "ensure connected",
      "disconnected 1",
      "reconnect",
      "link 2 second 20",
      "ensure connected",
    ]);
    expect(appliance.disconnected).toEqual([
      {
        generation: 1n,
        reason: "Replacing BLE link for explicit reconnect",
      },
    ]);
    await owner.dispose();
  });

  test("does not begin replacement while the old connection close barrier is pending", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const closeBarrier = deferred<void>();
    const first = new FakeBleConnection("same-peripheral");
    const second = new FakeBleConnection("same-peripheral");
    first.closeBehaviors.push(() => closeBarrier.promise);
    central.results.push(
      () => Promise.resolve(first),
      () => Promise.resolve(second),
    );
    const owner = transport(appliance, central);
    await owner.reconnect();

    const replacing = owner.reconnect();
    await waitFor(() => first.closeCount === 1, "old connection close");
    await Bun.sleep(0);

    expect(central.connectCount).toBe(1);
    expect(appliance.events).toEqual([
      "reconnect",
      "link 1 same-peripheral 20",
      "ensure connected",
    ]);

    closeBarrier.resolve();
    await replacing;
    expect(central.connectCount).toBe(2);
    expect(appliance.events).toEqual([
      "reconnect",
      "link 1 same-peripheral 20",
      "ensure connected",
      "disconnected 1",
      "reconnect",
      "link 2 same-peripheral 20",
      "ensure connected",
    ]);
    await owner.dispose();
  });

  test("does not reconnect after old-link teardown rejects", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const first = new FakeBleConnection("same-peripheral");
    first.closeBehaviors.push(() => Promise.reject(new Error("disconnect event timed out")));
    central.results.push(
      () => Promise.resolve(first),
      () => Promise.resolve(new FakeBleConnection("same-peripheral")),
    );
    const owner = transport(appliance, central);
    await owner.reconnect();

    await expect(owner.reconnect()).rejects.toThrow("disconnect event timed out");

    expect(central.connectCount).toBe(1);
    expect(appliance.events).toEqual([
      "reconnect",
      "link 1 same-peripheral 20",
      "ensure connected",
      "disconnected 1",
    ]);
    expect(appliance.disconnected[0]?.reason).toContain("BLE GATT teardown failed");
    await owner.dispose();
  });

  test("initial background start registers and wakes without a destructive reconnect", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    central.results.push(() => Promise.resolve(new FakeBleConnection()));
    const owner = transport(appliance, central);

    owner.start();
    await waitFor(() => appliance.events.includes("ensure connected"), "initial actor wake");

    expect(appliance.events).toEqual(["link 1 peripheral-1 20", "ensure connected"]);
    await owner.dispose();
  });

  test("retains one GATT generation while automatic ensures outpace a delayed client handshake", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection("delayed-client-hello");
    const physicalSetup = deferred<BleConnection>();
    central.results.push(() => physicalSetup.promise);
    const owner = transport(appliance, central);
    const scheduled: Array<{ readonly callback: () => void; readonly delayMs: number }> = [];
    const scheduler: RetryScheduler = (callback, delayMs) => {
      scheduled.push({ callback, delayMs });
      return () => undefined;
    };
    let request = 0;
    const retries = new ForegroundReconnect(
      () => {
        request += 1;
      },
      2_000,
      scheduler,
    );
    const foregroundClient = {
      ensureConnected: () => owner.ensureLink(),
      reconnect: () => owner.reconnect(),
    };
    const runAutomaticAttempt = async (): Promise<void> => {
      expect(retries.begin(request)).toBeTrue();
      try {
        await ensureForegroundConnection(foregroundClient);
      } finally {
        retries.settle();
      }
    };

    // Native ensureConnected only wakes the actor; ClientHello can arrive well
    // after the foreground scheduler's two-second retry cadence. Repeated
    // automatic ensures must leave the subscribed physical generation intact.
    const initial = runAutomaticAttempt();
    await waitFor(() => central.connectCount === 1, "initial automatic BLE setup");
    const overlapping = ensureForegroundConnection(foregroundClient);
    expect(central.connectCount).toBe(1);
    physicalSetup.resolve(connection);
    await Promise.all([initial, overlapping]);
    for (let index = 0; index < 2; index += 1) {
      expect(scheduled[index]?.delayMs).toBe(2_000);
      scheduled[index]?.callback();
      await runAutomaticAttempt();
    }
    retries.suspend();

    expect(central.connectCount).toBe(1);
    expect(connection.closeCount).toBe(0);
    expect(appliance.nextGeneration).toBe(2n);
    expect(appliance.events).toEqual([
      "link 1 delayed-client-hello 20",
      "ensure connected",
      "ensure connected",
      "ensure connected",
    ]);
    expect(appliance.events).not.toContain("reconnect");
    await owner.dispose();
  });

  test("reports each successful and failed platform write exactly once", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    connection.writeBehaviors.push(
      () => Promise.resolve(),
      () => Promise.reject(new Error("write rejected")),
    );
    central.results.push(() => Promise.resolve(connection));
    const owner = transport(appliance, central);
    await owner.reconnect();

    appliance.queue({ kind: "write", generation: 1n, token: 11n, bytes: Uint8Array.of(1).buffer });
    await waitFor(() => appliance.acknowledgements.length === 1, "successful write ack");
    appliance.queue({ kind: "write", generation: 1n, token: 12n, bytes: Uint8Array.of(2).buffer });
    await waitFor(() => appliance.acknowledgements.length === 2, "failed write ack");

    expect(connection.writes).toEqual([[1], [2]]);
    expect(appliance.acknowledgements).toEqual([
      { generation: 1n, token: 11n, outcome: "succeeded" },
      {
        generation: 1n,
        token: 12n,
        outcome: "failed",
        reason: "BLE GATT write failed: write rejected",
      },
    ]);
    await owner.dispose();
  });

  test("times out a stalled OS write and never reports a late success", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    const platformWrite = deferred<void>();
    connection.writeBehaviors.push(() => platformWrite.promise);
    central.results.push(() => Promise.resolve(connection));
    const owner = transport(appliance, central, 5);
    await owner.reconnect();

    appliance.queue({ kind: "write", generation: 1n, token: 9n, bytes: Uint8Array.of(7).buffer });
    await waitFor(() => appliance.acknowledgements.length === 1, "write timeout");
    platformWrite.resolve();
    await Bun.sleep(10);

    expect(appliance.acknowledgements).toHaveLength(1);
    expect(appliance.acknowledgements[0]).toMatchObject({
      generation: 1n,
      token: 9n,
      outcome: "failed",
    });
    expect(
      appliance.acknowledgements[0]?.outcome === "failed"
        ? appliance.acknowledgements[0].reason
        : "",
    ).toContain("timed out");
    await owner.dispose();
  });

  test("does not acknowledge a write after its generation remotely disconnects", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    const platformWrite = deferred<void>();
    connection.writeBehaviors.push(() => platformWrite.promise);
    central.results.push(() => Promise.resolve(connection));
    const owner = transport(appliance, central);
    await owner.reconnect();

    appliance.queue({ kind: "write", generation: 1n, token: 10n, bytes: Uint8Array.of(8).buffer });
    await waitFor(() => connection.writes.length === 1, "pending platform write");
    connection.emitDisconnect("link disappeared");
    platformWrite.reject(new Error("disconnected"));
    await waitFor(() => appliance.disconnected.length === 1, "remote disconnect report");
    await Bun.sleep(0);

    expect(appliance.acknowledgements).toEqual([]);
    await owner.dispose();
  });

  test("reports remote and native-requested disconnects once", async () => {
    const remoteAppliance = new FakeNativeBleAppliance();
    const remoteCentral = new FakeBleCentral();
    const remoteConnection = new FakeBleConnection("remote");
    remoteCentral.results.push(() => Promise.resolve(remoteConnection));
    const remoteOwner = transport(remoteAppliance, remoteCentral);
    await remoteOwner.reconnect();

    remoteConnection.emitDisconnect("radio link lost");
    remoteConnection.emitDisconnect("duplicate callback");
    await waitFor(() => remoteAppliance.disconnected.length === 1, "remote disconnect");
    await remoteOwner.dispose();
    expect(remoteAppliance.disconnected).toEqual([{ generation: 1n, reason: "radio link lost" }]);

    const localAppliance = new FakeNativeBleAppliance();
    const localCentral = new FakeBleCentral();
    const localConnection = new FakeBleConnection("local");
    localCentral.results.push(() => Promise.resolve(localConnection));
    const localOwner = transport(localAppliance, localCentral);
    await localOwner.reconnect();
    localAppliance.queue({ kind: "disconnect", generation: 1n, reason: "session lease ended" });
    await waitFor(() => localAppliance.disconnected.length === 1, "local disconnect");
    await localOwner.dispose();

    expect(localConnection.closeCount).toBe(1);
    expect(localAppliance.disconnected).toEqual([
      { generation: 1n, reason: "session lease ended" },
    ]);
  });

  test("tears down a platform command carrying the wrong generation", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    central.results.push(() => Promise.resolve(connection));
    const owner = transport(appliance, central);
    await owner.reconnect();

    appliance.queue({
      kind: "write",
      generation: 99n,
      token: 1n,
      bytes: Uint8Array.of(4).buffer,
    });
    await waitFor(() => appliance.disconnected.length === 1, "generation teardown");

    expect(connection.writes).toEqual([]);
    expect(connection.closeCount).toBe(1);
    expect(appliance.disconnected[0]?.generation).toBe(1n);
    expect(appliance.disconnected[0]?.reason).toContain(
      "command used generation 99; active generation is 1",
    );
    await owner.dispose();
  });

  test("dispose during connection closes the late link without registering it", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    const connection = new FakeBleConnection();
    const pendingConnection = deferred<BleConnection>();
    central.results.push(() => pendingConnection.promise);
    const owner = transport(appliance, central);
    owner.start();
    await waitFor(() => central.connectCount === 1, "pending BLE connect");

    const disposal = owner.dispose();
    pendingConnection.resolve(connection);
    await disposal;

    expect(connection.closeCount).toBe(1);
    expect(central.disposeCount).toBe(1);
    expect(appliance.events).toEqual([]);
    expect(appliance.disconnected).toEqual([]);
  });

  test("dispose aborts a never-resolving central attempt instead of awaiting it forever", async () => {
    const appliance = new FakeNativeBleAppliance();
    const central = new FakeBleCentral();
    central.results.push(
      (options) =>
        new Promise<BleConnection>((_resolve, reject) => {
          options?.signal?.addEventListener("abort", () => reject(options.signal?.reason), {
            once: true,
          });
        }),
    );
    const owner = transport(appliance, central);
    owner.start();
    await waitFor(() => central.connectCount === 1, "pending BLE connect");

    await owner.dispose();

    expect(central.disposeCount).toBe(1);
    expect(appliance.events).toEqual([]);
  });
});
