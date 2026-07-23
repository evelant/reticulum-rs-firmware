import { describe, expect, test } from "bun:test";

import { GattLifecycle } from "./main";

class FakeDevice extends EventTarget {
  readonly id = "fake-device";
  readonly name = "reticulum-e290-test";
  readonly gatt = undefined;
}

class FakeCharacteristic extends EventTarget {
  readonly properties = { write: true, indicate: true };
  stopCalls = 0;

  async startNotifications(): Promise<FakeCharacteristic> {
    return this;
  }

  async stopNotifications(): Promise<FakeCharacteristic> {
    this.stopCalls += 1;
    return this;
  }

  async writeValueWithResponse(): Promise<void> {}
}

class FakeServer {
  readonly connected = true;
  disconnectCalls = 0;

  constructor(readonly device: FakeDevice) {}

  async connect(): Promise<FakeServer> {
    return this;
  }

  disconnect(): void {
    this.disconnectCalls += 1;
    this.device.dispatchEvent(new Event("gattserverdisconnected"));
  }

  async getPrimaryService(): Promise<never> {
    throw new Error("not used by lifecycle tests");
  }
}

describe("Web Bluetooth lifecycle", () => {
  test("completed intentional cleanup removes the disconnect listener and is idempotent", async () => {
    const lifecycle = new GattLifecycle();
    const device = new FakeDevice();
    const server = new FakeServer(device);
    const tx = new FakeCharacteristic();
    let unexpectedDisconnects = 0;

    lifecycle.trackDevice(device, () => {
      unexpectedDisconnects += 1;
    });
    lifecycle.trackServer(server);
    lifecycle.trackTx(tx);
    lifecycle.markCompleted();

    await lifecycle.close();
    await lifecycle.close();

    expect(lifecycle.completed).toBe(true);
    expect(lifecycle.terminal).toBe(true);
    expect(tx.stopCalls).toBe(1);
    expect(server.disconnectCalls).toBe(1);
    expect(unexpectedDisconnects).toBe(0);
  });

  test("late device and connection results are disposed after terminal failure", async () => {
    const lifecycle = new GattLifecycle();
    expect(lifecycle.beginFailure()).toBe(true);
    expect(lifecycle.beginFailure()).toBe(false);
    await lifecycle.close();

    const device = new FakeDevice();
    const server = new FakeServer(device);
    const tx = new FakeCharacteristic();
    let disconnectEvents = 0;
    lifecycle.trackDevice(device, () => {
      disconnectEvents += 1;
    });
    lifecycle.trackServer(server);
    lifecycle.trackTx(tx);

    await expect(lifecycle.requireActive("late GATT connect")).rejects.toThrow(
      "late GATT connect completed after the local bridge became terminal",
    );
    expect(tx.stopCalls).toBe(1);
    expect(server.disconnectCalls).toBe(1);
    expect(disconnectEvents).toBe(0);

    await lifecycle.close();
    expect(tx.stopCalls).toBe(1);
    expect(server.disconnectCalls).toBe(1);
  });

  test("a device chooser result alone is detached after the bridge closes", async () => {
    const lifecycle = new GattLifecycle();
    lifecycle.beginFailure();

    const device = new FakeDevice();
    let disconnectEvents = 0;
    lifecycle.trackDevice(device, () => {
      disconnectEvents += 1;
    });

    await expect(
      lifecycle.requireActive("Web Bluetooth device selection"),
    ).rejects.toThrow();
    device.dispatchEvent(new Event("gattserverdisconnected"));
    expect(disconnectEvents).toBe(0);
  });
});
