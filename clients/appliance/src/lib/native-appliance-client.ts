import type {
  NativeApplianceLike,
  NativeBridgeContract,
  NativePrnsManagementCandidate,
  NativePrnsManagementIdentity,
  NativePrnsNodeLike,
  NativePrnsOtaStatus,
  NativeProfileStoreLike,
  NativeProfileStoreSnapshot,
  NativeProfileSummary,
} from "@reticulum/appliance-native";

import type {
  ApplianceLabelMutationOutcome,
  ApplianceLabelMutationRequest,
  ApplianceLabelView,
  ApplianceSnapshot,
  ContactRequest,
  ContactView,
  ConversationPeerView,
  ManualServiceAnnounceDisposition,
  MessageActivityPageRequest,
  MessageActivityPageView,
  MutationResponse,
  NearbyPeerView,
  NetworkConfigMutationOutcome,
  NetworkConfigMutationRequest,
  NetworkConfigView,
  NetworkRuntimeStatusView,
  NoContent,
  NomadFetchPollRequest,
  NomadFetchPollResponse,
  NomadFetchStartRequest,
  NomadFetchStartResponse,
  PhoneLocationObservationView,
  RadioRoutesStatusView,
  RadioTracePageRequest,
  RadioTracePageView,
  ReticulumProbePollRequest,
  ReticulumProbePollResponse,
  ReticulumProbeStartRequest,
  ReticulumProbeStartResponse,
  RetrySendRequest,
  RetrySendResponse,
  SendRequest,
  SendResponse,
  TimelineView,
} from "../generated/api.ts";
import type { ApplianceClient } from "./appliance-client.ts";
import { acquireExclusiveResource, type ExclusiveResource } from "./exclusive-resource.ts";
import { nativePathFromFileUri } from "./file-uri.ts";
import { assertNativeBridgeContract } from "./native-contract.ts";
import { type NativeErrorPredicate, normalizeNativeError } from "./native-error.ts";
import type { OnboardingView } from "./onboarding.ts";
import type {
  ReticulumApplianceCandidate,
  ReticulumDiscoveryOptions,
} from "./reticulum-appliance-candidate.ts";

const PROFILE_STORE_DIRECTORY_NAME = "reticulum-appliance-profiles-v3";
const PRNS_STORE_DIRECTORY_NAME = "reticulum-prns";
const DESTINATION_PATTERN = /^[0-9a-f]{32}$/;

export interface NativeApplianceBridge {
  readonly contract: NativeBridgeContract;
  readonly isNativeError: NativeErrorPredicate;
  activateProfile(profileStore: NativeProfileStoreLike, profileKey: string): NativeProfileSummary;
  destroy(appliance: NativeApplianceLike): void;
  destroyProfileStore(profileStore: NativeProfileStoreLike): void;
  forgetProfile(
    profileStore: NativeProfileStoreLike,
    profileKey: string,
  ): NativeProfileStoreSnapshot;
  open(profileStore: NativeProfileStoreLike): NativeApplianceLike;
  profileSnapshot(profileStore: NativeProfileStoreLike): NativeProfileStoreSnapshot;
  rememberProfile(
    profileStore: NativeProfileStoreLike,
    managementDestination: string,
    lxmfDestination: string,
  ): NativeProfileSummary;
}

export interface NativePrnsBridge {
  readonly node: NativePrnsNodeLike;
  close(): void;
  enroll(destinationHash: string, signal?: AbortSignal): Promise<void>;
  managementCandidates(): readonly NativePrnsManagementCandidate[];
  managementIdentity(
    destinationHash: string,
    signal?: AbortSignal,
  ): Promise<NativePrnsManagementIdentity>;
  publicIdentity(
    destinationHash: string,
    signal?: AbortSignal,
  ): Promise<NativePrnsManagementIdentity>;
  rebootOta(destinationHash: string, signal?: AbortSignal): Promise<NativePrnsOtaStatus>;
  stageOta(
    destinationHash: string,
    image: ArrayBuffer,
    version: string,
    signal?: AbortSignal,
  ): Promise<NativePrnsOtaStatus>;
}

export interface NativeApplianceRuntime {
  readonly bridge: NativeApplianceBridge;
  readonly prns: NativePrnsBridge;
  readonly profileStore: NativeProfileStoreLike;
}

