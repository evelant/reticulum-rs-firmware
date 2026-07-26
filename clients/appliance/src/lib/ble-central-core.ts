import type {
  BleCandidate,
  BleCentral,
  BleConnection,
  BleConnectionObserver,
  BleConnectOptions,
  BleDisconnectEvent,
  BleGattProfile,
  BleScanOptions,
} from "./ble-central-types.ts";

export const MINIMUM_WRITE_WITH_RESPONSE_BYTES = 20;

/**
 * Prefer the name carried by this advertisement over a cached platform name.
 *
 * Exact credential-derived targeting is about the peripheral's current local
 * name. iOS and Android may retain `Peripheral.name` from an earlier discovery,
 * so allowing that cache to mask `advertising.localName` can select the wrong
 * board when multiple E290s are nearby.
 */
export function advertisedPeripheralName(
  advertisedLocalName: string | undefined,
  platformName: string | undefined,
): string | undefined {
  return advertisedLocalName ?? platformName;
}
export const DEFAULT_BLE_SCAN_TIMEOUT_MS = 20_000;
export const DEFAULT_BLE_OPERATION_TIMEOUT_MS = 30_000;
export const DEFAULT_BLE_CONNECTION_TIMEOUT_MS = 90_000;
export const MAX_PENDING_INDICATION_BYTES = 64 * 1024;
export const MAX_PENDING_INDICATION_CHUNKS = 256;

export interface BleDiscoveredPeripheral {
  readonly id: string;
  readonly name?: string;
  readonly rssi?: number;
}

export interface BleDriverDisconnectEvent {
  readonly code?: number;
  readonly description?: string;
  readonly domain?: string;
  readonly peripheralId: string;
  readonly status?: number;
}

export interface BleDriverIndicationEvent {
  readonly bytes: Uint8Array;
  readonly characteristicUuid: string;
  readonly peripheralId: string;
  readonly serviceUuid: string;
}

export interface BleGattCharacteristic {
  readonly canIndicate: boolean;
  readonly canRead: boolean;
  readonly canWriteWithResponse: boolean;
  readonly characteristicUuid: string;
  readonly serviceUuid: string;
}

export interface BleGattDiscovery {
  readonly characteristics: readonly BleGattCharacteristic[];
  readonly serviceUuids: readonly string[];
}

