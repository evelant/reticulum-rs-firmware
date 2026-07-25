import type {
  ApplianceSnapshot,
  ContactRequest,
  ContactView,
  MutationResponse,
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
import type { BleCandidate, BleScanOptions } from "./ble-central-types.ts";
import type { NearbyPeerView } from "./nearby-peers.ts";

export interface ApplianceClient {
  bootstrapSession(): Promise<void>;
  snapshot(): Promise<ApplianceSnapshot>;
  onboarding(): Promise<OnboardingView>;
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
   * This check must be synchronous and side-effect free. It lets transports
   * such as native Wi-Fi omit an otherwise nonfunctional BLE action even when
   * they share the same client implementation.
   */
  supportsBleCandidateDiscovery?(): boolean;
  contacts(): Promise<ContactView[]>;
  /**
   * Rust-owned authenticated peer discovery, when compiled into this client.
   *
   * Optional during the alpha bridge transition: callers must show an
   * unavailable state rather than parsing raw device-API records themselves.
   */
  nearbyPeers?(): Promise<NearbyPeerView[]>;
  nomadFetchStart(request: NomadFetchStartRequest): Promise<NomadFetchStartResponse>;
  nomadFetchPoll(request: NomadFetchPollRequest): Promise<NomadFetchPollResponse>;
  timeline(destination: string): Promise<TimelineView[]>;
  upsertContact(destination: string, request: ContactRequest): Promise<MutationResponse>;
  send(request: SendRequest): Promise<SendResponse>;
  startOnboarding(): Promise<NoContent>;
  refreshOnboarding(): Promise<NoContent>;
  recoverOnboarding(request: RecoveryRequest): Promise<NoContent>;
  sync(): Promise<NoContent>;
  reconnect(): Promise<NoContent>;
  subscribeInvalidations(onInvalidate: () => void, onError: () => void): (() => void) | null;
  dispose(): void;
}