export type NativeApplianceRuntimeLoader = () => Promise<NativeApplianceRuntime>;

interface OwnedNativeAppliance {
  readonly appliance: NativeApplianceLike;
}

function parseNativeJson<T>(label: string, value: string): T {
  try {
    return JSON.parse(value) as T;
  } catch (error) {
    throw new Error(`native Rust bridge returned invalid ${label} JSON`, { cause: error });
  }
}

function normalizeDestination(value: string, label: string): string {
  const destination = value.trim().toLowerCase();
  if (!DESTINATION_PATTERN.test(destination)) {
    throw new Error(`${label} must be 32 hexadecimal characters.`);
  }
  return destination;
}

async function loadNativeApplianceRuntime(): Promise<NativeApplianceRuntime> {
  const [bindings, fileSystem] = await Promise.all([
    import("@reticulum/appliance-native"),
    import("expo-file-system"),
  ]);
  const profileStoreUri = new fileSystem.Directory(
    fileSystem.Paths.document,
    PROFILE_STORE_DIRECTORY_NAME,
  ).uri;
  const prnsStoreUri = new fileSystem.Directory(
    fileSystem.Paths.document,
    PRNS_STORE_DIRECTORY_NAME,
  ).uri;
  const profileStore = bindings.NativeProfileStore.open(nativePathFromFileUri(profileStoreUri));
  const prnsNode = bindings.NativePrnsNode.start(nativePathFromFileUri(prnsStoreUri));
  return {
    bridge: {
      contract: bindings.nativeBridgeContract(),
      isNativeError: bindings.NativeApplianceError.instanceOf,
      activateProfile(store, profileKey): NativeProfileSummary {
        return store.activateProfile(profileKey);
      },
      destroy(appliance): void {
        if (bindings.NativeAppliance.instanceOf(appliance)) appliance.uniffiDestroy();
      },
      destroyProfileStore(store): void {
        if (bindings.NativeProfileStore.instanceOf(store)) store.uniffiDestroy();
      },
      forgetProfile(store, profileKey): NativeProfileStoreSnapshot {
        return store.deleteInactiveProfile(profileKey);
      },
      open(store): NativeApplianceLike {
        return bindings.NativeAppliance.openPrnsProfile(store, prnsNode);
      },
      profileSnapshot(store): NativeProfileStoreSnapshot {
        return store.snapshot();
      },
      rememberProfile(store, managementDestination, lxmfDestination): NativeProfileSummary {
        return store.rememberProfile(managementDestination, lxmfDestination);
      },
    },
    prns: {
      node: prnsNode,
      close(): void {
        try {
          prnsNode.close();
        } finally {
          if (bindings.NativePrnsNode.instanceOf(prnsNode)) prnsNode.uniffiDestroy();
        }
      },
      enroll(destinationHash, signal): Promise<void> {
        return prnsNode.enrollManagement(
          destinationHash,
          signal === undefined ? undefined : { signal },
        );
      },
      managementCandidates(): readonly NativePrnsManagementCandidate[] {
        return prnsNode.managementCandidates();
      },
      managementIdentity(destinationHash, signal): Promise<NativePrnsManagementIdentity> {
        return prnsNode.managementIdentity(
          destinationHash,
          signal === undefined ? undefined : { signal },
        );
      },
      publicIdentity(destinationHash, signal): Promise<NativePrnsManagementIdentity> {
        return prnsNode.publicManagementIdentity(
          destinationHash,
          signal === undefined ? undefined : { signal },
        );
      },
      rebootOta(destinationHash, signal): Promise<NativePrnsOtaStatus> {
        return prnsNode.rebootIntoStagedOta(
          destinationHash,
          signal === undefined ? undefined : { signal },
        );
      },
      stageOta(destinationHash, image, version, signal): Promise<NativePrnsOtaStatus> {
        return prnsNode.stageOtaImage(
          destinationHash,
          image,
          version,
          signal === undefined ? undefined : { signal },
        );
      },
    },
    profileStore,
  };
}

/** Offline-first native client over one app-wide PRNS node. */
export class NativeApplianceClient implements ApplianceClient {
  readonly #loadRuntime: NativeApplianceRuntimeLoader;
  #opening: Promise<void> | null = null;
  #ownerFault: Error | null = null;
  #ownership: ExclusiveResource<OwnedNativeAppliance> | null = null;
  #profileSwitchTail: Promise<void> = Promise.resolve();
  #reopening: Promise<void> | null = null;
  #runtime: NativeApplianceRuntime | null = null;
  #enrollmentDestination: string | null = null;
  #disposed = false;