export interface BleCentralDriver {
  prepare(): Promise<void>;
  onDiscovered(listener: (peripheral: BleDiscoveredPeripheral) => void): () => void;
  onDisconnected(listener: (event: BleDriverDisconnectEvent) => void): () => void;
  onIndication(listener: (event: BleDriverIndicationEvent) => void): () => void;
  startScan(serviceUuid: string, allowDuplicates: boolean): Promise<void>;
  stopScan(): Promise<void>;
  connect(peripheralId: string): Promise<void>;
  discover(peripheralId: string, serviceUuid: string): Promise<BleGattDiscovery>;
  startIndications(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void>;
  stopIndications(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void>;
  maximumWriteWithResponseBytes(peripheralId: string): Promise<number>;
  read(peripheralId: string, serviceUuid: string, characteristicUuid: string): Promise<Uint8Array>;
  writeWithResponse(
    peripheralId: string,
    serviceUuid: string,
    characteristicUuid: string,
    chunk: Uint8Array,
    maximumChunkBytes: number,
  ): Promise<void>;
  disconnect(peripheralId: string): Promise<void>;
}

interface ConnectionResources {
  readonly driver: BleCentralDriver;
  readonly operationTimeoutMs: number;
  readonly profile: BleGattProfile;
  readonly release: () => void;
  readonly removeListeners: () => void;
}

function normalizeUuid(uuid: string): string {
  return uuid.trim().replaceAll("-", "").replaceAll("{", "").replaceAll("}", "").toLowerCase();
}

function sameUuid(left: string, right: string): boolean {
  return normalizeUuid(left) === normalizeUuid(right);
}

function samePeripheral(left: string, right: string): boolean {
  return left.toLowerCase() === right.toLowerCase();
}

function validateProfile(profile: BleGattProfile): void {
  const values = [
    ["service", profile.serviceUuid],
    ["write characteristic", profile.writeCharacteristicUuid],
    ["indicate characteristic", profile.indicateCharacteristicUuid],
    ["security confirmation characteristic", profile.securityConfirmationCharacteristicUuid],
  ] as const;
  for (const [label, uuid] of values) {
    if (normalizeUuid(uuid).length === 0) {
      throw new Error(`BLE ${label} UUID must not be empty`);
    }
  }
  if (
    !Number.isInteger(profile.maximumWriteValueBytes) ||
    profile.maximumWriteValueBytes < MINIMUM_WRITE_WITH_RESPONSE_BYTES
  ) {
    throw new Error(
      `BLE profile write value maximum must be an integer of at least ${MINIMUM_WRITE_WITH_RESPONSE_BYTES} bytes`,
    );
  }
  if (profile.securityConfirmationReadyValue.byteLength === 0) {
    throw new Error("BLE security confirmation ready value must not be empty");
  }
}

function validateDiscovery(discovery: BleGattDiscovery, profile: BleGattProfile): void {
  if (!discovery.serviceUuids.some((uuid) => sameUuid(uuid, profile.serviceUuid))) {
    throw new Error(`BLE appliance service ${profile.serviceUuid} was not discovered`);
  }

  const write = discovery.characteristics.find(
    (candidate) =>
      sameUuid(candidate.serviceUuid, profile.serviceUuid) &&
      sameUuid(candidate.characteristicUuid, profile.writeCharacteristicUuid),
  );
  if (!write?.canWriteWithResponse) {
    throw new Error(
      `BLE appliance characteristic ${profile.writeCharacteristicUuid} does not support write with response`,
    );
  }

  const indicate = discovery.characteristics.find(
    (candidate) =>
      sameUuid(candidate.serviceUuid, profile.serviceUuid) &&
      sameUuid(candidate.characteristicUuid, profile.indicateCharacteristicUuid),
  );
  if (!indicate?.canIndicate) {
    throw new Error(
      `BLE appliance characteristic ${profile.indicateCharacteristicUuid} does not support indications`,
    );
  }

  const securityConfirmation = discovery.characteristics.find(
    (candidate) =>
      sameUuid(candidate.serviceUuid, profile.serviceUuid) &&
      sameUuid(candidate.characteristicUuid, profile.securityConfirmationCharacteristicUuid),
  );
  if (!securityConfirmation?.canRead) {
    throw new Error(
      `BLE appliance characteristic ${profile.securityConfirmationCharacteristicUuid} does not support reads`,
    );
  }
}

function safeMaximumWriteBytes(reported: number): number {
  if (!Number.isFinite(reported) || reported <= 0) return MINIMUM_WRITE_WITH_RESPONSE_BYTES;
  const maximum = Math.floor(reported);
  if (maximum < MINIMUM_WRITE_WITH_RESPONSE_BYTES) {
    throw new Error(
      `BLE platform reported a ${maximum}-byte write-with-response maximum; at least ${MINIMUM_WRITE_WITH_RESPONSE_BYTES} bytes are required`,
    );
  }
  return maximum;
}

function disconnectReason(event: BleDriverDisconnectEvent): string {
  const details = [
    event.description === undefined ? null : event.description,
    event.status === undefined ? null : `status ${event.status}`,
    event.domain === undefined ? null : event.domain,
    event.code === undefined ? null : `code ${event.code}`,
  ].filter((detail): detail is string => detail !== null);
  return details.length === 0
    ? "BLE peripheral disconnected"
    : `BLE disconnected (${details.join(", ")})`;
}

function cancellationError(signal: AbortSignal): Error {
  return signal.reason instanceof Error
    ? signal.reason
    : new Error("BLE connection attempt was cancelled");
}

function driverOperation<T>(operation: () => Promise<T>): Promise<T> {
  try {
    return operation();
  } catch (error) {
    return Promise.reject(error);
  }
}

function withDeadline<T>(
  promise: Promise<T>,
  timeoutMs: number,
  message: string,
  signal?: AbortSignal,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const finish = (callback: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      callback();
    };
    const onAbort = () => finish(() => reject(cancellationError(signal as AbortSignal)));
    const timer = setTimeout(() => finish(() => reject(new Error(message))), timeoutMs);
    if (signal?.aborted) {
      onAbort();
    } else {
      signal?.addEventListener("abort", onAbort, { once: true });
    }
    promise.then(
      (value) => finish(() => resolve(value)),
      (error: unknown) => finish(() => reject(error)),
    );
  });
}

