import type {
  NativeApplianceLike,
  NativeBleGattProfile,
  NativeBleOnboardingLike,
  NativeBridgeContract,
  NativeCredentialSummary,
  NativeProfileStoreLike,
  NativeProfileSummary,
} from "@reticulum/appliance-native";

import type {
  ApplianceSnapshot,
  ContactRequest,
  ContactView,
  MutationResponse,
  NearbyPeerView,
  NoContent,
  NomadFetchPollRequest,
  NomadFetchPollResponse,
  NomadFetchStartRequest,
  NomadFetchStartResponse,
  OnboardingView,
  RecoveryRequest,
  SendRequest,
  SendResponse,
  TimelineView,
} from "../generated/api.ts";
import type { ApplianceClient } from "./appliance-client.ts";
import type { BleCandidate, BleGattProfile, BleScanOptions } from "./ble-central-types.ts";
import { acquireExclusiveResource, type ExclusiveResource } from "./exclusive-resource.ts";
import { nativePathFromFileUri } from "./file-uri.ts";
import {
  NativeBleOnboardingTransport,
  NativeBleTransport,
  type NativeBleTransportConfig,
} from "./native-ble-transport.ts";
import { assertNativeBridgeContract } from "./native-contract.ts";
import { type NativeErrorPredicate, normalizeNativeError } from "./native-error.ts";
import { nativePlatformOs } from "./native-platform";

// Previous single-profile artifacts are supplied only to Rust's one-time
// migration. New credentials and identity-bound databases live below the
// native-owned device-keyed profile root.
const DATABASE_FILE_NAME = "reticulum-lxmf-chat-alpha-schema3.sqlite3";
const DEVICE_CREDENTIAL_FILE_NAME = "reticulum-device-credential.rdpkey";
const PROFILE_STORE_DIRECTORY_NAME = "reticulum-appliance-profiles";

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
  destroyProfileStore(profileStore: NativeProfileStoreLike): void;
  importCredential(appliance: NativeApplianceLike, stagingPath: string): NativeCredentialSummary;
  open(profileStore: NativeProfileStoreLike): NativeApplianceLike;
}

export type NativeBleOnboardingPhase =
  | "idle"
  | "link_ready"
  | "preparing"
  | "checking_initialization"
  | "initializing"
  | "initialized"
  | "waiting_for_initialization_presence"
  | "waiting_for_begin_presence"
  | "pending_persisted"
  | "waiting_for_proof_presence"
  | "proof_challenge_accepted"
  | "activation_prepared"
  | "publishing_profile"
  | "complete"
  | "waiting_for_abort_presence"
  | "finalizing_abort"
  | "aborted"
  | "failed";

export type NativeBleOnboardingFailure =
  | "busy"
  | "no_subscribed_link"
  | "initialization_incomplete"
  | "resume_required"
  | "abort_required"
  | "reconciliation_required"
  | "invalid_recovery_state"
  | "protocol_failure"
  | "profile_publication_failure"
  | "internal";

export interface NativeBleOnboardingProjection {
  readonly completedProfile?: NativeProfileSummary;
  readonly failure?: NativeBleOnboardingFailure;
  readonly phase: NativeBleOnboardingPhase;
  readonly revision: bigint;
}

export interface NativeBleOnboardingBridge {
  destroy(onboarding: NativeBleOnboardingLike): void;
  open(profileStore: NativeProfileStoreLike): NativeBleOnboardingLike;
  snapshot(onboarding: NativeBleOnboardingLike): NativeBleOnboardingProjection;
}

export interface NativeApplianceRuntime {
  readonly bleOnboarding?: NativeBleOnboardingBridge;
  readonly bridge: NativeApplianceBridge;
  readonly createBle?: () => NativeBleTransportConfig;
  readonly pickCredential?: NativeCredentialPicker;
  readonly profileStore: NativeProfileStoreLike;
}

export type NativeApplianceRuntimeLoader = () => Promise<NativeApplianceRuntime>;

interface OwnedNativeAppliance {
  readonly appliance: NativeApplianceLike;
  readonly ble: NativeBleTransport | null;
  readonly pickCredential?: NativeCredentialPicker;
}

interface ActiveNativeBleOnboarding {
  readonly action: NativeBleOnboardingAction;
  readonly bridge: NativeBleOnboardingBridge;
  cancelRequested: boolean;
  linkReady: boolean;
  readonly native: NativeBleOnboardingLike;
  operationStarted: boolean;
  readonly peripheralId: string;
  released: boolean;
  readonly transport: NativeBleOnboardingTransport;
}

