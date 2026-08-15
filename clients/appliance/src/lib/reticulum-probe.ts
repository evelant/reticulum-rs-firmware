import type {
  ReticulumProbeFailure,
  ReticulumProbePhase,
  ReticulumProbePollRequest,
  ReticulumProbePollResponse,
  ReticulumProbeStartOutcome,
  ReticulumProbeStartRequest,
  ReticulumProbeStartResponse,
  ReticulumProbeSuccessView,
} from "../generated/api.ts";

export const RETICULUM_PROBE_POLL_INTERVAL_MS = 750;
/**
 * This presentation deadline deliberately exceeds the firmware's two
 * sequential 60-second path-resolution envelopes, 30-second packet-capacity
 * wait, and 60-second proof receipt lease. It is only a UI safety bound; the
 * device owns the operation.
 */
export const RETICULUM_PROBE_PRESENTATION_TIMEOUT_MS = 240_000;

export interface ReticulumProbeClient {
  reticulumProbePoll(request: ReticulumProbePollRequest): Promise<ReticulumProbePollResponse>;
  reticulumProbeStart(request: ReticulumProbeStartRequest): Promise<ReticulumProbeStartResponse>;
}

interface AcceptedProbe {
  readonly destination: string;
  readonly id: string;
  readonly outcome: ReticulumProbeStartOutcome;
  readonly phase: ReticulumProbePhase | null;
  readonly request: ReticulumProbeStartRequest;
}

export type ReticulumProbeState =
  | { readonly status: "idle" }
  | { readonly destination: string; readonly error: string; readonly status: "input_error" }
  | { readonly request: ReticulumProbeStartRequest; readonly status: "starting" }
  | {
      readonly error: string;
      readonly request: ReticulumProbeStartRequest;
      readonly stage: "start";
      readonly status: "error";
    }
  | (AcceptedProbe & {
      readonly error: string;
      readonly stage: "poll";
      readonly status: "error";
    })
  | (AcceptedProbe & { readonly status: "pending" })
  | (AcceptedProbe & { readonly status: "timed_out" })
  | (AcceptedProbe & { readonly result: ReticulumProbeSuccessView; readonly status: "succeeded" })
  | (AcceptedProbe & { readonly failure: ReticulumProbeFailure; readonly status: "failed" });

type ProbePollScheduler = (callback: () => void, delayMs: number) => () => void;

interface ReticulumProbeControllerOptions {
  readonly createIdempotencyKey: () => string;
  readonly now?: () => number;
  readonly pollIntervalMs?: number;
  readonly presentationTimeoutMs?: number;
  readonly schedule?: ProbePollScheduler;
}

const schedulePoll: ProbePollScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function inputError(destination: string): string | null {
  return /^[0-9a-f]{32}$/.test(destination)
    ? null
    : "Probe destination must be exactly 32 hexadecimal characters";
}

/**
 * Owns one bounded path-and-proof measurement presentation.
 *
 * It never overlaps starts or polls and never becomes a background ping loop.
 * The local safety timeout is longer than every firmware-owned operation
 * deadline, so ordinary path discovery cannot strand the device's one probe
 * slot merely because the client stopped polling early. Poll failures and
 * presentation timeouts retain the accepted probe ID so a retry resumes the
 * exact device operation instead of consuming another start identity.
 */
export class ReticulumProbeController {
  readonly #client: ReticulumProbeClient;
  readonly #createIdempotencyKey: () => string;
  readonly #listeners = new Set<(state: ReticulumProbeState) => void>();
  readonly #now: () => number;
  readonly #pollIntervalMs: number;
  readonly #presentationTimeoutMs: number;
  readonly #schedule: ProbePollScheduler;
  #cancelWait: (() => void) | null = null;
  #disposed = false;
  #generation = 0;
  #state: ReticulumProbeState = { status: "idle" };

  constructor(client: ReticulumProbeClient, options: ReticulumProbeControllerOptions) {
    const pollIntervalMs = options.pollIntervalMs ?? RETICULUM_PROBE_POLL_INTERVAL_MS;
    const presentationTimeoutMs =
      options.presentationTimeoutMs ?? RETICULUM_PROBE_PRESENTATION_TIMEOUT_MS;
    if (!Number.isFinite(pollIntervalMs) || pollIntervalMs <= 0) {
      throw new Error("Reticulum probe poll interval must be positive");
    }
    if (!Number.isFinite(presentationTimeoutMs) || presentationTimeoutMs <= 0) {
      throw new Error("Reticulum probe presentation timeout must be positive");
    }
    this.#client = client;
    this.#createIdempotencyKey = options.createIdempotencyKey;
    this.#now = options.now ?? Date.now;
    this.#pollIntervalMs = pollIntervalMs;
    this.#presentationTimeoutMs = presentationTimeoutMs;
    this.#schedule = options.schedule ?? schedulePoll;
  }

  get state(): ReticulumProbeState {
    return this.#state;
  }