function boundedDriverOperation<T>(
  operation: () => Promise<T>,
  timeoutMs: number,
  message: string,
  signal?: AbortSignal,
): Promise<T> {
  return withDeadline(driverOperation(operation), timeoutMs, message, signal);
}

function validateTimeout(value: number, label: string): number {
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`${label} must be positive`);
  }
  return value;
}

function waitForObservationWindow(timeoutMs: number, signal: AbortSignal): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const finish = (callback: () => void) => {
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      callback();
    };
    const onAbort = () => finish(() => reject(cancellationError(signal)));
    const timer = setTimeout(() => finish(resolve), timeoutMs);
    if (signal.aborted) onAbort();
    else signal.addEventListener("abort", onAbort, { once: true });
  });
}

function candidateSignal(candidate: BleCandidate): number {
  return candidate.rssi ?? Number.NEGATIVE_INFINITY;
}

function compareCandidateText(left: string, right: string): number {
  if (left === right) return 0;
  return left < right ? -1 : 1;
}

function compareCandidates(left: BleCandidate, right: BleCandidate): number {
  const leftSignal = candidateSignal(left);
  const rightSignal = candidateSignal(right);
  if (leftSignal !== rightSignal) return leftSignal > rightSignal ? -1 : 1;
  const name = compareCandidateText(left.peripheralName ?? "", right.peripheralName ?? "");
  if (name !== 0) return name;
  return compareCandidateText(left.peripheralId, right.peripheralId);
}

interface CandidateObservation {
  candidate: BleCandidate;
  nameSignal: number;
}

const MAX_BLE_SCAN_CANDIDATES = 64;

class ManagedBleConnection implements BleConnection {
  readonly name?: string;
  readonly peripheralId: string;
  maxWriteWithResponseBytes = MINIMUM_WRITE_WITH_RESPONSE_BYTES;

  private closed = false;
  private closing: Promise<void> | undefined;
  private disconnectConfirmed = false;
  private disconnectEvent: BleDisconnectEvent | undefined;
  private readonly disconnectObserved: Promise<void>;
  private observer: BleConnectionObserver | undefined;
  private pendingIndicationBytes = 0;
  private pendingIndications: Uint8Array[] = [];
  private releaseDisconnectObserved: () => void = () => {};
  private resourcesReleased = false;
  private writeTail: Promise<void> = Promise.resolve();

  constructor(
    peripheral: BleDiscoveredPeripheral,
    private readonly resources: ConnectionResources,
  ) {
    this.peripheralId = peripheral.id;
    this.name = peripheral.name;
    this.disconnectObserved = new Promise<void>((resolve) => {
      this.releaseDisconnectObserved = resolve;
    });
  }

  setMaximumWriteBytes(reported: number): void {
    this.maxWriteWithResponseBytes = Math.min(
      safeMaximumWriteBytes(reported),
      this.resources.profile.maximumWriteValueBytes,
    );
  }

  get isClosed(): boolean {
    return this.closed;
  }

  observe(observer: BleConnectionObserver): () => void {
    if (this.observer !== undefined) {
      throw new Error("BLE connection already has an observer");
    }
    this.observer = observer;
    const pending = this.pendingIndications;
    this.pendingIndications = [];
    this.pendingIndicationBytes = 0;
    for (const bytes of pending) observer.onBytes(bytes);
    if (this.disconnectEvent !== undefined) observer.onDisconnect(this.disconnectEvent);

    let observing = true;
    return () => {
      if (!observing) return;
      observing = false;
      if (this.observer === observer) this.observer = undefined;
    };
  }

  receive(bytes: Uint8Array): void {
    if (this.closed) return;
    const owned = new Uint8Array(bytes);
    if (this.observer === undefined) {
      if (
        this.pendingIndications.length >= MAX_PENDING_INDICATION_CHUNKS ||
        this.pendingIndicationBytes + owned.byteLength > MAX_PENDING_INDICATION_BYTES
      ) {
        this.failAndDisconnect(
          "BLE indication buffer overflow before stream observer was installed",
        );
        return;
      }
      this.pendingIndications.push(owned);
      this.pendingIndicationBytes += owned.byteLength;
    } else {
      this.observer.onBytes(owned);
    }
  }

