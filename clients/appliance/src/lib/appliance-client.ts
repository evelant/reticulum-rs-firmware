import type { NativeProfileStoreSnapshot } from "@reticulum/appliance-native";

import type {
  ApplianceSnapshot,
  ContactRequest,
  ContactView,
  ConversationPeerView,
  ManualServiceAnnounceDisposition,
  MessageActivityPageRequest,
  MessageActivityPageView,
  MutationResponse,
  NetworkConfigMutationOutcome,
  NetworkConfigMutationRequest,
  NetworkConfigView,
  NetworkRuntimeStatusView,
  NoContent,
  NomadFetchPollRequest,
  NomadFetchPollResponse,
  NomadFetchStartRequest,
  NomadFetchStartResponse,
  OnboardingView,
  PhoneLocationObservationView,
  RadioRoutesStatusView,
  RadioTracePageRequest,
  RadioTracePageView,
  RecoveryRequest,
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
import type { BleBondRepairProgress } from "./ble-bond-repair.ts";
import type { BleCandidate, BleScanOptions } from "./ble-central-types.ts";
import type { NearbyPeerView } from "./nearby-peers.ts";

export interface ApplianceClient {
  bootstrapSession(): Promise<void>;
  snapshot(): Promise<ApplianceSnapshot>;
  onboarding(): Promise<OnboardingView>;
  /**
   * List the native, device-keyed appliance profiles without exposing
   * credential bytes or app-private filesystem paths.
   *
   * HTTP/web clients omit this native-only appliance-management capability.
   */
  profiles?(): Promise<NativeProfileStoreSnapshot>;
  /**
   * Close the current native owner, select an existing Rust-owned profile, and
   * open its isolated credential/database pair.
   */
  activateProfile?(profileKey: string): Promise<NoContent>;
  /**
   * Delete one inactive, device-keyed profile from this client.
   *
   * This removes local credentials, contacts, messages, and outbox state. It
   * does not revoke the credential or Bluetooth bond retained by the board.
   */
  forgetProfile?(profileKey: string): Promise<NoContent>;
  /**
   * Quiesce the current native owner and enter the existing secure BLE
   * onboarding flow for one additional physical appliance.
   */
  beginAddAppliance?(): Promise<NoContent>;
  /**
   * Report whether this bootstrapped client can add an appliance over BLE.
   */
  supportsAdditionalBleOnboarding?(): boolean;
  /**
   * Credential-free, advertisement-only discovery for native first-run UI.
   *
   * Implementations must not connect or exchange appliance protocol bytes.
   */
  scanBleCandidates?(options?: BleScanOptions): Promise<readonly BleCandidate[]>;
  /**
   * Report whether the bootstrapped runtime owns a BLE central capable of
   * credential-free discovery.
   *
   * This check must be synchronous and side-effect free.
   */
  supportsBleCandidateDiscovery?(): boolean;
  contacts(): Promise<ContactView[]>;
  /** List saved contacts together with otherwise-unknown durable message peers. */
  conversationPeers(): Promise<ConversationPeerView[]>;
  /**
   * Rust-owned authenticated peer discovery, when compiled into this client.
   *
   * Optional for client backends that do not implement discovery: callers must show an
   * unavailable state rather than parsing raw device-API records themselves.
   */
  nearbyPeers?(): Promise<NearbyPeerView[]>;
  /** Queue one coalescing primary/LXMF/NomadNet service-announce cycle. */
  manualServiceAnnounce?(): Promise<ManualServiceAnnounceDisposition>;
  /**
   * Read the board-owned desired Wi-Fi and Reticulum TCP configuration.
   *
   * These operations are optional because not every local bearer exposes the
   * authenticated network-management lane. Callers must
   * capability-gate the Connectivity workspace rather than inventing a second
   * protocol path.
   */
  networkConfig?(): Promise<NetworkConfigView>;
  /** Read the current secret-free Wi-Fi and TCP actor state. */
  networkStatus?(): Promise<NetworkRuntimeStatusView>;
  /**
   * Read one Rust-aggregated, bounded radio/interface/route snapshot.
   *
   * Device-API paging and route-table revision reconciliation stay below this
   * client boundary.
   */
  radioRoutesStatus?(): Promise<RadioRoutesStatusView>;
  /** Query one bounded page of app-local, durable packet-correlated RF evidence. */
  radioTrace?(request: RadioTracePageRequest): Promise<RadioTracePageView>;
  /** Apply one compare-and-swap network configuration mutation. */
  mutateNetworkConfig?(
    request: NetworkConfigMutationRequest,
  ): Promise<NetworkConfigMutationOutcome>;
  nomadFetchStart(request: NomadFetchStartRequest): Promise<NomadFetchStartResponse>;
  nomadFetchPoll(request: NomadFetchPollRequest): Promise<NomadFetchPollResponse>;
  /** Begin or idempotently replay one bounded Reticulum proof measurement. */
  reticulumProbeStart(request: ReticulumProbeStartRequest): Promise<ReticulumProbeStartResponse>;
  /** Poll one accepted boot-scoped Reticulum proof measurement. */
  reticulumProbePoll(request: ReticulumProbePollRequest): Promise<ReticulumProbePollResponse>;
  timeline(destination: string): Promise<TimelineView[]>;
  /** Query one bounded newest-first page of durable message lifecycle activity. */
  messageActivity(request: MessageActivityPageRequest): Promise<MessageActivityPageView>;
  /** Read the private phone-location state used to stamp future local attempts. */
  phoneLocationObservation?(): Promise<PhoneLocationObservationView>;
  /** Replace the private phone-location state used by future local attempts. */
  updatePhoneLocationObservation?(
    observation: PhoneLocationObservationView,
  ): Promise<PhoneLocationObservationView>;
  upsertContact(destination: string, request: ContactRequest): Promise<MutationResponse>;
  send(request: SendRequest): Promise<SendResponse>;
  /**
   * Create a replacement durable device submission for one retryable terminal
   * outbox row without creating a new LXMF message.
   */
  retryMessage(request: RetrySendRequest): Promise<RetrySendResponse>;
  /**
   * Start first-run setup. Native BLE callers pass the exact candidate chosen
   * during the preceding advertisement-only scan.
   */
  startOnboarding(candidate?: BleCandidate): Promise<NoContent>;
  /**
   * Continue a retained native BLE onboarding ceremony only after Bluetooth
   * security completed. The operating system may either show a passkey sheet
   * for a new bond or silently reuse a saved bond. Native implementations
   * expose this deterministic barrier because iOS does not report the latter
   * transition; host-service clients do not need it.
   */
  continueOnboarding?(): Promise<NoContent>;
  refreshOnboarding(): Promise<NoContent>;
  recoverOnboarding(request: RecoveryRequest, candidate?: BleCandidate): Promise<NoContent>;
  /**
   * Close an in-flight native onboarding link. Durable Rust recovery state is
   * retained and classified on the next explicit attempt.
   */
  cancelOnboarding?(): Promise<NoContent>;
  /**
   * Replace a stale platform Bluetooth bond while retaining the active
   * appliance credential and all profile-local data.
   */
  repairBleBond?(onProgress?: BleBondRepairProgress): Promise<NoContent>;
  sync(): Promise<NoContent>;
  /**
   * Ensure that the selected bearer is being opened without replacing an
   * already usable physical link. Foreground automatic recovery uses this
   * idempotent path; explicit operator reconnect remains destructive.
   */
  ensureConnected(): Promise<NoContent>;
  reconnect(): Promise<NoContent>;
  subscribeInvalidations(onInvalidate: () => void, onError: () => void): (() => void) | null;
  dispose(): void;
}
