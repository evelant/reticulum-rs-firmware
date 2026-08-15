// src/bridge-core.ts
var BRIDGE_PROTOCOL_VERSION = 1;
var FRAME_WRITE = 1;
var FRAME_INDICATION = 2;
var MAX_SOCKET_BUFFER_BYTES = 64 * 1024;
var MAX_PRE_READY_INDICATION_FRAMES = 32;
var MAXIMUM_PROFILE_FRAGMENT_BYTES = 248;
var MAX_PRE_READY_INDICATION_BYTES = MAX_PRE_READY_INDICATION_FRAMES * (MAXIMUM_PROFILE_FRAGMENT_BYTES + 2);
function sendBounded(sender, frame) {
  if (sender.bufferedAmount + frame.byteLength > MAX_SOCKET_BUFFER_BYTES) {
    throw new Error("browser bridge WebSocket send buffer exceeded its bound");
  }
  sender.send(frame);
}

class PreReadyIndicationBuffer {
  #frames = [];
  #bytes = 0;
  #ready = false;
  push(sender, value, maximumFragmentBytes) {
    const frame = encodeIndicationFrame(value, maximumFragmentBytes);
    if (this.#ready) {
      sendBounded(sender, frame);
      return;
    }
    if (this.#frames.length >= MAX_PRE_READY_INDICATION_FRAMES || this.#bytes + frame.byteLength > MAX_PRE_READY_INDICATION_BYTES) {
      throw new Error("pre-ready GATT indication queue exceeded its bound");
    }
    this.#frames.push(frame);
    this.#bytes += frame.byteLength;
  }
  markReady(sender) {
    if (this.#ready) {
      throw new Error("GATT indication buffer was marked ready twice");
    }
    this.#ready = true;
    for (const frame of this.#frames) {
      sendBounded(sender, frame);
    }
    this.#frames.length = 0;
    this.#bytes = 0;
  }
}
function requireInteger(value, name, minimum, maximum) {
  if (typeof value !== "number" || !Number.isInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} is outside ${minimum}..=${maximum}`);
  }
  return value;
}
function requireUuid(value, name) {
  if (typeof value !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu.test(value)) {
    throw new Error(`${name} is not a canonical UUID`);
  }
  return value.toLowerCase();
}
function parseBridgeProfile(value) {
  if (typeof value !== "object" || value === null) {
    throw new Error("bridge profile is not an object");
  }
  const profile = value;
  const bridgeProtocol = requireInteger(profile.bridgeProtocol, "bridgeProtocol", 1, 255);
  if (bridgeProtocol !== BRIDGE_PROTOCOL_VERSION) {
    throw new Error(`unsupported bridge protocol ${bridgeProtocol}`);
  }
  const maximumFragmentBytes = requireInteger(profile.maximumFragmentBytes, "maximumFragmentBytes", 1, MAXIMUM_PROFILE_FRAGMENT_BYTES);
  const operationTimeoutMs = requireInteger(profile.operationTimeoutMs, "operationTimeoutMs", 1, 60000);
  if (profile.writeType !== "with_response") {
    throw new Error("profile does not require write-with-response");
  }
  if (profile.txDelivery !== "indication") {
    throw new Error("profile does not require TX indications");
  }
  return {
    bridgeProtocol,
    gattProfileMajor: requireInteger(profile.gattProfileMajor, "gattProfileMajor", 1, 65535),
    gattProfileMinor: requireInteger(profile.gattProfileMinor, "gattProfileMinor", 0, 65535),
    serviceUuid: requireUuid(profile.serviceUuid, "serviceUuid"),
    rxUuid: requireUuid(profile.rxUuid, "rxUuid"),
    txUuid: requireUuid(profile.txUuid, "txUuid"),
    maximumFragmentBytes,
    operationTimeoutMs,
    writeType: "with_response",
    txDelivery: "indication"
  };
}
function validateCapabilities(capabilities) {
  if (!capabilities.writeWithResponse) {
    throw new Error("RX characteristic lacks write-with-response");
  }
  if (!capabilities.indicate) {
    throw new Error("TX characteristic lacks indication support");
  }
}
function decodeWriteFrame(frame, maximumFragmentBytes) {
  const bytes = new Uint8Array(frame);
  const headerBytes = 6;
  if (bytes.length <= headerBytes || bytes.length > headerBytes + maximumFragmentBytes) {
    throw new Error("write bridge frame has an invalid size");
  }
  if (bytes[0] !== BRIDGE_PROTOCOL_VERSION || bytes[1] !== FRAME_WRITE) {
    throw new Error("write bridge frame has an invalid header");
  }
  const id = new DataView(frame).getUint32(2, false);
  return { id, value: bytes.slice(headerBytes) };
}
function encodeIndicationFrame(value, maximumFragmentBytes) {
  if (value.byteLength === 0 || value.byteLength > maximumFragmentBytes) {
    throw new Error("GATT indication has an invalid size");
  }
  const frame = new Uint8Array(2 + value.byteLength);
  frame[0] = BRIDGE_PROTOCOL_VERSION;
  frame[1] = FRAME_INDICATION;
  frame.set(new Uint8Array(value.buffer, value.byteOffset, value.byteLength), 2);
  return frame;
}
async function relayWrite(writer, sender, frame, maximumFragmentBytes) {
  const write = decodeWriteFrame(frame, maximumFragmentBytes);
  try {
    await writer.writeValueWithResponse(write.value);
    sender(JSON.stringify({ type: "write_ack", id: write.id }));
  } catch (error) {
    sender(JSON.stringify({
      type: "write_error",
      id: write.id,
      error: boundedError(error)
    }));
  }
}
function boundedError(error) {
  const text = error instanceof Error ? error.message : String(error);
  return text.slice(0, 256);
}

// src/main.ts
class GattLifecycle {
  #device;
  #disconnectListener;
  #server;
  #rx;
  #tx;
  #terminal = false;
  #completed = false;
  get terminal() {
    return this.#terminal;
  }
  get completed() {
    return this.#completed;
  }
  get rx() {
    return this.#rx;
  }
  trackDevice(device, disconnectListener) {
    this.#device = device;
    this.#disconnectListener = disconnectListener;
    device.addEventListener("gattserverdisconnected", disconnectListener);
  }
  trackServer(server) {
    this.#server = server;
  }
  trackRx(rx) {
    this.#rx = rx;
  }
  trackTx(tx) {
    this.#tx = tx;
  }
  markCompleted() {
    this.#completed = true;
    this.#terminal = true;
  }
  beginFailure() {
    const shouldReport = !this.#terminal && !this.#completed;
    this.#terminal = true;
    return shouldReport;
  }
  async requireActive(stage) {
    if (!this.#terminal) {
      return;
    }
    await this.close();
    throw new Error(`${stage} completed after the local bridge became terminal`);
  }
  async close() {
    const device = this.#device;
    const disconnectListener = this.#disconnectListener;
    const server = this.#server;
    const tx = this.#tx;
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
      } catch {}
    }
    server?.disconnect();
  }
}
var connectButton;
var status;
var evidence;
var indications = new PreReadyIndicationBuffer;
var lifecycle = new GattLifecycle;
var socket;
var profile;
var writeActive = false;
function binarySender() {
  return {
    get bufferedAmount() {
      return socket?.bufferedAmount ?? Number.MAX_SAFE_INTEGER;
    },
    send(value) {
      socket?.send(value);
    }
  };
}
function requiredElement(id) {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`missing page element ${id}`);
  }
  return element;
}
function setStatus(message, kind) {
  status.textContent = message;
  status.className = kind ?? "";
}
function bridgeUrl() {
  const url = new URL("bridge", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.href;
}
function withTimeout(operation, timeoutMs, stage) {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error(`${stage} exceeded ${timeoutMs} ms`)), timeoutMs);
    operation.then((value) => {
      window.clearTimeout(timer);
      resolve(value);
    }, (error) => {
      window.clearTimeout(timer);
      reject(error);
    });
  });
}
function sendControlText(text) {
  if (socket?.readyState !== WebSocket.OPEN) {
    throw new Error("local Rust bridge is not open");
  }
  if (socket.bufferedAmount + new TextEncoder().encode(text).byteLength > 64 * 1024) {
    throw new Error("local Rust bridge control queue exceeded its bound");
  }
  socket.send(text);
}
function sendControl(value) {
  sendControlText(JSON.stringify(value));
}
async function closeGatt() {
  await lifecycle.close();
}
async function fail(error) {
  const shouldReport = lifecycle.beginFailure();
  if (shouldReport) {
    const message = boundedError(error);
    setStatus(message, "error");
    try {
      sendControl({ type: "closed", error: message });
    } catch {}
  }
  await closeGatt();
  socket?.close(1000, "terminal");
  connectButton.disabled = true;
}
function characteristicValue(event) {
  const value = event.target.value;
  if (!value) {
    throw new Error("TX indication did not contain a value");
  }
  return value;
}
function handleProof(value) {
  if (typeof value !== "object" || value === null) {
    throw new Error("Rust proof envelope is invalid");
  }
  const envelope = value;
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
function handleSocketMessage(event) {
  if (lifecycle.terminal || !profile) {
    return;
  }
  if (event.data instanceof ArrayBuffer) {
    const rx = lifecycle.rx;
    if (!rx) {
      fail(new Error("Rust sent GATT bytes before BLE readiness"));
      return;
    }
    if (writeActive) {
      fail(new Error("Rust queued more than one browser GATT write"));
      return;
    }
    writeActive = true;
    relayWrite(rx, sendControlText, event.data, profile.maximumFragmentBytes).catch(fail).finally(() => {
      writeActive = false;
    });
    return;
  }
  if (typeof event.data === "string") {
    try {
      handleProof(JSON.parse(event.data));
    } catch (error) {
      fail(error);
    }
    return;
  }
  fail(new Error("local Rust bridge sent an unsupported WebSocket frame"));
}
async function connectGatt() {
  if (!profile || !socket || lifecycle.terminal) {
    throw new Error("local Rust bridge is not ready");
  }
  const bluetooth = navigator.bluetooth;
  if (!bluetooth) {
    throw new Error("Web Bluetooth is unavailable; use current Chrome or Edge on macOS");
  }
  connectButton.disabled = true;
  setStatus("Choose the E290 advertising the Reticulum service.");
  const device = await bluetooth.requestDevice({
    filters: [{ services: [profile.serviceUuid] }]
  });
  const onDisconnected = () => {
    if (lifecycle.terminal) {
      closeGatt();
      return;
    }
    fail(new Error("E290 GATT link disconnected"));
  };
  lifecycle.trackDevice(device, onDisconnected);
  await lifecycle.requireActive("Web Bluetooth device selection");
  if (!device.gatt) {
    throw new Error("selected device does not expose a GATT server");
  }
  const server = await withTimeout(device.gatt.connect(), profile.operationTimeoutMs, "GATT connect");
  lifecycle.trackServer(server);
  await lifecycle.requireActive("GATT connect");
  const service = await withTimeout(server.getPrimaryService(profile.serviceUuid), profile.operationTimeoutMs, "service discovery");
  await lifecycle.requireActive("service discovery");
  const rx = await withTimeout(service.getCharacteristic(profile.rxUuid), profile.operationTimeoutMs, "RX characteristic discovery");
  lifecycle.trackRx(rx);
  await lifecycle.requireActive("RX characteristic discovery");
  const tx = await withTimeout(service.getCharacteristic(profile.txUuid), profile.operationTimeoutMs, "TX characteristic discovery");
  lifecycle.trackTx(tx);
  await lifecycle.requireActive("TX characteristic discovery");
  validateCapabilities({
    writeWithResponse: rx.properties.write,
    indicate: tx.properties.indicate
  });
  tx.addEventListener("characteristicvaluechanged", (event) => {
    if (!profile || !socket) {
      fail(new Error("indication arrived without an active bridge"));
      return;
    }
    try {
      indications.push(binarySender(), characteristicValue(event), MAXIMUM_PROFILE_FRAGMENT_BYTES);
    } catch (error) {
      fail(error);
    }
  });
  await withTimeout(tx.startNotifications(), profile.operationTimeoutMs, "TX indication subscription");
  await lifecycle.requireActive("TX indication subscription");
  sendControl({
    type: "ready",
    device_id: device.id,
    device_name: device.name ?? null,
    rx_write_with_response: true,
    tx_indicate: true,
    maximum_write_bytes: profile.maximumFragmentBytes
  });
  indications.markReady(binarySender());
  setStatus("GATT validated. Rust is authenticating suite 3…");
}
async function initialize() {
  const response = await fetch(new URL("profile.json", window.location.href), {
    cache: "no-store"
  });
  if (!response.ok) {
    throw new Error(`profile request failed with HTTP ${response.status}`);
  }
  profile = parseBridgeProfile(await response.json());
  socket = new WebSocket(bridgeUrl());
  socket.binaryType = "arraybuffer";
  socket.addEventListener("message", handleSocketMessage);
  socket.addEventListener("close", () => {
    closeGatt();
    if (!lifecycle.terminal && !lifecycle.completed) {
      fail(new Error("local Rust bridge closed"));
    }
  });
  socket.addEventListener("error", () => {
    if (lifecycle.completed) {
      closeGatt();
    } else {
      fail(new Error("local Rust bridge WebSocket failed"));
    }
  });
  await withTimeout(new Promise((resolve, reject) => {
    socket?.addEventListener("open", () => resolve(), { once: true });
    socket?.addEventListener("error", () => reject(new Error("local Rust bridge WebSocket failed to open")), { once: true });
  }), profile.operationTimeoutMs, "local bridge connection");
  connectButton.disabled = false;
  setStatus("Local Rust bridge ready. Click to choose the E290.");
}
function boot() {
  connectButton = requiredElement("connect");
  status = requiredElement("status");
  evidence = requiredElement("evidence");
  connectButton.addEventListener("click", () => {
    connectGatt().catch(fail);
  });
  initialize().catch(fail);
}
if (typeof document !== "undefined") {
  boot();
}
export {
  GattLifecycle
};
