import type {
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBridgeContract,
  NativeCredentialSummary,
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

export type NativeCredentialState =
  | { readonly state: "missing" }
  | { readonly state: "active"; readonly summary: NativeCredentialSummary }
  | { readonly state: "invalid"; readonly reason: string };

export interface StagedNativeCredential {
  /**
   * Absolute path to an app-owned temporary copy. The selected external file
   * itself is never handed to Rust and its secret bytes never enter JS.
   */
  readonly stagingPath: string;
  cleanup(): void | Promise<void>;
}

export type NativeCredentialPicker = () => Promise<StagedNativeCredential | null>;

export interface NativeApplianceBridge {
  readonly contract: NativeBridgeContract;
  readonly isNativeError: NativeErrorPredicate;
  credentialStatus(appliance: NativeApplianceLike): NativeCredentialState;
  destroy(appliance: NativeApplianceLike): void;
  importCredential(appliance: NativeApplianceLike, stagingPath: string): NativeCredentialSummary;
  open(databasePath: string): NativeApplianceLike;
}

export interface NativeApplianceRuntime {
  readonly ble?: NativeBleTransportConfig;
  readonly bridge: NativeApplianceBridge;
  readonly databasePath: string;
  readonly pickCredential?: NativeCredentialPicker;
}

export type NativeApplianceRuntimeLoader = () => Promise<NativeApplianceRuntime>;

interface OwnedNativeAppliance {
  readonly appliance: NativeApplianceLike;
  readonly ble: NativeBleTransport | null;
  readonly pickCredential?: NativeCredentialPicker;
}

export function bleGattProfileFromNative(profile: NativeBleGattProfile): BleGattProfile {
  return {
    indicateCharacteristicUuid: profile.txUuid,
    maximumWriteValueBytes: profile.initialAttValueBytes,
    serviceUuid: profile.serviceUuid,
    writeCharacteristicUuid: profile.rxUuid,
  };
}

export function normalizeBlePeripheralName(value: string | undefined): string | undefined {
  const normalized = value?.trim();
  return normalized === "" ? undefined : normalized;
}

export function cleanupPickerOwnedCredential(
  platformOs: string,
  picked: { readonly exists: boolean; delete(): void },
): void {
  // Expo's iOS picker uses UIDocumentPicker's asCopy mode, so its result is
  // another app-owned temporary secret copy. Android returns a content://
  // provider handle instead; deleting that could delete the user's source.
  if (platformOs === "ios" && picked.exists) picked.delete();
}

function parseNativeJson<T>(label: string, value: string): T {
  try {
    return JSON.parse(value) as T;
  } catch (error) {
    throw new Error(`native Rust bridge returned invalid ${label} JSON`, { cause: error });
  }
}

function isCredentialPublicationUncertain(
  error: unknown,
  isNativeError: NativeErrorPredicate,
): boolean {
  return isNativeError(error) && error.tag === "CredentialPublicationUncertain";
}

function unsupportedOnboardingRecovery(): never {
  throw new Error(
    "Native credential import has no resumable recovery action; full in-app BLE pairing remains future work.",
  );
}

function nativeOnboardingView(
  status: NativeCredentialState,
  bleTargetAvailable: boolean,
  usesBle: boolean,
): OnboardingView {
  if (status.state === "missing") {
    return {
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "",
        lifecycle: { state: "needs_pairing" },
      },
    };
  }
  if (status.state === "invalid") {
    return {
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "",
        lifecycle: { state: "faulted", reason: "invalid_credential_artifact" },
      },
    };
  }
  if (
    status.state === "active" &&
    usesBle &&
    !bleTargetAvailable &&
    status.summary.expectedBleLocalName === undefined
  ) {
    return {
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "",
        lifecycle: { state: "faulted", reason: "unsupported_device" },
      },
    };
  }
  return {
    available: true,
    method: "credential_import",
    snapshot: {
      // The native generation is a u64 and therefore is not necessarily a
      // lossless JavaScript number. This projection only needs a stable local
      // revision; the generated native summary retains the exact bigint.
      revision: 0,
      // The shared HTTP DTO still calls this USB-specific field `usb_serial`.
      // Do not overload it with a BLE name; ready connection metadata carries
      // the neutral endpoint and device label.
      usb_serial: "",
      lifecycle: { state: "credential_ready" },
    },
  };
}

