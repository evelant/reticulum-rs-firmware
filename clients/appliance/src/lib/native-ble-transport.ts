import type { NativeApplianceLike, NativeBlePlatformCommand } from "@reticulum/appliance-native";

import type {
  BleCentral,
  BleConnection,
  BleDisconnectEvent,
  BleGattProfile,
} from "./ble-central-types.ts";

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

export type DecodedNativeBlePlatformCommand =
  | {
      readonly bytes: ArrayBuffer;
      readonly generation: bigint;
      readonly kind: "write";
      readonly token: bigint;
    }
  | {
      readonly generation: bigint;
      readonly kind: "disconnect";
      readonly reason: string;
    };

export type NativeBleCommandDecoder = (
  command: NativeBlePlatformCommand,
) => DecodedNativeBlePlatformCommand;

export interface NativeBleTransportConfig {
  readonly central: BleCentral;
  readonly decodeCommand: NativeBleCommandDecoder;
  readonly peripheralName?: string;
  readonly profile: BleGattProfile;
}

const MAX_PLATFORM_REASON_BYTES = 480;
export const PLATFORM_GATT_WRITE_TIMEOUT_MS = 10_000;
const textEncoder = new TextEncoder();

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function platformReason(reason: string): string {
  const normalized = reason.trim() || "BLE platform operation failed";
  if (textEncoder.encode(normalized).byteLength <= MAX_PLATFORM_REASON_BYTES) return normalized;

  let bounded = "";
  for (const character of normalized) {
    const candidate = `${bounded}${character}`;
    if (textEncoder.encode(candidate).byteLength > MAX_PLATFORM_REASON_BYTES) break;
    bounded = candidate;
  }
  return bounded;
}

function ownedArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const owned = new Uint8Array(bytes.byteLength);
  owned.set(bytes);
  return owned.buffer;
}

