import type {
  NetworkConfigMutation,
  NetworkConfigMutationOutcome,
  NetworkConfigMutationRequest,
  NetworkConfigView,
  NetworkRuntimeStatusView,
} from "../generated/api.ts";

export const NETWORK_STATUS_POLL_INTERVAL_MS = 2_000;

export interface NetworkConfigurationClient {
  mutateNetworkConfig(request: NetworkConfigMutationRequest): Promise<NetworkConfigMutationOutcome>;
  networkConfig(): Promise<NetworkConfigView>;
  networkStatus(): Promise<NetworkRuntimeStatusView>;
}

export type NetworkMutationState =
  | { readonly state: "idle" }
  | { readonly state: "running" }
  | { readonly error: string; readonly state: "error" }
  | { readonly error: string; readonly state: "retryable_error" }
  | {
      readonly currentRevision: number;
      readonly state: "revision_conflict";
    }
  | {
      readonly rebootRequired: boolean;
      readonly revision: number;
      readonly state: "applied";
    };

export interface NetworkConfigControllerState {
  readonly configuration: NetworkConfigView | null;
  readonly deviceKey: string | null;
  readonly loadError: string | null;
  readonly loadState: "inactive" | "loading" | "ready" | "error";
  readonly mutation: NetworkMutationState;
  readonly rebootRequired: boolean;
  readonly runtime: NetworkRuntimeStatusView | null;
  readonly statusError: string | null;
}

export type NetworkPollScheduler = (callback: () => void, delayMs: number) => () => void;

interface NetworkConfigControllerOptions {
  readonly createIdempotencyKey: () => string;
  readonly pollIntervalMs?: number;
  readonly schedule?: NetworkPollScheduler;
}

const schedulePoll: NetworkPollScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function inactiveState(): NetworkConfigControllerState {
  return {
    configuration: null,
    deviceKey: null,
    loadError: null,
    loadState: "inactive",
    mutation: { state: "idle" },
    rebootRequired: false,
    runtime: null,
    statusError: null,
  };
}

/**
 * Owns the active board's desired-network projection and mutation recovery.
 *
 * Configuration is loaded only while the Connectivity workspace is active.
 * Status polling is serialized, stops immediately on deactivation, and never
 * overlaps a mutation. A transport failure retains the exact secret-bearing
 * request privately so an explicit retry reuses the same CAS revision and
 * idempotency key; listeners never receive that request.
 */
export class NetworkConfigController {
  readonly #client: NetworkConfigurationClient;
  readonly #createIdempotencyKey: () => string;
  readonly #listeners = new Set<(state: NetworkConfigControllerState) => void>();
  readonly #pollIntervalMs: number;
  readonly #schedule: NetworkPollScheduler;

  #activeGeneration = 0;
  #cancelPoll: (() => void) | null = null;
  #disposed = false;
  #mutationInFlight = false;
  #pendingRequest: NetworkConfigMutationRequest | null = null;
  #pollingActive = false;
  #rebootRequiredByMutation = false;
  #state = inactiveState();

  constructor(client: NetworkConfigurationClient, options: NetworkConfigControllerOptions) {
    const pollIntervalMs = options.pollIntervalMs ?? NETWORK_STATUS_POLL_INTERVAL_MS;
    if (!Number.isFinite(pollIntervalMs) || pollIntervalMs <= 0) {
      throw new Error("network status poll interval must be positive");
    }
    this.#client = client;
    this.#createIdempotencyKey = options.createIdempotencyKey;
    this.#pollIntervalMs = pollIntervalMs;
    this.#schedule = options.schedule ?? schedulePoll;
  }

  get state(): NetworkConfigControllerState {
    return this.#state;
  }