  remotelyDisconnected(event: BleDriverDisconnectEvent): void {
    if (this.disconnectConfirmed) return;
    const notifyObserver = !this.closed;
    this.closed = true;
    this.disconnectConfirmed = true;
    this.pendingIndications = [];
    this.pendingIndicationBytes = 0;
    this.releaseResources();
    this.releaseDisconnectObserved();
    if (notifyObserver) {
      this.disconnectEvent = {
        peripheralId: this.peripheralId,
        reason: disconnectReason(event),
      };
      this.observer?.onDisconnect(this.disconnectEvent);
    }
  }

  private releaseResources(): void {
    if (this.resourcesReleased) return;
    this.resourcesReleased = true;
    this.resources.removeListeners();
    this.resources.release();
  }

  private failAndDisconnect(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    this.pendingIndications = [];
    this.pendingIndicationBytes = 0;
    this.disconnectEvent = { peripheralId: this.peripheralId, reason };
    this.observer?.onDisconnect(this.disconnectEvent);
    void this.close().catch(() => {
      // The stream failure was already reported. Ownership remains held until
      // the platform confirms that this peripheral disconnected.
    });
  }

  write(chunk: Uint8Array): Promise<void> {
    const owned = new Uint8Array(chunk);
    if (owned.byteLength === 0) {
      return Promise.reject(new Error("BLE write chunk must not be empty"));
    }
    if (owned.byteLength > this.maxWriteWithResponseBytes) {
      return Promise.reject(
        new Error(
          `BLE write chunk is ${owned.byteLength} bytes; negotiated maximum is ${this.maxWriteWithResponseBytes}`,
        ),
      );
    }

    const operation = this.writeTail.then(async () => {
      if (this.closed) throw new Error("BLE connection is closed");
      await this.resources.driver.writeWithResponse(
        this.peripheralId,
        this.resources.profile.serviceUuid,
        this.resources.profile.writeCharacteristicUuid,
        owned,
        this.maxWriteWithResponseBytes,
      );
      if (this.closed) {
        throw new Error("BLE disconnected before write acknowledgement");
      }
    });
    this.writeTail = operation.catch(() => {});
    return operation;
  }

  async read(characteristicUuid: string, timeoutMs?: number): Promise<Uint8Array> {
    if (this.closed) throw new Error("BLE connection is closed");
    const deadlineMs =
      timeoutMs === undefined
        ? this.resources.operationTimeoutMs
        : validateTimeout(timeoutMs, "BLE read timeout");
    const bytes = await boundedDriverOperation(
      () =>
        this.resources.driver.read(
          this.peripheralId,
          this.resources.profile.serviceUuid,
          characteristicUuid,
        ),
      deadlineMs,
      `BLE characteristic read for ${this.peripheralId} timed out`,
    );
    if (this.closed) {
      throw new Error("BLE disconnected before read response");
    }
    return new Uint8Array(bytes);
  }

  async close(): Promise<void> {
    if (this.closing !== undefined) {
      await this.closing;
      return;
    }
    if (this.disconnectConfirmed) return;
    this.closed = true;
    this.pendingIndications = [];
    this.pendingIndicationBytes = 0;
    const closing = (async () => {
      let stopError: unknown;
      try {
        await boundedDriverOperation(
          () =>
            this.resources.driver.stopIndications(
              this.peripheralId,
              this.resources.profile.serviceUuid,
              this.resources.profile.indicateCharacteristicUuid,
            ),
          this.resources.operationTimeoutMs,
          `BLE indication unsubscribe for ${this.peripheralId} timed out`,
        );
      } catch (error) {
        stopError = error;
      }
      if (!this.disconnectConfirmed) {
        try {
          await boundedDriverOperation(
            () => this.resources.driver.disconnect(this.peripheralId),
            this.resources.operationTimeoutMs,
            `BLE disconnect for ${this.peripheralId} timed out`,
          );
        } catch (error) {
          if (stopError === undefined) stopError = error;
        }
      }
      if (stopError === undefined && !this.disconnectConfirmed) {
        await withDeadline(
          this.disconnectObserved,
          this.resources.operationTimeoutMs,
          `BLE disconnect event for ${this.peripheralId} timed out`,
        );
      }
      if (stopError !== undefined) throw stopError;
    })();
    this.closing = closing;
    try {
      await closing;
    } finally {
      if (this.closing === closing) this.closing = undefined;
    }
  }
}

export class ForegroundBleCentral implements BleCentral {
  readonly supported = true;

