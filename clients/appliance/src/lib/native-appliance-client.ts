import type { NativeApplianceLike, NativeBridgeContract } from "@reticulum/appliance-native";

import type {
  ApplianceSnapshot,
  ContactRequest,
  ContactView,
  MutationResponse,
  NoContent,
  OnboardingView,
  RecoveryRequest,
  SendRequest,
  SendResponse,
  TimelineView,
} from "../generated/api.ts";
import type { ApplianceClient } from "./appliance-client.ts";
import { acquireExclusiveResource, type ExclusiveResource } from "./exclusive-resource.ts";
import { nativePathFromFileUri } from "./file-uri.ts";
import { assertNativeBridgeContract } from "./native-contract.ts";
import { type NativeErrorPredicate, normalizeNativeError } from "./native-error.ts";

const DATABASE_FILE_NAME = "reticulum-lxmf-chat.sqlite3";
const NATIVE_ONBOARDING: OnboardingView = { available: false, snapshot: null };

export interface NativeApplianceBridge {
  readonly contract: NativeBridgeContract;
  readonly isNativeError: NativeErrorPredicate;
  destroy(appliance: NativeApplianceLike): void;
  open(databasePath: string): NativeApplianceLike;
}

export interface NativeApplianceRuntime {
  readonly bridge: NativeApplianceBridge;
  readonly databasePath: string;
}

export type NativeApplianceRuntimeLoader = () => Promise<NativeApplianceRuntime>;

function parseNativeJson<T>(label: string, value: string): T {
  try {
    return JSON.parse(value) as T;
  } catch (error) {
    throw new Error(`native Rust bridge returned invalid ${label} JSON`, { cause: error });
  }
}

function unsupportedOnboarding(): never {
  throw new Error(
    "Pairing is not available through the native bridge yet; BLE, Wi-Fi, and USB remain explicit transport stubs.",
  );
}

async function loadNativeApplianceRuntime(): Promise<NativeApplianceRuntime> {
  const [bindings, fileSystem] = await Promise.all([
    import("@reticulum/appliance-native"),
    import("expo-file-system"),
  ]);
  const databaseUri = new fileSystem.File(fileSystem.Paths.document, DATABASE_FILE_NAME).uri;
  return {
    bridge: {
      contract: bindings.nativeBridgeContract(),
      isNativeError: bindings.NativeApplianceError.instanceOf,
      destroy(appliance): void {
        if (bindings.NativeAppliance.instanceOf(appliance)) appliance.uniffiDestroy();
      },
      open(databasePath): NativeApplianceLike {
        return bindings.NativeAppliance.open(
          databasePath,
          bindings.NativeTransport.BluetoothLowEnergy,
        );
      },
    },
    databasePath: nativePathFromFileUri(databaseUri),
  };
}

/**
 * Offline-first native adapter backed by the Rust single-owner actor and an
 * app-private SQLite database.
 *
 * BLE is selected as the intended mobile bearer, but it remains an explicit
 * unavailable connector until the platform GATT implementation lands. Local
 * contacts, timelines, and durable outbox writes work in the meantime.
 */
export class NativeApplianceClient implements ApplianceClient {
  readonly #loadRuntime: NativeApplianceRuntimeLoader;
  #bridge: NativeApplianceBridge | null = null;
  #opening: Promise<void> | null = null;
  #ownership: ExclusiveResource<NativeApplianceLike> | null = null;
  #disposed = false;

  constructor(loadRuntime: NativeApplianceRuntimeLoader = loadNativeApplianceRuntime) {
    this.#loadRuntime = loadRuntime;
  }

  async bootstrapSession(): Promise<void> {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#ownership !== null) return;
    if (this.#opening !== null) return this.#opening;

    const opening = this.#open();
    this.#opening = opening;
    try {
      await opening;
    } finally {
      if (this.#opening === opening) this.#opening = null;
    }
  }

  async snapshot(): Promise<ApplianceSnapshot> {
    return parseNativeJson("snapshot", await this.#call((appliance) => appliance.snapshotJson()));
  }

  async onboarding(): Promise<OnboardingView> {
    return NATIVE_ONBOARDING;
  }

  async contacts(): Promise<ContactView[]> {
    return parseNativeJson("contacts", await this.#call((appliance) => appliance.contactsJson()));
  }

  async timeline(destination: string): Promise<TimelineView[]> {
    return parseNativeJson(
      "timeline",
      await this.#call((appliance) => appliance.timelineJson(destination)),
    );
  }

  async upsertContact(destination: string, request: ContactRequest): Promise<MutationResponse> {
    return parseNativeJson(
      "contact mutation",
      await this.#call((appliance) =>
        appliance.upsertContactJson(destination, JSON.stringify(request)),
      ),
    );
  }

  async send(request: SendRequest): Promise<SendResponse> {
    return parseNativeJson(
      "send response",
      await this.#call((appliance) => appliance.sendMessageJson(JSON.stringify(request))),
    );
  }

  async startOnboarding(): Promise<NoContent> {
    unsupportedOnboarding();
  }

  async refreshOnboarding(): Promise<NoContent> {
    unsupportedOnboarding();
  }

  async recoverOnboarding(_request: RecoveryRequest): Promise<NoContent> {
    unsupportedOnboarding();
  }

  async sync(): Promise<NoContent> {
    await this.#call((appliance) => appliance.syncNow());
    return undefined;
  }

  async reconnect(): Promise<NoContent> {
    await this.#call((appliance) => appliance.reconnect());
    return undefined;
  }

  subscribeInvalidations(_onInvalidate: () => void, _onError: () => void): (() => void) | null {
    return null;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    const ownership = this.#ownership;
    this.#ownership = null;
    this.#bridge = null;
    if (ownership !== null) void ownership.release().catch(() => undefined);
  }

  async #open(): Promise<void> {
    const runtime = await this.#loadRuntime();
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    assertNativeBridgeContract(runtime.bridge.contract);

    let ownership: ExclusiveResource<NativeApplianceLike>;
    try {
      ownership = await acquireExclusiveResource(
        () => {
          if (this.#disposed) throw new Error("native appliance client has been disposed");
          return runtime.bridge.open(runtime.databasePath);
        },
        async (appliance) => {
          try {
            await appliance.close();
          } finally {
            runtime.bridge.destroy(appliance);
          }
        },
      );
    } catch (error) {
      throw normalizeNativeError(error, runtime.bridge.isNativeError);
    }

    if (this.#disposed) {
      await ownership.release().catch(() => undefined);
      throw new Error("native appliance client has been disposed");
    }
    this.#bridge = runtime.bridge;
    this.#ownership = ownership;
  }

  #active(): {
    readonly appliance: NativeApplianceLike;
    readonly bridge: NativeApplianceBridge;
  } {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#ownership === null || this.#bridge === null) {
      throw new Error("native appliance client has not been bootstrapped");
    }
    return { appliance: this.#ownership.value, bridge: this.#bridge };
  }

  async #call<T>(operation: (appliance: NativeApplianceLike) => T | Promise<T>): Promise<T> {
    const { appliance, bridge } = this.#active();
    try {
      return await operation(appliance);
    } catch (error) {
      throw normalizeNativeError(error, bridge.isNativeError);
    }
  }
}