  subscribe(listener: (state: NetworkConfigControllerState) => void): () => void {
    if (this.#disposed) return () => undefined;
    this.#listeners.add(listener);
    listener(this.#state);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async activate(deviceKey: string): Promise<void> {
    if (this.#disposed) return;
    if (this.#state.deviceKey === deviceKey) {
      if (this.#pollingActive) return;
      this.#pollingActive = true;
      const generation = this.#activeGeneration;
      await this.#loadAuthoritative(generation);
      this.#scheduleNextPoll(generation);
      return;
    }

    const generation = this.#replaceRun();
    this.#pollingActive = true;
    this.#pendingRequest = null;
    this.#rebootRequiredByMutation = false;
    this.#publish({
      configuration: null,
      deviceKey,
      loadError: null,
      loadState: "loading",
      mutation: { state: "idle" },
      rebootRequired: false,
      runtime: null,
      statusError: null,
    });

    await this.#loadAuthoritative(generation);
    this.#scheduleNextPoll(generation);
  }

  deactivate(): void {
    if (this.#disposed || this.#state.loadState === "inactive") return;
    this.#replaceRun();
    this.#pollingActive = false;
    this.#pendingRequest = null;
    this.#rebootRequiredByMutation = false;
    this.#publish(inactiveState());
  }

  /**
   * Stop reads while the workspace is hidden without discarding an uncertain
   * request for this same board. Activating a different device key still
   * clears that board-scoped recovery state.
   */
  suspend(): void {
    if (this.#disposed || !this.#pollingActive) return;
    this.#pollingActive = false;
    this.#cancelPoll?.();
    this.#cancelPoll = null;
  }

  async refresh(): Promise<void> {
    if (this.#disposed || this.#state.deviceKey === null) return;
    await this.#loadAuthoritative(this.#activeGeneration);
  }

  async mutate(mutation: NetworkConfigMutation): Promise<NetworkConfigMutationOutcome | null> {
    if (
      this.#disposed ||
      this.#state.deviceKey === null ||
      this.#state.configuration === null ||
      this.#mutationInFlight ||
      this.#pendingRequest !== null
    ) {
      return null;
    }

    let idempotencyKey: string;
    try {
      idempotencyKey = this.#createIdempotencyKey().toLowerCase();
    } catch (error) {
      this.#publish({
        ...this.#state,
        mutation: {
          error: `Could not create a mutation identity: ${errorText(error)}`,
          state: "error",
        },
      });
      return null;
    }
    if (!/^[0-9a-f]{32}$/.test(idempotencyKey)) {
      this.#publish({
        ...this.#state,
        mutation: {
          error: "Could not create a valid mutation identity",
          state: "error",
        },
      });
      return null;
    }

    const request: NetworkConfigMutationRequest = {
      expected_revision: this.#state.configuration.revision,
      idempotency_key: idempotencyKey,
      mutation,
    };
    this.#pendingRequest = request;
    return this.#submitPending();
  }

  async retryMutation(): Promise<NetworkConfigMutationOutcome | null> {
    if (
      this.#disposed ||
      this.#state.deviceKey === null ||
      this.#state.mutation.state !== "retryable_error" ||
      this.#pendingRequest === null ||
      this.#mutationInFlight
    ) {
      return null;
    }
    return this.#submitPending();
  }

  abandonMutationRetry(): void {
    if (this.#disposed || this.#mutationInFlight || this.#pendingRequest === null) return;
    this.#pendingRequest = null;
    this.#publish({ ...this.#state, mutation: { state: "idle" } });
  }

  clearMutationNotice(): void {
    if (this.#disposed || this.#mutationInFlight || this.#pendingRequest !== null) return;
    this.#publish({ ...this.#state, mutation: { state: "idle" } });
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#replaceRun();
    this.#pollingActive = false;
    this.#pendingRequest = null;
    this.#listeners.clear();
  }

  async #submitPending(): Promise<NetworkConfigMutationOutcome | null> {
    const request = this.#pendingRequest;
    if (request === null || this.#mutationInFlight) return null;
    const generation = this.#activeGeneration;
    this.#mutationInFlight = true;
    this.#publish({ ...this.#state, mutation: { state: "running" } });

    let outcome: NetworkConfigMutationOutcome;
    try {
      outcome = await this.#client.mutateNetworkConfig(request);
    } catch (error) {
      if (this.#accepts(generation)) {
        this.#publish({
          ...this.#state,
          mutation: { error: errorText(error), state: "retryable_error" },
        });
      }
      this.#mutationInFlight = false;
      return null;
    }
    this.#mutationInFlight = false;
    if (!this.#accepts(generation)) return null;
    this.#pendingRequest = null;

    if (outcome.outcome === "revision_conflict") {
      this.#publish({
        ...this.#state,
        mutation: {
          currentRevision: outcome.current_revision,
          state: "revision_conflict",
        },
      });
      await this.#loadAuthoritative(generation, this.#state.mutation);
      return outcome;
    }

    this.#rebootRequiredByMutation ||= outcome.reboot_required;
    const appliedState: NetworkMutationState = {
      rebootRequired: outcome.reboot_required,
      revision: outcome.revision,
      state: "applied",
    };
    this.#publish({
      ...this.#state,
      mutation: appliedState,
      rebootRequired: this.#effectiveRebootRequired(),
    });
    await this.#loadAuthoritative(generation, appliedState);
    return outcome;
  }

  async #loadAuthoritative(
    generation: number,
    retainedMutation: NetworkMutationState = this.#state.mutation,
  ): Promise<void> {
    const [configuration, runtime] = await Promise.allSettled([
      this.#client.networkConfig(),
      this.#client.networkStatus(),
    ]);
    if (!this.#accepts(generation)) return;

    const nextConfiguration =
      configuration.status === "fulfilled" ? configuration.value : this.#state.configuration;
    const nextRuntime = runtime.status === "fulfilled" ? runtime.value : this.#state.runtime;
    if (
      nextConfiguration !== null &&
      nextRuntime !== null &&
      nextConfiguration.revision === nextRuntime.configured_revision &&
      nextRuntime.configured_revision === nextRuntime.applied_revision
    ) {
      this.#rebootRequiredByMutation = false;
    }
    const loadError = configuration.status === "rejected" ? errorText(configuration.reason) : null;
    const statusError = runtime.status === "rejected" ? errorText(runtime.reason) : null;
    this.#publish({
      configuration: nextConfiguration,
      deviceKey: this.#state.deviceKey,
      loadError,
      loadState: nextConfiguration === null ? "error" : "ready",
      mutation: retainedMutation,
      rebootRequired: this.#effectiveRebootRequired(nextConfiguration, nextRuntime),
      runtime: nextRuntime,
      statusError,
    });
  }

  async #pollStatus(generation: number): Promise<void> {
    if (!this.#pollingActive || !this.#accepts(generation) || this.#mutationInFlight) return;
    try {
      const runtime = await this.#client.networkStatus();
      if (!this.#accepts(generation)) return;
      if (
        this.#state.configuration !== null &&
        this.#state.configuration.revision === runtime.configured_revision &&
        runtime.configured_revision === runtime.applied_revision
      ) {
        this.#rebootRequiredByMutation = false;
      }
      this.#publish({
        ...this.#state,
        rebootRequired: this.#effectiveRebootRequired(this.#state.configuration, runtime),
        runtime,
        statusError: null,
      });
    } catch (error) {
      if (this.#accepts(generation)) {
        this.#publish({ ...this.#state, statusError: errorText(error) });
      }
    }
  }

  #scheduleNextPoll(generation: number): void {
    if (!this.#pollingActive || !this.#accepts(generation)) return;
    this.#cancelPoll?.();
    this.#cancelPoll = this.#schedule(() => {
      this.#cancelPoll = null;
      void this.#pollStatus(generation).finally(() => this.#scheduleNextPoll(generation));
    }, this.#pollIntervalMs);
  }

  #effectiveRebootRequired(
    configuration = this.#state.configuration,
    runtime = this.#state.runtime,
  ): boolean {
    return (
      this.#rebootRequiredByMutation ||
      (configuration !== null &&
        runtime !== null &&
        (configuration.revision !== runtime.configured_revision ||
          runtime.configured_revision !== runtime.applied_revision))
    );
  }

  #replaceRun(): number {
    this.#activeGeneration += 1;
    this.#cancelPoll?.();
    this.#cancelPoll = null;
    return this.#activeGeneration;
  }

  #accepts(generation: number): boolean {
    return (
      !this.#disposed && generation === this.#activeGeneration && this.#state.deviceKey !== null
    );
  }

  #publish(state: NetworkConfigControllerState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener(state);
  }
}