  private active: ManagedBleConnection | undefined;
  private connecting: AbortController | undefined;
  private disposed = false;
  private retiringScan: object | undefined;
  private scanning: AbortController | undefined;
  private scanningCompletion: Promise<readonly BleCandidate[]> | undefined;
  // A failed setup still owns its peripheral until native teardown is no
  // longer ambiguous, even though its public connect promise has settled.
  private retiringSetup: object | undefined;

  constructor(private readonly driver: BleCentralDriver) {}

  scan(serviceUuid: string, options: BleScanOptions = {}): Promise<readonly BleCandidate[]> {
    if (this.disposed) return Promise.reject(new Error("BLE central is disposed"));
    if (
      this.scanning !== undefined ||
      this.connecting !== undefined ||
      this.active !== undefined ||
      this.retiringScan !== undefined ||
      this.retiringSetup !== undefined
    ) {
      return Promise.reject(new Error("BLE central already owns a scan or connection"));
    }
    if (normalizeUuid(serviceUuid).length === 0) {
      return Promise.reject(new Error("BLE service UUID must not be empty"));
    }

    const scanTimeoutMs = validateTimeout(
      options.scanTimeoutMs ?? DEFAULT_BLE_SCAN_TIMEOUT_MS,
      "BLE scan timeout",
    );
    const operationTimeoutMs = validateTimeout(
      options.operationTimeoutMs ?? DEFAULT_BLE_OPERATION_TIMEOUT_MS,
      "BLE operation timeout",
    );
    const abort = new AbortController();
    const forwardCancellation = () => {
      abort.abort(options.signal?.reason ?? new Error("BLE scan was cancelled"));
    };
    if (options.signal?.aborted) forwardCancellation();
    else options.signal?.addEventListener("abort", forwardCancellation, { once: true });
    this.scanning = abort;

    const scanning = this.runScan(serviceUuid, scanTimeoutMs, operationTimeoutMs, abort).finally(
      () => {
        options.signal?.removeEventListener("abort", forwardCancellation);
        if (this.scanning === abort) this.scanning = undefined;
        if (this.scanningCompletion === scanning) this.scanningCompletion = undefined;
      },
    );
    this.scanningCompletion = scanning;
    return scanning;
  }

  private async runScan(
    serviceUuid: string,
    scanTimeoutMs: number,
    operationTimeoutMs: number,
    abort: AbortController,
  ): Promise<readonly BleCandidate[]> {
    const scanOwnership = {};
    const observations = new Map<string, CandidateObservation>();
    const removeDiscovered = this.driver.onDiscovered((peripheral) => {
      const key = peripheral.id.toLowerCase();
      const name = peripheral.name?.trim() || undefined;
      const rssi =
        peripheral.rssi === undefined || !Number.isFinite(peripheral.rssi)
          ? undefined
          : Math.round(peripheral.rssi);
      const existing = observations.get(key);
      if (existing === undefined) {
        if (observations.size >= MAX_BLE_SCAN_CANDIDATES) return;
        observations.set(key, {
          candidate: {
            peripheralId: peripheral.id,
            peripheralName: name,
            rssi,
          },
          nameSignal:
            name === undefined ? Number.NEGATIVE_INFINITY : (rssi ?? Number.NEGATIVE_INFINITY),
        });
        return;
      }

      const strongestRssi =
        rssi === undefined
          ? existing.candidate.rssi
          : Math.max(existing.candidate.rssi ?? Number.NEGATIVE_INFINITY, rssi);
      const nameSignal =
        name === undefined ? Number.NEGATIVE_INFINITY : (rssi ?? Number.NEGATIVE_INFINITY);
      const retainNewName =
        name !== undefined &&
        (existing.candidate.peripheralName === undefined || nameSignal > existing.nameSignal);
      observations.set(key, {
        candidate: {
          peripheralId: existing.candidate.peripheralId,
          peripheralName: retainNewName ? name : existing.candidate.peripheralName,
          rssi: strongestRssi === Number.NEGATIVE_INFINITY ? undefined : strongestRssi,
        },
        nameSignal: retainNewName ? nameSignal : existing.nameSignal,
      });
    });

    let scanStarted = false;
    let stopRequested = false;
    const retainCleanup = (cleanup: Promise<boolean>) => {
      this.retiringScan = scanOwnership;
      void cleanup.then((safeToRelease) => {
        if (safeToRelease && this.retiringScan === scanOwnership) {
          this.retiringScan = undefined;
        }
      });
    };
    try {
      if (abort.signal.aborted) throw cancellationError(abort.signal);
      await boundedDriverOperation(
        () => this.driver.prepare(),
        operationTimeoutMs,
        "BLE platform preparation timed out",
        abort.signal,
      );
      const scanStart = driverOperation(() => this.driver.startScan(serviceUuid, true));
      try {
        await withDeadline(scanStart, operationTimeoutMs, "BLE scan start timed out", abort.signal);
      } catch (error) {
        retainCleanup(
          scanStart.then(
            () =>
              driverOperation(() => this.driver.stopScan()).then(
                () => true,
                () => false,
              ),
            () => true,
          ),
        );
        throw error;
      }
      scanStarted = true;
      await waitForObservationWindow(scanTimeoutMs, abort.signal);
      const scanStop = driverOperation(() => this.driver.stopScan());
      stopRequested = true;
      try {
        await withDeadline(scanStop, operationTimeoutMs, "BLE scan stop timed out", abort.signal);
      } catch (error) {
        retainCleanup(
          scanStop.then(
            () => true,
            () => false,
          ),
        );
        throw error;
      }
      scanStarted = false;
      return [...observations.values()]
        .map((observation) => observation.candidate)
        .sort(compareCandidates);
    } finally {
      removeDiscovered();
      if (scanStarted && !stopRequested) {
        const teardown = driverOperation(() => this.driver.stopScan());
        try {
          await withDeadline(teardown, operationTimeoutMs, "BLE scan teardown timed out");
        } catch {
          retainCleanup(
            teardown.then(
              () => true,
              () => false,
            ),
          );
        }
      }
    }
  }

