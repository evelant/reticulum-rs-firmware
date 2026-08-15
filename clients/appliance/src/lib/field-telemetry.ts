import type { PhoneLocationObservationView } from "../generated/api.ts";
import {
  type FieldTelemetryPreferenceStore,
  MemoryFieldTelemetryPreferenceStore,
} from "./field-telemetry-preference.ts";
import {
  type ForegroundPhoneLocationTelemetry,
  type PhoneLocationObservationSink,
  startForegroundPhoneLocationTelemetry,
} from "./phone-location.ts";

export interface FieldTelemetryClient {
  phoneLocationObservation(): Promise<PhoneLocationObservationView>;
  updatePhoneLocationObservation(
    observation: PhoneLocationObservationView,
  ): Promise<PhoneLocationObservationView>;
}

export interface FieldTelemetryControllerState {
  readonly deviceKey: string | null;
  readonly enabled: boolean;
  readonly error: string | null;
  readonly observation: PhoneLocationObservationView | null;
  readonly runState: "active" | "disabled" | "error" | "inactive" | "starting";
}

type TelemetryStarter = (
  onObservation: PhoneLocationObservationSink,
) => Promise<ForegroundPhoneLocationTelemetry>;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Generation-safe owner for private foreground app-submission telemetry.
 *
 * The app stamps the current sample only when it creates an initial submission
 * or an explicit terminal-row replacement. Board-owned automatic carrier
 * retries remain inside that durable submission and reuse its original stamp;
 * they do not wake the app or sample a new phone position.
 */
