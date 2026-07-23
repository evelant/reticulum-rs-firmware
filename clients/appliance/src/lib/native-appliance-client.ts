import type {
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBridgeContract,
} from "@reticulum/appliance-native";

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
import type { BleGattProfile } from "./ble-central-types.ts";
import { acquireExclusiveResource, type ExclusiveResource } from "./exclusive-resource.ts";
import { nativePathFromFileUri } from "./file-uri.ts";
import { NativeBleTransport, type NativeBleTransportConfig } from "./native-ble-transport.ts";
import { assertNativeBridgeContract } from "./native-contract.ts";
import { type NativeErrorPredicate, normalizeNativeError } from "./native-error.ts";

const DATABASE_FILE_NAME = "reticulum-lxmf-chat.sqlite3";
const DEVICE_CREDENTIAL_FILE_NAME = "reticulum-device-credential.rdpkey";
const NATIVE_ONBOARDING: OnboardingView = { available: false, snapshot: null };

export interface NativeApplianceBridge {
  readonly contract: NativeBridgeContract;
  readonly isNativeError: NativeErrorPredicate;
  destroy(appliance: NativeApplianceLike): void;
  open(databasePath: string): NativeApplianceLike;
}

export interface NativeApplianceRuntime {
  readonly ble?: NativeBleTransportConfig;
  readonly bridge: NativeApplianceBridge;
  readonly databasePath: string;
}

export type NativeApplianceRuntimeLoader = () => Promise<NativeApplianceRuntime>;

interface OwnedNativeAppliance {
  readonly appliance: NativeApplianceLike;
  readonly ble: NativeBleTransport | null;
}

export function bleGattProfileFromNative(profile: NativeBleGattProfile): BleGattProfile {
  return {
    indicateCharacteristicUuid: profile.txUuid,
    maximumWriteValueBytes: profile.initialAttValueBytes,
    serviceUuid: profile.serviceUuid,
    writeCharacteristicUuid: profile.rxUuid,
  };
}

function parseNativeJson<T>(label: string, value: string): T {
  try {
    return JSON.parse(value) as T;
  } catch (error) {
    throw new Error(`native Rust bridge returned invalid ${label} JSON`, { cause: error });
  }
}

function unsupportedOnboarding(): never {
  throw new Error(
    "Pairing is not available through the native bridge yet; seed an activated credential over the qualified USB workflow before using a BLE or Wi-Fi connector.",
  );
}

async function loadNativeApplianceRuntime(): Promise<NativeApplianceRuntime> {
  const [bindings, fileSystem] = await Promise.all([
    import("@reticulum/appliance-native"),
    import("expo-file-system"),
  ]);
  const databaseUri = new fileSystem.File(fileSystem.Paths.document, DATABASE_FILE_NAME).uri;
  const deviceCredentialUri = new fileSystem.File(
    fileSystem.Paths.document,
    DEVICE_CREDENTIAL_FILE_NAME,
  ).uri;
  const wifiEndpoint = process.env.EXPO_PUBLIC_APPLIANCE_WIFI_ENDPOINT?.trim() ?? "";
  const bleCentral = wifiEndpoint === "" ? await import("./ble-central") : null;
  return {
    ble:
      bleCentral === null
        ? undefined
        : {
            central: bleCentral.createBleCentral(),
            decodeCommand(command) {
              if (bindings.NativeBlePlatformCommand.Write.instanceOf(command)) {
                return { kind: "write", ...command.inner };
              }
              if (bindings.NativeBlePlatformCommand.Disconnect.instanceOf(command)) {
                return { kind: "disconnect", ...command.inner };
              }
              throw new Error("native Rust bridge returned an unknown BLE platform command");
            },
            profile: bleGattProfileFromNative(bindings.nativeBleGattProfile()),
          },
    bridge: {
      contract: bindings.nativeBridgeContract(),
      isNativeError: bindings.NativeApplianceError.instanceOf,
      destroy(appliance): void {
        if (bindings.NativeAppliance.instanceOf(appliance)) appliance.uniffiDestroy();
      },
      open(databasePath): NativeApplianceLike {
        if (wifiEndpoint !== "") {
          return bindings.NativeAppliance.openWifi(
            databasePath,
            wifiEndpoint,
            nativePathFromFileUri(deviceCredentialUri),
          );
        }
        return bindings.NativeAppliance.openBle(
          databasePath,
          nativePathFromFileUri(deviceCredentialUri),
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
 * `EXPO_PUBLIC_APPLIANCE_WIFI_ENDPOINT` opts a native build into the raw-TCP
 * Wi-Fi proof connector and its app-private activated credential. Without that
 * build-time endpoint, the platform foreground BLE central owns GATT while the
 * Rust bridge owns authentication and protocol bytes. Local contacts,
 * timelines, and durable outbox writes work immediately in both modes, even
 * while the initial BLE scan runs in the background.
 */
export class NativeApplianceClient implements ApplianceClient {
  readonly #loadRuntime: NativeApplianceRuntimeLoader;
  #bridge: NativeApplianceBridge | null = null;
  #opening: Promise<void> | null = null;
  #ownership: ExclusiveResource<OwnedNativeAppliance> | null = null;
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
    const { appliance, ble, bridge } = this.#active();
    try {
      if (ble === null) {
        await appliance.reconnect();
      } else {
        await ble.reconnect();
      }
    } catch (error) {
      throw normalizeNativeError(error, bridge.isNativeError);
    }
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

    let ownership: ExclusiveResource<OwnedNativeAppliance>;
    try {
      ownership = await acquireExclusiveResource(
        () => {
          if (this.#disposed) throw new Error("native appliance client has been disposed");
          const appliance = runtime.bridge.open(runtime.databasePath);
          const ble =
            runtime.ble === undefined ? null : new NativeBleTransport(appliance, runtime.ble);
          ble?.start();
          return { appliance, ble };
        },
        async ({ appliance, ble }) => {
          try {
            await ble?.dispose();
          } finally {
            try {
              await appliance.close();
            } finally {
              runtime.bridge.destroy(appliance);
            }
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
    readonly ble: NativeBleTransport | null;
    readonly bridge: NativeApplianceBridge;
  } {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#ownership === null || this.#bridge === null) {
      throw new Error("native appliance client has not been bootstrapped");
    }
    return {
      appliance: this.#ownership.value.appliance,
      ble: this.#ownership.value.ble,
      bridge: this.#bridge,
    };
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