  async connect(profile: BleGattProfile, options: BleConnectOptions = {}): Promise<BleConnection> {
    if (this.disposed) throw new Error("BLE central is disposed");
    if (
      this.connecting !== undefined ||
      this.scanning !== undefined ||
      this.active !== undefined ||
      this.retiringScan !== undefined ||
      this.retiringSetup !== undefined
    ) {
      throw new Error("BLE central already owns a connection");
    }
    validateProfile(profile);
    const selectedPeripheralId = options.peripheralId?.trim();
    if (options.peripheralId !== undefined && selectedPeripheralId === "") {
      throw new Error("BLE peripheral identifier must not be empty");
    }

    const scanTimeoutMs = validateTimeout(
      options.scanTimeoutMs ?? DEFAULT_BLE_SCAN_TIMEOUT_MS,
      "BLE scan timeout",
    );
    const operationTimeoutMs = validateTimeout(
      options.operationTimeoutMs ?? DEFAULT_BLE_OPERATION_TIMEOUT_MS,
      "BLE operation timeout",
    );
    const connectionTimeoutMs = validateTimeout(
      options.connectionTimeoutMs ?? DEFAULT_BLE_CONNECTION_TIMEOUT_MS,
      "BLE connection timeout",
    );
    const abort = new AbortController();
    const forwardCancellation = () => {
      abort.abort(options.signal?.reason ?? new Error("BLE connection attempt was cancelled"));
    };
    if (options.signal?.aborted) forwardCancellation();
    else options.signal?.addEventListener("abort", forwardCancellation, { once: true });
    this.connecting = abort;

    let connected = false;
    let indicationsStarted = false;
    let scanStarted = false;
    let connection: ManagedBleConnection | undefined;
    let connectionAttemptStarted = false;
    let disconnectedDuringSetup: BleDriverDisconnectEvent | undefined;
    let disconnectDuringSetupConfirmed = false;
    let platformConnectionRejected = false;
    let setupTeardownStarted = false;
    let selected: BleDiscoveredPeripheral | undefined;
    const setupOwnership = {};
    const removeListeners: Array<() => void> = [];

    const removeAllListeners = () => {
      while (removeListeners.length > 0) removeListeners.pop()?.();
    };

    let resolvePeripheral: ((peripheral: BleDiscoveredPeripheral) => void) | undefined;
    const peripheralFound = new Promise<BleDiscoveredPeripheral>((resolve) => {
      resolvePeripheral = resolve;
    });
    let releaseDisconnectDuringSetup: () => void = () => {};
    const disconnectDuringSetup = new Promise<void>((resolve) => {
      releaseDisconnectDuringSetup = resolve;
    });

    const confirmDisconnectDuringSetup = (event: BleDriverDisconnectEvent) => {
      if (disconnectDuringSetupConfirmed) return;
      disconnectDuringSetupConfirmed = true;
      disconnectedDuringSetup = event;
      if (connection === undefined) removeAllListeners();
      else connection.remotelyDisconnected(event);
      if (this.retiringSetup === setupOwnership) this.retiringSetup = undefined;
      releaseDisconnectDuringSetup();
    };

    const confirmPlatformConnectionRejected = () => {
      if (platformConnectionRejected || setupTeardownStarted) return;
      platformConnectionRejected = true;
      removeAllListeners();
      if (this.retiringSetup === setupOwnership) this.retiringSetup = undefined;
    };

    const run = <T>(operation: () => Promise<T>, message: string): Promise<T> =>
      boundedDriverOperation(operation, operationTimeoutMs, message, abort.signal);
    const cleanUp = (operation: () => Promise<unknown>, message: string): Promise<void> =>
      boundedDriverOperation(operation, operationTimeoutMs, message)
        .then(() => undefined)
        .catch(() => undefined);
    const cleanUpStopScan = (): Promise<void> =>
      cleanUp(() => this.driver.stopScan(), "BLE scan teardown timed out");
    const cleanUpIndications = (peripheralId: string): Promise<void> =>
      cleanUp(
        () =>
          this.driver.stopIndications(
            peripheralId,
            profile.serviceUuid,
            profile.indicateCharacteristicUuid,
          ),
        `BLE indication unsubscribe for ${peripheralId} timed out`,
      );
    const cleanUpLateScan = (): Promise<void> => {
      if (this.connecting !== undefined && this.connecting !== abort) return Promise.resolve();
      return cleanUpStopScan();
    };
    const cleanUpLateLink = (
      peripheralId: string,
      operation: () => Promise<void>,
    ): Promise<void> => {
      if (disconnectDuringSetupConfirmed) return Promise.resolve();
      if (this.connecting !== undefined && this.connecting !== abort) {
        return Promise.resolve();
      }
      if (this.active !== undefined && samePeripheral(this.active.peripheralId, peripheralId)) {
        return Promise.resolve();
      }
      return operation();
    };

    try {
      if (abort.signal.aborted) throw cancellationError(abort.signal);
      removeListeners.push(
        this.driver.onDiscovered((peripheral) => {
          if (options.peripheralName !== undefined && peripheral.name !== options.peripheralName) {
            return;
          }
          if (
            options.peripheralId !== undefined &&
            !samePeripheral(peripheral.id, options.peripheralId)
          ) {
            return;
          }
          resolvePeripheral?.(peripheral);
          resolvePeripheral = undefined;
        }),
        this.driver.onDisconnected((event) => {
          if (selected === undefined || !samePeripheral(event.peripheralId, selected.id)) return;
          if (connection !== undefined && this.active === connection) {
            connection.remotelyDisconnected(event);
          } else if (connectionAttemptStarted) {
            confirmDisconnectDuringSetup(event);
          }
        }),
        this.driver.onIndication((event) => {
          if (
            connection === undefined ||
            !samePeripheral(event.peripheralId, connection.peripheralId) ||
            !sameUuid(event.serviceUuid, profile.serviceUuid) ||
            !sameUuid(event.characteristicUuid, profile.indicateCharacteristicUuid)
          ) {
            return;
          }
          connection.receive(event.bytes);
        }),
      );

      await run(() => this.driver.prepare(), "BLE platform preparation timed out");
      let peripheral: BleDiscoveredPeripheral;
      if (selectedPeripheralId !== undefined) {
        peripheral = {
          id: selectedPeripheralId,
          name: options.peripheralName,
        };
        selected = peripheral;
      } else {
        const scanStart = driverOperation(() => this.driver.startScan(profile.serviceUuid, false));
        try {
          await withDeadline(
            scanStart,
            operationTimeoutMs,
            "BLE scan start timed out",
            abort.signal,
          );
        } catch (error) {
          void scanStart.then(cleanUpLateScan, () => undefined);
          throw error;
        }
        scanStarted = true;
        peripheral = await withDeadline(
          peripheralFound,
          scanTimeoutMs,
          `No BLE appliance advertising ${profile.serviceUuid} was found within ${scanTimeoutMs} ms`,
          abort.signal,
        );
        selected = peripheral;
        await run(() => this.driver.stopScan(), `BLE scan stop for ${peripheral.id} timed out`);
        scanStarted = false;
      }

      connectionAttemptStarted = true;
      const platformConnection = driverOperation(() => this.driver.connect(peripheral.id));
      // A rejection observed before teardown proves that no link was produced.
      // Once teardown starts, Android may induce this rejection itself, so only
      // the matching disconnect event can release ownership.
      void platformConnection.then(
        () => {},
        () => confirmPlatformConnectionRejected(),
      );
      await withDeadline(
        platformConnection,
        connectionTimeoutMs,
        `BLE connection to ${peripheral.id} timed out after ${connectionTimeoutMs} ms`,
        abort.signal,
      );
      connected = true;
      if (disconnectedDuringSetup !== undefined) {
        throw new Error(disconnectReason(disconnectedDuringSetup));
      }
      const discovery = await run(
        () => this.driver.discover(peripheral.id, profile.serviceUuid),
        `BLE GATT discovery for ${peripheral.id} timed out`,
      );
      if (disconnectedDuringSetup !== undefined) {
        throw new Error(disconnectReason(disconnectedDuringSetup));
      }
      validateDiscovery(discovery, profile);

      connection = new ManagedBleConnection(peripheral, {
        driver: this.driver,
        operationTimeoutMs,
        profile,
        release: () => {
          if (this.active === connection) this.active = undefined;
        },
        removeListeners: removeAllListeners,
      });

      const indicationSubscription = driverOperation(() =>
        this.driver.startIndications(
          peripheral.id,
          profile.serviceUuid,
          profile.indicateCharacteristicUuid,
        ),
      );
      try {
        await withDeadline(
          indicationSubscription,
          operationTimeoutMs,
          `BLE indication subscription for ${peripheral.id} timed out`,
          abort.signal,
        );
      } catch (error) {
        void indicationSubscription.then(
          () => cleanUpLateLink(peripheral.id, () => cleanUpIndications(peripheral.id)),
          () => undefined,
        );
        throw error;
      }
      indicationsStarted = true;
      if (connection.isClosed) {
        throw new Error("BLE peripheral disconnected while indications were being subscribed");
      }
      const maximum = await run(
        () => this.driver.maximumWriteWithResponseBytes(peripheral.id),
        `BLE write MTU lookup for ${peripheral.id} timed out`,
      );
      connection.setMaximumWriteBytes(maximum);
      if (connection.isClosed) {
        throw new Error("BLE peripheral disconnected while the GATT link was being prepared");
      }
      if (abort.signal.aborted) throw cancellationError(abort.signal);
      this.active = connection;
      return connection;
    } catch (error) {
      if (scanStarted) {
        await cleanUpStopScan();
      }
      if (indicationsStarted && selected !== undefined) {
        await cleanUpIndications(selected.id);
      }
      if ((connected || connectionAttemptStarted) && selected !== undefined) {
        const peripheralId = selected.id;
        if (!disconnectDuringSetupConfirmed && !platformConnectionRejected) {
          setupTeardownStarted = true;
          await boundedDriverOperation(
            () => this.driver.disconnect(peripheralId),
            operationTimeoutMs,
            `BLE disconnect for ${peripheralId} timed out`,
          ).catch(() => undefined);
        }
        if (!disconnectDuringSetupConfirmed && !platformConnectionRejected) {
          await withDeadline(
            disconnectDuringSetup,
            operationTimeoutMs,
            `BLE disconnect event for ${peripheralId} timed out`,
          ).catch(() => undefined);
        }
        if (!disconnectDuringSetupConfirmed && !platformConnectionRejected) {
          this.retiringSetup = setupOwnership;
        }
      } else {
        removeAllListeners();
      }
      throw error;
    } finally {
      options.signal?.removeEventListener("abort", forwardCancellation);
      if (this.connecting === abort) this.connecting = undefined;
    }
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.scanning?.abort(new Error("BLE central was disposed during scanning"));
    this.connecting?.abort(new Error("BLE central was disposed during connection setup"));
    await this.scanningCompletion?.catch(() => undefined);
    await this.active?.close();
  }
}