export class FieldTelemetryController {
  readonly #client: FieldTelemetryClient;
  readonly #listeners = new Set<(state: FieldTelemetryControllerState) => void>();
  readonly #preferenceStore: FieldTelemetryPreferenceStore;
  readonly #startTelemetry: TelemetryStarter;
  #disposed = false;
  #generation = 0;
  #preferenceEnabled = false;
  #preferenceLoaded = false;
  #preferenceUpdates = Promise.resolve();
  #subscription: ForegroundPhoneLocationTelemetry | null = null;
  #state: FieldTelemetryControllerState = {
    deviceKey: null,
    enabled: false,
    error: null,
    observation: null,
    runState: "disabled",
  };
  #updates = Promise.resolve();

  constructor(
    client: FieldTelemetryClient,
    startTelemetry: TelemetryStarter = startForegroundPhoneLocationTelemetry,
    preferenceStore: FieldTelemetryPreferenceStore = new MemoryFieldTelemetryPreferenceStore(),
  ) {
    this.#client = client;
    this.#startTelemetry = startTelemetry;
    this.#preferenceStore = preferenceStore;
  }

  get state(): FieldTelemetryControllerState {
    return this.#state;
  }

  subscribe(listener: (state: FieldTelemetryControllerState) => void): () => void {
    if (this.#disposed) return () => {};
    this.#listeners.add(listener);
    listener(this.#state);
    return () => this.#listeners.delete(listener);
  }

  async activate(deviceKey: string): Promise<void> {
    if (this.#disposed) return;
    if (
      this.#state.deviceKey === deviceKey &&
      (this.#state.runState === "active" || this.#state.runState === "starting")
    ) {
      return;
    }
    const deviceChanged = this.#state.deviceKey !== deviceKey;
    const generation = this.#replaceRun();
    this.#publish({
      ...this.#state,
      deviceKey,
      enabled: this.#preferenceLoaded ? this.#preferenceEnabled : false,
      error: null,
      observation: deviceChanged ? null : this.#state.observation,
      runState: "starting",
    });
    try {
      const [retained, enabled] = await Promise.all([
        this.#client.phoneLocationObservation(),
        this.#loadPreference(),
      ]);
      if (!this.#accepts(generation)) return;
      this.#publish({
        ...this.#state,
        enabled,
        observation: retained,
        runState: enabled ? "starting" : "disabled",
      });
      if (enabled) {
        await this.#start(generation);
      } else {
        await this.#recordDisabled(generation);
      }
    } catch (error) {
      if (this.#accepts(generation)) {
        await this.#recordDisabled(generation, errorText(error));
      }
    }
  }

  suspend(): void {
    if (this.#disposed) return;
    this.#replaceRun();
    this.#publish({
      ...this.#state,
      error: null,
      runState: this.#state.enabled ? "inactive" : "disabled",
    });
  }

  async setEnabled(enabled: boolean): Promise<void> {
    if (this.#disposed || (this.#preferenceLoaded && this.#state.enabled === enabled)) {
      return;
    }
    const generation = this.#replaceRun();
    this.#preferenceLoaded = true;
    this.#preferenceEnabled = enabled;
    if (!enabled) {
      const observation = {
        state: "unavailable",
        reason: "telemetry_disabled",
      } as const satisfies PhoneLocationObservationView;
      this.#publish({
        ...this.#state,
        enabled: false,
        error: null,
        observation,
        runState: "disabled",
      });
      const [saved, retained] = await Promise.allSettled([
        this.#savePreference(false),
        this.#client.updatePhoneLocationObservation(observation),
      ]);
      if (!this.#accepts(generation)) return;
      const errors = [saved, retained]
        .filter((result) => result.status === "rejected")
        .map((result) => errorText(result.reason));
      this.#publish({
        ...this.#state,
        error: errors.length === 0 ? null : errors.join("; "),
        observation: retained.status === "fulfilled" ? retained.value : observation,
      });
      return;
    }

    this.#publish({ ...this.#state, enabled: true, error: null, runState: "starting" });
    try {
      await this.#savePreference(true);
    } catch (error) {
      if (this.#accepts(generation)) {
        this.#preferenceEnabled = false;
        this.#publish({
          ...this.#state,
          enabled: false,
          error: `Location preference was not saved: ${errorText(error)}`,
          runState: "disabled",
        });
      }
      return;
    }
    if (!this.#accepts(generation)) return;
    if (this.#state.deviceKey === null) {
      this.#publish({ ...this.#state, runState: "inactive" });
      return;
    }
    await this.#start(generation);
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#replaceRun();
    this.#listeners.clear();
  }

  async #start(generation: number): Promise<void> {
    try {
      const subscription = await this.#startTelemetry((observation) =>
        this.#queueObservation(generation, observation),
      );
      if (!this.#accepts(generation)) {
        subscription.remove();
        return;
      }
      this.#subscription = subscription;
      this.#publish({
        ...this.#state,
        error: null,
        runState: subscription.collecting ? "active" : "inactive",
      });
    } catch (error) {
      if (!this.#accepts(generation)) return;
      const observation = {
        state: "unavailable",
        reason: "provider_error",
      } as const satisfies PhoneLocationObservationView;
      await this.#queueObservation(generation, observation);
      if (this.#accepts(generation)) this.#fail(error);
    }
  }

  async #loadPreference(): Promise<boolean> {
    if (this.#preferenceLoaded) return this.#preferenceEnabled;
    const enabled = await this.#preferenceStore.load();
    if (!this.#preferenceLoaded) {
      this.#preferenceLoaded = true;
      this.#preferenceEnabled = enabled;
    }
    return this.#preferenceEnabled;
  }

  #savePreference(enabled: boolean): Promise<void> {
    const update = this.#preferenceUpdates.then(() => this.#preferenceStore.save(enabled));
    this.#preferenceUpdates = update.catch(() => undefined);
    return update;
  }

  async #recordDisabled(generation: number, retainedError: string | null = null): Promise<void> {
    const observation = {
      state: "unavailable",
      reason: "telemetry_disabled",
    } as const satisfies PhoneLocationObservationView;
    try {
      const retained = await this.#client.updatePhoneLocationObservation(observation);
      if (this.#accepts(generation)) {
        this.#publish({
          ...this.#state,
          enabled: false,
          error: retainedError,
          observation: retained,
          runState: retainedError === null ? "disabled" : "error",
        });
      }
    } catch (error) {
      if (this.#accepts(generation)) {
        const combined = [retainedError, errorText(error)].filter((value) => value !== null);
        this.#publish({
          ...this.#state,
          enabled: false,
          error: combined.join("; "),
          observation,
          runState: "error",
        });
      }
    }
  }

  #queueObservation(generation: number, observation: PhoneLocationObservationView): Promise<void> {
    const update = this.#updates.then(async () => {
      if (!this.#accepts(generation)) return;
      const retained = await this.#client.updatePhoneLocationObservation(observation);
      if (this.#accepts(generation)) {
        this.#publish({ ...this.#state, error: null, observation: retained });
      }
    });
    this.#updates = update.catch((error) => {
      if (this.#accepts(generation)) this.#fail(error);
    });
    return update;
  }

  #replaceRun(): number {
    this.#generation += 1;
    this.#subscription?.remove();
    this.#subscription = null;
    return this.#generation;
  }

  #accepts(generation: number): boolean {
    return !this.#disposed && this.#generation === generation;
  }

  #fail(error: unknown): void {
    this.#subscription?.remove();
    this.#subscription = null;
    this.#publish({ ...this.#state, error: errorText(error), runState: "error" });
  }

  #publish(state: FieldTelemetryControllerState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener(state);
  }
}