async function loadNativeApplianceRuntime(): Promise<NativeApplianceRuntime> {
  const [bindings, crypto, fileSystem, reactNative] = await Promise.all([
    import("@reticulum/appliance-native"),
    import("expo-crypto"),
    import("expo-file-system"),
    import("react-native"),
  ]);
  const databaseUri = new fileSystem.File(fileSystem.Paths.document, DATABASE_FILE_NAME).uri;
  const deviceCredentialUri = new fileSystem.File(
    fileSystem.Paths.document,
    DEVICE_CREDENTIAL_FILE_NAME,
  ).uri;
  const wifiEndpoint = process.env.EXPO_PUBLIC_APPLIANCE_WIFI_ENDPOINT?.trim() ?? "";
  const blePeripheralName = normalizeBlePeripheralName(process.env.EXPO_PUBLIC_APPLIANCE_BLE_NAME);
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
            peripheralName: blePeripheralName,
            profile: bleGattProfileFromNative(bindings.nativeBleGattProfile()),
          },
    bridge: {
      contract: bindings.nativeBridgeContract(),
      isNativeError: bindings.NativeApplianceError.instanceOf,
      credentialStatus(appliance): NativeCredentialState {
        const status = appliance.credentialStatus();
        if (bindings.NativeCredentialStatus.Missing.instanceOf(status)) {
          return { state: "missing" };
        }
        if (bindings.NativeCredentialStatus.Active.instanceOf(status)) {
          return { state: "active", summary: status.inner.summary };
        }
        if (bindings.NativeCredentialStatus.Invalid.instanceOf(status)) {
          return { state: "invalid", reason: status.inner.reason };
        }
        throw new Error("native Rust bridge returned an unknown credential status");
      },
      destroy(appliance): void {
        if (bindings.NativeAppliance.instanceOf(appliance)) appliance.uniffiDestroy();
      },
      importCredential(appliance, stagingPath): NativeCredentialSummary {
        return appliance.importActivatedCredential(stagingPath);
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
    async pickCredential(): Promise<StagedNativeCredential | null> {
      const picked = await fileSystem.File.pickFileAsync({
        mimeTypes: ["application/octet-stream"],
        multipleFiles: false,
      });
      if (picked.canceled) return null;

      // Expo performs the copy natively. TypeScript receives only file handles,
      // never the activated credential bytes.
      const staging = new fileSystem.File(
        fileSystem.Paths.cache,
        `.reticulum-credential-import-${crypto.randomUUID()}.rdpkey`,
      );
      const cleanup = (): void => {
        if (staging.exists) staging.delete();
      };
      try {
        await picked.result.copy(staging, { overwrite: false });
        const stagingPath = nativePathFromFileUri(staging.uri);
        cleanupPickerOwnedCredential(reactNative.Platform.OS, picked.result);
        return { stagingPath, cleanup };
      } catch (error) {
        const cleanupErrors: unknown[] = [];
        try {
          cleanup();
        } catch (cleanupError) {
          cleanupErrors.push(cleanupError);
        }
        try {
          cleanupPickerOwnedCredential(reactNative.Platform.OS, picked.result);
        } catch (cleanupError) {
          cleanupErrors.push(cleanupError);
        }
        if (cleanupErrors.length > 0) {
          throw new AggregateError([error, ...cleanupErrors], "Credential staging cleanup failed.");
        }
        throw error;
      }
    },
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
    const { ble } = this.#active();
    return nativeOnboardingView(
      this.#credentialState(),
      ble?.hasPeripheralName ?? false,
      ble !== null,
    );
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
    const { appliance, ble, bridge, pickCredential } = this.#active();
    const current = this.#credentialState();
    if (current.state === "active") {
      if (
        ble !== null &&
        current.summary.expectedBleLocalName === undefined &&
        !ble.hasPeripheralName
      ) {
        throw new Error(
          "The active credential does not identify an exact BLE advertising name; refusing an untargeted scan.",
        );
      }
      return undefined;
    }
    if (current.state === "invalid") {
      throw new Error(
        "The app-private credential is invalid and cannot be replaced in place; remove this app's local data before importing a new credential.",
      );
    }
    if (pickCredential === undefined) {
      throw new Error("Credential file selection is unavailable in this native build.");
    }

    const staged = await pickCredential();
    if (staged === null) return undefined;

    let summary: NativeCredentialSummary | null = null;
    let importFailure: unknown;
    try {
      summary = bridge.importCredential(appliance, staged.stagingPath);
    } catch (error) {
      if (!isCredentialPublicationUncertain(error, bridge.isNativeError)) {
        importFailure = normalizeNativeError(error, bridge.isNativeError);
      } else {
        // Publication is atomic, but removing its temporary link or syncing
        // the directory can fail after the destination became visible. Rust
        // exposes only that post-publication phase as reconcilable. Validation
        // and exact-readback failures remain ordinary Storage errors and must
        // never be converted into success merely because the changed bytes
        // still decode as an Active credential.
        let reconciled: NativeCredentialState | null = null;
        try {
          reconciled = bridge.credentialStatus(appliance);
        } catch {
          // Preserve the more specific publication failure below.
        }
        if (reconciled?.state !== "active") {
          importFailure = normalizeNativeError(error, bridge.isNativeError);
        } else {
          summary = reconciled.summary;
        }
      }
    }

    let cleanupFailure: unknown;
    try {
      await staged.cleanup();
    } catch (error) {
      cleanupFailure = error;
    }

    if (summary === null) {
      if (importFailure !== undefined && cleanupFailure !== undefined) {
        throw new AggregateError(
          [importFailure, cleanupFailure],
          "Credential import failed and its app-private staging copy could not be removed.",
        );
      }
      if (importFailure !== undefined) throw importFailure;
      if (cleanupFailure !== undefined) throw cleanupFailure;
      throw new Error("Credential import did not produce an activated credential.");
    }

    if (ble !== null) {
      if (summary.expectedBleLocalName !== undefined) {
        ble.configurePeripheralName(summary.expectedBleLocalName);
      }
      if (!ble.hasPeripheralName) {
        throw new Error(
          "The imported credential does not identify an exact BLE advertising name; refusing an untargeted scan.",
        );
      }
      ble.start();
    } else {
      // A missing credential can leave the native Wi-Fi connector in a
      // permanent fault. Explicit reconnect clears that terminal attempt now
      // that authenticated material is available.
      await this.#call((activeAppliance) => activeAppliance.reconnect());
    }
    if (cleanupFailure !== undefined) {
      throw new Error(
        "The credential was installed, but its app-private staging copy could not be removed.",
        { cause: cleanupFailure },
      );
    }
    return undefined;
  }

  async refreshOnboarding(): Promise<NoContent> {
    this.#credentialState();
    return undefined;
  }

  async recoverOnboarding(_request: RecoveryRequest): Promise<NoContent> {
    unsupportedOnboardingRecovery();
  }

  async sync(): Promise<NoContent> {
    await this.#call((appliance) => appliance.syncNow());
    return undefined;
  }

  async reconnect(): Promise<NoContent> {
    const { appliance, ble, bridge } = this.#active();
    const credential = this.#credentialState();
    if (credential.state !== "active") {
      throw new Error("Import an activated device credential before connecting.");
    }
    if (
      ble !== null &&
      credential.summary.expectedBleLocalName === undefined &&
      !ble.hasPeripheralName
    ) {
      throw new Error(
        "The active credential does not identify an exact BLE advertising name; refusing an untargeted scan.",
      );
    }
    try {
      if (ble === null) {
        await appliance.reconnect();
      } else {
        if (credential.summary.expectedBleLocalName !== undefined) {
          ble.configurePeripheralName(credential.summary.expectedBleLocalName);
        }
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
        async () => {
          if (this.#disposed) throw new Error("native appliance client has been disposed");
          const appliance = runtime.bridge.open(runtime.databasePath);
          try {
            const credential = runtime.bridge.credentialStatus(appliance);
            const ble =
              runtime.ble === undefined ? null : new NativeBleTransport(appliance, runtime.ble);
            if (ble !== null && credential.state === "active") {
              if (credential.summary.expectedBleLocalName !== undefined) {
                ble.configurePeripheralName(credential.summary.expectedBleLocalName);
              }
              if (ble.hasPeripheralName) ble.start();
            }
            return { appliance, ble, pickCredential: runtime.pickCredential };
          } catch (error) {
            await appliance.close().catch(() => undefined);
            runtime.bridge.destroy(appliance);
            throw error;
          }
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
    readonly pickCredential?: NativeCredentialPicker;
  } {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#ownership === null || this.#bridge === null) {
      throw new Error("native appliance client has not been bootstrapped");
    }
    return {
      appliance: this.#ownership.value.appliance,
      ble: this.#ownership.value.ble,
      bridge: this.#bridge,
      pickCredential: this.#ownership.value.pickCredential,
    };
  }

  #credentialState(): NativeCredentialState {
    const { appliance, bridge } = this.#active();
    try {
      return bridge.credentialStatus(appliance);
    } catch (error) {
      throw normalizeNativeError(error, bridge.isNativeError);
    }
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