function withWriteTimeout(operation: Promise<void>, timeoutMs: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("BLE GATT write-with-response timed out")),
      timeoutMs,
    );
    operation.then(
      () => {
        clearTimeout(timer);
        resolve();
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

class NativeBleLink {
  readonly generation: bigint;

  #abort = new AbortController();
  #ending: Promise<void> | null = null;
  #removeObserver: (() => void) | null = null;
  #reportedDisconnected = false;
  #usable = true;

  private constructor(
    private readonly appliance: NativeBleAppliance,
    private readonly connection: BleConnection,
    private readonly decodeCommand: NativeBleCommandDecoder,
    private readonly writeTimeoutMs: number,
    generation: bigint,
  ) {
    this.generation = generation;
  }

  static async open(
    appliance: NativeBleAppliance,
    connection: BleConnection,
    decodeCommand: NativeBleCommandDecoder,
    writeTimeoutMs: number,
  ): Promise<NativeBleLink> {
    const generation = appliance.bleLinkConnected(
      connection.peripheralId,
      connection.maxWriteWithResponseBytes,
    );
    const link = new NativeBleLink(
      appliance,
      connection,
      decodeCommand,
      writeTimeoutMs,
      generation,
    );
    try {
      link.#begin();
    } catch (error) {
      await link
        .close(platformReason(`BLE observer setup failed: ${errorText(error)}`))
        .catch(() => undefined);
      throw error;
    }
    return link;
  }

  get usable(): boolean {
    return this.#usable;
  }

  async close(reason: string): Promise<void> {
    await this.#terminate(platformReason(reason), false);
  }

  #begin(): void {
    this.#removeObserver = this.connection.observe({
      onBytes: (bytes) => {
        if (!this.#usable) return;
        try {
          this.appliance.bleIngestIndication(this.generation, ownedArrayBuffer(bytes));
        } catch (error) {
          void this.#terminate(
            platformReason(`BLE indication handoff failed: ${errorText(error)}`),
            false,
          ).catch(() => undefined);
        }
      },
      onDisconnect: (event) => {
        void this.#remoteDisconnected(event);
      },
    });
    if (!this.#usable) return;

    void this.#pump().catch((error: unknown) => {
      if (!this.#usable || this.#abort.signal.aborted) return;
      void this.#terminate(
        platformReason(`Native BLE command pump failed: ${errorText(error)}`),
        false,
      ).catch(() => undefined);
    });
  }

  async #pump(): Promise<void> {
    while (this.#usable) {
      let encoded: NativeBlePlatformCommand | undefined;
      try {
        encoded = await this.appliance.bleNextPlatformCommand(this.generation, {
          signal: this.#abort.signal,
        });
      } catch (error) {
        if (!this.#usable || this.#abort.signal.aborted) return;
        throw error;
      }
      if (!this.#usable || encoded === undefined) continue;

      const command = this.decodeCommand(encoded);
      if (command.generation !== this.generation) {
        await this.#terminate(
          `Native BLE command used generation ${command.generation}; active generation is ${this.generation}`,
          false,
        );
        return;
      }

      if (command.kind === "disconnect") {
        await this.#terminate(platformReason(command.reason), false);
        return;
      }

      try {
        // Resolve before Rust's longer ambiguous-write deadline so exactly one
        // success or failure reaches the native bridge even when an OS BLE API
        // never settles its write promise.
        await withWriteTimeout(
          this.connection.write(new Uint8Array(command.bytes)),
          this.writeTimeoutMs,
        );
      } catch (error) {
        if (!this.#usable) return;
        this.appliance.bleWriteFailed(
          this.generation,
          command.token,
          platformReason(`BLE GATT write failed: ${errorText(error)}`),
        );
        continue;
      }
      if (!this.#usable) return;
      this.appliance.bleWriteSucceeded(this.generation, command.token);
    }
  }

  async #remoteDisconnected(event: BleDisconnectEvent): Promise<void> {
    if (!this.#usable) return;
    await this.#terminate(platformReason(event.reason), true).catch(() => undefined);
  }

  #terminate(reason: string, connectionAlreadyGone: boolean): Promise<void> {
    if (this.#ending !== null) return this.#ending;

    this.#usable = false;
    this.#abort.abort();
    this.#removeObserver?.();
    this.#removeObserver = null;
    this.#ending = (async () => {
      let finalReason = reason;
      let teardownError: unknown;
      if (!connectionAlreadyGone) {
        try {
          await this.connection.close();
        } catch (error) {
          teardownError = error;
          finalReason = platformReason(`${reason}; BLE GATT teardown failed: ${errorText(error)}`);
        }
      }
      if (!this.#reportedDisconnected) {
        this.#reportedDisconnected = true;
        this.appliance.bleDisconnected(this.generation, finalReason);
      }
      if (teardownError !== undefined) throw teardownError;
    })();
    const ending = this.#ending;
    void ending.catch(() => {
      if (this.#ending === ending) this.#ending = null;
    });
    return ending;
  }
}

/**
 * Owns the platform GATT link used by one native Rust appliance.
 *
 * Initial connection is intentionally fire-and-forget so opening the durable
 * SQLite client remains offline-first. Explicit reconnect calls await scanning,
 * subscription, native generation registration, and the actor wake-up.
 */
export class NativeBleTransport {
  readonly #appliance: NativeBleAppliance;
  readonly #central: BleCentral;
  readonly #decodeCommand: NativeBleCommandDecoder;
  readonly #profile: BleGattProfile;
  readonly #writeTimeoutMs: number;

  #active: NativeBleLink | null = null;
  #connectionAbort: AbortController | null = null;
  #disposed = false;
  #peripheralName?: string;
  #reconnecting: Promise<void> | null = null;
  #started = false;