type NativeBleOnboardingAction = "pair" | "resume" | "abort";

export function bleGattProfileFromNative(profile: NativeBleGattProfile): BleGattProfile {
  return {
    indicateCharacteristicUuid: profile.txUuid,
    maximumWriteValueBytes: profile.initialAttValueBytes,
    securityConfirmationCharacteristicUuid: profile.securityConfirmationUuid,
    securityConfirmationReadyValue: new Uint8Array(profile.securityConfirmationReadyValue),
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
    "Resumable pairing recovery is unavailable without the native BLE onboarding owner.",
  );
}

function surfacedBleOnboardingError(error: unknown, platformReason: string | null): unknown {
  if (platformReason === null) return error;
  return new Error(`Secure BLE transport failed: ${platformReason}`, { cause: error });
}

function nativeOnboardingView(
  status: NativeCredentialState,
  bleTargetAvailable: boolean,
  usesBle: boolean,
  filelessBleAvailable = false,
): OnboardingView {
  if (status.state === "missing") {
    return {
      available: true,
      method: filelessBleAvailable ? "managed_pairing" : "credential_import",
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

function boundedOnboardingRevision(revision: bigint): number {
  return Number(revision > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : revision);
}

function failedBleOnboardingView(
  peripheralId: string,
  revision: bigint,
  failure: NativeBleOnboardingFailure | undefined,
): OnboardingView {
  const lifecycle =
    failure === "resume_required"
      ? ({ state: "resume_available" } as const)
      : failure === "abort_required"
        ? ({ state: "abort_required" } as const)
        : failure === "reconciliation_required"
          ? ({ state: "activation_ambiguous" } as const)
          : ({
              state: "faulted",
              reason:
                failure === "invalid_recovery_state"
                  ? ("invalid_credential_artifact" as const)
                  : failure === "no_subscribed_link"
                    ? ("device_unavailable" as const)
                    : ("protocol_or_persistence_failure" as const),
            } as const);
  return {
    available: true,
    method: "managed_pairing",
    snapshot: {
      lifecycle,
      revision: boundedOnboardingRevision(revision),
      usb_serial: peripheralId,
    },
  };
}

function nativeBleOnboardingView(
  projection: NativeBleOnboardingProjection,
  peripheralId: string,
): OnboardingView {
  if (projection.phase === "failed") {
    return failedBleOnboardingView(peripheralId, projection.revision, projection.failure);
  }

  const lifecycle = (() => {
    switch (projection.phase) {
      case "idle":
      case "aborted":
        return { state: "needs_pairing" } as const;
      case "link_ready":
        return { state: "working", stage: "waiting_for_ble_security" } as const;
      case "preparing":
      case "checking_initialization":
        return { state: "working", stage: "checking_initialization" } as const;
      case "initializing":
      case "initialized":
        return { state: "working", stage: "initializing" } as const;
      case "waiting_for_initialization_presence":
        return { state: "working", stage: "waiting_for_initialization_presence" } as const;
      case "waiting_for_begin_presence":
      case "waiting_for_proof_presence":
        return { state: "working", stage: "waiting_for_pairing_presence" } as const;
      case "pending_persisted":
      case "proof_challenge_accepted":
        return { state: "working", stage: "proving" } as const;
      case "activation_prepared":
      case "publishing_profile":
        return { state: "working", stage: "activating" } as const;
      case "complete":
        return { state: "credential_ready" } as const;
      case "waiting_for_abort_presence":
        return { state: "working", stage: "waiting_for_abort_presence" } as const;
      case "finalizing_abort":
        return { state: "working", stage: "resuming" } as const;
    }
  })();

  return {
    available: true,
    method: "managed_pairing",
    snapshot: {
      lifecycle,
      revision: boundedOnboardingRevision(projection.revision),
      usb_serial: peripheralId,
    },
  };
}

function connectingBleOnboardingView(peripheralId: string): OnboardingView {
  return {
    available: true,
    method: "managed_pairing",
    snapshot: {
      lifecycle: { state: "working", stage: "opening_device" },
      revision: 0,
      usb_serial: peripheralId,
    },
  };
}

async function loadNativeApplianceRuntime(): Promise<NativeApplianceRuntime> {
  const [bindings, crypto, fileSystem] = await Promise.all([
    import("@reticulum/appliance-native"),
    import("expo-crypto"),
    import("expo-file-system"),
  ]);
  const profileStoreUri = new fileSystem.Directory(
    fileSystem.Paths.document,
    PROFILE_STORE_DIRECTORY_NAME,
  ).uri;
  const databaseUri = new fileSystem.File(fileSystem.Paths.document, DATABASE_FILE_NAME).uri;
  const deviceCredentialUri = new fileSystem.File(
    fileSystem.Paths.document,
    DEVICE_CREDENTIAL_FILE_NAME,
  ).uri;
  const wifiEndpoint = process.env.EXPO_PUBLIC_APPLIANCE_WIFI_ENDPOINT?.trim() ?? "";
  const blePeripheralName = normalizeBlePeripheralName(process.env.EXPO_PUBLIC_APPLIANCE_BLE_NAME);
  const bleCentral = wifiEndpoint === "" ? await import("./ble-central") : null;
  const profileStore = bindings.NativeProfileStore.open(
    nativePathFromFileUri(profileStoreUri),
    nativePathFromFileUri(databaseUri),
    nativePathFromFileUri(deviceCredentialUri),
  );
  const onboardingPhase = (phase: number): NativeBleOnboardingPhase => {
    switch (phase) {
      case bindings.NativeBleOnboardingPhase.Idle:
        return "idle";
      case bindings.NativeBleOnboardingPhase.LinkReady:
        return "link_ready";
      case bindings.NativeBleOnboardingPhase.Preparing:
        return "preparing";
      case bindings.NativeBleOnboardingPhase.CheckingInitialization:
        return "checking_initialization";
      case bindings.NativeBleOnboardingPhase.Initializing:
        return "initializing";
      case bindings.NativeBleOnboardingPhase.Initialized:
        return "initialized";
      case bindings.NativeBleOnboardingPhase.WaitingForInitializationPresence:
        return "waiting_for_initialization_presence";
      case bindings.NativeBleOnboardingPhase.WaitingForBeginPresence:
        return "waiting_for_begin_presence";
      case bindings.NativeBleOnboardingPhase.PendingPersisted:
        return "pending_persisted";
      case bindings.NativeBleOnboardingPhase.WaitingForProofPresence:
        return "waiting_for_proof_presence";
      case bindings.NativeBleOnboardingPhase.ProofChallengeAccepted:
        return "proof_challenge_accepted";
      case bindings.NativeBleOnboardingPhase.ActivationPrepared:
        return "activation_prepared";
      case bindings.NativeBleOnboardingPhase.PublishingProfile:
        return "publishing_profile";
      case bindings.NativeBleOnboardingPhase.Complete:
        return "complete";
      case bindings.NativeBleOnboardingPhase.WaitingForAbortPresence:
        return "waiting_for_abort_presence";
      case bindings.NativeBleOnboardingPhase.FinalizingAbort:
        return "finalizing_abort";
      case bindings.NativeBleOnboardingPhase.Aborted:
        return "aborted";
      case bindings.NativeBleOnboardingPhase.Failed:
        return "failed";
      default:
        throw new Error("native Rust bridge returned an unknown BLE onboarding phase");
    }
  };
  const onboardingFailure = (failure: number): NativeBleOnboardingFailure => {
    switch (failure) {
      case bindings.NativeBleOnboardingFailure.Busy:
        return "busy";
      case bindings.NativeBleOnboardingFailure.NoSubscribedLink:
        return "no_subscribed_link";
      case bindings.NativeBleOnboardingFailure.InitializationIncomplete:
        return "initialization_incomplete";
      case bindings.NativeBleOnboardingFailure.ResumeRequired:
        return "resume_required";
      case bindings.NativeBleOnboardingFailure.AbortRequired:
        return "abort_required";
      case bindings.NativeBleOnboardingFailure.ReconciliationRequired:
        return "reconciliation_required";
      case bindings.NativeBleOnboardingFailure.InvalidRecoveryState:
        return "invalid_recovery_state";
      case bindings.NativeBleOnboardingFailure.ProtocolFailure:
        return "protocol_failure";
      case bindings.NativeBleOnboardingFailure.ProfilePublicationFailure:
        return "profile_publication_failure";
      case bindings.NativeBleOnboardingFailure.Internal:
        return "internal";
      default:
        throw new Error("native Rust bridge returned an unknown BLE onboarding failure");
    }
  };
  return {
    bleOnboarding:
      bleCentral === null
        ? undefined
        : {
            destroy(onboarding): void {
              if (bindings.NativeBleOnboarding.instanceOf(onboarding)) {
                onboarding.uniffiDestroy();
              }
            },
            open(store): NativeBleOnboardingLike {
              return bindings.NativeBleOnboarding.open(store);
            },
            snapshot(onboarding): NativeBleOnboardingProjection {
              const snapshot = onboarding.snapshot();
              return {
                completedProfile: snapshot.completedProfile,
                failure:
                  snapshot.failure === undefined ? undefined : onboardingFailure(snapshot.failure),
                phase: onboardingPhase(snapshot.phase),
                revision: snapshot.revision,
              };
            },
          },
    createBle:
      bleCentral === null
        ? undefined
        : () => ({
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
          }),
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
      destroyProfileStore(store): void {
        if (bindings.NativeProfileStore.instanceOf(store)) store.uniffiDestroy();
      },
      importCredential(appliance, stagingPath): NativeCredentialSummary {
        return appliance.importActivatedCredential(stagingPath);
      },
      open(store): NativeApplianceLike {
        if (wifiEndpoint !== "") {
          return bindings.NativeAppliance.openWifiProfile(store, wifiEndpoint);
        }
        return bindings.NativeAppliance.openBleProfile(store);
      },
    },
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
        cleanupPickerOwnedCredential(nativePlatformOs, picked.result);
        return { stagingPath, cleanup };
      } catch (error) {
        const cleanupErrors: unknown[] = [];
        try {
          cleanup();
        } catch (cleanupError) {
          cleanupErrors.push(cleanupError);
        }
        try {
          cleanupPickerOwnedCredential(nativePlatformOs, picked.result);
        } catch (cleanupError) {
          cleanupErrors.push(cleanupError);
        }
        if (cleanupErrors.length > 0) {
          throw new AggregateError([error, ...cleanupErrors], "Credential staging cleanup failed.");
        }
        throw error;
      }
    },
    profileStore,
  };
}

