import { describe, expect, test } from "bun:test";
import { createBleCentral as createWebBleCentral } from "./ble-central.web.ts";
import {
  type BleCentralDriver,
  type BleDiscoveredPeripheral,
  type BleDriverDisconnectEvent,
  type BleDriverIndicationEvent,
  type BleGattDiscovery,
  ForegroundBleCentral,
} from "./ble-central-core.ts";
import type { BleGattProfile } from "./ble-central-types.ts";

const PROFILE: BleGattProfile = {
  serviceUuid: "10000000-0000-0000-0000-000000000001",
  writeCharacteristicUuid: "10000000-0000-0000-0000-000000000002",
  indicateCharacteristicUuid: "10000000-0000-0000-0000-000000000003",
  maximumWriteValueBytes: 20,
};

const PERIPHERAL: BleDiscoveredPeripheral = {
  id: "AA:BB:CC:DD:EE:FF",
  name: "test appliance",
};

interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly reject: (reason?: unknown) => void;
  readonly resolve: (value: T | PromiseLike<T>) => void;
}

function deferred<T = void>(): Deferred<T> {
  let resolve: (value: T | PromiseLike<T>) => void = () => {};
  let reject: (reason?: unknown) => void = () => {};
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

class FakeBleDriver implements BleCentralDriver {
  readonly events: string[] = [];
  readonly writes: Uint8Array[] = [];
  readonly writeGates: Array<Deferred<void>> = [];

  autoDiscover = true;
  disconnectOnConnect = false;
  discoveredPeripheral = PERIPHERAL;
  discovery: BleGattDiscovery = {
    serviceUuids: [PROFILE.serviceUuid.toUpperCase()],
    characteristics: [
      {
        serviceUuid: PROFILE.serviceUuid,
        characteristicUuid: PROFILE.writeCharacteristicUuid,
        canWriteWithResponse: true,
        canIndicate: false,
      },
      {
        serviceUuid: PROFILE.serviceUuid,
        characteristicUuid: PROFILE.indicateCharacteristicUuid,
        canWriteWithResponse: false,
        canIndicate: true,
      },
    ],
  };
  connectGate: Promise<void> = Promise.resolve();
  discoveryGate: Promise<void> = Promise.resolve();
  disconnectEventGate: Promise<void> = Promise.resolve();
  disconnectGate: Promise<void> = Promise.resolve();
  maximumWriteGate: Promise<void> = Promise.resolve();
  maximumWriteBytes = 64;
  notificationGate: Promise<void> = Promise.resolve();
  stopNotificationGate: Promise<void> = Promise.resolve();

  private readonly discoveredListeners = new Set<(peripheral: BleDiscoveredPeripheral) => void>();
  private readonly disconnectListeners = new Set<(event: BleDriverDisconnectEvent) => void>();
  private readonly indicationListeners = new Set<(event: BleDriverIndicationEvent) => void>();

  prepare(): Promise<void> {
    this.events.push("prepare");
    return Promise.resolve();
  }

  onDiscovered(listener: (peripheral: BleDiscoveredPeripheral) => void): () => void {
    this.discoveredListeners.add(listener);
    return () => this.discoveredListeners.delete(listener);
  }

  onDisconnected(listener: (event: BleDriverDisconnectEvent) => void): () => void {
    this.disconnectListeners.add(listener);
    return () => this.disconnectListeners.delete(listener);
  }

  onIndication(listener: (event: BleDriverIndicationEvent) => void): () => void {
    this.indicationListeners.add(listener);
    return () => this.indicationListeners.delete(listener);
  }

  startScan(serviceUuid: string): Promise<void> {
    this.events.push(`scan ${serviceUuid}`);
    if (this.autoDiscover) {
      queueMicrotask(() => {
        for (const listener of this.discoveredListeners) {
          listener(this.discoveredPeripheral);
        }
      });
    }
    return Promise.resolve();
  }

  stopScan(): Promise<void> {
    this.events.push("stop scan");
    return Promise.resolve();
  }

  async connect(peripheralId: string): Promise<void> {
    this.events.push(`connect ${peripheralId}`);
    if (this.disconnectOnConnect) this.emitDisconnect({ status: 8 });
    await this.connectGate;
  }

  async discover(peripheralId: string, serviceUuid: string): Promise<BleGattDiscovery> {
    this.events.push(`discover ${peripheralId} ${serviceUuid}`);
    await this.discoveryGate;
    return this.discovery;
  }

  async startIndications(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void> {
    this.events.push(`start indications ${peripheralId} ${serviceUuid} ${characteristicUuid}`);
    await this.notificationGate;
  }

  async stopIndications(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void> {
    this.events.push(`stop indications ${peripheralId} ${serviceUuid} ${characteristicUuid}`);
    await this.stopNotificationGate;
  }

  async maximumWriteWithResponseBytes(peripheralId: string): Promise<number> {
    this.events.push(`maximum write ${peripheralId}`);
    await this.maximumWriteGate;
    return this.maximumWriteBytes;
  }

  async writeWithResponse(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
    chunk: Uint8Array,
    maximumChunkBytes: number,
  ): Promise<void> {
    this.events.push(
      `write ${peripheralId} ${serviceUuid} ${characteristicUuid} ${maximumChunkBytes}`,
    );
    this.writes.push(new Uint8Array(chunk));
    const gate = this.writeGates.shift();
    if (gate !== undefined) await gate.promise;
  }

  async disconnect(peripheralId: string): Promise<void> {
    this.events.push(`disconnect ${peripheralId}`);
    await this.disconnectGate;
    void this.disconnectEventGate.then(
      () => this.emitDisconnect({ peripheralId }),
      () => {},
    );
  }

  emitIndication(bytes: Uint8Array, overrides: Partial<BleDriverIndicationEvent> = {}): void {
    const event: BleDriverIndicationEvent = {
      peripheralId: PERIPHERAL.id,
      serviceUuid: PROFILE.serviceUuid.toUpperCase(),
      characteristicUuid: PROFILE.indicateCharacteristicUuid.toUpperCase(),
      bytes,
      ...overrides,
    };
    for (const listener of this.indicationListeners) listener(event);
  }

  emitDisconnect(event: Partial<BleDriverDisconnectEvent> = {}): void {
    const complete: BleDriverDisconnectEvent = {
      peripheralId: PERIPHERAL.id,
      ...event,
    };
    for (const listener of this.disconnectListeners) listener(complete);
  }

  snapshotDisconnectListeners(): Array<(event: BleDriverDisconnectEvent) => void> {
    return [...this.disconnectListeners];
  }
}

describe("foreground BLE central", () => {
  test("declares the connection ready only after discovery and indication subscription", async () => {
    const driver = new FakeBleDriver();
    const notification = deferred();
    driver.notificationGate = notification.promise;
    const central = new ForegroundBleCentral(driver);

    let ready = false;
    const pending = central.connect(PROFILE).then((connection) => {
      ready = true;
      return connection;
    });
    await Bun.sleep(0);

    expect(ready).toBe(false);
    expect(driver.events.at(-1)?.startsWith("start indications")).toBe(true);
    notification.resolve();
    const connection = await pending;

    expect(connection.peripheralId).toBe(PERIPHERAL.id);
    expect(connection.name).toBe(PERIPHERAL.name);
    expect(connection.maxWriteWithResponseBytes).toBe(20);
    expect(driver.events.map((event) => event.split(" ")[0])).toEqual([
      "prepare",
      "scan",
      "stop",
      "connect",
      "discover",
      "start",
      "maximum",
    ]);
  });

  test("buffers and copies opaque indications until an observer is installed", async () => {
    const driver = new FakeBleDriver();
    const connection = await new ForegroundBleCentral(driver).connect(PROFILE);
    const source = Uint8Array.of(1, 2, 3);
    driver.emitIndication(source);
    source[0] = 99;

    const received: Uint8Array[] = [];
    connection.observe({
      onBytes: (bytes) => received.push(bytes),
      onDisconnect: () => {},
    });
    driver.emitIndication(Uint8Array.of(4, 5));
    driver.emitIndication(Uint8Array.of(8), {
      characteristicUuid: PROFILE.writeCharacteristicUuid,
    });

    expect(received.map((bytes) => Array.from(bytes))).toEqual([
      [1, 2, 3],
      [4, 5],
    ]);
  });

  test("serializes write-with-response chunks and owns the caller's bytes", async () => {
    const driver = new FakeBleDriver();
    const firstGate = deferred();
    driver.writeGates.push(firstGate);
    const connection = await new ForegroundBleCentral(driver).connect(PROFILE);
    const firstBytes = Uint8Array.of(10, 11);

    const first = connection.write(firstBytes);
    firstBytes[0] = 90;
    const second = connection.write(Uint8Array.of(20, 21));
    await Bun.sleep(0);

    expect(driver.writes.map((bytes) => Array.from(bytes))).toEqual([[10, 11]]);
    firstGate.resolve();
    await Promise.all([first, second]);
    expect(driver.writes.map((bytes) => Array.from(bytes))).toEqual([
      [10, 11],
      [20, 21],
    ]);
  });

  test("rejects invalid chunks before touching the GATT driver", async () => {
    const driver = new FakeBleDriver();
    driver.maximumWriteBytes = 20;
    const connection = await new ForegroundBleCentral(driver).connect(PROFILE);

    expect(connection.maxWriteWithResponseBytes).toBe(20);
    await expect(connection.write(new Uint8Array())).rejects.toThrow("must not be empty");
    await expect(connection.write(new Uint8Array(21))).rejects.toThrow("negotiated maximum is 20");
    expect(driver.writes).toHaveLength(0);
  });

  test("rejects a positive platform write maximum below the required floor", async () => {
    const driver = new FakeBleDriver();
    driver.maximumWriteBytes = 7;

    await expect(new ForegroundBleCentral(driver).connect(PROFILE)).rejects.toThrow(
      "at least 20 bytes are required",
    );
    expect(driver.events).toContain(`disconnect ${PERIPHERAL.id}`);
  });

  test("rejects an unusable generated GATT value limit before starting BLE", async () => {
    const driver = new FakeBleDriver();

    await expect(
      new ForegroundBleCentral(driver).connect({
        ...PROFILE,
        maximumWriteValueBytes: 19,
      }),
    ).rejects.toThrow("integer of at least 20 bytes");
    expect(driver.events).toEqual([]);
  });

  test("bounds indications received before stream ownership is installed", async () => {
    const driver = new FakeBleDriver();
    const connection = await new ForegroundBleCentral(driver).connect(PROFILE);

    for (let index = 0; index < 257; index += 1) {
      driver.emitIndication(Uint8Array.of(index));
    }

    const reasons: string[] = [];
    connection.observe({
      onBytes: () => {
        throw new Error("a failed buffered stream must not deliver partial bytes");
      },
      onDisconnect: (event) => reasons.push(event.reason),
    });
    await Bun.sleep(0);

    expect(reasons).toEqual([
      "BLE indication buffer overflow before stream observer was installed",
    ]);
    await expect(connection.write(Uint8Array.of(1))).rejects.toThrow("closed");
    expect(driver.events).toContain(`disconnect ${PERIPHERAL.id}`);
  });

  test("stops an active scan when discovery times out", async () => {
    const driver = new FakeBleDriver();
    driver.autoDiscover = false;

    await expect(
      new ForegroundBleCentral(driver).connect(PROFILE, { scanTimeoutMs: 5 }),
    ).rejects.toThrow("was found within 5 ms");
    expect(driver.events).toEqual(["prepare", `scan ${PROFILE.serviceUuid}`, "stop scan"]);
  });

  test("bounds connection, discovery, subscription, and MTU setup stages independently", async () => {
    const cases = [
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.connectGate = gate;
        },
        expected: "BLE connection to",
      },
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.discoveryGate = gate;
        },
        expected: "BLE GATT discovery for",
      },
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.notificationGate = gate;
        },
        expected: "BLE indication subscription for",
      },
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.maximumWriteGate = gate;
        },
        expected: "BLE write MTU lookup for",
      },
    ] as const;

    for (const setupCase of cases) {
      const driver = new FakeBleDriver();
      setupCase.configure(driver, new Promise<void>(() => {}));

      await expect(
        new ForegroundBleCentral(driver).connect(PROFILE, {
          operationTimeoutMs: 5,
        }),
      ).rejects.toThrow(setupCase.expected);
      expect(driver.events).toContain(`disconnect ${PERIPHERAL.id}`);
    }
  });

  test("setup failures retain ownership until their delayed disconnect event", async () => {
    const cases = [
      {
        event: "discover ",
        restore(driver: FakeBleDriver) {
          driver.discoveryGate = Promise.resolve();
        },
        stall(driver: FakeBleDriver, gate: Promise<void>) {
          driver.discoveryGate = gate;
        },
      },
      {
        event: "start indications ",
        restore(driver: FakeBleDriver) {
          driver.notificationGate = Promise.resolve();
        },
        stall(driver: FakeBleDriver, gate: Promise<void>) {
          driver.notificationGate = gate;
        },
      },
      {
        event: "maximum write ",
        restore(driver: FakeBleDriver) {
          driver.maximumWriteGate = Promise.resolve();
        },
        stall(driver: FakeBleDriver, gate: Promise<void>) {
          driver.maximumWriteGate = gate;
        },
      },
    ] as const;

    for (const setupCase of cases) {
      const driver = new FakeBleDriver();
      const stage = deferred();
      const disconnectEvent = deferred();
      setupCase.stall(driver, stage.promise);
      driver.disconnectEventGate = disconnectEvent.promise;
      const central = new ForegroundBleCentral(driver);
      const pending = central.connect(PROFILE, { operationTimeoutMs: 100 });
      let settled = false;
      void pending.then(
        () => {
          settled = true;
        },
        () => {
          settled = true;
        },
      );
      await waitFor(
        () => driver.events.some((event) => event.startsWith(setupCase.event)),
        setupCase.event,
      );
      const oldDisconnectListener = driver.snapshotDisconnectListeners()[0];

      stage.reject(new Error(`${setupCase.event.trim()} failed`));
      await waitFor(
        () => driver.events.includes(`disconnect ${PERIPHERAL.id}`),
        "failed setup disconnect request",
      );
      await Bun.sleep(0);

      expect(settled).toBe(false);
      await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

      disconnectEvent.resolve();
      await expect(pending).rejects.toThrow("failed");
      setupCase.restore(driver);
      const replacement = await central.connect(PROFILE);
      oldDisconnectListener?.({ peripheralId: PERIPHERAL.id, status: 19 });

      await expect(replacement.write(Uint8Array.of(1))).resolves.toBeUndefined();
      await central.dispose();
    }
  });

  test("a setup disconnect-event timeout fails closed until a late event arrives", async () => {
    const driver = new FakeBleDriver();
    const discovery = deferred();
    driver.discoveryGate = discovery.promise;
    driver.disconnectEventGate = new Promise<void>(() => {});
    const central = new ForegroundBleCentral(driver);
    const pending = central.connect(PROFILE, { operationTimeoutMs: 5 });
    await waitFor(
      () => driver.events.some((event) => event.startsWith("discover ")),
      "GATT discovery",
    );

    discovery.reject(new Error("discovery failed"));
    await expect(pending).rejects.toThrow("discovery failed");
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    driver.discoveryGate = Promise.resolve();
    driver.emitDisconnect();
    driver.disconnectEventGate = Promise.resolve();
    const replacement = await central.connect(PROFILE);
    expect(replacement.peripheralId).toBe(PERIPHERAL.id);
    await central.dispose();
  });

  test("a setup disconnect request error fails closed until a late event arrives", async () => {
    const driver = new FakeBleDriver();
    const discovery = deferred();
    const disconnectRequest = deferred();
    driver.discoveryGate = discovery.promise;
    driver.disconnectGate = disconnectRequest.promise;
    const central = new ForegroundBleCentral(driver);
    const pending = central.connect(PROFILE, { operationTimeoutMs: 10 });
    await waitFor(
      () => driver.events.some((event) => event.startsWith("discover ")),
      "GATT discovery",
    );

    discovery.reject(new Error("discovery failed"));
    await waitFor(
      () => driver.events.includes(`disconnect ${PERIPHERAL.id}`),
      "failed setup disconnect request",
    );
    disconnectRequest.reject(new Error("native disconnect rejected"));

    await expect(pending).rejects.toThrow("discovery failed");
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    driver.discoveryGate = Promise.resolve();
    driver.disconnectGate = Promise.resolve();
    driver.emitDisconnect();
    const replacement = await central.connect(PROFILE);
    expect(replacement.peripheralId).toBe(PERIPHERAL.id);
    await central.dispose();
  });

  test("a late timed-out connection is inert after its disconnect event permits replacement", async () => {
    const driver = new FakeBleDriver();
    const firstConnection = deferred();
    const disconnectEvent = deferred();
    driver.connectGate = firstConnection.promise;
    driver.disconnectEventGate = disconnectEvent.promise;
    const central = new ForegroundBleCentral(driver);
    const firstAttempt = central.connect(PROFILE, {
      operationTimeoutMs: 5,
    });
    const oldDisconnectListener = driver.snapshotDisconnectListeners()[0];

    await expect(firstAttempt).rejects.toThrow("BLE connection to");
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    firstConnection.resolve();
    await Bun.sleep(0);
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    disconnectEvent.resolve();
    await Bun.sleep(0);
    driver.connectGate = Promise.resolve();
    const replacement = await central.connect(PROFILE, {
      operationTimeoutMs: 5,
    });
    const disconnectsBeforeLateCompletion = driver.events.filter((event) =>
      event.startsWith("disconnect "),
    ).length;

    oldDisconnectListener?.({ peripheralId: PERIPHERAL.id, status: 19 });
    await Bun.sleep(0);

    expect(replacement.peripheralId).toBe(PERIPHERAL.id);
    expect(driver.events.filter((event) => event.startsWith("disconnect ")).length).toBe(
      disconnectsBeforeLateCompletion,
    );
    await expect(replacement.write(Uint8Array.of(1))).resolves.toBeUndefined();
    await central.dispose();
  });

  test("a teardown-induced connect rejection still requires the disconnect event", async () => {
    const driver = new FakeBleDriver();
    const firstConnection = deferred();
    const disconnectEvent = deferred();
    driver.connectGate = firstConnection.promise;
    driver.disconnectEventGate = disconnectEvent.promise;
    const central = new ForegroundBleCentral(driver);
    const firstAttempt = central.connect(PROFILE, {
      operationTimeoutMs: 5,
    });
    void firstAttempt.catch(() => {});

    await waitFor(
      () => driver.events.includes(`disconnect ${PERIPHERAL.id}`),
      "timed-out setup disconnect request",
    );
    firstConnection.reject(new Error("native connect was cancelled"));
    await expect(firstAttempt).rejects.toThrow("BLE connection to");
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    disconnectEvent.resolve();
    await Bun.sleep(0);
    driver.connectGate = Promise.resolve();
    const replacement = await central.connect(PROFILE);

    expect(replacement.peripheralId).toBe(PERIPHERAL.id);
    await central.dispose();
  });

  test("a natural connect rejection before teardown releases setup without disconnecting", async () => {
    const driver = new FakeBleDriver();
    const firstConnection = deferred();
    driver.connectGate = firstConnection.promise;
    const central = new ForegroundBleCentral(driver);
    const firstAttempt = central.connect(PROFILE, { operationTimeoutMs: 100 });
    await waitFor(
      () => driver.events.includes(`connect ${PERIPHERAL.id}`),
      "platform connection attempt",
    );
    const oldDisconnectListener = driver.snapshotDisconnectListeners()[0];

    firstConnection.reject(new Error("native connection failed"));
    await expect(firstAttempt).rejects.toThrow("native connection failed");
    expect(driver.events.some((event) => event.startsWith("disconnect "))).toBe(false);

    driver.connectGate = Promise.resolve();
    const replacement = await central.connect(PROFILE);
    oldDisconnectListener?.({ peripheralId: PERIPHERAL.id, status: 19 });

    await expect(replacement.write(Uint8Array.of(1))).resolves.toBeUndefined();
    await central.dispose();
  });

  test("a confirmed disconnect also makes another-peripheral late completion inert", async () => {
    const driver = new FakeBleDriver();
    const firstConnection = deferred();
    driver.connectGate = firstConnection.promise;
    const central = new ForegroundBleCentral(driver);

    await expect(
      central.connect(PROFILE, {
        operationTimeoutMs: 5,
      }),
    ).rejects.toThrow("BLE connection to");
    const replacementPeripheral = {
      id: "11:22:33:44:55:66",
      name: "replacement appliance",
    };
    driver.discoveredPeripheral = replacementPeripheral;
    driver.connectGate = Promise.resolve();
    const replacement = await central.connect(PROFILE, {
      operationTimeoutMs: 5,
    });
    const oldDisconnectsBeforeLateCompletion = driver.events.filter(
      (event) => event === `disconnect ${PERIPHERAL.id}`,
    ).length;

    firstConnection.resolve();
    await Bun.sleep(0);

    expect(replacement.peripheralId).toBe(replacementPeripheral.id);
    expect(driver.events.filter((event) => event === `disconnect ${PERIPHERAL.id}`).length).toBe(
      oldDisconnectsBeforeLateCompletion,
    );
    expect(driver.events).not.toContain(`disconnect ${replacementPeripheral.id}`);
    await central.dispose();
  });

  test("dispose cancels and invalidates a never-resolving setup stage", async () => {
    const cases = [
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.connectGate = gate;
        },
        event: "connect ",
      },
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.discoveryGate = gate;
        },
        event: "discover ",
      },
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.notificationGate = gate;
        },
        event: "start indications ",
      },
      {
        configure(driver: FakeBleDriver, gate: Promise<void>) {
          driver.maximumWriteGate = gate;
        },
        event: "maximum write ",
      },
    ] as const;

    for (const setupCase of cases) {
      const driver = new FakeBleDriver();
      setupCase.configure(driver, new Promise<void>(() => {}));
      const central = new ForegroundBleCentral(driver);
      const pending = central.connect(PROFILE, {
        operationTimeoutMs: 1_000,
        scanTimeoutMs: 1_000,
      });
      await waitFor(
        () => driver.events.some((event) => event.startsWith(setupCase.event)),
        setupCase.event,
      );

      await central.dispose();
      await expect(pending).rejects.toThrow("disposed during connection setup");
      await expect(central.connect(PROFILE)).rejects.toThrow("disposed");
    }
  });

  test("bounds indication unsubscribe and disconnect during disposal", async () => {
    const driver = new FakeBleDriver();
    const central = new ForegroundBleCentral(driver);
    await central.connect(PROFILE, { operationTimeoutMs: 5 });
    driver.stopNotificationGate = new Promise<void>(() => {});
    driver.disconnectGate = new Promise<void>(() => {});

    const startedAt = performance.now();
    await expect(central.dispose()).rejects.toThrow("indication unsubscribe");

    expect(performance.now() - startedAt).toBeLessThan(200);
    expect(driver.events).toContain(
      `stop indications ${PERIPHERAL.id} ${PROFILE.serviceUuid} ${PROFILE.indicateCharacteristicUuid}`,
    );
    expect(driver.events).toContain(`disconnect ${PERIPHERAL.id}`);
  });

  test("holds same-peripheral ownership until the disconnect event follows an early promise", async () => {
    const driver = new FakeBleDriver();
    const disconnectEvent = deferred();
    driver.disconnectEventGate = disconnectEvent.promise;
    const central = new ForegroundBleCentral(driver);
    const connection = await central.connect(PROFILE, { operationTimeoutMs: 100 });
    const oldDisconnectListener = driver.snapshotDisconnectListeners()[0];

    let closeFinished = false;
    const closing = connection.close().then(() => {
      closeFinished = true;
    });
    await waitFor(
      () => driver.events.includes(`disconnect ${PERIPHERAL.id}`),
      "platform disconnect request",
    );
    await Bun.sleep(0);

    expect(closeFinished).toBe(false);
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    disconnectEvent.resolve();
    await closing;
    const replacement = await central.connect(PROFILE);
    oldDisconnectListener?.({ peripheralId: PERIPHERAL.id, status: 19 });

    await expect(replacement.write(Uint8Array.of(1))).resolves.toBeUndefined();
    await central.dispose();
  });

  test("a missing disconnect event times out without releasing same-peripheral ownership", async () => {
    const driver = new FakeBleDriver();
    driver.disconnectEventGate = new Promise<void>(() => {});
    const central = new ForegroundBleCentral(driver);
    const connection = await central.connect(PROFILE, { operationTimeoutMs: 5 });

    await expect(connection.close()).rejects.toThrow("disconnect event");
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    driver.emitDisconnect();
    await central.dispose();
  });

  test("a disconnect request error does not release ownership for replacement", async () => {
    const driver = new FakeBleDriver();
    const disconnectRequest = deferred();
    driver.disconnectGate = disconnectRequest.promise;
    const central = new ForegroundBleCentral(driver);
    const connection = await central.connect(PROFILE, { operationTimeoutMs: 100 });

    const closing = connection.close();
    await waitFor(
      () => driver.events.includes(`disconnect ${PERIPHERAL.id}`),
      "platform disconnect request",
    );
    disconnectRequest.reject(new Error("native disconnect rejected"));

    await expect(closing).rejects.toThrow("native disconnect rejected");
    await expect(central.connect(PROFILE)).rejects.toThrow("already owns a connection");

    driver.emitDisconnect();
    await central.dispose();
  });

  test("surfaces a matching remote disconnect once and rejects later writes", async () => {
    const driver = new FakeBleDriver();
    const central = new ForegroundBleCentral(driver);
    const connection = await central.connect(PROFILE);
    const disconnects: string[] = [];
    connection.observe({
      onBytes: () => {},
      onDisconnect: (event) => disconnects.push(event.reason),
    });

    driver.emitDisconnect({ peripheralId: "another", status: 1 });
    driver.emitDisconnect({ status: 19 });
    driver.emitDisconnect({ status: 20 });

    expect(disconnects).toEqual(["BLE disconnected (status 19)"]);
    await expect(connection.write(Uint8Array.of(1))).rejects.toThrow("closed");
    const replacement = await central.connect(PROFILE);
    expect(replacement.peripheralId).toBe(PERIPHERAL.id);
  });

  test("rejects a peripheral that drops during connection setup", async () => {
    const driver = new FakeBleDriver();
    driver.disconnectOnConnect = true;

    await expect(new ForegroundBleCentral(driver).connect(PROFILE)).rejects.toThrow(
      "BLE disconnected (status 8)",
    );
    expect(driver.events.some((event) => event.startsWith("discover"))).toBe(false);
  });

  test("validates characteristic capabilities and cleans up the failed link", async () => {
    const driver = new FakeBleDriver();
    driver.discovery = {
      ...driver.discovery,
      characteristics: driver.discovery.characteristics.map((characteristic) => ({
        ...characteristic,
        canIndicate: false,
      })),
    };

    await expect(new ForegroundBleCentral(driver).connect(PROFILE)).rejects.toThrow(
      "does not support indications",
    );
    expect(driver.events).toContain(`disconnect ${PERIPHERAL.id}`);
    expect(driver.events.some((event) => event.startsWith("start indications"))).toBe(false);
  });

  test("intentional close stops indications before disconnecting without a callback", async () => {
    const driver = new FakeBleDriver();
    const connection = await new ForegroundBleCentral(driver).connect(PROFILE);
    let disconnects = 0;
    connection.observe({
      onBytes: () => {},
      onDisconnect: () => {
        disconnects += 1;
      },
    });

    await connection.close();
    driver.emitDisconnect();

    expect(disconnects).toBe(0);
    expect(driver.events.slice(-2).map((event) => event.split(" ")[0])).toEqual([
      "stop",
      "disconnect",
    ]);
  });
});

describe("web BLE boundary", () => {
  test("loads without the native BLE module and reports an explicit unsupported connector", async () => {
    const central = createWebBleCentral();

    expect(central.supported).toBe(false);
    await expect(central.connect(PROFILE)).rejects.toThrow(
      "available only in iOS and Android development builds",
    );
  });
});