  constructor(
    appliance: NativeBleAppliance,
    config: NativeBleTransportConfig,
    writeTimeoutMs = PLATFORM_GATT_WRITE_TIMEOUT_MS,
  ) {
    this.#appliance = appliance;
    this.#central = config.central;
    this.#decodeCommand = config.decodeCommand;
    this.#peripheralName = config.peripheralName;
    this.#profile = config.profile;
    this.#writeTimeoutMs = writeTimeoutMs;
    if (!Number.isFinite(this.#writeTimeoutMs) || this.#writeTimeoutMs <= 0) {
      throw new Error("native BLE platform write timeout must be positive");
    }
  }

  get hasPeripheralName(): boolean {
    return this.#peripheralName !== undefined;
  }

  /**
   * Select the exact advertised device before the first connection attempt.
   *
   * Credential onboarding calls this after Rust has validated the imported
   * credential and derived its expected E290 advertising name. Retargeting a
   * live or connecting transport is deliberately rejected because it could
   * bind authenticated protocol bytes to a different physical peripheral.
   */
  configurePeripheralName(peripheralName: string): void {
    if (this.#disposed) throw new Error("native BLE transport has been disposed");
    if (this.#peripheralName === peripheralName) return;
    if (this.#started || this.#reconnecting !== null || this.#active !== null) {
      throw new Error("native BLE peripheral must be selected before the transport starts");
    }
    this.#peripheralName = peripheralName;
  }

  start(): void {
    if (this.#started || this.#disposed) return;
    this.#started = true;
    void this.#beginReconnect(false).catch(() => undefined);
  }

  async reconnect(): Promise<void> {
    if (this.#disposed) throw new Error("native BLE transport has been disposed");
    return this.#beginReconnect(true);
  }

  async #beginReconnect(replaceExisting: boolean): Promise<void> {
    if (this.#disposed) throw new Error("native BLE transport has been disposed");
    if (this.#reconnecting !== null) return this.#reconnecting;

    const reconnecting = this.#reconnect(replaceExisting);
    this.#reconnecting = reconnecting;
    try {
      await reconnecting;
    } finally {
      if (this.#reconnecting === reconnecting) this.#reconnecting = null;
    }
  }

  async dispose(): Promise<void> {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#connectionAbort?.abort(new Error("native BLE transport was disposed"));

    const active = this.#active;
    this.#active = null;
    const centralDisposal = this.#central.dispose().catch(() => undefined);
    await active?.close("BLE transport owner was disposed").catch(() => undefined);
    await this.#reconnecting?.catch(() => undefined);
    await centralDisposal;
  }

  async #reconnect(replaceExisting: boolean): Promise<void> {
    const abort = new AbortController();
    this.#connectionAbort = abort;
    let link: NativeBleLink | null = null;
    try {
      if (replaceExisting) {
        const previous = this.#active;
        await previous?.close("Replacing BLE link for explicit reconnect");
        if (this.#active === previous) this.#active = null;
        if (this.#disposed) throw new Error("native BLE transport has been disposed");

        // This command drops the actor's current session. It must run after the
        // old generation was reported closed and before a replacement can be
        // claimed; registering first lets the actor race ahead and claim the
        // new generation just before this destructive command releases it.
        await this.#appliance.reconnect({ signal: abort.signal });
        if (this.#disposed) throw new Error("native BLE transport has been disposed");
      } else if (this.#active !== null) {
        return;
      }

      const connection = await this.#central.connect(this.#profile, {
        peripheralName: this.#peripheralName,
        signal: abort.signal,
      });
      if (this.#disposed || abort.signal.aborted) {
        await connection.close().catch(() => undefined);
        throw new Error("native BLE transport was disposed while connecting");
      }
      try {
        link = await NativeBleLink.open(
          this.#appliance,
          connection,
          this.#decodeCommand,
          this.#writeTimeoutMs,
        );
      } catch (error) {
        await connection.close().catch(() => undefined);
        throw error;
      }
      this.#active = link;

      if (!link.usable) throw new Error("BLE peripheral disconnected during link setup");
      await this.#appliance.ensureConnected({ signal: abort.signal });
      if (!link.usable) throw new Error("BLE peripheral disconnected during native reconnect");
    } catch (error) {
      if (link !== null) {
        if (this.#active === link) this.#active = null;
        await link
          .close(platformReason(`Native BLE reconnect failed: ${errorText(error)}`))
          .catch(() => undefined);
      }
      throw error;
    } finally {
      if (this.#connectionAbort === abort) this.#connectionAbort = null;
    }
  }
}
