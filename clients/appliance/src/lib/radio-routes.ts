import type {
  DiagnosticLoraLastTxView,
  RadioRoutesStatusView,
  RetainedRouteView,
} from "../generated/api.ts";

/** Normal foreground refresh interval for the bounded radio/routes snapshot. */
export const RADIO_ROUTES_POLL_INTERVAL_MS = 5_000;

export interface RadioRoutesClient {
  radioRoutesStatus(): Promise<RadioRoutesStatusView>;
}

export interface RadioRoutesControllerState {
  readonly deviceKey: string | null;
  readonly error: string | null;
  readonly loadState: "error" | "inactive" | "loading" | "ready";
  readonly snapshot: RadioRoutesStatusView | null;
  readonly updatedAtMs: number | null;
}

interface RadioRoutesControllerOptions {
  readonly now?: () => number;
  readonly pollIntervalMs?: number;
  readonly schedule?: (callback: () => void, delayMs: number) => () => void;
}

const scheduleTimeout = (callback: () => void, delayMs: number): (() => void) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

/** Compact duration for constrained mobile diagnostic rows. */
export function compactDurationLabel(milliseconds: number): string {
  if (milliseconds < 1_000) return "now";
  const seconds = Math.floor(milliseconds / 1_000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/** Elapsed-time label that does not imply an age when the device has none. */
export function elapsedAgeLabel(milliseconds: number | null): string {
  return milliseconds === null ? "unknown" : `${compactDurationLabel(milliseconds)} ago`;
}

/** Remaining route lifetime without turning a missing expiry into elapsed time. */
export function routeExpiryLabel(milliseconds: number | null): string {
  return milliseconds === null
    ? "Expiry: unknown"
    : `Expires in ${compactDurationLabel(milliseconds)}`;
}

/** Compact terminal-TX label with explicit DATA versus ordinary ownership. */
export function loraTxSummaryLabel(lastTx: DiagnosticLoraLastTxView): string {
  const family = lastTx.family === null ? "unknown family" : lastTx.family;
  return `${elapsedAgeLabel(lastTx.age_ms)} · ${family} · ${lastTx.outcome.replaceAll("_", " ")}`;
}

/** Exact prepared DATA identity shared with per-message packet evidence. */
export function loraDataTxEvidenceLabel(lastTx: DiagnosticLoraLastTxView): string | null {
  const evidence = lastTx.data_evidence;
  return evidence === null
    ? null
    : `Interface ${evidence.interface_id} · ${evidence.encoded_packet_len} bytes`;
}

/** Transport family used to group retained routes in the diagnostics UI. */
export type RetainedRouteTransportFamily = "lora" | "tcp" | "other";

/**
 * Resolve a retained route's transport family through its retained interface
 * record. Broadcast-fallback routes carry no interface and belong to "other":
 * they are not specific to a single packet interface.
 */
export function retainedRouteFamily(
  route: RetainedRouteView,
  snapshot: RadioRoutesStatusView,
): RetainedRouteTransportFamily {
  if (route.retained_interface_id === null) return "other";
  const record = snapshot.interfaces.find(
    (candidate) => candidate.id === route.retained_interface_id,
  );
  switch (record?.kind) {
    case "lora":
      return "lora";
    case "tcp_client":
    case "tcp_server":
      return "tcp";
    default:
      return "other";
  }
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * One-in-flight, generation-safe reader for volatile node diagnostics.
 *
 * Paging and route-table reconciliation remain inside Rust. This controller
 * only owns foreground polling and rejects results from a previous appliance
 * or activation generation.
 */
export class RadioRoutesController {
  readonly #client: RadioRoutesClient;
  readonly #listeners = new Set<(state: RadioRoutesControllerState) => void>();
  readonly #now: () => number;
  readonly #pollIntervalMs: number;
  readonly #schedule: (callback: () => void, delayMs: number) => () => void;
  #activeGeneration = 0;
  #cancelPoll: (() => void) | null = null;
  #disposed = false;
  #inFlight = false;
  #pendingRead = false;
  #pollingActive = false;
  #state: RadioRoutesControllerState = {
    deviceKey: null,
    error: null,
    loadState: "inactive",
    snapshot: null,
    updatedAtMs: null,
  };

  constructor(client: RadioRoutesClient, options: RadioRoutesControllerOptions = {}) {
    const pollIntervalMs = options.pollIntervalMs ?? RADIO_ROUTES_POLL_INTERVAL_MS;
    if (!Number.isFinite(pollIntervalMs) || pollIntervalMs <= 0) {
      throw new Error("radio/routes poll interval must be positive");
    }
    this.#client = client;
    this.#now = options.now ?? Date.now;
    this.#pollIntervalMs = pollIntervalMs;
    this.#schedule = options.schedule ?? scheduleTimeout;
  }

  get state(): RadioRoutesControllerState {
    return this.#state;
  }

  subscribe(listener: (state: RadioRoutesControllerState) => void): () => void {
    if (this.#disposed) return () => {};
    this.#listeners.add(listener);
    listener(this.#state);
    return () => this.#listeners.delete(listener);
  }

  async activate(deviceKey: string): Promise<void> {
    if (this.#disposed) return;
    if (this.#pollingActive && this.#state.deviceKey === deviceKey) return;

    const sameDevice = this.#state.deviceKey === deviceKey;
    const generation = this.#replaceRun();
    this.#pollingActive = true;
    this.#publish({
      deviceKey,
      error: null,
      loadState: sameDevice && this.#state.snapshot !== null ? "ready" : "loading",
      snapshot: sameDevice ? this.#state.snapshot : null,
      updatedAtMs: sameDevice ? this.#state.updatedAtMs : null,
    });
    await this.#read(generation);
  }

  /** Stop background reads while retaining the latest visible snapshot. */
  suspend(): void {
    if (this.#disposed || !this.#pollingActive) return;
    this.#pollingActive = false;
    this.#pendingRead = false;
    this.#replaceRun();
  }

  /** Explicitly refresh the active appliance without permitting overlap. */
  async refresh(): Promise<void> {
    if (this.#disposed || !this.#pollingActive || this.#state.deviceKey === null) {
      return;
    }
    await this.#read(this.#activeGeneration);
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#pollingActive = false;
    this.#pendingRead = false;
    this.#replaceRun();
    this.#listeners.clear();
  }

  async #read(generation: number): Promise<void> {
    if (!this.#accepts(generation)) return;
    if (this.#inFlight) {
      this.#pendingRead = true;
      return;
    }

    this.#inFlight = true;
    try {
      const snapshot = await this.#client.radioRoutesStatus();
      if (!this.#accepts(generation)) return;
      this.#publish({
        deviceKey: this.#state.deviceKey,
        error: null,
        loadState: "ready",
        snapshot,
        updatedAtMs: this.#now(),
      });
    } catch (error) {
      if (!this.#accepts(generation)) return;
      this.#publish({
        ...this.#state,
        error: errorText(error),
        loadState: this.#state.snapshot === null ? "error" : "ready",
      });
    } finally {
      this.#inFlight = false;
      if (this.#pendingRead && this.#pollingActive && !this.#disposed) {
        this.#pendingRead = false;
        void this.#read(this.#activeGeneration);
      } else if (this.#accepts(generation)) {
        this.#scheduleNext(generation);
      }
    }
  }

  #scheduleNext(generation: number): void {
    if (!this.#accepts(generation)) return;
    this.#cancelPoll?.();
    this.#cancelPoll = this.#schedule(() => {
      this.#cancelPoll = null;
      void this.#read(generation);
    }, this.#pollIntervalMs);
  }

  #replaceRun(): number {
    this.#activeGeneration += 1;
    this.#cancelPoll?.();
    this.#cancelPoll = null;
    return this.#activeGeneration;
  }

  #accepts(generation: number): boolean {
    return !this.#disposed && this.#pollingActive && generation === this.#activeGeneration;
  }

  #publish(state: RadioRoutesControllerState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener(state);
  }
}