  subscribe(listener: (state: ReticulumProbeState) => void): () => void {
    if (this.#disposed) return () => {};
    this.#listeners.add(listener);
    listener(this.#state);
    return () => this.#listeners.delete(listener);
  }

  async measure(destination: string): Promise<void> {
    if (this.#disposed || this.#isRunning()) return;
    const normalized = destination.trim().toLowerCase();
    const validationError = inputError(normalized);
    if (validationError !== null) {
      this.#replaceRun();
      this.#publish({ destination: normalized, error: validationError, status: "input_error" });
      return;
    }

    const retainedPoll = this.#state;
    if (
      retainedPoll.status === "timed_out" ||
      (retainedPoll.status === "error" && retainedPoll.stage === "poll")
    ) {
      if (retainedPoll.destination === normalized) {
        await this.#resumePolling();
      }
      return;
    }

    const retained =
      this.#state.status === "error" &&
      this.#state.stage === "start" &&
      this.#state.request.destination === normalized
        ? this.#state.request
        : null;
    let request = retained;
    if (request === null) {
      let idempotencyKey: string;
      try {
        idempotencyKey = this.#createIdempotencyKey().toLowerCase();
      } catch (error) {
        this.#replaceRun();
        this.#publish({
          destination: normalized,
          error: `Probe request identity could not be created: ${errorText(error)}`,
          status: "input_error",
        });
        return;
      }
      if (!/^[0-9a-f]{32}$/.test(idempotencyKey)) {
        this.#replaceRun();
        this.#publish({
          destination: normalized,
          error: "Probe request identity could not be created",
          status: "input_error",
        });
        return;
      }
      request = { destination: normalized, idempotency_key: idempotencyKey };
    }

    const generation = this.#replaceRun();
    this.#publish({ request, status: "starting" });
    let response: ReticulumProbeStartResponse;
    try {
      response = await this.#client.reticulumProbeStart(request);
    } catch (error) {
      if (this.#isCurrent(generation)) {
        this.#publish({ error: errorText(error), request, stage: "start", status: "error" });
      }
      return;
    }
    if (!this.#isCurrent(generation)) return;

    const accepted: AcceptedProbe = {
      destination: request.destination,
      id: response.id,
      outcome: response.outcome,
      phase: null,
      request,
    };
    this.#publish({ ...accepted, status: "pending" });
    this.#schedulePoll(accepted, generation, this.#now() + this.#presentationTimeoutMs);
  }

  /**
   * Forget a locally retained recovery-state ID without contacting the node.
   *
   * This is intentionally explicit: abandoning a transient poll failure can
   * leave the device's one volatile probe slot occupied until its own bounded
   * deadline. It primarily recovers an ID made stale by a device reboot.
   */
  abandonRetainedProbe(): void {
    if (this.#disposed || !this.#hasRetainedPoll()) return;
    this.#replaceRun();
    this.#publish({ status: "idle" });
  }

  reset(): void {
    if (this.#disposed) return;
    this.#replaceRun();
    this.#publish({ status: "idle" });
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#replaceRun();
    this.#listeners.clear();
  }

  #isRunning(): boolean {
    return this.#state.status === "starting" || this.#state.status === "pending";
  }

  #hasRetainedPoll(): boolean {
    return (
      this.#state.status === "timed_out" ||
      (this.#state.status === "error" && this.#state.stage === "poll")
    );
  }

  async #resumePolling(): Promise<void> {
    const retained = this.#state;
    if (
      retained.status !== "timed_out" &&
      (retained.status !== "error" || retained.stage !== "poll")
    ) {
      return;
    }
    const accepted: AcceptedProbe = {
      destination: retained.destination,
      id: retained.id,
      outcome: retained.outcome,
      phase: retained.phase,
      request: retained.request,
    };
    const generation = this.#replaceRun();
    this.#publish({ ...accepted, status: "pending" });
    await this.#poll(accepted, generation, this.#now() + this.#presentationTimeoutMs);
  }

  #schedulePoll(accepted: AcceptedProbe, generation: number, deadlineMs: number): void {
    if (!this.#isCurrent(generation)) return;
    this.#cancelWait = this.#schedule(() => {
      this.#cancelWait = null;
      void this.#poll(accepted, generation, deadlineMs);
    }, this.#pollIntervalMs);
  }

  async #poll(accepted: AcceptedProbe, generation: number, deadlineMs: number): Promise<void> {
    if (!this.#isCurrent(generation)) return;
    if (this.#now() >= deadlineMs) {
      this.#publish({ ...accepted, status: "timed_out" });
      return;
    }

    let response: ReticulumProbePollResponse;
    try {
      response = await this.#client.reticulumProbePoll({ id: accepted.id });
    } catch (error) {
      if (this.#isCurrent(generation)) {
        this.#publish({
          ...accepted,
          error: errorText(error),
          stage: "poll",
          status: "error",
        });
      }
      return;
    }
    if (!this.#isCurrent(generation)) return;

    switch (response.state) {
      case "pending":
        {
          const pending = { ...accepted, phase: response.phase };
          this.#publish({ ...pending, status: "pending" });
          this.#schedulePoll(pending, generation, deadlineMs);
        }
        return;
      case "succeeded":
        this.#publish({ ...accepted, result: response.result, status: "succeeded" });
        return;
      case "failed":
        this.#publish({ ...accepted, failure: response.failure, status: "failed" });
    }
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

  #publish(state: ReticulumProbeState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener(state);
  }
}
