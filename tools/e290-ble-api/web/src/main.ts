import {
  type BridgeProfile,
  PreReadyIndicationBuffer,
  boundedError,
  parseBridgeProfile,
  relayWrite,
  validateCapabilities,
} from "./bridge-core";

type BluetoothProperties = Readonly<{
  write: boolean;
  indicate: boolean;
}>;

interface BluetoothCharacteristic extends EventTarget {
  readonly properties: BluetoothProperties;
  startNotifications(): Promise<BluetoothCharacteristic>;
  stopNotifications(): Promise<BluetoothCharacteristic>;
  writeValueWithResponse(value: Uint8Array): Promise<void>;
}

interface BluetoothService {
  getCharacteristic(uuid: string): Promise<BluetoothCharacteristic>;
}

interface BluetoothGattServer {
  readonly connected: boolean;
  connect(): Promise<BluetoothGattServer>;
  disconnect(): void;
  getPrimaryService(uuid: string): Promise<BluetoothService>;
}

interface BluetoothDevice extends EventTarget {
  readonly id: string;
  readonly name?: string;
  readonly gatt?: BluetoothGattServer;
}

interface BluetoothFacade {
  requestDevice(options: {
    filters: ReadonlyArray<{ services: ReadonlyArray<string> }>;
  }): Promise<BluetoothDevice>;
}

type BluetoothValueEvent = Event & {
  readonly target: BluetoothCharacteristic & { readonly value?: DataView };
};

type ProofEnvelope = Readonly<{
  type: "proof";
  evidence: unknown;
}>;

export class GattLifecycle {
  #device: BluetoothDevice | undefined;
  #disconnectListener: EventListener | undefined;
  #server: BluetoothGattServer | undefined;
  #rx: BluetoothCharacteristic | undefined;
  #tx: BluetoothCharacteristic | undefined;
  #terminal = false;
  #completed = false;

  get terminal(): boolean {
    return this.#terminal;
  }

  get completed(): boolean {
    return this.#completed;
  }

  get rx(): BluetoothCharacteristic | undefined {
    return this.#rx;
  }

  trackDevice(device: BluetoothDevice, disconnectListener: EventListener): void {
    this.#device = device;
    this.#disconnectListener = disconnectListener;
    device.addEventListener("gattserverdisconnected", disconnectListener);
  }

  trackServer(server: BluetoothGattServer): void {
    this.#server = server;
  }

  trackRx(rx: BluetoothCharacteristic): void {
    this.#rx = rx;
  }

  trackTx(tx: BluetoothCharacteristic): void {
    this.#tx = tx;
  }

  markCompleted(): void {
    this.#completed = true;
    this.#terminal = true;
  }

  beginFailure(): boolean {
    const shouldReport = !this.#terminal && !this.#completed;
    this.#terminal = true;
    return shouldReport;
  }

  async requireActive(stage: string): Promise<void> {
    if (!this.#terminal) {
      return;
    }
    await this.close();
    throw new Error(`${stage} completed after the local bridge became terminal`);
  }

  async close(): Promise<void> {
    const device = this.#device;
    const disconnectListener = this.#disconnectListener;
    const server = this.#server;
    const tx = this.#tx;

    // Clear ownership before awaiting platform cleanup. A repeated cleanup is
    // therefore harmless, while a late async result can be tracked and
    // disposed by a subsequent requireActive()/close() call.
    this.#device = undefined;
    this.#disconnectListener = undefined;
    this.#server = undefined;
    this.#rx = undefined;
    this.#tx = undefined;

    if (device && disconnectListener) {
      device.removeEventListener("gattserverdisconnected", disconnectListener);
    }
    if (tx) {
      try {
        await tx.stopNotifications();
      } catch {
        // The physical link may already be gone.
      }
    }
    server?.disconnect();
  }
}

let connectButton: HTMLButtonElement;
let status: HTMLParagraphElement;
let evidence: HTMLPreElement;
const indications = new PreReadyIndicationBuffer();
const lifecycle = new GattLifecycle();

let socket: WebSocket | undefined;
let profile: BridgeProfile | undefined;
let writeActive = false;