  constructor(loadRuntime: NativeApplianceRuntimeLoader = loadNativeApplianceRuntime) {
    this.#loadRuntime = loadRuntime;
  }

  async bootstrapSession(): Promise<void> {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#ownerFault !== null) throw this.#ownerFault;
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

  async profiles(): Promise<NativeProfileStoreSnapshot> {
    await this.#awaitReopening();
    const runtime = this.#requiredRuntime();
    try {
      return runtime.bridge.profileSnapshot(runtime.profileStore);
    } catch (error) {
      throw normalizeNativeError(error, runtime.bridge.isNativeError);
    }
  }

  async activateProfile(profileKey: string): Promise<NoContent> {
    const key = normalizeDestination(profileKey, "profile key");
    const switching = this.#profileSwitchTail.then(
      () => this.#activateProfileSerialized(key),
      () => this.#activateProfileSerialized(key),
    );
    this.#profileSwitchTail = switching.then(
      () => undefined,
      () => undefined,
    );
    await switching;
    return undefined;
  }

  async forgetProfile(profileKey: string): Promise<NoContent> {
    const key = normalizeDestination(profileKey, "profile key");
    const forgetting = this.#profileSwitchTail.then(
      () => this.#forgetProfileSerialized(key),
      () => this.#forgetProfileSerialized(key),
    );
    this.#profileSwitchTail = forgetting.then(
      () => undefined,
      () => undefined,
    );
    await forgetting;
    return undefined;
  }

  async beginAddAppliance(): Promise<NoContent> {
    await this.#awaitReopening();
    this.#requiredRuntime();
    return undefined;
  }

  supportsAdditionalApplianceEnrollment(): boolean {
    return !this.#disposed && this.#runtime !== null;
  }

  async onboarding(): Promise<OnboardingView> {
    const catalog = await this.profiles();
    const active = catalog.profiles.find(
      (profile) => profile.profileKey === catalog.activeProfileKey,
    );
    if (active !== undefined) {
      return {
        available: true,
        lifecycle: {
          state: "ready",
          managementDestination: active.managementDestination,
        },
      };
    }
    return {
      available: true,
      lifecycle:
        this.#enrollmentDestination === null
          ? { state: "needs_candidate" }
          : {
              state: "authorizing",
              managementDestination: this.#enrollmentDestination,
            },
    };
  }

  supportsReticulumCandidateDiscovery(): boolean {
    return !this.#disposed && this.#runtime !== null;
  }

  supportsFirmwareUpdate(): boolean {
    return !this.#disposed && this.#runtime !== null;
  }

  async stageFirmwareUpdate(image: ArrayBuffer, version: string): Promise<NativePrnsOtaStatus> {
    await this.#awaitReopening();
    const runtime = this.#requiredRuntime();
    const destination = this.#activeManagementDestination(runtime);
    return runtime.prns.stageOta(destination, image, version);
  }

  async rebootIntoStagedFirmware(): Promise<NativePrnsOtaStatus> {
    await this.#awaitReopening();
    const runtime = this.#requiredRuntime();
    const destination = this.#activeManagementDestination(runtime);
    return runtime.prns.rebootOta(destination);
  }

  async scanReticulumCandidates(
    options?: ReticulumDiscoveryOptions,
  ): Promise<readonly ReticulumApplianceCandidate[]> {
    await this.#awaitReopening();
    const runtime = this.#requiredRuntime();
    if (options?.signal?.aborted === true) throw options.signal.reason;
    const verified = await Promise.all(
      runtime.prns.managementCandidates().map(async (candidate) => {
        try {
          const identity = await runtime.prns.publicIdentity(
            candidate.destinationHash,
            options?.signal,
          );
          if (
            identity.managementDestination !== candidate.destinationHash ||
            identity.lxmfDestination === undefined
          ) {
            return null;
          }
          return {
            managementDestination: identity.managementDestination,
            lxmfDestination: identity.lxmfDestination,
            interfaceId: candidate.interfaceId,
            hops: candidate.hops,
          } satisfies ReticulumApplianceCandidate;
        } catch (error) {
          if (options?.signal?.aborted === true) throw error;
          return null;
        }
      }),
    );
    return verified.filter(
      (candidate): candidate is ReticulumApplianceCandidate => candidate !== null,
    );
  }

  async contacts(): Promise<ContactView[]> {
    return parseNativeJson("contacts", await this.#call((appliance) => appliance.contactsJson()));
  }

  async conversationPeers(): Promise<ConversationPeerView[]> {
    return parseNativeJson(
      "conversation peers",
      await this.#call((appliance) => appliance.conversationPeersJson()),
    );
  }

  async applianceLabel(): Promise<ApplianceLabelView> {
    return parseNativeJson(
      "appliance label",
      await this.#call((appliance) => appliance.applianceLabelJson()),
    );
  }

  async mutateApplianceLabel(
    request: ApplianceLabelMutationRequest,
  ): Promise<ApplianceLabelMutationOutcome> {
    return parseNativeJson(
      "appliance label mutation",
      await this.#call((appliance) => appliance.mutateApplianceLabelJson(JSON.stringify(request))),
    );
  }

  async nearbyPeers(): Promise<NearbyPeerView[]> {
    return parseNativeJson(
      "nearby peers",
      await this.#call((appliance) => appliance.nearbyPeersJson()),
    );
  }

  async networkConfig(): Promise<NetworkConfigView> {
    return parseNativeJson(
      "network configuration",
      await this.#call((appliance) => appliance.networkConfigJson()),
    );
  }

  async manualServiceAnnounce(): Promise<ManualServiceAnnounceDisposition> {
    return parseNativeJson(
      "manual service announce",
      await this.#call((appliance) => appliance.manualServiceAnnounceJson()),
    );
  }

  async networkStatus(): Promise<NetworkRuntimeStatusView> {
    return parseNativeJson(
      "network status",
      await this.#call((appliance) => appliance.networkStatusJson()),
    );
  }

  async radioRoutesStatus(): Promise<RadioRoutesStatusView> {
    return parseNativeJson(
      "radio and route diagnostics",
      await this.#call((appliance) => appliance.radioRoutesStatusJson()),
    );
  }

  async radioTrace(request: RadioTracePageRequest): Promise<RadioTracePageView> {
    return parseNativeJson(
      "radio trace",
      await this.#call((appliance) => appliance.radioTraceJson(JSON.stringify(request))),
    );
  }

  async mutateNetworkConfig(
    request: NetworkConfigMutationRequest,
  ): Promise<NetworkConfigMutationOutcome> {
    return parseNativeJson(
      "network configuration mutation",
      await this.#call((appliance) => appliance.mutateNetworkConfigJson(JSON.stringify(request))),
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

  async reticulumProbeStart(
    request: ReticulumProbeStartRequest,
  ): Promise<ReticulumProbeStartResponse> {
    return parseNativeJson(
      "Reticulum probe start response",
      await this.#call((appliance) => appliance.reticulumProbeStartJson(JSON.stringify(request))),
    );
  }

  async reticulumProbePoll(
    request: ReticulumProbePollRequest,
  ): Promise<ReticulumProbePollResponse> {
    return parseNativeJson(
      "Reticulum probe poll response",
      await this.#call((appliance) => appliance.reticulumProbePollJson(JSON.stringify(request))),
    );
  }

  async timeline(destination: string): Promise<TimelineView[]> {
    return parseNativeJson(
      "timeline",
      await this.#call((appliance) => appliance.timelineJson(destination)),
    );
  }

  async messageActivity(request: MessageActivityPageRequest): Promise<MessageActivityPageView> {
    return parseNativeJson(
      "message activity",
      await this.#call((appliance) => appliance.messageActivityJson(JSON.stringify(request))),
    );
  }

  async phoneLocationObservation(): Promise<PhoneLocationObservationView> {
    return parseNativeJson(
      "phone location observation",
      await this.#call((appliance) => appliance.phoneLocationObservationJson()),
    );
  }

  async updatePhoneLocationObservation(
    observation: PhoneLocationObservationView,
  ): Promise<PhoneLocationObservationView> {
    return parseNativeJson(
      "phone location observation update",
      await this.#call((appliance) =>
        appliance.updatePhoneLocationObservationJson(JSON.stringify(observation)),
      ),
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

  async retryMessage(request: RetrySendRequest): Promise<RetrySendResponse> {
    return parseNativeJson(
      "retry send response",
      await this.#call((appliance) => appliance.retryMessageJson(JSON.stringify(request))),
    );
  }

  async startOnboarding(candidate?: ReticulumApplianceCandidate): Promise<NoContent> {
    await this.#awaitReopening();
    if (candidate === undefined) {
      throw new Error("Select a verified Reticulum appliance before enrollment.");
    }
    const runtime = this.#requiredRuntime();
    const destination = normalizeDestination(
      candidate.managementDestination,
      "management destination",
    );
    const expectedLxmf = normalizeDestination(candidate.lxmfDestination, "LXMF destination");
    this.#enrollmentDestination = destination;
    try {
      const publicIdentity = await runtime.prns.publicIdentity(destination);
      if (
        publicIdentity.managementDestination !== destination ||
        publicIdentity.lxmfDestination !== expectedLxmf
      ) {
        throw new Error("The selected announce no longer identifies the verified appliance.");
      }
      try {
        await runtime.prns.managementIdentity(destination);
      } catch {
        await runtime.prns.enroll(destination);
      }
      const authorized = await runtime.prns.managementIdentity(destination);
      if (
        authorized.managementDestination !== destination ||
        authorized.lxmfDestination !== expectedLxmf
      ) {
        throw new Error("The appliance did not authorize the selected management application.");
      }
      await this.#rememberAndOpen(runtime, destination, expectedLxmf);
      await this.#call((appliance) => appliance.ensureConnected());
    } finally {
      this.#enrollmentDestination = null;
    }
    return undefined;
  }

  async sync(): Promise<NoContent> {
    await this.#call((appliance) => appliance.syncNow());
    return undefined;
  }

  async ensureConnected(): Promise<NoContent> {
    await this.#call((appliance) => appliance.ensureConnected());
    return undefined;
  }

  async reconnect(): Promise<NoContent> {
    await this.#call((appliance) => appliance.reconnect());
    return undefined;
  }

  subscribeInvalidations(): null {
    return null;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    const ownership = this.#ownership;
    const runtime = this.#runtime;
    this.#ownership = null;
    this.#runtime = null;
    this.#ownerFault = null;
    void (async () => {
      try {
        await ownership?.release();
      } finally {
        if (runtime !== null) {
          try {
            runtime.prns.close();
          } finally {
            runtime.bridge.destroyProfileStore(runtime.profileStore);
          }
        }
      }
    })();
  }

  async #open(): Promise<void> {
    let runtime = this.#runtime;
    if (runtime === null) {
      runtime = await this.#loadRuntime();
      if (this.#disposed) {
        runtime.prns.close();
        runtime.bridge.destroyProfileStore(runtime.profileStore);
        throw new Error("native appliance client has been disposed");
      }
      try {
        assertNativeBridgeContract(runtime.bridge.contract);
      } catch (error) {
        runtime.prns.close();
        runtime.bridge.destroyProfileStore(runtime.profileStore);
        throw error;
      }
      this.#runtime = runtime;
    }
    this.#ownership = await this.#acquireAppliance(runtime);
    this.#ownerFault = null;
  }

  async #acquireAppliance(
    runtime: NativeApplianceRuntime,
  ): Promise<ExclusiveResource<OwnedNativeAppliance>> {
    try {
      return await acquireExclusiveResource(
        () => {
          if (this.#disposed) throw new Error("native appliance client has been disposed");
          return { appliance: runtime.bridge.open(runtime.profileStore) };
        },
        async ({ appliance }) => {
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
  }

  async #activateProfileSerialized(profileKey: string): Promise<void> {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    await this.#awaitReopening();
    const runtime = this.#requiredRuntime();
    const catalog = runtime.bridge.profileSnapshot(runtime.profileStore);
    if (!catalog.profiles.some((profile) => profile.profileKey === profileKey)) {
      throw new Error("the selected appliance profile no longer exists");
    }
    if (catalog.activeProfileKey === profileKey) return;
    await this.#replaceSelectedProfile(runtime, profileKey, catalog.activeProfileKey);
  }

  async #forgetProfileSerialized(profileKey: string): Promise<void> {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    await this.#awaitReopening();
    const runtime = this.#requiredRuntime();
    const catalog = runtime.bridge.profileSnapshot(runtime.profileStore);
    if (catalog.activeProfileKey === profileKey) {
      throw new Error("switch to another appliance before forgetting the active profile");
    }
    if (!catalog.profiles.some((profile) => profile.profileKey === profileKey)) {
      throw new Error("the selected appliance profile no longer exists");
    }
    runtime.bridge.forgetProfile(runtime.profileStore, profileKey);
  }

  async #rememberAndOpen(
    runtime: NativeApplianceRuntime,
    managementDestination: string,
    lxmfDestination: string,
  ): Promise<void> {
    const previous = runtime.bridge.profileSnapshot(runtime.profileStore).activeProfileKey;
    await this.#closeAppliance(runtime);
    try {
      runtime.bridge.rememberProfile(runtime.profileStore, managementDestination, lxmfDestination);
      this.#ownership = await this.#acquireAppliance(runtime);
      this.#ownerFault = null;
    } catch (error) {
      if (previous !== undefined && previous !== managementDestination) {
        try {
          runtime.bridge.activateProfile(runtime.profileStore, previous);
        } catch {
          // The original error remains primary; the reopen below provides the
          // authoritative usable-state check.
        }
      }
      try {
        this.#ownership = await this.#acquireAppliance(runtime);
        this.#ownerFault = null;
      } catch (restoreError) {
        const fault = new AggregateError(
          [error, restoreError],
          "The Reticulum profile could not be opened and no appliance owner could be restored.",
        );
        this.#ownerFault = fault;
        throw fault;
      }
      throw error;
    }
  }

  async #replaceSelectedProfile(
    runtime: NativeApplianceRuntime,
    target: string,
    previous: string | undefined,
  ): Promise<void> {
    await this.#closeAppliance(runtime);
    try {
      runtime.bridge.activateProfile(runtime.profileStore, target);
      this.#ownership = await this.#acquireAppliance(runtime);
      this.#ownerFault = null;
    } catch (error) {
      if (previous !== undefined) {
        try {
          runtime.bridge.activateProfile(runtime.profileStore, previous);
        } catch {
          // Reopening below determines whether a usable owner remains.
        }
      }
      try {
        this.#ownership = await this.#acquireAppliance(runtime);
        this.#ownerFault = null;
      } catch (restoreError) {
        const fault = new AggregateError(
          [error, restoreError],
          "The selected Reticulum profile could not open and the previous owner was not restored.",
        );
        this.#ownerFault = fault;
        throw fault;
      }
      throw error;
    }
  }

  async #closeAppliance(runtime: NativeApplianceRuntime): Promise<void> {
    const ownership = this.#ownership;
    if (ownership === null) return;
    this.#ownership = null;
    try {
      await ownership.release();
    } catch (error) {
      const fault = new Error(
        "The current appliance could not close cleanly, so another owner was not opened.",
        { cause: normalizeNativeError(error, runtime.bridge.isNativeError) },
      );
      this.#ownerFault = fault;
      throw fault;
    }
  }

  async #awaitReopening(): Promise<void> {
    await this.#reopening;
  }

  #requiredRuntime(): NativeApplianceRuntime {
    if (this.#disposed) throw new Error("native appliance client has been disposed");
    if (this.#ownerFault !== null) throw this.#ownerFault;
    if (this.#runtime === null)
      throw new Error("native appliance client has not been bootstrapped");
    return this.#runtime;
  }

  #activeManagementDestination(runtime: NativeApplianceRuntime): string {
    if (this.#ownership === null) {
      throw new Error("select an appliance before starting a firmware update");
    }
    const catalog = runtime.bridge.profileSnapshot(runtime.profileStore);
    const active = catalog.profiles.find(
      (profile) => profile.profileKey === catalog.activeProfileKey,
    );
    if (active === undefined) {
      throw new Error("the active appliance profile no longer exists");
    }
    return normalizeDestination(active.managementDestination, "management destination");
  }

  #active(): { readonly appliance: NativeApplianceLike; readonly bridge: NativeApplianceBridge } {
    const runtime = this.#requiredRuntime();
    if (this.#ownership === null) {
      throw new Error("native appliance client has no active profile owner");
    }
    return { appliance: this.#ownership.value.appliance, bridge: runtime.bridge };
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
