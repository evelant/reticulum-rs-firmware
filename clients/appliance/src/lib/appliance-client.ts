import type { NativePrnsOtaStatus, NativeProfileStoreSnapshot } from "@reticulum/appliance-native";

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
import type { NearbyPeerView } from "./nearby-peers.ts";
import type { OnboardingView } from "./onboarding.ts";
import type {
  ReticulumApplianceCandidate,
  ReticulumDiscoveryOptions,
} from "./reticulum-appliance-candidate.ts";

export interface ApplianceClient {
  bootstrapSession(): Promise<void>;
  snapshot(): Promise<ApplianceSnapshot>;
  onboarding(): Promise<OnboardingView>;
  /**
   * List native profiles keyed by verified management destination without
   * exposing app-private filesystem paths.
   *
   * HTTP/web clients omit this native-only appliance-management capability.
   */
  profiles?(): Promise<NativeProfileStoreSnapshot>;
  /**
   * Close the current native owner, select an existing Rust-owned profile, and
   * open its isolated local application database.
   */
  activateProfile?(profileKey: string): Promise<NoContent>;
  /**
   * Delete one inactive, device-keyed profile from this client.
   *
   * This removes local contacts, messages, and outbox state. It does not revoke
   * the app identity from the appliance management allow-list.
   */
  forgetProfile?(profileKey: string): Promise<NoContent>;
  /**
   * Enter Reticulum discovery and enrollment for another physical appliance.
   */
  beginAddAppliance?(): Promise<NoContent>;
  /**
   * Report whether this client can enroll another appliance over Reticulum.
   */
  supportsAdditionalApplianceEnrollment?(): boolean;
  /**
   * Verify management announces through the app-owned PRNS node.
   */
  scanReticulumCandidates?(
    options?: ReticulumDiscoveryOptions,
  ): Promise<readonly ReticulumApplianceCandidate[]>;
  /**
   * Report whether the bootstrapped runtime owns a PRNS node capable of
   * management-application discovery.
   *
   * This check must be synchronous and side-effect free.
   */
  supportsReticulumCandidateDiscovery?(): boolean;
  /**
   * Report whether this client owns a native PRNS node that can stage firmware
   * through the appliance management destination.
   */
  supportsFirmwareUpdate?(): boolean;
  /** Stage one complete ESP application image without rebooting the board. */
  stageFirmwareUpdate?(image: ArrayBuffer, version: string): Promise<NativePrnsOtaStatus>;
  /** Reboot the board only after it reports a completely verified staged slot. */
  rebootIntoStagedFirmware?(): Promise<NativePrnsOtaStatus>;
  contacts(): Promise<ContactView[]>;
  /** List saved contacts together with otherwise-unknown durable message peers. */
  conversationPeers(): Promise<ConversationPeerView[]>;
  /** Read the product-owned label for this physical appliance. */
  applianceLabel?(): Promise<ApplianceLabelView>;
  /** Compare-and-swap the product-owned label without changing app announce names. */
  mutateApplianceLabel?(
    request: ApplianceLabelMutationRequest,
  ): Promise<ApplianceLabelMutationOutcome>;
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
   * These operations are optional because not every client backend exposes the
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
   * Start first-run setup for the exact verified management application chosen
   * during the preceding PRNS discovery pass.
   */
  startOnboarding(candidate?: ReticulumApplianceCandidate): Promise<NoContent>;
  sync(): Promise<NoContent>;
  /**
   * Ensure that the Reticulum session is being opened without replacing an
   * already usable Link. Foreground automatic recovery uses this
   * idempotent path; explicit operator reconnect remains destructive.
   */
  ensureConnected(): Promise<NoContent>;
  reconnect(): Promise<NoContent>;
  subscribeInvalidations(onInvalidate: () => void, onError: () => void): (() => void) | null;
  dispose(): void;
}