function binarySender() {
  return {
    get bufferedAmount() {
      return socket?.bufferedAmount ?? Number.MAX_SAFE_INTEGER;
    },
    send(value: Uint8Array) {
      socket?.send(value);
    },
  };
}

function requiredElement<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`missing page element ${id}`);
  }
  return element as T;
}

function setStatus(message: string, kind?: "error" | "pass"): void {
  status.textContent = message;
  status.className = kind ?? "";
}

function bridgeUrl(): string {
  const url = new URL("bridge", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.href;
}

function withTimeout<T>(
  operation: Promise<T>,
  timeoutMs: number,
  stage: string,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(
      () => reject(new Error(`${stage} exceeded ${timeoutMs} ms`)),
      timeoutMs,
    );
    operation.then(
      (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function sendControlText(text: string): void {
  if (socket?.readyState !== WebSocket.OPEN) {
    throw new Error("local Rust bridge is not open");
  }
  if (
    socket.bufferedAmount + new TextEncoder().encode(text).byteLength >
    64 * 1024
  ) {
    throw new Error("local Rust bridge control queue exceeded its bound");
  }
  socket.send(text);
}

function sendControl(value: object): void {
  sendControlText(JSON.stringify(value));
}

async function closeGatt(): Promise<void> {
  await lifecycle.close();
}

async function fail(error: unknown): Promise<void> {
  const shouldReport = lifecycle.beginFailure();
  if (shouldReport) {
    const message = boundedError(error);
    setStatus(message, "error");
    try {
      sendControl({ type: "closed", error: message });
    } catch {
      // Rust may already have closed the loopback bridge.
    }
  }
  await closeGatt();
  socket?.close(1000, "terminal");
  connectButton.disabled = true;
}

function characteristicValue(event: Event): DataView {
  const value = (event as BluetoothValueEvent).target.value;
  if (!value) {
    throw new Error("TX indication did not contain a value");
  }
  return value;
}

function handleProof(value: unknown): void {
  if (typeof value !== "object" || value === null) {
    throw new Error("Rust proof envelope is invalid");
  }
  const envelope = value as Partial<ProofEnvelope>;
  if (envelope.type !== "proof" || envelope.evidence === undefined) {
    throw new Error("Rust sent an unknown bridge control message");
  }
  const serialized = JSON.stringify(envelope.evidence, null, 2);
  if (serialized.length > 16 * 1024) {
    throw new Error("Rust proof exceeded the browser display bound");
  }
  evidence.textContent = serialized;
  evidence.className = "pass";
  setStatus("Authenticated suite-3 identity proof received.", "pass");
  sendControl({ type: "proof_ack" });
  lifecycle.markCompleted();
}

function handleSocketMessage(event: MessageEvent<unknown>): void {
  if (lifecycle.terminal || !profile) {
    return;
  }
  if (event.data instanceof ArrayBuffer) {
    const rx = lifecycle.rx;
    if (!rx) {
      void fail(new Error("Rust sent GATT bytes before BLE readiness"));
      return;
    }
    if (writeActive) {
      void fail(new Error("Rust queued more than one browser GATT write"));
      return;
    }
    writeActive = true;
    void relayWrite(
      rx,
      sendControlText,
      event.data,
      profile.maximumFragmentBytes,
    )
      .catch(fail)
      .finally(() => {
        writeActive = false;
      });
    return;
  }
  if (typeof event.data === "string") {
    try {
      handleProof(JSON.parse(event.data));
    } catch (error) {
      void fail(error);
    }
    return;
  }
  void fail(new Error("local Rust bridge sent an unsupported WebSocket frame"));
}

async function connectGatt(): Promise<void> {
  if (!profile || !socket || lifecycle.terminal) {
    throw new Error("local Rust bridge is not ready");
  }
  const bluetooth = (
    navigator as Navigator & { readonly bluetooth?: BluetoothFacade }
  ).bluetooth;
  if (!bluetooth) {
    throw new Error(
      "Web Bluetooth is unavailable; use current Chrome or Edge on macOS",
    );
  }
  connectButton.disabled = true;
  setStatus("Choose the E290 advertising the Reticulum service.");
  const device = await bluetooth.requestDevice({
    filters: [{ services: [profile.serviceUuid] }],
  });
  const onDisconnected: EventListener = () => {
    if (lifecycle.terminal) {
      void closeGatt();
      return;
    }
    void fail(new Error("E290 GATT link disconnected"));
  };
  lifecycle.trackDevice(device, onDisconnected);
  await lifecycle.requireActive("Web Bluetooth device selection");
  if (!device.gatt) {
    throw new Error("selected device does not expose a GATT server");
  }
  const server = await withTimeout(
    device.gatt.connect(),
    profile.operationTimeoutMs,
    "GATT connect",
  );
  lifecycle.trackServer(server);
  await lifecycle.requireActive("GATT connect");
  const service = await withTimeout(
    server.getPrimaryService(profile.serviceUuid),
    profile.operationTimeoutMs,
    "service discovery",
  );
  await lifecycle.requireActive("service discovery");
  const rx = await withTimeout(
    service.getCharacteristic(profile.rxUuid),
    profile.operationTimeoutMs,
    "RX characteristic discovery",
  );
  lifecycle.trackRx(rx);
  await lifecycle.requireActive("RX characteristic discovery");
  const tx = await withTimeout(
    service.getCharacteristic(profile.txUuid),
    profile.operationTimeoutMs,
    "TX characteristic discovery",
  );
  lifecycle.trackTx(tx);
  await lifecycle.requireActive("TX characteristic discovery");
  validateCapabilities({
    writeWithResponse: rx.properties.write,
    indicate: tx.properties.indicate,
  });
  tx.addEventListener("characteristicvaluechanged", (event) => {
    if (!profile || !socket) {
      void fail(new Error("indication arrived without an active bridge"));
      return;
    }
    try {
      indications.push(
        binarySender(),
        characteristicValue(event),
        profile.maximumFragmentBytes,
      );
    } catch (error) {
      void fail(error);
    }
  });
  await withTimeout(
    tx.startNotifications(),
    profile.operationTimeoutMs,
    "TX indication subscription",
  );
  await lifecycle.requireActive("TX indication subscription");
  sendControl({
    type: "ready",
    device_id: device.id,
    device_name: device.name ?? null,
    rx_write_with_response: true,
    tx_indicate: true,
    maximum_write_bytes: profile.maximumFragmentBytes,
  });
  indications.markReady(binarySender());
  setStatus("GATT validated. Rust is authenticating suite 3…");
}

async function initialize(): Promise<void> {
  const response = await fetch(new URL("profile.json", window.location.href), {
    cache: "no-store",
  });
  if (!response.ok) {
    throw new Error(`profile request failed with HTTP ${response.status}`);
  }
  profile = parseBridgeProfile(await response.json());
  socket = new WebSocket(bridgeUrl());
  socket.binaryType = "arraybuffer";
  socket.addEventListener("message", handleSocketMessage);
  socket.addEventListener("close", () => {
    void closeGatt();
    if (!lifecycle.terminal && !lifecycle.completed) {
      void fail(new Error("local Rust bridge closed"));
    }
  });
  socket.addEventListener("error", () => {
    if (lifecycle.completed) {
      void closeGatt();
    } else {
      void fail(new Error("local Rust bridge WebSocket failed"));
    }
  });
  await withTimeout(
    new Promise<void>((resolve, reject) => {
      socket?.addEventListener("open", () => resolve(), { once: true });
      socket?.addEventListener(
        "error",
        () => reject(new Error("local Rust bridge WebSocket failed to open")),
        { once: true },
      );
    }),
    profile.operationTimeoutMs,
    "local bridge connection",
  );
  connectButton.disabled = false;
  setStatus("Local Rust bridge ready. Click to choose the E290.");
}

function boot(): void {
  connectButton = requiredElement<HTMLButtonElement>("connect");
  status = requiredElement<HTMLParagraphElement>("status");
  evidence = requiredElement<HTMLPreElement>("evidence");
  connectButton.addEventListener("click", () => {
    void connectGatt().catch(fail);
  });
  void initialize().catch(fail);
}

if (typeof document !== "undefined") {
  boot();
}