/**
 * Offline-first native adapter backed by the Rust single-owner actor and an
 * active device profile's app-private SQLite database.
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
  #bleOnboarding: ActiveNativeBleOnboarding | null = null;
  #bridge: NativeApplianceBridge | null = null;
  #lastBleOnboardingView: OnboardingView | null = null;
  #opening: Promise<void> | null = null;
  #ownership: ExclusiveResource<OwnedNativeAppliance> | null = null;
  #reopening: Promise<void> | null = null;
  #runtime: NativeApplianceRuntime | null = null;
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
    const activeOnboarding = this.#bleOnboarding;
    const runtime = this.#runtime;
    if (activeOnboarding !== null && runtime?.bleOnboarding !== undefined) {
      if (!activeOnboarding.linkReady) {
        return connectingBleOnboardingView(activeOnboarding.peripheralId);
      }
      if (!activeOnboarding.operationStarted && !activeOnboarding.transport.usable) {
        await this.#releaseBleOnboarding(activeOnboarding);
        this.#lastBleOnboardingView = failedBleOnboardingView(
          activeOnboarding.peripheralId,
          0n,
          "no_subscribed_link",
        );
        return this.#lastBleOnboardingView;
      }
      const projection = runtime.bleOnboarding.snapshot(activeOnboarding.native);
      // Pairing has completed inside Rust, but the ordinary authenticated
      // appliance actor is not ready until it reopens the newly selected
      // device profile with its own BLE hub.
      if (projection.phase === "complete") {
        return {
          available: true,
          method: "managed_pairing",
          snapshot: {
            lifecycle: { state: "working", stage: "activating" },
            revision: boundedOnboardingRevision(projection.revision),
            usb_serial: activeOnboarding.peripheralId,
          },
        };
      }
      return nativeBleOnboardingView(projection, activeOnboarding.peripheralId);
    }

    await this.#awaitReopening();
    const { ble } = this.#active();
    const credential = this.#credentialState();
    if (credential.state === "missing" && this.#lastBleOnboardingView !== null) {
      return this.#lastBleOnboardingView;
    }
    return nativeOnboardingView(
      credential,
      ble?.hasPeripheralName ?? false,
      ble !== null,
      runtime?.bleOnboarding !== undefined && runtime.createBle !== undefined,
    );
  }

  supportsBleCandidateDiscovery(): boolean {
    return !this.#disposed && (this.#ownership?.value.ble ?? null) !== null;
  }

  async scanBleCandidates(options?: BleScanOptions): Promise<readonly BleCandidate[]> {
    await this.#awaitReopening();
    if (this.#bleOnboarding !== null) {
      throw new Error("Nearby BLE discovery is unavailable while onboarding owns a link.");
    }
    const { ble } = this.#active();
    const credential = this.#credentialState();
    if (credential.state !== "missing") {
      throw new Error("Nearby BLE appliance discovery is available only before credential setup.");
    }
    if (ble === null) {
      throw new Error("Nearby BLE appliance discovery is unavailable for this transport.");
    }
    return ble.scan(options);
  }

  async contacts(): Promise<ContactView[]> {
    return parseNativeJson("contacts", await this.#call((appliance) => appliance.contactsJson()));
  }

  async nearbyPeers(): Promise<NearbyPeerView[]> {
    return parseNativeJson(
      "nearby peers",
      await this.#call((appliance) => appliance.nearbyPeersJson()),
    );
  }

  async nomadFetchStart(request: NomadFetchStartRequest): Promise<NomadFetchStartResponse> {
    return parseNativeJson(
      "Nomad fetch start response",
      await this.#call((appliance) => appliance.nomadFetchStartJson(JSON.stringify(request))),
    );
  }

  async nomadFetchPoll(request: NomadFetchPollRequest): Promise<NomadFetchPollResponse> {
    return parseNativeJson(
      "Nomad fetch poll response",
      await this.#call((appliance) => appliance.nomadFetchPollJson(JSON.stringify(request))),
    );
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

  async startOnboarding(candidate?: BleCandidate): Promise<NoContent> {
    await this.#awaitReopening();
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
    const runtime = this.#runtime;
    if (runtime?.bleOnboarding !== undefined && runtime.createBle !== undefined) {
      if (candidate === undefined) {
        throw new Error("Select a nearby BLE appliance before starting secure pairing.");
      }
      await this.#prepareBleOnboarding("pair", candidate);
      return undefined;
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

    // Import activates a device-keyed profile. Reopen the native actor so both
    // its credential and identity-bound SQLite database come from that profile;
    // this also gives BLE a fresh platform central after disposing the
    // credential-free discovery owner.
    await this.#reopenAppliance();
    if (cleanupFailure !== undefined) {
      throw new Error(
        "The credential was installed, but its app-private staging copy could not be removed.",
        { cause: cleanupFailure },
      );
    }
    return undefined;
  }

  async refreshOnboarding(): Promise<NoContent> {
    await this.onboarding();
    return undefined;
  }

  async recoverOnboarding(request: RecoveryRequest, candidate?: BleCandidate): Promise<NoContent> {
    await this.#awaitReopening();
    const runtime = this.#runtime;
    if (runtime?.bleOnboarding === undefined || runtime.createBle === undefined) {
      unsupportedOnboardingRecovery();
    }
    if (candidate === undefined) {
      throw new Error("Select the same nearby BLE appliance before recovering pairing.");
    }
    await this.#prepareBleOnboarding(
      request.action === "resume_known_pending" ? "resume" : "abort",
      candidate,
    );
    return undefined;
  }

  async continueOnboarding(): Promise<NoContent> {
    await this.#continueBleOnboarding();
    return undefined;
  }

  async cancelOnboarding(): Promise<NoContent> {
    const active = this.#bleOnboarding;
    if (active === null) return undefined;
    const onboardingBridge = this.#runtime?.bleOnboarding;
    if (active.linkReady && onboardingBridge !== undefined) {
      const phase = onboardingBridge.snapshot(active.native).phase;
      if (
        phase === "activation_prepared" ||
        phase === "publishing_profile" ||
        phase === "complete"
      ) {
        throw new Error(
          "Pairing activation is already committing and can no longer be safely cancelled.",
        );
      }
    }
    active.cancelRequested = true;
    if (!active.operationStarted) {
      try {
        await active.transport.disconnect("BLE onboarding cancelled by the user");
      } finally {
        await this.#releaseBleOnboarding(active);
        this.#lastBleOnboardingView = {
          available: true,
          method: "managed_pairing",
          snapshot: {
            lifecycle: { state: "needs_pairing" },
            revision: 0,
            usb_serial: active.peripheralId,
          },
        };
      }
      return undefined;
    }
    await active.transport.disconnect("BLE onboarding cancelled by the user");
    return undefined;
  }

  async sync(): Promise<NoContent> {
    await this.#call((appliance) => appliance.syncNow());
    return undefined;
  }

  async reconnect(): Promise<NoContent> {
    await this.#awaitReopening();
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
    const onboarding = this.#bleOnboarding;
    const ownership = this.#ownership;
    const reopening = this.#reopening;
    const runtime = this.#runtime;
    this.#bleOnboarding = null;
    this.#ownership = null;
    this.#bridge = null;
    this.#runtime = null;
    if (runtime !== null) {
      void (async () => {
        try {
          if (onboarding !== null && !onboarding.released) {
            onboarding.cancelRequested = true;
            onboarding.released = true;
            await onboarding.transport.dispose().catch(() => undefined);
            onboarding.bridge.destroy(onboarding.native);
          }
        } finally {
          await reopening?.catch(() => undefined);
          await ownership?.release().catch(() => undefined);
        }
        runtime.bridge.destroyProfileStore(runtime.profileStore);
      })();
    } else if (ownership !== null) {
      void ownership.release().catch(() => undefined);
    }
  }

  async #prepareBleOnboarding(
    action: NativeBleOnboardingAction,
    candidate: BleCandidate,
  ): Promise<void> {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#bleOnboarding !== null) {
      throw new Error("another BLE onboarding operation is already active");
    }
    const runtime = this.#runtime;
    const onboardingBridge = runtime?.bleOnboarding;
    const createBle = runtime?.createBle;
    if (
      runtime === null ||
      runtime === undefined ||
      onboardingBridge === undefined ||
      createBle === undefined
    ) {
      throw new Error("fileless BLE onboarding is unavailable in this native build");
    }

    const peripheralId = candidate.peripheralId.trim();
    if (peripheralId === "") {
      throw new Error("select a nearby BLE appliance before pairing");
    }

    const native = onboardingBridge.open(runtime.profileStore);
    let transport: NativeBleOnboardingTransport;
    try {
      transport = new NativeBleOnboardingTransport(native, createBle());
    } catch (error) {
      onboardingBridge.destroy(native);
      throw error;
    }

    const active: ActiveNativeBleOnboarding = {
      action,
      bridge: onboardingBridge,
      cancelRequested: false,
      linkReady: false,
      native,
      operationStarted: false,
      peripheralId,
      released: false,
      transport,
    };
    this.#bleOnboarding = active;
    this.#lastBleOnboardingView = null;

    try {
      await transport.connectSelected(peripheralId);
      active.linkReady = true;
      if (active.cancelRequested) {
        throw new Error("BLE onboarding cancelled by the user");
      }
    } catch (error) {
      const platformFailure = transport.failureReason;
      await this.#releaseBleOnboarding(active);
      if (active.cancelRequested) {
        this.#lastBleOnboardingView = {
          available: true,
          method: "managed_pairing",
          snapshot: {
            lifecycle: { state: "needs_pairing" },
            revision: 0,
            usb_serial: peripheralId,
          },
        };
        return;
      }
      this.#lastBleOnboardingView = failedBleOnboardingView(peripheralId, 0n, "no_subscribed_link");
      throw surfacedBleOnboardingError(error, platformFailure);
    }
  }

  async #continueBleOnboarding(): Promise<void> {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    const active = this.#bleOnboarding;
    if (active === null || active.released) {
      throw new Error("open a selected BLE appliance before continuing secure pairing");
    }
    if (!active.linkReady) {
      throw new Error("the selected BLE appliance is still opening");
    }
    if (!active.transport.usable) {
      const platformFailure = active.transport.failureReason;
      await this.#releaseBleOnboarding(active);
      this.#lastBleOnboardingView = failedBleOnboardingView(
        active.peripheralId,
        0n,
        "no_subscribed_link",
      );
      throw surfacedBleOnboardingError(
        new Error("the selected BLE appliance disconnected before secure pairing continued"),
        platformFailure,
      );
    }
    if (active.operationStarted) {
      throw new Error("the retained BLE onboarding operation has already started");
    }
    active.operationStarted = true;

    let failureView: OnboardingView | null = null;
    try {
      // The explicit UI Continue action reaches this boundary only after the
      // operator has completed the OS passkey prompt. Do not let Rust emit its
      // first pairing record until an authenticated read also reports that the
      // firmware consumed PairingComplete and durably opened this retained
      // application-pairing link.
      await active.transport.confirmAuthenticated();
      if (active.action === "pair") {
        await active.native.pair();
      } else if (active.action === "resume") {
        await active.native.resume();
      } else {
        await active.native.abortCurrent();
      }
    } catch (error) {
      try {
        const projection = active.bridge.snapshot(active.native);
        if (projection.phase === "failed") {
          failureView = nativeBleOnboardingView(projection, active.peripheralId);
        }
      } catch {
        // Preserve the operation error. The UI projection remains coarse.
      }
      const platformFailure = active.transport.failureReason;
      await this.#releaseBleOnboarding(active);
      if (active.cancelRequested) {
        this.#lastBleOnboardingView = {
          available: true,
          method: "managed_pairing",
          snapshot: {
            lifecycle: { state: "needs_pairing" },
            revision: 0,
            usb_serial: active.peripheralId,
          },
        };
        return;
      }
      this.#lastBleOnboardingView =
        failureView ?? failedBleOnboardingView(active.peripheralId, 0n, "no_subscribed_link");
      throw surfacedBleOnboardingError(error, platformFailure);
    }

    await this.#releaseBleOnboarding(active);
    if (active.action === "abort") {
      this.#lastBleOnboardingView = {
        available: true,
        method: "managed_pairing",
        snapshot: {
          lifecycle: { state: "needs_pairing" },
          revision: 0,
          usb_serial: active.peripheralId,
        },
      };
      return;
    }

    await this.#reopenAppliance();
    this.#lastBleOnboardingView = null;
  }

  async #releaseBleOnboarding(active: ActiveNativeBleOnboarding): Promise<void> {
    if (active.released) return;
    active.released = true;
    try {
      await active.transport.dispose().catch(() => undefined);
    } finally {
      try {
        active.bridge.destroy(active.native);
      } finally {
        if (this.#bleOnboarding === active) this.#bleOnboarding = null;
      }
    }
  }

  async #open(): Promise<void> {
    let runtime = this.#runtime;
    if (runtime === null) {
      runtime = await this.#loadRuntime();
      if (this.#disposed) {
        runtime.bridge.destroyProfileStore(runtime.profileStore);
        throw new Error("native appliance client has been disposed");
      }
      try {
        assertNativeBridgeContract(runtime.bridge.contract);
      } catch (error) {
        runtime.bridge.destroyProfileStore(runtime.profileStore);
        throw error;
      }
      this.#runtime = runtime;
      this.#bridge = runtime.bridge;
    }

    const ownership = await this.#acquireAppliance(runtime);
    if (this.#disposed) {
      await ownership.release().catch(() => undefined);
      throw new Error("native appliance client has been disposed");
    }
    this.#ownership = ownership;
  }

  async #acquireAppliance(
    runtime: NativeApplianceRuntime,
  ): Promise<ExclusiveResource<OwnedNativeAppliance>> {
    try {
      return await acquireExclusiveResource(
        async () => {
          if (this.#disposed) throw new Error("native appliance client has been disposed");
          const appliance = runtime.bridge.open(runtime.profileStore);
          try {
            const credential = runtime.bridge.credentialStatus(appliance);
            const bleConfig = runtime.createBle?.();
            const ble =
              bleConfig === undefined ? null : new NativeBleTransport(appliance, bleConfig);
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
  }

  async #reopenAppliance(): Promise<void> {
    if (this.#reopening !== null) return this.#reopening;
    const reopening = this.#replaceAppliance();
    this.#reopening = reopening;
    try {
      await reopening;
    } finally {
      if (this.#reopening === reopening) this.#reopening = null;
    }
  }

  async #replaceAppliance(): Promise<void> {
    const runtime = this.#runtime;
    const previous = this.#ownership;
    if (runtime === null || previous === null) {
      throw new Error("native appliance client has not been bootstrapped");
    }
    this.#ownership = null;
    await previous.release();
    const replacement = await this.#acquireAppliance(runtime);
    if (this.#disposed) {
      await replacement.release().catch(() => undefined);
      throw new Error("native appliance client has been disposed");
    }
    this.#ownership = replacement;
  }

  async #awaitReopening(): Promise<void> {
    await this.#reopening;
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
    await this.#awaitReopening();
    const { appliance, bridge } = this.#active();
    try {
      return await operation(appliance);
    } catch (error) {
      throw normalizeNativeError(error, bridge.isNativeError);
    }
  }
}
