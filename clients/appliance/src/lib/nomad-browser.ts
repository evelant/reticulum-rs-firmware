import type {
  NomadFetchFailure,
  NomadFetchPhase,
  NomadFetchPollRequest,
  NomadFetchPollResponse,
  NomadFetchStartOutcome,
  NomadFetchStartRequest,
  NomadFetchStartResponse,
} from "../generated/api.ts";

export const DEFAULT_NOMAD_PAGE_PATH = "/page/index.mu";
export const NOMAD_POLL_INTERVAL_MS = 1_000;
export const NOMAD_PRESENTATION_TIMEOUT_MS = 120_000;

export interface NomadFetchClient {
  nomadFetchPoll(request: NomadFetchPollRequest): Promise<NomadFetchPollResponse>;
  nomadFetchStart(request: NomadFetchStartRequest): Promise<NomadFetchStartResponse>;
}

interface AcceptedFetch {
  readonly id: string;
  readonly outcome: NomadFetchStartOutcome;
  readonly phase: NomadFetchPhase | null;
  readonly request: NomadFetchStartRequest;
}

export type NomadBrowserState =
  | { readonly status: "idle" }
  | {
      readonly destination: string;
      readonly error: string;
      readonly path: string;
      readonly status: "input_error";
    }
  | { readonly request: NomadFetchStartRequest; readonly status: "starting" }
  | {
      readonly error: string;
      readonly request: NomadFetchStartRequest;
      readonly status: "start_error";
    }
  | (AcceptedFetch & { readonly status: "pending" })
  | (AcceptedFetch & { readonly error: string; readonly status: "poll_error" })
  | (AcceptedFetch & { readonly status: "timed_out" })
  | (AcceptedFetch & { readonly page: string; readonly status: "ready" })
  | (AcceptedFetch & { readonly failure: NomadFetchFailure; readonly status: "failed" });

export function nomadRequestProvenance(
  state: NomadBrowserState,
): { readonly destination: string; readonly path: string } | null {
  if (!("request" in state)) return null;
  return { destination: state.request.destination, path: state.request.path };
}

export type NomadPollScheduler = (callback: () => void, delayMs: number) => () => void;

interface NomadBrowserControllerOptions {
  readonly createIdempotencyKey: () => string;
  readonly now?: () => number;
  readonly pollIntervalMs?: number;
  readonly presentationTimeoutMs?: number;
  readonly schedule?: NomadPollScheduler;
}

const schedulePoll: NomadPollScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function sameStartSemantics(
  request: NomadFetchStartRequest,
  destination: string,
  path: string,
): boolean {
  return request.destination === destination && request.path === path;
}

export function nomadFetchInputError(destination: string, path: string): string | null {
  if (!/^[0-9a-f]{32}$/.test(destination)) {
    return "Nomad destination must be exactly 32 hexadecimal characters";
  }
  if (path.length === 0 || !path.startsWith("/") || path.includes("\0")) {
    return "Nomad path must be absolute, non-empty, and contain no NUL byte";
  }
  return null;
}

export function nomadDestinationHintApplication(
  currentDestination: string,
  hint: string | null,
  slotOwned: boolean,
): { readonly consumed: boolean; readonly destination: string } {
  if (hint === null || slotOwned) {
    return { consumed: false, destination: currentDestination };
  }
  return { consumed: true, destination: hint };
}

/**
 * Owns one UI presentation of the device's boot-scoped Nomad fetch.
 *
 * The controller never overlaps polls. Ambiguous start failures retain the
 * exact timestamp and idempotency key for a safe replay, while a presentation
 * timeout stops local polling without discarding the device fetch ID. Only an
 * explicit local abandon releases a retained recovery-state ID for a new
 * start.
 */
export class NomadBrowserController {
  readonly #client: NomadFetchClient;
  readonly #createIdempotencyKey: () => string;
  readonly #listeners = new Set<(state: NomadBrowserState) => void>();
  readonly #now: () => number;
  readonly #pollIntervalMs: number;
  readonly #presentationTimeoutMs: number;
  readonly #schedule: NomadPollScheduler;

