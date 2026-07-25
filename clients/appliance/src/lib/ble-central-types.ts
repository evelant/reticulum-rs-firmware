export interface BleGattProfile {
  readonly indicateCharacteristicUuid: string;
  /**
   * The server profile's maximum characteristic value length. This is separate
   * from the platform/ATT write maximum and should come from generated profile
   * metadata alongside the UUIDs.
   */
  readonly maximumWriteValueBytes: number;
  readonly serviceUuid: string;
  readonly writeCharacteristicUuid: string;
}

export interface BleConnectOptions {
  /**
   * Selects a peripheral by its exact advertised name when more than one
   * appliance advertises the requested service.
   */
  readonly peripheralName?: string;
  /**
   * Selects a known peripheral when more than one appliance advertises the
   * requested service. The first matching advertisement is used when omitted.
   */
  readonly peripheralId?: string;
  /**
   * Bounds each native BLE setup or teardown operation independently.
   */
  readonly operationTimeoutMs?: number;
  readonly scanTimeoutMs?: number;
  /**
   * Cancels this connection attempt. Implementations must invalidate any late
   * native result and clean up a link that completes after cancellation.
   */
  readonly signal?: AbortSignal;
}

export interface BleCandidate {
  readonly peripheralId: string;
  readonly peripheralName?: string;
  readonly rssi?: number;
}

export interface BleScanOptions {
  /**
   * Bounds each native BLE setup or teardown operation independently.
   */
  readonly operationTimeoutMs?: number;
  /**
   * Bounds the foreground observation window. Candidates are returned only
   * after the full window closes so the caller can require an explicit choice.
   */
  readonly scanTimeoutMs?: number;
  readonly signal?: AbortSignal;
}

export interface BleDisconnectEvent {
  readonly peripheralId: string;
  readonly reason: string;
}

export interface BleConnectionObserver {
  readonly onBytes: (bytes: Uint8Array) => void;
  readonly onDisconnect: (event: BleDisconnectEvent) => void;
}

export interface BleConnection {
  readonly maxWriteWithResponseBytes: number;
  readonly name?: string;
  readonly peripheralId: string;

  /**
   * Installs the one owner of this ordered byte stream. Bytes received between
   * GATT subscription and observer installation are replayed in order.
   */
  observe(observer: BleConnectionObserver): () => void;

  /**
   * Writes one opaque GATT chunk with response. Concurrent calls are serialized
   * and chunks larger than maxWriteWithResponseBytes are rejected.
   */
  write(chunk: Uint8Array): Promise<void>;

  /**
   * Stops indications and requests a disconnect, then waits for the platform's
   * matching disconnect event. An intentional close is not reported to the
   * observer as a remote disconnect.
   */
  close(): Promise<void>;
}

export interface BleCentral {
  /**
   * This first implementation is foreground-only. It deliberately configures
   * no background restoration, scanning, or Android foreground service.
   */
  readonly supported: boolean;

  /**
   * Observes advertisements for the caller-owned service without connecting,
   * discovering characteristics, subscribing, or exchanging protocol bytes.
   */
  scan(serviceUuid: string, options?: BleScanOptions): Promise<readonly BleCandidate[]>;

  /**
   * Scans for the caller-owned service UUID and returns only after the service
   * and characteristics are discovered and TX indications are subscribed.
   */
  connect(profile: BleGattProfile, options?: BleConnectOptions): Promise<BleConnection>;

  dispose(): Promise<void>;
}
