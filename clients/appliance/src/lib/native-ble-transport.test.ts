import { describe, expect, test } from "bun:test";

import type { NativeApplianceLike, NativeBlePlatformCommand } from "@reticulum/appliance-native";

import type {
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleGattProfile,
} from "./ble-central-types.ts";
import {
  type DecodedNativeBlePlatformCommand,
  NativeBleTransport,
} from "./native-ble-transport.ts";

const PROFILE: BleGattProfile = {
  indicateCharacteristicUuid: "tx",
  maximumWriteValueBytes: 20,
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
): NativeBleTransport {
  return new NativeBleTransport(
    appliance,
    {
      central,
      decodeCommand: (command) => command as unknown as DecodedNativeBlePlatformCommand,
      peripheralName,
      profile: PROFILE,
    },
    writeTimeoutMs,
  );
}

describe("native BLE transport orchestration", () => {
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