  #cancelWait: (() => void) | null = null;
  #disposed = false;
  #generation = 0;
  #state: NomadBrowserState = { status: "idle" };

  constructor(client: NomadFetchClient, options: NomadBrowserControllerOptions) {
    const pollIntervalMs = options.pollIntervalMs ?? NOMAD_POLL_INTERVAL_MS;
    const presentationTimeoutMs = options.presentationTimeoutMs ?? NOMAD_PRESENTATION_TIMEOUT_MS;
    if (!Number.isFinite(pollIntervalMs) || pollIntervalMs <= 0) {
      throw new Error("Nomad poll interval must be positive");
    }
    if (!Number.isFinite(presentationTimeoutMs) || presentationTimeoutMs <= 0) {
      throw new Error("Nomad presentation timeout must be positive");
    }
    this.#client = client;
    this.#createIdempotencyKey = options.createIdempotencyKey;
    this.#now = options.now ?? Date.now;
    this.#pollIntervalMs = pollIntervalMs;
    this.#presentationTimeoutMs = presentationTimeoutMs;
    this.#schedule = options.schedule ?? schedulePoll;
  }

  get state(): NomadBrowserState {
    return this.#state;
  }

  subscribe(listener: (state: NomadBrowserState) => void): () => void {
    if (this.#disposed) return () => undefined;
    this.#listeners.add(listener);
    listener(this.#state);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async start(destination: string, path = DEFAULT_NOMAD_PAGE_PATH): Promise<void> {
    if (
      this.#disposed ||
      this.#state.status === "starting" ||
      this.#state.status === "pending" ||
      this.#state.status === "poll_error" ||
      this.#state.status === "timed_out"
    ) {
      return;
    }
    const normalizedDestination = destination.trim().toLowerCase();
    const inputError = nomadFetchInputError(normalizedDestination, path);
    if (inputError !== null) {
      this.#replaceRun();
      this.#publish({
        destination: normalizedDestination,
        error: inputError,
        path,
        status: "input_error",
      });
      return;
    }

    const retained =
      this.#state.status === "start_error" &&
      sameStartSemantics(this.#state.request, normalizedDestination, path)
        ? this.#state.request
        : null;
    let request = retained;
    if (request === null) {
      const timestampUnixMs = this.#now();
      let idempotencyKey: string;
      try {
        idempotencyKey = this.#createIdempotencyKey().toLowerCase();
      } catch (error) {
        this.#replaceRun();
        this.#publish({
          destination: normalizedDestination,
          error: `Nomad request identity could not be created: ${errorText(error)}`,
          path,
          status: "input_error",
        });
        return;
      }
      if (
        !Number.isSafeInteger(timestampUnixMs) ||
        timestampUnixMs <= 0 ||
        !/^[0-9a-f]{32}$/.test(idempotencyKey)
      ) {
        this.#replaceRun();
        this.#publish({
          destination: normalizedDestination,
          error: "Nomad request identity could not be created",
          path,
          status: "input_error",
        });
        return;
      }
      request = {
        destination: normalizedDestination,
        idempotency_key: idempotencyKey,
        path,
        timestamp_unix_ms: timestampUnixMs,
      };
    }
    await this.#startRequest(request);
  }

  async retryStart(): Promise<void> {
    if (this.#disposed || this.#state.status !== "start_error") return;
    await this.#startRequest(this.#state.request);
  }

  async resumePolling(): Promise<void> {
    if (
      this.#disposed ||
      (this.#state.status !== "poll_error" && this.#state.status !== "timed_out")
    ) {
      return;
    }
    const accepted: AcceptedFetch = {
      id: this.#state.id,
      outcome: this.#state.outcome,
      phase: this.#state.phase,
      request: this.#state.request,
    };
    const generation = this.#replaceRun();
    this.#publish({ ...accepted, status: "pending" });
    await this.#poll(accepted, generation, this.#now() + this.#presentationTimeoutMs);
  }

  /**
   * Forget one locally retained recovery-state ID without contacting the node.
   *
   * This is intentionally limited to poll-error and presentation-timeout
   * states. It recovers from a boot-scoped ID made stale by a device reset,
   * while an active start or poll cannot be abandoned accidentally.
   */
  abandonRetainedFetch(): void {
    if (
      this.#disposed ||
      (this.#state.status !== "poll_error" && this.#state.status !== "timed_out")
    ) {
      return;
    }
    this.#replaceRun();
    this.#publish({ status: "idle" });
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#replaceRun();
    this.#listeners.clear();
  }

  async #startRequest(request: NomadFetchStartRequest): Promise<void> {
    const generation = this.#replaceRun();
    this.#publish({ request, status: "starting" });
    let response: NomadFetchStartResponse;
    try {
      response = await this.#client.nomadFetchStart(request);
    } catch (error) {
      if (this.#isCurrent(generation)) {
        this.#publish({ error: errorText(error), request, status: "start_error" });
      }
      return;
    }
    if (!this.#isCurrent(generation)) return;
    const accepted: AcceptedFetch = {
      id: response.id,
      outcome: response.outcome,
      phase: null,
      request,
    };
    this.#publish({ ...accepted, status: "pending" });
    await this.#poll(accepted, generation, this.#now() + this.#presentationTimeoutMs);
  }

  async #poll(
    initial: AcceptedFetch,
    generation: number,
    presentationDeadlineMs: number,
  ): Promise<void> {
    let accepted = initial;
    while (this.#isCurrent(generation)) {
      if (this.#now() >= presentationDeadlineMs) {
        this.#publish({ ...accepted, status: "timed_out" });
        return;
      }

      let response: NomadFetchPollResponse;
      try {
        response = await this.#client.nomadFetchPoll({ id: accepted.id });
      } catch (error) {
        if (this.#isCurrent(generation)) {
          this.#publish({ ...accepted, error: errorText(error), status: "poll_error" });
        }
        return;
      }
      if (!this.#isCurrent(generation)) return;

      if (response.state === "ready") {
        this.#publish({ ...accepted, page: response.page, status: "ready" });
        return;
      }
      if (response.state === "failed") {
        this.#publish({ ...accepted, failure: response.failure, status: "failed" });
        return;
      }

      accepted = { ...accepted, phase: response.phase };
      if (this.#now() >= presentationDeadlineMs) {
        this.#publish({ ...accepted, status: "timed_out" });
        return;
      }
      this.#publish({ ...accepted, status: "pending" });
      const delayMs = Math.min(
        this.#pollIntervalMs,
        Math.max(1, presentationDeadlineMs - this.#now()),
      );
      if (!(await this.#wait(generation, delayMs))) return;
    }
  }

  #wait(generation: number, delayMs: number): Promise<boolean> {
    return new Promise((resolve) => {
      let settled = false;
      let cancelScheduled: () => void = () => undefined;
      const finish = (elapsed: boolean) => {
        if (settled) return;
        settled = true;
        if (this.#cancelWait === cancelWait) this.#cancelWait = null;
        resolve(elapsed && this.#isCurrent(generation));
      };
      const cancelWait = () => {
        cancelScheduled();
        finish(false);
      };
      cancelScheduled = this.#schedule(() => finish(true), delayMs);
      if (!settled) this.#cancelWait = cancelWait;
    });
  }

  #replaceRun(): number {
    this.#generation += 1;
    this.#cancelWait?.();
    this.#cancelWait = null;
    return this.#generation;
  }

  #isCurrent(generation: number): boolean {
    return !this.#disposed && generation === this.#generation;
  }

  #publish(state: NomadBrowserState): void {
    if (this.#disposed) return;
    this.#state = state;
    for (const listener of this.#listeners) listener(state);
  }
}
